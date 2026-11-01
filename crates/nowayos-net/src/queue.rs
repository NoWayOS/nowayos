// SPDX-License-Identifier: MIT OR Apache-2.0
//! sDDF's network queues, byte for byte (point 2), with
//! NoWayOS's own checks.
//!
//! Written from sDDF 0.7.0's `include/sddf/network/queue.h` and
//! `include/sddf/network/constants.h` (BSD-2-Clause): the layout and the
//! protocol they define.
//!
//! # Layout
//!
//! One queue is one shared memory region. Integers are little-endian, as
//! sDDF's C lays them out on AArch64:
//!
//! | Offset | Size | sDDF's `net_queue_t` | Meaning |
//! | --- | --- | --- | --- |
//! | 0 | 2 | `tail` | the producer's index: descriptors ever enqueued, free-running |
//! | 2 | 2 | `head` | the consumer's index: descriptors ever dequeued, free-running |
//! | 4 | 4 | `consumer_signalled` | 0 while the consumer wants a notification (see "Signalling") |
//! | 8 | 16 × capacity | `buffers` | the descriptors; index `i` is slot `i % capacity` |
//!
//! and one descriptor:
//!
//! | Offset | Size | sDDF's `net_buff_desc_t` | Meaning |
//! | --- | --- | --- | --- |
//! | 0 | 8 | `io_or_offset` | between a driver and its one client: the buffer's offset in its data region |
//! | 8 | 2 | `len` | the frame's length in the buffer; 0 in a free buffer |
//! | 10 | 1 | `oid` (its low 6 bits) | sDDF's owner id: written 0 and ignored when read, as sDDF's drivers and clients do |
//! | 11 | 5 | - | padding: written 0, ignored |
//!
//! A data region holds [`BUFFER_SIZE`]-byte buffers, buffer `i` at offset
//! `i × BUFFER_SIZE` ([`DataRegion`]). Each direction has two queues:
//!
//! | Queue | Producer | Consumer | Carries |
//! | --- | --- | --- | --- |
//! | receive free | client | driver | empty receive buffers, for the device to fill |
//! | receive active | driver | client | received frames |
//! | transmit active | client | driver | frames to send |
//! | transmit free | driver | client | transmit buffers the device has sent |
//!
//! The capacity is a power of two, at most [`MAX_CAPACITY`], so that
//! `index % capacity` names the same slot across the 16-bit indices' wrap;
//! the descriptors in flight are `tail - head`, modulo 2^16.
//!
//! # Trust
//!
//! sDDF's components trust their neighbours' writes to the queues. NoWayOS's
//! do not. Both sides map a queue read-write - each writes its own index,
//! and both write the flag - so a peer can write anything there, at any
//! moment:
//!
//! * each side keeps its own index privately and only ever *writes* it to
//!   the shared memory. It reads only the peer's index, and checks it every
//!   time: a peer index that would put more than the capacity in flight is
//!   refused ([`QueueError::CorruptIndex`]), and nothing is read or written;
//! * a descriptor is copied out of shared memory before anything looks at
//!   it, and [`DataRegion`] checks the copy before it is used: inside the
//!   region, on a buffer boundary, with a length that fits;
//! * [`BufferSet`] keeps a side from handing on a buffer it does not hold.
//!
//! Both sides start at index 0, as the zeroed region does (Microkit zeroes
//! memory regions at boot), so neither ever adopts an index from shared
//! memory. sDDF's clients fill their own transmit free queue at start-up; a
//! NoWayOS client keeps its transmit buffers privately instead, so that each
//! index has one writer.
//!
//! # Signalling
//!
//! sDDF's protocol: before it waits, the consumer writes 0 to the flag
//! ([`Consumer::request_signal`]) and then checks the queue once more; after
//! enqueueing, the producer notifies the consumer only if the flag is 0, and
//! sets it to 1 ([`Producer::signal_wanted`]). The flag is a hint: a peer
//! that lies about it only delays or repeats its own notifications.

use core::fmt;
use core::ops::RangeInclusive;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering, fence};

/// The size of every buffer: sDDF's `NET_BUFFER_SIZE`.
pub const BUFFER_SIZE: usize = 2048;

/// The bytes before a queue's descriptors: `tail`, `head` and
/// `consumer_signalled`.
pub const QUEUE_HEADER_LEN: usize = 8;

/// The bytes of one descriptor, `net_buff_desc_t`, padding included.
pub const DESCRIPTOR_LEN: usize = 16;

/// Where `tail` is in a queue.
pub const TAIL_OFFSET: usize = 0;

/// Where `head` is in a queue.
pub const HEAD_OFFSET: usize = 2;

/// Where `consumer_signalled` is in a queue.
pub const SIGNAL_OFFSET: usize = 4;

