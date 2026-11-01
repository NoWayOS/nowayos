// SPDX-License-Identifier: MIT OR Apache-2.0
//! virtio-net, the network card: its receive and transmit queues,
//! for a driver PD that passes frames to and from one client through
//! sDDF's queues and never maps the packet buffers.
//!
//! Written from the virtio 1.2 specification (OASIS, "Virtual I/O Device
//! (VIRTIO) Version 1.2"), section 5.1: device ID 1; the feature
//! `VIRTIO_NET_F_MAC` (bit 5) and the configuration's `mac`; receiveq1
//! (virtqueue 0) and transmitq1 (virtqueue 1); and `struct virtio_net_hdr`
//! before every frame, 12 bytes on a modern device without
//! `VIRTIO_NET_F_HASH_REPORT`.
//!
//! Each frame is a [`ChainQueue`] chain: the header in the driver's own DMA
//! memory, in a descriptor of its own, then the frame in a packet buffer of
//! the client's data region, an [`IoRegion`]. The driver negotiates no
//! offload - no checksum, segmentation or merged receive buffers - so:
//!
//! * a transmitted frame's header is all zeros (section 5.1.6.2: no
//!   checksum to complete, `gso_type` `VIRTIO_NET_HDR_GSO_NONE`,
//!   `num_buffers` 0), and its length runs from an Ethernet header to
//!   [`MAX_FRAME_LEN`];
//! * a received frame is checked before it is passed on
//!   ([`NetRx::receive`]): the length the device reports must fit the chain
//!   and leave a frame of [`MIN_FRAME_LEN`] to [`MAX_FRAME_LEN`] bytes, and
//!   its header must neither ask for a checksum to be completed
//!   (`VIRTIO_NET_HDR_F_NEEDS_CSUM`) nor carry a segmentation type, which a
//!   device without those features must not send (section 5.1.6.4). A frame
//!   that fails is dropped and counted - oversize or malformed - and its
//!   buffer offered to the device again. The header's other fields are
//!   ignored; `num_buffers` means nothing without merged buffers.
//!
//! The frames' bytes are never read here: this PD does not map them. This
//! module has no `unsafe` code.

use crate::virtqueue::{ChainQueue, UsedChain};
use crate::{DmaRegion, HwError, IoRegion};

/// The network card's device ID.
pub const DEVICE_ID: u32 = crate::virtio::DEVICE_NET;

/// `VIRTIO_NET_F_MAC`, feature bit 5: the configuration holds the card's
/// MAC address.
pub const F_MAC: u64 = 1 << 5;

/// The receive queue, receiveq1.
pub const RECEIVE_QUEUE: u32 = 0;

/// The transmit queue, transmitq1.
pub const TRANSMIT_QUEUE: u32 = 1;

/// Where `mac` is in the device-specific configuration, and its length.
pub const MAC_OFFSET: usize = 0;

/// The bytes of `struct virtio_net_hdr` before every frame.
pub const HEADER_LEN: usize = 12;

/// The shortest frame passed on: an Ethernet header.
pub const MIN_FRAME_LEN: usize = 14;

/// The longest frame passed on: an Ethernet header and a 1500-byte payload
/// (section 5.1.6.3's 1526-byte receive buffers, less the header).
pub const MAX_FRAME_LEN: usize = 1514;

/// `VIRTIO_NET_HDR_F_NEEDS_CSUM`: the device left a checksum to complete.
const HDR_F_NEEDS_CSUM: u8 = 1;

/// `VIRTIO_NET_HDR_GSO_NONE`: not a segmentation offload.
const HDR_GSO_NONE: u8 = 0;

/// What [`NetRx::receive`] got from the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// A frame that passed its checks: its buffer's offset in the data
    /// region, and its length.
    Frame {
        /// The buffer's offset.
        offset: usize,
        /// The frame's length.
        len: usize,
    },
    /// A frame dropped and counted; its buffer, which the driver still
    /// holds, should be offered again.
    Dropped {
        /// The buffer's offset.
        offset: usize,
    },
}

/// The receive queue: the client's empty buffers offered to the device,
/// filled frames checked as they come back.
#[derive(Debug)]
pub struct NetRx<const N: usize> {
    queue: ChainQueue<N>,
    buffer_len: usize,
    oversize: u32,
    malformed: u32,
}

