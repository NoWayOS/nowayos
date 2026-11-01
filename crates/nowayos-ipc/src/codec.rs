// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounds-checked cursors for writing and reading message payloads.
//!
//! Payload encoders and decoders use these instead of indexing, so an
//! implementation of [`Message`](crate::Message) cannot read or write out of
//! bounds or panic, whatever bytes arrive. Multi-byte integers are
//! little-endian.

use crate::{DecodeError, EncodeError};

/// Writes a payload into a fixed buffer, front to back.
#[derive(Debug)]
pub struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    /// A writer that fills `buf` from the start.
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Bytes of space left.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Writes `bytes` verbatim.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if they do not fit; nothing is written.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self.pos.saturating_add(bytes.len());
        let available = self.buf.len();
        let target = self
            .buf
            .get_mut(self.pos..end)
            .ok_or(EncodeError::BufferTooSmall {
                needed: end,
                available,
            })?;
        target.copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }

    /// Writes `count` zero bytes (padding).
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if they do not fit; nothing is written.
    pub fn write_zeros(&mut self, count: usize) -> Result<(), EncodeError> {
        let end = self.pos.saturating_add(count);
        let available = self.buf.len();
        let target = self
            .buf
            .get_mut(self.pos..end)
            .ok_or(EncodeError::BufferTooSmall {
                needed: end,
                available,
            })?;
        target.fill(0);
        self.pos = end;
        Ok(())
    }

    /// Writes one byte.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.write_bytes(&[value])
    }

    /// Writes a `bool` as one byte, `0` or `1`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_bool(&mut self, value: bool) -> Result<(), EncodeError> {
        self.write_u8(u8::from(value))
    }

    /// Writes a little-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_le_bytes())
    }

    /// Writes a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_le_bytes())
    }

    /// Writes a little-endian `u64`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_le_bytes())
    }

    /// Writes a little-endian two's-complement `i32`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if there is no room.
    pub fn write_i32(&mut self, value: i32) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_le_bytes())
    }
}

