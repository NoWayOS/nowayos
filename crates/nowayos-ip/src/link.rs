// SPDX-License-Identifier: MIT OR Apache-2.0
//! The link: smoltcp's [`Device`] over the NIC driver's four sDDF queues.
//!
//! The stack PD is the driver's one client. It holds its receive buffers
//! read-only and its transmit buffers read-write, and the driver hands both
//! to the card by physical address. Nothing the driver writes is trusted:
//!
//! * every descriptor is copied out of its queue before it is looked at
//!   (`nowayos_net::Consumer`), and a driver index that would put more than
//!   a queue's capacity in flight is counted and nothing more is read from
//!   that queue this round;
//! * a received frame's descriptor must name a receive buffer - inside the
//!   region, on a buffer boundary - that the stack gave the driver and has
//!   not had back; a frame longer than [`MAX_FRAME_LEN`] is **oversize**,
//!   and anything else that fails - a bad offset, a buffer the driver did
//!   not hold, fewer bytes than an Ethernet header - is **malformed**. Each
//!   is dropped and counted ([`Counts`]), and a buffer that was the
//!   driver's is given back to it;
//! * a frame that passes is **copied into private memory before smoltcp
//!   sees it**, and its buffer given straight back: smoltcp parses the copy
//!   ([`RxToken`]), so the driver - or the card - changing the shared buffer
//!   afterwards changes nothing it reads;
//! * a sent buffer coming back must name a transmit buffer the stack gave
//!   the driver; anything else is counted as malformed.
//!
//! Each [`Device::receive`] skips at most a queue's worth of bad
//! descriptors, so a driver that keeps writing them cannot hold the stack
//! in one call. The driver is notified by sDDF's protocol: after enqueueing,
//! only if it asked ([`Link::wake_driver`]); before waiting, the stack asks
//! for its own notifications and looks once more ([`Link::request_signals`]).

use core::sync::atomic::{AtomicU8, Ordering};

use nowayos_net::{
    BufferSet, Consumer, DataRegion, Descriptor, ETHERNET_HEADER_LEN, MAX_FRAME_LEN, Producer,
    QueueError, QueueMemory,
};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant as NetInstant;

/// The receive buffers, as the stack PD sees them: read-only.
pub trait RxMemory {
    /// Copies the bytes at `offset` into `out`. Returns `false`, copying
    /// nothing, if they are not all inside the buffers.
    fn load(&self, offset: usize, out: &mut [u8]) -> bool;
}

/// The transmit buffers, as the stack PD sees them: read-write.
pub trait TxMemory {
    /// Copies `bytes` to `offset`. Returns `false`, copying nothing, if they
    /// do not all fit.
    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool;
}

impl<M: RxMemory + ?Sized> RxMemory for &M {
    fn load(&self, offset: usize, out: &mut [u8]) -> bool {
        (**self).load(offset, out)
    }
}

/// Shared memory another side may write at any moment: relaxed byte loads.
impl RxMemory for [AtomicU8] {
    fn load(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some(span) = offset
            .checked_add(out.len())
            .and_then(|end| self.get(offset..end))
        else {
            return false;
        };
        for (byte, cell) in out.iter_mut().zip(span) {
            *byte = cell.load(Ordering::Relaxed);
        }
        true
    }
}

/// Relaxed byte stores: the bytes reach the driver when the descriptor that
/// names them is published, with release ordering.
impl TxMemory for &[AtomicU8] {
    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let Some(span) = offset
            .checked_add(bytes.len())
            .and_then(|end| self.get(offset..end))
        else {
            return false;
        };
        for (cell, byte) in span.iter().zip(bytes) {
            cell.store(*byte, Ordering::Relaxed);
        }
        true
    }
}

/// The four queues with the driver, as the stack sees them.
#[derive(Debug)]
pub struct Queues<Q> {
    /// Empty receive buffers, to the driver.
    pub rx_free: Q,
    /// Received frames, from the driver.
    pub rx_active: Q,
    /// Sent transmit buffers, from the driver.
    pub tx_free: Q,
    /// Frames to send, to the driver.
    pub tx_active: Q,
}

