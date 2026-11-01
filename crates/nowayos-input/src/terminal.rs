// SPDX-License-Identifier: MIT OR Apache-2.0
//! A terminal's keys as key presses.
//!
//! A terminal - on a developer's Mac, attached to the board's debug UART
//! through a probe - sends the keys typed in it as bytes: a printable
//! character as its ASCII code, a few keys as control characters, and keys
//! with no character, such as the arrows, as escape sequences. [`Decoder`]
//! turns those bytes back into [`Press`]es of the keys a US-English keyboard
//! would have typed, each of which is a short series of canonical
//! [`InputEvent`]s ([`Press::events`]). A development route only: a
//! product's keys come from its own input devices, never from a serial line.
//!
//! Written from ECMA-48, 5th edition (1991), "Control Functions for Coded
//! Character Sets": the C0 control characters, ESC, an escape sequence's
//! form (ESC, intermediate bytes 02/00 to 02/15, a final byte 03/00 to
//! 07/14), a control sequence's form (CSI, written ESC 05/11 in a 7-bit
//! code; parameter bytes 03/00 to 03/15, intermediate bytes 02/00 to 02/15,
//! a final byte 04/00 to 07/14), SS3 (ESC 04/15) and CBT, cursor backward
//! tabulation (CSI 05/10); and, for which sequences a terminal sends for
//! which keys, xterm's documentation, "Xterm Control Sequences" (the cursor
//! keys in normal and application mode, and CBT for Shift-Tab). The keys'
//! numbers are the USB HID Usage Tables', as everywhere in this crate.
//!
//! # What it understands
//!
//! | Bytes | Key |
//! | --- | --- |
//! | 0x20 to 0x7E | the key that types that character on a US-English layout, with Shift for `A`-`Z` and the shifted symbols ([`crate::keymap::us_key`]) |
//! | CR (0x0D) | Enter |
//! | LF (0x0A) | Enter, unless it follows a CR: a CR LF pair is one Enter |
//! | HT (0x09) | Tab |
//! | BS (0x08), DEL (0x7F) | Backspace |
//! | `ESC [ A`, `ESC [ B`, `ESC [ C`, `ESC [ D` | Up, Down, Right, Left |
//! | `ESC O A` to `ESC O D` | the same arrows, as a terminal in application cursor mode sends them |
//! | `ESC [ Z` | Shift-Tab |
//! | ESC alone | Escape |
//!
//! **The bytes are not trusted.** Anything else is dropped and counted
//! ([`Decoder::dropped`]), never a panic: any other control character and
//! any byte above 0x7E ([`DropCounts::unknown_bytes`]); any other complete
//! escape or control sequence, a parameter making it another key - Ctrl-Up's
//! `ESC [ 1 ; 5 A` - or a function key's ([`DropCounts::unknown_sequences`]);
//! and a sequence cut short, by a byte that cannot continue it or by the
//! line going quiet ([`DropCounts::cut_short`]). A byte that cuts a sequence
//! short is then read afresh, so an Enter or a new ESC is never lost to a
//! broken sequence before it. The decoder keeps no bytes - a sequence's
//! length is counted, saturating, never stored - so its memory is fixed
//! whatever it is sent.
//!
//! # Escape, and when the line goes quiet
//!
//! ESC alone is the Escape key, but ESC also begins every sequence, and the
//! bytes cannot say which it is: only time can. A decoder left part-way
//! through a sequence says so ([`Decoder::pending`]); the caller, once no
//! byte has come for a while, calls [`Decoder::end_of_input`], which turns
//! an ESC still waiting into Escape and counts any other sequence as cut
//! short. A terminal writes a key's whole sequence at once, so its bytes
//! arrive back to back: at 115,200 baud, under 0.1 ms apart.

use crate::{InputEvent, KeyCode, keymap::us_key};

/// ESC, which begins every escape and control sequence.
const ESC: u8 = 0x1B;