/// Reads a payload from bytes that are already in private memory, front to
/// back.
///
/// Every read is bounds-checked and returns [`DecodeError::Truncated`]
/// rather than panicking when the input runs out.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader positioned at the start of `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes read so far.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Bytes left to read.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Reads the next `count` bytes.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer remain; the position is unchanged.
    pub fn read_bytes(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.saturating_add(count);
        let bytes = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated {
            needed: end,
            available: self.buf.len(),
        })?;
        self.pos = end;
        Ok(bytes)
    }

    /// Reads the next `N` bytes as an array.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer remain; the position is unchanged.
    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let bytes = self.read_bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// Reads `count` bytes of padding, which must all be zero.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer remain, or
    /// [`DecodeError::NonZeroPadding`] if any byte is not zero.
    pub fn read_zeros(&mut self, count: usize) -> Result<(), DecodeError> {
        if self.read_bytes(count)?.iter().all(|&b| b == 0) {
            Ok(())
        } else {
            Err(DecodeError::NonZeroPadding)
        }
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] at the end of the input.
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let [value] = self.read_array::<1>()?;
        Ok(value)
    }

    /// Reads a `bool` stored as one byte. Only `0` and `1` are accepted, so
    /// every value has exactly one encoding.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] at the end of the input, or
    /// [`DecodeError::InvalidValue`] for any other byte.
    pub fn read_bool(&mut self) -> Result<bool, DecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::InvalidValue { field: "bool" }),
        }
    }

    /// Reads a little-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer than 2 bytes remain.
    pub fn read_u16(&mut self) -> Result<u16, DecodeError> {
        self.read_array().map(u16::from_le_bytes)
    }

    /// Reads a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer than 4 bytes remain.
    pub fn read_u32(&mut self) -> Result<u32, DecodeError> {
        self.read_array().map(u32::from_le_bytes)
    }

    /// Reads a little-endian `u64`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer than 8 bytes remain.
    pub fn read_u64(&mut self) -> Result<u64, DecodeError> {
        self.read_array().map(u64::from_le_bytes)
    }

    /// Reads a little-endian two's-complement `i32`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`] if fewer than 4 bytes remain.
    pub fn read_i32(&mut self) -> Result<i32, DecodeError> {
        self.read_array().map(i32::from_le_bytes)
    }

    /// Checks that every byte has been read.
    ///
    /// # Errors
    ///
    /// [`DecodeError::TrailingBytes`] if any remain.
    pub fn finish(&self) -> Result<(), DecodeError> {
        match self.remaining() {
            0 => Ok(()),
            unread => Err(DecodeError::TrailingBytes { unread }),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn writes_little_endian_in_order() {
        let mut buf = [0xFFu8; 16];
        let mut w = Writer::new(&mut buf);
        w.write_u8(1).unwrap();
        w.write_bool(true).unwrap();
        w.write_u16(0x0302).unwrap();
        w.write_i32(-2).unwrap();
        w.write_zeros(2).unwrap();
        w.write_u32(0x0706_0504).unwrap();
        assert_eq!(w.position(), 14);
        assert_eq!(w.remaining(), 2);
        assert_eq!(
            buf[..14],
            [1, 1, 2, 3, 0xFE, 0xFF, 0xFF, 0xFF, 0, 0, 4, 5, 6, 7]
        );
    }

    #[test]
    fn writer_refuses_overflow_without_partial_writes() {
        let mut buf = [0u8; 3];
        let mut w = Writer::new(&mut buf);
        w.write_u16(0xAAAA).unwrap();
        assert_eq!(
            w.write_u16(1),
            Err(EncodeError::BufferTooSmall {
                needed: 4,
                available: 3
            })
        );
        assert_eq!(w.position(), 2);
        assert_eq!(buf, [0xAA, 0xAA, 0]);
    }

    #[test]
    fn reads_back_what_was_written() {
        let mut buf = [0u8; 19];
        let mut w = Writer::new(&mut buf);
        w.write_u8(9).unwrap();
        w.write_bool(false).unwrap();
        w.write_u16(515).unwrap();
        w.write_i32(i32::MIN).unwrap();
        w.write_u64(u64::MAX - 1).unwrap();
        w.write_zeros(3).unwrap();
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u8().unwrap(), 9);
        assert!(!r.read_bool().unwrap());
        assert_eq!(r.read_u16().unwrap(), 515);
        assert_eq!(r.read_i32().unwrap(), i32::MIN);
        assert_eq!(r.read_u64().unwrap(), u64::MAX - 1);
        r.read_zeros(3).unwrap();
        r.finish().unwrap();
    }

    #[test]
    fn reader_reports_truncation_and_keeps_position() {
        let buf = [1u8, 2, 3];
        let mut r = Reader::new(&buf);
        r.read_u8().unwrap();
        assert_eq!(
            r.read_u32(),
            Err(DecodeError::Truncated {
                needed: 5,
                available: 3
            })
        );
        assert_eq!(r.position(), 1);
        assert_eq!(r.finish(), Err(DecodeError::TrailingBytes { unread: 2 }));
    }

    #[test]
    fn strict_bool_and_padding() {
        assert_eq!(
            Reader::new(&[2]).read_bool(),
            Err(DecodeError::InvalidValue { field: "bool" })
        );
        assert_eq!(
            Reader::new(&[0, 1]).read_zeros(2),
            Err(DecodeError::NonZeroPadding)
        );
    }

    #[test]
    fn huge_counts_do_not_overflow() {
        let buf = [0u8; 4];
        let mut r = Reader::new(&buf);
        r.read_u8().unwrap();
        assert!(matches!(
            r.read_bytes(usize::MAX),
            Err(DecodeError::Truncated { .. })
        ));
        let mut out = [0u8; 4];
        let mut w = Writer::new(&mut out);
        w.write_u8(0).unwrap();
        assert!(w.write_zeros(usize::MAX).is_err());
    }
}
