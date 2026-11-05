// SPDX-License-Identifier: MIT OR Apache-2.0
//! The canonical events and their wire encoding.

use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

/// The message kind of [`InputEvent`] records on the input channel.
pub const INPUT_EVENT_KIND: u16 = 0x0100;

/// A key, numbered as in the USB HID Usage Tables' Keyboard/Keypad page
/// (page 0x07).
///
/// HID numbering is used because every input path we plan — USB HID
/// directly, or a sanitising microcontroller that parses HID for us — maps
/// onto it without a translation table, and it is publicly specified. Only
/// the defined keyboard usages are accepted: `0x04..=0xA4` (letters to
/// ExSel) and the eight modifiers `0xE0..=0xE7`. The "no event" and error
/// codes `0x00..=0x03` and the reserved ranges are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyCode(u16);

impl KeyCode {
    /// The key with HID usage ID `usage`, or `None` if `usage` is not a
    /// defined keyboard key.
    #[must_use]
    pub const fn new(usage: u16) -> Option<Self> {
        match usage {
            0x04..=0xA4 | 0xE0..=0xE7 => Some(Self(usage)),
            _ => None,
        }
    }

    /// The HID usage ID.
    #[must_use]
    pub const fn usage(self) -> u16 {
        self.0
    }

    /// Whether this is Ctrl, Shift, Alt or the platform (Meta) key, left or
    /// right.
    #[must_use]
    pub const fn is_modifier(self) -> bool {
        matches!(self.0, 0xE0..=0xE7)
    }

    /// Whether this is either Shift key.
    #[must_use]
    pub const fn is_shift(self) -> bool {
        matches!(self.0, 0xE1 | 0xE5)
    }

    /// The letter key for `letter` (`'a'..='z'` or `'A'..='Z'`).
    #[must_use]
    pub const fn letter(letter: char) -> Option<Self> {
        let lower = letter.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            // 'a'..='z' map to 0x04..=0x1D. `lower` is an ASCII lowercase
            // letter, so the offset is 0..=25 and nothing can wrap.
            let offset = (lower as u8).wrapping_sub(b'a') as u16;
            Some(Self(0x04u16.wrapping_add(offset)))
        } else {
            None
        }
    }

    /// The main-row digit key for `digit` (`0..=9`).
    #[must_use]
    pub const fn digit(digit: u8) -> Option<Self> {
        match digit {
            0 => Some(Self::DIGIT_0),
            // 1..=9 map to 0x1E..=0x26; the offset is 0..=8.
            1..=9 => Some(Self(0x1Eu16.wrapping_add(digit.wrapping_sub(1) as u16))),
            _ => None,
        }
    }

    /// A.
    pub const A: Self = Self(0x04);
    /// B.
    pub const B: Self = Self(0x05);
    /// C.
    pub const C: Self = Self(0x06);
    /// Z.
    pub const Z: Self = Self(0x1D);
    /// 1 and !.
    pub const DIGIT_1: Self = Self(0x1E);
    /// 0 and ).
    pub const DIGIT_0: Self = Self(0x27);
    /// Enter (Return).
    pub const ENTER: Self = Self(0x28);
    /// Escape.
    pub const ESCAPE: Self = Self(0x29);
    /// Backspace.
    pub const BACKSPACE: Self = Self(0x2A);
    /// Tab.
    pub const TAB: Self = Self(0x2B);
    /// Space bar.
    pub const SPACE: Self = Self(0x2C);
    /// - and _.
    pub const MINUS: Self = Self(0x2D);
    /// = and +.
    pub const EQUAL: Self = Self(0x2E);
    /// \[ and {.
    pub const LEFT_BRACKET: Self = Self(0x2F);
    /// \] and }.
    pub const RIGHT_BRACKET: Self = Self(0x30);
    /// \\ and |.
    pub const BACKSLASH: Self = Self(0x31);
    /// ; and :.
    pub const SEMICOLON: Self = Self(0x33);
    /// ' and ".
    pub const APOSTROPHE: Self = Self(0x34);
    /// \` and ~.
    pub const GRAVE: Self = Self(0x35);
    /// , and <.
    pub const COMMA: Self = Self(0x36);
    /// . and >.
    pub const PERIOD: Self = Self(0x37);
    /// / and ?.
    pub const SLASH: Self = Self(0x38);
    /// Caps Lock.
    pub const CAPS_LOCK: Self = Self(0x39);
    /// The non-US # and ~ key, beside Enter on an ISO keyboard.
    pub const NON_US_HASH: Self = Self(0x32);
    /// The non-US \\ and | key, beside the left Shift on an ISO keyboard.
    pub const NON_US_BACKSLASH: Self = Self(0x64);
    /// F1.
    pub const F1: Self = Self(0x3A);
    /// F12.
    pub const F12: Self = Self(0x45);
    /// Insert.
    pub const INSERT: Self = Self(0x49);
    /// Home.
    pub const HOME: Self = Self(0x4A);
    /// Page Up.
    pub const PAGE_UP: Self = Self(0x4B);
    /// Delete (forward delete).
    pub const DELETE: Self = Self(0x4C);
    /// End.
    pub const END: Self = Self(0x4D);
    /// Page Down.
    pub const PAGE_DOWN: Self = Self(0x4E);
    /// Right arrow.
    pub const RIGHT: Self = Self(0x4F);
    /// Left arrow.
    pub const LEFT: Self = Self(0x50);
    /// Down arrow.
    pub const DOWN: Self = Self(0x51);
    /// Up arrow.
    pub const UP: Self = Self(0x52);
    /// Left Ctrl.
    pub const LEFT_CTRL: Self = Self(0xE0);
    /// Left Shift.
    pub const LEFT_SHIFT: Self = Self(0xE1);
    /// Left Alt.
    pub const LEFT_ALT: Self = Self(0xE2);
    /// Left platform key (Windows / Command).
    pub const LEFT_META: Self = Self(0xE3);
    /// Right Ctrl.
    pub const RIGHT_CTRL: Self = Self(0xE4);
    /// Right Shift.
    pub const RIGHT_SHIFT: Self = Self(0xE5);
    /// Right Alt (AltGr).
    pub const RIGHT_ALT: Self = Self(0xE6);
    /// Right platform key.
    pub const RIGHT_META: Self = Self(0xE7);
}