/// The largest capacity: 2^15 descriptors. A larger queue could not tell
/// full from corrupt with 16-bit indices.
pub const MAX_CAPACITY: u16 = 1 << 15;

/// The bytes a queue of `capacity` descriptors occupies, or `None` if that
/// overflows.
#[must_use]
pub const fn queue_len(capacity: usize) -> Option<usize> {
    match capacity.checked_mul(DESCRIPTOR_LEN) {
        Some(descriptors) => descriptors.checked_add(QUEUE_HEADER_LEN),
        None => None,
    }
}

// ---- Descriptors -------------------------------------------------------------

/// One descriptor, copied out of (or about to be copied into) a queue.
/// Nothing in it is checked: [`DataRegion`] checks it before it is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Descriptor {
    /// Between a driver and its one client, the buffer's offset in its data
    /// region (sDDF's `io_or_offset`).
    pub io_or_offset: u64,
    /// The bytes of frame in the buffer; 0 in a free buffer.
    pub len: u16,
}

impl Descriptor {
    /// A descriptor for the buffer at `io_or_offset` holding `len` bytes.
    #[must_use]
    pub const fn new(io_or_offset: u64, len: u16) -> Self {
        Self { io_or_offset, len }
    }

    /// The descriptor's 16 bytes: the owner id and the padding are 0.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DESCRIPTOR_LEN] {
        let [o0, o1, o2, o3, o4, o5, o6, o7] = self.io_or_offset.to_le_bytes();
        let [l0, l1] = self.len.to_le_bytes();
        [o0, o1, o2, o3, o4, o5, o6, o7, l0, l1, 0, 0, 0, 0, 0, 0]
    }

    /// The descriptor in 16 bytes copied out of a queue. The owner id and
    /// the padding are ignored, as sDDF's drivers ignore them.
    #[must_use]
    pub const fn from_bytes(bytes: &[u8; DESCRIPTOR_LEN]) -> Self {
        let [o0, o1, o2, o3, o4, o5, o6, o7, l0, l1, _oid, _, _, _, _, _] = *bytes;
        Self {
            io_or_offset: u64::from_le_bytes([o0, o1, o2, o3, o4, o5, o6, o7]),
            len: u16::from_le_bytes([l0, l1]),
        }
    }
}

// ---- Shared memory -----------------------------------------------------------

/// One queue's shared memory, as a side sees it.
///
/// [`InMemoryQueue`] implements it for host tests; the view of a real
/// Microkit region needs `unsafe` and is `nowayos-region`'s (`net_queue`).
///
/// # Contract
///
/// * `load_tail` and `load_head` have at least **acquire** ordering;
///   `store_tail` and `store_head` at least **release**, so that a
///   descriptor written before an index is stored is seen by the side that
///   loads the index;
/// * `load_signal` and `store_signal` are sequentially consistent;
/// * `read_descriptor` and `write_descriptor` copy a whole descriptor and
///   tolerate the peer changing the memory concurrently without undefined
///   behaviour (byte-wise relaxed atomics, say). They are only called with
///   `slot < capacity()`, and must ignore any other slot rather than panic;
/// * `capacity` returns the same value every time.
pub trait QueueMemory {
    /// The descriptors the queue holds: a power of two, at most
    /// [`MAX_CAPACITY`].
    fn capacity(&self) -> u16;
    /// Loads `tail` (acquire).
    fn load_tail(&self) -> u16;
    /// Loads `head` (acquire).
    fn load_head(&self) -> u16;
    /// Stores `tail` (release).
    fn store_tail(&self, tail: u16);
    /// Stores `head` (release).
    fn store_head(&self, head: u16);
    /// Loads `consumer_signalled`.
    fn load_signal(&self) -> u32;
    /// Stores `consumer_signalled`.
    fn store_signal(&self, value: u32);
    /// Copies the descriptor in `slot` into `out`.
    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]);
    /// Copies `bytes` into the descriptor in `slot`.
    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]);
}

impl<T: QueueMemory + ?Sized> QueueMemory for &T {
    fn capacity(&self) -> u16 {
        (**self).capacity()
    }

    fn load_tail(&self) -> u16 {
        (**self).load_tail()
    }

    fn load_head(&self) -> u16 {
        (**self).load_head()
    }

    fn store_tail(&self, tail: u16) {
        (**self).store_tail(tail);
    }

    fn store_head(&self, head: u16) {
        (**self).store_head(head);
    }

    fn load_signal(&self) -> u32 {
        (**self).load_signal()
    }

    fn store_signal(&self, value: u32) {
        (**self).store_signal(value);
    }

    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]) {
        (**self).read_descriptor(slot, out);
    }

    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]) {
        (**self).write_descriptor(slot, bytes);
    }
}