/// What the link has passed on and dropped since it started. Every count
/// saturates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Frames copied out and handed to smoltcp.
    pub received: u32,
    /// Frames handed to the driver to send.
    pub sent: u32,
    /// Received frames longer than [`MAX_FRAME_LEN`]: dropped.
    pub oversize: u32,
    /// Descriptors from the driver that failed their checks - outside the
    /// region, off a buffer boundary, a buffer the driver did not hold, or
    /// a frame shorter than an Ethernet header: dropped.
    pub malformed: u32,
    /// Times a driver index was out of range: nothing more was read from
    /// that queue that round.
    pub indices: u32,
    /// Frames smoltcp built that could not be sent - no transmit buffer, a
    /// length out of range, or a full queue: dropped.
    pub unsent: u32,
}

impl Counts {
    /// What was dropped: the counts [`Counts::received`] and
    /// [`Counts::sent`] left out, which change with every frame.
    #[must_use]
    pub const fn dropped(&self) -> Self {
        Self {
            received: 0,
            sent: 0,
            ..*self
        }
    }
}

/// Why a link could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkError {
    /// A queue's capacity is not a power of two up to `nowayos_net`'s
    /// maximum.
    Queue(QueueError),
    /// The link has no buffers, or more than the queues can hold.
    Buffers,
}

impl core::fmt::Display for LinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Queue(error) => write!(f, "a queue: {error}"),
            Self::Buffers => f.write_str("the buffers do not match the queues"),
        }
    }
}

impl From<QueueError> for LinkError {
    fn from(error: QueueError) -> Self {
        Self::Queue(error)
    }
}

/// The receive side: the queues the driver passes frames on, the buffers,
/// and the private copy smoltcp parses.
struct Receiver<Q, R, const N: usize> {
    free: Producer<Q>,
    active: Consumer<Q>,
    buffers: R,
    region: DataRegion,
    /// Receive buffers the stack holds; the rest are the driver's.
    held: BufferSet<N>,
    /// The frame smoltcp sees: copied out of the shared buffer.
    copy: [u8; MAX_FRAME_LEN],
    /// Whether a buffer was given to the driver since it was last notified.
    wake: bool,
    received: u32,
    oversize: u32,
    malformed: u32,
    indices: u32,
}

impl<Q: QueueMemory, R: RxMemory, const N: usize> Receiver<Q, R, N> {
    /// Gives the driver every receive buffer the stack holds.
    fn refill(&mut self) {
        for buffer in 0..N {
            if self.held.contains(buffer) {
                self.give(buffer);
            }
        }
    }

    /// Gives buffer `buffer` to the driver, empty. It stays the stack's if
    /// the queue will not take it, to be given again later.
    fn give(&mut self, buffer: usize) {
        let Some(offset) = self.region.offset_of(buffer) else {
            return;
        };
        match self.free.enqueue(Descriptor::new(as_u64(offset), 0)) {
            Ok(()) => {
                self.held.remove(buffer);
                self.wake = true;
            }
            Err(QueueError::CorruptIndex { .. }) => self.indices = self.indices.saturating_add(1),
            Err(_) => {}
        }
    }

    /// The next received frame that passes its checks, copied into
    /// [`Receiver::copy`]: its length. What fails is dropped and counted;
    /// every buffer the driver passes back is given back to it. At most
    /// `N` descriptors are looked at.
    fn next(&mut self) -> Option<usize> {
        for _ in 0..N {
            let descriptor = match self.active.dequeue() {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => return None,
                Err(_) => {
                    self.indices = self.indices.saturating_add(1);
                    return None;
                }
            };
            // A receive buffer the stack gave the driver: the stack's again.
            let buffer = match self.region.buffer(descriptor) {
                Ok(buffer) if self.held.insert(buffer) => buffer,
                _ => {
                    self.malformed = self.malformed.saturating_add(1);
                    continue;
                }
            };
            let len = usize::from(descriptor.len);
            let copied = if len > MAX_FRAME_LEN {
                self.oversize = self.oversize.saturating_add(1);
                false
            } else if len < ETHERNET_HEADER_LEN {
                self.malformed = self.malformed.saturating_add(1);
                false
            } else {
                // The copy: from here on smoltcp sees only private memory.
                let loaded = match (self.region.offset_of(buffer), self.copy.get_mut(..len)) {
                    (Some(offset), Some(out)) => self.buffers.load(offset, out),
                    _ => false,
                };
                if !loaded {
                    self.malformed = self.malformed.saturating_add(1);
                }
                loaded
            };
            self.give(buffer);
            if copied {
                self.received = self.received.saturating_add(1);
                return Some(len);
            }
        }
        None
    }
}

