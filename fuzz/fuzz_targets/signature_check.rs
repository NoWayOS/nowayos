// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-signature`'s P-256 check - any public key,
//! signature and message, as the update PD is handed them; and OpenSSL's
//! known answer with bytes the fuzzer changes.
//!
//! Checks: nothing panics; a key or a signature is refused exactly when the
//! rules say, with the rule's error, the key's before the signature's; and
//! the known answer, with any byte of its message, signature or key
//! changed, is refused.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_signature::{
    Error, PUBLIC_KEY_LEN, PublicKey, SIGNATURE_LEN, Signature, check, known_answer,
};

/// P-256's group order, n, big-endian.
const ORDER: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xBC, 0xE6, 0xFA, 0xAD, 0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63, 0x25, 0x51,
];

/// The error the rules give a key's bytes, if they are not a key's form;
/// `None` for 65 bytes starting 0x04, whose point the curve decides.
fn key_form_error(key: &[u8]) -> Option<Error> {
    match key {
        [0] => Some(Error::KeyAtInfinity),
        _ if key.len() != PUBLIC_KEY_LEN => Some(Error::KeyLength { found: key.len() }),
        [first, ..] if *first != 4 => Some(Error::KeyPrefix { found: *first }),
        _ => None,
    }
}

/// The error the rules give a signature's bytes, if any.
fn signature_error(signature: &[u8]) -> Option<Error> {
    let Some((r, s)) = signature
        .split_first_chunk::<32>()
        .filter(|_| signature.len() == SIGNATURE_LEN)
    else {
        return Some(Error::SignatureLength {
            found: signature.len(),
        });
    };
    let scalar = |value: &[u8], zero, too_large| {
        if value.iter().all(|byte| *byte == 0) {
            Some(zero)
        } else if value >= ORDER.as_slice() {
            Some(too_large)
        } else {
            None
        }
    };
    scalar(r, Error::RIsZero, Error::RNotBelowOrder)
        .or_else(|| scalar(s, Error::SIsZero, Error::SNotBelowOrder))
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    if input.byte() % 2 == 1 {
        // Any key, signature and message (an odd first byte).
        let key: Vec<u8> = (0..usize::from(input.byte()) % 80)
            .map(|_| input.byte())
            .collect();
        let signature: Vec<u8> = (0..usize::from(input.byte()) % 80)
            .map(|_| input.byte())
            .collect();
        let message = input.rest();
        let parsed_key = PublicKey::from_sec1(&key);
        match key_form_error(&key) {
            Some(error) => assert_eq!(parsed_key.as_ref().err(), Some(&error)),
            None => assert!(
                matches!(parsed_key, Ok(_) | Err(Error::KeyNotOnCurve)),
                "{parsed_key:?}"
            ),
        }
        let parsed_signature = Signature::from_bytes(&signature);
        assert_eq!(
            parsed_signature.as_ref().err(),
            signature_error(&signature).as_ref()
        );
        let checked = check(&key, message, &signature);
        match (parsed_key, parsed_signature) {
            (Err(error), _) | (Ok(_), Err(error)) => assert_eq!(checked, Err(error)),
            (Ok(_), Ok(_)) => assert!(matches!(checked, Ok(()) | Err(Error::Mismatch))),
        }
    } else {
        // The known answer, with up to three bytes changed (an even one).
        let mut key = known_answer::PUBLIC_KEY;
        let mut signature = known_answer::SIGNATURE;
        let mut message = known_answer::MESSAGE.to_vec();
        let mut changed = false;
        for _ in 0..=input.byte() % 3 {
            let (part, at, change) = (input.byte() % 3, usize::from(input.u16()), input.byte());
            let target = match part {
                0 => message.as_mut_slice(),
                1 => signature.as_mut_slice(),
                _ => key.as_mut_slice(),
            };
            let len = target.len();
            if let Some(byte) = at.checked_rem(len).and_then(|at| target.get_mut(at)) {
                *byte ^= change;
                changed |= change != 0;
            }
        }
        let checked = check(&key, &message, &signature);
        let unchanged = key == known_answer::PUBLIC_KEY
            && signature == known_answer::SIGNATURE
            && message == known_answer::MESSAGE;
        if unchanged {
            assert_eq!(checked, Ok(()), "the known answer is refused");
        } else {
            assert!(
                changed && checked.is_err(),
                "a changed known answer is accepted"
            );
        }
    }
});
