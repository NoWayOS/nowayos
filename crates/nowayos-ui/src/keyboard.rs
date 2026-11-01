// SPDX-License-Identifier: MIT OR Apache-2.0
//! The on-screen keyboard's layouts:
//! keys drawn as buttons that send canonical key events to the
//! focused text input, laid out for that input's [`InputMode`] - letters
//! with Shift for text, digits for digits, and letters with `@` and `.` for
//! an address. No suggestions; the keyboard holds no copy of what is typed.

use nowayos_input::KeyCode;

use crate::InputMode;

/// How many rows of keys every layout has, so the keyboard keeps its height
/// as focus moves between inputs of different modes.
pub const KEY_ROWS: u8 = 4;

/// What a key does when it is pressed and released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    /// Types a letter's key: its capital while the keyboard's Shift is on,
    /// which then goes off.
    Letter(KeyCode),
    /// Types `key`, Shift held if `shift` (`@` is Shift and 2), whatever
    /// the keyboard's Shift.
    Type {
        /// The key.
        key: KeyCode,
        /// Whether Shift is held with it.
        shift: bool,
    },
    /// Turns Shift on for the next letter, or off again.
    Shift,
}

/// One key: where it sits on its layout's grid, what it does and its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// Its row, from 0 at the top, below [`KEY_ROWS`].
    pub row: u8,
    /// Its first column, from 0 at the left.
    pub column: u8,
    /// How many columns it spans.
    pub span: u8,
    /// What it does.
    pub action: KeyAction,
    /// Its label.
    pub label: &'static str,
    /// Its label while Shift is on: a letter's capital.
    pub shifted: &'static str,
}

impl Key {
    /// Its label, with the keyboard's Shift on or off.
    #[must_use]
    pub const fn label_for(&self, shift: bool) -> &'static str {
        if shift { self.shifted } else { self.label }
    }
}

/// One layout: a grid `columns` wide and [`KEY_ROWS`] high, and its keys,
/// in groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyLayout {
    /// The grid's columns: a letter key spans two of the letter layouts'
    /// twenty, so a row of nine can sit half a key in.
    pub columns: u8,
    groups: &'static [&'static [Key]],
}

impl KeyLayout {
    /// Its keys, in order: a key's place in this order is its index.
    pub fn keys(&self) -> impl Iterator<Item = &'static Key> + use<> {
        let groups: &'static [&'static [Key]] = self.groups;
        groups.iter().flat_map(|group| group.iter())
    }

    /// Key `index`, if it has one.
    #[must_use]
    pub fn key(&self, index: usize) -> Option<&'static Key> {
        self.keys().nth(index)
    }
}

/// The layout for a text input in `mode`.
#[must_use]
pub const fn layout(mode: InputMode) -> &'static KeyLayout {
    match mode {
        InputMode::Text => &TEXT,
        InputMode::Digits => &DIGITS,
        InputMode::Email => &EMAIL,
    }
}

/// A letter's key code; Space for a character that is not a letter, which
/// the tests show no key here holds.
const fn letter_code(letter: char) -> KeyCode {
    match KeyCode::letter(letter) {
        Some(key) => key,
        None => KeyCode::SPACE,
    }
}

/// A digit's key code; Space for one above 9, which no key here holds.
const fn digit_code(digit: u8) -> KeyCode {
    match KeyCode::digit(digit) {
        Some(key) => key,
        None => KeyCode::SPACE,
    }
}

const fn letter(row: u8, column: u8, label: &'static str, shifted: &'static str) -> Key {
    let ch = match label.as_bytes().first() {
        Some(&byte) => byte as char,
        None => ' ',
    };
    Key {
        row,
        column,
        span: 2,
        action: KeyAction::Letter(letter_code(ch)),
        label,
        shifted,
    }
}

const fn typed(
    row: u8,
    column: u8,
    span: u8,
    key: KeyCode,
    shift: bool,
    label: &'static str,
) -> Key {
    Key {
        row,
        column,
        span,
        action: KeyAction::Type { key, shift },
        label,
        shifted: label,
    }
}

const fn digit(row: u8, column: u8, digit: u8, label: &'static str) -> Key {
    typed(row, column, 1, digit_code(digit), false, label)
}