/// Why a queue operation did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueueError {
    /// The capacity is not a power of two from 1 to [`MAX_CAPACITY`].
    BadCapacity {
        /// The capacity the memory reported.
        capacity: u16,
    },
    /// The peer's index puts more than the capacity in flight: the peer is
    /// faulty or hostile. Nothing was read or written.
    CorruptIndex {
        /// The producer's index, as this side sees it.
        tail: u16,
        /// The consumer's index, as this side sees it.
        head: u16,
    },
    /// The queue is full. Nothing was written.
    Full,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadCapacity { capacity } => write!(
                f,
                "a queue of {capacity} descriptors: the capacity must be a power of two up to {MAX_CAPACITY}"
            ),
            Self::CorruptIndex { tail, head } => write!(
                f,
                "corrupt queue indices: tail {tail}, head {head} put more than the capacity in flight"
            ),
            Self::Full => f.write_str("the queue is full"),
        }
    }
}

impl core::error::Error for QueueError {}

/// `capacity` if it is a power of two up to [`MAX_CAPACITY`].
fn checked_capacity(capacity: u16) -> Result<u16, QueueError> {
    if capacity.is_power_of_two() && capacity <= MAX_CAPACITY {
        Ok(capacity)
    } else {
        Err(QueueError::BadCapacity { capacity })
    }
}

/// The slot index `index` names in a queue of `capacity`, a power of two.
const fn slot(index: u16, capacity: u16) -> u16 {
    index & capacity.wrapping_sub(1)
}

// ---- The two ends -------------------------------------------------------------

/// The producer's end of a queue: it alone writes `tail`, and it keeps that
/// index privately.
#[derive(Debug)]
pub struct Producer<M> {
    memory: M,
    capacity: u16,
    tail: u16,
}

impl<M: QueueMemory> Producer<M> {
    /// The producer's end over `memory`, starting at index 0.
    ///
    /// # Errors
    ///
    /// [`QueueError::BadCapacity`] unless the memory's capacity is a power
    /// of two up to [`MAX_CAPACITY`].
    pub fn new(memory: M) -> Result<Self, QueueError> {
        let capacity = checked_capacity(memory.capacity())?;
        Ok(Self {
            memory,
            capacity,
            tail: 0,
        })
    }

    /// The queue's capacity.
    #[must_use]
    pub const fn capacity(&self) -> u16 {
        self.capacity
    }

    /// The descriptors enqueued and not yet dequeued.
    ///
    /// # Errors
    ///
    /// [`QueueError::CorruptIndex`] if the consumer's index puts more than
    /// the capacity in flight.
    pub fn in_flight(&self) -> Result<u16, QueueError> {
        let head = self.memory.load_head();
        let in_flight = self.tail.wrapping_sub(head);
        if in_flight > self.capacity {
            return Err(QueueError::CorruptIndex {
                tail: self.tail,
                head,
            });
        }
        Ok(in_flight)
    }

    /// Enqueues `descriptor`: copies it into the next slot, then publishes
    /// the new `tail` (release).
    ///
    /// # Errors
    ///
    /// [`QueueError::Full`], or [`QueueError::CorruptIndex`] as for
    /// [`Producer::in_flight`]. Either way nothing is written.
    pub fn enqueue(&mut self, descriptor: Descriptor) -> Result<(), QueueError> {
        if self.in_flight()? == self.capacity {
            return Err(QueueError::Full);
        }
        self.memory
            .write_descriptor(slot(self.tail, self.capacity), &descriptor.to_bytes());
        self.tail = self.tail.wrapping_add(1);
        self.memory.store_tail(self.tail);
        Ok(())
    }

    /// Whether the consumer asked for a notification (the flag is 0); if it
    /// did, marks it notified (1). Call after enqueueing, and notify the
    /// consumer when it returns `true`.
    pub fn signal_wanted(&self) -> bool {
        // The tail stored before must be visible before the flag is read,
        // and the consumer's flag store before its re-check (see
        // `Consumer::request_signal`): a store-to-load order, which only a
        // full fence gives.
        fence(Ordering::SeqCst);
        if self.memory.load_signal() == 0 {
            self.memory.store_signal(1);
            true
        } else {
            false
        }
    }
}

/// The consumer's end of a queue: it alone writes `head`, and it keeps that
/// index privately.
#[derive(Debug)]
pub struct Consumer<M> {
    memory: M,
    capacity: u16,
    head: u16,
}

impl<M: QueueMemory> Consumer<M> {
    /// The consumer's end over `memory`, starting at index 0.
    ///
    /// # Errors
    ///
    /// As for [`Producer::new`].
    pub fn new(memory: M) -> Result<Self, QueueError> {
        let capacity = checked_capacity(memory.capacity())?;
        Ok(Self {
            memory,
            capacity,
            head: 0,
        })
    }

