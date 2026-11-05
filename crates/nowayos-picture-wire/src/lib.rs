// SPDX-License-Identifier: MIT OR Apache-2.0
//! The records between a client (the UI PD, or a PD that fetches
//! pictures) and the picture decoder PD (#10084): a picture to decode, and
//! its outcome.
//!
//! A picture received at run time is hostile input, so decoding it is the
//! work of a PD that holds nothing else. The client writes the encoded
//! bytes - a PNG or a JPEG, at most [`MAX_ENCODED`] - into a region shared
//! with the decoder, and sends a [`Decode`] naming how many and which
//! format. The decoder copies them into its own memory before it reads a
//! byte, decodes them within [`MAX_SIDE`] and [`MAX_PIXELS`], and writes
//! the pixels - premultiplied RGBA, four bytes a pixel, row by row - into a
//! second shared region, then answers with a [`Decoded`]: the picture's
//! size, or why it was refused. The client copies the pixels out of the
//! region, checking them against the size it was told, before it draws
//! them; a decoder that faults is restarted, and its client never is.
//!
//! Both records are fixed-size, versioned, and validated field by field on
//! receipt (`nowayos-ipc`); `src/proofs.rs` proves any bytes decode to a
//! record whose encoding is exactly those bytes, or to a typed error.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(kani)]
mod proofs;

use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

/// The message kind of a [`Decode`].
pub const DECODE_KIND: u16 = 0x0B01;

/// The message kind of a [`Decoded`].
pub const DECODED_KIND: u16 = 0x0B81;

/// The most bytes of an encoded picture: the size of the shared region
/// the client writes them into.
pub const MAX_ENCODED: u32 = 1 << 20;

/// The most pixels along either side of a decoded picture.
pub const MAX_SIDE: u16 = 1024;

/// The most pixels of a decoded picture: the size of the shared region
/// the decoder writes them into is four bytes each.
pub const MAX_PIXELS: u32 = 1024 * 1024;

/// The bytes of the region the decoder writes pixels into.
pub const PIXEL_BYTES: usize = 4 * 1024 * 1024;

/// A picture's encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// PNG.
    Png,
    /// JPEG (baseline or progressive).
    Jpeg,
}

impl Format {
    const fn wire(self) -> u8 {
        match self {
            Self::Png => 1,
            Self::Jpeg => 2,
        }
    }

    const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Png),
            2 => Some(Self::Jpeg),
            _ => None,
        }
    }
}

/// Decode the `len` bytes at the start of the encoded region as `format`;
/// `id` is the client's, echoed in the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decode {
    /// The client's number for this picture.
    pub id: u16,
    /// Its encoding.
    pub format: Format,
    /// How many bytes it is: 1 to [`MAX_ENCODED`].
    pub len: u32,
}

/// Wire layout, 8 bytes, little-endian: `id` (`u16`), `format` (1 PNG, 2
/// JPEG), a zero byte, `len` (`u32`, 1 to [`MAX_ENCODED`]).
impl Message for Decode {
    const KIND: u16 = DECODE_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 8;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        if self.len == 0 || self.len > MAX_ENCODED {
            return Err(EncodeError::InvalidValue { field: "len" });
        }
        out.write_u16(self.id)?;
        out.write_u8(self.format.wire())?;
        out.write_zeros(1)?;
        out.write_u32(self.len)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let id = input.read_u16()?;
        let format = Format::from_wire(input.read_u8()?)
            .ok_or(DecodeError::InvalidValue { field: "format" })?;
        input.read_zeros(1)?;
        let len = input.read_u32()?;
        if len == 0 || len > MAX_ENCODED {
            return Err(DecodeError::InvalidValue { field: "len" });
        }
        Ok(Self { id, format, len })
    }
}

/// Why a picture was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// Wider or taller than [`MAX_SIDE`], or more than [`MAX_PIXELS`]: told
    /// by its header, before any of it is decoded.
    TooLarge,
    /// Not a well-formed picture of its format: truncated, or corrupt.
    Malformed,
    /// A kind of picture the decoder does not take: a colour type or
    /// encoding outside what it decodes.
    Unsupported,
    /// The decoder ran out of its bounded memory.
    OutOfMemory,
}

impl Refusal {
    const fn wire(self) -> u8 {
        match self {
            Self::TooLarge => 1,
            Self::Malformed => 2,
            Self::Unsupported => 3,
            Self::OutOfMemory => 4,
        }
    }

    const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::TooLarge),
            2 => Some(Self::Malformed),
            3 => Some(Self::Unsupported),
            4 => Some(Self::OutOfMemory),
            _ => None,
        }
    }
}

/// What became of picture `id`: its size, its pixels now in the pixel
/// region, or why it was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decoded {
    /// The client's number for it.
    pub id: u16,
    /// Its width and height in pixels, each 1 to [`MAX_SIDE`], at most
    /// [`MAX_PIXELS`] together; or why it was refused.
    pub outcome: Result<(u16, u16), Refusal>,
}