/// The transmit side: the queues frames go out on, the buffers, and the
/// private buffer smoltcp writes a frame into.
struct Transmitter<Q, T, const N: usize> {
    active: Producer<Q>,
    free: Consumer<Q>,
    buffers: T,
    region: DataRegion,
    /// Transmit buffers the stack holds; the rest are with the driver.
    held: BufferSet<N>,
    /// Where smoltcp writes a frame, before it is copied to a buffer.
    scratch: [u8; MAX_FRAME_LEN],
    /// Whether a frame was given to the driver since it was last notified.
    wake: bool,
    sent: u32,
    unsent: u32,
    malformed: u32,
    indices: u32,
}

impl<Q: QueueMemory, T: TxMemory, const N: usize> Transmitter<Q, T, N> {
    /// Transmit buffers the driver has sent: the stack's again. At most `N`
    /// descriptors are looked at.
    fn reclaim(&mut self) {
        for _ in 0..N {
            match self.free.dequeue() {
                Ok(Some(descriptor)) => match self.region.buffer(descriptor) {
                    Ok(buffer) if self.held.insert(buffer) => {}
                    _ => self.malformed = self.malformed.saturating_add(1),
                },
                Ok(None) => return,
                Err(_) => {
                    self.indices = self.indices.saturating_add(1);
                    return;
                }
            }
        }
    }

    /// Sends the first `len` bytes of the scratch buffer: copied into a
    /// transmit buffer the stack holds, then handed to the driver.
    fn send(&mut self, len: usize) {
        self.reclaim();
        let sent = match (self.held.first(), self.scratch.get(..len)) {
            (Some(buffer), Some(frame)) => match self.region.offset_of(buffer) {
                Some(offset) if self.buffers.store(offset, frame) => {
                    let descriptor = Descriptor::new(as_u64(offset), as_u16(len));
                    match self.active.enqueue(descriptor) {
                        Ok(()) => {
                            self.held.remove(buffer);
                            true
                        }
                        Err(QueueError::CorruptIndex { .. }) => {
                            self.indices = self.indices.saturating_add(1);
                            false
                        }
                        Err(_) => false,
                    }
                }
                _ => false,
            },
            _ => false,
        };
        if sent {
            self.sent = self.sent.saturating_add(1);
            self.wake = true;
        } else {
            self.unsent = self.unsent.saturating_add(1);
        }
    }
}

/// smoltcp's device: the stack PD's link to its NIC driver, over `N`
/// buffers in each direction.
pub struct Link<Q, R, T, const N: usize> {
    rx: Receiver<Q, R, N>,
    tx: Transmitter<Q, T, N>,
}

impl<Q: QueueMemory, R: RxMemory, T: TxMemory, const N: usize> Link<Q, R, T, N> {
    /// A link over `queues`, with the receive buffers `rx_buffers` and the
    /// transmit buffers `tx_buffers`: `N` buffers each, which the stack
    /// holds until [`Link::start`] gives the receive buffers to the driver.
    ///
    /// # Errors
    ///
    /// [`LinkError::Queue`] if a queue's capacity is not a power of two up
    /// to the maximum; [`LinkError::Buffers`] if there are no buffers, or
    /// more than a queue holds.
    pub fn new(queues: Queues<Q>, rx_buffers: R, tx_buffers: T) -> Result<Self, LinkError> {
        let region = DataRegion::new(N).ok_or(LinkError::Buffers)?;
        let rx_free = Producer::new(queues.rx_free)?;
        if usize::from(rx_free.capacity()) < N {
            return Err(LinkError::Buffers);
        }
        Ok(Self {
            rx: Receiver {
                free: rx_free,
                active: Consumer::new(queues.rx_active)?,
                buffers: rx_buffers,
                region,
                held: BufferSet::all(),
                copy: [0; MAX_FRAME_LEN],
                wake: false,
                received: 0,
                oversize: 0,
                malformed: 0,
                indices: 0,
            },
            tx: Transmitter {
                active: Producer::new(queues.tx_active)?,
                free: Consumer::new(queues.tx_free)?,
                buffers: tx_buffers,
                region,
                held: BufferSet::all(),
                scratch: [0; MAX_FRAME_LEN],
                wake: false,
                sent: 0,
                unsent: 0,
                malformed: 0,
                indices: 0,
            },
        })
    }

