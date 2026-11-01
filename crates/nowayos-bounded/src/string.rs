// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`BoundedStr`]: a UTF-8 string with a compile-time capacity in bytes.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::Deref;
use core::str::FromStr;

use crate::{CapacityError, FromUtf8Error};

/// A UTF-8 string of at most `N` bytes, stored inline.
///
/// The contents are valid UTF-8 at all times: every way in is either a `&str`,
/// a `char`, or bytes that are validated first ([`from_utf8`](Self::from_utf8)).
/// Operations that would exceed `N` bytes fail and leave the string unchanged.
///
/// Capacity is counted in **bytes**, not characters: a 16-byte string holds
/// sixteen ASCII letters but only four 4-byte emoji.
///
/// The type is deliberately `Clone` but not `Copy`, so duplicates of a value
/// (such as something a person typed into a kiosk) are only made on purpose.
///
/// ```
/// use nowayos_bounded::BoundedStr;
///
/// let mut label: BoundedStr<8> = BoundedStr::try_from_str("Room").unwrap();
/// label.push_str(" 12").unwrap();
/// assert_eq!(label.as_str(), "Room 12");
/// assert!(label.push_str(" and more").is_err()); // would need 16 bytes
/// assert_eq!(label.pop(), Some('2'));
/// ```
#[derive(Clone)]
pub struct BoundedStr<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> BoundedStr<N> {
    /// The maximum length in bytes, `N`.
    pub const CAPACITY: usize = N;

    /// Creates an empty string.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    /// Creates a string holding a copy of `s`.
    ///
    /// # Errors
    ///
    /// [`CapacityError`] if `s` is longer than `N` bytes.
    pub fn try_from_str(s: &str) -> Result<Self, CapacityError> {
        let mut out = Self::new();
        out.push_str(s)?;
        Ok(out)
    }

    /// Creates a string from the longest prefix of `s` that fits in `N`
    /// bytes, cut at a character boundary. Useful for labels, where showing
    /// the start of an over-long text beats showing nothing.
    #[must_use]
    pub fn from_str_truncated(s: &str) -> Self {
        let mut end = s.len().min(N);
        while !s.is_char_boundary(end) {
            // `end` > 0 here: index 0 is always a boundary.
            end = end.saturating_sub(1);
        }
        let mut out = Self::new();
        if let Some(prefix) = s.get(..end) {
            // Cannot fail: `prefix` is at most `N` bytes.
            let _ = out.push_str(prefix);
        }
        out
    }

    /// Validates `bytes` as UTF-8 and copies them in.
    ///
    /// Oversized input is rejected before it is validated or copied.
    ///
    /// # Errors
    ///
    /// [`FromUtf8Error::TooLong`] if `bytes` is longer than `N`;
    /// [`FromUtf8Error::InvalidUtf8`] if it is not valid UTF-8.
    pub fn from_utf8(bytes: &[u8]) -> Result<Self, FromUtf8Error> {
        if bytes.len() > N {
            return Err(FromUtf8Error::TooLong(CapacityError {
                capacity: N,
                required: bytes.len(),
            }));
        }
        let s = core::str::from_utf8(bytes).map_err(|e| FromUtf8Error::InvalidUtf8 {
            valid_up_to: e.valid_up_to(),
        })?;
        Self::try_from_str(s).map_err(FromUtf8Error::TooLong)
    }