impl<const N: usize> NetRx<N> {
    /// A receive queue of `N` chains in `region`, filling buffers of
    /// `buffer_len` bytes in `buffers`.
    ///
    /// # Errors
    ///
    /// [`HwError::FrameLength`] if a buffer cannot hold the longest frame;
    /// otherwise as for [`ChainQueue::new`].
    pub fn new(region: DmaRegion, buffers: IoRegion, buffer_len: usize) -> Result<Self, HwError> {
        if buffer_len < MAX_FRAME_LEN {
            return Err(HwError::FrameLength { len: buffer_len });
        }
        Ok(Self {
            queue: ChainQueue::new(region, buffers, HEADER_LEN, true)?,
            buffer_len,
            oversize: 0,
            malformed: 0,
        })
    }

    /// The queue, to hand to
    /// [`VirtioMmio::set_up_queue`](crate::virtio::VirtioMmio::set_up_queue).
    #[must_use]
    pub const fn queue(&self) -> &ChainQueue<N> {
        &self.queue
    }

    /// Offers the device the buffer at `offset` to fill. `Ok(false)` if it
    /// already holds `N`. Notify the device afterwards.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the buffer is not inside the data
    /// region.
    pub fn offer(&mut self, offset: usize) -> Result<bool, HwError> {
        self.queue.offer(offset, self.buffer_len)
    }

    /// The next frame the device has returned, checked; `None` when it has
    /// returned nothing new. A dropped frame's buffer comes back as
    /// [`Received::Dropped`], to offer again.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`NetRx::new`] made.
    pub fn receive(&mut self) -> Result<Option<Received>, HwError> {
        let mut header = [0u8; HEADER_LEN];
        let Some(used) = self.queue.take(&mut header)? else {
            return Ok(None);
        };
        let offset = used.buffer.offset;
        Ok(Some(match check(&used, &header) {
            Ok(len) => Received::Frame { offset, len },
            Err(Refusal::Oversize) => {
                self.oversize = self.oversize.saturating_add(1);
                Received::Dropped { offset }
            }
            Err(Refusal::Malformed) => {
                self.malformed = self.malformed.saturating_add(1);
                Received::Dropped { offset }
            }
        }))
    }

    /// Frames dropped for being longer than [`MAX_FRAME_LEN`].
    #[must_use]
    pub const fn oversize(&self) -> u32 {
        self.oversize
    }

    /// Frames dropped as malformed: a length that does not fit the chain
    /// or is shorter than an Ethernet header, or a header the device must
    /// not send.
    #[must_use]
    pub const fn malformed(&self) -> u32 {
        self.malformed
    }

    /// Used entries dropped because they named no chain the device holds
    /// ([`ChainQueue::rejected`]).
    #[must_use]
    pub const fn rejected(&self) -> u32 {
        self.queue.rejected()
    }
}

/// Why a received frame was dropped.
enum Refusal {
    Oversize,
    Malformed,
}

/// The frame's length, if a used receive chain and its header pass their
/// checks.
fn check(used: &UsedChain, header: &[u8; HEADER_LEN]) -> Result<usize, Refusal> {
    let written = usize::try_from(used.written).map_err(|_| Refusal::Malformed)?;
    if written > HEADER_LEN.saturating_add(used.buffer.len) {
        // More than the chain holds: the device's report is false.
        return Err(Refusal::Malformed);
    }
    let len = written.checked_sub(HEADER_LEN).ok_or(Refusal::Malformed)?;
    if len > MAX_FRAME_LEN {
        return Err(Refusal::Oversize);
    }
    if len < MIN_FRAME_LEN {
        return Err(Refusal::Malformed);
    }
    let [flags, gso_type, ..] = *header;
    if flags & HDR_F_NEEDS_CSUM != 0 || gso_type != HDR_GSO_NONE {
        return Err(Refusal::Malformed);
    }
    Ok(len)
}

/// The transmit queue: the client's frames given to the device, their
/// buffers taken back once sent.
#[derive(Debug)]
pub struct NetTx<const N: usize> {
    queue: ChainQueue<N>,
}

impl<const N: usize> NetTx<N> {
    /// A transmit queue of `N` chains in `region`, sending from buffers in
    /// `buffers`.
    ///
    /// # Errors
    ///
    /// As for [`ChainQueue::new`].
    pub fn new(region: DmaRegion, buffers: IoRegion) -> Result<Self, HwError> {
        Ok(Self {
            queue: ChainQueue::new(region, buffers, HEADER_LEN, false)?,
        })
    }

