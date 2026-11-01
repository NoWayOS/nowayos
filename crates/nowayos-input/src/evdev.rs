// SPDX-License-Identifier: MIT OR Apache-2.0
//! virtio-input's events turned into canonical [`InputEvent`]s.
//!
//! A virtio input device (virtio 1.2, section 5.8) delivers each event as 8
//! bytes, little-endian: a 16-bit type, a 16-bit code and a 32-bit value,
//! with the event types and codes of the Linux input layer ("evdev"). The
//! numeric values here are that interface's, as published in Linux's UAPI
//! header `input-event-codes.h` (read for its values only); the keyboard
//! usages they map to are the USB HID Usage Tables' (Keyboard/Keypad page).
//!
//! **The device is not trusted.** An event of a type or code this
//! translator does not know, a key value other than release, press or
//! repeat, or an absolute position outside the device's range is dropped and
//! counted ([`Translator::dropped`]): never forwarded, never a panic.
//! Absolute positions (QEMU's virtio tablet: 0 to [`ABS_MAX`] on each axis)
//! are scaled to the screen and reported once per report (`SYN_REPORT`), as
//! one [`InputEvent::PointerMove`].

use crate::{InputEvent, KeyCode};

/// The size of one event.
pub const EVENT_LEN: usize = 8;

/// Event types.
pub const EV_SYN: u16 = 0x00;
/// A key or button.
pub const EV_KEY: u16 = 0x01;
/// Relative motion: not supported here.
pub const EV_REL: u16 = 0x02;
/// An absolute axis.
pub const EV_ABS: u16 = 0x03;
/// Miscellaneous (scan codes): ignored.
pub const EV_MSC: u16 = 0x04;

/// The end of one report: what came before it belongs together.
pub const SYN_REPORT: u16 = 0x00;
/// The horizontal axis.
pub const ABS_X: u16 = 0x00;
/// The vertical axis.
pub const ABS_Y: u16 = 0x01;
/// The primary, secondary and middle buttons.
pub const BTN_LEFT: u16 = 0x110;
/// The secondary button.
pub const BTN_RIGHT: u16 = 0x111;
/// The middle button.
pub const BTN_MIDDLE: u16 = 0x112;

/// The largest absolute position QEMU's virtio tablet reports on each axis.
pub const ABS_MAX: u32 = 0x7FFF;

/// How many events of each kind were dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropCounts {
    /// Events of a type this translator does not know.
    pub unknown_type: u32,
    /// Keys, buttons and axes it does not know.
    pub unknown_code: u32,
    /// Key values other than 0, 1 and 2; positions beyond [`ABS_MAX`].
    pub out_of_range: u32,
}

impl DropCounts {
    /// All dropped events.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.unknown_type
            .saturating_add(self.unknown_code)
            .saturating_add(self.out_of_range)
    }
}

/// Turns one device's events into canonical events, for a screen of
/// `width` x `height` pixels.
#[derive(Clone, Copy, Debug)]
pub struct Translator {
    width: u32,
    height: u32,
    x: Option<u32>,
    y: Option<u32>,
    moved: bool,
    dropped: DropCounts,
}

