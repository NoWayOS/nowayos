// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s deployment record - any bytes decoded
//! as one, as the update PD decodes an Update Server's answer that the TLS
//! PD hands it; and records built from the fuzzer's choices, encoded, then
//! changed in a few places.
//!
//! Checks: nothing panics; `Deployment::decode` and `nowayos-ipc`'s
//! `decode_exact` refuse exactly the same inputs, the first's errors mapping
//! to the second's; whatever decodes names a product and a board of allowed
//! characters, its signature covers exactly its first `SIGNED_LEN` bytes, a
//! "current" record names no manifest, and it encodes back to exactly the
//! bytes it came from (`nowayos_fuzz::check_decode`).

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::{HEADER_LEN, Message, decode_exact, encode};
use nowayos_update::deployment::SIGNED_LEN;
use nowayos_update::manifest::{is_board_character, is_product_character};
use nowayos_update::{
    Deployment, DeploymentContents, DeploymentError, DeploymentFields, Instruction, NamedSignature,
};

/// A name of up to 33 bytes, mostly of `alphabet`.
fn name<'b>(input: &Input<'_>, alphabet: &[u8], out: &'b mut [u8; 34]) -> Option<&'b str> {
    let len = usize::from(input.byte()) % 34;
    for byte in out.iter_mut().take(len) {
        let choice = input.byte();
        *byte = if choice < 0xF0 {
            usize::from(choice)
                .checked_rem(alphabet.len())
                .and_then(|at| alphabet.get(at))
                .copied()
                .unwrap_or(b'a')
        } else {
            input.byte() % 0x80
        };
    }
    core::str::from_utf8(out.get(..len)?).ok()
}

/// A record built from the fuzzer's bytes, if the constructor takes it.
fn built(input: &Input<'_>) -> Option<Deployment> {
    let mut product = [0u8; 34];
    let product = name(
        input,
        b"abcdefghijklmnopqrstuvwxyz0123456789-",
        &mut product,
    )?;
    let mut board = [0u8; 34];
    let board = name(input, b"abcdefghijklmnopqrstuvwxyz0123456789-_", &mut board)?;
    let instruction = if input.byte() % 2 == 1 {
        Instruction::Current
    } else {
        Instruction::Install(input.array())
    };
    let contents = DeploymentContents::new(&DeploymentFields {
        product,
        board,
        nonce: input.array(),
        cohort: input.u32(),
        instruction,
    })
    .ok()?;
    Some(Deployment::new(
        contents,
        &NamedSignature {
            key_id: input.array(),
            signature: input.array(),
        },
    ))
}

fuzz_target!(|data: &[u8]| {
    const RECORD_LEN: usize = HEADER_LEN + Deployment::PAYLOAD_LEN;
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 3 {
        1 => frame::<Deployment>(input.rest(), &mut buf),
        2 => {
            let Some(deployment) = built(&input) else {
                return;
            };
            let len = encode(&deployment, &mut buf).unwrap_or(0);
            assert_eq!(len, RECORD_LEN, "{deployment:?} encodes to {len} bytes");
            assert_eq!(
                Deployment::decode(buf.get(..len).unwrap_or_default()).as_ref(),
                Ok(&deployment)
            );
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
    let kept = check_decode::<Deployment>(record);
    let decoded = Deployment::decode(record);
    assert_eq!(
        decoded.clone().map_err(DeploymentError::to_decode_error),
        decode_exact::<Deployment>(record),
        "the two decoders disagree on {record:02x?}"
    );
    assert_eq!(decoded.as_ref().ok(), kept.as_ref());
    if let Ok(deployment) = decoded {
        assert!(
            deployment.product().bytes().all(is_product_character),
            "{deployment:?}"
        );
        assert!(
            deployment.board().bytes().all(is_board_character),
            "{deployment:?}"
        );
        assert_eq!(deployment.signed_bytes().get(..), record.get(..SIGNED_LEN));
        if deployment.instruction() == Instruction::Current {
            assert!(
                record
                    .get(HEADER_LEN + 104..HEADER_LEN + 136)
                    .is_some_and(|hash| hash.iter().all(|byte| *byte == 0)),
                "{deployment:?}"
            );
        }
    }
});