    /// Gives the driver every receive buffer the stack holds: at start-up,
    /// and again in each round for any the queue would not take before.
    pub fn start(&mut self) {
        self.rx.refill();
    }

    /// Takes back the transmit buffers the driver has sent, and gives the
    /// driver any receive buffer still held: one round's housekeeping.
    pub fn service(&mut self) {
        self.tx.reclaim();
        self.rx.refill();
    }

    /// Whether to notify the driver now: something was given to it since
    /// the last notification, on a queue whose flag says it wants one
    /// (sDDF's protocol; the flag is then marked).
    pub fn wake_driver(&mut self) -> bool {
        let rx = core::mem::take(&mut self.rx.wake) && self.rx.free.signal_wanted();
        let tx = core::mem::take(&mut self.tx.wake) && self.tx.active.signal_wanted();
        rx || tx
    }

    /// Asks the driver to notify the stack when it next passes something
    /// back. Call before waiting, then look at the queues once more.
    pub fn request_signals(&self) {
        self.rx.active.request_signal();
        self.tx.free.request_signal();
    }

    /// What has been passed on and dropped so far.
    #[must_use]
    pub fn counts(&self) -> Counts {
        Counts {
            received: self.rx.received,
            sent: self.tx.sent,
            oversize: self.rx.oversize,
            malformed: self.rx.malformed.saturating_add(self.tx.malformed),
            indices: self.rx.indices.saturating_add(self.tx.indices),
            unsent: self.tx.unsent,
        }
    }

    /// How many receive and transmit buffers the stack holds.
    #[must_use]
    pub fn held(&self) -> (usize, usize) {
        (self.rx.held.len(), self.tx.held.len())
    }
}

/// A received frame, as smoltcp sees it: the stack's private copy.
pub struct RxToken<'a> {
    frame: &'a [u8],
}

impl phy::RxToken for RxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.frame)
    }
}

/// A frame smoltcp is about to send.
pub struct TxToken<'a, Q, T, const N: usize> {
    tx: &'a mut Transmitter<Q, T, N>,
}

impl<Q: QueueMemory, T: TxMemory, const N: usize> phy::TxToken for TxToken<'_, Q, T, N> {
    /// smoltcp writes the frame into private memory; it is then copied
    /// into a transmit buffer and handed to the driver - or, with no buffer
    /// free or a length from neither an Ethernet header nor at most
    /// [`MAX_FRAME_LEN`], dropped and counted. smoltcp never asks for more
    /// than the link's MTU, [`MAX_FRAME_LEN`] (it drops an IPv4 packet that
    /// would need fragmenting, which this build cannot do); a longer
    /// request would get only that many bytes.
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let span = len.min(MAX_FRAME_LEN);
        let mut none = [0u8; 0];
        let buffer = match self.tx.scratch.get_mut(..span) {
            Some(buffer) => buffer,
            None => &mut none[..],
        };
        let result = f(buffer);
        if (ETHERNET_HEADER_LEN..=MAX_FRAME_LEN).contains(&len) {
            self.tx.send(len);
        } else {
            self.tx.unsent = self.tx.unsent.saturating_add(1);
        }
        result
    }
}

impl<Q: QueueMemory, R: RxMemory, T: TxMemory, const N: usize> Device for Link<Q, R, T, N> {
    type RxToken<'a>
        = RxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a, Q, T, N>
    where
        Self: 'a;

    fn receive(
        &mut self,
        _timestamp: NetInstant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let len = self.rx.next()?;
        let frame = self.rx.copy.get(..len)?;
        Some((RxToken { frame }, TxToken { tx: &mut self.tx }))
    }

    fn transmit(&mut self, _timestamp: NetInstant) -> Option<Self::TxToken<'_>> {
        self.tx.reclaim();
        if self.tx.held.is_empty() {
            None
        } else {
            Some(TxToken { tx: &mut self.tx })
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = MAX_FRAME_LEN;
        capabilities.max_burst_size = None;
        // Every checksum is computed and checked in software: the driver
        // negotiates no offload with the card.
        capabilities
    }
}

