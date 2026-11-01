// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the update manifest: every byte string either
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
use nowayos_update::manifest::{
    MAX_PRODUCT_LEN, MAX_REFERENCE_VALUES, RECORD_LEN, SIGNED_LEN, is_product_character,
};
use nowayos_update::{
    DIGEST_LEN, KEY_ID_LEN, Manifest, ManifestContents, ManifestError, ManifestFields,
    NamedSignature, SIGNATURE_LEN, Version,
};
use proptest::prelude::*;

/// Where the payload's fields start, in the whole record.
const NAME_LEN_AT: usize = HEADER_LEN;
const DEVELOPMENT_AT: usize = HEADER_LEN + 1;
const PRODUCT_AT: usize = HEADER_LEN + 8;
const IMAGE_LEN_AT: usize = HEADER_LEN + 48;
const REFERENCE_COUNT_AT: usize = HEADER_LEN + 88;
const REFERENCES_AT: usize = HEADER_LEN + 96;

/// The characters a product name may have.
const PRODUCT_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";

fn product() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(PRODUCT_CHARACTERS),
        1..=MAX_PRODUCT_LEN,
    )
    .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

/// A manifest that keeps every rule.
fn manifest() -> impl Strategy<Value = Manifest> {
    (
        product(),
        any::<(u16, u16, u16)>(),
        any::<(u32, u32, bool)>(),
        1u64..,
        any::<[u8; DIGEST_LEN]>(),
        prop::collection::vec(any::<[u8; DIGEST_LEN]>(), 0..=MAX_REFERENCE_VALUES),
        any::<([u8; KEY_ID_LEN], [u8; 32], [u8; 32])>(),
    )
        .prop_map(
            |(
                product,
                (major, minor, patch),
                (security_version, trust_list_version, development),
                image_len,
                image_sha256,
                reference_values,
                (key_id, r, s),
            )| {
                let contents = ManifestContents::new(&ManifestFields {
                    product: &product,
                    version: Version {
                        major,
                        minor,
                        patch,
                    },
                    security_version,
                    trust_list_version,
                    development,
                    image_len,
                    image_sha256,
                    reference_values: &reference_values,
                })
                .unwrap();
                let mut signature = [0u8; SIGNATURE_LEN];
                signature[..32].copy_from_slice(&r);
                signature[32..].copy_from_slice(&s);
                Manifest::new(contents, &NamedSignature { key_id, signature })
            },
        )
}

fn encoded(manifest: &Manifest) -> Vec<u8> {
    let mut out = vec![0u8; RECORD_LEN];
    assert_eq!(encode(manifest, &mut out), Ok(RECORD_LEN));
    out
}

/// Decodes `bytes` with both decoders and checks what every input must
/// satisfy: they agree, and a manifest that decodes encodes back to exactly
/// `bytes` and keeps the rules.
fn decode_both(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let decoded = Manifest::decode(bytes);
    assert_eq!(
        decoded.clone().map_err(ManifestError::to_decode_error),
        decode_exact::<Manifest>(bytes),
        "the two decoders disagree"
    );
    if let Ok(manifest) = &decoded {
        assert_eq!(
            encoded(manifest),
            bytes,
            "a decoded manifest encodes differently"
        );
        assert_eq!(manifest.signed_bytes()[..], bytes[..SIGNED_LEN]);
        let product = manifest.product().as_bytes();
        assert_eq!(product, manifest.contents().product_bytes());
        assert!((1..=MAX_PRODUCT_LEN).contains(&product.len()));
        assert!(product.iter().all(|byte| is_product_character(*byte)));
        assert_ne!(manifest.image_len(), 0);
        let values: Vec<[u8; DIGEST_LEN]> = manifest.reference_values().collect();
        assert_eq!(values.len(), manifest.contents().reference_value_count());
        assert!(values.len() <= MAX_REFERENCE_VALUES);
        // Fields read back from the bytes are where the layout puts them.
        assert_eq!(
            manifest.image_len().to_le_bytes()[..],
            bytes[IMAGE_LEN_AT..IMAGE_LEN_AT + 8]
        );
        assert_eq!(
            manifest.signature().key_id[..],
            bytes[SIGNED_LEN..SIGNED_LEN + KEY_ID_LEN]
        );
    }
    decoded
}

/// Every byte of `bytes` that must be zero: the reserved bytes, the name's
/// bytes past its length and the reference values past their number.
fn reserved_offsets(manifest: &Manifest) -> Vec<usize> {
    let name_len = manifest.product().len();
    let mut offsets: Vec<usize> = (PRODUCT_AT + name_len..PRODUCT_AT + MAX_PRODUCT_LEN).collect();
    offsets.extend(REFERENCE_COUNT_AT + 1..REFERENCES_AT);
    let count = manifest.contents().reference_value_count();
    offsets.extend(
        REFERENCES_AT + count * DIGEST_LEN..REFERENCES_AT + MAX_REFERENCE_VALUES * DIGEST_LEN,
    );
    offsets
}