    /// The queue, to hand to
    /// [`VirtioMmio::set_up_queue`](crate::virtio::VirtioMmio::set_up_queue).
    #[must_use]
    pub const fn queue(&self) -> &ChainQueue<N> {
        &self.queue
    }

    /// Gives the device the `len`-byte frame at `offset` to send.
    /// `Ok(false)` if it already holds `N`. Notify the device afterwards.
    ///
    /// # Errors
    ///
    /// [`HwError::FrameLength`] unless `len` runs from [`MIN_FRAME_LEN`] to
    /// [`MAX_FRAME_LEN`]; [`HwError::OutsideRegion`] if the frame is not
    /// inside the data region. Nothing is sent.
    pub fn send(&mut self, offset: usize, len: usize) -> Result<bool, HwError> {
        if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len) {
            return Err(HwError::FrameLength { len });
        }
        self.queue.offer(offset, len)
    }

    /// The offset of the next buffer the device has sent; `None` when it
    /// has returned nothing new.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`]: never, for a queue [`NetTx::new`] made.
    pub fn reclaim(&mut self) -> Result<Option<usize>, HwError> {
        let mut header = [0u8; HEADER_LEN];
        Ok(self.queue.take(&mut header)?.map(|used| used.buffer.offset))
    }

    /// Used entries dropped because they named no chain the device holds.
    #[must_use]
    pub const fn rejected(&self) -> u32 {
        self.queue.rejected()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! The receive checks against a simulated device writing its used ring
    //! and headers - well-formed or not. No registers, so these run under
    //! Miri too.

    use core::sync::atomic::{AtomicU8, Ordering};

    use super::*;
    use crate::virtqueue::ChainLayout;

    const CHAINS: usize = 4;
    const SIZE: usize = 512;
    const BUFFER: usize = 2048;

    macro_rules! memory {
        () => {{
            static MEMORY: [AtomicU8; SIZE] = [const { AtomicU8::new(0) }; SIZE];
            &MEMORY
        }};
    }

    /// The device's side: it returns chain `chain` having written `written`
    /// bytes and the header `header`.
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

        fn returns(&mut self, chain: usize, written: u32, header: [u8; HEADER_LEN]) {
            self.write(self.layout.headers + chain * HEADER_LEN, &header);
            let slot = usize::from(self.used_idx) % (2 * CHAINS);
            let mut entry = [0u8; 8];
            entry[..4].copy_from_slice(&((2 * chain) as u32).to_le_bytes());
            entry[4..].copy_from_slice(&written.to_le_bytes());
            self.write(self.layout.used + 4 + slot * 8, &entry);
            self.used_idx = self.used_idx.wrapping_add(1);
            self.write(self.layout.used + 2, &self.used_idx.to_le_bytes());
        }
    }

    fn receiver(memory: &'static [AtomicU8; SIZE]) -> (NetRx<CHAINS>, Device) {
        let rx = NetRx::<CHAINS>::new(
            DmaRegion::for_test(memory, 0x4100_0000),
            IoRegion::for_test(0x4200_0000, CHAINS * BUFFER),
            BUFFER,
        )
        .unwrap();
        let device = Device {
            memory,
            layout: rx.queue().layout(),
            used_idx: 0,
        };
        (rx, device)
    }

    const PLAIN: [u8; HEADER_LEN] = [0; HEADER_LEN];

    #[test]
    fn a_well_formed_frame_is_passed_on() {
        let (mut rx, mut device) = receiver(memory!());
        assert_eq!(rx.offer(2048), Ok(true));
        device.returns(0, (HEADER_LEN + 64) as u32, PLAIN);
        assert_eq!(
            rx.receive(),
            Ok(Some(Received::Frame {
                offset: 2048,
                len: 64
            }))
        );
        assert_eq!(rx.receive(), Ok(None));
        assert_eq!((rx.oversize(), rx.malformed(), rx.rejected()), (0, 0, 0));
    }

    #[test]
    fn the_longest_and_shortest_frames_pass_and_no_further() {
        let (mut rx, mut device) = receiver(memory!());
        for offset in [0, 2048, 4096, 6144] {
            rx.offer(offset).unwrap();
        }
        device.returns(0, (HEADER_LEN + MAX_FRAME_LEN) as u32, PLAIN);
        device.returns(1, (HEADER_LEN + MIN_FRAME_LEN) as u32, PLAIN);
        device.returns(2, (HEADER_LEN + MAX_FRAME_LEN + 1) as u32, PLAIN);
        device.returns(3, (HEADER_LEN + MIN_FRAME_LEN - 1) as u32, PLAIN);
        assert_eq!(
            rx.receive(),
            Ok(Some(Received::Frame {
                offset: 0,
                len: MAX_FRAME_LEN
            }))
        );
        assert_eq!(
            rx.receive(),
            Ok(Some(Received::Frame {
                offset: 2048,
                len: MIN_FRAME_LEN
            }))
        );
        assert_eq!(rx.receive(), Ok(Some(Received::Dropped { offset: 4096 })));
        assert_eq!(rx.receive(), Ok(Some(Received::Dropped { offset: 6144 })));
        assert_eq!((rx.oversize(), rx.malformed()), (1, 1));
    }

    #[test]
    fn malformed_frames_are_dropped_counted_and_their_buffers_kept() {
        let (mut rx, mut device) = receiver(memory!());
        for offset in [0, 2048, 4096, 6144] {
            rx.offer(offset).unwrap();
        }
        let mut needs_csum = PLAIN;
        needs_csum[0] = HDR_F_NEEDS_CSUM;
        let mut segmented = PLAIN;
        segmented[1] = 1; // VIRTIO_NET_HDR_GSO_TCPV4
        device.returns(0, (HEADER_LEN + 60) as u32, needs_csum);
        device.returns(1, (HEADER_LEN + 60) as u32, segmented);
        // More than the chain holds: 12 + 2049 bytes.
        device.returns(2, (HEADER_LEN + BUFFER + 1) as u32, PLAIN);
        // Less than a header.
        device.returns(3, 4, PLAIN);
        for offset in [0, 2048, 4096, 6144] {
            assert_eq!(rx.receive(), Ok(Some(Received::Dropped { offset })));
        }
        assert_eq!((rx.oversize(), rx.malformed()), (0, 4));
        // The buffers come back to be offered again.
        for offset in [0, 2048, 4096, 6144] {
            assert_eq!(rx.offer(offset), Ok(true));
        }
    }

    #[test]
    fn the_header_fields_a_plain_frame_may_carry_are_ignored() {
        let (mut rx, mut device) = receiver(memory!());
        rx.offer(0).unwrap();
        let mut header = [0xff; HEADER_LEN];
        header[0] = !HDR_F_NEEDS_CSUM; // VIRTIO_NET_HDR_F_DATA_VALID and unknown bits
        header[1] = HDR_GSO_NONE;
        device.returns(0, (HEADER_LEN + 60) as u32, header);
        assert_eq!(
            rx.receive(),
            Ok(Some(Received::Frame { offset: 0, len: 60 }))
        );
    }

    #[test]
    fn a_receive_buffer_must_hold_the_longest_frame() {
        assert_eq!(
            NetRx::<CHAINS>::new(
                DmaRegion::for_test(memory!(), 0x4100_0000),
                IoRegion::for_test(0x4200_0000, CHAINS * BUFFER),
                MAX_FRAME_LEN - 1,
            )
            .err(),
            Some(HwError::FrameLength {
                len: MAX_FRAME_LEN - 1
            })
        );
    }

    #[test]
    fn frames_are_sent_from_the_data_region_and_their_buffers_come_back() {
        let memory = memory!();
        let mut tx = NetTx::<CHAINS>::new(
            DmaRegion::for_test(memory, 0x4100_0000),
            IoRegion::for_test(0x4200_0000, CHAINS * BUFFER),
        )
        .unwrap();
        let mut device = Device {
            memory,
            layout: tx.queue().layout(),
            used_idx: 0,
        };
        for len in [0, MIN_FRAME_LEN - 1, MAX_FRAME_LEN + 1] {
            assert_eq!(tx.send(0, len), Err(HwError::FrameLength { len }));
        }
        assert!(matches!(
            tx.send(4 * BUFFER, 60),
            Err(HwError::OutsideRegion { .. })
        ));
        assert_eq!(tx.send(2048, 60), Ok(true));
        assert_eq!(tx.reclaim(), Ok(None));
        device.returns(0, 0, PLAIN);
        assert_eq!(tx.reclaim(), Ok(Some(2048)));
        assert_eq!(tx.reclaim(), Ok(None));
        assert_eq!(tx.rejected(), 0);
    }
}
