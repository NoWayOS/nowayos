// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s update manifest - any bytes
//! decoded as a manifest, as the update PD decodes one copied out of the
//! chunks that reached it; and manifests built from the fuzzer's choices,
//! encoded, then changed in a few places, so that the decoder's later rules
//! are fuzzed from the start.
//!
//! Checks: nothing panics; `Manifest::decode` and `nowayos-ipc`'s
//! `decode_exact` refuse exactly the same inputs, the first's errors mapping
//! to the second's; whatever decodes keeps the manifest's rules and encodes
//! back to exactly the bytes it came from (`nowayos_fuzz::check_decode`);
//! and a manifest the constructor accepts decodes to itself, while one it
//! refuses is refused for the first rule it breaks.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::{decode_exact, encode};
use nowayos_update::manifest::{
    MAX_PRODUCT_LEN, MAX_REFERENCE_VALUES, RECORD_LEN, SIGNED_LEN, is_product_character,
};
use nowayos_update::{
    Manifest, ManifestContents, ManifestError, ManifestFields, NamedSignature, Version,
};

/// What the constructor must refuse `fields` with, if anything: the first
/// rule they break, in the order decoding checks them.
fn expected_refusal(fields: &ManifestFields<'_>) -> Option<ManifestError> {
    let name = fields.product.as_bytes();
    if name.is_empty() || name.len() > MAX_PRODUCT_LEN {
        return Some(ManifestError::ProductNameLength { len: name.len() });
    }
    if fields.reference_values.len() > MAX_REFERENCE_VALUES {
        return Some(ManifestError::TooManyReferenceValues {
            count: fields.reference_values.len(),
        });
    }
    if let Some((at, byte)) = name
        .iter()
        .enumerate()
        .find(|(_, byte)| !is_product_character(**byte))
    {
        return Some(ManifestError::ProductNameCharacter { at, byte: *byte });
    }
    (fields.image_len == 0).then_some(ManifestError::EmptyImage)
}

/// A manifest built from the fuzzer's bytes, if the constructor takes it.
fn built(input: &Input<'_>) -> Option<Manifest> {
    let name_len = usize::from(input.byte()) % (MAX_PRODUCT_LEN + 2);
    let mut name = [0u8; MAX_PRODUCT_LEN + 1];
    for byte in name.iter_mut().take(name_len) {
        // Mostly allowed characters, sometimes any byte below 0x80.
        let choice = input.byte();
        *byte = if choice < 0xF0 {
            b"abcdefghijklmnopqrstuvwxyz0123456789-"
                .get(usize::from(choice) % 37)
                .copied()
                .unwrap_or(b'a')
        } else {
            input.byte() % 0x80
        };
    }
    let product = core::str::from_utf8(name.get(..name_len)?).ok()?;
    let count = usize::from(input.byte()) % (MAX_REFERENCE_VALUES + 2);
    let mut values = [[0u8; 32]; MAX_REFERENCE_VALUES + 1];
    for value in values.iter_mut().take(count) {
        *value = input.array();
    }
    let fields = ManifestFields {
        product,
        version: Version {
            major: input.u16(),
            minor: input.u16(),
            patch: input.u16(),
        },
        security_version: input.u32(),
        trust_list_version: input.u32(),
        development: input.byte() % 2 == 1,
        image_len: u64::from_le_bytes(input.array())
            .checked_shr(u32::from(input.byte() % 64))
            .unwrap_or(0),
        image_sha256: input.array(),
        reference_values: values.get(..count)?,
    };
    let contents = ManifestContents::new(&fields);
    assert_eq!(
        contents.as_ref().err().copied(),
        expected_refusal(&fields),
        "{fields:?}"
    );
    let signature = NamedSignature {
        key_id: input.array(),
        signature: input.array(),
    };
    Some(Manifest::new(contents.ok()?, &signature))
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 3 {
        1 => frame::<Manifest>(input.rest(), &mut buf),
        2 => {
            let Some(manifest) = built(&input) else {
                return;
            };
            let len = encode(&manifest, &mut buf).unwrap_or(0);
            let encoded = buf.get(..len).unwrap_or_default();
            assert_eq!(len, RECORD_LEN, "{manifest:?} encodes to {len} bytes");
            assert_eq!(Manifest::decode(encoded).as_ref(), Ok(&manifest));
            assert_eq!(
                Some(manifest.signed_bytes().as_slice()),
                encoded.get(..SIGNED_LEN)
            );
            // Then a few bytes changed, each at an offset and to a value the
            // fuzzer chooses.
            for _ in 0..input.byte() % 4 {
                let at = usize::from(input.u16()) % RECORD_LEN;
                if let Some(byte) = buf.get_mut(at) {
                    *byte = input.byte();
                }
            }
            buf.get(..RECORD_LEN).unwrap_or_default()
        }
        _ => input.rest(),
    };
    check_header(record);
    let kept = check_decode::<Manifest>(record);
    let decoded = Manifest::decode(record);
    assert_eq!(
        decoded.clone().map_err(ManifestError::to_decode_error),
        decode_exact::<Manifest>(record),
        "the two decoders disagree on {record:02x?}"
    );
    assert_eq!(decoded.as_ref().ok(), kept.as_ref());
    if let Ok(manifest) = decoded {
        // What the manifest says, read from its bytes, keeps its rules.
        let product = manifest.product().as_bytes();
        assert_eq!(product, manifest.contents().product_bytes(), "{manifest:?}");
        assert!(
            (1..=MAX_PRODUCT_LEN).contains(&product.len()),
            "{manifest:?}"
        );
        assert!(
            product.iter().all(|byte| is_product_character(*byte)),
            "{manifest:?}"
        );
        assert_ne!(manifest.image_len(), 0, "{manifest:?}");
        let count = manifest.contents().reference_value_count();
        assert!(count <= MAX_REFERENCE_VALUES, "{manifest:?}");
        assert_eq!(manifest.reference_values().count(), count, "{manifest:?}");
    }
});