proptest! {
    #[test]
    fn manifests_that_keep_the_rules_round_trip(manifest in manifest()) {
        let bytes = encoded(&manifest);
        prop_assert_eq!(decode_both(&bytes), Ok(manifest));
    }

    #[test]
    fn any_bytes_decode_or_are_refused(bytes in prop::collection::vec(any::<u8>(), 0..2 * RECORD_LEN)) {
        let _ = decode_both(&bytes);
    }

    #[test]
    fn any_payload_behind_the_header_decodes_or_is_refused(
        manifest in manifest(),
        payload in prop::collection::vec(any::<u8>(), RECORD_LEN - HEADER_LEN),
    ) {
        let mut bytes = encoded(&manifest);
        bytes[HEADER_LEN..].copy_from_slice(&payload);
        let _ = decode_both(&bytes);
    }

    #[test]
    fn changed_manifests_decode_or_are_refused(
        manifest in manifest(),
        changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        let mut bytes = encoded(&manifest);
        for (at, value) in changes {
            let at = at.index(bytes.len());
            bytes[at] = value;
        }
        let _ = decode_both(&bytes);
    }

    #[test]
    fn a_product_name_of_no_length_or_too_long_is_refused(manifest in manifest(), len in 33u8..) {
        let mut bytes = encoded(&manifest);
        bytes[NAME_LEN_AT] = 0;
        prop_assert_eq!(decode_both(&bytes), Err(ManifestError::ProductNameLength { len: 0 }));
        bytes[NAME_LEN_AT] = len;
        prop_assert_eq!(
            decode_both(&bytes),
            Err(ManifestError::ProductNameLength { len: usize::from(len) })
        );
    }

    #[test]
    fn a_bad_development_mark_is_refused(manifest in manifest(), found in 2u8..) {
        let mut bytes = encoded(&manifest);
        bytes[DEVELOPMENT_AT] = found;
        prop_assert_eq!(decode_both(&bytes), Err(ManifestError::DevelopmentMark { found }));
    }

    #[test]
    fn a_product_name_outside_its_characters_is_refused(
        manifest in manifest(),
        at in any::<prop::sample::Index>(),
        byte in any::<u8>().prop_filter("not allowed", |byte| !is_product_character(*byte)),
    ) {
        let mut bytes = encoded(&manifest);
        let at = at.index(manifest.product().len());
        bytes[PRODUCT_AT + at] = byte;
        prop_assert_eq!(decode_both(&bytes), Err(ManifestError::ProductNameCharacter { at, byte }));
    }

    #[test]
    fn a_reserved_byte_that_is_not_zero_is_refused(
        manifest in manifest(),
        at in any::<prop::sample::Index>(),
        value in 1u8..,
    ) {
        let mut bytes = encoded(&manifest);
        let offsets = reserved_offsets(&manifest);
        prop_assume!(!offsets.is_empty());
        bytes[offsets[at.index(offsets.len())]] = value;
        prop_assert_eq!(decode_both(&bytes), Err(ManifestError::ReservedNotZero));
    }

    #[test]
    fn an_empty_image_is_refused(manifest in manifest()) {
        let mut bytes = encoded(&manifest);
        bytes[IMAGE_LEN_AT..IMAGE_LEN_AT + 8].copy_from_slice(&0u64.to_le_bytes());
        prop_assert_eq!(decode_both(&bytes), Err(ManifestError::EmptyImage));
    }

    #[test]
    fn more_reference_values_than_the_limit_are_refused(manifest in manifest(), count in 33u8..) {
        let mut bytes = encoded(&manifest);
        bytes[REFERENCE_COUNT_AT] = count;
        prop_assert_eq!(
            decode_both(&bytes),
            Err(ManifestError::TooManyReferenceValues { count: usize::from(count) })
        );
    }

    #[test]
    fn a_bad_frame_is_refused(
        manifest in manifest(),
        kind in prop_oneof![0u16..2, 3u16..],
        version in 2u16..,
        length in any::<u16>(),
        cut in 0..RECORD_LEN,
    ) {
        let bytes = encoded(&manifest);
        let mut bad = bytes.clone();
        bad[..2].copy_from_slice(&kind.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ManifestError::Frame(DecodeError::WrongKind { expected: 2, found: kind }))
        );
        let mut bad = bytes.clone();
        bad[2..4].copy_from_slice(&version.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ManifestError::Frame(DecodeError::UnsupportedVersion { expected: 1, found: version }))
        );
        prop_assume!(usize::from(length) != RECORD_LEN - HEADER_LEN);
        let mut bad = bytes.clone();
        bad[4..6].copy_from_slice(&length.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ManifestError::Frame(DecodeError::WrongLength {
                expected: RECORD_LEN - HEADER_LEN,
                found: usize::from(length),
            }))
        );
        let mut bad = bytes.clone();
        bad[7] = 1;
        prop_assert_eq!(decode_both(&bad), Err(ManifestError::Frame(DecodeError::ReservedNotZero)));
        let truncated = matches!(
            decode_both(&bytes[..cut]),
            Err(ManifestError::Frame(DecodeError::Truncated { .. }))
        );
        prop_assert!(truncated, "a record cut to {} bytes", cut);
        let mut long = bytes;
        long.push(0);
        prop_assert_eq!(
            decode_both(&long),
            Err(ManifestError::Frame(DecodeError::TrailingBytes { unread: 1 }))
        );
    }
}
