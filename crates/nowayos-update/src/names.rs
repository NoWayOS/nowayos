// SPDX-License-Identifier: MIT OR Apache-2.0
//! The product and board names the check-in and deployment records carry,
//! checked by one rule each, as the manifest checks its own: a length from
//! 1 to the field's size, the allowed characters, and zeros after the name.

use crate::manifest::{MAX_BOARD_LEN, MAX_PRODUCT_LEN, is_board_character, is_product_character};

/// The size of a name field: the longer of the two names.
pub(crate) const NAME_LEN: usize = if MAX_PRODUCT_LEN > MAX_BOARD_LEN {
    MAX_PRODUCT_LEN
} else {
    MAX_BOARD_LEN
};

/// Which name a field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Name {
    /// A product: lowercase ASCII letters, digits and `-`.
    Product,
    /// A board: also `_`.
    Board,
}

/// What is wrong with a name field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameFault {
    /// No character, or more than the name's most.
    Length(usize),
    /// A character not allowed, at its place from 0.
    Character {
        /// Its place in the name, from 0.
        at: usize,
        /// The byte.
        byte: u8,
    },
    /// A byte after the name that is not zero.
    NotZero,
}

impl Name {
    const fn max(self) -> usize {
        match self {
            Self::Product => MAX_PRODUCT_LEN,
            Self::Board => MAX_BOARD_LEN,
        }
    }

    const fn allows(self, byte: u8) -> bool {
        match self {
            Self::Product => is_product_character(byte),
            Self::Board => is_board_character(byte),
        }
    }

    /// The name's length, if it is from 1 to the name's most.
    pub(crate) fn check_len(self, len: usize) -> Result<usize, NameFault> {
        if (1..=self.max()).contains(&len) {
            Ok(len)
        } else {
            Err(NameFault::Length(len))
        }
    }

    /// Checks a field: the first `len` bytes allowed characters, every byte
    /// after them zero, over the whole field, in byte order.
    pub(crate) fn check_field(self, len: usize, field: &[u8; NAME_LEN]) -> Result<(), NameFault> {
        for (at, byte) in field.iter().enumerate() {
            if at < len {
                if !self.allows(*byte) {
                    return Err(NameFault::Character { at, byte: *byte });
                }
            } else if *byte != 0 {
                return Err(NameFault::NotZero);
            }
        }
        Ok(())
    }

    /// The field for `name`, and its length byte.
    pub(crate) fn field(self, name: &str) -> Result<(u8, [u8; NAME_LEN]), NameFault> {
        let len = self.check_len(name.len())?;
        let mut field = [0u8; NAME_LEN];
        for (slot, byte) in field.iter_mut().zip(name.as_bytes()) {
            *slot = *byte;
        }
        self.check_field(len, &field)?;
        Ok((u8::try_from(len).unwrap_or(u8::MAX), field))
    }
}

/// The name a checked field holds: its first `len` bytes.
pub(crate) fn as_str(field: &[u8], len: u8) -> &str {
    field
        .get(..usize::from(len))
        .and_then(|bytes| core::str::from_utf8(bytes).ok())
        .unwrap_or_default()
}
