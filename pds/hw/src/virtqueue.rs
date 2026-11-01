// SPDX-License-Identifier: MIT OR Apache-2.0
//! Split virtqueues in a driver's DMA region: a receive queue, whose buffers
//! the device fills - input events; a transmit queue, whose buffers
//! the device reads - a console's output; and chain queues, whose
//! packet buffers lie in memory the driver never maps - the network card's
//! receive and transmit queues.
//!
//! Written from the virtio 1.2 specification, section 2.7 ("Split
//! Virtqueues"). A queue of `N` entries (a power of two) lies in one
//! [`DmaRegion`], laid out by [`Layout`]: the descriptor table (16 bytes an
//! entry), the available ring the driver writes, the used ring the device
//! writes, then `N` buffers of the same size. Descriptor `i` always names
//! buffer `i`, device-writable, and never chains.
//!
//! **The device is not trusted.** Everything it writes - the used ring's
//! index, each entry's buffer id and length, the buffer's bytes - is copied
//! out of DMA memory before it is looked at, and each used entry is checked:
//! its id must name a buffer the device holds, and its length must fit the
//! buffer and the caller's copy. An entry that fails is dropped and counted
//! ([`RxQueue::rejected`]), never followed; a used index that claims more
//! entries than the queue has is dropped whole and counted. Nothing the
//! device writes can make the driver read or write outside its buffers, or
//! panic.
//!
//! A [`TxQueue`] is laid out the same way, its buffers device-readable: the
//! driver copies bytes into a free buffer and offers it, and the device
//! gives it back through the used ring, where each entry's id is checked as
//! a receive queue's is. The device never writes those buffers, so nothing
//! is read back from them.
//!
//! A [`ChainQueue`] gives each buffer a chain of two descriptors instead: a
//! header in the queue's own region, then the buffer, in an [`IoRegion`] -
//! another PD's memory, which this PD hands to the device by physical
//! address and never maps. It checks used entries the same way,
//! and never reads the buffers.
//!
//! This module has no `unsafe` code: all of it goes through [`DmaRegion`]'s
//! copying reads and writes, and [`IoRegion`]'s checked addresses. Telling
//! the device where a queue is, and notifying it, is
//! [`VirtioMmio`](crate::virtio::VirtioMmio)'s; only the queues here
//! implement [`Virtqueue`], which it takes.

use crate::cpu::dsb;
use crate::{DmaRegion, HwError, IoRegion};

/// Descriptor flag: the chain continues in the descriptor `next` names.
const DESC_F_NEXT: u16 = 1;

/// Descriptor flag: the device writes the buffer.
const DESC_F_WRITE: u16 = 2;

/// The most descriptors a split virtqueue may have (section 2.7).
const MAX_QUEUE_SIZE: usize = 32768;

mod sealed {
    /// Seals [`Virtqueue`](super::Virtqueue).
    pub trait Sealed {}
}

/// A virtqueue this module laid out in its own DMA region, which
/// [`VirtioMmio::set_up_queue`](crate::virtio::VirtioMmio::set_up_queue)
/// hands to a device. Sealed: only this module's queues, whose every
/// address is computed and checked inside their regions, implement it.
pub trait Virtqueue: sealed::Sealed {
    /// The queue's size, QueueNum: its descriptors, a power of two.
    fn size(&self) -> u32;

    /// The device addresses of its descriptor table and rings.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue this module made.
    fn addresses(&self) -> Result<QueueAddresses, HwError>;
}

/// The bytes of one descriptor and one used-ring entry.
const DESC_LEN: usize = 16;
const USED_ENTRY_LEN: usize = 8;

/// Where a queue's parts lie in its DMA region (section 2.7: the descriptor
/// table 16-byte aligned, the available ring 2-byte aligned, the used ring
/// 4-byte aligned).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The descriptor table, at the region's start.
    pub desc: usize,
    /// The available ring: flags, index, `N` entries, used_event.
    pub avail: usize,
    /// The used ring: flags, index, `N` entries, avail_event.
    pub used: usize,
    /// The first buffer; buffer `i` is at `buffers + i * buffer_len`.
    pub buffers: usize,
    /// The size of each buffer.
    pub buffer_len: usize,
    /// One past the last byte used.
    pub end: usize,
}

impl Layout {
    /// The layout of a queue of `entries` buffers of `buffer_len` bytes, or
    /// `None` if the sizes overflow.
    #[must_use]
    pub fn new(entries: usize, buffer_len: usize) -> Option<Self> {
        let desc = 0;
        let avail = entries.checked_mul(DESC_LEN)?;
        let avail_end = avail.checked_add(entries.checked_mul(2)?.checked_add(6)?)?;
        let used = round_up(avail_end, 4)?;
        let used_end = used.checked_add(entries.checked_mul(USED_ENTRY_LEN)?.checked_add(6)?)?;
        let buffers = round_up(used_end, 16)?;
        let end = buffers.checked_add(entries.checked_mul(buffer_len)?)?;
        Some(Self {
            desc,
            avail,
            used,
            buffers,
            buffer_len,
            end,
        })
    }
}

fn round_up(value: usize, to: usize) -> Option<usize> {
    value
        .checked_add(to.checked_sub(1)?)?
        .checked_div(to)?
        .checked_mul(to)
}

/// The device addresses the transport is given: the descriptor table, the
/// available ("driver") ring and the used ("device") ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueAddresses {
    /// The descriptor table.
    pub desc: u64,
    /// The available ring.
    pub driver: u64,
    /// The used ring.
    pub device: u64,
}

/// A receive queue of `N` buffers: the device fills them, the driver copies
/// them out and gives them back.
#[derive(Debug)]
pub struct RxQueue<const N: usize> {
    region: DmaRegion,
    layout: Layout,
    /// The available ring's next index (free-running).
    next_avail: u16,
    /// The used ring's next entry to read (free-running).
    next_used: u16,
    /// Which buffers the device holds.
    with_device: [bool; N],
    rejected: u32,
}

