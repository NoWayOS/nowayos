// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s bundle chunks - any bytes decoded
//! as a chunk, as the update PD decodes one copied out of its ring slot; and
//! chunks built from the fuzzer's choices - offsets near the top of the
//! range among them - encoded, then changed in a few places.
//!
//! Checks: nothing panics; `Chunk::decode` and `nowayos-ipc`'s
//! `decode_exact` refuse exactly the same inputs, the first's errors mapping
//! to the second's; whatever decodes carries 1 to `CHUNK_LEN` bytes, ends at
//! its offset plus their number without overflow, and encodes back to
//! exactly the bytes it came from (`nowayos_fuzz::check_decode`); and a
//! chunk the constructor accepts decodes to itself, while one it refuses is
//! refused for the first rule it breaks.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::{decode_exact, encode};
use nowayos_update::chunk::{CHUNK_LEN, RECORD_LEN};
use nowayos_update::{Chunk, ChunkError, Part};

/// A chunk built from the fuzzer's bytes, if the constructor takes it.
fn built(input: &Input<'_>) -> Option<Chunk> {
    let part = Part::from_code((input.byte() % 3).saturating_add(1))?;
    let len = usize::from(input.u16()) % (CHUNK_LEN + 2);
    // Any offset, or one within a few chunks of the top.
    let offset = if input.byte() % 2 == 1 {
        u64::MAX.saturating_sub(u64::from(input.u16()))
    } else {
        u64::from_le_bytes(input.array())
    };
    let mut bytes = [0u8; CHUNK_LEN + 1];
    for byte in bytes.iter_mut().take(len) {
        *byte = input.byte();
    }
    let chunk = Chunk::new(part, offset, bytes.get(..len)?);
    let expected = match len {
        0 => Some(ChunkError::Empty),
        len if len > CHUNK_LEN => Some(ChunkError::TooLong { len }),
        len if offset.checked_add(u64::try_from(len).ok()?).is_none() => {
            Some(ChunkError::EndOverflows { offset, len })
        }
        _ => None,
    };
    assert_eq!(
        chunk.as_ref().err().copied(),
        expected,
        "{len} bytes at {offset}"
    );
    chunk.ok()
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 3 {
        1 => frame::<Chunk>(input.rest(), &mut buf),
        2 => {
            let Some(chunk) = built(&input) else {
                return;
            };
            let len = encode(&chunk, &mut buf).unwrap_or(0);
            let encoded = buf.get(..len).unwrap_or_default();
            assert_eq!(len, RECORD_LEN, "{chunk:?} encodes to {len} bytes");
            assert_eq!(Chunk::decode(encoded).as_ref(), Ok(&chunk));
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
    let kept = check_decode::<Chunk>(record);
    let decoded = Chunk::decode(record);
    assert_eq!(
        decoded.clone().map_err(ChunkError::to_decode_error),
        decode_exact::<Chunk>(record),
        "the two decoders disagree on {record:02x?}"
    );
    assert_eq!(decoded.as_ref().ok(), kept.as_ref());
    if let Ok(chunk) = decoded {
        let len = chunk.bytes().len();
        assert!((1..=CHUNK_LEN).contains(&len), "{chunk:?}");
        let count = u64::try_from(len).unwrap_or(u64::MAX);
        assert_eq!(
            chunk.offset().checked_add(count),
            Some(chunk.end()),
            "{chunk:?}"
        );
        let part = Part::from_code(chunk.part().code());
        assert_eq!(part, Some(chunk.part()), "{chunk:?}");
    }
});
