// SPDX-License-Identifier: MIT OR Apache-2.0
//! A single-producer, single-consumer ring of fixed-size slots over shared
//! memory, with copy-then-validate on the consumer side.
//!
//! # Protocol
//!
//! The shared memory holds two `u32` indices and `capacity` slots of `SLOT`
//! bytes each. The **producer index** is written only by the producer and
//! counts records ever published; the **consumer index** is written only by
//! the consumer and counts records ever taken. Both run freely and wrap at
//! 2³², so `capacity` must be a power of two (at most [`MAX_CAPACITY`]) for
//! `index & (capacity - 1)` to name the same slot across the wrap.
//!
//! * Records in flight = `producer.wrapping_sub(consumer)`. Zero means empty,
//!   `capacity` means full, and **anything larger means the other side is
//!   faulty or hostile**: the ring reports [`RingError::CorruptIndices`] and
//!   touches nothing.
//! * Each side keeps its own index privately and only ever *reads* the
//!   other's, re-checking it every time. A peer can lie about its own index;
//!   it cannot make this side read or write outside the slot array.
//!
//! # Memory ordering
//!
//! This is the part that works by accident on a strongly ordered x86 host and
//! fails on Arm if it is wrong, which is why CI runs AArch64.
//!
//! * **Producer**: copy the record into the slot, *then* store the producer
//!   index with **release** ordering. The consumer's **acquire** load of that
//!   index therefore sees the slot contents.
//! * **Consumer**: copy the slot into private memory, *then* store the
//!   consumer index with **release** ordering. The producer's **acquire**
//!   load of that index therefore cannot let it overwrite the slot while the
//!   copy is still being made.
//!
//! [`RingMemory`], [`ProducerMemory`] and [`ConsumerMemory`]
//! implementations provide exactly those orderings.
//!
//! # Each side's view
//!
//! The memory traits are split by side: a [`Producer`] needs
//! [`ProducerMemory`] (it stores the producer index and writes slots) and a
//! [`Consumer`] needs [`ConsumerMemory`] (it stores the consumer index and
//! reads slots); both read the indices through their common supertrait
//! [`RingMemory`]. A view of the shared memory that implements only one
//! side — like a protection domain's, with the other side's region mapped
//! read-only — cannot be given to the other end of the ring: that mistake
//! does not compile.
//!
//! # Notifications
//!
//! The ring does not signal. After sending, the producer notifies the
//! consumer on their Microkit channel; on a notification, the consumer
//! drains the ring until [`Consumer::try_recv`] returns `Ok(None)`.
//!
//! # Restart epochs
//!
//! A side that restarts resumes from its own index, which lives in memory it
//! owns (`Producer::new`, `Consumer::new`). What its peer cannot see from the
//! indices is *that* it restarted, and so lost whatever it held of the
//! ring. So each side
//! also owns a **restart epoch** word beside its index:
//!
//! * when a protection domain starts - at boot, and again after each
//!   restart - each side it owns calls `begin_epoch` once, which adds one to
//!   its epoch (0 means "never started"; the count skips 0 when it wraps),
//!   and then notifies its peer;
//! * on each notification, the peer loads that epoch (`peer_epoch`) and
//!   compares it with the one it saw last ([`EpochWatch`]). A change means
//!   the other side has started again;
//! * a consumer that starts drops every record still in flight
//!   (`Consumer::skip_in_flight`): nothing sent to its last epoch is acted on,
//!   so a record that made it fault is not read again. The survivor, seeing
//!   the change, decides **per message type** what to send again - a request
//!   still unanswered, a whole frame - and what to let go, such as input
//!   events. A producer that starts keeps what it had published: every
//!   published record is complete, so its consumer takes them in order.
//!
//! An epoch, like an index, is written by the peer: a lying peer can make
//! this side believe it restarted, and so send things again or drop them,
//! but nothing more.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use crate::{DecodeError, Header, Message, RecvError, RingError, SendError, decode, encode};

/// The largest ring capacity, 2³¹ slots. Larger rings could not tell full
/// from empty with 32-bit free-running indices.
pub const MAX_CAPACITY: u32 = 1 << 31;