impl<const N: usize> RxQueue<N> {
    /// A queue of `N` buffers of `buffer_len` bytes in `region`, with its
    /// rings zeroed and every descriptor naming its buffer, device-writable.
    /// No buffer is offered yet: call [`RxQueue::offer_all`].
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if `N` is not a power of two from 1 to
    /// 32768, or the queue does not fit in the region.
    pub fn new(mut region: DmaRegion, buffer_len: usize) -> Result<Self, HwError> {
        let too_big = HwError::OutsideRegion {
            offset: 0,
            len: usize::MAX,
            size: region.len(),
        };
        if !N.is_power_of_two() || N > 32768 {
            return Err(too_big);
        }
        let layout = Layout::new(N, buffer_len).ok_or(too_big)?;
        if layout.end > region.len() {
            return Err(HwError::OutsideRegion {
                offset: 0,
                len: layout.end,
                size: region.len(),
            });
        }
        let zeros = [0u8; 64];
        let mut at = 0;
        while at < layout.buffers {
            let len = (layout.buffers.saturating_sub(at)).min(zeros.len());
            region.write(at, zeros.get(..len).unwrap_or_default())?;
            at = at.saturating_add(len);
        }
        let len = u32::try_from(buffer_len).map_err(|_| too_big)?;
        for index in 0..N {
            let offset = buffer_offset(&layout, index).ok_or(too_big)?;
            let address = region.device_address(offset, buffer_len)?;
            let mut desc = [0u8; DESC_LEN];
            if let Some((addr, rest)) = desc.split_first_chunk_mut::<8>() {
                *addr = address.to_le_bytes();
                if let Some((length, rest)) = rest.split_first_chunk_mut::<4>() {
                    *length = len.to_le_bytes();
                    if let Some((flags, _next)) = rest.split_first_chunk_mut::<2>() {
                        *flags = DESC_F_WRITE.to_le_bytes();
                    }
                }
            }
            let at = index
                .checked_mul(DESC_LEN)
                .and_then(|o| o.checked_add(layout.desc))
                .ok_or(too_big)?;
            region.write(at, &desc)?;
        }
        Ok(Self {
            region,
            layout,
            next_avail: 0,
            next_used: 0,
            with_device: [false; N],
            rejected: 0,
        })
    }

    /// The queue's layout in its region.
    #[must_use]
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// The device addresses of the descriptor table and the two rings.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`RxQueue::new`] made.
    pub fn addresses(&self) -> Result<QueueAddresses, HwError> {
        Ok(QueueAddresses {
            desc: self
                .region
                .device_address(self.layout.desc, self.layout.avail)?,
            driver: self.region.device_address(
                self.layout.avail,
                self.layout.used.saturating_sub(self.layout.avail),
            )?,
            device: self.region.device_address(
                self.layout.used,
                self.layout.buffers.saturating_sub(self.layout.used),
            )?,
        })
    }

    /// Offers every buffer the device does not hold. Notify the device
    /// afterwards.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`RxQueue::new`] made.
    pub fn offer_all(&mut self) -> Result<(), HwError> {
        for index in 0..N {
            if !self.with_device.get(index).copied().unwrap_or(true) {
                self.place(index)?;
            }
        }
        self.publish()
    }

    /// Copies the next buffer the device has filled into `out` and offers it
    /// back, returning how many bytes it holds; `None` when the device has
    /// returned nothing new. Used entries that fail their checks are dropped
    /// and counted, and the next one is tried. Notify the device afterwards
    /// if a buffer was taken.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`RxQueue::new`] made.
    pub fn take(&mut self, out: &mut [u8]) -> Result<Option<usize>, HwError> {
        // The device writes the entries before the index: read the index,
        // then (after a barrier) the entries it covers.
        let device_used = self.read_u16(self.layout.used.saturating_add(2))?;
        dsb();
        let pending = device_used.wrapping_sub(self.next_used);
        if usize::from(pending) > N {
            // The device claims more entries than the queue has: drop them
            // all, unread.
            self.rejected = self.rejected.saturating_add(u32::from(pending));
            self.next_used = device_used;
            return Ok(None);
        }
        for _ in 0..pending {
            let slot = usize::from(self.next_used).checked_rem(N).unwrap_or(0);
            self.next_used = self.next_used.wrapping_add(1);
            let at = slot
                .checked_mul(USED_ENTRY_LEN)
                .and_then(|o| o.checked_add(self.layout.used.saturating_add(4)))
                .unwrap_or(usize::MAX);
            let mut entry = [0u8; USED_ENTRY_LEN];
            self.region.read(at, &mut entry)?;
            let (id, len) = entry.split_at(4);
            let id = usize::try_from(u32::from_le_bytes(id.try_into().unwrap_or_default()))
                .unwrap_or(usize::MAX);
            let len = usize::try_from(u32::from_le_bytes(len.try_into().unwrap_or_default()))
                .unwrap_or(usize::MAX);
            let held = self.with_device.get(id).copied().unwrap_or(false);
            if !held || len > self.layout.buffer_len || len > out.len() {
                self.rejected = self.rejected.saturating_add(1);
                continue;
            }
            let from = buffer_offset(&self.layout, id).unwrap_or(usize::MAX);
            self.region
                .read(from, out.get_mut(..len).unwrap_or_default())?;
            if let Some(flag) = self.with_device.get_mut(id) {
                *flag = false;
            }
            self.place(id)?;
            self.publish()?;
            return Ok(Some(len));
        }
        Ok(None)
    }

    /// How many used entries the device returned that failed their checks.
    #[must_use]
    pub const fn rejected(&self) -> u32 {
        self.rejected
    }

    /// Puts buffer `index` in the next available-ring slot; the device sees
    /// it once [`RxQueue::publish`] writes the index.
    fn place(&mut self, index: usize) -> Result<(), HwError> {
        let slot = usize::from(self.next_avail).checked_rem(N).unwrap_or(0);
        let at = slot
            .checked_mul(2)
            .and_then(|o| o.checked_add(self.layout.avail.saturating_add(4)))
            .unwrap_or(usize::MAX);
        let id = u16::try_from(index).unwrap_or(u16::MAX);
        self.region.write(at, &id.to_le_bytes())?;
        self.next_avail = self.next_avail.wrapping_add(1);
        if let Some(flag) = self.with_device.get_mut(index) {
            *flag = true;
        }
        Ok(())
    }

    /// Makes the placed entries visible: the entries before the index.
    fn publish(&mut self) -> Result<(), HwError> {
        dsb();
        self.region.write(
            self.layout.avail.saturating_add(2),
            &self.next_avail.to_le_bytes(),
        )?;
        dsb();
        Ok(())
    }

    fn read_u16(&self, at: usize) -> Result<u16, HwError> {
        let mut bytes = [0u8; 2];
        self.region.read(at, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }
}

impl<const N: usize> sealed::Sealed for RxQueue<N> {}

impl<const N: usize> Virtqueue for RxQueue<N> {
    fn size(&self) -> u32 {
        u32::try_from(N).unwrap_or(u32::MAX)
    }

