// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the update records share: their frame, checked in the order
//! `nowayos_ipc::decode_exact` checks it, so that a record's own decoder and
//! `nowayos-ipc`'s report the same first problem for any input.

use nowayos_ipc::{DecodeError, HEADER_LEN, Header, Message, Reader};

/// Decodes `bytes` as exactly one record of type `M`, reading its payload
/// with `read`, which reports the record's own errors, `E`. The checks run in
/// `decode_exact`'s order: the header (present, reserved field zero), its
/// kind, version and payload length, the payload present, then the payload
/// itself and all of it read, and last, nothing after the record.
pub(crate) fn decode_exact<M, E>(
    bytes: &[u8],
    read: fn(&mut Reader<'_>) -> Result<M, E>,
) -> Result<M, E>
where
    M: Message,
    E: From<DecodeError>,
{
    let header = Header::parse(bytes)?;
    if header.kind != M::KIND {
        return Err(DecodeError::WrongKind {
            expected: M::KIND,
            found: header.kind,
        }
        .into());
    }
    if header.version != M::VERSION {
        return Err(DecodeError::UnsupportedVersion {
            expected: M::VERSION,
            found: header.version,
        }
        .into());
    }
    let payload_len = usize::from(header.payload_len);
    if payload_len != M::PAYLOAD_LEN {
        return Err(DecodeError::WrongLength {
            expected: M::PAYLOAD_LEN,
            found: payload_len,
        }
        .into());
    }
    let total = header.record_len();
    let payload = bytes.get(HEADER_LEN..total).ok_or(DecodeError::Truncated {
        needed: total,
        available: bytes.len(),
    })?;
    let mut reader = Reader::new(payload);
    let message = read(&mut reader)?;
    reader.finish()?;
    match bytes.len().saturating_sub(total) {
        0 => Ok(message),
        unread => Err(DecodeError::TrailingBytes { unread }.into()),
    }
}

/// Reads `N` bytes that must all be zero - reserved bytes, or an entry past
/// its count - and fails as `Reader::read_zeros` does. The bytes are read as
/// one array and compared whole, so the check never loops over a slice
/// whose length a model checker cannot see: Kani unrolls it exactly.
///
/// # Errors
///
/// [`DecodeError::Truncated`] if fewer than `N` bytes remain, or
/// [`DecodeError::NonZeroPadding`] if any is not zero.
pub(crate) fn zeros<const N: usize>(input: &mut Reader<'_>) -> Result<(), DecodeError> {
    if input.read_array::<N>()? == [0u8; N] {
        Ok(())
    } else {
        Err(DecodeError::NonZeroPadding)
    }
}

/// The header every record of type `M` starts with, as its bytes: kind,
/// layout version and payload length, little-endian, then the two reserved
/// bytes, zero. Each record's module checks at compile time that its payload
/// length fits the header.
pub(crate) fn header_bytes<M: Message>() -> [u8; HEADER_LEN] {
    let payload_len = u16::try_from(M::PAYLOAD_LEN).unwrap_or(u16::MAX);
    let fields = M::KIND
        .to_le_bytes()
        .into_iter()
        .chain(M::VERSION.to_le_bytes())
        .chain(payload_len.to_le_bytes());
    let mut header = [0u8; HEADER_LEN];
    for (slot, byte) in header.iter_mut().zip(fields) {
        *slot = byte;
    }
    header
}