/// The shared memory behind a ring — two indices and `capacity` slots of
/// `SLOT` bytes — as either side sees it: what both sides read.
///
/// A side's view also implements [`ProducerMemory`] or [`ConsumerMemory`]
/// (see the module documentation, "Each side's view"). [`InMemoryRing`]
/// implements both, for host tests and queues within one address space.
/// The views over real Microkit shared-memory regions need `unsafe` (they
/// turn a mapped address into atomics) and belong in an approved-`unsafe`
/// crate (`nowayos-region`).
///
/// # Contract (all three traits)
///
/// * `load_*` have at least **acquire** ordering; `store_*` have at least
///   **release** ordering (see the module documentation).
/// * `write_slot` and `read_slot` copy whole slots and must tolerate the other
///   side changing the memory concurrently without undefined behaviour — for
///   example byte-wise relaxed atomics or volatile accesses. They need no
///   ordering of their own. They are only called with `slot < capacity()`;
///   an implementation must still ignore an out-of-range slot rather than
///   panic.
/// * `capacity` returns the same value every time.
/// * Each side's restart epoch lives beside its index, in memory that side
///   owns (see the module documentation, "Restart epochs"), with the same
///   orderings.
pub trait RingMemory<const SLOT: usize> {
    /// Number of slots: a power of two, at most [`MAX_CAPACITY`].
    fn capacity(&self) -> u32;
    /// Loads the producer index (acquire).
    fn load_producer_index(&self) -> u32;
    /// Loads the consumer index (acquire).
    fn load_consumer_index(&self) -> u32;
    /// Loads the producer's restart epoch (acquire).
    fn load_producer_epoch(&self) -> u32;
    /// Loads the consumer's restart epoch (acquire).
    fn load_consumer_epoch(&self) -> u32;
}

/// The producer's view of a ring's shared memory: it can also publish its
/// index and fill slots. Required by [`Producer`].
///
/// A view that is only a [`ConsumerMemory`] cannot be used by a producer:
///
/// ```compile_fail
/// use nowayos_ipc::{ConsumerMemory, Producer, RingMemory};
///
/// /// A consumer's view: it can store the consumer index and read slots.
/// struct ConsumerView;
/// impl RingMemory<8> for ConsumerView {
///     fn capacity(&self) -> u32 { 4 }
///     fn load_producer_index(&self) -> u32 { 0 }
///     fn load_consumer_index(&self) -> u32 { 0 }
///     fn load_producer_epoch(&self) -> u32 { 0 }
///     fn load_consumer_epoch(&self) -> u32 { 0 }
/// }
/// impl ConsumerMemory<8> for ConsumerView {
///     fn store_consumer_index(&self, _index: u32) {}
///     fn store_consumer_epoch(&self, _epoch: u32) {}
///     fn read_slot(&self, _slot: u32, _out: &mut [u8; 8]) {}
/// }
///
/// let view = ConsumerView;
/// let _ = Producer::new(&view); // a ConsumerView is not a ProducerMemory
/// ```
pub trait ProducerMemory<const SLOT: usize>: RingMemory<SLOT> {
    /// Stores the producer index (release).
    fn store_producer_index(&self, index: u32);
    /// Stores the producer's restart epoch (release).
    fn store_producer_epoch(&self, epoch: u32);
    /// Copies `record` into slot `slot`, which the producer owns.
    fn write_slot(&self, slot: u32, record: &[u8; SLOT]);
}

