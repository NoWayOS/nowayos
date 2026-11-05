// SPDX-License-Identifier: MIT OR Apache-2.0
//! Key-to-character mapping: the keyboard layouts a system names (#10081).
//!
//! A [`Keymap`] turns the canonical key events a UI receives into the
//! characters they type on its [`Layout`] - US, UK or US International -
//! with Shift, Caps Lock, AltGr (the right Alt key, on UK and US
//! International) and dead keys, which put an accent on the letter typed
//! next. On UK and US International every printable Latin-1 character can
//! be typed; nothing outside Latin-1 is, since NoWayOS's glyph atlases draw
//! no more (so a UK keyboard's AltGr+4 types `¤`, not the euro sign).
//!
//! [`us_char`] and [`us_key`], the US layout without state, stay for the
//! on-screen keyboard's keys and the serial console's characters.

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

/// A keyboard layout: what each key types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    /// US English: no AltGr and no dead keys; the right Alt key is Alt.
    #[default]
    Us,
    /// UK English: `"` on Shift+2, `£` on Shift+3, `@` on Shift+', `#` and
    /// `~` beside Enter, `\` and `|` beside the left Shift, `¬` on Shift+`;
    /// AltGr types the rest of Latin-1 - `á` `é` `í` `ó` `ú` directly - and
    /// AltGr with `` ` ``, `'`, `2`, `6` or `#` is a dead grave, acute,
    /// diaeresis, circumflex or tilde.
    Uk,
    /// US International: `'` `"` `` ` `` `~` `^` are dead keys (a space
    /// after one types it plainly), and AltGr types the rest of Latin-1.
    UsInternational,
}

impl Layout {
    /// Every layout.
    pub const ALL: [Self; 3] = [Self::Us, Self::Uk, Self::UsInternational];

    /// Whether the right Alt key is AltGr.
    #[must_use]
    pub const fn has_altgr(self) -> bool {
        !matches!(self, Self::Us)
    }
}

/// The accent a dead key puts on the letter typed after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Accent {
    /// `à`.
    Grave,
    /// `á`; on US International, `'` then `c` types `ç` too.
    Acute,
    /// `â`.
    Circumflex,
    /// `ã`, `ñ`.
    Tilde,
    /// `ä`.
    Diaeresis,
}

impl Accent {
    /// `letter` with this accent, if Latin-1 has it.
    #[must_use]
    pub fn on(self, letter: char) -> Option<char> {
        let (plain, accented): (&str, &str) = match self {
            Self::Grave => ("aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
            Self::Acute => ("aeiouycAEIOUYC", "áéíóúýçÁÉÍÓÚÝÇ"),
            Self::Circumflex => ("aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
            Self::Tilde => ("anoANO", "ãñõÃÑÕ"),
            Self::Diaeresis => ("aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
        };
        let at = plain.chars().position(|ch| ch == letter)?;
        accented.chars().nth(at)
    }
}

/// What one key does at one level of a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Glyph {
    /// Types a character.
    Char(char),
    /// A dead key: an accent for the next letter, and what it types
    /// before a space, or before a letter that takes no such accent.
    Dead(Accent, char),
}

/// The characters one key typed: none, one, or - a dead key followed by a
/// letter its accent does not go on - two. Its `Debug` shows how many,
/// never which: what is typed may be sensitive.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Typed {
    first: Option<char>,
    second: Option<char>,
}

impl core::fmt::Debug for Typed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Typed")
            .field("chars", &self.chars().count())
            .finish()
    }
}

impl Typed {
    /// Nothing typed.
    pub const NONE: Self = Self {
        first: None,
        second: None,
    };

    /// `ch` typed, if any.
    #[must_use]
    pub const fn one(ch: Option<char>) -> Self {
        Self {
            first: ch,
            second: None,
        }
    }

    /// `first`, then `second`.
    #[must_use]
    pub const fn two(first: char, second: char) -> Self {
        Self {
            first: Some(first),
            second: Some(second),
        }
    }

    /// The characters, in order.
    pub fn chars(self) -> impl Iterator<Item = char> {
        self.first.into_iter().chain(self.second)
    }

    /// Whether nothing was typed.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.first.is_none() && self.second.is_none()
    }
}

/// A keyboard's state on a layout: the modifiers held, Caps Lock, and a
/// dead key waiting for its letter. Feed it every key event, in order;
/// [`key_down`](Self::key_down) says what each key typed. Its `Debug`
/// shows whether a dead key waits, never which.
#[derive(Clone, PartialEq, Eq)]
pub struct Keymap {
    layout: Layout,
    shift: [bool; 2],
    ctrl: [bool; 2],
    alt: [bool; 2],
    altgr: bool,
    caps: bool,
    dead: Option<(Accent, char)>,
}

