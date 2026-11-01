// SPDX-License-Identifier: MIT OR Apache-2.0
//! A lossy ring: one producer that never waits, one consumer that
//! loses the oldest records when it falls behind - the ring from the USB
//! host PD.
//!
//! `nowayos_ipc`'s [`Producer`](nowayos_ipc::Producer) and
//! [`Consumer`](nowayos_ipc::Consumer) ring never loses a record: when it is full the producer must wait or drop the
//! newest. A stream whose old records are worth less than new ones - a
//! microphone's sound, which a call wants now or never - wants the
//! opposite, and a producer that serves hardware on a deadline must not
//! wait for a consumer at all. Here **the producer always writes**: each
//! record goes into the next slot, over whatever is there, and the
//! consumer takes the oldest record the ring still holds, counting those
//! written over before it read them as lost ([`LossyConsumer::lost`]).
//!
//! It uses the same shared memory as that ring, through the same views
//! ([`ProducerMemory`], [`ConsumerMemory`]; `nowayos-region`'s `RingTx` and
//! `RingRx` over two Microkit regions), with the same indices and restart
//! epochs. Only what they mean differs:
//!
//! * the **producer index** counts the records ever published, as before,
//!   but nothing stops it running more than the capacity ahead of the
//!   consumer;
//! * the **consumer index** counts the records the consumer has passed,
//!   read or lost. The producer never reads it to decide anything: a
//!   consumer that stops, or lies, cannot hold the producer up.
//!   [`LossyProducer::behind`] reports it, for counts only.
//!
//! # The window
//!
//! The consumer reads only the newest `capacity - 1` records published:
//! the slot after them is the one the producer may be writing now. A record
//! older than that is lost. The consumer copies a slot out, and then checks
//! that the producer had not, by the end of the copy, published far enough
//! to begin writing over it: if it had, the copy may be torn, and it is
//! thrown away and counted lost too.
//!
//! # Memory ordering
//!
//! Each side keeps the other ring's orderings - the producer stores its
//! index with **release** after writing a slot, and the consumer loads it
//! with **acquire** before reading one - and adds one fence, as a
//! sequence lock does:
//!
//! * **Producer:** a **release fence**, then the slot's bytes, then the
//!   index. The fence puts the index stored by the last record before any
//!   byte of this one.
//! * **Consumer:** the slot's bytes, then an **acquire fence**, then the
//!   producer index loaded again. If any byte copied came from a later
//!   record written into the same slot, the fences synchronise, and the
//!   index loaded after the copy is at least that record's: the check
//!   above sees it and throws the copy away.
//!
//! Slot bytes are relaxed atomics in every view (the
//! [`RingMemory`](nowayos_ipc::RingMemory) contract), so a copy torn this way is never undefined behaviour - only
//! a copy to throw away.
//!
//! # A hostile peer
//!
//! A producer can publish any index: the consumer then reads the newest
//! window as that index places it, which may be old or never-written
//! slots, and counts the rest lost. Records are still copied before they
//! are examined and decoded with every check, so all a lying producer can
//! do is send bad records or none, as on the other ring. A consumer can
//! store any index: the producer only reports it.
//!
//! Provisional: here, beside its one user, the audio channel; it moves to
//! `nowayos-ipc` when a second channel needs it.

use core::sync::atomic::{Ordering, fence};

use nowayos_ipc::{
    ConsumerMemory, DecodeError, EncodeError, MAX_CAPACITY, Message, ProducerMemory, RingError,
    SendError, decode, encode,
};

/// How many times one [`LossyConsumer::try_recv_record`] copies a slot
/// before it gives up for now: a copy is thrown away only when the
/// producer laps the consumer while it copies, which a producer at any
/// sane rate cannot do twice running.
const ATTEMPTS: u32 = 4;

