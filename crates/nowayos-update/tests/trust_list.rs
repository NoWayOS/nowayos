// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the trust list: every byte string either
//! decodes and encodes back to the same bytes, or is refused with a typed
//! error - each rule with its own - and nothing panics; and the crate's
//! decoder and `nowayos-ipc`'s agree on every input.

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
use nowayos_update::trust_list::{MAX_KEYS, MAX_SIGNATURES, RECORD_LEN, SIGNED_LEN};
use nowayos_update::{
    KEY_ID_LEN, KEY_LEN, ListedKey, NamedSignature, Purpose, SIGNATURE_LEN, TrustList,
    TrustListContents, TrustListError,
};
use proptest::prelude::*;

/// Where the payload's fields start, in the whole record.
const KEY_COUNT_AT: usize = HEADER_LEN + 5;
const DEVELOPMENT_AT: usize = HEADER_LEN + 4;
const KEYS_AT: usize = HEADER_LEN + 8;
const KEY_ENTRY_LEN: usize = 72;
const SIGNATURE_COUNT_AT: usize = HEADER_LEN + 584;
const SIGNATURES_AT: usize = HEADER_LEN + 592;
const SIGNATURE_ENTRY_LEN: usize = 96;

fn purpose() -> impl Strategy<Value = Purpose> {
    prop_oneof![
        Just(Purpose::TrustList),
        Just(Purpose::Image),
        Just(Purpose::Configuration),
    ]
}

/// 64 random bytes, the first replaced by `entry` so that a list's keys
/// differ.
fn key_bytes(entry: usize, x: [u8; 32], y: [u8; 32]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    key[..32].copy_from_slice(&x);
    key[32..].copy_from_slice(&y);
    key[0] = u8::try_from(entry).unwrap();
    key
}

/// Contents that keep every rule: 1 to `MAX_KEYS` different keys, one of
/// them, at a random entry, a trust-list key.
fn contents() -> impl Strategy<Value = TrustListContents> {
    (
        any::<u32>(),
        any::<bool>(),
        prop::collection::vec(
            (purpose(), any::<[u8; 32]>(), any::<[u8; 32]>()),
            1..=MAX_KEYS,
        ),
        any::<prop::sample::Index>(),
    )
        .prop_map(|(version, development, entries, root)| {
            let root = root.index(entries.len());
            let keys: Vec<ListedKey> = entries
                .iter()
                .enumerate()
                .map(|(entry, (purpose, x, y))| ListedKey {
                    purpose: if entry == root {
                        Purpose::TrustList
                    } else {
                        *purpose
                    },
                    key: key_bytes(entry, *x, *y),
                })
                .collect();
            TrustListContents::new(version, development, &keys).unwrap()
        })
}

/// A list that keeps every rule, with 1 or 2 signatures naming different
/// keys.
fn list() -> impl Strategy<Value = TrustList> {
    (
        contents(),
        prop::collection::vec(
            (any::<[u8; 32]>(), any::<[u8; 32]>(), any::<[u8; 32]>()),
            1..=MAX_SIGNATURES,
        ),
    )
        .prop_map(|(contents, entries)| {
            let signatures: Vec<NamedSignature> = entries
                .iter()
                .enumerate()
                .map(|(entry, (id, r, s))| {
                    let mut key_id = [0u8; KEY_ID_LEN];
                    key_id.copy_from_slice(id);
                    key_id[0] = u8::try_from(entry).unwrap();
                    let mut signature = [0u8; SIGNATURE_LEN];
                    signature[..32].copy_from_slice(r);
                    signature[32..].copy_from_slice(s);
                    NamedSignature { key_id, signature }
                })
                .collect();
            TrustList::new(contents, &signatures).unwrap()
        })
}

fn encoded(list: &TrustList) -> Vec<u8> {
    let mut out = vec![0u8; RECORD_LEN];
    assert_eq!(encode(list, &mut out), Ok(RECORD_LEN));
    out
}