    fn addresses(&self) -> Result<QueueAddresses, HwError> {
        RxQueue::addresses(self)
    }
}

// ---- Transmit queues ------------------------------------------------------------

/// A transmit queue of `N` buffers: the driver copies bytes into a free
/// buffer and offers it, device-readable; the device reads it and gives it
/// back. A driver never offers a device-writable buffer on a queue the
/// device only reads, such as a console's transmit queue (section 5.3.6.1).
#[derive(Debug)]
pub struct TxQueue<const N: usize> {
    region: DmaRegion,
    layout: Layout,
    /// The available ring's next index (free-running).
    next_avail: u16,
    /// The used ring's next entry to read (free-running).
    next_used: u16,
    /// Which buffers the device holds.
    with_device: [bool; N],
    rejected: u32,
}

impl<const N: usize> TxQueue<N> {
    /// A queue of `N` buffers of `buffer_len` bytes in `region`, with its
    /// rings zeroed and every descriptor naming its buffer, device-readable.
    /// Nothing is offered yet: [`TxQueue::send`] offers each buffer as it
    /// fills it.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if `N` is not a power of two from 1 to
    /// 32768, or the queue does not fit in the region.
    pub fn new(mut region: DmaRegion, buffer_len: usize) -> Result<Self, HwError> {
        let too_big = HwError::OutsideRegion {
            offset: 0,
            len: usize::MAX,
            size: region.len(),
        };
        if !N.is_power_of_two() || N > MAX_QUEUE_SIZE {
            return Err(too_big);
        }
        let layout = Layout::new(N, buffer_len).ok_or(too_big)?;
        if layout.end > region.len() {
            return Err(HwError::OutsideRegion {
                offset: 0,
                len: layout.end,
                size: region.len(),
            });
        }
        zero(&mut region, 0, layout.end)?;
        for index in 0..N {
            let offset = buffer_offset(&layout, index).ok_or(too_big)?;
            let address = region.device_address(offset, buffer_len)?;
            write_descriptor(
                &mut region,
                layout.desc,
                index,
                &Descriptor {
                    address,
                    len: 0,
                    flags: 0,
                    next: 0,
                },
            )?;
        }
        Ok(Self {
            region,
            layout,
            next_avail: 0,
            next_used: 0,
            with_device: [false; N],
            rejected: 0,
        })
    }

    /// The queue's layout in its region.
    #[must_use]
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// The device addresses of the descriptor table and the two rings.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`TxQueue::new`] made.
    pub fn addresses(&self) -> Result<QueueAddresses, HwError> {
        Ok(QueueAddresses {
            desc: self
                .region
                .device_address(self.layout.desc, self.layout.avail)?,
            driver: self.region.device_address(
                self.layout.avail,
                self.layout.used.saturating_sub(self.layout.avail),
            )?,
            device: self.region.device_address(
                self.layout.used,
                self.layout.buffers.saturating_sub(self.layout.used),
            )?,
        })
    }

    /// The buffers the device holds.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.with_device.iter().filter(|&&held| held).count()
    }

    /// Copies `bytes` into the first free buffer and offers it to the device
    /// to read; `Ok(false)` if the device holds every buffer. Notify the
    /// device afterwards.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if `bytes` is empty or longer than a
    /// buffer; nothing is offered.
    pub fn send(&mut self, bytes: &[u8]) -> Result<bool, HwError> {
        let refused = HwError::OutsideRegion {
            offset: 0,
            len: bytes.len(),
            size: self.layout.buffer_len,
        };
        if bytes.is_empty() || bytes.len() > self.layout.buffer_len {
            return Err(refused);
        }
        let Some(index) = self.with_device.iter().position(|&held| !held) else {
            return Ok(false);
        };
        let offset = buffer_offset(&self.layout, index).ok_or(refused)?;
        self.region.write(offset, bytes)?;
        let address = self.region.device_address(offset, bytes.len())?;
        let len = u32::try_from(bytes.len()).map_err(|_| refused)?;
        write_descriptor(
            &mut self.region,
            self.layout.desc,
            index,
            &Descriptor {
                address,
                len,
                flags: 0,
                next: 0,
            },
        )?;
        let slot = usize::from(self.next_avail).checked_rem(N).unwrap_or(0);
        let at = slot
            .checked_mul(2)
            .and_then(|o| o.checked_add(self.layout.avail.saturating_add(4)))
            .unwrap_or(usize::MAX);
        let id = u16::try_from(index).unwrap_or(u16::MAX);
        self.region.write(at, &id.to_le_bytes())?;
        self.next_avail = self.next_avail.wrapping_add(1);
        if let Some(flag) = self.with_device.get_mut(index) {
            *flag = true;
        }
        // The buffer, its descriptor and the ring entry before the index.
        dsb();
        self.region.write(
            self.layout.avail.saturating_add(2),
            &self.next_avail.to_le_bytes(),
        )?;
        dsb();
        Ok(true)
    }

    /// Takes back every buffer the device has finished reading, and returns
    /// how many. A used entry whose id names no buffer the device holds is
    /// dropped and counted ([`TxQueue::rejected`]), never followed; a used
    /// index that claims more entries than the queue has is dropped whole and
    /// counted.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`TxQueue::new`] made.
    pub fn reclaim(&mut self) -> Result<usize, HwError> {
        // The device writes the entries before the index: read the index,
        // then (after a barrier) the entries it covers.
        let mut index = [0u8; 2];
        self.region
            .read(self.layout.used.saturating_add(2), &mut index)?;
        let device_used = u16::from_le_bytes(index);
        dsb();
        let pending = device_used.wrapping_sub(self.next_used);
        if usize::from(pending) > N {
            self.rejected = self.rejected.saturating_add(u32::from(pending));
            self.next_used = device_used;
            return Ok(0);
        }
        let mut reclaimed = 0usize;
        for _ in 0..pending {
            let slot = usize::from(self.next_used).checked_rem(N).unwrap_or(0);
            self.next_used = self.next_used.wrapping_add(1);
            let at = slot
                .checked_mul(USED_ENTRY_LEN)
                .and_then(|o| o.checked_add(self.layout.used.saturating_add(4)))
                .unwrap_or(usize::MAX);
            // Only the id: the length a device writes into a buffer it
            // reads means nothing here.
            let mut id = [0u8; 4];
            self.region.read(at, &mut id)?;
            let id = usize::try_from(u32::from_le_bytes(id)).unwrap_or(usize::MAX);
            match self.with_device.get_mut(id) {
                Some(held) if *held => {
                    *held = false;
                    reclaimed = reclaimed.saturating_add(1);
                }
                _ => self.rejected = self.rejected.saturating_add(1),
            }
        }
        Ok(reclaimed)
    }

    /// How many used entries the device returned that failed their checks.
    #[must_use]
    pub const fn rejected(&self) -> u32 {
        self.rejected
    }
}

impl<const N: usize> sealed::Sealed for TxQueue<N> {}

impl<const N: usize> Virtqueue for TxQueue<N> {
    fn size(&self) -> u32 {
        u32::try_from(N).unwrap_or(u32::MAX)
    }

    fn addresses(&self) -> Result<QueueAddresses, HwError> {
        TxQueue::addresses(self)
    }
}

// ---- Chain queues ---------------------------------------------------------------

/// Where a chain queue's parts lie in its DMA region: the descriptor table
/// and the two rings, as for [`Layout`], then one header per chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainLayout {
    /// The descriptor table, at the region's start.
    pub desc: usize,
    /// The available ring.
    pub avail: usize,
    /// The used ring.
    pub used: usize,
    /// The first header; chain `i`'s is at `headers + i * header_len`.
    pub headers: usize,
    /// The size of each header.
    pub header_len: usize,
    /// One past the last byte used.
    pub end: usize,
}

