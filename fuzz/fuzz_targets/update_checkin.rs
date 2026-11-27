// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s check-in record - any bytes decoded as
//! one, as an Update Server decodes a check-in from a device it does not
//! trust; and check-ins built from the fuzzer's choices, encoded, then
//! changed in a few places.
//!
//! Checks: nothing panics; `CheckIn::decode` and `nowayos-ipc`'s
//! `decode_exact` refuse exactly the same inputs, the first's errors mapping
//! to the second's; whatever decodes names a product and a board of allowed
//! characters and encodes back to exactly the bytes it came from
//! (`nowayos_fuzz::check_decode`).

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::{HEADER_LEN, Message, decode_exact, encode};
use nowayos_update::manifest::{is_board_character, is_product_character};
use nowayos_update::{CheckIn, CheckInError, CheckInFields, Version};

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

/// A check-in built from the fuzzer's bytes, if the constructor takes it.
fn built(input: &Input<'_>) -> Option<CheckIn> {
    let mut product = [0u8; 34];
    let product = name(
        input,
        b"abcdefghijklmnopqrstuvwxyz0123456789-",
        &mut product,
    )?;
    let mut board = [0u8; 34];
    let board = name(input, b"abcdefghijklmnopqrstuvwxyz0123456789-_", &mut board)?;
    CheckIn::new(&CheckInFields {
        product,
        board,
        version: Version {
            major: input.u16(),
            minor: input.u16(),
            patch: input.u16(),
        },
        security_version: input.u32(),
        trust_list_version: input.u32(),
        last_result: input.u16(),
        running_manifest: input.array(),
        nonce: input.array(),
        device: input.array(),
    })
    .ok()
}

fuzz_target!(|data: &[u8]| {
    const RECORD_LEN: usize = HEADER_LEN + CheckIn::PAYLOAD_LEN;
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 3 {
        1 => frame::<CheckIn>(input.rest(), &mut buf),
        2 => {
            let Some(checkin) = built(&input) else {
                return;
            };
            let len = encode(&checkin, &mut buf).unwrap_or(0);
            assert_eq!(len, RECORD_LEN, "{checkin:?} encodes to {len} bytes");
            assert_eq!(
                CheckIn::decode(buf.get(..len).unwrap_or_default()).as_ref(),
                Ok(&checkin)
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
    let kept = check_decode::<CheckIn>(record);
    let decoded = CheckIn::decode(record);
    assert_eq!(
        decoded.clone().map_err(CheckInError::to_decode_error),
        decode_exact::<CheckIn>(record),
        "the two decoders disagree on {record:02x?}"
    );
    assert_eq!(decoded.as_ref().ok(), kept.as_ref());
    if let Ok(checkin) = decoded {
        assert!(!checkin.product().is_empty(), "{checkin:?}");
        assert!(
            checkin.product().bytes().all(is_product_character),
            "{checkin:?}"
        );
        assert!(!checkin.board().is_empty(), "{checkin:?}");
        assert!(
            checkin.board().bytes().all(is_board_character),
            "{checkin:?}"
        );
    }
});
