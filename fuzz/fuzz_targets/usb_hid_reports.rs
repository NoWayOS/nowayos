// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: HID boot-protocol reports from a USB keyboard and mouse,
//! as the USB host PD turns them into canonical input events
//! (`nowayos_usb::hid`). A device is not trusted, so its reports are any
//! bytes of any length the fuzzer chooses, one after another, then the
//! device goes. Checks:
//!
//! * no report panics, and none makes more events than its bound - 20 for
//!   a keyboard, 4 for a mouse;
//! * a keyboard never reports a key down that is already down, or up that
//!   is not; only keys HID defines come out; and when it goes, every key
//!   still down is released, so nothing is left held;
//! * a mouse's pointer stays on the screen - any size the fuzzer picks -
//!   and only buttons 0 to 2 change, each down only when up and up only
//!   when down; when it goes, every button still down is released.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_input::{InputEvent, InputLimits, KeyCode};
use nowayos_usb::hid::{Keyboard, MAX_KEYBOARD_EVENTS, MAX_MOUSE_EVENTS, Mouse, REPORT_MAX};

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let width = input.u16().max(1);
    let height = input.u16().max(1);
    let Some(limits) = InputLimits::new(width, height) else {
        return;
    };
    let mut keyboard = Keyboard::new();
    let mut mouse = Mouse::new(limits);
    let mut keys_down = [false; 256];
    let mut buttons_down = [false; 3];
    let mut report = [0u8; REPORT_MAX + 8];
    for _ in 0..32 {
        if input.is_empty() {
            break;
        }
        let to_keyboard = input.byte().is_multiple_of(2);
        let len = usize::from(input.byte())
            .checked_rem(report.len())
            .unwrap_or(0);
        for byte in report.iter_mut().take(len) {
            *byte = input.byte();
        }
        let bytes = report.get(..len).unwrap_or_default();
        let mut count = 0usize;
        if to_keyboard {
            keyboard.report(bytes, |event| {
                count = count.saturating_add(1);
                key(&mut keys_down, event);
            });
            assert!(
                count <= MAX_KEYBOARD_EVENTS,
                "{count} events from one keyboard report"
            );
        } else {
            mouse.report(bytes, |event| {
                count = count.saturating_add(1);
                pointer(&mut buttons_down, limits, event);
            });
            assert!(
                count <= MAX_MOUSE_EVENTS,
                "{count} events from one mouse report"
            );
        }
    }
    keyboard.release_all(|event| key(&mut keys_down, event));
    assert!(keys_down.iter().all(|down| !down), "a key left down");
    mouse.release_all(|event| pointer(&mut buttons_down, limits, event));
    assert!(buttons_down.iter().all(|down| !down), "a button left down");
});

/// Checks one keyboard event against the keys down, and records it.
fn key(down: &mut [bool; 256], event: InputEvent) {
    let (key, pressed) = match event {
        InputEvent::KeyDown(key) => (key, true),
        InputEvent::KeyUp(key) => (key, false),
        _ => {
            assert!(
                matches!(event, InputEvent::KeyDown(_) | InputEvent::KeyUp(_)),
                "{event:?} from a keyboard"
            );
            return;
        }
    };
    assert_eq!(KeyCode::new(key.usage()), Some(key));
    let Some(state) = down.get_mut(usize::from(key.usage())) else {
        assert!(key.usage() < 256, "key {key:?} beyond the keyboard page");
        return;
    };
    assert_ne!(*state, pressed, "{event:?} twice");
    *state = pressed;
}

/// Checks one mouse event against the screen and the buttons down, and
/// records it.
fn pointer(down: &mut [bool; 3], limits: InputLimits, event: InputEvent) {
    match event {
        InputEvent::PointerMove { x, y } => {
            assert!(x >= 0 && x < i32::from(limits.width()), "x {x}");
            assert!(y >= 0 && y < i32::from(limits.height()), "y {y}");
        }
        InputEvent::PointerButton {
            button,
            down: pressed,
        } => {
            let Some(state) = down.get_mut(usize::from(button)) else {
                assert!(button < 3, "button {button}");
                return;
            };
            assert_ne!(*state, pressed, "{event:?} twice");
            *state = pressed;
        }
        _ => assert!(
            matches!(
                event,
                InputEvent::PointerMove { .. } | InputEvent::PointerButton { .. }
            ),
            "{event:?} from a mouse"
        ),
    }
}