/// Where a touch contact is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// The finger went down.
    Start,
    /// The finger moved.
    Move,
    /// The finger lifted.
    End,
    /// The contact was abandoned (for example, palm rejection); treat as
    /// "never happened", not as a tap.
    Cancel,
}

impl TouchPhase {
    const fn wire(self) -> u8 {
        match self {
            Self::Start => 1,
            Self::Move => 2,
            Self::End => 3,
            Self::Cancel => 4,
        }
    }

    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Start),
            2 => Some(Self::Move),
            3 => Some(Self::End),
            4 => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// One canonical input event.
///
/// Positions are absolute screen coordinates in pixels. Relative devices are
/// turned into absolute positions by the input PD ([`PointerTracker`]), and
/// every event is checked against the screen by
/// [`InputLimits`](crate::InputLimits) before it is forwarded.
///
/// [`PointerTracker`]: crate::PointerTracker
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    /// A key was pressed.
    KeyDown(KeyCode),
    /// A key was released.
    KeyUp(KeyCode),
    /// The pointer moved to `(x, y)`.
    PointerMove {
        /// Horizontal position.
        x: i32,
        /// Vertical position.
        y: i32,
    },
    /// A pointer button changed state. Button 0 is the primary button.
    PointerButton {
        /// Button number, from 0.
        button: u8,
        /// `true` when pressed, `false` when released.
        down: bool,
    },
    /// A touch contact, identified by `id` while it lasts.
    Touch {
        /// Contact number, from 0.
        id: u8,
        /// Horizontal position.
        x: i32,
        /// Vertical position.
        y: i32,
        /// Where the contact is in its life.
        phase: TouchPhase,
    },
    /// A mouse's wheel turned `notches` detents: positive away from the
    /// user, which scrolls up, and negative towards the user, as HID and
    /// evdev count. It carries no position: the UI scrolls what the pointer
    /// is over.
    Wheel {
        /// Detents turned.
        notches: i8,
    },
}

const TAG_KEY_DOWN: u8 = 1;
const TAG_KEY_UP: u8 = 2;
const TAG_POINTER_MOVE: u8 = 3;
const TAG_POINTER_BUTTON: u8 = 4;
const TAG_TOUCH: u8 = 5;
const TAG_WHEEL: u8 = 6;