impl ChainLayout {
    /// The layout of a queue of `chains` chains - twice as many
    /// descriptors - with headers of `header_len` bytes, or `None` if the
    /// sizes overflow.
    #[must_use]
    pub fn new(chains: usize, header_len: usize) -> Option<Self> {
        let rings = Layout::new(chains.checked_mul(2)?, 0)?;
        Some(Self {
            desc: rings.desc,
            avail: rings.avail,
            used: rings.used,
            headers: rings.buffers,
            header_len,
            end: rings.buffers.checked_add(chains.checked_mul(header_len)?)?,
        })
    }
}

/// A buffer in a chain queue's I/O region: its offset and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chain {
    /// Where the buffer starts in the I/O region.
    pub offset: usize,
    /// The bytes the device may write, or reads.
    pub len: usize,
}

/// A chain the device has finished with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsedChain {
    /// The buffer it held.
    pub buffer: Chain,
    /// The bytes the device says it wrote into the chain, header and
    /// buffer (0 for a chain it only reads). Not checked here: the caller
    /// checks it against the buffer.
    pub written: u32,
}

/// A queue of `N` chains of two descriptors: a header in the
/// queue's own DMA region, then a buffer in an [`IoRegion`] - memory this
/// PD hands to the device but never maps. virtio-net's queues are these:
/// its header in a descriptor of its own, the frame in the client's buffer.
///
/// Chain `i` is descriptors `2i` (header `i`, always) and `2i + 1` (the
/// buffer it is given), so the queue has `2N` descriptors. A buffer's
/// address is computed with [`IoRegion::device_address`], so it lies
/// inside the I/O region. Headers start zeroed: what virtio-net's transmit
/// header must be when no offload is negotiated.
///
/// **The device is not trusted.** Everything it writes - the used ring's
/// index, each entry's id and length, the headers - is copied out of DMA
/// memory before it is looked at. A used entry whose id does not name a
/// chain the device holds is dropped and counted ([`ChainQueue::rejected`]),
/// never followed; a used index that claims more entries than the queue has
/// is dropped whole and counted. The length it reports is handed to the
/// caller to check ([`UsedChain::written`]). The buffers themselves are
/// never read here.
#[derive(Debug)]
pub struct ChainQueue<const N: usize> {
    region: DmaRegion,
    buffers: IoRegion,
    layout: ChainLayout,
    /// [`DESC_F_WRITE`] for a queue the device writes, 0 for one it reads.
    write_flag: u16,
    /// The available ring's next index (free-running).
    next_avail: u16,
    /// The used ring's next entry to read (free-running).
    next_used: u16,
    /// The buffer each chain holds while the device has it.
    chains: [Option<Chain>; N],
    rejected: u32,
}

impl<const N: usize> ChainQueue<N> {
    /// A queue of `N` chains in `region`, with headers of `header_len` bytes
    /// and buffers in `buffers`, which the device writes if `device_writes`
    /// and reads otherwise. Its rings and headers are zeroed; nothing is
    /// offered yet.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if `2N` is not a power of two from 2 to
    /// 32768, or the queue does not fit in the region.
    pub fn new(
        mut region: DmaRegion,
        buffers: IoRegion,
        header_len: usize,
        device_writes: bool,
    ) -> Result<Self, HwError> {
        let too_big = HwError::OutsideRegion {
            offset: 0,
            len: usize::MAX,
            size: region.len(),
        };
        let size = N.checked_mul(2).ok_or(too_big)?;
        if N == 0 || !size.is_power_of_two() || size > MAX_QUEUE_SIZE {
            return Err(too_big);
        }
        let layout = ChainLayout::new(N, header_len).ok_or(too_big)?;
        if layout.end > region.len() {
            return Err(HwError::OutsideRegion {
                offset: 0,
                len: layout.end,
                size: region.len(),
            });
        }
        zero(&mut region, 0, layout.end)?;
        let write_flag = if device_writes { DESC_F_WRITE } else { 0 };
        let header_len_u32 = u32::try_from(header_len).map_err(|_| too_big)?;
        for chain in 0..N {
            let header = header_offset(&layout, chain).ok_or(too_big)?;
            let address = region.device_address(header, header_len)?;
            let head = chain.checked_mul(2).ok_or(too_big)?;
            let next = u16::try_from(head.saturating_add(1)).map_err(|_| too_big)?;
            write_descriptor(
                &mut region,
                layout.desc,
                head,
                &Descriptor {
                    address,
                    len: header_len_u32,
                    flags: DESC_F_NEXT | write_flag,
                    next,
                },
            )?;
        }
        Ok(Self {
            region,
            buffers,
            layout,
            write_flag,
            next_avail: 0,
            next_used: 0,
            chains: [None; N],
            rejected: 0,
        })
    }

    /// The queue's layout in its region.
    #[must_use]
    pub const fn layout(&self) -> ChainLayout {
        self.layout
    }