/// How many bytes went into no key, by why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropCounts {
    /// Bytes that type no key on their own: control characters other than
    /// CR, LF, HT, BS and ESC, and every byte above 0x7E.
    pub unknown_bytes: u32,
    /// Complete escape and control sequences of no key the decoder sends.
    pub unknown_sequences: u32,
    /// Sequences cut short, by a byte that cannot continue them or by the
    /// end of input.
    pub cut_short: u32,
}

impl DropCounts {
    /// Every byte or sequence dropped, saturating.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.unknown_bytes
            .saturating_add(self.unknown_sequences)
            .saturating_add(self.cut_short)
    }
}

/// One key pressed and released, with Shift held around it or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Press {
    key: KeyCode,
    shift: bool,
}

impl Press {
    /// `key` pressed and released, with Shift held around it if `shift`.
    #[must_use]
    pub const fn new(key: KeyCode, shift: bool) -> Self {
        Self { key, shift }
    }

    /// The key.
    #[must_use]
    pub const fn key(self) -> KeyCode {
        self.key
    }

    /// Whether Shift is held around it.
    #[must_use]
    pub const fn shift(self) -> bool {
        self.shift
    }

    /// How many events it is ([`Press::events`]): 2, or 4 with Shift.
    #[must_use]
    pub const fn event_count(self) -> usize {
        if self.shift { 4 } else { 2 }
    }

    /// Its events, in order: Left Shift down if Shift is held, the key down,
    /// the key up, then Left Shift up if it was held. A press is whole: a
    /// caller sends all of its events or none, so Shift is never left down.
    pub fn events(self) -> impl Iterator<Item = InputEvent> {
        let shift = self.shift;
        [
            shift.then_some(InputEvent::KeyDown(KeyCode::LEFT_SHIFT)),
            Some(InputEvent::KeyDown(self.key)),
            Some(InputEvent::KeyUp(self.key)),
            shift.then_some(InputEvent::KeyUp(KeyCode::LEFT_SHIFT)),
        ]
        .into_iter()
        .flatten()
    }
}

/// The presses one byte (or the end of input) completes: none, one, or two
/// (an ESC that turned out to be Escape, then the press of the byte after
/// it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Presses {
    presses: [Option<Press>; 2],
}

impl Presses {
    const NONE: Self = Self {
        presses: [None, None],
    };

    const fn one(press: Press) -> Self {
        Self {
            presses: [Some(press), None],
        }
    }

    /// `first`, then whatever `then` holds (at most one press, as every
    /// caller below gives it).
    const fn then(first: Press, then: Self) -> Self {
        Self {
            presses: [Some(first), then.presses[0]],
        }
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.presses[0].is_none()
    }
}

impl IntoIterator for Presses {
    type Item = Press;
    type IntoIter = core::iter::Flatten<core::array::IntoIter<Option<Press>, 2>>;

    fn into_iter(self) -> Self::IntoIter {
        self.presses.into_iter().flatten()
    }
}

/// Where the decoder is in a sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Between keys.
    Ground,
    /// After an ESC: Escape, or the start of a sequence.
    Escape,
    /// After ESC and an intermediate byte: an escape sequence of no key,
    /// until its final byte.
    EscapeIntermediate,
    /// After CSI (ESC `[`) or SS3 (ESC `O`) and `len` parameter or
    /// intermediate bytes, until a final byte.
    Sequence { kind: Kind, len: u8 },
}

/// Which introducer a sequence began with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// CSI, ESC `[`.
    Csi,
    /// SS3, ESC `O`.
    Ss3,
}