    /// The queue's capacity.
    #[must_use]
    pub const fn capacity(&self) -> u16 {
        self.capacity
    }

    /// Dequeues the next descriptor: loads the producer's `tail`
    /// (acquire), copies the descriptor out of its slot, then publishes the
    /// new `head` (release), so the producer cannot reuse the slot while it
    /// is being copied. `None` when the queue is empty. The descriptor is
    /// not checked: check it with [`DataRegion`] before using it.
    ///
    /// # Errors
    ///
    /// [`QueueError::CorruptIndex`] if the producer's index puts more than
    /// the capacity in flight; nothing is read.
    pub fn dequeue(&mut self) -> Result<Option<Descriptor>, QueueError> {
        let tail = self.memory.load_tail();
        let pending = tail.wrapping_sub(self.head);
        if pending > self.capacity {
            return Err(QueueError::CorruptIndex {
                tail,
                head: self.head,
            });
        }
        if pending == 0 {
            return Ok(None);
        }
        let mut bytes = [0u8; DESCRIPTOR_LEN];
        self.memory
            .read_descriptor(slot(self.head, self.capacity), &mut bytes);
        self.head = self.head.wrapping_add(1);
        self.memory.store_head(self.head);
        Ok(Some(Descriptor::from_bytes(&bytes)))
    }

    /// Asks the producer for a notification on its next enqueue (the flag
    /// to 0). Call before waiting, then dequeue once more: a descriptor
    /// enqueued just before the flag was written brings no notification.
    pub fn request_signal(&self) {
        self.memory.store_signal(0);
        // The flag must be visible before the re-check loads `tail` (see
        // `Producer::signal_wanted`).
        fence(Ordering::SeqCst);
    }
}

// Kani only (`proofs`): each end with its own index anywhere, as
// after that many operations, so that a proof covers every index without
// performing 65,536 of them. The index is still the side's own: nothing here
// reads it from shared memory.
#[cfg(kani)]
impl<M: QueueMemory> Producer<M> {
    /// The producer's end over `memory` with its own index at `tail`.
    pub(crate) fn at(memory: M, tail: u16) -> Result<Self, QueueError> {
        let mut producer = Self::new(memory)?;
        producer.tail = tail;
        Ok(producer)
    }

    /// The producer's own index.
    pub(crate) const fn index(&self) -> u16 {
        self.tail
    }
}

#[cfg(kani)]
impl<M: QueueMemory> Consumer<M> {
    /// The consumer's end over `memory` with its own index at `head`.
    pub(crate) fn at(memory: M, head: u16) -> Result<Self, QueueError> {
        let mut consumer = Self::new(memory)?;
        consumer.head = head;
        Ok(consumer)
    }

    /// The consumer's own index.
    pub(crate) const fn index(&self) -> u16 {
        self.head
    }
}

// ---- A queue in ordinary memory -------------------------------------------------

/// A queue in ordinary memory, for host tests and examples: both ends can
/// share it by reference.
///
/// ```
/// use nowayos_net::{Consumer, Descriptor, InMemoryQueue, Producer};
///
/// let queue: InMemoryQueue<4> = InMemoryQueue::new();
/// let mut producer = Producer::new(&queue).unwrap();
/// let mut consumer = Consumer::new(&queue).unwrap();
/// producer.enqueue(Descriptor::new(2048, 60)).unwrap();
/// assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(2048, 60))));
/// assert_eq!(consumer.dequeue(), Ok(None));
/// ```
#[derive(Debug)]
pub struct InMemoryQueue<const CAPACITY: usize> {
    tail: AtomicU16,
    head: AtomicU16,
    signal: AtomicU32,
    descriptors: [[AtomicU8; DESCRIPTOR_LEN]; CAPACITY],
}

impl<const CAPACITY: usize> InMemoryQueue<CAPACITY> {
    /// An empty queue, everything zero. Usable in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tail: AtomicU16::new(0),
            head: AtomicU16::new(0),
            signal: AtomicU32::new(0),
            descriptors: [const { [const { AtomicU8::new(0) }; DESCRIPTOR_LEN] }; CAPACITY],
        }
    }
}

impl<const CAPACITY: usize> Default for InMemoryQueue<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> QueueMemory for InMemoryQueue<CAPACITY> {
    fn capacity(&self) -> u16 {
        // A capacity that does not fit is reported as 0: refused.
        u16::try_from(CAPACITY).unwrap_or(0)
    }

    fn load_tail(&self) -> u16 {
        self.tail.load(Ordering::Acquire)
    }

    fn load_head(&self) -> u16 {
        self.head.load(Ordering::Acquire)
    }

    fn store_tail(&self, tail: u16) {
        self.tail.store(tail, Ordering::Release);
    }

    fn store_head(&self, head: u16) {
        self.head.store(head, Ordering::Release);
    }

