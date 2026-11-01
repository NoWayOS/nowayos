// SPDX-License-Identifier: MIT OR Apache-2.0
//! A seed for a protection domain's own generator.
//!
//! Every PD that needs randomness runs a generator of its own, seeded by
//! the entropy PD, and **seeds travel one way**: the entropy PD writes them
//! on a ring of the consumer's own - a `nowayos-ipc` ring whose messages
//! region only the entropy PD writes and only that consumer reads - and only
//! the consumer's index comes back. This crate is the record that ring
//! carries, its geometry, and the consumer's side: [`take`] copies the next
//! record out of the ring, moves the consumer's index past it, and only then
//! decodes it, so no seed is ever used twice, even by a PD that restarts in
//! between.
//!
//! **No seed, no service**: a record of all zeros is
//! refused - a source that hands out zeros is broken, and zero is smoltcp's
//! default seed, which the network stack PD must never run on - and a PD
//! with no seed ring, or nothing on it yet, is not ready.
//!
//! # The record
//!
//! A fixed-size `nowayos-ipc` [`Message`]: an 8-byte header naming its kind
//! ([`SEED_KIND`], a block of its own), layout version 1 and payload length,
//! then the payload:
//!
//! | Offset | Size | Field | Rule |
//! | --- | --- | --- | --- |
//! | 0 | 32 | `seed` | 256 bits from the entropy PD; not all zero |
//!
//! Every 32-byte string but zeros is a valid seed, so a record that decodes
//! has exactly one encoding.
//!
//! # Who sends it
//!
//! The entropy PD (`nowayos-entropy-source`): it puts seeds on each
//! consumer's ring with [`give`], and with [`overwrite_taken`] writes over
//! each slot once the consumer's index has passed it, so a taken seed does
//! not linger. In the network stack's test systems a test
//! PD still writes one fixed test seed - never random, so it may run only
//! there (`tools/grant-rules.toml`, `[test_only]`) - and overwrites nothing.
//!
//! Provisional: the first cut of the seed ring.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(kani)]
mod proofs;

use core::fmt;

use nowayos_bounded::Wipe;
use nowayos_ipc::{
    Consumer, ConsumerMemory, DecodeError, EncodeError, Message, Producer, ProducerMemory, Reader,
    RecvError, SendError, Writer, encoded_len,
};

/// Message kind of a [`Seed`] on a seed ring.
pub const SEED_KIND: u16 = 0x0601;

/// A seed's bytes: 256 bits.
pub const SEED_LEN: usize = 32;

/// A seed record's length: header and payload.
pub const SEED_RECORD_LEN: usize = encoded_len::<Seed>();

/// The slots in a seed ring: a consumer takes a seed at start-up and before
/// each use that needs a fresh one, so a few are plenty.
pub const SEED_RING_SLOTS: usize = 4;

/// The size of each slot in a seed ring: room for one seed record.
pub const SEED_RING_SLOT: usize = 64;

/// The most seeds the entropy PD puts on one consumer's ring in a second,
/// once the ring's first fill is taken: each consumer's rate, so that none
/// can drain the source. At most
/// `SEED_RING_SLOTS + SEEDS_PER_SECOND * t` seeds in `t` seconds.
pub const SEEDS_PER_SECOND: u32 = 10;

// A seed record fits a slot, and the ring's capacity is a power of two, as
// nowayos-ipc's ring requires.
const _: () = assert!(SEED_RECORD_LEN <= SEED_RING_SLOT && SEED_RING_SLOTS.is_power_of_two());

/// A seed: 256 bits, never all zero. It is not `Clone` or `Copy`, so it is
/// not duplicated by accident, and its `Debug` form never shows its bytes.
/// Wipe it ([`Wipe::wipe`]) once the generator it keys is keyed.
#[derive(PartialEq, Eq)]
pub struct Seed {
    bytes: [u8; SEED_LEN],
}

impl Seed {
    /// The seed `bytes`, or `None` if they are all zero.
    #[must_use]
    pub fn new(bytes: [u8; SEED_LEN]) -> Option<Self> {
        if bytes.iter().all(|byte| *byte == 0) {
            None
        } else {
            Some(Self { bytes })
        }
    }

    /// The seed's bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SEED_LEN] {
        &self.bytes
    }

    /// The seed folded to 64 bits: its four little-endian 64-bit words,
    /// exclusive-ored. For a generator that takes a 64-bit seed (smoltcp's
    /// `random_seed`); 0 only if the words cancel out.
    #[must_use]
    pub fn fold_u64(&self) -> u64 {
        self.bytes.chunks_exact(8).fold(0, |folded, word| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(word);
            folded ^ u64::from_le_bytes(bytes)
        })
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(..)")
    }
}