    /// The chains the device holds.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.chains.iter().filter(|chain| chain.is_some()).count()
    }

    /// Offers the device the `len` bytes at `offset` in the I/O region, in
    /// the first free chain, with its header zeroed: to fill, in a queue the
    /// device writes, or to read. `Ok(false)` if the device holds every
    /// chain. Notify the device afterwards.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the bytes are not all inside the I/O
    /// region; nothing is offered.
    pub fn offer(&mut self, offset: usize, len: usize) -> Result<bool, HwError> {
        let Some(chain) = self.chains.iter().position(Option::is_none) else {
            return Ok(false);
        };
        let address = self.buffers.device_address(offset, len)?;
        let outside = HwError::OutsideRegion {
            offset,
            len,
            size: self.buffers.len(),
        };
        let len_u32 = u32::try_from(len).map_err(|_| outside)?;
        let head = chain.checked_mul(2).ok_or(outside)?;
        let header = header_offset(&self.layout, chain).ok_or(outside)?;
        zero(&mut self.region, header, self.layout.header_len)?;
        write_descriptor(
            &mut self.region,
            self.layout.desc,
            head.saturating_add(1),
            &Descriptor {
                address,
                len: len_u32,
                flags: self.write_flag,
                next: 0,
            },
        )?;
        if let Some(slot) = self.chains.get_mut(chain) {
            *slot = Some(Chain { offset, len });
        }
        self.place(head)?;
        self.publish()?;
        Ok(true)
    }

    /// The next chain the device has finished with, with its header copied
    /// into the start of `header`; `None` when the device has returned
    /// nothing new. Used entries that fail their checks are dropped and
    /// counted, and the next one is tried.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`ChainQueue::new`]
    /// made.
    pub fn take(&mut self, header: &mut [u8]) -> Result<Option<UsedChain>, HwError> {
        let size = self.size_usize();
        // The device writes the entries before the index: read the index,
        // then (after a barrier) the entries it covers.
        let device_used = self.read_u16(self.layout.used.saturating_add(2))?;
        dsb();
        let pending = device_used.wrapping_sub(self.next_used);
        if usize::from(pending) > size {
            // The device claims more entries than the queue has: drop them
            // all, unread.
            self.rejected = self.rejected.saturating_add(u32::from(pending));
            self.next_used = device_used;
            return Ok(None);
        }
        for _ in 0..pending {
            let slot = usize::from(self.next_used).checked_rem(size).unwrap_or(0);
            self.next_used = self.next_used.wrapping_add(1);
            let at = slot
                .checked_mul(USED_ENTRY_LEN)
                .and_then(|o| o.checked_add(self.layout.used.saturating_add(4)))
                .unwrap_or(usize::MAX);
            let mut entry = [0u8; USED_ENTRY_LEN];
            self.region.read(at, &mut entry)?;
            let [i0, i1, i2, i3, w0, w1, w2, w3] = entry;
            let id = usize::try_from(u32::from_le_bytes([i0, i1, i2, i3])).unwrap_or(usize::MAX);
            let written = u32::from_le_bytes([w0, w1, w2, w3]);
            // Only the head of a chain - an even descriptor - names one.
            let chain = if id.is_multiple_of(2) {
                id.checked_div(2).unwrap_or(usize::MAX)
            } else {
                usize::MAX
            };
            let Some(buffer) = self.chains.get(chain).copied().flatten() else {
                self.rejected = self.rejected.saturating_add(1);
                continue;
            };
            let from = header_offset(&self.layout, chain).unwrap_or(usize::MAX);
            let len = self.layout.header_len.min(header.len());
            self.region
                .read(from, header.get_mut(..len).unwrap_or_default())?;
            if let Some(slot) = self.chains.get_mut(chain) {
                *slot = None;
            }
            return Ok(Some(UsedChain { buffer, written }));
        }
        Ok(None)
    }

    /// How many used entries the device returned that failed their checks.
    #[must_use]
    pub const fn rejected(&self) -> u32 {
        self.rejected
    }

    /// The queue's size in descriptors, `2N` (checked in `new`).
    const fn size_usize(&self) -> usize {
        N.saturating_mul(2)
    }

    /// Puts descriptor `head` in the next available-ring slot; the device
    /// sees it once [`ChainQueue::publish`] writes the index.
    fn place(&mut self, head: usize) -> Result<(), HwError> {
        let slot = usize::from(self.next_avail)
            .checked_rem(self.size_usize())
            .unwrap_or(0);
        let at = slot
            .checked_mul(2)
            .and_then(|o| o.checked_add(self.layout.avail.saturating_add(4)))
            .unwrap_or(usize::MAX);
        let id = u16::try_from(head).unwrap_or(u16::MAX);
        self.region.write(at, &id.to_le_bytes())?;
        self.next_avail = self.next_avail.wrapping_add(1);
        Ok(())
    }

    /// Makes the placed entries visible: the entries before the index.
    fn publish(&mut self) -> Result<(), HwError> {
        dsb();
        self.region.write(
            self.layout.avail.saturating_add(2),
            &self.next_avail.to_le_bytes(),
        )?;
        dsb();
        Ok(())
    }

    fn read_u16(&self, at: usize) -> Result<u16, HwError> {
        let mut bytes = [0u8; 2];
        self.region.read(at, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }
}

impl<const N: usize> sealed::Sealed for ChainQueue<N> {}

impl<const N: usize> Virtqueue for ChainQueue<N> {
    fn size(&self) -> u32 {
        u32::try_from(self.size_usize()).unwrap_or(u32::MAX)
    }

    fn addresses(&self) -> Result<QueueAddresses, HwError> {
        Ok(QueueAddresses {
            desc: self
                .region
                .device_address(self.layout.desc, self.layout.avail)?,
            driver: self.region.device_address(
                self.layout.avail,
                self.layout.used.saturating_sub(self.layout.avail),
            )?,
            device: self.region.device_address(
                self.layout.used,
                self.layout.headers.saturating_sub(self.layout.used),
            )?,
        })
    }
}

fn header_offset(layout: &ChainLayout, chain: usize) -> Option<usize> {
    chain
        .checked_mul(layout.header_len)?
        .checked_add(layout.headers)
}

/// Zeroes the `len` bytes at `at` in `region`.
fn zero(region: &mut DmaRegion, at: usize, len: usize) -> Result<(), HwError> {
    let zeros = [0u8; 64];
    let end = at.saturating_add(len);
    let mut from = at;
    while from < end {
        let chunk = end.saturating_sub(from).min(zeros.len());
        region.write(from, zeros.get(..chunk).unwrap_or_default())?;
        from = from.saturating_add(chunk);
    }
    Ok(())
}

/// One descriptor (section 2.7.5): a 64-bit address, a 32-bit length,
/// 16-bit flags and next.
#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Writes descriptor `index` of the descriptor table at `table` in
/// `region`.
fn write_descriptor(
    region: &mut DmaRegion,
    table: usize,
    index: usize,
    descriptor: &Descriptor,
) -> Result<(), HwError> {
    let Descriptor {
        address,
        len,
        flags,
        next,
    } = *descriptor;
    let [a0, a1, a2, a3, a4, a5, a6, a7] = address.to_le_bytes();
    let [l0, l1, l2, l3] = len.to_le_bytes();
    let [f0, f1] = flags.to_le_bytes();
    let [n0, n1] = next.to_le_bytes();
    let at = index
        .checked_mul(DESC_LEN)
        .and_then(|o| o.checked_add(table))
        .unwrap_or(usize::MAX);
    region.write(
        at,
        &[
            a0, a1, a2, a3, a4, a5, a6, a7, l0, l1, l2, l3, f0, f1, n0, n1,
        ],
    )
}

