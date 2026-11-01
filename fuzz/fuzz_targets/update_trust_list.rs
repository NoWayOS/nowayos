// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s trust list - any bytes decoded as
//! a trust list, as the update PD decodes a list copied out of what reached
//! it; and lists built from the fuzzer's choices, encoded, then changed in a
//! few places, so that the decoder's later rules are fuzzed from the start.
//!
//! Checks: nothing panics; `TrustList::decode` and `nowayos-ipc`'s
//! `decode_exact` refuse exactly the same inputs, the first's errors mapping
//! to the second's; whatever decodes keeps the list's rules and encodes back
//! to exactly the bytes it came from (`nowayos_fuzz::check_decode`); and a
//! list the constructors accept decodes to itself.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::{decode_exact, encode};
use nowayos_update::trust_list::{MAX_KEYS, MAX_SIGNATURES, RECORD_LEN, SIGNED_LEN};
use nowayos_update::{
    ListedKey, NamedSignature, Purpose, TrustList, TrustListContents, TrustListError,
};

/// A list built from the fuzzer's bytes, if the constructors take it.
fn built(input: &Input<'_>) -> Option<TrustList> {
    let version = input.u32();
    let development = input.byte() % 2 == 1;
    let key_count = usize::from(input.byte()) % (MAX_KEYS + 1);
    let mut keys = [ListedKey {
        purpose: Purpose::TrustList,
        key: [0; 64],
    }; MAX_KEYS];
    for key in keys.iter_mut().take(key_count) {
        key.purpose = Purpose::from_code(input.byte() % 4).unwrap_or(Purpose::TrustList);
        key.key = input.array();
    }
    let contents = TrustListContents::new(version, development, keys.get(..key_count)?);
    let signature_count = usize::from(input.byte()) % (MAX_SIGNATURES + 2);
    let mut signatures = [NamedSignature {
        key_id: [0; 32],
        signature: [0; 64],
    }; MAX_SIGNATURES + 1];
    for signature in signatures.iter_mut().take(signature_count) {
        signature.key_id = input.array();
        signature.signature = input.array();
    }
    let list = TrustList::new(contents.ok()?, signatures.get(..signature_count)?);
    if let Err(error) = list {
        let [first, second, ..] = signatures;
        let expected = match signature_count {
            0 => Some(TrustListError::NoSignature),
            count if count > MAX_SIGNATURES => Some(TrustListError::TooManySignatures { count }),
            _ if first.key_id == second.key_id => Some(TrustListError::SignerNamedTwice),
            _ => None,
        };
        assert_eq!(Some(error), expected, "{signature_count} signatures");
    }
    list.ok()
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 3 {
        1 => frame::<TrustList>(input.rest(), &mut buf),
        2 => {
            let Some(list) = built(&input) else {
                return;
            };
            let len = encode(&list, &mut buf).unwrap_or(0);
            let encoded = buf.get(..len).unwrap_or_default();
            assert_eq!(len, RECORD_LEN, "{list:?} encodes to {len} bytes");
            assert_eq!(TrustList::decode(encoded).as_ref(), Ok(&list));
            assert_eq!(
                Some(list.signed_bytes().as_slice()),
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
    let kept = check_decode::<TrustList>(record);
    let decoded = TrustList::decode(record);
    assert_eq!(
        decoded.clone().map_err(TrustListError::to_decode_error),
        decode_exact::<TrustList>(record),
        "the two decoders disagree on {record:02x?}"
    );
    assert_eq!(decoded.as_ref().ok(), kept.as_ref());
    if let Ok(list) = decoded {
        // What the list says, read from its bytes, keeps the list's rules.
        let count = list.contents().key_count();
        assert!((1..=MAX_KEYS).contains(&count), "{list:?}");
        assert_eq!(list.keys().count(), count, "{list:?}");
        assert!(
            list.keys().any(|key| key.purpose == Purpose::TrustList),
            "{list:?}"
        );
        for (first, earlier) in list.keys().enumerate() {
            assert!(
                list.keys()
                    .skip(first.saturating_add(1))
                    .all(|later| later.key != earlier.key),
                "{list:?}"
            );
        }
        let signed = list.signature_count();
        assert!((1..=MAX_SIGNATURES).contains(&signed), "{list:?}");
        assert_eq!(list.signatures().count(), signed, "{list:?}");
        let mut signers = list.signatures().map(|signature| signature.key_id);
        if let (Some(first), Some(second)) = (signers.next(), signers.next()) {
            assert_ne!(first, second, "{list:?}");
        }
    }
});