    fn load_signal(&self) -> u32 {
        self.signal.load(Ordering::SeqCst)
    }

    fn store_signal(&self, value: u32) {
        self.signal.store(value, Ordering::SeqCst);
    }

    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]) {
        if let Some(cells) = self.descriptors.get(usize::from(slot)) {
            for (byte, cell) in out.iter_mut().zip(cells) {
                *byte = cell.load(Ordering::Relaxed);
            }
        }
    }

    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]) {
        if let Some(cells) = self.descriptors.get(usize::from(slot)) {
            for (cell, byte) in cells.iter().zip(bytes) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }
    }
}

// ---- Data regions and their buffers ------------------------------------------------

/// A data region: `buffers` buffers of [`BUFFER_SIZE`] bytes, buffer `i` at
/// offset `i × BUFFER_SIZE`. It checks the descriptors that name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataRegion {
    buffers: usize,
    len: usize,
}

/// A frame a descriptor names, checked by [`DataRegion::frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The buffer's number in its region.
    pub buffer: usize,
    /// The buffer's offset in its region.
    pub offset: usize,
    /// The frame's length.
    pub len: usize,
}

/// Why a descriptor was refused. It was not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DescriptorError {
    /// Its offset lies outside the data region.
    OutsideRegion {
        /// The offset it gave.
        offset: u64,
    },
    /// Its offset is inside the region but not at the start of a buffer.
    NotOnBoundary {
        /// The offset it gave.
        offset: u64,
    },
    /// Its length is outside the lengths allowed.
    BadLength {
        /// The length it gave.
        len: u16,
    },
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideRegion { offset } => {
                write!(f, "offset {offset:#x} is outside the data region")
            }
            Self::NotOnBoundary { offset } => write!(
                f,
                "offset {offset:#x} is not on a {BUFFER_SIZE}-byte buffer boundary"
            ),
            Self::BadLength { len } => write!(f, "a frame of {len} bytes"),
        }
    }
}

impl core::error::Error for DescriptorError {}

impl DataRegion {
    /// A region of `buffers` buffers, or `None` if there are none or their
    /// size overflows.
    #[must_use]
    pub const fn new(buffers: usize) -> Option<Self> {
        if buffers == 0 {
            return None;
        }
        match buffers.checked_mul(BUFFER_SIZE) {
            Some(len) => Some(Self { buffers, len }),
            None => None,
        }
    }

    /// The number of buffers.
    #[must_use]
    pub const fn buffers(&self) -> usize {
        self.buffers
    }

    /// The region's size in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the region has no buffers (never: [`DataRegion::new`]
    /// refuses that).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buffers == 0
    }

    /// The offset of buffer `index`, if there is one.
    #[must_use]
    pub fn offset_of(&self, index: usize) -> Option<usize> {
        if index < self.buffers {
            index.checked_mul(BUFFER_SIZE)
        } else {
            None
        }
    }

    /// The buffer a descriptor names: its number, if its offset is inside
    /// the region and at the start of a buffer. Its length is not looked at.
    ///
    /// # Errors
    ///
    /// [`DescriptorError::OutsideRegion`] or
    /// [`DescriptorError::NotOnBoundary`].
    pub fn buffer(&self, descriptor: Descriptor) -> Result<usize, DescriptorError> {
        let offset = descriptor.io_or_offset;
        let inside = usize::try_from(offset)
            .ok()
            .filter(|inside| *inside < self.len)
            .ok_or(DescriptorError::OutsideRegion { offset })?;
        if !inside.is_multiple_of(BUFFER_SIZE) {
            return Err(DescriptorError::NotOnBoundary { offset });
        }
        inside
            .checked_div(BUFFER_SIZE)
            .ok_or(DescriptorError::OutsideRegion { offset })
    }

    /// The frame a descriptor names: its buffer as for
    /// [`DataRegion::buffer`], and a length within `lengths` that fits in
    /// a buffer.
    ///
    /// # Errors
    ///
    /// As for [`DataRegion::buffer`], or [`DescriptorError::BadLength`].
    pub fn frame(
        &self,
        descriptor: Descriptor,
        lengths: RangeInclusive<usize>,
    ) -> Result<Frame, DescriptorError> {
        let buffer = self.buffer(descriptor)?;
        let len = usize::from(descriptor.len);
        if !lengths.contains(&len) || len > BUFFER_SIZE {
            return Err(DescriptorError::BadLength {
                len: descriptor.len,
            });
        }
        let offset = self
            .offset_of(buffer)
            .ok_or(DescriptorError::OutsideRegion {
                offset: descriptor.io_or_offset,
            })?;
        Ok(Frame {
            buffer,
            offset,
            len,
        })
    }
}