impl core::fmt::Debug for Keymap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Keymap")
            .field("layout", &self.layout)
            .field("caps", &self.caps)
            .field("dead", &self.dead.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(Layout::Us)
    }
}

impl Keymap {
    /// A keyboard on `layout`, nothing held, Caps Lock off.
    #[must_use]
    pub const fn new(layout: Layout) -> Self {
        Self {
            layout,
            shift: [false; 2],
            ctrl: [false; 2],
            alt: [false; 2],
            altgr: false,
            caps: false,
            dead: None,
        }
    }

    /// Its layout.
    #[must_use]
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// Changes to `layout`, dropping a dead key that waits.
    pub const fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.altgr = false;
        self.dead = None;
    }

    /// Whether Caps Lock is on.
    #[must_use]
    pub const fn caps_lock(&self) -> bool {
        self.caps
    }

    /// Drops a dead key that waits: when focus moves, an accent is not
    /// carried to another field.
    pub const fn cancel(&mut self) {
        self.dead = None;
    }

    /// `key` pressed: what it types. A modifier, Caps Lock, a key pressed
    /// with Ctrl or Alt, or a key that types nothing (Enter, the arrows…)
    /// types nothing; any but a modifier drops a dead key that waits.
    ///
    /// ```
    /// use nowayos_input::KeyCode;
    /// use nowayos_input::keymap::{Keymap, Layout};
    ///
    /// let mut keys = Keymap::new(Layout::UsInternational);
    /// // ' is a dead key: then e types é.
    /// assert!(keys.key_down(KeyCode::APOSTROPHE).is_empty());
    /// assert_eq!(keys.key_down(KeyCode::letter('e').unwrap()).chars().next(), Some('é'));
    /// ```
    pub fn key_down(&mut self, key: KeyCode) -> Typed {
        if self.modifier(key, true) {
            return Typed::NONE;
        }
        if key == KeyCode::CAPS_LOCK {
            self.caps = !self.caps;
            return Typed::NONE;
        }
        if self.ctrl.contains(&true) || self.alt.contains(&true) {
            return Typed::NONE;
        }
        let glyph = self.glyph(key);
        match (self.dead.take(), glyph) {
            (_, None) => Typed::NONE,
            (None, Some(Glyph::Char(ch))) => Typed::one(Some(ch)),
            (None, Some(Glyph::Dead(accent, alone))) => {
                self.dead = Some((accent, alone));
                Typed::NONE
            }
            (Some((_, alone)), Some(Glyph::Char(' '))) => Typed::one(Some(alone)),
            (Some((accent, alone)), Some(Glyph::Char(ch))) => accent
                .on(ch)
                .map_or(Typed::two(alone, ch), |ch| Typed::one(Some(ch))),
            // The same dead key twice types its character once; another
            // types the first's and waits in its place.
            (Some((first, alone)), Some(Glyph::Dead(accent, next))) => {
                if first != accent {
                    self.dead = Some((accent, next));
                }
                Typed::one(Some(alone))
            }
        }
    }

    /// `key` released.
    pub fn key_up(&mut self, key: KeyCode) {
        self.modifier(key, false);
    }

    /// Records a modifier going `down` or up: whether `key` is one.
    fn modifier(&mut self, key: KeyCode, down: bool) -> bool {
        let held = match key {
            KeyCode::LEFT_SHIFT => self.shift.get_mut(0),
            KeyCode::RIGHT_SHIFT => self.shift.get_mut(1),
            KeyCode::LEFT_CTRL => self.ctrl.get_mut(0),
            KeyCode::RIGHT_CTRL => self.ctrl.get_mut(1),
            KeyCode::LEFT_ALT => self.alt.get_mut(0),
            KeyCode::RIGHT_ALT if self.layout.has_altgr() => {
                self.altgr = down;
                return true;
            }
            KeyCode::RIGHT_ALT => self.alt.get_mut(1),
            _ => return key.is_modifier(),
        };
        if let Some(held) = held {
            *held = down;
        }
        true
    }

    /// What `key` does with the modifiers held and Caps Lock: Caps Lock
    /// changes the case of what a letter key types.
    fn glyph(&self, key: KeyCode) -> Option<Glyph> {
        let shift = self.shift.contains(&true);
        let glyph = if self.altgr {
            altgr(self.layout, key.usage(), shift)
        } else {
            base(self.layout, key, shift)
        };
        match glyph {
            Some(Glyph::Char(ch)) if self.caps && matches!(key.usage(), 0x04..=0x1D) => {
                Some(Glyph::Char(other_case(ch)))
            }
            glyph => glyph,
        }
    }
}