/// The consumer's view of a ring's shared memory: it can also publish its
/// index and copy slots out. Required by [`Consumer`].
///
/// A view that is only a [`ProducerMemory`] cannot be used by a consumer:
///
/// ```compile_fail
/// use nowayos_ipc::{Consumer, ProducerMemory, RingMemory};
///
/// /// A producer's view: it can store the producer index and write slots.
/// struct ProducerView;
/// impl RingMemory<8> for ProducerView {
///     fn capacity(&self) -> u32 { 4 }
///     fn load_producer_index(&self) -> u32 { 0 }
///     fn load_consumer_index(&self) -> u32 { 0 }
///     fn load_producer_epoch(&self) -> u32 { 0 }
///     fn load_consumer_epoch(&self) -> u32 { 0 }
/// }
/// impl ProducerMemory<8> for ProducerView {
///     fn store_producer_index(&self, _index: u32) {}
///     fn store_producer_epoch(&self, _epoch: u32) {}
///     fn write_slot(&self, _slot: u32, _record: &[u8; 8]) {}
/// }
///
/// let view = ProducerView;
/// let _ = Consumer::new(&view); // a ProducerView is not a ConsumerMemory
/// ```
///
/// The same view does make a producer (the control for the example above):
///
/// ```
/// # use nowayos_ipc::{Producer, ProducerMemory, RingMemory};
/// # struct ProducerView;
/// # impl RingMemory<8> for ProducerView {
/// #     fn capacity(&self) -> u32 { 4 }
/// #     fn load_producer_index(&self) -> u32 { 0 }
/// #     fn load_consumer_index(&self) -> u32 { 0 }
/// #     fn load_producer_epoch(&self) -> u32 { 0 }
/// #     fn load_consumer_epoch(&self) -> u32 { 0 }
/// # }
/// # impl ProducerMemory<8> for ProducerView {
/// #     fn store_producer_index(&self, _index: u32) {}
/// #     fn store_producer_epoch(&self, _epoch: u32) {}
/// #     fn write_slot(&self, _slot: u32, _record: &[u8; 8]) {}
/// # }
/// let view = ProducerView;
/// assert!(Producer::new(&view).is_ok());
/// ```
pub trait ConsumerMemory<const SLOT: usize>: RingMemory<SLOT> {
    /// Stores the consumer index (release).
    fn store_consumer_index(&self, index: u32);
    /// Stores the consumer's restart epoch (release).
    fn store_consumer_epoch(&self, epoch: u32);
    /// Copies slot `slot`, which the consumer owns, into `out`.
    fn read_slot(&self, slot: u32, out: &mut [u8; SLOT]);
}

/// A record copied out of the ring into private memory.
///
/// The only way to get one from a [`Consumer`] is by copy, so anything
/// decoded from it cannot be changed underneath the decoder. Use
/// [`header`](Self::header) to dispatch on the message kind when a channel
/// carries several.
#[derive(Clone, PartialEq, Eq)]
pub struct Record<const SLOT: usize> {
    bytes: [u8; SLOT],
}

impl<const SLOT: usize> Record<SLOT> {
    /// The record's header, checked.
    ///
    /// # Errors
    ///
    /// As for [`Header::parse`].
    pub fn header(&self) -> Result<Header, DecodeError> {
        Header::parse(&self.bytes)
    }

    /// Decodes the record as message type `M`.
    ///
    /// # Errors
    ///
    /// As for [`decode`](crate::decode).
    pub fn decode<M: Message>(&self) -> Result<M, DecodeError> {
        decode(&self.bytes)
    }

    /// The raw slot bytes (header, payload and zero padding).
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SLOT] {
        &self.bytes
    }
}

impl<const SLOT: usize> core::fmt::Debug for Record<SLOT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Record")
            .field("header", &self.header())
            .finish_non_exhaustive()
    }
}

/// Checks a ring capacity and returns its index mask.
fn index_mask(capacity: u32) -> Result<u32, RingError> {
    if capacity.is_power_of_two() && capacity <= MAX_CAPACITY {
        Ok(capacity.wrapping_sub(1))
    } else {
        Err(RingError::InvalidCapacity { capacity })
    }
}

/// Records in flight, or an error if the indices are impossible.
fn in_flight(producer: u32, consumer: u32, capacity: u32) -> Result<u32, RingError> {
    let used = producer.wrapping_sub(consumer);
    if used > capacity {
        Err(RingError::CorruptIndices { producer, consumer })
    } else {
        Ok(used)
    }
}

/// The epoch after `epoch`: one more, skipping 0 ("never started") when the
/// count wraps.
const fn next_epoch(epoch: u32) -> u32 {
    match epoch.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

/// What one side knows of its peer's restart epoch: the one it saw last
/// (see the module documentation, "Restart epochs"). Kept in the side's
/// private memory, so it starts empty each time the side starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EpochWatch {
    seen: Option<u32>,
}

