// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the update state: every byte string is a
//! copy that checks and decodes, encoding back to the same bytes, or is
//! refused with a typed error, and nothing panics; a write cut short after
//! any number of bytes, in either copy, leaves exactly the old state or
//! exactly the new one; no write lowers the rollback index or the trust
//! list; and with both copies lost the store says so and writes nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_ipc::{DecodeError, HEADER_LEN, decode_exact, encode};
use nowayos_update::state::{
    COPY_LEN, Lowered, STATE_RECORD_LEN, Slot, StateCopy, StateError, StateRecord, StateStorage,
    StateStore, StorageError, StoreError, Trial, UpdateState, crc32, decode_copy, encode_copy,
};
use nowayos_update::trust_list::MAX_KEYS;
use nowayos_update::{
    KEY_ID_LEN, KEY_LEN, ListedKey, NamedSignature, Purpose, SIGNATURE_LEN, TrustList,
    TrustListContents,
};
use proptest::prelude::*;

/// A trust list of `version` with one root key and one signature: a
/// record that keeps every rule, signatures unchecked here.
fn list(version: u32, fill: u8) -> TrustList {
    let keys = [ListedKey {
        purpose: Purpose::TrustList,
        key: [fill; KEY_LEN],
    }];
    let contents = TrustListContents::new(version, false, &keys).unwrap();
    let signature = NamedSignature {
        key_id: [fill; KEY_ID_LEN],
        signature: [fill.wrapping_add(1); SIGNATURE_LEN],
    };
    TrustList::new(contents, &[signature]).unwrap()
}

fn slot() -> impl Strategy<Value = Slot> {
    prop_oneof![Just(Slot::A), Just(Slot::B)]
}

/// A state that keeps every rule, its index and list version at least
/// `floor` and `list_floor`.
fn state_from(floor: u32, list_floor: u32) -> impl Strategy<Value = UpdateState> {
    (
        slot(),
        any::<bool>(),
        floor..=floor.saturating_add(1_000),
        0u32..=1_000,
        prop::option::weighted(
            0.6,
            (list_floor..=list_floor.saturating_add(100), any::<u8>()),
        ),
    )
        .prop_map(
            move |(boots, trial, rollback_index, ahead, held)| UpdateState {
                boots,
                trial: trial.then(|| Trial {
                    slot: boots.other(),
                    security_version: rollback_index.saturating_add(ahead),
                    image_key: [u8::try_from(ahead % 256).unwrap(); KEY_ID_LEN],
                }),
                rollback_index,
                trust_list: held.map(|(version, fill)| list(version, fill)),
            },
        )
}

fn state() -> impl Strategy<Value = UpdateState> {
    state_from(0, 0)
}

/// Two copies in memory; a write stops after `cut` bytes, if set, as a
/// reset would stop it.
#[derive(Clone)]
struct Memory {
    copies: [[u8; COPY_LEN]; 2],
    cut: Option<usize>,
    fail_reads: bool,
}

impl Memory {
    fn blank() -> Self {
        Self {
            copies: [[0; COPY_LEN]; 2],
            cut: None,
            fail_reads: false,
        }
    }

    /// `state` provisioned in both copies, the first `generation`.
    fn with(state: &UpdateState, generation: u64) -> Self {
        let mut memory = Self::blank();
        memory.copies[0] = encode_copy(generation, state).unwrap();
        memory.copies[1] = encode_copy(generation - 1, state).unwrap_or([0; COPY_LEN]);
        memory
    }

    const fn index(copy: StateCopy) -> usize {
        match copy {
            StateCopy::A => 0,
            StateCopy::B => 1,
        }
    }
}

impl StateStorage for Memory {
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError> {
        if self.fail_reads {
            return Err(StorageError);
        }
        *out = self.copies[Self::index(copy)];
        Ok(())
    }

    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError> {
        let target = &mut self.copies[Self::index(copy)];
        match self.cut {
            Some(cut) if cut < COPY_LEN => {
                target[..cut].copy_from_slice(&bytes[..cut]);
                Err(StorageError)
            }
            _ => {
                *target = *bytes;
                Ok(())
            }
        }
    }
}

