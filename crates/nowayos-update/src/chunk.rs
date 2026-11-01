// SPDX-License-Identifier: MIT OR Apache-2.0
//! The chunks a bundle arrives in.
//!
//! The update PD receives a **bundle** - the newest trust list, the
//! manifest and the image - from the TLS PD, which it does not trust, in
//! fixed-size chunks on one channel: each a record that fills one
//! [`RECORD_LEN`]-byte slot of the channel's ring. A chunk names the part of
//! the bundle it carries, where in that part its bytes go, and how many
//! there are. Putting the parts together - and refusing a chunk past a
//! part's end, out of order or over bytes already written - is the update
//! PD's: this module decodes each chunk, so that nothing about one is used
//! before its bytes are copied and checked.
//!
//! # Wire layout
//!
//! A `nowayos-ipc` record of kind [`Chunk::KIND`] (3), layout version 1,
//! whose payload is exactly [`Chunk::PAYLOAD_LEN`] (504) bytes:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 1 | The part: 1 the trust list, 2 the manifest, 3 the image ([`Part`]) |
//! | 1 | 1 | Reserved, zero |
//! | 2 | 2 | The number of bytes the chunk carries, 1 to [`CHUNK_LEN`] |
//! | 4 | 4 | Reserved, zero |
//! | 8 | 8 | The offset of its first byte in the part |
//! | 16 | [`CHUNK_LEN`] | The bytes, then zeros |
//!
//! # What the decoder refuses
//!
//! Besides a malformed frame ([`ChunkError::Frame`]), in the order the
//! fields are read: a part code that names no part; reserved byte 1 not
//! zero; no bytes, or more than [`CHUNK_LEN`]; reserved bytes 4 to 7 not
//! zero; an offset whose sum with the number of bytes does not fit in 64
//! bits; a byte past the chunk's bytes that is not zero.

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer};

use crate::record;

/// The most bytes one chunk carries: so that a chunk's record fills one
/// 512-byte ring slot.
pub const CHUNK_LEN: usize = 488;

/// Where the bytes start in the payload.
const BYTES_AT: usize = 16;

/// The length of the payload: the fields, then the bytes.
const PAYLOAD_LEN: usize = BYTES_AT + CHUNK_LEN;

/// The length of a whole chunk record, header included: one 512-byte slot.
pub const RECORD_LEN: usize = HEADER_LEN + PAYLOAD_LEN;

const _: () = assert!(RECORD_LEN == 512 && CHUNK_LEN <= u16::MAX as usize);

/// The part of a bundle a chunk carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    /// The trust list, as its record's bytes (`crate::TrustList`).
    TrustList,
    /// The manifest, as its record's bytes (`crate::Manifest`).
    Manifest,
    /// The image, whose length and SHA-256 the manifest gives.
    Image,
}

impl Part {
    /// The part's code on the wire.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::TrustList => 1,
            Self::Manifest => 2,
            Self::Image => 3,
        }
    }

    /// The part a code names, or `None` for any other byte.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::TrustList),
            2 => Some(Self::Manifest),
            3 => Some(Self::Image),
            _ => None,
        }
    }
}

/// Why a chunk was refused. Decoding refuses with the first problem found,
/// in the order the module documentation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChunkError {
    /// The record's frame, as `nowayos-ipc` checks it: too short, another
    /// kind, version or payload length, the header's reserved field set, or
    /// bytes after the record.
    Frame(DecodeError),
    /// A reserved byte, or a byte past the chunk's bytes, is not zero.
    ReservedNotZero,
    /// The part code names no part.
    UnknownPart {
        /// The code.
        code: u8,
    },
    /// The chunk carries no bytes.
    Empty,
    /// The chunk says it carries more than [`CHUNK_LEN`] bytes.
    TooLong {
        /// The number of bytes it gives.
        len: usize,
    },
    /// The offset plus the number of bytes does not fit in 64 bits.
    EndOverflows {
        /// The offset.
        offset: u64,
        /// The number of bytes.
        len: usize,
    },
}