    /// The contents as a string slice.
    ///
    /// Re-checks UTF-8 on every call (O(length)) because this crate uses no
    /// `unsafe`; the check cannot fail, since every mutation preserves
    /// validity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let bytes = self.as_bytes();
        match core::str::from_utf8(bytes) {
            Ok(s) => s,
            // Unreachable by construction; degrade to the valid prefix rather
            // than panic.
            Err(e) => bytes
                .get(..e.valid_up_to())
                .and_then(|valid| core::str::from_utf8(valid).ok())
                .unwrap_or(""),
        }
    }

    /// The contents as bytes (always valid UTF-8).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the string is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The maximum length in bytes, `N`.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// How many more bytes fit.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        N.saturating_sub(self.len)
    }

    /// Appends one character.
    ///
    /// # Errors
    ///
    /// [`CapacityError`] if its UTF-8 encoding does not fit.
    pub fn push(&mut self, ch: char) -> Result<(), CapacityError> {
        let mut buf = [0u8; 4];
        self.push_str(ch.encode_utf8(&mut buf))
    }

    /// Appends all of `s`, or none of it.
    ///
    /// # Errors
    ///
    /// [`CapacityError`] if `s` does not fit; the string is unchanged.
    pub fn push_str(&mut self, s: &str) -> Result<(), CapacityError> {
        let required = self.len.saturating_add(s.len());
        let error = CapacityError {
            capacity: N,
            required,
        };
        if required > N {
            return Err(error);
        }
        let target = self.bytes.get_mut(self.len..required).ok_or(error)?;
        target.copy_from_slice(s.as_bytes());
        self.len = required;
        Ok(())
    }

    /// Removes and returns the last character, zeroing its bytes. Returns
    /// `None` if the string is empty. This is what a backspace key does.
    pub fn pop(&mut self) -> Option<char> {
        let ch = self.as_str().chars().next_back()?;
        let new_len = self.len.checked_sub(ch.len_utf8())?;
        if let Some(removed) = self.bytes.get_mut(new_len..self.len) {
            removed.fill(0);
        }
        self.len = new_len;
        keep_stores(&self.bytes);
        Some(ch)
    }

    /// Empties the string, zeroing every byte of storage. See
    /// [`Wipe`](crate::Wipe) for what that does and does not guarantee.
    pub fn clear(&mut self) {
        self.bytes.fill(0);
        self.len = 0;
        keep_stores(&self.bytes);
    }

    /// Whether the string is in the state a wipe leaves: empty, with every
    /// byte of its storage zero. [`clear`](Self::clear) and
    /// [`Wipe::wipe`](crate::Wipe::wipe) leave it so, and a new string
    /// starts so.
    ///
    /// Unlike [`is_empty`](Self::is_empty), it reads all `N` bytes, so a
    /// test can prove that what a string held was zeroed, not merely
    /// forgotten - through a shared reference, without write access.
    ///
    /// Safe to ship as well as to test: it only reads the string's own
    /// storage and returns one bit, and the bytes beyond the length are
    /// always zero ([`unused_is_zeroed`](Self::unused_is_zeroed)), so it
    /// tells nothing that [`as_bytes`](Self::as_bytes) does not. Copies made
    /// earlier, by moves or clones, are out of its sight (see
    /// [`Wipe`](crate::Wipe)).
    ///
    /// ```
    /// use nowayos_bounded::{BoundedStr, Wipe};
    ///
    /// let mut answer: BoundedStr<16> = BoundedStr::try_from_str("synthetic").unwrap();
    /// assert!(!answer.is_zeroed());
    /// answer.wipe();
    /// assert!(answer.is_zeroed()); // all 16 bytes are zero, not just the length
    /// ```
    #[must_use]
    pub fn is_zeroed(&self) -> bool {
        self.len == 0 && all_zero(&self.bytes)
    }

    /// Whether every byte of storage beyond the length is zero. Always
    /// true: pushing writes only up to the new length, and popping and
    /// clearing zero the bytes they let go of, so a test may check it after
    /// any operation. Safe to ship, like [`is_zeroed`](Self::is_zeroed).
    #[must_use]
    pub fn unused_is_zeroed(&self) -> bool {
        self.bytes.get(self.len..).is_some_and(all_zero)
    }
}

/// Whether every byte is zero.
fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&byte| byte == 0)
}

/// Asks the compiler to treat `bytes` as read after they were zeroed, so it
/// keeps the zeroing stores even when nothing else reads the string again
/// (for example when it is dropped straight after). Best effort: see
/// [`core::hint::black_box`].
fn keep_stores<const N: usize>(bytes: &[u8; N]) {
    core::hint::black_box(bytes);
}

impl<const N: usize> Default for BoundedStr<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Deref for BoundedStr<N> {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> AsRef<str> for BoundedStr<N> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> fmt::Debug for BoundedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> fmt::Display for BoundedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lets `write!` format into a bounded string without allocating.
///
/// Each `write_str` call is all-or-nothing, but a `write!` with several
/// pieces may stop part-way: on `Err`, the pieces before the one that did not
/// fit remain. Call [`BoundedStr::clear`] if you need all-or-nothing.
impl<const N: usize> fmt::Write for BoundedStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s).map_err(|_| fmt::Error)
    }
}

impl<const N: usize> PartialEq for BoundedStr<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl<const N: usize> Eq for BoundedStr<N> {}

impl<const N: usize> PartialEq<str> for BoundedStr<N> {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl<const N: usize> PartialEq<&str> for BoundedStr<N> {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl<const N: usize> PartialOrd for BoundedStr<N> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: usize> Ord for BoundedStr<N> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl<const N: usize> Hash for BoundedStr<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl<const N: usize> TryFrom<&str> for BoundedStr<N> {
    type Error = CapacityError;

    fn try_from(s: &str) -> Result<Self, CapacityError> {
        Self::try_from_str(s)
    }
}

impl<const N: usize> FromStr for BoundedStr<N> {
    type Err = CapacityError;