/// The checked capacity's mask: a power of two, as for `nowayos_ipc`'s
/// ring, at most [`MAX_CAPACITY`], and at least 2, so the window holds a
/// record.
fn lossy_mask(capacity: u32) -> Result<u32, RingError> {
    if capacity >= 2 && capacity.is_power_of_two() && capacity <= MAX_CAPACITY {
        Ok(capacity.wrapping_sub(1))
    } else {
        Err(RingError::InvalidCapacity { capacity })
    }
}

/// The epoch after `epoch`: one more, skipping 0 ("never started") when the
/// count wraps - as `nowayos_ipc`'s ring counts them.
const fn next_epoch(epoch: u32) -> u32 {
    match epoch.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

/// The writing end of a lossy ring. There must be exactly one per ring.
///
/// Provisional: the API with the audio channel it was made for.
pub struct LossyProducer<'m, R: ?Sized, const SLOT: usize> {
    memory: &'m R,
    /// Our index: the only copy we trust.
    index: u32,
    mask: u32,
}

impl<'m, R: ProducerMemory<SLOT> + ?Sized, const SLOT: usize> LossyProducer<'m, R, SLOT> {
    /// Attaches to `memory` as its producer, resuming from the producer
    /// index already there. The consumer index is not checked: whatever it
    /// says, the producer writes.
    ///
    /// # Errors
    ///
    /// [`RingError::InvalidCapacity`] unless the capacity is a power of two
    /// from 2 to [`MAX_CAPACITY`].
    pub fn new(memory: &'m R) -> Result<Self, RingError> {
        let mask = lossy_mask(memory.capacity())?;
        Ok(Self {
            memory,
            index: memory.load_producer_index(),
            mask,
        })
    }

    /// Starts this side's next restart epoch and returns it, as
    /// [`Producer::begin_epoch`](nowayos_ipc::Producer::begin_epoch) does.
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

    /// The records published so far, wrapping at 2^32.
    #[must_use]
    pub const fn published(&self) -> u32 {
        self.index
    }

    /// How many records the consumer says it has yet to pass: what it has
    /// not read, including any it will find lost. For counts only - the
    /// consumer writes it, and nothing here depends on it.
    #[must_use]
    pub fn behind(&self) -> u32 {
        self.index.wrapping_sub(self.memory.load_consumer_index())
    }

    /// Encodes `message` into a zero-padded slot and publishes it, over the
    /// oldest record if the ring is full.
    ///
    /// # Errors
    ///
    /// [`SendError::Encode`] if the message does not fit a slot; nothing is
    /// published.
    pub fn send<M: Message>(&mut self, message: &M) -> Result<(), SendError> {
        let mut record = [0u8; SLOT];
        encode(message, &mut record)?;
        self.publish(&record);
        Ok(())
    }

    /// Publishes `bytes` as one zero-padded slot, for records encoded
    /// elsewhere.
    ///
    /// # Errors
    ///
    /// [`SendError::Encode`] ([`EncodeError::BufferTooSmall`]) if `bytes`
    /// is longer than `SLOT`; nothing is published.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), SendError> {
        let mut record = [0u8; SLOT];
        record
            .get_mut(..bytes.len())
            .ok_or(EncodeError::BufferTooSmall {
                needed: bytes.len(),
                available: SLOT,
            })?
            .copy_from_slice(bytes);
        self.publish(&record);
        Ok(())
    }

    fn publish(&mut self, record: &[u8; SLOT]) {
        // Release fence: the index the last record published is ordered
        // before every byte of this one, so a consumer that copies any of
        // them sees that index, or a later one, when it checks its copy.
        fence(Ordering::Release);
        self.memory.write_slot(self.index & self.mask, record);
        self.index = self.index.wrapping_add(1);
        // Release: the slot's bytes are visible before the new index is.
        self.memory.store_producer_index(self.index);
    }
}

/// The reading end of a lossy ring. There must be exactly one per ring.
///
/// Every record is copied out of shared memory before it is examined, and
/// kept only if the producer had not begun to write over it by the end of
/// the copy.
///
/// Provisional: the API with the audio channel it was made for.
pub struct LossyConsumer<'m, R: ?Sized, const SLOT: usize> {
    memory: &'m R,
    /// Our index: the only copy we trust.
    index: u32,
    /// The records the window holds: the capacity less one.
    window: u32,
    mask: u32,
    lost: u64,
}

