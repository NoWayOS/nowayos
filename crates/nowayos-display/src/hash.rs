// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`FrameHash`]: a fingerprint of a frame's bytes, so a test can check a
//! frame a display PD shows against one rendered on the host.
//!
//! FNV-1a, 64 bits: simple, deterministic and the same everywhere. **Not a
//! security measure** - an adversary can make collisions at will; it only
//! catches accidental differences.

/// FNV-1a 64-bit offset basis.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// An FNV-1a 64-bit hash of bytes fed in order, in any number of pieces.
///
/// ```
/// use nowayos_display::FrameHash;
///
/// let mut hash = FrameHash::new();
/// hash.update(b"fr");
/// hash.update(b"ame");
/// assert_eq!(hash.finish(), FrameHash::of(b"frame"));
/// assert_eq!(FrameHash::of(b""), 0xcbf2_9ce4_8422_2325);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHash(u64);

impl Default for FrameHash {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameHash {
    /// The hash of no bytes yet.
    #[must_use]
    pub const fn new() -> Self {
        Self(OFFSET)
    }

    /// Feeds `bytes` in.
    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(PRIME);
        }
    }

    /// The hash of everything fed in.
    #[must_use]
    pub const fn finish(self) -> u64 {
        self.0
    }

    /// The hash of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> u64 {
        let mut hash = Self::new();
        hash.update(bytes);
        hash.finish()
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code: tests index known data")]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_fnv1a_test_vectors() {
        assert_eq!(FrameHash::of(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(FrameHash::of(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn one_changed_byte_changes_the_hash() {
        let mut frame = [0x5A_u8; 64];
        let before = FrameHash::of(&frame);
        frame[63] ^= 1;
        assert_ne!(FrameHash::of(&frame), before);
    }
}