/// The three rows of letters both letter layouts share: Q to P, A to L
/// half a key in, then Shift, Z to M and Delete.
const LETTERS: [Key; 28] = [
    letter(0, 0, "q", "Q"),
    letter(0, 2, "w", "W"),
    letter(0, 4, "e", "E"),
    letter(0, 6, "r", "R"),
    letter(0, 8, "t", "T"),
    letter(0, 10, "y", "Y"),
    letter(0, 12, "u", "U"),
    letter(0, 14, "i", "I"),
    letter(0, 16, "o", "O"),
    letter(0, 18, "p", "P"),
    letter(1, 1, "a", "A"),
    letter(1, 3, "s", "S"),
    letter(1, 5, "d", "D"),
    letter(1, 7, "f", "F"),
    letter(1, 9, "g", "G"),
    letter(1, 11, "h", "H"),
    letter(1, 13, "j", "J"),
    letter(1, 15, "k", "K"),
    letter(1, 17, "l", "L"),
    Key {
        row: 2,
        column: 0,
        span: 3,
        action: KeyAction::Shift,
        label: "Shift",
        shifted: "Shift",
    },
    letter(2, 3, "z", "Z"),
    letter(2, 5, "x", "X"),
    letter(2, 7, "c", "C"),
    letter(2, 9, "v", "V"),
    letter(2, 11, "b", "B"),
    letter(2, 13, "n", "N"),
    letter(2, 15, "m", "M"),
    typed(2, 17, 3, KeyCode::BACKSPACE, false, "Delete"),
];

/// Text: the letters over an apostrophe and a hyphen, for names, the space
/// bar, a comma and a full stop.
const TEXT: KeyLayout = KeyLayout {
    columns: 20,
    groups: &[
        &LETTERS,
        &[
            typed(3, 0, 2, KeyCode::APOSTROPHE, false, "'"),
            typed(3, 2, 2, KeyCode::MINUS, false, "-"),
            typed(3, 4, 12, KeyCode::SPACE, false, "Space"),
            typed(3, 16, 2, KeyCode::COMMA, false, ","),
            typed(3, 18, 2, KeyCode::PERIOD, false, "."),
        ],
    ],
};

/// An address: the letters over a hyphen, an underscore, `@` and `.`, and
/// no space bar - an address has no space.
const EMAIL: KeyLayout = KeyLayout {
    columns: 20,
    groups: &[
        &LETTERS,
        &[
            typed(3, 0, 2, KeyCode::MINUS, false, "-"),
            typed(3, 2, 2, KeyCode::MINUS, true, "_"),
            typed(3, 4, 8, digit_code(2), true, "@"),
            typed(3, 12, 8, KeyCode::PERIOD, false, "."),
        ],
    ],
};

/// Digits: a telephone's pad, 1 to 9 in threes, then 0 and Delete.
const DIGITS: KeyLayout = KeyLayout {
    columns: 3,
    groups: &[&[
        digit(0, 0, 1, "1"),
        digit(0, 1, 2, "2"),
        digit(0, 2, 3, "3"),
        digit(1, 0, 4, "4"),
        digit(1, 1, 5, "5"),
        digit(1, 2, 6, "6"),
        digit(2, 0, 7, "7"),
        digit(2, 1, 8, "8"),
        digit(2, 2, 9, "9"),
        digit(3, 1, 0, "0"),
        typed(3, 2, 1, KeyCode::BACKSPACE, false, "Delete"),
    ]],
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use nowayos_input::keymap;

    const MODES: [InputMode; 3] = [InputMode::Text, InputMode::Digits, InputMode::Email];

    #[test]
    fn every_key_types_what_its_label_shows_in_its_layouts_mode() {
        for mode in MODES {
            for key in layout(mode).keys() {
                for shift in [false, true] {
                    let typed = match key.action {
                        KeyAction::Letter(code) => keymap::us_char(code, shift),
                        KeyAction::Type { key: code, shift } => keymap::us_char(code, shift),
                        KeyAction::Shift => continue,
                    };
                    match typed {
                        Some(' ') => assert_eq!(key.label, "Space"),
                        Some(ch) => {
                            assert_eq!(key.label_for(shift), ch.to_string(), "{mode:?}");
                            assert!(mode.accepts(ch), "{mode:?} refuses {ch:?}");
                        }
                        None => assert_eq!(key.label, "Delete"),
                    }
                }
            }
        }
    }

    #[test]
    fn every_layout_fills_its_grid_without_overlap() {
        for mode in MODES {
            let layout = layout(mode);
            let mut cells =
                std::vec![std::vec![0u8; usize::from(layout.columns)]; usize::from(KEY_ROWS)];
            for key in layout.keys() {
                assert!(key.row < KEY_ROWS && key.span > 0);
                for column in key.column..key.column + key.span {
                    cells[usize::from(key.row)][usize::from(column)] += 1;
                }
            }
            for row in &cells {
                assert!(row.iter().all(|&count| count <= 1), "{mode:?}: overlap");
            }
            // Every row of the letter layouts is full but the second, half
            // a key in at each end; the digits' last row leaves its first
            // place empty.
            let empty: usize = cells.iter().flatten().filter(|&&count| count == 0).count();
            assert_eq!(
                empty,
                if mode == InputMode::Digits { 1 } else { 2 },
                "{mode:?}"
            );
        }
    }

    #[test]
    fn every_label_can_be_drawn() {
        for mode in MODES {
            for key in layout(mode).keys() {
                crate::text::check_printable(key.label).unwrap();
                crate::text::check_printable(key.shifted).unwrap();
            }
        }
    }
}