impl<'m, R: ConsumerMemory<SLOT> + ?Sized, const SLOT: usize> LossyConsumer<'m, R, SLOT> {
    /// Attaches to `memory` as its consumer, resuming from the consumer
    /// index already there.
    ///
    /// # Errors
    ///
    /// [`RingError::InvalidCapacity`] unless the capacity is a power of two
    /// from 2 to [`MAX_CAPACITY`].
    pub fn new(memory: &'m R) -> Result<Self, RingError> {
        let capacity = memory.capacity();
        let mask = lossy_mask(capacity)?;
        Ok(Self {
            memory,
            index: memory.load_consumer_index(),
            window: capacity.wrapping_sub(1),
            mask,
            lost: 0,
        })
    }

    /// Starts this side's next restart epoch and returns it, as
    /// [`Consumer::begin_epoch`](nowayos_ipc::Consumer::begin_epoch) does.
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

    /// The records lost so far: written over before this side read them.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Passes every record published so far, unread and not counted lost:
    /// for a side that has just started, or wants only what comes next.
    /// Returns how many it passed, up to the window.
    pub fn skip_to_newest(&mut self) -> u32 {
        let producer = self.memory.load_producer_index();
        let passed = producer.wrapping_sub(self.index).min(self.window);
        self.index = producer;
        self.memory.store_consumer_index(self.index);
        passed
    }

    /// Moves past every record older than the window `producer` places,
    /// counting them lost.
    fn catch_up(&mut self, producer: u32) {
        let behind = producer.wrapping_sub(self.index);
        if behind > self.window {
            let skipped = behind.wrapping_sub(self.window);
            self.lost = self.lost.saturating_add(u64::from(skipped));
            self.index = producer.wrapping_sub(self.window);
        }
    }

    /// Takes the oldest record the ring still holds, its slot's bytes
    /// copied into private memory - to dispatch on with
    /// `nowayos_ipc::Header::parse` and decode with `nowayos_ipc::decode` -
    /// or `None` if there is none: the ring is empty, or the producer wrote
    /// over every copy this call made. Records written over before they
    /// were read are counted ([`Self::lost`]).
    pub fn try_recv_record(&mut self) -> Option<[u8; SLOT]> {
        let mut record = None;
        for _ in 0..ATTEMPTS {
            // Acquire: the slots the index covers are visible.
            let producer = self.memory.load_producer_index();
            self.catch_up(producer);
            if producer == self.index {
                break;
            }
            let mut bytes = [0u8; SLOT];
            // The copy: from here on only `bytes` is examined.
            self.memory.read_slot(self.index & self.mask, &mut bytes);
            // Acquire fence: a byte of any later record in this slot,
            // copied above, makes the index loaded next at least that
            // record's.
            fence(Ordering::Acquire);
            let after = self.memory.load_producer_index();
            if after.wrapping_sub(self.index) <= self.window {
                self.index = self.index.wrapping_add(1);
                record = Some(bytes);
                break;
            }
            // Written over while it was copied: lost, and on to the
            // oldest the window still holds.
            self.catch_up(after);
        }
        // Release: our reads complete before the producer could see this
        // index - for its counts; it never waits on it.
        self.memory.store_consumer_index(self.index);
        record
    }