/// How a peer's restart epoch compares with the one seen before
/// ([`EpochWatch::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochChange {
    /// The first epoch this side has seen: nothing to compare it with.
    First(u32),
    /// The same epoch as last time: the peer has not started again.
    Same(u32),
    /// A different epoch: the peer has started since it was last seen -
    /// for the first time, from epoch 0, or again - and holds nothing of
    /// what it held of the ring before.
    Changed {
        /// The epoch seen before.
        from: u32,
        /// The epoch now.
        to: u32,
    },
}

impl EpochWatch {
    /// A watch that has seen no epoch yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { seen: None }
    }

    /// Records `epoch`, the peer's restart epoch as just loaded
    /// (`Producer::peer_epoch`, `Consumer::peer_epoch`), and says how it
    /// compares with the one seen before.
    pub const fn observe(&mut self, epoch: u32) -> EpochChange {
        let change = match self.seen {
            None => EpochChange::First(epoch),
            Some(seen) if seen == epoch => EpochChange::Same(epoch),
            Some(seen) => EpochChange::Changed {
                from: seen,
                to: epoch,
            },
        };
        self.seen = Some(epoch);
        change
    }
}

/// The sending end of a ring. There must be exactly one per ring.
///
/// Provisional: this API may change.
pub struct Producer<'m, R: ?Sized, const SLOT: usize> {
    memory: &'m R,
    /// Our index: the only copy we trust.
    index: u32,
    capacity: u32,
    mask: u32,
}

impl<'m, R: ProducerMemory<SLOT> + ?Sized, const SLOT: usize> Producer<'m, R, SLOT> {
    /// Attaches to `memory` as its producer, resuming from the producer index
    /// already there (so a restarted producer carries on where it stopped).
    ///
    /// # Errors
    ///
    /// [`RingError`] if the capacity is invalid or the indices are
    /// inconsistent.
    pub fn new(memory: &'m R) -> Result<Self, RingError> {
        let capacity = memory.capacity();
        let mask = index_mask(capacity)?;
        let index = memory.load_producer_index();
        in_flight(index, memory.load_consumer_index(), capacity)?;
        Ok(Self {
            memory,
            index,
            capacity,
            mask,
        })
    }

    /// Starts this side's next restart epoch and returns it. Call it once
    /// each time the protection domain starts, before sending, then notify
    /// the consumer (see the module documentation, "Restart epochs").
    pub fn begin_epoch(&mut self) -> u32 {
        let epoch = next_epoch(self.memory.load_producer_epoch());
        self.memory.store_producer_epoch(epoch);
        epoch
    }

    /// The consumer's restart epoch, as it last stored it: 0 if it has never
    /// started.
    #[must_use]
    pub fn peer_epoch(&self) -> u32 {
        self.memory.load_consumer_epoch()
    }

    /// Slots free right now.
    ///
    /// # Errors
    ///
    /// [`RingError::CorruptIndices`] if the consumer index is impossible.
    pub fn free_slots(&self) -> Result<u32, RingError> {
        let used = in_flight(self.index, self.memory.load_consumer_index(), self.capacity)?;
        Ok(self.capacity.saturating_sub(used))
    }

    /// Encodes `message` into a zero-padded slot and publishes it.
    ///
    /// # Errors
    ///
    /// [`SendError::Full`] if no slot is free, [`SendError::Encode`] if the
    /// message does not fit a slot, [`SendError::Ring`] if the ring is
    /// corrupt. Nothing is published on error.
    pub fn try_send<M: Message>(&mut self, message: &M) -> Result<(), SendError> {
        let mut record = [0u8; SLOT];
        encode(message, &mut record)?;
        self.publish(&record)
    }

    /// Publishes `bytes` as one zero-padded slot, for records encoded
    /// elsewhere.
    ///
    /// # Errors
    ///
    /// As for [`try_send`](Self::try_send); `bytes` longer than `SLOT` is
    /// [`EncodeError::BufferTooSmall`](crate::EncodeError::BufferTooSmall).
    pub fn try_send_bytes(&mut self, bytes: &[u8]) -> Result<(), SendError> {
        let mut record = [0u8; SLOT];
        let target = record
            .get_mut(..bytes.len())
            .ok_or(crate::EncodeError::BufferTooSmall {
                needed: bytes.len(),
                available: SLOT,
            })?;
        target.copy_from_slice(bytes);
        self.publish(&record)
    }