impl Wipe for Seed {
    /// Zeroes the seed's bytes in place. Ordinary safe code, with
    /// `nowayos-bounded`'s limits: a copy made earlier is not reached.
    fn wipe(&mut self) {
        self.bytes = [0; SEED_LEN];
        core::hint::black_box(&mut self.bytes);
    }
}

impl Message for Seed {
    const KIND: u16 = SEED_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = SEED_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.bytes)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::new(input.read_array()?).ok_or(DecodeError::InvalidValue { field: "seed" })
    }
}

/// Takes the next seed off a consumer's seed ring: the record is copied out
/// and the consumer's index moved past it before it is decoded, so it is
/// never taken twice. `Ok(None)` if the ring is empty.
///
/// # Errors
///
/// [`RecvError::Decode`] if the record is not a seed - of another kind,
/// version or length, or all zeros; it is consumed, and the next call looks
/// at the next one. [`RecvError::Ring`] if the ring's indices are
/// impossible: nothing is consumed.
pub fn take<R: ConsumerMemory<SEED_RING_SLOT> + ?Sized>(
    ring: &R,
) -> Result<Option<Seed>, RecvError> {
    Consumer::new(ring)?.try_recv::<Seed>()
}

/// Puts `seed` on a consumer's seed ring: the entropy PD's side.
/// `Ok(true)` if it was written, `Ok(false)` if the ring is full, when the
/// seed is untouched and the caller keeps or wipes it.
///
/// # Errors
///
/// [`SendError::Ring`] if the ring's indices are impossible: nothing is
/// written.
pub fn give<R: ProducerMemory<SEED_RING_SLOT> + ?Sized>(
    ring: &R,
    seed: &Seed,
) -> Result<bool, SendError> {
    match Producer::new(ring)?.try_send(seed) {
        Ok(()) => Ok(true),
        Err(SendError::Full) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Writes zeros over every slot whose seed the consumer has taken since
/// `mark` - what this function last returned, or the consumer's index at
/// the start - and returns the new mark, the consumer's index now: the entropy PD's
/// side, so no seed lingers on a ring once taken. A slot
/// that already holds a newer seed, not yet taken, is left alone.
///
/// A consumer index that is impossible - past what was given, or behind
/// `mark` - is not trusted: nothing is written and the mark stays.
pub fn overwrite_taken<R: ProducerMemory<SEED_RING_SLOT> + ?Sized>(ring: &R, mark: u32) -> u32 {
    let given = ring.load_producer_index();
    let taken_to = ring.load_consumer_index();
    let capacity = ring.capacity();
    let taken = taken_to.wrapping_sub(mark);
    let in_flight = given.wrapping_sub(taken_to);
    let Some(mask) = capacity.checked_sub(1) else {
        return mark;
    };
    if in_flight > capacity || taken > given.wrapping_sub(mark) {
        return mark;
    }
    // The taken slots no newer seed has been written to: at most the
    // `capacity - in_flight` slots just behind the consumer's index.
    let stale = taken.min(capacity.saturating_sub(in_flight));
    for back in 1..=stale {
        ring.write_slot(taken_to.wrapping_sub(back) & mask, &[0; SEED_RING_SLOT]);
    }
    taken_to
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{
        ConsumerMemory, HEADER_LEN, InMemoryRing, RingError, RingMemory, decode, decode_exact,
        encode,
    };
    use proptest::prelude::*;

    use super::*;

    type Ring = InMemoryRing<SEED_RING_SLOTS, SEED_RING_SLOT>;

    fn seed(first: u8) -> Seed {
        let mut bytes = [0u8; SEED_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = first.wrapping_add(index as u8);
        }
        Seed::new(bytes).unwrap()
    }

    #[test]
    fn the_layout_is_a_header_and_thirty_two_bytes() {
        assert_eq!(SEED_RECORD_LEN, 40);
        let mut out = [0xaa; 64];
        assert_eq!(encode(&seed(1), &mut out), Ok(40));
        assert_eq!(&out[..8], &[0x01, 0x06, 1, 0, 32, 0, 0, 0]);
        assert_eq!(out[8], 1);
        assert_eq!(out[39], 32);
        assert_eq!(out[40], 0xaa, "nothing written past the record");
    }

    #[test]
    fn zeros_are_never_a_seed() {
        assert!(Seed::new([0; SEED_LEN]).is_none());
        let mut record = [0u8; SEED_RECORD_LEN];
        encode(&seed(1), &mut record).unwrap();
        record[HEADER_LEN..].fill(0);
        assert_eq!(
            decode_exact::<Seed>(&record),
            Err(DecodeError::InvalidValue { field: "seed" })
        );
        // One bit is enough.
        record[SEED_RECORD_LEN - 1] = 0x80;
        assert!(decode_exact::<Seed>(&record).is_ok());
    }

    #[test]
    fn the_fold_is_the_words_exclusive_ored() {
        let mut bytes = [0u8; SEED_LEN];
        bytes[0] = 1;
        bytes[8] = 2;
        bytes[23] = 0x80;
        bytes[31] = 0x01;
        let folded = Seed::new(bytes).unwrap().fold_u64();
        assert_eq!(folded, 1 ^ 2 ^ (0x80 << 56) ^ (0x01 << 56));
        // Words that cancel out fold to 0: the caller refuses that.
        let mut cancel = [0u8; SEED_LEN];
        cancel[0] = 7;
        cancel[8] = 7;
        assert_eq!(Seed::new(cancel).unwrap().fold_u64(), 0);
    }

    #[test]
    fn debug_never_shows_the_bytes_and_wiping_zeroes_them() {
        let mut s = seed(0x41);
        assert_eq!(format!("{s:?}"), "Seed(..)");
        s.wipe();
        assert_eq!(s.as_bytes(), &[0u8; SEED_LEN]);
    }

    #[test]
    fn other_records_are_refused_by_their_header() {
        let mut record = [0u8; SEED_RECORD_LEN];
        encode(&seed(1), &mut record).unwrap();
        let mut wrong_kind = record;
        wrong_kind[0] = 0x02;
        assert_eq!(
            decode::<Seed>(&wrong_kind),
            Err(DecodeError::WrongKind {
                expected: SEED_KIND,
                found: 0x0602
            })
        );
        let mut wrong_version = record;
        wrong_version[2] = 2;
        assert!(matches!(
            decode::<Seed>(&wrong_version),
            Err(DecodeError::UnsupportedVersion { .. })
        ));
        assert!(matches!(
            decode::<Seed>(&record[..SEED_RECORD_LEN - 1]),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn a_seed_is_taken_once_and_a_bad_record_is_consumed() {
        let ring = Ring::new();
        let mut producer = Producer::new(&ring).unwrap();
        assert_eq!(take(&ring), Ok(None));
        let mut zeros = [0u8; SEED_RECORD_LEN];
        encode(&seed(1), &mut zeros).unwrap();
        zeros[HEADER_LEN..].fill(0);
        producer.try_send_bytes(&zeros).unwrap();
        producer.try_send(&seed(9)).unwrap();
        assert_eq!(
            take(&ring),
            Err(RecvError::Decode(DecodeError::InvalidValue {
                field: "seed"
            }))
        );
        assert_eq!(take(&ring), Ok(Some(seed(9))));
        assert_eq!(take(&ring), Ok(None), "never taken twice");
    }

    #[test]
    fn a_corrupt_ring_gives_nothing() {
        let ring = Ring::new();
        ring.store_producer_index(SEED_RING_SLOTS as u32 + 1);
        assert!(matches!(
            take(&ring),
            Err(RecvError::Ring(RingError::CorruptIndices { .. }))
        ));
    }

    /// Slot `slot`'s bytes, as the consumer would copy them.
    fn slot_bytes(ring: &Ring, slot: u32) -> [u8; SEED_RING_SLOT] {
        let mut out = [0u8; SEED_RING_SLOT];
        ring.read_slot(slot, &mut out);
        out
    }

    #[test]
    fn the_entropy_pd_gives_until_the_ring_is_full() {
        let ring = Ring::new();
        for first in 1..=4 {
            assert_eq!(give(&ring, &seed(first)), Ok(true));
        }
        assert_eq!(give(&ring, &seed(5)), Ok(false), "full: nothing written");
        assert_eq!(take(&ring).unwrap(), Some(seed(1)));
        assert_eq!(give(&ring, &seed(5)), Ok(true));
    }

    #[test]
    fn a_taken_seed_is_overwritten_and_the_rest_are_kept() {
        let ring = Ring::new();
        for first in 1..=4 {
            give(&ring, &seed(first)).unwrap();
        }
        assert_eq!(take(&ring).unwrap(), Some(seed(1)));
        assert_eq!(take(&ring).unwrap(), Some(seed(2)));
        assert_eq!(overwrite_taken(&ring, 0), 2);
        assert_eq!(slot_bytes(&ring, 0), [0; SEED_RING_SLOT]);
        assert_eq!(slot_bytes(&ring, 1), [0; SEED_RING_SLOT]);
        assert_eq!(
            take(&ring).unwrap(),
            Some(seed(3)),
            "the seeds not yet taken are kept"
        );
        assert_eq!(take(&ring).unwrap(), Some(seed(4)));
        assert_eq!(overwrite_taken(&ring, 2), 4);
        assert_eq!(
            overwrite_taken(&ring, 4),
            4,
            "nothing taken since: nothing to do"
        );
    }

    #[test]
    fn a_slot_a_newer_seed_holds_is_left_alone() {
        let ring = Ring::new();
        for first in 1..=4 {
            give(&ring, &seed(first)).unwrap();
        }
        for _ in 0..4 {
            take(&ring).unwrap();
        }
        // Slots 0 and 1 hold two newer seeds before the PD looks.
        give(&ring, &seed(5)).unwrap();
        give(&ring, &seed(6)).unwrap();
        assert_eq!(overwrite_taken(&ring, 0), 4);
        assert_eq!(slot_bytes(&ring, 2), [0; SEED_RING_SLOT]);
        assert_eq!(slot_bytes(&ring, 3), [0; SEED_RING_SLOT]);
        assert_eq!(take(&ring).unwrap(), Some(seed(5)));
        assert_eq!(take(&ring).unwrap(), Some(seed(6)));
    }

    #[test]
    fn an_impossible_consumer_index_is_not_trusted() {
        let ring = Ring::new();
        give(&ring, &seed(1)).unwrap();
        ring.store_consumer_index(3);
        assert_eq!(overwrite_taken(&ring, 0), 0, "past what was given");
        assert_ne!(slot_bytes(&ring, 0), [0; SEED_RING_SLOT], "nothing written");
        ring.store_consumer_index(1);
        assert_eq!(overwrite_taken(&ring, 0), 1);
        give(&ring, &seed(2)).unwrap();
        ring.store_consumer_index(2);
        assert_eq!(overwrite_taken(&ring, 1), 2);
        ring.store_consumer_index(1);
        assert_eq!(overwrite_taken(&ring, 2), 2, "moved back behind the mark");
    }

    /// One step of a seed ring's life, for the property below.
    #[derive(Clone, Debug)]
    enum Step {
        Give(u8),
        Take,
        Overwrite,
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (1u8..=200).prop_map(Step::Give),
            Just(Step::Take),
            Just(Step::Overwrite)
        ]
    }

    proptest! {
        /// However gives, takes and overwrites interleave, every seed not
        /// yet taken is taken intact and in order, and after each overwrite
        /// every slot that holds no seed waiting to be taken is zeros.
        #[test]
        fn overwriting_never_loses_a_seed_and_leaves_no_taken_one(steps in proptest::collection::vec(step(), 0..64)) {
            let ring = Ring::new();
            let mut waiting = std::collections::VecDeque::new();
            let mut mark = 0u32;
            for step in steps {
                match step {
                    Step::Give(first) => {
                        if give(&ring, &seed(first)).unwrap() {
                            waiting.push_back(first);
                        } else {
                            prop_assert_eq!(waiting.len(), SEED_RING_SLOTS);
                        }
                    }
                    Step::Take => match waiting.pop_front() {
                        Some(first) => prop_assert_eq!(take(&ring).unwrap(), Some(seed(first))),
                        None => prop_assert!(take(&ring).unwrap().is_none()),
                    },
                    Step::Overwrite => {
                        mark = overwrite_taken(&ring, mark);
                        let taken_to = ring.load_consumer_index();
                        prop_assert_eq!(mark, taken_to);
                        for slot in 0..SEED_RING_SLOTS as u32 {
                            let holds_waiting = (0..waiting.len() as u32)
                                .any(|ahead| taken_to.wrapping_add(ahead) & 3 == slot);
                            if !holds_waiting {
                                prop_assert_eq!(slot_bytes(&ring, slot), [0; SEED_RING_SLOT]);
                            }
                        }
                    }
                }
            }
        }

        #[test]
        fn every_non_zero_seed_round_trips(bytes in any::<[u8; SEED_LEN]>()) {
            let mut record = [0u8; SEED_RECORD_LEN];
            match Seed::new(bytes) {
                Some(s) => {
                    prop_assert_eq!(encode(&s, &mut record), Ok(SEED_RECORD_LEN));
                    let back = decode_exact::<Seed>(&record).unwrap();
                    prop_assert_eq!(back.as_bytes(), &bytes);
                }
                None => prop_assert!(bytes.iter().all(|b| *b == 0)),
            }
        }

        #[test]
        fn decoding_never_panics_and_keeps_the_rule(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
            if let Ok(s) = decode::<Seed>(&bytes) {
                prop_assert!(s.as_bytes().iter().any(|b| *b != 0));
                prop_assert_eq!(&bytes[HEADER_LEN..SEED_RECORD_LEN], &s.as_bytes()[..]);
            }
            let _ = decode_exact::<Seed>(&bytes);
        }
    }
}