    /// Takes the oldest record the ring still holds and decodes it as `M`,
    /// or returns `Ok(None)` if there is none.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if the record is malformed; it is still passed.
    pub fn try_recv<M: Message>(&mut self) -> Result<Option<M>, DecodeError> {
        match self.try_recv_record() {
            Some(record) => Ok(Some(decode(&record)?)),
            None => Ok(None),
        }
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
    use super::*;
    use core::sync::atomic::AtomicU32;
    use nowayos_ipc::{InMemoryRing, Reader, RingMemory, Writer};
    use proptest::prelude::*;

    #[derive(Debug, PartialEq)]
    struct Word(u32);

    impl Message for Word {
        const KIND: u16 = 0x0043;
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
    fn records_come_in_order_while_the_consumer_keeps_up() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        let mut rx = LossyConsumer::new(&ring).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        for n in 0..3 {
            tx.send(&Word(n)).unwrap();
        }
        assert_eq!(tx.behind(), 3);
        for n in 0..3 {
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(n))));
        }
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        assert_eq!((rx.lost(), tx.behind(), tx.published()), (0, 0, 3));
    }

    #[test]
    fn a_slow_consumer_loses_the_oldest_and_the_producer_never_waits() {
        let ring: InMemoryRing<4, 16> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        let mut rx = LossyConsumer::new(&ring).unwrap();
        // Ten records into four slots: the window holds the newest three.
        for n in 0..10 {
            tx.send(&Word(n)).unwrap();
        }
        assert_eq!(tx.behind(), 10);
        for n in 7..10 {
            assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(n))));
        }
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        assert_eq!(rx.lost(), 7);
        // And it carries on from there.
        tx.send(&Word(10)).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(10))));
        assert_eq!(rx.lost(), 7);
    }

    #[test]
    fn skipping_to_the_newest_passes_what_waits() {
        let ring: InMemoryRing<8, 16> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        let mut rx = LossyConsumer::new(&ring).unwrap();
        for n in 0..5 {
            tx.send(&Word(n)).unwrap();
        }
        assert_eq!(rx.skip_to_newest(), 5);
        assert_eq!(rx.try_recv::<Word>(), Ok(None));
        tx.send(&Word(5)).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(5))));
        assert_eq!(rx.lost(), 0);
    }

    #[test]
    fn capacities_below_two_or_not_a_power_of_two_are_refused() {
        let one: InMemoryRing<1, 16> = InMemoryRing::new();
        assert_eq!(
            LossyProducer::new(&one).err(),
            Some(RingError::InvalidCapacity { capacity: 1 })
        );
        assert!(LossyConsumer::new(&one).is_err());
        let three: InMemoryRing<3, 16> = InMemoryRing::new();
        assert!(LossyProducer::new(&three).is_err());
        assert!(LossyConsumer::new(&three).is_err());
    }

    #[test]
    fn a_record_longer_than_a_slot_is_not_published() {
        let ring: InMemoryRing<2, 4> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        assert!(matches!(
            tx.send_bytes(&[1; 5]),
            Err(SendError::Encode(EncodeError::BufferTooSmall { .. }))
        ));
        assert!(matches!(tx.send(&Word(1)), Err(SendError::Encode(_))));
        assert_eq!(tx.published(), 0);
    }

    #[test]
    fn epochs_count_up_and_skip_zero() {
        let ring: InMemoryRing<2, 16> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        let mut rx = LossyConsumer::new(&ring).unwrap();
        assert_eq!((tx.peer_epoch(), rx.peer_epoch()), (0, 0));
        assert_eq!((tx.begin_epoch(), rx.begin_epoch()), (1, 1));
        assert_eq!((tx.peer_epoch(), rx.peer_epoch()), (1, 1));
    }

    /// A consumer's view whose producer index moves on by `jump` between
    /// the load before the copy and the load after it: a producer that
    /// laps the consumer mid-copy.
    struct Lapping {
        ring: InMemoryRing<4, 16>,
        loads: AtomicU32,
        jump: u32,
    }

    impl RingMemory<16> for Lapping {
        fn capacity(&self) -> u32 {
            self.ring.capacity()
        }
        fn load_producer_index(&self) -> u32 {
            let loads = self.loads.fetch_add(1, Ordering::Relaxed);
            let index = self.ring.load_producer_index();
            if loads % 2 == 1 {
                index + self.jump
            } else {
                index
            }
        }
        fn load_consumer_index(&self) -> u32 {
            self.ring.load_consumer_index()
        }
        fn load_producer_epoch(&self) -> u32 {
            0
        }
        fn load_consumer_epoch(&self) -> u32 {
            0
        }
    }

    impl ConsumerMemory<16> for Lapping {
        fn store_consumer_index(&self, index: u32) {
            self.ring.store_consumer_index(index);
        }
        fn store_consumer_epoch(&self, _epoch: u32) {}
        fn read_slot(&self, slot: u32, out: &mut [u8; 16]) {
            self.ring.read_slot(slot, out);
        }
    }

    #[test]
    fn a_copy_the_producer_wrote_over_is_thrown_away() {
        let lapping = Lapping {
            ring: InMemoryRing::new(),
            loads: AtomicU32::new(0),
            jump: 3,
        };
        let mut tx = LossyProducer::new(&lapping.ring).unwrap();
        tx.send(&Word(0)).unwrap();
        let mut rx = LossyConsumer::new(&lapping).unwrap();
        // The copy is followed by a load three records on: index 0's slot
        // is the next to be written, so the copy is thrown away and counted
        // lost, and the window then holds nothing more.
        assert_eq!(rx.try_recv_record(), None);
        assert_eq!(rx.lost(), 1);
        // Two records on is still inside the window: the copy is kept.
        let lapping = Lapping {
            ring: InMemoryRing::new(),
            loads: AtomicU32::new(0),
            jump: 2,
        };
        let mut tx = LossyProducer::new(&lapping.ring).unwrap();
        tx.send(&Word(9)).unwrap();
        let mut rx = LossyConsumer::new(&lapping).unwrap();
        assert_eq!(rx.try_recv::<Word>(), Ok(Some(Word(9))));
        assert_eq!(rx.lost(), 0);
    }

    #[test]
    fn a_producer_racing_a_consumer_never_hands_it_a_torn_record() {
        // Each record is 64 copies of one byte, its index's low byte, and
        // its index in the first four: a torn copy mixes two records.
        let ring: InMemoryRing<4, 64> = InMemoryRing::new();
        const RECORDS: u32 = 20_000;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut tx = LossyProducer::new(&ring).unwrap();
                for n in 0..RECORDS {
                    let mut record = [n.to_le_bytes()[0]; 64];
                    record[..4].copy_from_slice(&n.to_le_bytes());
                    tx.send_bytes(&record).unwrap();
                }
            });
            let mut rx = LossyConsumer::new(&ring).unwrap();
            let mut last = None::<u32>;
            let mut taken = 0u64;
            while last != Some(RECORDS - 1) {
                let Some(record) = rx.try_recv_record() else {
                    std::thread::yield_now();
                    continue;
                };
                let bytes = &record;
                let n = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                assert!(bytes[4..].iter().all(|byte| *byte == n.to_le_bytes()[0]));
                assert!(last.is_none_or(|last| n > last), "{n} after {last:?}");
                last = Some(n);
                taken += 1;
            }
            assert_eq!(taken + rx.lost(), u64::from(RECORDS));
        });
    }

    proptest! {
        #[test]
        fn every_record_is_read_or_counted_lost(
            bursts in proptest::collection::vec((0u32..12, 0u32..12), 1..20),
        ) {
            let ring: InMemoryRing<8, 16> = InMemoryRing::new();
            let mut tx = LossyProducer::new(&ring).unwrap();
            let mut rx = LossyConsumer::new(&ring).unwrap();
            let (mut sent, mut read) = (0u32, 0u64);
            let mut last = None::<u32>;
            for (writes, reads) in bursts {
                for _ in 0..writes {
                    tx.send(&Word(sent)).unwrap();
                    sent += 1;
                }
                for _ in 0..reads {
                    if let Some(Word(n)) = rx.try_recv::<Word>().unwrap() {
                        prop_assert!(last.is_none_or(|last| n > last));
                        // Only the newest seven are ever read.
                        prop_assert!(sent - n <= 7);
                        last = Some(n);
                        read += 1;
                    }
                }
            }
            while rx.try_recv::<Word>().unwrap().is_some() {
                read += 1;
            }
            prop_assert_eq!(read + rx.lost(), u64::from(sent));
        }
    }
}