    fn publish(&mut self, record: &[u8; SLOT]) -> Result<(), SendError> {
        if self.free_slots()? == 0 {
            return Err(SendError::Full);
        }
        self.memory.write_slot(self.index & self.mask, record);
        self.index = self.index.wrapping_add(1);
        // Release: the slot contents are visible before the new index is.
        self.memory.store_producer_index(self.index);
        Ok(())
    }
}

/// The receiving end of a ring. There must be exactly one per ring.
///
/// Every record is copied out of shared memory into private memory before it
/// is examined, and each record is consumed exactly once — including a
/// malformed one, so a hostile producer cannot wedge the queue.
///
/// Provisional: this API may change.
pub struct Consumer<'m, R: ?Sized, const SLOT: usize> {
    memory: &'m R,
    /// Our index: the only copy we trust.
    index: u32,
    capacity: u32,
    mask: u32,
}

impl<'m, R: ConsumerMemory<SLOT> + ?Sized, const SLOT: usize> Consumer<'m, R, SLOT> {
    /// Attaches to `memory` as its consumer, resuming from the consumer index
    /// already there.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the capacity is invalid or the indices are
    /// inconsistent.
    pub fn new(memory: &'m R) -> Result<Self, RingError> {
        let capacity = memory.capacity();
        let mask = index_mask(capacity)?;
        let index = memory.load_consumer_index();
        in_flight(memory.load_producer_index(), index, capacity)?;
        Ok(Self {
            memory,
            index,
            capacity,
            mask,
        })
    }

    /// Starts this side's next restart epoch and returns it. Call it once
    /// each time the protection domain starts, then drop what is in flight
    /// ([`skip_in_flight`](Self::skip_in_flight)) and notify the producer
    /// (see the module documentation, "Restart epochs").
    pub fn begin_epoch(&mut self) -> u32 {
        let epoch = next_epoch(self.memory.load_consumer_epoch());
        self.memory.store_consumer_epoch(epoch);
        epoch
    }

    /// The producer's restart epoch, as it last stored it: 0 if it has never
    /// started.
    #[must_use]
    pub fn peer_epoch(&self) -> u32 {
        self.memory.load_producer_epoch()
    }

    /// Drops every record in flight without reading it, and returns how
    /// many there were: for a side that has just started, so that nothing
    /// sent to its last epoch is acted on.
    ///
    /// # Errors
    ///
    /// [`RingError::CorruptIndices`] if the producer index is impossible;
    /// nothing is dropped.
    pub fn skip_in_flight(&mut self) -> Result<u32, RingError> {
        let producer = self.memory.load_producer_index();
        let dropped = in_flight(producer, self.index, self.capacity)?;
        self.index = producer;
        // Release, as after a read: the producer may reuse every slot.
        self.memory.store_consumer_index(self.index);
        Ok(dropped)
    }

    /// Records waiting right now.
    ///
    /// # Errors
    ///
    /// [`RingError::CorruptIndices`] if the producer index is impossible.
    pub fn pending(&self) -> Result<u32, RingError> {
        in_flight(self.memory.load_producer_index(), self.index, self.capacity)
    }

    /// Takes the next record, copied into private memory, or `Ok(None)` if
    /// the ring is empty.
    ///
    /// # Errors
    ///
    /// [`RingError::CorruptIndices`] if the producer index is impossible;
    /// nothing is consumed.
    pub fn try_recv_record(&mut self) -> Result<Option<Record<SLOT>>, RingError> {
        if self.pending()? == 0 {
            return Ok(None);
        }
        let mut record = Record { bytes: [0u8; SLOT] };
        // The copy: from here on only `record` is examined.
        self.memory
            .read_slot(self.index & self.mask, &mut record.bytes);
        self.index = self.index.wrapping_add(1);
        // Release: our reads of the slot complete before the producer can
        // observe that it may reuse it.
        self.memory.store_consumer_index(self.index);
        Ok(Some(record))
    }