fn buffer_offset(layout: &Layout, index: usize) -> Option<usize> {
    index
        .checked_mul(layout.buffer_len)?
        .checked_add(layout.buffers)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! A simulated device: the tests write the used ring and the buffers in
    //! the region's memory, as a device would - well-formed or not. No
    //! registers, so these run under Miri too.

    use core::sync::atomic::AtomicU8;

    use super::*;

    const N: usize = 4;
    const BUFFER: usize = 8;
    const SIZE: usize = 512;
    const PADDR: u64 = 0x4100_0000;

    /// Zeroed test memory of its own for each place it is written: a static
    /// per expansion, so tests running in parallel never share one.
    macro_rules! memory {
        () => {{
            static MEMORY: [AtomicU8; SIZE] = [const { AtomicU8::new(0) }; SIZE];
            &MEMORY
        }};
    }

    struct Device {
        memory: &'static [AtomicU8; SIZE],
        layout: Layout,
        used_idx: u16,
    }

    impl Device {
        fn write(&self, at: usize, bytes: &[u8]) {
            for (cell, byte) in self.memory[at..at + bytes.len()].iter().zip(bytes) {
                cell.store(*byte, core::sync::atomic::Ordering::Relaxed);
            }
        }

        fn read(&self, at: usize, len: usize) -> Vec<u8> {
            self.memory[at..at + len]
                .iter()
                .map(|cell| cell.load(core::sync::atomic::Ordering::Relaxed))
                .collect()
        }

        /// Returns used entry (id, len), filling the buffer first when the
        /// id names one.
        fn returns(&mut self, id: u32, len: u32, fill: u8) {
            if (id as usize) < N {
                let at = self.layout.buffers + id as usize * BUFFER;
                self.write(at, &[fill; BUFFER]);
            }
            let slot = usize::from(self.used_idx) % N;
            let mut entry = [0u8; 8];
            entry[..4].copy_from_slice(&id.to_le_bytes());
            entry[4..].copy_from_slice(&len.to_le_bytes());
            self.write(self.layout.used + 4 + slot * 8, &entry);
            self.used_idx = self.used_idx.wrapping_add(1);
            self.write(self.layout.used + 2, &self.used_idx.to_le_bytes());
        }

        fn set_used_idx(&mut self, idx: u16) {
            self.used_idx = idx;
            self.write(self.layout.used + 2, &idx.to_le_bytes());
        }

        fn avail(&self) -> (u16, Vec<u16>) {
            let idx = u16::from_le_bytes(self.read(self.layout.avail + 2, 2).try_into().unwrap());
            let ring = (0..N)
                .map(|i| {
                    u16::from_le_bytes(
                        self.read(self.layout.avail + 4 + 2 * i, 2)
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect();
            (idx, ring)
        }
    }

    fn queue(memory: &'static [AtomicU8; SIZE]) -> (RxQueue<N>, Device) {
        let queue = RxQueue::<N>::new(DmaRegion::for_test(memory, PADDR), BUFFER).unwrap();
        let device = Device {
            memory,
            layout: queue.layout(),
            used_idx: 0,
        };
        (queue, device)
    }

    #[test]
    fn the_layout_follows_the_specification() {
        let layout = Layout::new(4, 8).unwrap();
        assert_eq!((layout.desc, layout.avail, layout.used), (0, 64, 80));
        assert_eq!((layout.buffers, layout.end), (128, 160));
        assert_eq!(Layout::new(usize::MAX, 8), None);
    }

    #[test]
    fn descriptors_name_their_buffers_device_writable() {
        let (queue, device) = queue(memory!());
        for i in 0..N {
            let desc = device.read(16 * i, 16);
            let addr = u64::from_le_bytes(desc[..8].try_into().unwrap());
            assert_eq!(addr, PADDR + (queue.layout().buffers + i * BUFFER) as u64);
            assert_eq!(
                u32::from_le_bytes(desc[8..12].try_into().unwrap()),
                BUFFER as u32
            );
            assert_eq!(
                u16::from_le_bytes(desc[12..14].try_into().unwrap()),
                DESC_F_WRITE
            );
        }
        let addresses = queue.addresses().unwrap();
        assert_eq!(addresses.desc, PADDR);
        assert_eq!(addresses.driver, PADDR + 64);
        assert_eq!(addresses.device, PADDR + 80);
    }

    #[test]
    fn filled_buffers_are_copied_out_and_offered_again() {
        let (mut queue, mut device) = queue(memory!());
        queue.offer_all().unwrap();
        assert_eq!(device.avail(), (4, vec![0, 1, 2, 3]));
        device.returns(2, 5, 0xAB);
        let mut out = [0u8; BUFFER];
        assert_eq!(queue.take(&mut out).unwrap(), Some(5));
        assert_eq!(out[..5], [0xAB; 5]);
        assert_eq!(
            device.avail(),
            (5, vec![2, 1, 2, 3]),
            "buffer 2 offered again"
        );
        assert_eq!(queue.take(&mut out).unwrap(), None);
        assert_eq!(queue.rejected(), 0);
    }

    #[test]
    fn malformed_used_entries_are_dropped_and_counted() {
        let (mut queue, mut device) = queue(memory!());
        queue.offer_all().unwrap();
        device.returns(7, 4, 0x11); // no such buffer
        device.returns(1, 9, 0x22); // longer than the buffer
        device.returns(0, 3, 0x33); // well formed
        let mut out = [0u8; BUFFER];
        assert_eq!(queue.take(&mut out).unwrap(), Some(3));
        assert_eq!(out[..3], [0x33; 3]);
        assert_eq!(queue.rejected(), 2);
        assert_eq!(queue.take(&mut out).unwrap(), None);
    }

    #[test]
    fn a_buffer_the_device_was_never_given_is_refused() {
        let (mut queue, mut device) = queue(memory!());
        device.returns(0, 4, 0x11); // nothing offered yet
        let mut out = [0u8; BUFFER];
        assert_eq!(queue.take(&mut out).unwrap(), None);
        assert_eq!(queue.rejected(), 1);
        assert_eq!(out, [0; BUFFER], "nothing copied");
    }

    #[test]
    fn a_used_index_beyond_the_queue_is_dropped_whole() {
        let (mut queue, mut device) = queue(memory!());
        queue.offer_all().unwrap();
        device.set_used_idx(100);
        let mut out = [0u8; BUFFER];
        assert_eq!(queue.take(&mut out).unwrap(), None);
        assert_eq!(queue.rejected(), 100);
        device.returns(1, 1, 0x99);
        assert_eq!(
            queue.take(&mut out).unwrap(),
            Some(1),
            "back in step after the drop"
        );
    }

    #[test]
    fn a_short_copy_is_refused_not_truncated() {
        let (mut queue, mut device) = queue(memory!());
        queue.offer_all().unwrap();
        device.returns(0, 8, 0x12);
        let mut out = [0u8; 4];
        assert_eq!(queue.take(&mut out).unwrap(), None);
        assert_eq!(queue.rejected(), 1);
    }

    #[test]
    fn queues_that_do_not_fit_are_refused() {
        assert!(RxQueue::<3>::new(DmaRegion::for_test(memory!(), PADDR), BUFFER).is_err());
        assert!(RxQueue::<64>::new(DmaRegion::for_test(memory!(), PADDR), BUFFER).is_err());
    }

    extern crate std;
    use std::vec;
    use std::vec::Vec;
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tx_tests {
    //! A simulated device for transmit queues: the tests read what the
    //! driver offered and write the used ring in the region's memory, as a
    //! device would - well-formed or not. No registers, so these run under
    //! Miri too.

    extern crate std;

    use core::sync::atomic::{AtomicU8, Ordering};
    use std::vec::Vec;

    use super::*;

    const N: usize = 4;
    const BUFFER: usize = 8;
    const SIZE: usize = 512;
    const PADDR: u64 = 0x4100_0000;

    /// Zeroed test memory of its own for each place it is written.
    macro_rules! memory {
        () => {{
            static MEMORY: [AtomicU8; SIZE] = [const { AtomicU8::new(0) }; SIZE];
            &MEMORY
        }};
    }

    struct Device {
        memory: &'static [AtomicU8; SIZE],
        layout: Layout,
        used_idx: u16,
        /// The available ring's next entry to read.
        next_avail: u16,
    }

    impl Device {
        fn write(&self, at: usize, bytes: &[u8]) {
            for (cell, byte) in self.memory[at..at + bytes.len()].iter().zip(bytes) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }

        fn read(&self, at: usize, len: usize) -> Vec<u8> {
            self.memory[at..at + len]
                .iter()
                .map(|cell| cell.load(Ordering::Relaxed))
                .collect()
        }

        fn u16_at(&self, at: usize) -> u16 {
            u16::from_le_bytes(self.read(at, 2).try_into().unwrap())
        }

        /// Reads every buffer offered since the last call, as a device
        /// would: by the descriptor each available entry names.
        fn take_offered(&mut self) -> Vec<(u16, Vec<u8>)> {
            let avail_idx = self.u16_at(self.layout.avail + 2);
            let mut taken = Vec::new();
            while self.next_avail != avail_idx {
                let slot = usize::from(self.next_avail) % N;
                let id = self.u16_at(self.layout.avail + 4 + 2 * slot);
                let desc = self.read(usize::from(id) * 16, 16);
                let address = u64::from_le_bytes(desc[..8].try_into().unwrap());
                let len = u32::from_le_bytes(desc[8..12].try_into().unwrap()) as usize;
                let flags = u16::from_le_bytes(desc[12..14].try_into().unwrap());
                assert_eq!(flags, 0, "device-readable, never chained");
                let at = usize::try_from(address - PADDR).unwrap();
                taken.push((id, self.read(at, len)));
                self.next_avail = self.next_avail.wrapping_add(1);
            }
            taken
        }

        /// Returns used entry (id, len).
        fn returns(&mut self, id: u32, len: u32) {
            let slot = usize::from(self.used_idx) % N;
            let mut entry = [0u8; 8];
            entry[..4].copy_from_slice(&id.to_le_bytes());
            entry[4..].copy_from_slice(&len.to_le_bytes());
            self.write(self.layout.used + 4 + slot * 8, &entry);
            self.used_idx = self.used_idx.wrapping_add(1);
            self.write(self.layout.used + 2, &self.used_idx.to_le_bytes());
        }
    }

    fn queue(memory: &'static [AtomicU8; SIZE]) -> (TxQueue<N>, Device) {
        let queue = TxQueue::<N>::new(DmaRegion::for_test(memory, PADDR), BUFFER).unwrap();
        let device = Device {
            memory,
            layout: queue.layout(),
            used_idx: 0,
            next_avail: 0,
        };
        (queue, device)
    }

    #[test]
    fn descriptors_name_their_buffers_device_readable() {
        let (queue, device) = queue(memory!());
        for i in 0..N {
            let desc = device.read(16 * i, 16);
            let addr = u64::from_le_bytes(desc[..8].try_into().unwrap());
            assert_eq!(addr, PADDR + (queue.layout().buffers + i * BUFFER) as u64);
            assert_eq!(u16::from_le_bytes(desc[12..14].try_into().unwrap()), 0);
        }
        assert_eq!(queue.addresses().unwrap().desc, PADDR);
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn bytes_sent_reach_the_device_in_order() {
        let (mut queue, mut device) = queue(memory!());
        assert_eq!(queue.send(b"hello"), Ok(true));
        assert_eq!(queue.send(b"world!!!"), Ok(true));
        let taken = device.take_offered();
        assert_eq!(taken, [(0, b"hello".to_vec()), (1, b"world!!!".to_vec())]);
        assert_eq!(queue.in_flight(), 2);
    }

    #[test]
    fn buffers_come_back_through_the_used_ring() {
        let (mut queue, mut device) = queue(memory!());
        for _ in 0..N {
            assert_eq!(queue.send(b"x"), Ok(true));
        }
        assert_eq!(queue.send(b"y"), Ok(false), "every buffer with the device");
        device.take_offered();
        device.returns(2, 0);
        device.returns(0, 0);
        assert_eq!(queue.reclaim(), Ok(2));
        assert_eq!(queue.in_flight(), 2);
        assert_eq!(queue.send(b"again"), Ok(true));
        assert_eq!(device.take_offered(), [(0, b"again".to_vec())]);
        assert_eq!(queue.rejected(), 0);
    }

    #[test]
    fn malformed_used_entries_are_dropped_and_counted() {
        let (mut queue, mut device) = queue(memory!());
        assert_eq!(queue.send(b"a"), Ok(true));
        device.take_offered();
        device.returns(9, 0); // no such buffer
        device.returns(3, 0); // a buffer the device was never given
        device.returns(0, 0); // well formed
        device.returns(0, 0); // the same buffer twice
        assert_eq!(queue.reclaim(), Ok(1));
        assert_eq!(queue.rejected(), 3);
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn a_used_index_beyond_the_queue_is_dropped_whole() {
        let (mut queue, mut device) = queue(memory!());
        device.used_idx = 50;
        device.write(device.layout.used + 2, &50u16.to_le_bytes());
        assert_eq!(queue.reclaim(), Ok(0));
        assert_eq!(queue.rejected(), 50);
    }

    #[test]
    fn empty_or_oversized_sends_are_refused_and_nothing_is_offered() {
        let (mut queue, mut device) = queue(memory!());
        assert!(queue.send(b"").is_err());
        assert!(queue.send(&[1; BUFFER + 1]).is_err());
        assert!(device.take_offered().is_empty());
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn queues_that_do_not_fit_are_refused() {
        assert!(TxQueue::<3>::new(DmaRegion::for_test(memory!(), PADDR), BUFFER).is_err());
        assert!(TxQueue::<64>::new(DmaRegion::for_test(memory!(), PADDR), BUFFER).is_err());
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod chain_tests {
    //! A simulated device for chain queues: the tests write the used ring
    //! and the headers in the region's memory, as a device would. No
    //! registers, so these run under Miri too.

    extern crate std;

    use core::sync::atomic::{AtomicU8, Ordering};
    use std::vec::Vec;

    use super::*;

    const CHAINS: usize = 4;
    const HEADER: usize = 12;
    const SIZE: usize = 512;
    const PADDR: u64 = 0x4100_0000;
    const IO_PADDR: u64 = 0x4200_0000;
    const IO_LEN: usize = 4 * 2048;

    /// Zeroed test memory of its own for each place it is written.
    macro_rules! memory {
        () => {{
            static MEMORY: [AtomicU8; SIZE] = [const { AtomicU8::new(0) }; SIZE];
            &MEMORY
        }};
    }

    struct Device {
        memory: &'static [AtomicU8; SIZE],
        layout: ChainLayout,
        used_idx: u16,
    }

    impl Device {
        fn write(&self, at: usize, bytes: &[u8]) {
            for (cell, byte) in self.memory[at..at + bytes.len()].iter().zip(bytes) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }

        fn read(&self, at: usize, len: usize) -> Vec<u8> {
            self.memory[at..at + len]
                .iter()
                .map(|cell| cell.load(Ordering::Relaxed))
                .collect()
        }

        fn descriptor(&self, index: usize) -> (u64, u32, u16, u16) {
            let bytes = self.read(16 * index, 16);
            (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
                u16::from_le_bytes(bytes[14..].try_into().unwrap()),
            )
        }

        fn avail(&self) -> (u16, Vec<u16>) {
            let idx = u16::from_le_bytes(self.read(self.layout.avail + 2, 2).try_into().unwrap());
            let ring = (0..2 * CHAINS)
                .map(|i| {
                    u16::from_le_bytes(
                        self.read(self.layout.avail + 4 + 2 * i, 2)
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect();
            (idx, ring)
        }

        /// Returns used entry (id, written), first writing `header` into the
        /// header of the chain the id names, if it names one.
        fn returns(&mut self, id: u32, written: u32, header: &[u8]) {
            if id.is_multiple_of(2) && (id as usize) < 2 * CHAINS {
                self.write(self.layout.headers + (id as usize / 2) * HEADER, header);
            }
            let slot = usize::from(self.used_idx) % (2 * CHAINS);
            let mut entry = [0u8; 8];
            entry[..4].copy_from_slice(&id.to_le_bytes());
            entry[4..].copy_from_slice(&written.to_le_bytes());
            self.write(self.layout.used + 4 + slot * 8, &entry);
            self.used_idx = self.used_idx.wrapping_add(1);
            self.write(self.layout.used + 2, &self.used_idx.to_le_bytes());
        }
    }

    fn queue(
        memory: &'static [AtomicU8; SIZE],
        device_writes: bool,
    ) -> (ChainQueue<CHAINS>, Device) {
        let queue = ChainQueue::<CHAINS>::new(
            DmaRegion::for_test(memory, PADDR),
            IoRegion::for_test(IO_PADDR, IO_LEN),
            HEADER,
            device_writes,
        )
        .unwrap();
        let device = Device {
            memory,
            layout: queue.layout(),
            used_idx: 0,
        };
        (queue, device)
    }

    #[test]
    fn the_layout_follows_the_specification() {
        let layout = ChainLayout::new(4, 12).unwrap();
        assert_eq!((layout.desc, layout.avail, layout.used), (0, 128, 152));
        assert_eq!((layout.headers, layout.end), (224, 272));
        assert_eq!(ChainLayout::new(usize::MAX, 12), None);
    }

    #[test]
    fn each_chain_is_its_header_then_a_buffer() {
        let (queue, device) = queue(memory!(), true);
        for chain in 0..CHAINS {
            assert_eq!(
                device.descriptor(2 * chain),
                (
                    PADDR + 224 + (chain * HEADER) as u64,
                    HEADER as u32,
                    DESC_F_NEXT | DESC_F_WRITE,
                    (2 * chain + 1) as u16
                )
            );
        }
        assert_eq!(queue.size(), 8);
        let addresses = Virtqueue::addresses(&queue).unwrap();
        assert_eq!(
            (addresses.desc, addresses.driver, addresses.device),
            (PADDR, PADDR + 128, PADDR + 152)
        );
    }

    #[test]
    fn a_buffer_is_offered_by_its_checked_address() {
        let (mut queue, device) = queue(memory!(), true);
        assert_eq!(queue.offer(2048, 2048), Ok(true));
        assert_eq!(
            device.descriptor(1),
            (IO_PADDR + 2048, 2048, DESC_F_WRITE, 0)
        );
        assert_eq!(device.avail(), (1, vec![0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(queue.offer(4096, 2048), Ok(true));
        assert_eq!(device.avail().0, 2);
        assert_eq!(device.avail().1[1], 2, "chain 1's head");
        // Outside the I/O region: refused, nothing offered.
        assert_eq!(
            queue.offer(3 * 2048, 2049),
            Err(HwError::OutsideRegion {
                offset: 3 * 2048,
                len: 2049,
                size: IO_LEN
            })
        );
        assert_eq!(device.avail().0, 2);
        assert_eq!(queue.in_flight(), 2);
        assert_eq!(queue.offer(0, 1), Ok(true));
        assert_eq!(queue.offer(0, 1), Ok(true));
        assert_eq!(queue.offer(0, 1), Ok(false), "every chain with the device");
    }

    #[test]
    fn a_used_chain_comes_back_with_its_header_copied_out() {
        let (mut queue, mut device) = queue(memory!(), true);
        queue.offer(2048, 2048).unwrap();
        device.returns(0, 12 + 60, &[0xab; HEADER]);
        let mut header = [0u8; HEADER];
        assert_eq!(
            queue.take(&mut header),
            Ok(Some(UsedChain {
                buffer: Chain {
                    offset: 2048,
                    len: 2048
                },
                written: 72
            }))
        );
        assert_eq!(header, [0xab; HEADER]);
        assert_eq!(queue.take(&mut header), Ok(None));
        assert_eq!((queue.in_flight(), queue.rejected()), (0, 0));
        // The chain is free again, and its header zeroed when reused.
        queue.offer(0, 2048).unwrap();
        assert_eq!(device.read(224, HEADER), [0; HEADER]);
    }

    #[test]
    fn used_entries_that_name_no_chain_the_device_holds_are_dropped() {
        let (mut queue, mut device) = queue(memory!(), true);
        queue.offer(0, 2048).unwrap();
        device.returns(1, 72, &[]); // not the head of a chain
        device.returns(2, 72, &[0x11; HEADER]); // chain 1: never offered
        device.returns(100, 72, &[]); // no such descriptor
        device.returns(0, 72, &[0x22; HEADER]); // well formed
        let mut header = [0u8; HEADER];
        let used = queue.take(&mut header).unwrap().unwrap();
        assert_eq!(used.buffer.offset, 0);
        assert_eq!(header, [0x22; HEADER]);
        assert_eq!(queue.rejected(), 3);
        device.returns(0, 72, &[]); // chain 0 again, but it was returned
        assert_eq!(queue.take(&mut header), Ok(None));
        assert_eq!(queue.rejected(), 4);
    }

    #[test]
    fn a_used_index_beyond_the_queue_is_dropped_whole() {
        let (mut queue, mut device) = queue(memory!(), true);
        queue.offer(0, 2048).unwrap();
        device.used_idx = 100;
        device.write(device.layout.used + 2, &100u16.to_le_bytes());
        let mut header = [0u8; HEADER];
        assert_eq!(queue.take(&mut header), Ok(None));
        assert_eq!(queue.rejected(), 100);
        device.returns(0, 72, &[]);
        assert!(queue.take(&mut header).unwrap().is_some(), "back in step");
    }

    #[test]
    fn a_queue_the_device_reads_never_marks_a_descriptor_writable() {
        let (mut queue, device) = queue(memory!(), false);
        assert_eq!(device.descriptor(0).2, DESC_F_NEXT);
        queue.offer(2048, 60).unwrap();
        assert_eq!(device.descriptor(1), (IO_PADDR + 2048, 60, 0, 0));
    }

    #[test]
    fn queues_that_do_not_fit_are_refused() {
        let io = || IoRegion::for_test(IO_PADDR, IO_LEN);
        assert!(
            ChainQueue::<3>::new(DmaRegion::for_test(memory!(), PADDR), io(), 12, true).is_err()
        );
        assert!(
            ChainQueue::<0>::new(DmaRegion::for_test(memory!(), PADDR), io(), 12, true).is_err()
        );
        assert!(
            ChainQueue::<16>::new(DmaRegion::for_test(memory!(), PADDR), io(), 12, true).is_err()
        );
    }

    use std::vec;
}