impl ChunkError {
    /// The error `nowayos-ipc` reports for the same input: a frame's error
    /// as it is, a reserved byte as [`DecodeError::NonZeroPadding`], and
    /// every other rule as [`DecodeError::InvalidValue`] naming its field.
    #[must_use]
    pub const fn to_decode_error(self) -> DecodeError {
        match self {
            Self::Frame(error) => error,
            Self::ReservedNotZero => DecodeError::NonZeroPadding,
            Self::UnknownPart { .. } => DecodeError::InvalidValue { field: "part" },
            Self::Empty | Self::TooLong { .. } => DecodeError::InvalidValue { field: "len" },
            Self::EndOverflows { .. } => DecodeError::InvalidValue { field: "offset" },
        }
    }
}

impl From<DecodeError> for ChunkError {
    /// A reserved byte that is not zero, as the payload reader reports it,
    /// is [`ChunkError::ReservedNotZero`]; anything else is the frame's.
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NonZeroPadding => Self::ReservedNotZero,
            other => Self::Frame(other),
        }
    }
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "bundle chunk record: {error}"),
            Self::ReservedNotZero => f.write_str("a reserved byte of a bundle chunk is not zero"),
            Self::UnknownPart { code } => write!(f, "a bundle chunk names an unknown part, {code}"),
            Self::Empty => f.write_str("a bundle chunk carries no bytes"),
            Self::TooLong { len } => write!(
                f,
                "a bundle chunk says it carries {len} bytes, more than {CHUNK_LEN}"
            ),
            Self::EndOverflows { offset, len } => write!(
                f,
                "a bundle chunk's offset {offset} plus its {len} bytes overflows"
            ),
        }
    }
}

impl core::error::Error for ChunkError {}

/// Up to [`CHUNK_LEN`] bytes of one part of a bundle, at least one, and
/// where they go. It holds its payload's bytes, checked when it was made -
/// by [`Chunk::new`] or by decoding - and reads what it says from them, so
/// it encodes as exactly the bytes it came from, and its end never
/// overflows.
#[derive(Clone, PartialEq, Eq)]
pub struct Chunk {
    payload: [u8; PAYLOAD_LEN],
}

impl Chunk {
    /// A chunk of `bytes` from `part`, the first at `offset`.
    ///
    /// # Errors
    ///
    /// [`ChunkError::Empty`], [`ChunkError::TooLong`] or
    /// [`ChunkError::EndOverflows`], as decoding would refuse them.
    pub fn new(part: Part, offset: u64, bytes: &[u8]) -> Result<Self, ChunkError> {
        let len = check_len(bytes.len())?;
        check_end(offset, len)?;
        let fields = [part.code(), 0]
            .into_iter()
            .chain(u16::try_from(len).unwrap_or(u16::MAX).to_le_bytes())
            .chain([0; 4])
            .chain(offset.to_le_bytes())
            .chain(bytes.iter().copied());
        let mut payload = [0u8; PAYLOAD_LEN];
        for (slot, byte) in payload.iter_mut().zip(fields) {
            *slot = byte;
        }
        check_payload(&payload)?;
        Ok(Self { payload })
    }

    /// Decodes `bytes` - copied out of the ring first - as exactly one chunk
    /// record.
    ///
    /// # Errors
    ///
    /// The first problem found, as the module documentation lists them.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChunkError> {
        record::decode_exact(bytes, Self::read)
    }

    /// The part the chunk carries bytes of.
    #[must_use]
    pub fn part(&self) -> Part {
        self.payload
            .first()
            .and_then(|code| Part::from_code(*code))
            .unwrap_or(Part::Image)
    }

    /// The offset of the chunk's first byte in its part.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.payload
            .get(8..)
            .and_then(<[u8]>::first_chunk::<8>)
            .map_or(0, |bytes| u64::from_le_bytes(*bytes))
    }

    /// The chunk's bytes: from 1 to [`CHUNK_LEN`].
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        let len = self
            .payload
            .get(2..)
            .and_then(<[u8]>::first_chunk::<2>)
            .map_or(0, |bytes| usize::from(u16::from_le_bytes(*bytes)));
        self.payload
            .get(BYTES_AT..)
            .and_then(|rest| rest.get(..len))
            .unwrap_or_default()
    }

    /// The offset just past the chunk's last byte in its part: its offset
    /// plus the number of its bytes, which never overflows.
    #[must_use]
    pub fn end(&self) -> u64 {
        let len = u64::try_from(self.bytes().len()).unwrap_or(u64::MAX);
        self.offset().saturating_add(len)
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, ChunkError> {
        let payload = input.read_array()?;
        check_payload(&payload)?;
        Ok(Self { payload })
    }
}