/// Turns a terminal's bytes into key presses. See the module's
/// documentation for what it understands.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    state: State,
    /// The last byte was a CR, so an LF now is the second half of one Enter.
    after_cr: bool,
    dropped: DropCounts,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// A decoder between keys, having dropped nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Ground,
            after_cr: false,
            dropped: DropCounts {
                unknown_bytes: 0,
                unknown_sequences: 0,
                cut_short: 0,
            },
        }
    }

    /// What it has dropped so far.
    #[must_use]
    pub const fn dropped(&self) -> DropCounts {
        self.dropped
    }

    /// Whether it is part-way through a sequence - or holds an ESC that may
    /// yet be Escape - which only more bytes or [`Decoder::end_of_input`]
    /// can finish.
    #[must_use]
    pub const fn pending(&self) -> bool {
        !matches!(self.state, State::Ground)
    }

    /// Takes the next byte; returns the presses it completes.
    pub fn push(&mut self, byte: u8) -> Presses {
        match self.state {
            State::Ground => self.ground(byte),
            State::Escape => match byte {
                b'[' => self.enter(State::Sequence {
                    kind: Kind::Csi,
                    len: 0,
                }),
                b'O' => self.enter(State::Sequence {
                    kind: Kind::Ss3,
                    len: 0,
                }),
                // Another ESC: the first was Escape, and this one begins
                // again.
                ESC => Presses::one(Press::new(KeyCode::ESCAPE, false)),
                // An intermediate byte: an escape sequence of no key, to
                // its final byte.
                0x20..=0x2F => self.enter(State::EscapeIntermediate),
                // A final byte: a two-byte escape sequence of no key.
                0x30..=0x7E => self.unknown_sequence(),
                // A control character or a byte above 0x7E cannot continue
                // a sequence: the ESC was Escape, and the byte is read
                // afresh.
                _ => {
                    self.state = State::Ground;
                    Presses::then(Press::new(KeyCode::ESCAPE, false), self.ground(byte))
                }
            },
            State::EscapeIntermediate => match byte {
                0x20..=0x2F => Presses::NONE,
                0x30..=0x7E => self.unknown_sequence(),
                _ => self.cut_short(byte),
            },
            State::Sequence { kind, len } => match byte {
                // Parameter and intermediate bytes: counted, never kept.
                0x20..=0x3F => {
                    self.state = State::Sequence {
                        kind,
                        len: len.saturating_add(1),
                    };
                    Presses::NONE
                }
                0x40..=0x7E => {
                    self.state = State::Ground;
                    match (kind, len, final_key(kind, byte)) {
                        (_, 0, Some(press)) => Presses::one(press),
                        _ => self.unknown_sequence(),
                    }
                }
                _ => self.cut_short(byte),
            },
        }
    }

    /// The line has gone quiet: an ESC still waiting was Escape, and any
    /// other sequence is cut short and counted. Returns the press, if any.
    pub fn end_of_input(&mut self) -> Presses {
        let state = self.state;
        self.state = State::Ground;
        match state {
            State::Ground => Presses::NONE,
            State::Escape => Presses::one(Press::new(KeyCode::ESCAPE, false)),
            State::EscapeIntermediate | State::Sequence { .. } => {
                self.dropped.cut_short = self.dropped.cut_short.saturating_add(1);
                Presses::NONE
            }
        }
    }

    /// A byte between keys.
    fn ground(&mut self, byte: u8) -> Presses {
        let after_cr = core::mem::replace(&mut self.after_cr, byte == b'\r');
        let press = |key| Presses::one(Press::new(key, false));
        match byte {
            b'\r' => press(KeyCode::ENTER),
            b'\n' if after_cr => Presses::NONE,
            b'\n' => press(KeyCode::ENTER),
            b'\t' => press(KeyCode::TAB),
            0x08 | 0x7F => press(KeyCode::BACKSPACE),
            ESC => self.enter(State::Escape),
            _ => match us_key(char::from(byte)) {
                Some((key, shift)) if (0x20..=0x7E).contains(&byte) => {
                    Presses::one(Press::new(key, shift))
                }
                _ => {
                    self.dropped.unknown_bytes = self.dropped.unknown_bytes.saturating_add(1);
                    Presses::NONE
                }
            },
        }
    }

    const fn enter(&mut self, state: State) -> Presses {
        self.state = state;
        Presses::NONE
    }

    /// A complete sequence of no key the decoder sends.
    fn unknown_sequence(&mut self) -> Presses {
        self.state = State::Ground;
        self.dropped.unknown_sequences = self.dropped.unknown_sequences.saturating_add(1);
        Presses::NONE
    }

    /// `byte` cannot continue the sequence: it is cut short, counted, and
    /// `byte` read afresh.
    fn cut_short(&mut self, byte: u8) -> Presses {
        self.state = State::Ground;
        self.dropped.cut_short = self.dropped.cut_short.saturating_add(1);
        self.ground(byte)
    }
}