/// Wire layout, 12 bytes, little-endian; every byte not listed is zero:
///
/// | Bytes | Key events | `PointerMove` | `PointerButton` | `Touch` | `Wheel` |
/// | --- | --- | --- | --- | --- | --- |
/// | 0 | tag 1 / 2 | tag 3 | tag 4 | tag 5 | tag 6 |
/// | 1 | — | — | button | id | notches (`i8`) |
/// | 2 | — | — | down (0/1) | phase (1–4) | — |
/// | 3 | — | — | — | — | — |
/// | 4–7 | key usage (`u16`), then zero | `x` (`i32`) | — | `x` (`i32`) | — |
/// | 8–11 | — | `y` (`i32`) | — | `y` (`i32`) | — |
///
/// Version 2 added `Wheel`.
impl Message for InputEvent {
    const KIND: u16 = INPUT_EVENT_KIND;
    const VERSION: u16 = 2;
    const PAYLOAD_LEN: usize = 12;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        match *self {
            Self::KeyDown(key) | Self::KeyUp(key) => {
                let tag = if matches!(self, Self::KeyDown(_)) {
                    TAG_KEY_DOWN
                } else {
                    TAG_KEY_UP
                };
                out.write_u8(tag)?;
                out.write_zeros(3)?;
                out.write_u16(key.usage())?;
                out.write_zeros(6)
            }
            Self::PointerMove { x, y } => {
                out.write_u8(TAG_POINTER_MOVE)?;
                out.write_zeros(3)?;
                out.write_i32(x)?;
                out.write_i32(y)
            }
            Self::PointerButton { button, down } => {
                out.write_u8(TAG_POINTER_BUTTON)?;
                out.write_u8(button)?;
                out.write_bool(down)?;
                out.write_zeros(9)
            }
            Self::Touch { id, x, y, phase } => {
                out.write_u8(TAG_TOUCH)?;
                out.write_u8(id)?;
                out.write_u8(phase.wire())?;
                out.write_zeros(1)?;
                out.write_i32(x)?;
                out.write_i32(y)
            }
            Self::Wheel { notches } => {
                let [notches] = notches.to_le_bytes();
                out.write_u8(TAG_WHEEL)?;
                out.write_u8(notches)?;
                out.write_zeros(10)
            }
        }
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let tag = input.read_u8()?;
        match tag {
            TAG_KEY_DOWN | TAG_KEY_UP => {
                input.read_zeros(3)?;
                let key = KeyCode::new(input.read_u16()?)
                    .ok_or(DecodeError::InvalidValue { field: "key" })?;
                input.read_zeros(6)?;
                Ok(if tag == TAG_KEY_DOWN {
                    Self::KeyDown(key)
                } else {
                    Self::KeyUp(key)
                })
            }
            TAG_POINTER_MOVE => {
                input.read_zeros(3)?;
                let x = input.read_i32()?;
                let y = input.read_i32()?;
                Ok(Self::PointerMove { x, y })
            }
            TAG_POINTER_BUTTON => {
                let button = input.read_u8()?;
                let down = input.read_bool()?;
                input.read_zeros(9)?;
                Ok(Self::PointerButton { button, down })
            }
            TAG_TOUCH => {
                let id = input.read_u8()?;
                let phase = TouchPhase::from_wire(input.read_u8()?)
                    .ok_or(DecodeError::InvalidValue { field: "phase" })?;
                input.read_zeros(1)?;
                let x = input.read_i32()?;
                let y = input.read_i32()?;
                Ok(Self::Touch { id, x, y, phase })
            }
            TAG_WHEEL => {
                let notches = i8::from_le_bytes([input.read_u8()?]);
                input.read_zeros(10)?;
                Ok(Self::Wheel { notches })
            }
            _ => Err(DecodeError::InvalidValue { field: "tag" }),
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
    use nowayos_ipc::{HEADER_LEN, decode_exact, encode};

    fn round_trip(event: InputEvent) -> [u8; 20] {
        let mut buf = [0u8; 20];
        assert_eq!(encode(&event, &mut buf).unwrap(), 20);
        assert_eq!(decode_exact::<InputEvent>(&buf), Ok(event));
        buf
    }

    #[test]
    fn every_variant_round_trips() {
        round_trip(InputEvent::KeyDown(KeyCode::ENTER));
        round_trip(InputEvent::KeyUp(KeyCode::RIGHT_META));
        round_trip(InputEvent::PointerMove { x: -5, y: i32::MAX });
        round_trip(InputEvent::PointerButton {
            button: 2,
            down: true,
        });
        round_trip(InputEvent::Touch {
            id: 9,
            x: 100,
            y: 200,
            phase: TouchPhase::Cancel,
        });
        round_trip(InputEvent::Wheel { notches: -128 });
        round_trip(InputEvent::Wheel { notches: 3 });
    }

    #[test]
    fn layout_matches_the_table() {
        let buf = round_trip(InputEvent::Touch {
            id: 3,
            x: 0x0102_0304,
            y: -1,
            phase: TouchPhase::Move,
        });
        assert_eq!(
            buf[HEADER_LEN..],
            [5, 3, 2, 0, 4, 3, 2, 1, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        let buf = round_trip(InputEvent::KeyDown(KeyCode::A));
        assert_eq!(buf[HEADER_LEN..], [1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
        let buf = round_trip(InputEvent::Wheel { notches: -2 });
        assert_eq!(buf[HEADER_LEN..], [6, 0xFE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    fn payload_with(mutate: impl FnOnce(&mut [u8])) -> Result<InputEvent, DecodeError> {
        let mut buf = round_trip(InputEvent::KeyDown(KeyCode::A));
        mutate(&mut buf[HEADER_LEN..]);
        decode_exact(&buf)
    }

    #[test]
    fn rejects_unknown_tags_keys_phases_and_padding() {
        assert_eq!(
            payload_with(|p| p[0] = 0),
            Err(DecodeError::InvalidValue { field: "tag" })
        );
        assert_eq!(
            payload_with(|p| p[0] = 7),
            Err(DecodeError::InvalidValue { field: "tag" })
        );
        // A wheel's bytes 2 to 11 are padding: here the key usage's byte.
        assert_eq!(
            payload_with(|p| p[0] = TAG_WHEEL),
            Err(DecodeError::NonZeroPadding)
        );
        // Key usage 0x03 ("ErrorUndefined") is not a key.
        assert_eq!(
            payload_with(|p| p[4] = 3),
            Err(DecodeError::InvalidValue { field: "key" })
        );
        assert_eq!(
            payload_with(|p| p[11] = 1),
            Err(DecodeError::NonZeroPadding)
        );
        assert_eq!(
            payload_with(|p| {
                p[0] = TAG_TOUCH;
                p[2] = 9;
                p[4] = 0;
            }),
            Err(DecodeError::InvalidValue { field: "phase" })
        );
        assert_eq!(
            payload_with(|p| {
                p[0] = TAG_POINTER_BUTTON;
                p[2] = 2;
                p[4] = 0;
            }),
            Err(DecodeError::InvalidValue { field: "bool" })
        );
    }

    #[test]
    fn key_codes_are_validated() {
        assert_eq!(KeyCode::new(0x00), None);
        assert_eq!(KeyCode::new(0x03), None);
        assert_eq!(KeyCode::new(0x04), Some(KeyCode::A));
        assert_eq!(KeyCode::new(0xA4).map(KeyCode::usage), Some(0xA4));
        assert_eq!(KeyCode::new(0xA5), None);
        assert_eq!(KeyCode::new(0xDF), None);
        assert!(KeyCode::new(0xE0).unwrap().is_modifier());
        assert_eq!(KeyCode::new(0xE8), None);
        assert!(KeyCode::RIGHT_SHIFT.is_shift());
        assert!(!KeyCode::LEFT_CTRL.is_shift());
    }

    #[test]
    fn letter_and_digit_constructors() {
        assert_eq!(KeyCode::letter('a'), Some(KeyCode::A));
        assert_eq!(KeyCode::letter('C'), Some(KeyCode::C));
        assert_eq!(KeyCode::letter('z'), Some(KeyCode::Z));
        assert_eq!(KeyCode::letter('1'), None);
        assert_eq!(KeyCode::letter('é'), None);
        assert_eq!(KeyCode::digit(1), Some(KeyCode::DIGIT_1));
        assert_eq!(KeyCode::digit(0), Some(KeyCode::DIGIT_0));
        assert_eq!(KeyCode::digit(9).map(KeyCode::usage), Some(0x26));
        assert_eq!(KeyCode::digit(10), None);
    }
}