impl fmt::Debug for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chunk")
            .field("part", &self.part())
            .field("offset", &self.offset())
            .field("len", &self.bytes().len())
            .finish_non_exhaustive()
    }
}

impl Message for Chunk {
    const KIND: u16 = 3;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.payload)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(ChunkError::to_decode_error)
    }
}

/// Splits `bytes`, a whole part, into chunks in order: `ceil(len /
/// CHUNK_LEN)` of them, all full but the last, the first at offset 0.
pub fn chunks(part: Part, bytes: &[u8]) -> impl Iterator<Item = Chunk> + '_ {
    bytes
        .chunks(CHUNK_LEN)
        .enumerate()
        .map_while(move |(index, piece)| {
            let offset = u64::try_from(index.checked_mul(CHUNK_LEN)?).ok()?;
            Chunk::new(part, offset, piece).ok()
        })
}

/// A number of bytes, if it is from 1 to `CHUNK_LEN`.
fn check_len(len: usize) -> Result<usize, ChunkError> {
    match len {
        0 => Err(ChunkError::Empty),
        len if len > CHUNK_LEN => Err(ChunkError::TooLong { len }),
        len => Ok(len),
    }
}

/// `offset`, if `offset + len` fits in 64 bits.
fn check_end(offset: u64, len: usize) -> Result<u64, ChunkError> {
    u64::try_from(len)
        .ok()
        .and_then(|count| offset.checked_add(count))
        .map(|_| offset)
        .ok_or(ChunkError::EndOverflows { offset, len })
}