    /// Takes the next record and decodes it as `M`, or returns `Ok(None)` if
    /// the ring is empty.
    ///
    /// # Errors
    ///
    /// [`RecvError::Decode`] if the record is malformed (it is still
    /// consumed); [`RecvError::Ring`] if the ring is corrupt (nothing is
    /// consumed).
    pub fn try_recv<M: Message>(&mut self) -> Result<Option<M>, RecvError> {
        match self.try_recv_record()? {
            Some(record) => Ok(Some(record.decode()?)),
            None => Ok(None),
        }
    }
}

/// A ring whose "shared memory" is an ordinary Rust value — for host tests,
/// and for queues between threads or components in one address space.
///
/// `SLOTS` is the capacity and must be a power of two; `SLOT` is the slot size
/// in bytes. Slot bytes are relaxed atomics, so a misbehaving peer writing
/// concurrently cannot cause undefined behaviour — the same property a real
/// shared-memory implementation must have.
///
/// ```
/// use nowayos_ipc::{Consumer, InMemoryRing, Producer};
///
/// let ring: InMemoryRing<4, 16> = InMemoryRing::new();
/// let mut tx = Producer::new(&ring).unwrap();
/// let mut rx = Consumer::new(&ring).unwrap();
/// tx.try_send_bytes(b"hello").unwrap();
/// let record = rx.try_recv_record().unwrap().unwrap();
/// assert_eq!(&record.as_bytes()[..5], b"hello");
/// assert!(rx.try_recv_record().unwrap().is_none());
/// ```
pub struct InMemoryRing<const SLOTS: usize, const SLOT: usize> {
    producer_index: AtomicU32,
    consumer_index: AtomicU32,
    producer_epoch: AtomicU32,
    consumer_epoch: AtomicU32,
    slots: [[AtomicU8; SLOT]; SLOTS],
}

impl<const SLOTS: usize, const SLOT: usize> InMemoryRing<SLOTS, SLOT> {
    /// An empty ring with both indices and both epochs at zero. Usable in a
    /// `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            producer_index: AtomicU32::new(0),
            consumer_index: AtomicU32::new(0),
            producer_epoch: AtomicU32::new(0),
            consumer_epoch: AtomicU32::new(0),
            slots: [const { [const { AtomicU8::new(0) }; SLOT] }; SLOTS],
        }
    }
}

impl<const SLOTS: usize, const SLOT: usize> Default for InMemoryRing<SLOTS, SLOT> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize, const SLOT: usize> RingMemory<SLOT> for InMemoryRing<SLOTS, SLOT> {
    fn capacity(&self) -> u32 {
        // A capacity that does not fit in u32 is reported as 0: invalid.
        u32::try_from(SLOTS).unwrap_or(0)
    }

    fn load_producer_index(&self) -> u32 {
        self.producer_index.load(Ordering::Acquire)
    }

    fn load_consumer_index(&self) -> u32 {
        self.consumer_index.load(Ordering::Acquire)
    }

    fn load_producer_epoch(&self) -> u32 {
        self.producer_epoch.load(Ordering::Acquire)
    }

    fn load_consumer_epoch(&self) -> u32 {
        self.consumer_epoch.load(Ordering::Acquire)
    }
}

impl<const SLOTS: usize, const SLOT: usize> ProducerMemory<SLOT> for InMemoryRing<SLOTS, SLOT> {
    fn store_producer_index(&self, index: u32) {
        self.producer_index.store(index, Ordering::Release);
    }

    fn store_producer_epoch(&self, epoch: u32) {
        self.producer_epoch.store(epoch, Ordering::Release);
    }

    fn write_slot(&self, slot: u32, record: &[u8; SLOT]) {
        let cells = usize::try_from(slot).ok().and_then(|i| self.slots.get(i));
        if let Some(cells) = cells {
            for (cell, byte) in cells.iter().zip(record) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }
    }
}

impl<const SLOTS: usize, const SLOT: usize> ConsumerMemory<SLOT> for InMemoryRing<SLOTS, SLOT> {
    fn store_consumer_index(&self, index: u32) {
        self.consumer_index.store(index, Ordering::Release);
    }

    fn store_consumer_epoch(&self, epoch: u32) {
        self.consumer_epoch.store(epoch, Ordering::Release);
    }