#[test]
fn crc32_matches_its_check_value() {
    // The check value every CRC-32/ISO-HDLC implementation gives.
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn the_record_layout_matches_its_table() {
    let record = StateRecord {
        boots: Slot::B,
        trial: Some(Trial {
            slot: Slot::A,
            security_version: 0x0807_0605,
            image_key: [0xEE; KEY_ID_LEN],
        }),
        rollback_index: 0x0403_0201,
        holds_trust_list: true,
    };
    let mut out = [0u8; STATE_RECORD_LEN];
    assert_eq!(encode(&record, &mut out), Ok(STATE_RECORD_LEN));
    assert_eq!(out[..HEADER_LEN], [4, 0, 1, 0, 48, 0, 0, 0]);
    assert_eq!(
        out[HEADER_LEN..HEADER_LEN + 16],
        [2, 1, 1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0]
    );
    assert_eq!(out[HEADER_LEN + 16..], [0xEE; KEY_ID_LEN]);
    assert_eq!(StateRecord::decode(&out), Ok(record));
}

#[test]
fn each_rule_of_the_record_is_refused_with_its_own_error() {
    let good = StateRecord {
        boots: Slot::A,
        trial: Some(Trial {
            slot: Slot::B,
            security_version: 9,
            image_key: [5; KEY_ID_LEN],
        }),
        rollback_index: 7,
        holds_trust_list: false,
    };
    let mut bytes = [0u8; STATE_RECORD_LEN];
    encode(&good, &mut bytes).unwrap();
    let at = HEADER_LEN;
    let cases: [(usize, u8, StateError); 9] = [
        (at, 0, StateError::UnknownSlot { code: 0 }),
        (at, 3, StateError::UnknownSlot { code: 3 }),
        (at + 1, 3, StateError::UnknownSlot { code: 3 }),
        (at + 1, 1, StateError::TrialIsBooting),
        (at + 2, 2, StateError::UnknownHeldMark { code: 2 }),
        (at + 3, 1, StateError::Frame(DecodeError::NonZeroPadding)),
        (at + 15, 1, StateError::Frame(DecodeError::NonZeroPadding)),
        // The trial's version below the index.
        (at + 8, 6, StateError::TrialVersion),
        // A version with no trial.
        (at + 1, 0, StateError::TrialVersion),
    ];
    // An image key with no trial.
    let mut keyed = bytes;
    keyed[at + 1] = 0;
    keyed[at + 8..at + 12].fill(0);
    assert_eq!(StateRecord::decode(&keyed), Err(StateError::TrialKey));
    for (offset, value, expected) in cases {
        let mut bad = bytes;
        bad[offset] = value;
        assert_eq!(
            StateRecord::decode(&bad),
            Err(expected),
            "byte {offset} = {value}"
        );
    }
}

#[test]
fn both_copies_lost_is_reported_and_nothing_is_written() {
    let mut store = StateStore::new(Memory::blank());
    assert_eq!(store.read(), Err(StoreError::Lost));
    let state = UpdateState {
        boots: Slot::A,
        trial: None,
        rollback_index: 0,
        trust_list: None,
    };
    assert_eq!(store.write(&state), Err(StoreError::Lost));
    let memory = store.into_inner();
    assert_eq!(memory.copies, [[0; COPY_LEN]; 2], "nothing written");
}

#[test]
fn a_reading_error_is_passed_on_never_taken_for_a_lost_copy() {
    let state = UpdateState {
        boots: Slot::A,
        trial: None,
        rollback_index: 3,
        trust_list: None,
    };
    let mut memory = Memory::with(&state, 2);
    memory.fail_reads = true;
    let mut store = StateStore::new(memory);
    assert_eq!(store.read(), Err(StoreError::Storage(StorageError)));
    assert_eq!(store.write(&state), Err(StoreError::Storage(StorageError)));
}

#[test]
fn dropping_the_held_list_lowers_it() {
    let held = UpdateState {
        boots: Slot::A,
        trial: None,
        rollback_index: 1,
        trust_list: Some(list(0, 7)),
    };
    let mut store = StateStore::new(Memory::with(&held, 5));
    let dropped = UpdateState {
        trust_list: None,
        ..held.clone()
    };
    assert_eq!(
        store.write(&dropped),
        Err(StoreError::Lower(Lowered::TrustList {
            held: 0,
            offered: 0
        }))
    );
    assert_eq!(store.read(), Ok(held));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any bytes: a copy that decodes encodes back to exactly those bytes;
    /// the record's decoder agrees with `nowayos-ipc`'s on what it accepts.
    #[test]
    fn any_bytes_decode_or_are_refused(
        bytes in prop::collection::vec(any::<u8>(), COPY_LEN..=COPY_LEN),
        record in prop::collection::vec(any::<u8>(), 0..=STATE_RECORD_LEN + 1),
    ) {
        let copy: [u8; COPY_LEN] = bytes.try_into().unwrap();
        if let Ok((generation, state)) = decode_copy(&copy) {
            prop_assert_eq!(encode_copy(generation, &state).unwrap(), copy);
        }
        // With a good CRC, so the record and list are read.
        let mut sealed = copy;
        let crc = crc32(&sealed[..COPY_LEN - 4]);
        sealed[COPY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        if let Ok((generation, state)) = decode_copy(&sealed) {
            prop_assert_eq!(encode_copy(generation, &state).unwrap(), sealed);
        }
        let ours = StateRecord::decode(&record);
        let theirs = decode_exact::<StateRecord>(&record);
        prop_assert_eq!(ours.is_ok(), theirs.is_ok());
        if let Ok(decoded) = ours {
            let mut out = [0u8; STATE_RECORD_LEN];
            encode(&decoded, &mut out).unwrap();
            prop_assert_eq!(&out[..], &record[..]);
        }
    }

    /// A good copy round-trips, and any one byte changed is refused.
    #[test]
    fn a_copy_round_trips_and_any_byte_changed_is_refused(
        state in state(),
        generation in 1u64..,
        at in 0..COPY_LEN,
        flip in 1u8..,
    ) {
        let copy = encode_copy(generation, &state).unwrap();
        prop_assert_eq!(decode_copy(&copy), Ok((generation, state)));
        let mut damaged = copy;
        damaged[at] ^= flip;
        prop_assert!(decode_copy(&damaged).is_err());
    }

    /// Random writes, some cut short: the index and the list's version read
    /// back never fall, and a write that would lower either is refused.
    #[test]
    fn no_write_lowers_the_index_or_the_trust_list(
        first in state(),
        writes in prop::collection::vec((state(), prop::option::of(0..COPY_LEN)), 1..12),
    ) {
        let mut store = StateStore::new(Memory::with(&first, 1));
        let mut held = first;
        for (offered, cut) in writes {
            let mut storage = store.into_inner();
            storage.cut = cut;
            store = StateStore::new(storage);
            let result = store.write(&offered);
            let lowers = offered.rollback_index < held.rollback_index
                || offered.trust_list_version() < held.trust_list_version()
                || (held.trust_list.is_some() && offered.trust_list.is_none());
            if lowers {
                prop_assert!(matches!(result, Err(StoreError::Lower(_))), "{:?}", result);
            }
            let read = store.read().unwrap();
            prop_assert!(read.rollback_index >= held.rollback_index);
            prop_assert!(read.trust_list_version() >= held.trust_list_version());
            match result {
                Ok(()) => prop_assert_eq!(&read, &offered),
                // A cut write leaves the copy as it was, or whole when the
                // bytes it left out already held the new ones.
                Err(StoreError::Storage(_)) => {
                    prop_assert!(read == held || read == offered, "{:?}", read);
                }
                Err(_) => prop_assert_eq!(&read, &held),
            }
            held = read;
        }
    }
}

#[test]
fn a_cut_write_whose_missing_bytes_were_already_new_leaves_the_new_state() {
    // Found by the torn-write property below: the copy written was already
    // the new copy but for its first byte, so a write cut after one byte
    // left it whole. The write still reports the storage's error.
    let old = UpdateState {
        boots: Slot::A,
        trial: None,
        rollback_index: 1,
        trust_list: None,
    };
    let newer = UpdateState {
        rollback_index: 2,
        ..old.clone()
    };
    let mut memory = Memory::with(&old, 5);
    let whole = encode_copy(6, &newer).unwrap();
    memory.copies[1] = whole;
    memory.copies[1][0] ^= 0xFF;
    memory.cut = Some(1);
    let mut store = StateStore::new(memory);
    assert_eq!(store.read().unwrap(), old, "the copy is not whole yet");
    assert_eq!(store.write(&newer), Err(StoreError::Storage(StorageError)));
    assert_eq!(store.read().unwrap(), newer);
}

#[test]
fn the_copy_length_holds_the_largest_list() {
    // A list of the most keys fits, and still round-trips.
    let keys: Vec<ListedKey> = (0..MAX_KEYS)
        .map(|entry| ListedKey {
            purpose: if entry == 0 {
                Purpose::TrustList
            } else {
                Purpose::Image
            },
            key: [u8::try_from(entry).unwrap() + 1; KEY_LEN],
        })
        .collect();
    let contents = TrustListContents::new(u32::MAX, true, &keys).unwrap();
    let signatures = [
        NamedSignature {
            key_id: [1; KEY_ID_LEN],
            signature: [2; SIGNATURE_LEN],
        },
        NamedSignature {
            key_id: [3; KEY_ID_LEN],
            signature: [4; SIGNATURE_LEN],
        },
    ];
    let state = UpdateState {
        boots: Slot::B,
        trial: Some(Trial {
            slot: Slot::A,
            security_version: u32::MAX,
            image_key: [0xFF; KEY_ID_LEN],
        }),
        rollback_index: u32::MAX,
        trust_list: Some(TrustList::new(contents, &signatures).unwrap()),
    };
    let copy = encode_copy(u64::MAX, &state).unwrap();
    assert_eq!(decode_copy(&copy), Ok((u64::MAX, state)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// A write cut short after any number of bytes, whichever copy counts,
    /// leaves exactly the old state, or the new one once it is whole: every
    /// cut point, so fewer cases. "Whole" is byte for byte: a cut can leave a
    /// whole copy when the bytes it left out already held the new ones (the
    /// last byte of the CRC, one time in 256), and then the new state is what
    /// must be read.
    #[test]
    fn a_write_cut_anywhere_leaves_the_old_state_or_the_new(
        old in state(),
        newer in state_from(1_000, 100),
        generation in 2u64..1_000_000,
        b_counts in any::<bool>(),
        lost_other in any::<bool>(),
    ) {
        let mut memory = Memory::with(&old, generation);
        if b_counts {
            memory.copies.swap(0, 1);
        }
        if lost_other {
            // The copy not counting already torn: garbage.
            let other = usize::from(!b_counts);
            memory.copies[other][..8].copy_from_slice(&[0xA5; 8]);
        }
        let newer = UpdateState {
            rollback_index: newer.rollback_index.max(old.rollback_index),
            trust_list: newer.trust_list.or(old.trust_list.clone()),
            trial: None,
            ..newer
        };
        // The copy that does not count is the one written, at the next
        // generation.
        let target = usize::from(!b_counts);
        let whole = encode_copy(generation + 1, &newer).unwrap();
        for cut in 0..=COPY_LEN {
            let mut storage = memory.clone();
            storage.cut = Some(cut);
            let mut store = StateStore::new(storage);
            let written = store.write(&newer);
            let read = store.read().unwrap();
            let landed = store.into_inner().copies[target] == whole;
            if cut == COPY_LEN {
                prop_assert_eq!(written, Ok(()));
                prop_assert_eq!(&read, &newer);
            } else {
                prop_assert_eq!(written, Err(StoreError::Storage(StorageError)));
                if landed {
                    prop_assert_eq!(&read, &newer, "cut at {} left the copy whole", cut);
                } else {
                    prop_assert_eq!(&read, &old, "cut at {}", cut);
                }
            }
        }
    }
}