    fn from_str(s: &str) -> Result<Self, CapacityError> {
        Self::try_from_str(s)
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
    use core::fmt::Write as _;

    #[test]
    fn capacity_is_in_bytes() {
        let s: BoundedStr<4> = BoundedStr::try_from_str("abcd").unwrap();
        assert_eq!(s.len(), 4);
        assert!(BoundedStr::<4>::try_from_str("abcde").is_err());
        // 'é' is two bytes: two fit, a third does not.
        let mut t: BoundedStr<4> = BoundedStr::new();
        t.push('é').unwrap();
        t.push('é').unwrap();
        assert_eq!(
            t.push('é'),
            Err(CapacityError {
                capacity: 4,
                required: 6
            })
        );
        assert_eq!(t.as_str(), "éé");
    }

    #[test]
    fn push_str_is_all_or_nothing() {
        let mut s: BoundedStr<5> = BoundedStr::try_from_str("ab").unwrap();
        assert!(s.push_str("cdef").is_err());
        assert_eq!(s.as_str(), "ab");
        s.push_str("cde").unwrap();
        assert_eq!(s, "abcde");
    }

    #[test]
    fn pop_removes_whole_characters_and_zeroes_them() {
        let mut s: BoundedStr<8> = BoundedStr::try_from_str("aé€").unwrap();
        assert_eq!(s.pop(), Some('€'));
        assert_eq!(s.as_str(), "aé");
        assert_eq!(s.bytes[3..], [0; 5]);
        assert_eq!(s.pop(), Some('é'));
        assert_eq!(s.pop(), Some('a'));
        assert_eq!(s.pop(), None);
        assert_eq!(s.bytes, [0; 8]);
    }

    #[test]
    fn clear_zeroes_storage() {
        let mut s: BoundedStr<6> = BoundedStr::try_from_str("secret").unwrap();
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.bytes, [0; 6]);
    }

    #[test]
    fn the_zeroed_checks_read_every_byte_of_storage() {
        let mut s: BoundedStr<6> = BoundedStr::new();
        assert!(s.is_zeroed() && s.unused_is_zeroed());
        s.push_str("abc").unwrap();
        assert!(!s.is_zeroed());
        assert!(s.unused_is_zeroed());
        // Empty is not enough: a stale byte beyond the length is caught,
        // whichever byte it is.
        for stale in 0..6 {
            let mut forgotten: BoundedStr<6> = BoundedStr::new();
            forgotten.bytes[stale] = b'x';
            assert!(forgotten.is_empty());
            assert!(!forgotten.is_zeroed(), "byte {stale}");
            assert!(!forgotten.unused_is_zeroed(), "byte {stale}");
        }
        // NUL characters are text: zero bytes, but not a wiped string.
        let nul: BoundedStr<6> = BoundedStr::try_from_str("\0\0").unwrap();
        assert!(!nul.is_zeroed());
        assert!(nul.unused_is_zeroed());
        let full: BoundedStr<3> = BoundedStr::try_from_str("abc").unwrap();
        assert!(full.unused_is_zeroed(), "nothing beyond a full string");
        assert!(BoundedStr::<0>::new().is_zeroed());
    }

    #[test]
    fn from_utf8_rejects_oversize_before_validating() {
        // Too long *and* invalid: length is reported, because it is checked first.
        let err = BoundedStr::<2>::from_utf8(&[0xff, 0xff, 0xff]).unwrap_err();
        assert_eq!(
            err,
            FromUtf8Error::TooLong(CapacityError {
                capacity: 2,
                required: 3
            })
        );
        let err = BoundedStr::<8>::from_utf8(&[b'o', b'k', 0xc3]).unwrap_err();
        assert_eq!(err, FromUtf8Error::InvalidUtf8 { valid_up_to: 2 });
        let ok = BoundedStr::<8>::from_utf8("né".as_bytes()).unwrap();
        assert_eq!(ok, "né");
    }

    #[test]
    fn truncation_cuts_at_char_boundary() {
        // "héllo": 'é' occupies bytes 1..3, so a 2-byte cut must back off to 1.
        let s: BoundedStr<2> = BoundedStr::from_str_truncated("héllo");
        assert_eq!(s.as_str(), "h");
        let s: BoundedStr<3> = BoundedStr::from_str_truncated("héllo");
        assert_eq!(s.as_str(), "hé");
        let s: BoundedStr<16> = BoundedStr::from_str_truncated("short");
        assert_eq!(s.as_str(), "short");
        let s: BoundedStr<0> = BoundedStr::from_str_truncated("x");
        assert!(s.is_empty());
    }

    #[test]
    fn write_macro_formats_without_allocating() {
        let mut s: BoundedStr<16> = BoundedStr::new();
        write!(s, "step {} of {}", 2, 4).unwrap();
        assert_eq!(s.as_str(), "step 2 of 4");
        assert!(write!(s, " and far too much more").is_err());
    }

    #[test]
    fn ordering_and_parsing() {
        let a: BoundedStr<4> = "abc".parse().unwrap();
        let b = BoundedStr::<4>::try_from("abd").unwrap();
        assert!(a < b);
        assert_eq!(std::format!("{a}"), "abc");
        assert_eq!(std::format!("{a:?}"), "\"abc\"");
        assert!(a.starts_with("ab")); // via Deref<Target = str>
    }
}