impl Translator {
    /// A translator for a screen of `width` x `height` pixels.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            x: None,
            y: None,
            moved: false,
            dropped: DropCounts {
                unknown_type: 0,
                unknown_code: 0,
                out_of_range: 0,
            },
        }
    }

    /// The canonical event `event` makes, if any. Positions wait for the
    /// report's end: a `SYN_REPORT` after a change of either axis makes one
    /// [`InputEvent::PointerMove`], once both axes are known.
    pub fn translate(&mut self, event: &[u8; EVENT_LEN]) -> Option<InputEvent> {
        let [t0, t1, c0, c1, v0, v1, v2, v3] = *event;
        let kind = u16::from_le_bytes([t0, t1]);
        let code = u16::from_le_bytes([c0, c1]);
        let value = u32::from_le_bytes([v0, v1, v2, v3]);
        match kind {
            EV_KEY => self.key(code, value),
            EV_ABS => {
                if value > ABS_MAX {
                    self.dropped.out_of_range = self.dropped.out_of_range.saturating_add(1);
                    return None;
                }
                match code {
                    ABS_X => self.x = Some(value),
                    ABS_Y => self.y = Some(value),
                    _ => {
                        self.dropped.unknown_code = self.dropped.unknown_code.saturating_add(1);
                        return None;
                    }
                }
                self.moved = true;
                None
            }
            EV_SYN => {
                if code != SYN_REPORT || !self.moved {
                    return None;
                }
                let (Some(x), Some(y)) = (self.x, self.y) else {
                    return None;
                };
                self.moved = false;
                Some(InputEvent::PointerMove {
                    x: scale(x, self.width),
                    y: scale(y, self.height),
                })
            }
            EV_MSC => None,
            _ => {
                self.dropped.unknown_type = self.dropped.unknown_type.saturating_add(1);
                None
            }
        }
    }

    /// How many events were dropped, by reason.
    #[must_use]
    pub const fn dropped(&self) -> DropCounts {
        self.dropped
    }

    fn key(&mut self, code: u16, value: u32) -> Option<InputEvent> {
        if let Some(button) = code
            .checked_sub(BTN_LEFT)
            .filter(|_| code <= BTN_MIDDLE)
            .and_then(|b| u8::try_from(b).ok())
        {
            return match value {
                0 | 1 => Some(InputEvent::PointerButton {
                    button,
                    down: value == 1,
                }),
                _ => {
                    self.dropped.out_of_range = self.dropped.out_of_range.saturating_add(1);
                    None
                }
            };
        }
        let Some(key) = key_code(code) else {
            self.dropped.unknown_code = self.dropped.unknown_code.saturating_add(1);
            return None;
        };
        match value {
            0 => Some(InputEvent::KeyUp(key)),
            // 2 is the device's auto-repeat: another press.
            1 | 2 => Some(InputEvent::KeyDown(key)),
            _ => {
                self.dropped.out_of_range = self.dropped.out_of_range.saturating_add(1);
                None
            }
        }
    }
}

/// `value` (0 to `ABS_MAX`) as a pixel position on an axis of `size` pixels.
fn scale(value: u32, size: u32) -> i32 {
    let scaled = u64::from(value)
        .saturating_mul(u64::from(size))
        .checked_div(u64::from(ABS_MAX).saturating_add(1))
        .unwrap_or(0);
    i32::try_from(scaled).unwrap_or(i32::MAX)
}

