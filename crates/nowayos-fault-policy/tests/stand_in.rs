// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the supervisor's stand-in counts: whatever
//! the bytes, reading them never panics; it reads exactly what
//! `tools/board-system` writes for counts; and any single flipped bit is
//! refused.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_fault_policy::Counts;
use nowayos_fault_policy::stand_in::{self, RECORD_LEN, STAND_IN_LEN, Unreadable};
use proptest::prelude::*;

fn counts() -> impl Strategy<Value = Counts> {
    (any::<u8>(), any::<u8>()).prop_map(|(restarts, sensitive)| Counts {
        restarts,
        sensitive,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// The counts a stand-in holds read back as themselves.
    #[test]
    fn counts_read_back(counts in counts()) {
        prop_assert_eq!(stand_in::read(&stand_in::stand_in(counts)), Ok(counts));
        prop_assert_eq!(stand_in::decode(&stand_in::encode(counts)), Ok(counts));
    }

    /// Any bytes at all: no panic, and what reads is exactly a stand-in's
    /// first bytes; bytes past them never matter.
    #[test]
    fn any_bytes_read_only_as_a_stand_in(bytes in prop::collection::vec(any::<u8>(), 0..32)) {
        if let Ok(counts) = stand_in::read(&bytes) {
            prop_assert_eq!(&bytes[..STAND_IN_LEN], &stand_in::stand_in(counts)[..]);
        }
        if let Ok(counts) = stand_in::decode(&bytes) {
            prop_assert_eq!(&bytes[..], &stand_in::encode(counts)[..]);
        }
    }

    /// One flipped bit anywhere - in the length word, the version, a count
    /// or the check - is refused, never read as other counts.
    #[test]
    fn a_flipped_bit_is_refused(counts in counts(), bit in 0..STAND_IN_LEN * 8) {
        let mut bytes = stand_in::stand_in(counts);
        bytes[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(stand_in::read(&bytes).is_err());
    }

    /// A record cut anywhere short of its end reads as cut short, or as
    /// missing when nothing of it is left.
    #[test]
    fn a_record_cut_short_is_told_apart(counts in counts(), len in 0..RECORD_LEN) {
        let record = stand_in::encode(counts);
        let expected = if len == 0 {
            Unreadable::Missing
        } else {
            Unreadable::CutShort { len }
        };
        prop_assert_eq!(stand_in::decode(&record[..len]), Err(expected));
    }
}
