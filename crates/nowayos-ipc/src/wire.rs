// SPDX-License-Identifier: MIT OR Apache-2.0
//! The record format: header, the [`Message`] trait, [`encode`] and [`decode`].

use crate::{DecodeError, EncodeError, Reader, Writer};

/// Size of the record header in bytes.
pub const HEADER_LEN: usize = 8;

/// The largest payload a header can describe.
pub const MAX_PAYLOAD_LEN: usize = u16::MAX as usize;

/// The fixed header in front of every record. See the crate documentation
/// for the byte layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header {
    /// Message type, unique on its channel.
    pub kind: u16,
    /// Layout version of that message type.
    pub version: u16,
    /// Number of payload bytes after the header.
    pub payload_len: u16,
}

impl Header {
    /// Reads and checks the header at the start of `bytes`. Only the header
    /// is examined; the payload is not.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if `bytes` is shorter than [`HEADER_LEN`];
    /// [`DecodeError::ReservedNotZero`] if the reserved field is set.
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let kind = reader.read_u16()?;
        let version = reader.read_u16()?;
        let payload_len = reader.read_u16()?;
        if reader.read_u16()? != 0 {
            return Err(DecodeError::ReservedNotZero);
        }
        Ok(Self {
            kind,
            version,
            payload_len,
        })
    }

    /// Writes the header to the start of `out`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if `out` is shorter than
    /// [`HEADER_LEN`].
    pub fn write(&self, out: &mut [u8]) -> Result<(), EncodeError> {
        let mut writer = Writer::new(out);
        writer.write_u16(self.kind)?;
        writer.write_u16(self.version)?;
        writer.write_u16(self.payload_len)?;
        writer.write_zeros(2)
    }

    /// Header plus payload length in bytes.
    #[must_use]
    pub const fn record_len(&self) -> usize {
        HEADER_LEN.saturating_add(self.payload_len as usize)
    }
}

/// A fixed-size, plain-data record that can cross a protection-domain
/// boundary.
///
/// Implementations describe the payload field by field with [`Writer`] and
/// [`Reader`], so the wire layout is explicit and independent of Rust's
/// memory layout. Rules for implementors:
///
/// * write exactly [`PAYLOAD_LEN`](Self::PAYLOAD_LEN) bytes (checked by
///   [`encode`]);
/// * in [`decode_payload`](Self::decode_payload), validate every field —
///   enum tags, ranges, padding — and return
///   [`DecodeError::InvalidValue`] rather than guessing;
/// * give every value exactly one encoding (for example, require padding to
///   be zero), so equal messages are equal bytes;
/// * never put pointers, references or addresses in a payload.
///
/// Provisional: this API may change.
///
/// ```
/// use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer, decode, encode};
///
/// #[derive(Debug, PartialEq)]
/// struct Ping {
///     sequence: u32,
/// }
///
/// impl Message for Ping {
///     const KIND: u16 = 0x0001;
///     const VERSION: u16 = 1;
///     const PAYLOAD_LEN: usize = 4;
///
///     fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
///         out.write_u32(self.sequence)
///     }
///
///     fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
///         Ok(Self { sequence: input.read_u32()? })
///     }
/// }
///
/// let mut buf = [0u8; 16];
/// let len = encode(&Ping { sequence: 7 }, &mut buf).unwrap();
/// assert_eq!(len, 12);
/// assert_eq!(decode::<Ping>(&buf[..len]), Ok(Ping { sequence: 7 }));
/// assert!(decode::<Ping>(&buf[..5]).is_err()); // truncated: an error, never a panic
/// ```
pub trait Message: Sized {
    /// The message type's number on its channel.
    const KIND: u16;
    /// The payload layout version this implementation writes and reads.
    const VERSION: u16;
    /// The exact payload length in bytes, at most [`MAX_PAYLOAD_LEN`].
    const PAYLOAD_LEN: usize;

    /// Writes the payload. `out` has room for at least
    /// [`PAYLOAD_LEN`](Self::PAYLOAD_LEN) bytes.
    ///
    /// # Errors
    ///
    /// Whatever the [`Writer`] reports, or [`EncodeError::InvalidValue`] for
    /// a value that cannot be represented.
    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError>;

    /// Reads and validates a payload of exactly
    /// [`PAYLOAD_LEN`](Self::PAYLOAD_LEN) bytes, already copied into private
    /// memory.
    ///
    /// # Errors
    ///
    /// Whatever the [`Reader`] reports, or [`DecodeError::InvalidValue`] for
    /// a field that fails validation.
    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError>;
}

