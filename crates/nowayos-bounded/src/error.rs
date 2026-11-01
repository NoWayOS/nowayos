// SPDX-License-Identifier: MIT OR Apache-2.0
//! Error types shared by the bounded containers.

use core::fmt;

/// An operation needed more room than a bounded container has.
///
/// The container is left exactly as it was before the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    /// The container's total capacity (items, or bytes for a string).
    pub capacity: usize,
    /// The size the operation would have needed, saturating at `usize::MAX`.
    pub required: usize,
}

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bounded capacity exceeded: needed {}, capacity {}",
            self.required, self.capacity
        )
    }
}

impl core::error::Error for CapacityError {}

/// Bytes could not be turned into a [`BoundedStr`](crate::BoundedStr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromUtf8Error {
    /// The input is longer than the string's capacity. It is rejected before
    /// anything is validated or copied.
    TooLong(CapacityError),
    /// The input is not valid UTF-8.
    InvalidUtf8 {
        /// Length in bytes of the longest valid UTF-8 prefix.
        valid_up_to: usize,
    },
}

impl fmt::Display for FromUtf8Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(err) => write!(f, "string too long: {err}"),
            Self::InvalidUtf8 { valid_up_to } => {
                write!(f, "invalid UTF-8 after {valid_up_to} valid bytes")
            }
        }
    }
}

impl core::error::Error for FromUtf8Error {}