/// The key with evdev key code `code`, or `None` for codes this translator
/// does not map.
#[must_use]
pub const fn key_code(code: u16) -> Option<KeyCode> {
    let usage: u16 = match code {
        1 => 0x29, // KEY_ESC
        // KEY_1 .. KEY_9 -> 0x1E .. 0x26
        2..=10 => match code.checked_add(0x1C) {
            Some(usage) => usage,
            None => return None,
        },
        11 => 0x27, // KEY_0
        12 => 0x2D, // KEY_MINUS
        13 => 0x2E, // KEY_EQUAL
        14 => 0x2A, // KEY_BACKSPACE
        15 => 0x2B, // KEY_TAB
        16 => 0x14, // KEY_Q
        17 => 0x1A, // KEY_W
        18 => 0x08, // KEY_E
        19 => 0x15, // KEY_R
        20 => 0x17, // KEY_T
        21 => 0x1C, // KEY_Y
        22 => 0x18, // KEY_U
        23 => 0x0C, // KEY_I
        24 => 0x12, // KEY_O
        25 => 0x13, // KEY_P
        26 => 0x2F, // KEY_LEFTBRACE
        27 => 0x30, // KEY_RIGHTBRACE
        28 => 0x28, // KEY_ENTER
        29 => 0xE0, // KEY_LEFTCTRL
        30 => 0x04, // KEY_A
        31 => 0x16, // KEY_S
        32 => 0x07, // KEY_D
        33 => 0x09, // KEY_F
        34 => 0x0A, // KEY_G
        35 => 0x0B, // KEY_H
        36 => 0x0D, // KEY_J
        37 => 0x0E, // KEY_K
        38 => 0x0F, // KEY_L
        39 => 0x33, // KEY_SEMICOLON
        40 => 0x34, // KEY_APOSTROPHE
        41 => 0x35, // KEY_GRAVE
        42 => 0xE1, // KEY_LEFTSHIFT
        43 => 0x31, // KEY_BACKSLASH
        44 => 0x1D, // KEY_Z
        45 => 0x1B, // KEY_X
        46 => 0x06, // KEY_C
        47 => 0x19, // KEY_V
        48 => 0x05, // KEY_B
        49 => 0x11, // KEY_N
        50 => 0x10, // KEY_M
        51 => 0x36, // KEY_COMMA
        52 => 0x37, // KEY_DOT
        53 => 0x38, // KEY_SLASH
        54 => 0xE5, // KEY_RIGHTSHIFT
        56 => 0xE2, // KEY_LEFTALT
        57 => 0x2C, // KEY_SPACE
        58 => 0x39, // KEY_CAPSLOCK
        // KEY_F1 .. KEY_F10 -> 0x3A .. 0x43
        59..=68 => match code.checked_sub(1) {
            Some(usage) => usage,
            None => return None,
        },
        87 => 0x44,  // KEY_F11
        88 => 0x45,  // KEY_F12
        97 => 0xE4,  // KEY_RIGHTCTRL
        100 => 0xE6, // KEY_RIGHTALT
        102 => 0x4A, // KEY_HOME
        103 => 0x52, // KEY_UP
        104 => 0x4B, // KEY_PAGEUP
        105 => 0x50, // KEY_LEFT
        106 => 0x4F, // KEY_RIGHT
        107 => 0x4D, // KEY_END
        108 => 0x51, // KEY_DOWN
        109 => 0x4E, // KEY_PAGEDOWN
        110 => 0x49, // KEY_INSERT
        111 => 0x4C, // KEY_DELETE
        125 => 0xE3, // KEY_LEFTMETA
        126 => 0xE7, // KEY_RIGHTMETA
        _ => return None,
    };
    KeyCode::new(usage)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn event(kind: u16, code: u16, value: u32) -> [u8; EVENT_LEN] {
        let mut bytes = [0; EVENT_LEN];
        bytes[..2].copy_from_slice(&kind.to_le_bytes());
        bytes[2..4].copy_from_slice(&code.to_le_bytes());
        bytes[4..].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    #[test]
    fn keys_map_to_their_usages() {
        assert_eq!(key_code(30), Some(KeyCode::A));
        assert_eq!(key_code(44), Some(KeyCode::Z));
        assert_eq!(key_code(2), KeyCode::digit(1));
        assert_eq!(key_code(11), Some(KeyCode::DIGIT_0));
        assert_eq!(key_code(28), Some(KeyCode::ENTER));
        assert_eq!(key_code(57), Some(KeyCode::SPACE));
        assert_eq!(key_code(103), Some(KeyCode::UP));
        assert_eq!(key_code(42), Some(KeyCode::LEFT_SHIFT));
        assert_eq!(key_code(59), Some(KeyCode::F1));
        assert_eq!(key_code(88), Some(KeyCode::F12));
        assert_eq!(key_code(55), None, "KEY_KPASTERISK is not mapped");
    }

    #[test]
    fn presses_releases_and_repeats() {
        let mut t = Translator::new(800, 480);
        assert_eq!(
            t.translate(&event(EV_KEY, 30, 1)),
            Some(InputEvent::KeyDown(KeyCode::A))
        );
        assert_eq!(
            t.translate(&event(EV_KEY, 30, 2)),
            Some(InputEvent::KeyDown(KeyCode::A))
        );
        assert_eq!(
            t.translate(&event(EV_KEY, 30, 0)),
            Some(InputEvent::KeyUp(KeyCode::A))
        );
        assert_eq!(t.translate(&event(EV_KEY, 30, 7)), None);
        assert_eq!(t.dropped().out_of_range, 1);
    }

    #[test]
    fn tablet_positions_are_scaled_and_reported_once_per_report() {
        let mut t = Translator::new(800, 480);
        assert_eq!(t.translate(&event(EV_ABS, ABS_X, 16384)), None);
        assert_eq!(t.translate(&event(EV_ABS, ABS_Y, 16384)), None);
        assert_eq!(
            t.translate(&event(EV_SYN, SYN_REPORT, 0)),
            Some(InputEvent::PointerMove { x: 400, y: 240 })
        );
        assert_eq!(
            t.translate(&event(EV_SYN, SYN_REPORT, 0)),
            None,
            "nothing moved"
        );
        t.translate(&event(EV_ABS, ABS_X, ABS_MAX));
        assert_eq!(
            t.translate(&event(EV_SYN, SYN_REPORT, 0)),
            Some(InputEvent::PointerMove { x: 799, y: 240 })
        );
        assert_eq!(
            t.translate(&event(EV_KEY, BTN_LEFT, 1)),
            Some(InputEvent::PointerButton {
                button: 0,
                down: true
            })
        );
        assert_eq!(
            t.translate(&event(EV_KEY, BTN_MIDDLE, 0)),
            Some(InputEvent::PointerButton {
                button: 2,
                down: false
            })
        );
    }

    #[test]
    fn a_position_needs_both_axes() {
        let mut t = Translator::new(800, 480);
        t.translate(&event(EV_ABS, ABS_X, 100));
        assert_eq!(t.translate(&event(EV_SYN, SYN_REPORT, 0)), None);
    }

    #[test]
    fn unknown_and_out_of_range_events_are_dropped_and_counted() {
        let mut t = Translator::new(800, 480);
        assert_eq!(t.translate(&event(EV_REL, 0, 5)), None);
        assert_eq!(t.translate(&event(0x1F, 0, 0)), None);
        assert_eq!(t.translate(&event(EV_KEY, 0x2FF, 1)), None);
        assert_eq!(t.translate(&event(EV_ABS, 0x2F, 10)), None);
        assert_eq!(t.translate(&event(EV_ABS, ABS_X, ABS_MAX + 1)), None);
        assert_eq!(t.translate(&event(EV_KEY, BTN_LEFT, 2)), None);
        assert_eq!(
            t.dropped(),
            DropCounts {
                unknown_type: 2,
                unknown_code: 2,
                out_of_range: 2
            }
        );
        assert_eq!(t.dropped().total(), 6);
        assert_eq!(
            t.translate(&event(EV_MSC, 4, 0x1E)),
            None,
            "scan codes are ignored"
        );
        assert_eq!(t.dropped().total(), 6);
    }

    proptest! {
        #[test]
        fn any_event_is_translated_or_dropped_never_a_panic(bytes in prop::array::uniform8(any::<u8>()), width in 1u32..4096, height in 1u32..4096) {
            let mut t = Translator::new(width, height);
            let before = t.dropped().total();
            if let Some(InputEvent::PointerMove { x, y }) = t.translate(&bytes) {
                prop_assert!(x >= 0 && (x as u32) < width && y >= 0 && (y as u32) < height);
            }
            prop_assert!(t.dropped().total() >= before);
        }

        #[test]
        fn positions_always_land_on_the_screen(x in 0u32..=ABS_MAX, y in 0u32..=ABS_MAX) {
            let mut t = Translator::new(800, 480);
            t.translate(&event(EV_ABS, ABS_X, x));
            t.translate(&event(EV_ABS, ABS_Y, y));
            let Some(InputEvent::PointerMove { x, y }) = t.translate(&event(EV_SYN, SYN_REPORT, 0)) else {
                return Err(TestCaseError::fail("no move"));
            };
            prop_assert!((0..800).contains(&x) && (0..480).contains(&y));
        }
    }
}
