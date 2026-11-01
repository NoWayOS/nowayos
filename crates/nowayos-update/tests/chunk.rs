// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the chunks a bundle arrives in: every byte
//! string either decodes and encodes back to the same bytes, or is refused
//! with a typed error - each rule with its own - and nothing panics; the
//! crate's decoder and `nowayos-ipc`'s agree on every input; and any part
//! split into chunks is put back together from them.

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
use nowayos_update::chunk::{CHUNK_LEN, RECORD_LEN, chunks};
use nowayos_update::{Chunk, ChunkError, Part};
use proptest::prelude::*;

/// Where the payload's fields start, in the whole record.
const PART_AT: usize = HEADER_LEN;
const LEN_AT: usize = HEADER_LEN + 2;
const OFFSET_AT: usize = HEADER_LEN + 8;
const BYTES_AT: usize = HEADER_LEN + 16;

fn part() -> impl Strategy<Value = Part> {
    prop_oneof![
        Just(Part::TrustList),
        Just(Part::Manifest),
        Just(Part::Image)
    ]
}

/// A chunk that keeps every rule: 1 to `CHUNK_LEN` bytes, at an offset
/// anywhere its end still fits in 64 bits - often near the top.
fn chunk() -> impl Strategy<Value = Chunk> {
    (
        part(),
        prop::collection::vec(any::<u8>(), 1..=CHUNK_LEN),
        prop_oneof![
            any::<u64>(),
            (0u64..2 * CHUNK_LEN as u64).prop_map(|back| u64::MAX - back)
        ],
    )
        .prop_map(|(part, bytes, offset)| {
            let offset = offset.min(u64::MAX - bytes.len() as u64);
            Chunk::new(part, offset, &bytes).unwrap()
        })
}

fn encoded(chunk: &Chunk) -> Vec<u8> {
    let mut out = vec![0u8; RECORD_LEN];
    assert_eq!(encode(chunk, &mut out), Ok(RECORD_LEN));
    out
}

/// Decodes `bytes` with both decoders and checks what every input must
/// satisfy: they agree, and a chunk that decodes encodes back to exactly
/// `bytes` and keeps the rules.
fn decode_both(bytes: &[u8]) -> Result<Chunk, ChunkError> {
    let decoded = Chunk::decode(bytes);
    assert_eq!(
        decoded.clone().map_err(ChunkError::to_decode_error),
        decode_exact::<Chunk>(bytes),
        "the two decoders disagree"
    );
    if let Ok(chunk) = &decoded {
        assert_eq!(encoded(chunk), bytes, "a decoded chunk encodes differently");
        let len = chunk.bytes().len();
        assert!((1..=CHUNK_LEN).contains(&len));
        assert_eq!(chunk.offset().checked_add(len as u64), Some(chunk.end()));
        assert_eq!(chunk.bytes(), &bytes[BYTES_AT..BYTES_AT + len]);
        assert_eq!(chunk.part().code(), bytes[PART_AT]);
    }
    decoded
}

