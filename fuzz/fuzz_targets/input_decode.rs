// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-input`'s canonical events - any record decoded as an
//! `InputEvent`, then validated and normalised against a screen and device
//! limits the fuzzer chooses too, as the input broker and the UI do.
//!
//! Checks: decoding never panics, agrees with itself and is canonical
//! (`nowayos_fuzz::check_decode`); a key that decodes is one HID defines,
//! and the key map takes it with or without Shift; an event `validate`
//! accepts, `normalise` leaves alone; `normalise` refuses exactly the button
//! and contact numbers `validate` refuses, and `validate` accepts whatever
//! it returns; and relative pointer motion, however large, stays on the
//! screen.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_input::keymap::us_char;
use nowayos_input::{InputError, InputEvent, InputLimits, KeyCode, PointerTracker};

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let framed = input.byte() % 2 == 1;
    let (width, height) = (input.u16(), input.u16());
    let (buttons, touch_points) = (input.byte(), input.byte());
    let (dx, dy) = (input.i32(), input.i32());
    let mut buf = [0u8; FRAME_MAX];
    let record = if framed {
        frame::<InputEvent>(input.rest(), &mut buf)
    } else {
        input.rest()
    };
    check_header(record);
    let Some(event) = check_decode::<InputEvent>(record) else {
        return;
    };
    if let InputEvent::KeyDown(key) | InputEvent::KeyUp(key) = event {
        assert_eq!(KeyCode::new(key.usage()), Some(key), "{event:?}");
        let _ = (us_char(key, false), us_char(key, true));
    }
    let Some(limits) = InputLimits::new(width, height) else {
        assert!(width == 0 || height == 0, "{width}x{height} refused");
        return;
    };
    let limits = limits.with_buttons(buttons).with_touch_points(touch_points);
    let validated = limits.validate(event);
    let normalised = limits.normalise(event);
    let consistent = match (validated, normalised) {
        (Ok(valid), Ok(normal)) => valid == event && normal == event,
        (Err(InputError::OutsideScreen { .. }), Ok(normal)) => {
            limits.validate(normal) == Ok(normal)
        }
        (Err(invalid), Err(refused)) => invalid == refused,
        _ => false,
    };
    assert!(
        consistent,
        "{event:?} on {limits:?}: validate {validated:?}, normalise {normalised:?}"
    );
    let mut pointer = PointerTracker::new(limits);
    let moved = pointer.apply_delta(dx, dy);
    assert_eq!(limits.validate(moved), Ok(moved), "{limits:?}");
    if let InputEvent::PointerMove { x, y } = event {
        let moved = pointer.apply_delta(x, y);
        assert_eq!(limits.validate(moved), Ok(moved), "{limits:?}");
    }
});