/// Decodes `bytes` with both decoders and checks what every input must
/// satisfy: they agree, and a list that decodes encodes back to exactly
/// `bytes`.
fn decode_both(bytes: &[u8]) -> Result<TrustList, TrustListError> {
    let decoded = TrustList::decode(bytes);
    assert_eq!(
        decoded.clone().map_err(TrustListError::to_decode_error),
        decode_exact::<TrustList>(bytes),
        "the two decoders disagree"
    );
    if let Ok(list) = &decoded {
        assert_eq!(encoded(list), bytes, "a decoded list encodes differently");
        assert_eq!(list.signed_bytes()[..], bytes[..SIGNED_LEN]);
        // What it says agrees with its bytes, and keeps the rules.
        let keys: Vec<ListedKey> = list.keys().collect();
        assert_eq!(keys.len(), list.contents().key_count());
        assert!((1..=MAX_KEYS).contains(&keys.len()));
        assert!(keys.iter().any(|key| key.purpose == Purpose::TrustList));
        for (first, earlier) in keys.iter().enumerate() {
            assert!(
                keys[first + 1..]
                    .iter()
                    .all(|later| later.key != earlier.key)
            );
        }
        let signatures: Vec<NamedSignature> = list.signatures().collect();
        assert_eq!(signatures.len(), list.signature_count());
        assert!((1..=MAX_SIGNATURES).contains(&signatures.len()));
        if let [first, second] = signatures.as_slice() {
            assert_ne!(first.key_id, second.key_id);
        }
    }
    decoded
}

/// Every byte of `bytes` that must be zero: the reserved bytes and the
/// entries past each count.
fn reserved_offsets(list: &TrustList) -> Vec<usize> {
    let mut offsets = vec![HEADER_LEN + 6, HEADER_LEN + 7];
    for entry in 0..MAX_KEYS {
        let at = KEYS_AT + entry * KEY_ENTRY_LEN;
        if entry < list.contents().key_count() {
            offsets.extend(at + 1..at + 8);
        } else {
            offsets.extend(at..at + KEY_ENTRY_LEN);
        }
    }
    offsets.extend(SIGNATURE_COUNT_AT + 1..SIGNATURES_AT);
    for entry in list.signature_count()..MAX_SIGNATURES {
        let at = SIGNATURES_AT + entry * SIGNATURE_ENTRY_LEN;
        offsets.extend(at..at + SIGNATURE_ENTRY_LEN);
    }
    offsets
}