/// The full record length (header plus payload) for message type `M`.
///
/// Usable in constants, for example to check at compile time that a message
/// fits a ring slot:
///
/// ```
/// # use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};
/// # struct Ping;
/// # impl Message for Ping {
/// #     const KIND: u16 = 1;
/// #     const VERSION: u16 = 1;
/// #     const PAYLOAD_LEN: usize = 4;
/// #     fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> { out.write_u32(0) }
/// #     fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> { input.read_u32()?; Ok(Ping) }
/// # }
/// const SLOT: usize = 16;
/// const _: () = assert!(nowayos_ipc::encoded_len::<Ping>() <= SLOT);
/// ```
#[must_use]
pub const fn encoded_len<M: Message>() -> usize {
    HEADER_LEN.saturating_add(M::PAYLOAD_LEN)
}

/// Encodes `message` as a record at the start of `out`, returning the record
/// length. Bytes of `out` after the record are left untouched.
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`] if `out` cannot hold the record,
/// [`EncodeError::PayloadTooLarge`] if the type's payload length does not fit
/// the header, [`EncodeError::WrongLength`] if the payload encoder wrote the
/// wrong amount, or any error from the payload encoder.
pub fn encode<M: Message>(message: &M, out: &mut [u8]) -> Result<usize, EncodeError> {
    let payload_len = u16::try_from(M::PAYLOAD_LEN).map_err(|_| EncodeError::PayloadTooLarge {
        len: M::PAYLOAD_LEN,
    })?;
    let total = encoded_len::<M>();
    let available = out.len();
    let record = out.get_mut(..total).ok_or(EncodeError::BufferTooSmall {
        needed: total,
        available,
    })?;
    let (header, payload) =
        record
            .split_at_mut_checked(HEADER_LEN)
            .ok_or(EncodeError::BufferTooSmall {
                needed: total,
                available,
            })?;
    Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len,
    }
    .write(header)?;
    let mut writer = Writer::new(payload);
    message.encode_payload(&mut writer)?;
    if writer.position() != M::PAYLOAD_LEN {
        return Err(EncodeError::WrongLength {
            declared: M::PAYLOAD_LEN,
            written: writer.position(),
        });
    }
    Ok(total)
}

/// Decodes a record of type `M` from the start of `bytes`. Bytes after the
/// record (such as the unused tail of a ring slot) are ignored; use
/// [`decode_exact`] when `bytes` must be exactly one record.
///
/// `bytes` must be in private memory — see the crate documentation on
/// copy-then-validate. [`Consumer`](crate::Consumer) arranges that.
///
/// # Errors
///
/// A [`DecodeError`] describing the first problem found. The checks run in
/// order: header present and well-formed, kind, version, payload length,
/// payload present, then the payload's own validation and full consumption.
pub fn decode<M: Message>(bytes: &[u8]) -> Result<M, DecodeError> {
    decode_prefix(bytes).map(|(message, _)| message)
}

/// Like [`decode`], but also fails with [`DecodeError::TrailingBytes`] if
/// `bytes` continues past the record.
///
/// # Errors
///
/// As for [`decode`], plus [`DecodeError::TrailingBytes`].
pub fn decode_exact<M: Message>(bytes: &[u8]) -> Result<M, DecodeError> {
    let (message, used) = decode_prefix(bytes)?;
    match bytes.len().saturating_sub(used) {
        0 => Ok(message),
        unread => Err(DecodeError::TrailingBytes { unread }),
    }
}

fn decode_prefix<M: Message>(bytes: &[u8]) -> Result<(M, usize), DecodeError> {
    let header = Header::parse(bytes)?;
    if header.kind != M::KIND {
        return Err(DecodeError::WrongKind {
            expected: M::KIND,
            found: header.kind,
        });
    }
    if header.version != M::VERSION {
        return Err(DecodeError::UnsupportedVersion {
            expected: M::VERSION,
            found: header.version,
        });
    }
    let payload_len = usize::from(header.payload_len);
    if payload_len != M::PAYLOAD_LEN {
        return Err(DecodeError::WrongLength {
            expected: M::PAYLOAD_LEN,
            found: payload_len,
        });
    }
    let total = header.record_len();
    let payload = bytes.get(HEADER_LEN..total).ok_or(DecodeError::Truncated {
        needed: total,
        available: bytes.len(),
    })?;
    let mut reader = Reader::new(payload);
    let message = M::decode_payload(&mut reader)?;
    reader.finish()?;
    Ok((message, total))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Pair {
        a: u16,
        b: bool,
    }

    impl Message for Pair {
        const KIND: u16 = 0x0A0B;
        const VERSION: u16 = 3;
        const PAYLOAD_LEN: usize = 4;

        fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
            out.write_u16(self.a)?;
            out.write_bool(self.b)?;
            out.write_zeros(1)
        }

        fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
            let a = input.read_u16()?;
            let b = input.read_bool()?;
            input.read_zeros(1)?;
            Ok(Self { a, b })
        }
    }

    /// Writes one byte too few.
    struct Short;

    impl Message for Short {
        const KIND: u16 = 1;
        const VERSION: u16 = 1;
        const PAYLOAD_LEN: usize = 2;

        fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
            out.write_u8(0)
        }

        fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
            input.read_u16()?;
            Ok(Self)
        }
    }

    /// Declares a payload too large for the header.
    struct Huge;

    impl Message for Huge {
        const KIND: u16 = 1;
        const VERSION: u16 = 1;
        const PAYLOAD_LEN: usize = MAX_PAYLOAD_LEN + 1;

        fn encode_payload(&self, _out: &mut Writer<'_>) -> Result<(), EncodeError> {
            Ok(())
        }

        fn decode_payload(_input: &mut Reader<'_>) -> Result<Self, DecodeError> {
            Ok(Self)
        }
    }

    fn encoded(pair: &Pair) -> [u8; 12] {
        let mut buf = [0u8; 12];
        assert_eq!(encode(pair, &mut buf).unwrap(), 12);
        buf
    }

    #[test]
    fn layout_is_exactly_as_documented() {
        let buf = encoded(&Pair { a: 0x1234, b: true });
        assert_eq!(buf, [0x0B, 0x0A, 3, 0, 4, 0, 0, 0, 0x34, 0x12, 1, 0]);
    }

    #[test]
    fn round_trip() {
        let pair = Pair { a: 7, b: false };
        assert_eq!(decode::<Pair>(&encoded(&pair)), Ok(pair));
    }

    #[test]
    fn header_checks_in_order() {
        let good = encoded(&Pair { a: 1, b: true });
        let mut bad = good;
        bad[6] = 1;
        assert_eq!(decode::<Pair>(&bad), Err(DecodeError::ReservedNotZero));
        let mut bad = good;
        bad[0] = 0;
        assert_eq!(
            decode::<Pair>(&bad),
            Err(DecodeError::WrongKind {
                expected: 0x0A0B,
                found: 0x0A00
            })
        );
        let mut bad = good;
        bad[2] = 4;
        assert_eq!(
            decode::<Pair>(&bad),
            Err(DecodeError::UnsupportedVersion {
                expected: 3,
                found: 4
            })
        );
        let mut bad = good;
        bad[4] = 200;
        assert_eq!(
            decode::<Pair>(&bad),
            Err(DecodeError::WrongLength {
                expected: 4,
                found: 200
            })
        );
    }

    #[test]
    fn payload_validation_is_applied() {
        let mut bad = encoded(&Pair { a: 1, b: true });
        bad[10] = 2; // bool must be 0 or 1
        assert_eq!(
            decode::<Pair>(&bad),
            Err(DecodeError::InvalidValue { field: "bool" })
        );
        let mut bad = encoded(&Pair { a: 1, b: true });
        bad[11] = 9; // padding must be zero
        assert_eq!(decode::<Pair>(&bad), Err(DecodeError::NonZeroPadding));
    }

    #[test]
    fn truncation_at_every_length() {
        let buf = encoded(&Pair { a: 1, b: true });
        for len in 0..buf.len() {
            assert!(
                matches!(
                    decode::<Pair>(&buf[..len]),
                    Err(DecodeError::Truncated { .. })
                ),
                "prefix of {len} bytes"
            );
        }
    }

    #[test]
    fn trailing_bytes_prefix_versus_exact() {
        let mut buf = [0u8; 16];
        encode(&Pair { a: 5, b: false }, &mut buf).unwrap();
        assert!(decode::<Pair>(&buf).is_ok());
        assert_eq!(
            decode_exact::<Pair>(&buf),
            Err(DecodeError::TrailingBytes { unread: 4 })
        );
        assert!(decode_exact::<Pair>(&buf[..12]).is_ok());
    }

    #[test]
    fn encode_errors() {
        let mut small = [0u8; 11];
        assert_eq!(
            encode(&Pair { a: 0, b: false }, &mut small),
            Err(EncodeError::BufferTooSmall {
                needed: 12,
                available: 11
            })
        );
        let mut buf = [0u8; 16];
        assert_eq!(
            encode(&Short, &mut buf),
            Err(EncodeError::WrongLength {
                declared: 2,
                written: 1
            })
        );
        assert_eq!(
            encode(&Huge, &mut buf),
            Err(EncodeError::PayloadTooLarge {
                len: MAX_PAYLOAD_LEN + 1
            })
        );
    }

    #[test]
    fn encode_leaves_the_rest_of_the_buffer_alone() {
        let mut buf = [0xEEu8; 14];
        encode(&Pair { a: 0, b: false }, &mut buf).unwrap();
        assert_eq!(buf[12..], [0xEE, 0xEE]);
    }
}