/// Wire layout, 8 bytes, little-endian: `id` (`u16`), then 0 and a zero
/// byte, `width` and `height` (`u16` each), for a picture decoded; or the
/// refusal (1 to 4) and five zero bytes.
impl Message for Decoded {
    const KIND: u16 = DECODED_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 8;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u16(self.id)?;
        match self.outcome {
            Ok((width, height)) => {
                if !fits(width, height) {
                    return Err(EncodeError::InvalidValue { field: "size" });
                }
                out.write_zeros(2)?;
                out.write_u16(width)?;
                out.write_u16(height)
            }
            Err(refusal) => {
                out.write_u8(refusal.wire())?;
                out.write_zeros(5)
            }
        }
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let id = input.read_u16()?;
        let tag = input.read_u8()?;
        let outcome = if tag == 0 {
            input.read_zeros(1)?;
            let width = input.read_u16()?;
            let height = input.read_u16()?;
            if !fits(width, height) {
                return Err(DecodeError::InvalidValue { field: "size" });
            }
            Ok((width, height))
        } else {
            let refusal =
                Refusal::from_wire(tag).ok_or(DecodeError::InvalidValue { field: "refusal" })?;
            input.read_zeros(5)?;
            Err(refusal)
        };
        Ok(Self { id, outcome })
    }
}

/// Whether a picture `width` x `height` may be decoded: each side 1 to
/// [`MAX_SIDE`], at most [`MAX_PIXELS`] together.
#[must_use]
pub const fn fits(width: u16, height: u16) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SIDE
        && height <= MAX_SIDE
        && (width as u32).saturating_mul(height as u32) <= MAX_PIXELS
}

/// The bytes of the pixels of a picture `width` x `height` that
/// [`fits`]: four a pixel.
#[must_use]
pub const fn pixel_len(width: u16, height: u16) -> usize {
    (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod tests {
    use nowayos_ipc::{HEADER_LEN, decode_exact, encode, encoded_len};
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn records_go_round_and_refuse_what_is_out_of_range() {
        let mut out = [0_u8; 16];
        let request = Decode {
            id: 7,
            format: Format::Jpeg,
            len: 1234,
        };
        let n = encode(&request, &mut out).unwrap();
        assert_eq!(n, encoded_len::<Decode>());
        assert_eq!(decode_exact::<Decode>(&out[..n]), Ok(request));
        for (id, outcome) in [
            (1, Ok((MAX_SIDE, MAX_SIDE))),
            (2, Ok((1, 1))),
            (3, Err(Refusal::TooLarge)),
            (4, Err(Refusal::OutOfMemory)),
        ] {
            let answer = Decoded { id, outcome };
            let n = encode(&answer, &mut out).unwrap();
            assert_eq!(decode_exact::<Decoded>(&out[..n]), Ok(answer));
        }
        // Nothing past the bounds is ever sent.
        for len in [0, MAX_ENCODED + 1] {
            let bad = Decode { len, ..request };
            assert!(encode(&bad, &mut out).is_err());
        }
        for size in [(0, 4), (MAX_SIDE + 1, 1)] {
            let bad = Decoded {
                id: 1,
                outcome: Ok(size),
            };
            assert!(encode(&bad, &mut out).is_err());
        }
        assert_eq!(pixel_len(MAX_SIDE, MAX_SIDE), PIXEL_BYTES);
    }

    proptest! {
        /// Any payload decodes to a record whose encoding is exactly its
        /// bytes, or to a typed error: never a panic.
        #[test]
        fn any_payload_decodes_to_its_one_encoding_or_an_error(payload in any::<[u8; 8]>()) {
            for kind in [DECODE_KIND, DECODED_KIND] {
                let mut bytes = [0_u8; HEADER_LEN + 8];
                let header = nowayos_ipc::Header { kind, version: 1, payload_len: 8 };
                header.write(&mut bytes).unwrap();
                bytes[HEADER_LEN..].copy_from_slice(&payload);
                let mut out = [0_u8; HEADER_LEN + 8];
                if kind == DECODE_KIND {
                    if let Ok(record) = decode_exact::<Decode>(&bytes) {
                        prop_assert_eq!(encode(&record, &mut out), Ok(bytes.len()));
                        prop_assert_eq!(out, bytes);
                    }
                } else if let Ok(record) = decode_exact::<Decoded>(&bytes) {
                    prop_assert_eq!(encode(&record, &mut out), Ok(bytes.len()));
                    prop_assert_eq!(out, bytes);
                    if let Ok((width, height)) = record.outcome {
                        prop_assert!(pixel_len(width, height) <= PIXEL_BYTES);
                    }
                }
            }
        }
    }
}
