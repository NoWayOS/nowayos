// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the security PD's signing records and its one accepted input
//! (`nowayos-signing`) - any bytes decoded as each
//! record, any payload behind each record's own header, and any bytes given
//! to the recogniser.
//!
//! Checks: nothing panics; each decoder agrees with itself and whatever
//! decodes re-encodes to the bytes it came from (`nowayos_fuzz::check_decode`);
//! a request holds no more than the longest input; a signed answer has
//! neither half zero; and the recogniser accepts only a TLS 1.3 client
//! `CertificateVerify` input, returning its transcript hash.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, frame};
use nowayos_signing::{
    CLIENT_CONTEXT, INPUT_MAX, KeyCode, PADDING_LEN, PREFIX_LEN, SignAnswer, SignRequest, recognise,
};

/// An input the recogniser accepted has the client's form.
fn check_recognised(input: &[u8]) {
    if let Ok(hash) = recognise(input) {
        assert!(input.len() == PREFIX_LEN + 32 || input.len() == PREFIX_LEN + 48);
        assert!(
            input
                .get(..PADDING_LEN)
                .is_some_and(|padding| padding.iter().all(|byte| *byte == b' '))
        );
        assert_eq!(
            input.get(PADDING_LEN..PREFIX_LEN.saturating_sub(1)),
            Some(&CLIENT_CONTEXT[..])
        );
        assert_eq!(input.get(PREFIX_LEN.saturating_sub(1)), Some(&0));
        assert_eq!(input.get(PREFIX_LEN..), Some(hash.as_bytes()));
    }
}

fn check_answer(answer: &SignAnswer) {
    if let SignAnswer::Signed(signature) = answer {
        let (r, s) = signature.split_at(32);
        assert!(
            r.iter().any(|byte| *byte != 0) && s.iter().any(|byte| *byte != 0),
            "a zero half"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    match input.byte() % 5 {
        // Any bytes, as each record.
        0 => {
            let bytes = input.rest();
            if let Some(request) = check_decode::<SignRequest>(bytes) {
                assert!(request.input().len() <= INPUT_MAX);
                check_recognised(request.input());
            }
            if let Some(answer) = check_decode::<SignAnswer>(bytes) {
                check_answer(&answer);
            }
            let _ = check_decode::<KeyCode>(bytes);
        }
        // Any payload behind each record's header.
        1 => {
            if let Some(request) =
                check_decode::<SignRequest>(frame::<SignRequest>(input.rest(), &mut buf))
            {
                assert!(request.input().len() <= INPUT_MAX);
                check_recognised(request.input());
            }
        }
        2 => {
            if let Some(answer) =
                check_decode::<SignAnswer>(frame::<SignAnswer>(input.rest(), &mut buf))
            {
                check_answer(&answer);
            }
        }
        3 => {
            if let Some(code) = check_decode::<KeyCode>(frame::<KeyCode>(input.rest(), &mut buf)) {
                let _ = code.groups();
            }
        }
        // Any bytes to the recogniser, and the same behind a client prefix.
        _ => {
            let bytes = input.rest();
            check_recognised(bytes);
            let mut prefixed = [0u8; FRAME_MAX];
            let mut len = 0usize;
            for chunk in [
                &[b' '; PADDING_LEN][..],
                &CLIENT_CONTEXT[..],
                &[0u8][..],
                bytes,
            ] {
                let end = len.saturating_add(chunk.len()).min(FRAME_MAX);
                let Some(target) = prefixed.get_mut(len..end) else {
                    break;
                };
                target.copy_from_slice(chunk.get(..end.saturating_sub(len)).unwrap_or_default());
                len = end;
            }
            check_recognised(prefixed.get(..len).unwrap_or_default());
        }
    }
});