/// The key a sequence with no parameters names by its final byte, if any.
const fn final_key(kind: Kind, byte: u8) -> Option<Press> {
    let key = match byte {
        b'A' => KeyCode::UP,
        b'B' => KeyCode::DOWN,
        b'C' => KeyCode::RIGHT,
        b'D' => KeyCode::LEFT,
        // CBT, cursor backward tabulation: what a terminal sends for
        // Shift-Tab. A control sequence only.
        b'Z' if matches!(kind, Kind::Csi) => return Some(Press::new(KeyCode::TAB, true)),
        _ => return None,
    };
    Some(Press::new(key, false))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Every press `bytes` makes, then those the end of input makes.
    fn decode(bytes: &[u8]) -> (Vec<Press>, DropCounts) {
        let mut decoder = Decoder::new();
        let mut presses: Vec<Press> = bytes.iter().flat_map(|&byte| decoder.push(byte)).collect();
        presses.extend(decoder.end_of_input());
        assert!(!decoder.pending());
        (presses, decoder.dropped())
    }

    fn plain(key: KeyCode) -> Press {
        Press::new(key, false)
    }

    fn counts(unknown_bytes: u32, unknown_sequences: u32, cut_short: u32) -> DropCounts {
        DropCounts {
            unknown_bytes,
            unknown_sequences,
            cut_short,
        }
    }

    #[test]
    fn printable_characters_are_their_keys() {
        let (presses, dropped) = decode(b"Ada 1!~");
        assert_eq!(
            presses,
            [
                Press::new(KeyCode::A, true),
                plain(KeyCode::letter('d').unwrap()),
                plain(KeyCode::A),
                plain(KeyCode::SPACE),
                plain(KeyCode::DIGIT_1),
                Press::new(KeyCode::DIGIT_1, true),
                Press::new(KeyCode::GRAVE, true),
            ]
        );
        assert_eq!(dropped, DropCounts::default());
    }

    #[test]
    fn the_control_keys() {
        let (presses, dropped) = decode(b"\r\n\n\t\x08\x7f\r");
        assert_eq!(
            presses,
            [
                plain(KeyCode::ENTER),
                plain(KeyCode::ENTER),
                plain(KeyCode::TAB),
                plain(KeyCode::BACKSPACE),
                plain(KeyCode::BACKSPACE),
                plain(KeyCode::ENTER),
            ],
            "CR LF is one Enter; an LF on its own another"
        );
        assert_eq!(dropped, DropCounts::default());
    }

    #[test]
    fn arrows_shift_tab_and_escape() {
        let (presses, dropped) = decode(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1bOA\x1bOD\x1b[Z\x1b");
        assert_eq!(
            presses,
            [
                plain(KeyCode::UP),
                plain(KeyCode::DOWN),
                plain(KeyCode::RIGHT),
                plain(KeyCode::LEFT),
                plain(KeyCode::UP),
                plain(KeyCode::LEFT),
                Press::new(KeyCode::TAB, true),
                plain(KeyCode::ESCAPE),
            ]
        );
        assert_eq!(dropped, DropCounts::default());
    }

    #[test]
    fn escape_waits_for_the_end_of_input_or_a_byte_that_cannot_follow_it() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(ESC).is_empty());
        assert!(decoder.pending());
        assert_eq!(
            decoder.end_of_input().into_iter().collect::<Vec<_>>(),
            [plain(KeyCode::ESCAPE)]
        );
        assert!(!decoder.pending());
        // ESC ESC: the first was Escape; the second still waits.
        assert!(decoder.push(ESC).is_empty());
        assert_eq!(
            decoder.push(ESC).into_iter().collect::<Vec<_>>(),
            [plain(KeyCode::ESCAPE)]
        );
        assert!(decoder.pending());
        // ESC then CR: Escape, then Enter.
        assert_eq!(
            decoder.push(b'\r').into_iter().collect::<Vec<_>>(),
            [plain(KeyCode::ESCAPE), plain(KeyCode::ENTER)]
        );
        assert!(!decoder.pending());
        assert_eq!(decoder.dropped(), DropCounts::default());
    }

    #[test]
    fn other_sequences_are_dropped_and_counted() {
        // Ctrl-Up (a parameter), F1 (SS3 P), Delete (CSI 3 ~), Alt-x (ESC
        // x), an escape sequence with an intermediate byte (ESC ( B), and a
        // control sequence with a thousand parameter bytes.
        let mut bytes = b"\x1b[1;5A\x1bOP\x1b[3~\x1bx\x1b(B\x1b[".to_vec();
        bytes.extend([b'9'; 1000]);
        bytes.push(b'A');
        let (presses, dropped) = decode(&bytes);
        assert_eq!(presses, []);
        assert_eq!(dropped, counts(0, 6, 0));
    }

    #[test]
    fn a_sequence_cut_short_is_counted_and_the_byte_read_afresh() {
        // CSI cut short by CR (Enter), then by ESC (which begins the Up
        // that follows), then an SS3 by DEL, an intermediate by a byte
        // above 0x7E, and a CSI by the end of input.
        let (presses, dropped) = decode(b"\x1b[1\r\x1b[\x1b[A\x1bO\x7f\x1b \xff\x1b[2");
        assert_eq!(
            presses,
            [
                plain(KeyCode::ENTER),
                plain(KeyCode::UP),
                plain(KeyCode::BACKSPACE),
            ]
        );
        assert_eq!(dropped, counts(1, 0, 5));
    }

    #[test]
    fn bytes_of_no_key_are_dropped_and_counted() {
        let (presses, dropped) = decode(b"\x00\x01\x03\x07\x1a\x80\xc3\xa9\xff");
        assert_eq!(presses, []);
        assert_eq!(dropped, counts(9, 0, 0));
        assert_eq!(dropped.total(), 9);
    }

    #[test]
    fn a_press_is_its_events() {
        let events: Vec<_> = Press::new(KeyCode::A, true).events().collect();
        assert_eq!(
            events,
            [
                InputEvent::KeyDown(KeyCode::LEFT_SHIFT),
                InputEvent::KeyDown(KeyCode::A),
                InputEvent::KeyUp(KeyCode::A),
                InputEvent::KeyUp(KeyCode::LEFT_SHIFT),
            ]
        );
        assert_eq!(Press::new(KeyCode::A, true).event_count(), 4);
        let events: Vec<_> = plain(KeyCode::ENTER).events().collect();
        assert_eq!(
            events,
            [
                InputEvent::KeyDown(KeyCode::ENTER),
                InputEvent::KeyUp(KeyCode::ENTER)
            ]
        );
        assert_eq!(plain(KeyCode::ENTER).event_count(), 2);
    }

    /// The keys the decoder sends.
    fn sent(press: Press) -> bool {
        let key = press.key();
        let printable =
            (0x20u8..=0x7E).any(|byte| us_key(char::from(byte)) == Some((key, press.shift())));
        printable
            || (!press.shift()
                && [
                    KeyCode::ENTER,
                    KeyCode::TAB,
                    KeyCode::BACKSPACE,
                    KeyCode::ESCAPE,
                    KeyCode::UP,
                    KeyCode::DOWN,
                    KeyCode::LEFT,
                    KeyCode::RIGHT,
                ]
                .contains(&key))
            || press == Press::new(KeyCode::TAB, true)
    }

    /// What a terminal sends for each key the decoder knows (nothing for
    /// any other).
    fn typed(press: Press) -> Vec<u8> {
        let byte = (0x20u8..=0x7E)
            .find(|&byte| us_key(char::from(byte)) == Some((press.key(), press.shift())));
        if let Some(byte) = byte {
            return vec![byte];
        }
        match (press.key(), press.shift()) {
            (KeyCode::ENTER, false) => b"\r".to_vec(),
            (KeyCode::TAB, false) => b"\t".to_vec(),
            (KeyCode::TAB, true) => b"\x1b[Z".to_vec(),
            (KeyCode::BACKSPACE, false) => b"\x7f".to_vec(),
            (KeyCode::ESCAPE, false) => b"\x1b".to_vec(),
            (KeyCode::UP, false) => b"\x1b[A".to_vec(),
            (KeyCode::DOWN, false) => b"\x1bOB".to_vec(),
            (KeyCode::RIGHT, false) => b"\x1b[C".to_vec(),
            (KeyCode::LEFT, false) => b"\x1bOD".to_vec(),
            _ => Vec::new(),
        }
    }

    fn a_key() -> impl Strategy<Value = Press> {
        prop_oneof![
            (0x20u8..=0x7E).prop_map(|byte| {
                let (key, shift) = us_key(char::from(byte)).unwrap();
                Press::new(key, shift)
            }),
            prop::sample::select(vec![
                plain(KeyCode::ENTER),
                plain(KeyCode::TAB),
                Press::new(KeyCode::TAB, true),
                plain(KeyCode::BACKSPACE),
                plain(KeyCode::ESCAPE),
                plain(KeyCode::UP),
                plain(KeyCode::DOWN),
                plain(KeyCode::RIGHT),
                plain(KeyCode::LEFT),
            ]),
        ]
    }

    proptest! {
        /// Any keys typed, each in its own burst - the line going quiet
        /// after each, as it does between a person's keys - come back as
        /// the same presses, with nothing dropped.
        #[test]
        fn keys_typed_one_at_a_time_come_back(keys in prop::collection::vec(a_key(), 0..64)) {
            let mut decoder = Decoder::new();
            let mut presses = Vec::new();
            for &key in &keys {
                for byte in typed(key) {
                    presses.extend(decoder.push(byte));
                }
                presses.extend(decoder.end_of_input());
            }
            prop_assert_eq!(presses, keys);
            prop_assert_eq!(decoder.dropped(), DropCounts::default());
        }

        /// Any bytes at all, with the end of input anywhere: nothing
        /// panics, every press is a key the decoder sends, the counts only
        /// grow, and the end of input always leaves it between keys.
        #[test]
        fn any_bytes_make_only_known_keys(
            chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..24), 0..16)
        ) {
            let mut decoder = Decoder::new();
            for chunk in chunks {
                for byte in chunk {
                    let before = decoder.dropped();
                    for press in decoder.push(byte) {
                        prop_assert!(sent(press), "{:?}", press);
                    }
                    let after = decoder.dropped();
                    prop_assert!(after.total() >= before.total());
                }
                for press in decoder.end_of_input() {
                    prop_assert_eq!(press, plain(KeyCode::ESCAPE));
                }
                prop_assert!(!decoder.pending());
            }
        }

        /// Every byte that is not ESC, or part of a sequence, is accounted
        /// for: it makes a press or is counted, but for the LF of a CR LF.
        #[test]
        fn every_byte_outside_a_sequence_is_a_press_or_counted(
            bytes in prop::collection::vec(any::<u8>().prop_filter("no ESC", |&byte| byte != ESC), 0..256)
        ) {
            let mut decoder = Decoder::new();
            let mut previous = None;
            for byte in bytes {
                let before = decoder.dropped().unknown_bytes;
                let presses = decoder.push(byte);
                let counted = decoder.dropped().unknown_bytes - before;
                let lf_of_crlf = byte == b'\n' && previous == Some(b'\r');
                prop_assert_eq!(
                    usize::from(!presses.is_empty()) + usize::try_from(counted).unwrap() + usize::from(lf_of_crlf),
                    1,
                    "byte {:#04x}", byte
                );
                previous = Some(byte);
            }
        }
    }
}