/// A Latin-1 letter in the other case; any other character as it is (`ß`
/// and `ÿ` have no capital in Latin-1).
const fn other_case(ch: char) -> char {
    let code = ch as u32;
    let other = match code {
        0x61..=0x7A | 0xE0..=0xFE if code != 0xF7 => code.wrapping_sub(0x20),
        0x41..=0x5A | 0xC0..=0xDE if code != 0xD7 => code.wrapping_add(0x20),
        _ => code,
    };
    match char::from_u32(other) {
        Some(other) => other,
        None => ch,
    }
}

/// What `key` types without AltGr.
fn base(layout: Layout, key: KeyCode, shift: bool) -> Option<Glyph> {
    let pick = |plain: char, shifted: char| Some(Glyph::Char(if shift { shifted } else { plain }));
    match (layout, key.usage()) {
        (Layout::UsInternational, 0x34) if shift => Some(Glyph::Dead(Accent::Diaeresis, '"')),
        (Layout::UsInternational, 0x34) => Some(Glyph::Dead(Accent::Acute, '\'')),
        (Layout::UsInternational, 0x35) if shift => Some(Glyph::Dead(Accent::Tilde, '~')),
        (Layout::UsInternational, 0x35) => Some(Glyph::Dead(Accent::Grave, '`')),
        (Layout::UsInternational, 0x23) if shift => Some(Glyph::Dead(Accent::Circumflex, '^')),
        (Layout::Uk, 0x1F) => pick('2', '"'),
        (Layout::Uk, 0x20) => pick('3', '£'),
        (Layout::Uk, 0x34) => pick('\'', '@'),
        (Layout::Uk, 0x35) => pick('`', '¬'),
        // The key beside Enter: HID's non-US hash key, which Linux, and so
        // a virtio keyboard, reports as the US backslash.
        (Layout::Uk, 0x31 | 0x32) => pick('#', '~'),
        // The key beside the left Shift on an ISO keyboard.
        (Layout::Uk, 0x64) => pick('\\', '|'),
        _ => us_char(key, shift).map(Glyph::Char),
    }
}