/// Checks the payload's bytes, in the order the module documentation gives.
/// The bytes past the chunk's are checked over the whole array, every byte
/// whatever it holds: what is read, and how often the loop runs, never
/// depend on the input, and the loop has no early exit, which keeps Kani's
/// proof linear in the chunk's size.
fn check_payload(payload: &[u8; PAYLOAD_LEN]) -> Result<(), ChunkError> {
    let mut input = Reader::new(payload);
    let code = input.read_u8()?;
    if Part::from_code(code).is_none() {
        return Err(ChunkError::UnknownPart { code });
    }
    record::zeros::<1>(&mut input)?;
    let len = check_len(usize::from(input.read_u16()?))?;
    record::zeros::<4>(&mut input)?;
    check_end(input.read_u64()?, len)?;
    let bytes: [u8; CHUNK_LEN] = input.read_array()?;
    let mut past = 0u8;
    for (at, byte) in bytes.iter().enumerate() {
        past |= if at < len { 0 } else { *byte };
    }
    if past != 0 {
        return Err(ChunkError::ReservedNotZero);
    }
    input.finish()?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{decode_exact, encode};

    use super::*;

    fn encoded(chunk: &Chunk) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        assert_eq!(encode(chunk, &mut out), Ok(RECORD_LEN));
        out
    }

    #[test]
    fn layout_is_exactly_as_documented() {
        let chunk = Chunk::new(Part::Manifest, 0x0102_0304_0506_0708, b"abc").unwrap();
        let bytes = encoded(&chunk);
        // Header: kind 3, version 1, payload length 504, reserved.
        assert_eq!(bytes[..8], [3, 0, 1, 0, 0xF8, 0x01, 0, 0]);
        let payload = &bytes[HEADER_LEN..];
        assert_eq!(payload[..8], [2, 0, 3, 0, 0, 0, 0, 0]);
        assert_eq!(payload[8..16], [8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(&payload[16..19], b"abc");
        assert!(payload[19..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn round_trip() {
        let chunk = Chunk::new(Part::Image, 4000, &[9; CHUNK_LEN]).unwrap();
        let bytes = encoded(&chunk);
        assert_eq!(Chunk::decode(&bytes), Ok(chunk.clone()));
        assert_eq!(decode_exact::<Chunk>(&bytes), Ok(chunk.clone()));
        assert_eq!(chunk.part(), Part::Image);
        assert_eq!(chunk.offset(), 4000);
        assert_eq!(chunk.bytes(), [9; CHUNK_LEN]);
        assert_eq!(chunk.end(), 4488);
        assert!(format!("{chunk:?}").contains("offset: 4000"));
    }

    #[test]
    fn the_constructor_keeps_the_rules() {
        assert_eq!(Chunk::new(Part::Image, 0, &[]), Err(ChunkError::Empty));
        assert_eq!(
            Chunk::new(Part::Image, 0, &[1; CHUNK_LEN + 1]),
            Err(ChunkError::TooLong { len: CHUNK_LEN + 1 })
        );
        assert_eq!(
            Chunk::new(Part::Image, u64::MAX, &[1]),
            Err(ChunkError::EndOverflows {
                offset: u64::MAX,
                len: 1
            })
        );
        let last = Chunk::new(Part::Image, u64::MAX - 2, &[1, 2]).unwrap();
        assert_eq!(last.end(), u64::MAX);
    }

    #[test]
    fn a_part_splits_into_chunks_that_put_it_back_together() {
        let part: Vec<u8> = (0..2 * CHUNK_LEN + 7).map(|at| at as u8).collect();
        let pieces: Vec<Chunk> = chunks(Part::TrustList, &part).collect();
        assert_eq!(pieces.len(), 3);
        let mut joined = vec![0u8; part.len()];
        for piece in &pieces {
            assert_eq!(piece.part(), Part::TrustList);
            let at = usize::try_from(piece.offset()).unwrap();
            joined[at..usize::try_from(piece.end()).unwrap()].copy_from_slice(piece.bytes());
        }
        assert_eq!(joined, part);
        assert_eq!(pieces[2].bytes().len(), 7);
        assert_eq!(chunks(Part::Image, &[]).count(), 0);
    }

    #[test]
    fn errors_map_to_what_nowayos_ipc_reports() {
        let bytes = encoded(&Chunk::new(Part::TrustList, 0, b"x").unwrap());
        let mut bad = bytes;
        bad[HEADER_LEN] = 4;
        assert_eq!(
            Chunk::decode(&bad),
            Err(ChunkError::UnknownPart { code: 4 })
        );
        assert_eq!(
            decode_exact::<Chunk>(&bad),
            Err(DecodeError::InvalidValue { field: "part" })
        );
        let mut bad = bytes;
        bad[HEADER_LEN + BYTES_AT + 1] = 1;
        assert_eq!(Chunk::decode(&bad), Err(ChunkError::ReservedNotZero));
        assert_eq!(
            decode_exact::<Chunk>(&bad),
            Err(DecodeError::NonZeroPadding)
        );
        let mut bad = bytes;
        bad[HEADER_LEN + 8..HEADER_LEN + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            Chunk::decode(&bad),
            Err(ChunkError::EndOverflows {
                offset: u64::MAX,
                len: 1
            })
        );
        assert_eq!(
            decode_exact::<Chunk>(&bad),
            Err(DecodeError::InvalidValue { field: "offset" })
        );
    }

    #[test]
    fn parts_have_one_code_each() {
        for code in 0..=u8::MAX {
            if let Some(part) = Part::from_code(code) {
                assert_eq!(part.code(), code);
            } else {
                assert!(code == 0 || code > 3);
            }
        }
    }

    #[test]
    fn every_error_says_what_it_is() {
        let errors = [
            ChunkError::Frame(DecodeError::NonZeroPadding),
            ChunkError::ReservedNotZero,
            ChunkError::UnknownPart { code: 9 },
            ChunkError::Empty,
            ChunkError::TooLong { len: 489 },
            ChunkError::EndOverflows {
                offset: u64::MAX,
                len: 1,
            },
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(!error.to_decode_error().to_string().is_empty());
        }
    }
}
