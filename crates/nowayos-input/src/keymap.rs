// SPDX-License-Identifier: MIT OR Apache-2.0
//! Key-to-character mapping.
//!
//! Provisional and deliberately minimal: a US-English layout, enough for
//! bring-up and host demos. Per-language layouts and the on-screen keyboard
//! are open design work; they will
//! replace this, not extend it.

use crate::KeyCode;

const LETTERS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
const LETTERS_SHIFTED: &[u8; 26] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// Usages 0x1E..=0x27: 1 2 3 4 5 6 7 8 9 0.
const DIGITS: &[u8; 10] = b"1234567890";
const DIGITS_SHIFTED: &[u8; 10] = b"!@#$%^&*()";
/// Usages 0x2D..=0x38 (0x32, the non-US hash key, has no US meaning).
const PUNCTUATION: &[u8; 12] = b"-=[]\\\0;'`,./";
const PUNCTUATION_SHIFTED: &[u8; 12] = b"_+{}|\0:\"~<>?";

/// The character a US-English keyboard produces for `key`, with Shift held
/// or not. `None` for keys that produce no character (Enter, arrows,
/// modifiers, function keys…).
///
/// ```
/// use nowayos_input::{KeyCode, keymap::us_char};
///
/// assert_eq!(us_char(KeyCode::A, false), Some('a'));
/// assert_eq!(us_char(KeyCode::A, true), Some('A'));
/// assert_eq!(us_char(KeyCode::DIGIT_1, true), Some('!'));
/// assert_eq!(us_char(KeyCode::ENTER, false), None);
/// ```
#[must_use]
pub fn us_char(key: KeyCode, shift: bool) -> Option<char> {
    let usage = key.usage();
    let pick = |plain: &[u8], shifted: &[u8], first: u16| -> Option<char> {
        let index = usize::from(usage.checked_sub(first)?);
        let table = if shift { shifted } else { plain };
        let byte = *table.get(index)?;
        (byte != 0).then_some(char::from(byte))
    };
    match usage {
        0x04..=0x1D => pick(LETTERS, LETTERS_SHIFTED, 0x04),
        0x1E..=0x27 => pick(DIGITS, DIGITS_SHIFTED, 0x1E),
        0x2C => Some(' '),
        0x2D..=0x38 => pick(PUNCTUATION, PUNCTUATION_SHIFTED, 0x2D),
        _ => None,
    }
}

/// The key a US-English keyboard types `character` with, and whether Shift
/// is held for it: the inverse of [`us_char`]. `None` for a character no
/// key on that layout types alone (anything but printable ASCII, `' '` to
/// `'~'`).
///
/// ```
/// use nowayos_input::{KeyCode, keymap::us_key};
///
/// assert_eq!(us_key('a'), Some((KeyCode::A, false)));
/// assert_eq!(us_key('A'), Some((KeyCode::A, true)));
/// assert_eq!(us_key('!'), Some((KeyCode::DIGIT_1, true)));
/// assert_eq!(us_key('\n'), None);
/// ```
#[must_use]
pub fn us_key(character: char) -> Option<(KeyCode, bool)> {
    // A match rather than a search of the tables above: the same answers
    // (the tests check every character against `us_char`), at a fixed cost.
    let byte = u8::try_from(character).ok()?;
    let from = |first: u16, base: u8| first.wrapping_add(u16::from(byte.wrapping_sub(base)));
    let (usage, shift) = match byte {
        b'a'..=b'z' => (from(0x04, b'a'), false),
        b'A'..=b'Z' => (from(0x04, b'A'), true),
        b'1'..=b'9' => (from(0x1E, b'1'), false),
        b'0' => (0x27, false),
        b'!' => (0x1E, true),
        b'@' => (0x1F, true),
        b'#' => (0x20, true),
        b'$' => (0x21, true),
        b'%' => (0x22, true),
        b'^' => (0x23, true),
        b'&' => (0x24, true),
        b'*' => (0x25, true),
        b'(' => (0x26, true),
        b')' => (0x27, true),
        b' ' => (0x2C, false),
        b'-' => (0x2D, false),
        b'_' => (0x2D, true),
        b'=' => (0x2E, false),
        b'+' => (0x2E, true),
        b'[' => (0x2F, false),
        b'{' => (0x2F, true),
        b']' => (0x30, false),
        b'}' => (0x30, true),
        b'\\' => (0x31, false),
        b'|' => (0x31, true),
        b';' => (0x33, false),
        b':' => (0x33, true),
        b'\'' => (0x34, false),
        b'"' => (0x34, true),
        b'`' => (0x35, false),
        b'~' => (0x35, true),
        b',' => (0x36, false),
        b'<' => (0x36, true),
        b'.' => (0x37, false),
        b'>' => (0x37, true),
        b'/' => (0x38, false),
        b'?' => (0x38, true),
        _ => return None,
    };
    Some((KeyCode::new(usage)?, shift))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_shift() {
        assert_eq!(us_char(KeyCode::letter('q').unwrap(), false), Some('q'));
        assert_eq!(us_char(KeyCode::Z, true), Some('Z'));
    }

    #[test]
    fn digits_row() {
        assert_eq!(us_char(KeyCode::digit(7).unwrap(), false), Some('7'));
        assert_eq!(us_char(KeyCode::DIGIT_0, false), Some('0'));
        assert_eq!(us_char(KeyCode::DIGIT_0, true), Some(')'));
    }

    #[test]
    fn punctuation_and_space() {
        assert_eq!(us_char(KeyCode::SPACE, true), Some(' '));
        assert_eq!(us_char(KeyCode::SLASH, true), Some('?'));
        assert_eq!(us_char(KeyCode::APOSTROPHE, true), Some('"'));
        assert_eq!(us_char(KeyCode::BACKSLASH, false), Some('\\'));
        // The non-US hash key produces nothing on a US layout.
        assert_eq!(us_char(KeyCode::new(0x32).unwrap(), false), None);
    }

    #[test]
    fn every_printable_character_has_one_key_and_types_back() {
        for byte in 0x20u8..=0x7E {
            let character = char::from(byte);
            let (key, shift) = us_key(character).unwrap();
            assert_eq!(us_char(key, shift), Some(character), "{character:?}");
        }
        for character in ['\0', '\t', '\r', '\n', '\x1b', '\x7f', '\u{80}', 'é', '€'] {
            assert_eq!(us_key(character), None, "{character:?}");
        }
    }

    #[test]
    fn non_printing_keys() {
        for key in [
            KeyCode::ENTER,
            KeyCode::BACKSPACE,
            KeyCode::TAB,
            KeyCode::ESCAPE,
            KeyCode::LEFT,
            KeyCode::LEFT_SHIFT,
            KeyCode::F1,
        ] {
            assert_eq!(us_char(key, false), None, "{key:?}");
        }
    }
}
