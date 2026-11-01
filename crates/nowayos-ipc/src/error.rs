// SPDX-License-Identifier: MIT OR Apache-2.0
//! Error types. Every failure mode of encoding, decoding and the ring has a
//! variant here; nothing in this crate panics instead.

use core::fmt;

/// Why a record could not be decoded. Returned for malformed input; decoding
/// never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Fewer bytes than the header or the declared payload needs.
    Truncated {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },
    /// The header's reserved field is not zero.
    ReservedNotZero,
    /// The record is a different kind of message from the one expected.
    WrongKind {
        /// The kind the decoder reads.
        expected: u16,
        /// The kind in the header.
        found: u16,
    },
    /// The record uses a layout version this decoder does not read.
    UnsupportedVersion {
        /// The version the decoder reads.
        expected: u16,
        /// The version in the header.
        found: u16,
    },
    /// The header's payload length is not this kind's fixed length.
    WrongLength {
        /// The kind's fixed payload length.
        expected: usize,
        /// The length in the header.
        found: usize,
    },
    /// The payload decoder finished without reading every payload byte.
    TrailingBytes {
        /// Bytes left unread.
        unread: usize,
    },
    /// Padding that must be zero is not.
    NonZeroPadding,
    /// A field holds a value its type does not allow.
    InvalidValue {
        /// Which field (a static name, for diagnostics).
        field: &'static str,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => {
                write!(
                    f,
                    "truncated record: needed {needed} bytes, had {available}"
                )
            }
            Self::ReservedNotZero => f.write_str("reserved header field is not zero"),
            Self::WrongKind { expected, found } => {
                write!(
                    f,
                    "wrong message kind: expected {expected:#06x}, found {found:#06x}"
                )
            }
            Self::UnsupportedVersion { expected, found } => {
                write!(f, "unsupported version: expected {expected}, found {found}")
            }
            Self::WrongLength { expected, found } => {
                write!(
                    f,
                    "wrong payload length: expected {expected}, found {found}"
                )
            }
            Self::TrailingBytes { unread } => write!(f, "{unread} payload bytes left unread"),
            Self::NonZeroPadding => f.write_str("padding bytes are not zero"),
            Self::InvalidValue { field } => write!(f, "invalid value in field `{field}`"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Why a message could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The output buffer is shorter than the encoded record.
    BufferTooSmall {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },
    /// The message type declares a payload longer than the header can state.
    PayloadTooLarge {
        /// The declared payload length.
        len: usize,
    },
    /// The payload encoder wrote a different number of bytes than the
    /// message type declares.
    WrongLength {
        /// The declared payload length.
        declared: usize,
        /// Bytes actually written.
        written: usize,
    },
    /// A field holds a value that cannot be represented on the wire.
    InvalidValue {
        /// Which field (a static name, for diagnostics).
        field: &'static str,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { needed, available } => {
                write!(
                    f,
                    "buffer too small: needed {needed} bytes, had {available}"
                )
            }
            Self::PayloadTooLarge { len } => write!(f, "payload of {len} bytes is too large"),
            Self::WrongLength { declared, written } => {
                write!(
                    f,
                    "payload encoder wrote {written} bytes, declared {declared}"
                )
            }
            Self::InvalidValue { field } => write!(f, "cannot encode field `{field}`"),
        }
    }
}

impl core::error::Error for EncodeError {}

/// A problem with the ring itself, as opposed to one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RingError {
    /// The memory reports a capacity that is zero, not a power of two, or
    /// above [`MAX_CAPACITY`](crate::MAX_CAPACITY).
    InvalidCapacity {
        /// The capacity reported.
        capacity: u32,
    },
    /// The indices imply more records in flight than the ring can hold. The
    /// other side is faulty or hostile; the ring is not touched further.
    CorruptIndices {
        /// Producer index as read.
        producer: u32,
        /// Consumer index as read.
        consumer: u32,
    },
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity { capacity } => write!(f, "invalid ring capacity {capacity}"),
            Self::CorruptIndices { producer, consumer } => {
                write!(
                    f,
                    "corrupt ring indices: producer {producer}, consumer {consumer}"
                )
            }
        }
    }
}

impl core::error::Error for RingError {}

/// Why [`Producer::try_send`](crate::Producer::try_send) did not send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SendError {
    /// Every slot holds an unconsumed record. Nothing was written; try again
    /// after the consumer has caught up.
    Full,
    /// The message could not be encoded into one slot.
    Encode(EncodeError),
    /// The ring is unusable.
    Ring(RingError),
}

impl From<EncodeError> for SendError {
    fn from(err: EncodeError) -> Self {
        Self::Encode(err)
    }
}

impl From<RingError> for SendError {
    fn from(err: RingError) -> Self {
        Self::Ring(err)
    }
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("ring is full"),
            Self::Encode(err) => write!(f, "encode failed: {err}"),
            Self::Ring(err) => write!(f, "ring failed: {err}"),
        }
    }
}

impl core::error::Error for SendError {}

/// Why [`Consumer::try_recv`](crate::Consumer::try_recv) returned no message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecvError {
    /// A record was received but is malformed. It has been consumed, so one
    /// bad record cannot block the queue.
    Decode(DecodeError),
    /// The ring is unusable.
    Ring(RingError),
}

impl From<DecodeError> for RecvError {
    fn from(err: DecodeError) -> Self {
        Self::Decode(err)
    }
}

impl From<RingError> for RecvError {
    fn from(err: RingError) -> Self {
        Self::Ring(err)
    }
}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "malformed record: {err}"),
            Self::Ring(err) => write!(f, "ring failed: {err}"),
        }
    }
}

impl core::error::Error for RecvError {}