/// What the key with HID usage `usage` types with AltGr, Shift held or
/// not: the rest of Latin-1, on US International as Windows places it
/// (with the Latin-1 characters it lacks on keys it leaves free), and on UK
/// the same but for its dead keys.
fn altgr(layout: Layout, usage: u16, shift: bool) -> Option<Glyph> {
    use Glyph::{Char, Dead};
    let pick =
        |plain: Option<char>, shifted: Option<char>| if shift { shifted } else { plain }.map(Char);
    if layout == Layout::Uk {
        match (usage, shift) {
            (0x35, false) => return Some(Dead(Accent::Grave, '`')),
            (0x35, true) => return Some(Char('¦')),
            (0x34, false) => return Some(Dead(Accent::Acute, '´')),
            (0x34, true) => return None,
            (0x1F, false) => return Some(Dead(Accent::Diaeresis, '¨')),
            (0x1F, true) => return Some(Char('²')),
            (0x23, false) => return Some(Dead(Accent::Circumflex, '^')),
            (0x23, true) => return Some(Char('¼')),
            (0x31 | 0x32, false) => return Some(Dead(Accent::Tilde, '~')),
            (0x31 | 0x32, true) => return None,
            _ => {}
        }
    }
    match usage {
        0x04 => pick(Some('á'), Some('Á')),
        0x06 => pick(Some('©'), Some('¢')),
        0x07 => pick(Some('ð'), Some('Ð')),
        0x08 => pick(Some('é'), Some('É')),
        0x0C => pick(Some('í'), Some('Í')),
        0x0F => pick(Some('ø'), Some('Ø')),
        0x10 => pick(Some('µ'), None),
        0x11 => pick(Some('ñ'), Some('Ñ')),
        0x12 => pick(Some('ó'), Some('Ó')),
        0x13 => pick(Some('ö'), Some('Ö')),
        0x14 => pick(Some('ä'), Some('Ä')),
        0x15 => pick(Some('®'), None),
        0x16 => pick(Some('ß'), Some('§')),
        0x17 => pick(Some('þ'), Some('Þ')),
        0x18 => pick(Some('ú'), Some('Ú')),
        0x1A => pick(Some('å'), Some('Å')),
        0x1C => pick(Some('ü'), Some('Ü')),
        0x1D => pick(Some('æ'), Some('Æ')),
        0x1E => pick(Some('¡'), Some('¹')),
        0x1F => pick(Some('²'), None),
        0x20 => pick(Some('³'), Some('¯')),
        0x21 => pick(Some('¤'), Some('£')),
        0x23 => pick(Some('¼'), None),
        0x24 => pick(Some('½'), None),
        0x25 => pick(Some('¾'), None),
        0x26 => pick(Some('ª'), None),
        0x27 => pick(Some('º'), None),
        0x2C => pick(Some('\u{A0}'), None),
        0x2D => pick(Some('¥'), Some('±')),
        0x2E => pick(Some('×'), Some('÷')),
        0x2F => pick(Some('«'), None),
        0x30 => pick(Some('»'), None),
        0x31 => pick(Some('¬'), Some('¦')),
        0x33 => pick(Some('¶'), Some('°')),
        0x34 => pick(Some('´'), Some('¨')),
        0x36 => pick(Some('ç'), Some('Ç')),
        0x37 => pick(Some('·'), Some('¸')),
        0x38 => pick(Some('¿'), None),
        _ => None,
    }
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

    /// `usage` pressed and released on `keys`, with Shift and AltGr held
    /// as asked.
    fn press(keys: &mut Keymap, usage: u16, shift: bool, altgr: bool) -> Typed {
        let key = KeyCode::new(usage).unwrap();
        if shift {
            keys.key_down(KeyCode::LEFT_SHIFT);
        }
        if altgr {
            keys.key_down(KeyCode::RIGHT_ALT);
        }
        let typed = keys.key_down(key);
        keys.key_up(key);
        if altgr {
            keys.key_up(KeyCode::RIGHT_ALT);
        }
        if shift {
            keys.key_up(KeyCode::LEFT_SHIFT);
        }
        typed
    }

    fn text(typed: Typed) -> String {
        typed.chars().collect()
    }

    /// Every key that may type: the letters to the slash, and the ISO key.
    fn usages() -> impl Iterator<Item = u16> {
        (0x04..=0x38).chain([0x64])
    }

    #[test]
    fn uk_puts_its_symbols_where_its_keycaps_show_them() {
        let mut keys = Keymap::new(Layout::Uk);
        for (usage, shift, typed) in [
            (0x1F, true, "\""),
            (0x20, true, "£"),
            (0x34, false, "'"),
            (0x34, true, "@"),
            (0x35, true, "¬"),
            (0x31, false, "#"),
            (0x32, true, "~"),
            (0x64, false, "\\"),
            (0x64, true, "|"),
            (0x04, true, "A"),
            (0x23, true, "^"),
        ] {
            assert_eq!(
                text(press(&mut keys, usage, shift, false)),
                typed,
                "{usage:#x}"
            );
        }
        // AltGr: an acute vowel directly, or a dead key then a letter.
        assert_eq!(text(press(&mut keys, 0x04, false, true)), "á");
        assert_eq!(text(press(&mut keys, 0x04, true, true)), "Á");
        assert!(
            press(&mut keys, 0x1F, false, true).is_empty(),
            "AltGr+2: dead diaeresis"
        );
        assert_eq!(text(press(&mut keys, 0x08, false, false)), "ë");
        assert!(
            press(&mut keys, 0x35, false, true).is_empty(),
            "AltGr+`: dead grave"
        );
        assert_eq!(text(press(&mut keys, 0x04, true, false)), "À");
    }

    #[test]
    fn us_international_dead_keys_accent_the_next_letter() {
        let mut keys = Keymap::new(Layout::UsInternational);
        let mut typed = String::new();
        // Siobhán, Zoë, crème, señor, hôtel, ça, then the plain marks.
        for (usage, shift) in [
            (0x16, true),
            (0x0C, false),
            (0x12, false),
            (0x05, false),
            (0x0B, false),
            (0x34, false),
            (0x04, false),
            (0x11, false),
            (0x2C, false),
            (0x1D, true),
            (0x12, false),
            (0x34, true),
            (0x08, false),
            (0x2C, false),
            (0x06, false),
            (0x15, false),
            (0x35, false),
            (0x08, false),
            (0x10, false),
            (0x08, false),
            (0x2C, false),
            (0x16, false),
            (0x08, false),
            (0x35, true),
            (0x11, false),
            (0x12, false),
            (0x15, false),
            (0x2C, false),
            (0x0B, false),
            (0x23, true),
            (0x12, false),
            (0x17, false),
            (0x08, false),
            (0x0F, false),
            (0x2C, false),
            (0x34, false),
            (0x06, false),
            (0x04, false),
            (0x2C, false),
            // ' then space: an apostrophe; " then t: both.
            (0x34, false),
            (0x2C, false),
            (0x34, true),
            (0x17, false),
        ] {
            typed.extend(press(&mut keys, usage, shift, false).chars());
        }
        assert_eq!(typed, "Siobhán Zoë crème señor hôtel ça '\"t");
        // The same dead key twice types it once; another types the first
        // and waits.
        assert_eq!(text(press(&mut keys, 0x35, false, false)), "");
        assert_eq!(text(press(&mut keys, 0x35, false, false)), "`");
        press(&mut keys, 0x35, false, false);
        assert_eq!(text(press(&mut keys, 0x34, false, false)), "`");
        assert_eq!(text(press(&mut keys, 0x08, false, false)), "é");
        // A key that types nothing, or focus moving, drops it.
        press(&mut keys, 0x34, false, false);
        keys.key_down(KeyCode::BACKSPACE);
        assert_eq!(text(press(&mut keys, 0x08, false, false)), "e");
        press(&mut keys, 0x34, false, false);
        keys.cancel();
        assert_eq!(text(press(&mut keys, 0x08, false, false)), "e");
    }

    #[test]
    fn caps_lock_turns_letters_only_and_shift_turns_them_back() {
        for layout in Layout::ALL {
            let mut keys = Keymap::new(layout);
            keys.key_down(KeyCode::CAPS_LOCK);
            keys.key_up(KeyCode::CAPS_LOCK);
            assert!(keys.caps_lock());
            assert_eq!(text(press(&mut keys, 0x04, false, false)), "A");
            assert_eq!(text(press(&mut keys, 0x04, true, false)), "a");
            assert_eq!(text(press(&mut keys, 0x1E, false, false)), "1");
            if layout.has_altgr() {
                assert_eq!(text(press(&mut keys, 0x04, false, true)), "Á");
                assert_eq!(text(press(&mut keys, 0x16, false, true)), "ß");
            }
            keys.key_down(KeyCode::CAPS_LOCK);
            assert_eq!(text(press(&mut keys, 0x04, false, false)), "a");
        }
    }

    #[test]
    fn ctrl_alt_and_us_right_alt_type_nothing() {
        let mut keys = Keymap::new(Layout::Us);
        keys.key_down(KeyCode::LEFT_CTRL);
        assert!(keys.key_down(KeyCode::C).is_empty());
        keys.key_up(KeyCode::LEFT_CTRL);
        // On US the right Alt is Alt.
        assert!(press(&mut keys, 0x04, false, true).is_empty());
        assert_eq!(text(press(&mut keys, 0x04, false, false)), "a");
        // Every US key types what us_char says.
        for usage in usages() {
            for shift in [false, true] {
                let key = KeyCode::new(usage).unwrap();
                assert_eq!(
                    press(&mut keys, usage, shift, false),
                    Typed::one(us_char(key, shift))
                );
            }
        }
    }

    #[test]
    fn every_printable_latin_1_character_can_be_typed_on_uk_and_us_international() {
        use std::collections::BTreeSet;
        let printable = (0x20_u32..=0x7E)
            .chain((0xA0..=0xFF).filter(|&code| code != 0xAD))
            .filter_map(char::from_u32)
            .collect::<BTreeSet<_>>();
        for layout in [Layout::Uk, Layout::UsInternational] {
            let mut typed = BTreeSet::new();
            let levels = [(false, false), (true, false), (false, true), (true, true)];
            for usage in usages() {
                for (shift, altgr) in levels {
                    let mut keys = Keymap::new(layout);
                    let first = press(&mut keys, usage, shift, altgr);
                    typed.extend(first.chars());
                    if !first.is_empty() {
                        continue;
                    }
                    // A dead key, or a key that types nothing: every key after.
                    for next in usages() {
                        for (shift, altgr) in levels {
                            let mut after = keys.clone();
                            typed.extend(press(&mut after, next, shift, altgr).chars());
                        }
                    }
                }
            }
            let missing = printable.difference(&typed).collect::<Vec<_>>();
            assert!(missing.is_empty(), "{layout:?} cannot type {missing:?}");
            let stray = typed.difference(&printable).collect::<Vec<_>>();
            assert!(stray.is_empty(), "{layout:?} types {stray:?}");
        }
    }

    #[test]
    fn debug_never_shows_what_was_typed_or_waits() {
        let mut keys = Keymap::new(Layout::UsInternational);
        press(&mut keys, 0x34, false, false);
        let typed = press(&mut keys, 0x08, false, false);
        press(&mut keys, 0x35, true, false);
        let shown = format!("{keys:?} {typed:?}");
        assert!(!shown.contains('é') && !shown.contains("Tilde"), "{shown}");
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
