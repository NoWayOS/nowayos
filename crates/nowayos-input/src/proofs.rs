// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: an [`InputEvent`] decoded from any 12-byte payload
//! and checked against any [`InputLimits`] never panics or overflows; every
//! event accepted is inside the limits; and every rejection is the typed
//! error that names the limit broken. And the terminal decoder:
//! any bytes a terminal sends make only the keys it sends, never a panic or
//! an overflow, and the end of input leaves it between keys.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, encode};

use crate::{InputError, InputEvent, InputLimits};

/// The record length of an input event.
const RECORD_LEN: usize = HEADER_LEN + InputEvent::PAYLOAD_LEN;

/// A record with an input event's header in front of `payload`.
fn record(payload: [u8; InputEvent::PAYLOAD_LEN]) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    let header = Header {
        kind: InputEvent::KIND,
        version: InputEvent::VERSION,
        payload_len: InputEvent::PAYLOAD_LEN as u16,
    };
    assert!(header.write(&mut record).is_ok());
    record[HEADER_LEN..].copy_from_slice(&payload);
    record
}

/// Any limits: any screen size (`None` only when a side is zero), any
/// number of buttons and of touch contacts.
fn any_limits() -> Option<InputLimits> {
    let width: u16 = kani::any();
    let height: u16 = kani::any();
    let limits = InputLimits::new(width, height);
    assert!(limits.is_none() == (width == 0 || height == 0));
    limits.map(|limits| {
        limits
            .with_buttons(kani::any())
            .with_touch_points(kani::any())
    })
}

/// `(x, y)` is on the limits' screen.
fn on_screen(limits: &InputLimits, x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && x < i32::from(limits.width()) && y < i32::from(limits.height())
}

/// `event` is within `limits`.
fn within(limits: &InputLimits, event: InputEvent) -> bool {
    match event {
        InputEvent::KeyDown(_) | InputEvent::KeyUp(_) => true,
        InputEvent::PointerMove { x, y } => on_screen(limits, x, y),
        InputEvent::PointerButton { button, .. } => button < limits.buttons(),
        InputEvent::Touch { id, x, y, .. } => id < limits.touch_points() && on_screen(limits, x, y),
    }
}

/// Every payload either fails to decode with a typed error or decodes to an
/// event whose encoding is exactly those bytes: one encoding per event.
#[kani::proof]
#[kani::unwind(21)]
fn any_payload_decodes_to_its_one_encoding() {
    let bytes = record(kani::any());
    if let Ok(event) = nowayos_ipc::decode_exact::<InputEvent>(&bytes) {
        let mut out = [0u8; RECORD_LEN];
        assert!(encode(&event, &mut out) == Ok(RECORD_LEN));
        assert!(out == bytes);
    }
}

/// For any payload and any limits: `validate` accepts an event only when
/// it is within the limits, and otherwise names the limit it breaks.
#[kani::proof]
#[kani::unwind(21)]
fn validate_accepts_only_events_within_any_limits() {
    let Some(limits) = any_limits() else {
        return;
    };
    let Ok(event) = nowayos_ipc::decode_exact::<InputEvent>(&record(kani::any())) else {
        return;
    };
    match limits.validate(event) {
        Ok(accepted) => {
            assert!(accepted == event);
            assert!(within(&limits, accepted));
        }
        Err(InputError::UnknownButton { button }) => {
            assert!(matches!(event, InputEvent::PointerButton { button: b, .. } if b == button));
            assert!(button >= limits.buttons());
        }
        Err(InputError::TouchIdOutOfRange { id }) => {
            assert!(matches!(event, InputEvent::Touch { id: i, .. } if i == id));
            assert!(id >= limits.touch_points());
        }
        Err(InputError::OutsideScreen { x, y }) => {
            assert!(matches!(
                event,
                InputEvent::PointerMove { x: ex, y: ey } | InputEvent::Touch { x: ex, y: ey, .. }
                    if ex == x && ey == y
            ));
            assert!(!on_screen(&limits, x, y));
        }
    }
}

/// For any payload and any limits: `normalise` either refuses an event whose
/// button or contact number is out of range, naming it, or returns an event
/// within the limits.
#[kani::proof]
#[kani::unwind(21)]
fn normalise_returns_only_events_within_any_limits() {
    let Some(limits) = any_limits() else {
        return;
    };
    let Ok(event) = nowayos_ipc::decode_exact::<InputEvent>(&record(kani::any())) else {
        return;
    };
    match limits.normalise(event) {
        Ok(fixed) => assert!(within(&limits, fixed)),
        Err(InputError::UnknownButton { button }) => assert!(button >= limits.buttons()),
        Err(InputError::TouchIdOutOfRange { id }) => assert!(id >= limits.touch_points()),
        Err(InputError::OutsideScreen { .. }) => {
            panic!("normalise clamps positions; it never refuses one")
        }
    }
}

/// A press the terminal decoder may make: a key of the main block -
/// the letters, digits and punctuation, Enter, Escape, Backspace, Tab and
/// Space, all 0x04 to 0x38 - or an arrow, 0x4F to 0x52; with Shift, only a
/// key that types a character, or Tab.
fn a_terminal_key(press: crate::terminal::Press) -> bool {
    let usage = press.key().usage();
    let main_block = (0x04..=0x38).contains(&usage) && usage != 0x32;
    let arrow = (0x4F..=0x52).contains(&usage);
    let shiftable = main_block && !(0x28..=0x2A).contains(&usage) && usage != 0x2C;
    (main_block || arrow) && (!press.shift() || shiftable)
}

/// For any six bytes from a terminal, then the end of input: the decoder
/// never panics or overflows, makes only a key it sends, makes Escape and
/// nothing else at the end of input, and is left between keys.
#[kani::proof]
#[kani::unwind(8)]
fn any_terminal_bytes_make_only_terminal_keys() {
    let bytes: [u8; 6] = kani::any();
    let mut decoder = crate::terminal::Decoder::new();
    for byte in bytes {
        for press in decoder.push(byte) {
            assert!(a_terminal_key(press));
        }
    }
    for press in decoder.end_of_input() {
        assert!(press == crate::terminal::Press::new(crate::KeyCode::ESCAPE, false));
    }
    assert!(!decoder.pending());
}