/// Which of a region's `N` buffers a side holds, so that it never hands on
/// a buffer it does not hold, nor takes back one it already has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferSet<const N: usize> {
    held: [bool; N],
}

impl<const N: usize> BufferSet<N> {
    /// No buffer held.
    #[must_use]
    pub const fn none() -> Self {
        Self { held: [false; N] }
    }

    /// Every buffer held.
    #[must_use]
    pub const fn all() -> Self {
        Self { held: [true; N] }
    }

    /// Whether buffer `index` is held.
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.held.get(index).copied().unwrap_or(false)
    }

    /// Takes buffer `index` in. `false`, changing nothing, if it is already
    /// held or there is no such buffer.
    pub fn insert(&mut self, index: usize) -> bool {
        match self.held.get_mut(index) {
            Some(held) if !*held => {
                *held = true;
                true
            }
            _ => false,
        }
    }

    /// Hands buffer `index` on. `false`, changing nothing, if it is not
    /// held.
    pub fn remove(&mut self, index: usize) -> bool {
        match self.held.get_mut(index) {
            Some(held) if *held => {
                *held = false;
                true
            }
            _ => false,
        }
    }

    /// The lowest-numbered buffer held.
    #[must_use]
    pub fn first(&self) -> Option<usize> {
        self.held.iter().position(|held| *held)
    }

    /// How many buffers are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.iter().filter(|held| **held).count()
    }

    /// Whether no buffer is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.held.contains(&true)
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
    use std::collections::VecDeque;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_layout_is_sddfs() {
        assert_eq!((TAIL_OFFSET, HEAD_OFFSET, SIGNAL_OFFSET), (0, 2, 4));
        assert_eq!(
            (QUEUE_HEADER_LEN, DESCRIPTOR_LEN, BUFFER_SIZE),
            (8, 16, 2048)
        );
        assert_eq!(queue_len(128), Some(8 + 16 * 128));
        assert_eq!(queue_len(usize::MAX), None);
        assert_eq!(
            Descriptor::new(0x0807_0605_0403_0201, 0x0a09).to_bytes(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn the_owner_id_and_padding_are_ignored() {
        let mut bytes = Descriptor::new(4096, 60).to_bytes();
        bytes[10] = 0x3f; // owner id 63
        bytes[11..].fill(0xee);
        assert_eq!(Descriptor::from_bytes(&bytes), Descriptor::new(4096, 60));
    }

    #[test]
    fn capacities_are_powers_of_two_up_to_the_maximum() {
        assert!(Producer::new(&InMemoryQueue::<4>::new()).is_ok());
        assert_eq!(
            Producer::new(&InMemoryQueue::<3>::new()).err(),
            Some(QueueError::BadCapacity { capacity: 3 })
        );
        assert_eq!(
            Consumer::new(&InMemoryQueue::<0>::new()).err(),
            Some(QueueError::BadCapacity { capacity: 0 })
        );
        assert_eq!(checked_capacity(MAX_CAPACITY), Ok(MAX_CAPACITY));
        assert!(checked_capacity(u16::MAX).is_err());
    }

    #[test]
    fn descriptors_cross_in_order_and_the_queue_fills() {
        let queue = InMemoryQueue::<4>::new();
        let mut producer = Producer::new(&queue).unwrap();
        let mut consumer = Consumer::new(&queue).unwrap();
        for i in 0..4 {
            producer.enqueue(Descriptor::new(i * 2048, 0)).unwrap();
        }
        assert_eq!(
            producer.enqueue(Descriptor::default()),
            Err(QueueError::Full)
        );
        assert_eq!(producer.in_flight(), Ok(4));
        for i in 0..4 {
            assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(i * 2048, 0))));
        }
        assert_eq!(consumer.dequeue(), Ok(None));
        assert_eq!(producer.in_flight(), Ok(0));
    }

    #[test]
    fn the_indices_wrap_at_two_to_the_sixteen() {
        let queue = InMemoryQueue::<4>::new();
        let mut producer = Producer::new(&queue).unwrap();
        let mut consumer = Consumer::new(&queue).unwrap();
        for i in 0..70_000u64 {
            producer.enqueue(Descriptor::new(i, 1)).unwrap();
            assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(i, 1))));
        }
        assert_eq!(queue.load_tail(), (70_000u32 % 65_536) as u16);
    }

    #[test]
    fn a_corrupt_consumer_index_stops_the_producer() {
        let queue = InMemoryQueue::<4>::new();
        let mut producer = Producer::new(&queue).unwrap();
        producer.enqueue(Descriptor::new(0, 0)).unwrap();
        // The consumer claims to have taken descriptors never enqueued.
        queue.store_head(3);
        assert_eq!(
            producer.enqueue(Descriptor::new(2048, 0)),
            Err(QueueError::CorruptIndex { tail: 1, head: 3 })
        );
        assert_eq!(queue.load_tail(), 1, "nothing published");
        let mut slot = [0; DESCRIPTOR_LEN];
        queue.read_descriptor(1, &mut slot);
        assert_eq!(slot, [0; DESCRIPTOR_LEN], "nothing written");
    }

    #[test]
    fn a_corrupt_producer_index_stops_the_consumer() {
        let queue = InMemoryQueue::<4>::new();
        let mut consumer = Consumer::new(&queue).unwrap();
        queue.store_tail(5);
        assert_eq!(
            consumer.dequeue(),
            Err(QueueError::CorruptIndex { tail: 5, head: 0 })
        );
        assert_eq!(queue.load_head(), 0, "nothing taken");
        // A tail moved backwards is just as corrupt.
        queue.store_tail(u16::MAX);
        assert!(consumer.dequeue().is_err());
        // Back in range, the queue works again.
        queue.store_tail(1);
        assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::default())));
    }

    #[test]
    fn a_side_never_adopts_its_own_index_from_shared_memory() {
        let queue = InMemoryQueue::<4>::new();
        let mut producer = Producer::new(&queue).unwrap();
        let mut consumer = Consumer::new(&queue).unwrap();
        producer.enqueue(Descriptor::new(0, 1)).unwrap();
        // The consumer overwrites the producer's tail; the producer carries
        // on from its own copy, and its next store puts it right.
        queue.store_tail(0);
        producer.enqueue(Descriptor::new(2048, 2)).unwrap();
        assert_eq!(queue.load_tail(), 2);
        // The producer overwrites the consumer's head likewise.
        assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(0, 1))));
        queue.store_head(0);
        assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(2048, 2))));
        assert_eq!(queue.load_head(), 2);
    }

    #[test]
    fn signals_follow_sddfs_protocol() {
        let queue = InMemoryQueue::<4>::new();
        let producer = Producer::new(&queue).unwrap();
        let consumer = Consumer::new(&queue).unwrap();
        // A zeroed queue: the consumer wants the first notification.
        assert!(producer.signal_wanted());
        assert!(!producer.signal_wanted(), "once only");
        assert_eq!(queue.load_signal(), 1);
        consumer.request_signal();
        assert_eq!(queue.load_signal(), 0);
        assert!(producer.signal_wanted());
    }

    #[test]
    fn descriptors_are_checked_against_the_region() {
        let region = DataRegion::new(4).unwrap();
        assert_eq!((region.buffers(), region.len()), (4, 8192));
        assert_eq!(region.buffer(Descriptor::new(6144, 0)), Ok(3));
        assert_eq!(
            region.buffer(Descriptor::new(8192, 0)),
            Err(DescriptorError::OutsideRegion { offset: 8192 })
        );
        assert_eq!(
            region.buffer(Descriptor::new(u64::MAX, 0)),
            Err(DescriptorError::OutsideRegion { offset: u64::MAX })
        );
        assert_eq!(
            region.buffer(Descriptor::new(2048 + 100, 0)),
            Err(DescriptorError::NotOnBoundary { offset: 2148 })
        );
        assert_eq!(
            region.frame(Descriptor::new(2048, 60), 14..=1514),
            Ok(Frame {
                buffer: 1,
                offset: 2048,
                len: 60
            })
        );
        for len in [0, 13, 1515, 2049, u16::MAX] {
            assert_eq!(
                region.frame(Descriptor::new(2048, len), 14..=1514),
                Err(DescriptorError::BadLength { len })
            );
        }
        assert_eq!(
            region.frame(Descriptor::new(0, 2049), 0..=usize::MAX),
            Err(DescriptorError::BadLength { len: 2049 }),
            "never longer than a buffer"
        );
        assert_eq!(DataRegion::new(0), None);
        assert_eq!(DataRegion::new(usize::MAX), None);
        assert_eq!(region.offset_of(3), Some(6144));
        assert_eq!(region.offset_of(4), None);
    }

    #[test]
    fn a_buffer_set_holds_each_buffer_once() {
        let mut set = BufferSet::<4>::none();
        assert!(set.is_empty());
        assert!(set.insert(2));
        assert!(!set.insert(2), "already held");
        assert!(!set.insert(4), "no such buffer");
        assert_eq!((set.len(), set.first()), (1, Some(2)));
        assert!(set.remove(2));
        assert!(!set.remove(2), "not held");
        assert_eq!(BufferSet::<4>::all().len(), 4);
        assert_eq!(BufferSet::<4>::all().first(), Some(0));
    }

    /// One step of a property test: the producer's, the consumer's, or a
    /// hostile peer's write to an index.
    #[derive(Clone, Debug)]
    enum Step {
        Enqueue(u64, u16),
        Dequeue,
        CorruptHead(u16),
        CorruptTail(u16),
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => (any::<u64>(), any::<u16>()).prop_map(|(o, l)| Step::Enqueue(o, l)),
            4 => Just(Step::Dequeue),
            1 => any::<u16>().prop_map(Step::CorruptHead),
            1 => any::<u16>().prop_map(Step::CorruptTail),
        ]
    }

    proptest! {
        #[test]
        fn with_honest_sides_the_queue_is_a_fifo(
            steps in proptest::collection::vec(step(), 0..200)
        ) {
            let queue = InMemoryQueue::<8>::new();
            let mut producer = Producer::new(&queue).unwrap();
            let mut consumer = Consumer::new(&queue).unwrap();
            let mut model = VecDeque::new();
            for step in steps {
                match step {
                    Step::Enqueue(offset, len) => {
                        let descriptor = Descriptor::new(offset, len);
                        if model.len() == 8 {
                            prop_assert_eq!(producer.enqueue(descriptor), Err(QueueError::Full));
                        } else {
                            prop_assert_eq!(producer.enqueue(descriptor), Ok(()));
                            model.push_back(descriptor);
                        }
                    }
                    Step::Dequeue => prop_assert_eq!(consumer.dequeue(), Ok(model.pop_front())),
                    Step::CorruptHead(_) | Step::CorruptTail(_) => {}
                }
                prop_assert_eq!(usize::from(producer.in_flight().unwrap()), model.len());
            }
        }

        #[test]
        fn a_hostile_peer_cannot_make_a_side_overrun_or_panic(
            steps in proptest::collection::vec(step(), 0..200)
        ) {
            let queue = InMemoryQueue::<8>::new();
            let mut producer = Producer::new(&queue).unwrap();
            let mut consumer = Consumer::new(&queue).unwrap();
            for step in steps {
                match step {
                    Step::Enqueue(offset, len) => {
                        let head = queue.load_head();
                        let before = producer.tail;
                        match producer.enqueue(Descriptor::new(offset, len)) {
                            Ok(()) => prop_assert!(before.wrapping_sub(head) < 8),
                            Err(QueueError::Full) => prop_assert_eq!(before.wrapping_sub(head), 8),
                            Err(QueueError::CorruptIndex { .. }) => {
                                prop_assert!(before.wrapping_sub(head) > 8);
                            }
                            Err(other) => prop_assert!(false, "{other:?}"),
                        }
                    }
                    Step::Dequeue => {
                        let tail = queue.load_tail();
                        let before = consumer.head;
                        match consumer.dequeue() {
                            Ok(Some(_)) => {
                                let pending = tail.wrapping_sub(before);
                                prop_assert!((1..=8).contains(&pending));
                            }
                            Ok(None) => prop_assert_eq!(tail, before),
                            Err(QueueError::CorruptIndex { .. }) => {
                                prop_assert!(tail.wrapping_sub(before) > 8);
                            }
                            Err(other) => prop_assert!(false, "{other:?}"),
                        }
                    }
                    Step::CorruptHead(head) => queue.store_head(head),
                    Step::CorruptTail(tail) => queue.store_tail(tail),
                }
            }
        }

        #[test]
        fn descriptors_round_trip(offset in any::<u64>(), len in any::<u16>(), junk in any::<[u8; 6]>()) {
            let descriptor = Descriptor::new(offset, len);
            let mut bytes = descriptor.to_bytes();
            prop_assert_eq!(&bytes[10..], &[0u8; 6][..]);
            bytes[10..].copy_from_slice(&junk);
            prop_assert_eq!(Descriptor::from_bytes(&bytes), descriptor);
        }

        #[test]
        fn a_descriptor_is_accepted_exactly_when_it_fits(
            buffers in 1usize..64,
            offset in any::<u64>(),
            len in any::<u16>(),
        ) {
            let region = DataRegion::new(buffers).unwrap();
            let descriptor = Descriptor::new(offset, len);
            let inside = offset < (buffers * BUFFER_SIZE) as u64;
            let aligned = offset % BUFFER_SIZE as u64 == 0;
            let fits = (14..=1514).contains(&len);
            match region.frame(descriptor, 14..=1514) {
                Ok(frame) => {
                    prop_assert!(inside && aligned && fits);
                    prop_assert_eq!(frame.offset as u64, offset);
                    prop_assert_eq!(frame.buffer * BUFFER_SIZE, frame.offset);
                    prop_assert!(frame.buffer < buffers);
                    prop_assert_eq!(frame.len, usize::from(len));
                }
                Err(DescriptorError::OutsideRegion { .. }) => prop_assert!(!inside),
                Err(DescriptorError::NotOnBoundary { .. }) => prop_assert!(inside && !aligned),
                Err(DescriptorError::BadLength { .. }) => prop_assert!(inside && aligned && !fits),
            }
        }
    }
}