    fn read_slot(&self, slot: u32, out: &mut [u8; SLOT]) {
        let cells = usize::try_from(slot).ok().and_then(|i| self.slots.get(i));
        if let Some(cells) = cells {
            for (byte, cell) in out.iter_mut().zip(cells) {
                *byte = cell.load(Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::{EncodeError, Reader, Writer};

    #[derive(Debug, PartialEq)]
    struct Word(u32);

    impl Message for Word {
        const KIND: u16 = 0x0042;
        const VERSION: u16 = 1;
        const PAYLOAD_LEN: usize = 4;

        fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
            out.write_u32(self.0)
        }

        fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
            input.read_u32().map(Self)
        }
    }

    #[test]
    fn send_and_receive_in_order() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        for i in 0..4 {
            tx.try_send(&Word(i)).unwrap();
        }
        assert_eq!(tx.try_send(&Word(99)), Err(SendError::Full));
        assert_eq!(rx.pending(), Ok(4));
        for i in 0..4 {
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(i))));
        }
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        assert_eq!(tx.free_slots(), Ok(4));
    }

    #[test]
    fn rejects_bad_capacities() {
        let three: InMemoryRing<3, 16> = InMemoryRing::new();
        assert_eq!(
            Producer::new(&three).err(),
            Some(RingError::InvalidCapacity { capacity: 3 })
        );
        let zero: InMemoryRing<0, 16> = InMemoryRing::new();
        assert_eq!(
            Consumer::new(&zero).err(),
            Some(RingError::InvalidCapacity { capacity: 0 })
        );
    }

    #[test]
    fn message_too_big_for_slot_is_an_encode_error() {
        let ring: InMemoryRing<2, 8> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        assert_eq!(
            tx.try_send(&Word(1)),
            Err(SendError::Encode(EncodeError::BufferTooSmall {
                needed: 12,
                available: 8
            }))
        );
        assert_eq!(ring.load_producer_index(), 0);
    }

    #[test]
    fn hostile_producer_index_is_detected_and_nothing_is_consumed() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut rx = Consumer::new(&ring).unwrap();
        ring.store_producer_index(5); // claims 5 records in a 4-slot ring
        assert_eq!(
            rx.try_recv::<Word>(),
            Err(RecvError::Ring(RingError::CorruptIndices {
                producer: 5,
                consumer: 0
            }))
        );
        assert_eq!(ring.load_consumer_index(), 0);
        // Attaching to a ring that is already corrupt fails too.
        assert!(Consumer::new(&ring).is_err());
        assert!(Producer::new(&ring).is_err());
    }

    #[test]
    fn hostile_consumer_index_is_detected() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        ring.store_consumer_index(7); // "consumed" records never sent
        assert_eq!(
            tx.try_send(&Word(1)),
            Err(SendError::Ring(RingError::CorruptIndices {
                producer: 0,
                consumer: 7
            }))
        );
        assert_eq!(ring.load_producer_index(), 0);
    }

    #[test]
    fn malformed_record_is_consumed_not_stuck() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        tx.try_send_bytes(&[0xFF; 12]).unwrap();
        tx.try_send(&Word(7)).unwrap();
        assert!(matches!(rx.try_recv::<Word>(), Err(RecvError::Decode(_))));
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(7))));
    }

    #[test]
    fn indices_wrap_around_u32() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let start = u32::MAX - 2;
        ring.store_producer_index(start);
        ring.store_consumer_index(start);
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        for i in 0..20 {
            tx.try_send(&Word(i)).unwrap();
            tx.try_send(&Word(i + 1000)).unwrap();
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(i))));
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(i + 1000))));
        }
        assert_eq!(ring.load_producer_index(), start.wrapping_add(40));
    }

    #[test]
    fn a_restarted_side_resumes_from_shared_indices() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        {
            let mut tx = Producer::new(&ring).unwrap();
            tx.try_send(&Word(1)).unwrap();
            tx.try_send(&Word(2)).unwrap();
        }
        {
            let mut rx = Consumer::new(&ring).unwrap();
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(1))));
        }
        // A new consumer (as after a restart) picks up at record 2.
        let mut rx = Consumer::new(&ring).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(2))));
        let mut tx = Producer::new(&ring).unwrap();
        assert_eq!(tx.free_slots(), Ok(4));
        tx.try_send(&Word(3)).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(3))));
    }

    #[test]
    fn epochs_start_at_one_and_skip_zero_when_they_wrap() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        assert_eq!((tx.peer_epoch(), rx.peer_epoch()), (0, 0));
        assert_eq!(tx.begin_epoch(), 1);
        assert_eq!(rx.begin_epoch(), 1);
        assert_eq!(tx.begin_epoch(), 2);
        assert_eq!((tx.peer_epoch(), rx.peer_epoch()), (1, 2));
        ring.store_producer_epoch(u32::MAX);
        assert_eq!(tx.begin_epoch(), 1);
    }

    #[test]
    fn a_watch_reports_the_first_epoch_then_same_or_changed() {
        let mut watch = EpochWatch::new();
        assert_eq!(watch.observe(0), EpochChange::First(0));
        assert_eq!(watch.observe(0), EpochChange::Same(0));
        assert_eq!(watch.observe(1), EpochChange::Changed { from: 0, to: 1 });
        assert_eq!(watch.observe(1), EpochChange::Same(1));
        assert_eq!(watch.observe(2), EpochChange::Changed { from: 1, to: 2 });
        assert_eq!(EpochWatch::default(), EpochWatch::new());
    }

    #[test]
    fn a_restarted_consumer_drops_what_is_in_flight_and_its_producer_sees_the_new_epoch() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        tx.begin_epoch();
        let mut watch = EpochWatch::new();
        {
            let mut rx = Consumer::new(&ring).unwrap();
            assert_eq!(rx.begin_epoch(), 1);
            assert_eq!(rx.skip_in_flight(), Ok(0));
        }
        assert_eq!(watch.observe(tx.peer_epoch()), EpochChange::First(1));
        for i in 0..3 {
            tx.try_send(&Word(i)).unwrap();
        }
        {
            // Takes record 0, then stops (faults) with 1 and 2 in flight.
            let mut rx = Consumer::new(&ring).unwrap();
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(0))));
        }
        // Started again: a new epoch, and nothing from before is read.
        let mut rx = Consumer::new(&ring).unwrap();
        assert_eq!(rx.begin_epoch(), 2);
        assert_eq!(rx.skip_in_flight(), Ok(2));
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        // The producer sees the change and sends again what must be sent.
        assert_eq!(
            watch.observe(tx.peer_epoch()),
            EpochChange::Changed { from: 1, to: 2 }
        );
        assert_eq!(tx.free_slots(), Ok(4));
        tx.try_send(&Word(2)).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(2))));
    }

    #[test]
    fn skipping_with_a_hostile_producer_index_drops_nothing() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut rx = Consumer::new(&ring).unwrap();
        ring.store_producer_index(9);
        assert_eq!(
            rx.skip_in_flight(),
            Err(RingError::CorruptIndices {
                producer: 9,
                consumer: 0
            })
        );
        assert_eq!(ring.load_consumer_index(), 0);
    }

    #[test]
    fn records_dispatch_on_kind() {
        let ring: InMemoryRing<2, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        tx.try_send(&Word(5)).unwrap();
        let record = rx.try_recv_record().unwrap().unwrap();
        let header = record.header().unwrap();
        assert_eq!(header.kind, Word::KIND);
        assert_eq!(record.decode::<Word>(), Ok(Word(5)));
        // Zero padding after the 12-byte record.
        assert_eq!(record.as_bytes()[12..], [0; 4]);
    }

    #[test]
    fn works_through_trait_objects() {
        let ring: InMemoryRing<2, 16> = InMemoryRing::new();
        let tx_view: &dyn ProducerMemory<16> = &ring;
        let rx_view: &dyn ConsumerMemory<16> = &ring;
        let mut tx = Producer::new(tx_view).unwrap();
        let mut rx = Consumer::new(rx_view).unwrap();
        tx.try_send(&Word(11)).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(11))));
        assert_eq!(tx_view.load_consumer_index(), 1);
    }

    #[test]
    fn usable_as_a_static() {
        static RING: InMemoryRing<2, 16> = InMemoryRing::new();
        let mut tx = Producer::new(&RING).unwrap();
        tx.try_send(&Word(3)).unwrap();
        let mut rx = Consumer::new(&RING).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(3))));
    }
}