proptest! {
    #[test]
    fn lists_that_keep_the_rules_round_trip(list in list()) {
        let bytes = encoded(&list);
        prop_assert_eq!(decode_both(&bytes), Ok(list));
    }

    #[test]
    fn any_bytes_decode_or_are_refused(bytes in prop::collection::vec(any::<u8>(), 0..2 * RECORD_LEN)) {
        let _ = decode_both(&bytes);
    }

    #[test]
    fn any_payload_behind_the_header_decodes_or_is_refused(
        list in list(),
        payload in prop::collection::vec(any::<u8>(), RECORD_LEN - HEADER_LEN),
    ) {
        let mut bytes = encoded(&list);
        bytes[HEADER_LEN..].copy_from_slice(&payload);
        let _ = decode_both(&bytes);
    }

    #[test]
    fn changed_lists_decode_or_are_refused(
        list in list(),
        changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        let mut bytes = encoded(&list);
        for (at, value) in changes {
            let at = at.index(bytes.len());
            bytes[at] = value;
        }
        let _ = decode_both(&bytes);
    }

    #[test]
    fn a_bad_development_mark_is_refused(list in list(), found in 2u8..) {
        let mut bytes = encoded(&list);
        bytes[DEVELOPMENT_AT] = found;
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::DevelopmentMark { found }));
    }

    #[test]
    fn no_key_or_too_many_are_refused(list in list(), count in 9u8..) {
        let mut bytes = encoded(&list);
        bytes[KEY_COUNT_AT] = 0;
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::NoKey));
        bytes[KEY_COUNT_AT] = count;
        prop_assert_eq!(
            decode_both(&bytes),
            Err(TrustListError::TooManyKeys { count: usize::from(count) })
        );
    }

    #[test]
    fn a_reserved_byte_that_is_not_zero_is_refused(
        list in list(),
        at in any::<prop::sample::Index>(),
        value in 1u8..,
    ) {
        let mut bytes = encoded(&list);
        let offsets = reserved_offsets(&list);
        bytes[offsets[at.index(offsets.len())]] = value;
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::ReservedNotZero));
    }

    #[test]
    fn an_unknown_purpose_is_refused(
        list in list(),
        entry in any::<prop::sample::Index>(),
        code in prop_oneof![Just(0u8), 4u8..],
    ) {
        let mut bytes = encoded(&list);
        let entry = entry.index(list.contents().key_count());
        bytes[KEYS_AT + entry * KEY_ENTRY_LEN] = code;
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::UnknownPurpose { entry, code }));
    }

    #[test]
    fn a_list_with_no_trust_list_key_is_refused(
        list in list(),
        purposes in prop::collection::vec(prop_oneof![Just(Purpose::Image), Just(Purpose::Configuration)], MAX_KEYS),
    ) {
        let mut bytes = encoded(&list);
        for entry in 0..list.contents().key_count() {
            bytes[KEYS_AT + entry * KEY_ENTRY_LEN] = purposes[entry].code();
        }
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::NoTrustListKey));
    }

    #[test]
    fn a_key_listed_twice_or_for_two_purposes_is_refused(
        list in list().prop_filter("two keys at least", |list| list.contents().key_count() >= 2),
        pair in any::<(prop::sample::Index, prop::sample::Index)>(),
        same_purpose in any::<bool>(),
    ) {
        let count = list.contents().key_count();
        let keys: Vec<ListedKey> = list.keys().collect();
        let (first, second) = {
            let a = pair.0.index(count);
            let b = pair.1.index(count - 1);
            let b = if b >= a { b + 1 } else { b };
            (a.min(b), a.max(b))
        };
        let mut bytes = encoded(&list);
        let (first_at, second_at) = (KEYS_AT + first * KEY_ENTRY_LEN, KEYS_AT + second * KEY_ENTRY_LEN);
        let key: Vec<u8> = bytes[first_at + 8..first_at + 72].to_vec();
        bytes[second_at + 8..second_at + 72].copy_from_slice(&key);
        let first_purpose = keys[first].purpose;
        let second_purpose = if same_purpose {
            first_purpose
        } else if first_purpose == Purpose::Image {
            Purpose::Configuration
        } else {
            Purpose::Image
        };
        bytes[second_at] = second_purpose.code();
        // The list may now have lost its only trust-list key, which is
        // checked first.
        let roots = keys
            .iter()
            .enumerate()
            .filter(|(entry, key)| {
                let purpose = if *entry == second { second_purpose } else { key.purpose };
                purpose == Purpose::TrustList
            })
            .count();
        let expected = if roots == 0 {
            TrustListError::NoTrustListKey
        } else if same_purpose {
            TrustListError::KeyListedTwice { first, second }
        } else {
            TrustListError::KeyForTwoPurposes { first, second }
        };
        prop_assert_eq!(decode_both(&bytes), Err(expected));
    }

    #[test]
    fn no_signature_or_too_many_are_refused(list in list(), count in 3u8..) {
        let mut bytes = encoded(&list);
        bytes[SIGNATURE_COUNT_AT] = 0;
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::NoSignature));
        bytes[SIGNATURE_COUNT_AT] = count;
        prop_assert_eq!(
            decode_both(&bytes),
            Err(TrustListError::TooManySignatures { count: usize::from(count) })
        );
    }

    #[test]
    fn two_signatures_naming_one_key_are_refused(
        list in list().prop_filter("two signatures", |list| list.signature_count() == 2),
    ) {
        let mut bytes = encoded(&list);
        let id: Vec<u8> = bytes[SIGNATURES_AT..SIGNATURES_AT + KEY_ID_LEN].to_vec();
        let second = SIGNATURES_AT + SIGNATURE_ENTRY_LEN;
        bytes[second..second + KEY_ID_LEN].copy_from_slice(&id);
        prop_assert_eq!(decode_both(&bytes), Err(TrustListError::SignerNamedTwice));
    }

    #[test]
    fn a_bad_frame_is_refused(list in list(), kind in 2u16.., version in 2u16.., length in any::<u16>(), cut in 0..RECORD_LEN) {
        let bytes = encoded(&list);
        let mut bad = bytes.clone();
        bad[..2].copy_from_slice(&kind.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(TrustListError::Frame(DecodeError::WrongKind { expected: 1, found: kind }))
        );
        let mut bad = bytes.clone();
        bad[2..4].copy_from_slice(&version.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(TrustListError::Frame(DecodeError::UnsupportedVersion { expected: 1, found: version }))
        );
        prop_assume!(usize::from(length) != RECORD_LEN - HEADER_LEN);
        let mut bad = bytes.clone();
        bad[4..6].copy_from_slice(&length.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(TrustListError::Frame(DecodeError::WrongLength {
                expected: RECORD_LEN - HEADER_LEN,
                found: usize::from(length),
            }))
        );
        let mut bad = bytes.clone();
        bad[7] = 1;
        prop_assert_eq!(decode_both(&bad), Err(TrustListError::Frame(DecodeError::ReservedNotZero)));
        let truncated = matches!(
            decode_both(&bytes[..cut]),
            Err(TrustListError::Frame(DecodeError::Truncated { .. }))
        );
        prop_assert!(truncated, "a record cut to {} bytes", cut);
        let mut long = bytes;
        long.push(0);
        prop_assert_eq!(
            decode_both(&long),
            Err(TrustListError::Frame(DecodeError::TrailingBytes { unread: 1 }))
        );
    }
}