/// An offset in a data region, as a descriptor holds it.
fn as_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// A frame's length, as a descriptor holds it (at most [`MAX_FRAME_LEN`]).
fn as_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_net::{BUFFER_SIZE, InMemoryQueue, QueueMemory};
    use phy::{RxToken as _, TxToken as _};

    use super::*;

    const N: usize = 4;
    type Queue = InMemoryQueue<N>;

    /// Both sides' queues and buffers, as a test driver sees them.
    struct Shared {
        rx_free: Queue,
        rx_active: Queue,
        tx_free: Queue,
        tx_active: Queue,
        rx: Vec<AtomicU8>,
        tx: Vec<AtomicU8>,
    }

    impl Shared {
        fn new() -> Self {
            Self {
                rx_free: Queue::new(),
                rx_active: Queue::new(),
                tx_free: Queue::new(),
                tx_active: Queue::new(),
                rx: (0..N * BUFFER_SIZE).map(|_| AtomicU8::new(0)).collect(),
                tx: (0..N * BUFFER_SIZE).map(|_| AtomicU8::new(0)).collect(),
            }
        }

        fn link(&self) -> Link<&Queue, &[AtomicU8], &[AtomicU8], N> {
            Link::new(
                Queues {
                    rx_free: &self.rx_free,
                    rx_active: &self.rx_active,
                    tx_free: &self.tx_free,
                    tx_active: &self.tx_active,
                },
                &self.rx[..],
                &self.tx[..],
            )
            .unwrap()
        }
    }

    /// The driver's ends of the queues.
    struct Driver<'a> {
        rx_free: Consumer<&'a Queue>,
        rx_active: Producer<&'a Queue>,
        tx_free: Producer<&'a Queue>,
        tx_active: Consumer<&'a Queue>,
        shared: &'a Shared,
    }

    impl<'a> Driver<'a> {
        fn new(shared: &'a Shared) -> Self {
            Self {
                rx_free: Consumer::new(&shared.rx_free).unwrap(),
                rx_active: Producer::new(&shared.rx_active).unwrap(),
                tx_free: Producer::new(&shared.tx_free).unwrap(),
                tx_active: Consumer::new(&shared.tx_active).unwrap(),
                shared,
            }
        }

        /// Takes an empty buffer and passes `frame` back in it.
        fn deliver(&mut self, frame: &[u8]) -> u64 {
            let offset = self.rx_free.dequeue().unwrap().unwrap().io_or_offset;
            let mut memory = &self.shared.rx[..];
            assert!(TxMemory::store(&mut memory, offset as usize, frame));
            self.rx_active
                .enqueue(Descriptor::new(offset, frame.len() as u16))
                .unwrap();
            offset
        }

        /// The next frame the stack sent, and its buffer given back.
        fn collect(&mut self) -> Option<Vec<u8>> {
            let descriptor = self.tx_active.dequeue().unwrap()?;
            let mut frame = vec![0u8; usize::from(descriptor.len)];
            assert!(self.shared.tx[..].load(descriptor.io_or_offset as usize, &mut frame));
            self.tx_free
                .enqueue(Descriptor::new(descriptor.io_or_offset, 0))
                .unwrap();
            Some(frame)
        }
    }

    fn frame(len: usize, fill: u8) -> Vec<u8> {
        vec![fill; len]
    }

    #[test]
    fn start_gives_the_driver_every_receive_buffer() {
        let shared = Shared::new();
        let mut link = shared.link();
        assert_eq!(link.held(), (N, N));
        link.start();
        assert_eq!(link.held(), (0, N));
        assert!(link.wake_driver(), "a zeroed flag asks for a notification");
        assert!(!link.wake_driver(), "nothing new since");
        let mut driver = Driver::new(&shared);
        let offsets: Vec<u64> = (0..N)
            .map(|_| driver.rx_free.dequeue().unwrap().unwrap().io_or_offset)
            .collect();
        assert_eq!(offsets, [0, 2048, 4096, 6144]);
    }

    #[test]
    fn a_frame_is_copied_before_smoltcp_sees_it_and_its_buffer_given_back() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.start();
        let mut driver = Driver::new(&shared);
        let offset = driver.deliver(&frame(60, 0xab));
        let (rx, _) = link.receive(NetInstant::ZERO).unwrap();
        // The driver rewrites the shared buffer after the copy: smoltcp
        // still sees what was copied.
        let mut memory = &shared.rx[..];
        assert!(TxMemory::store(&mut memory, offset as usize, &[0x11; 60]));
        rx.consume(|bytes| assert_eq!(bytes, &frame(60, 0xab)[..]));
        // The buffer went straight back to the driver.
        assert_eq!(link.held(), (0, N));
        assert_eq!(link.counts().received, 1);
    }

    #[test]
    fn oversize_and_malformed_frames_are_dropped_and_counted() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.start();
        let mut driver = Driver::new(&shared);
        let buffers: Vec<u64> = (0..N)
            .map(|_| driver.rx_free.dequeue().unwrap().unwrap().io_or_offset)
            .collect();
        let bad = [
            // Oversize: longer than a frame may be, though it fits a buffer.
            Descriptor::new(buffers[0], (MAX_FRAME_LEN + 1) as u16),
            // Longer than a buffer: oversize too.
            Descriptor::new(buffers[1], u16::MAX),
            // Shorter than an Ethernet header: malformed.
            Descriptor::new(buffers[2], (ETHERNET_HEADER_LEN - 1) as u16),
            // Outside the region: malformed.
            Descriptor::new((N * BUFFER_SIZE) as u64, 60),
        ];
        for descriptor in bad {
            driver.rx_active.enqueue(descriptor).unwrap();
        }
        assert!(link.receive(NetInstant::ZERO).is_none());
        // Off a buffer boundary: malformed.
        driver
            .rx_active
            .enqueue(Descriptor::new(buffers[3] + 1, 60))
            .unwrap();
        assert!(link.receive(NetInstant::ZERO).is_none());
        let counts = link.counts();
        assert_eq!((counts.oversize, counts.malformed), (2, 3));
        assert_eq!(counts.received, 0);
        // The three buffers named went straight back to the driver; the
        // fourth is still the driver's.
        assert_eq!(link.held(), (0, N));
        let given_back: Vec<u64> = (0..3)
            .map(|_| driver.rx_free.dequeue().unwrap().unwrap().io_or_offset)
            .collect();
        assert_eq!(given_back, buffers[..3]);
        assert_eq!(Counts::default().dropped(), Counts::default());
        assert_eq!(counts.dropped().received, 0);
    }

    #[test]
    fn a_buffer_the_driver_does_not_hold_is_malformed() {
        let shared = Shared::new();
        // The stack holds every buffer: it has given the driver none.
        let mut link = shared.link();
        let mut driver = Driver::new(&shared);
        driver
            .rx_active
            .enqueue(Descriptor::new(BUFFER_SIZE as u64, 60))
            .unwrap();
        assert!(link.receive(NetInstant::ZERO).is_none());
        assert_eq!(link.counts().malformed, 1);
        assert_eq!(link.held(), (N, N), "nothing given away");
    }

    #[test]
    fn a_corrupt_driver_index_is_counted_and_nothing_read() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.start();
        shared.rx_active.store_tail(N as u16 + 1);
        assert!(link.receive(NetInstant::ZERO).is_none());
        assert_eq!(link.counts().indices, 1);
        assert_eq!(link.counts().received, 0);
    }

    #[test]
    fn at_most_a_queue_of_bad_descriptors_per_receive() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.start();
        let mut driver = Driver::new(&shared);
        for _ in 0..N {
            driver
                .rx_active
                .enqueue(Descriptor::new(u64::MAX, 60))
                .unwrap();
        }
        assert!(link.receive(NetInstant::ZERO).is_none());
        assert_eq!(link.counts().malformed, N as u32);
    }

    #[test]
    fn frames_go_out_in_the_stacks_buffers_and_come_back() {
        let shared = Shared::new();
        let mut link = shared.link();
        let mut driver = Driver::new(&shared);
        for round in 0..3 * N {
            let tx = link.transmit(NetInstant::ZERO).unwrap();
            tx.consume(42, |buffer| buffer.fill(round as u8));
            assert_eq!(driver.collect(), Some(frame(42, round as u8)));
        }
        assert_eq!(link.counts().sent, 3 * N as u32);
        // A round's housekeeping: every transmit buffer back, and every
        // receive buffer given to the driver.
        link.service();
        assert_eq!(link.held(), (0, N));
    }

    #[test]
    fn with_every_transmit_buffer_out_there_is_no_token() {
        let shared = Shared::new();
        let mut link = shared.link();
        for _ in 0..N {
            link.transmit(NetInstant::ZERO)
                .unwrap()
                .consume(60, |buffer| buffer.fill(1));
        }
        assert!(link.transmit(NetInstant::ZERO).is_none());
        assert_eq!(link.held(), (N, 0));
    }

    #[test]
    fn a_reply_with_no_buffer_free_is_dropped_and_counted() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.start();
        for _ in 0..N {
            link.transmit(NetInstant::ZERO)
                .unwrap()
                .consume(60, |buffer| buffer.fill(1));
        }
        let mut driver = Driver::new(&shared);
        driver.deliver(&frame(60, 2));
        let (_, tx) = link.receive(NetInstant::ZERO).unwrap();
        tx.consume(60, |buffer| buffer.fill(3));
        assert_eq!(link.counts().unsent, 1);
        // Lengths out of range are never sent.
        let shared = Shared::new();
        let mut link = shared.link();
        link.transmit(NetInstant::ZERO)
            .unwrap()
            .consume(ETHERNET_HEADER_LEN - 1, |_| ());
        link.transmit(NetInstant::ZERO)
            .unwrap()
            .consume(MAX_FRAME_LEN + 1, |buffer| {
                assert_eq!(buffer.len(), MAX_FRAME_LEN)
            });
        assert_eq!(link.counts().unsent, 2);
        assert_eq!(link.held(), (N, N));
    }

    #[test]
    fn a_sent_buffer_the_stack_already_holds_is_malformed() {
        let shared = Shared::new();
        let mut link = shared.link();
        let mut driver = Driver::new(&shared);
        driver.tx_free.enqueue(Descriptor::new(0, 0)).unwrap();
        driver.tx_free.enqueue(Descriptor::new(1, 0)).unwrap();
        link.service();
        assert_eq!(link.counts().malformed, 2);
        assert_eq!(link.held().1, N);
    }

    #[test]
    fn signals_follow_sddfs_protocol() {
        let shared = Shared::new();
        let mut link = shared.link();
        link.request_signals();
        assert_eq!(shared.rx_active.load_signal(), 0);
        assert_eq!(shared.tx_free.load_signal(), 0);
        assert!(!link.wake_driver(), "nothing given");
        link.transmit(NetInstant::ZERO)
            .unwrap()
            .consume(60, |buffer| buffer.fill(1));
        assert!(link.wake_driver());
        // The driver did not ask again: no notification.
        link.transmit(NetInstant::ZERO)
            .unwrap()
            .consume(60, |buffer| buffer.fill(1));
        assert!(!link.wake_driver());
    }

    #[test]
    fn a_queue_too_small_for_the_buffers_is_refused() {
        let small = InMemoryQueue::<2>::new();
        let rx: Vec<AtomicU8> = Vec::new();
        let tx: Vec<AtomicU8> = Vec::new();
        let made = Link::<_, _, _, N>::new(
            Queues {
                rx_free: &small,
                rx_active: &small,
                tx_free: &small,
                tx_active: &small,
            },
            &rx[..],
            &tx[..],
        );
        assert_eq!(made.err(), Some(LinkError::Buffers));
        let three = InMemoryQueue::<3>::new();
        let made = Link::<_, _, _, 2>::new(
            Queues {
                rx_free: &three,
                rx_active: &three,
                tx_free: &three,
                tx_active: &three,
            },
            &rx[..],
            &tx[..],
        );
        assert!(matches!(made.err(), Some(LinkError::Queue(_))));
    }

    #[test]
    fn a_buffer_the_memory_cannot_hold_is_malformed() {
        // Receive buffers smaller than the region the queues name.
        let shared = Shared::new();
        let short: Vec<AtomicU8> = (0..BUFFER_SIZE).map(|_| AtomicU8::new(0)).collect();
        let mut link: Link<&Queue, &[AtomicU8], &[AtomicU8], N> = Link::new(
            Queues {
                rx_free: &shared.rx_free,
                rx_active: &shared.rx_active,
                tx_free: &shared.tx_free,
                tx_active: &shared.tx_active,
            },
            &short[..],
            &shared.tx[..],
        )
        .unwrap();
        link.start();
        let mut driver = Driver::new(&shared);
        let _ = driver.rx_free.dequeue().unwrap();
        let second = driver.rx_free.dequeue().unwrap().unwrap();
        driver
            .rx_active
            .enqueue(Descriptor::new(second.io_or_offset, 60))
            .unwrap();
        assert!(link.receive(NetInstant::ZERO).is_none());
        assert_eq!(link.counts().malformed, 1);
    }
}