proptest! {
    #[test]
    fn chunks_that_keep_the_rules_round_trip(chunk in chunk()) {
        let bytes = encoded(&chunk);
        prop_assert_eq!(decode_both(&bytes), Ok(chunk));
    }

    #[test]
    fn any_bytes_decode_or_are_refused(bytes in prop::collection::vec(any::<u8>(), 0..2 * RECORD_LEN)) {
        let _ = decode_both(&bytes);
    }

    #[test]
    fn any_payload_behind_the_header_decodes_or_is_refused(
        chunk in chunk(),
        payload in prop::collection::vec(any::<u8>(), RECORD_LEN - HEADER_LEN),
    ) {
        let mut bytes = encoded(&chunk);
        bytes[HEADER_LEN..].copy_from_slice(&payload);
        let _ = decode_both(&bytes);
    }

    #[test]
    fn changed_chunks_decode_or_are_refused(
        chunk in chunk(),
        changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        let mut bytes = encoded(&chunk);
        for (at, value) in changes {
            let at = at.index(bytes.len());
            bytes[at] = value;
        }
        let _ = decode_both(&bytes);
    }

    #[test]
    fn an_unknown_part_is_refused(chunk in chunk(), code in prop_oneof![Just(0u8), 4u8..]) {
        let mut bytes = encoded(&chunk);
        bytes[PART_AT] = code;
        prop_assert_eq!(decode_both(&bytes), Err(ChunkError::UnknownPart { code }));
    }

    #[test]
    fn no_bytes_or_more_than_the_chunk_size_are_refused(
        chunk in chunk(),
        len in (CHUNK_LEN as u16 + 1)..,
    ) {
        let mut bytes = encoded(&chunk);
        bytes[LEN_AT..LEN_AT + 2].copy_from_slice(&0u16.to_le_bytes());
        prop_assert_eq!(decode_both(&bytes), Err(ChunkError::Empty));
        bytes[LEN_AT..LEN_AT + 2].copy_from_slice(&len.to_le_bytes());
        prop_assert_eq!(decode_both(&bytes), Err(ChunkError::TooLong { len: usize::from(len) }));
    }

    #[test]
    fn an_offset_whose_end_overflows_is_refused(chunk in chunk(), over in 0u64..CHUNK_LEN as u64) {
        let mut bytes = encoded(&chunk);
        let len = chunk.bytes().len();
        // The highest offsets: from u64::MAX - len + 1, the first that
        // overflows, up.
        let offset = u64::MAX - over.min(len as u64 - 1);
        bytes[OFFSET_AT..OFFSET_AT + 8].copy_from_slice(&offset.to_le_bytes());
        prop_assert_eq!(decode_both(&bytes), Err(ChunkError::EndOverflows { offset, len }));
        // One lower, the end fits exactly.
        let fits = u64::MAX - len as u64;
        bytes[OFFSET_AT..OFFSET_AT + 8].copy_from_slice(&fits.to_le_bytes());
        prop_assert_eq!(decode_both(&bytes).map(|chunk| chunk.end()), Ok(u64::MAX));
    }

    #[test]
    fn a_reserved_byte_that_is_not_zero_is_refused(
        chunk in chunk(),
        at in any::<prop::sample::Index>(),
        value in 1u8..,
    ) {
        let mut bytes = encoded(&chunk);
        let len = chunk.bytes().len();
        let mut offsets = vec![PART_AT + 1, HEADER_LEN + 4, HEADER_LEN + 5, HEADER_LEN + 6, HEADER_LEN + 7];
        offsets.extend(BYTES_AT + len..RECORD_LEN);
        bytes[offsets[at.index(offsets.len())]] = value;
        prop_assert_eq!(decode_both(&bytes), Err(ChunkError::ReservedNotZero));
    }

    #[test]
    fn a_part_is_put_back_together_from_its_chunks(
        part in part(),
        bytes in prop::collection::vec(any::<u8>(), 0..4 * CHUNK_LEN),
    ) {
        let pieces: Vec<Chunk> = chunks(part, &bytes).collect();
        prop_assert_eq!(pieces.len(), bytes.len().div_ceil(CHUNK_LEN));
        let mut joined = Vec::new();
        for piece in &pieces {
            prop_assert_eq!(piece.part(), part);
            prop_assert_eq!(piece.offset(), joined.len() as u64);
            let encoded = encoded(piece);
            joined.extend_from_slice(decode_both(&encoded).unwrap().bytes());
        }
        prop_assert_eq!(joined, bytes);
    }

    #[test]
    fn a_bad_frame_is_refused(
        chunk in chunk(),
        kind in prop_oneof![0u16..3, 4u16..],
        version in 2u16..,
        length in any::<u16>(),
        cut in 0..RECORD_LEN,
    ) {
        let bytes = encoded(&chunk);
        let mut bad = bytes.clone();
        bad[..2].copy_from_slice(&kind.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ChunkError::Frame(DecodeError::WrongKind { expected: 3, found: kind }))
        );
        let mut bad = bytes.clone();
        bad[2..4].copy_from_slice(&version.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ChunkError::Frame(DecodeError::UnsupportedVersion { expected: 1, found: version }))
        );
        prop_assume!(usize::from(length) != RECORD_LEN - HEADER_LEN);
        let mut bad = bytes.clone();
        bad[4..6].copy_from_slice(&length.to_le_bytes());
        prop_assert_eq!(
            decode_both(&bad),
            Err(ChunkError::Frame(DecodeError::WrongLength {
                expected: RECORD_LEN - HEADER_LEN,
                found: usize::from(length),
            }))
        );
        let mut bad = bytes.clone();
        bad[7] = 1;
        prop_assert_eq!(decode_both(&bad), Err(ChunkError::Frame(DecodeError::ReservedNotZero)));
        let truncated = matches!(
            decode_both(&bytes[..cut]),
            Err(ChunkError::Frame(DecodeError::Truncated { .. }))
        );
        prop_assert!(truncated, "a record cut to {} bytes", cut);
        let mut long = bytes;
        long.push(0);
        prop_assert_eq!(
            decode_both(&long),
            Err(ChunkError::Frame(DecodeError::TrailingBytes { unread: 1 }))
        );
    }
}
