// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: HID report descriptors, and the reports of the absolute
//! pointer one lays out - a touchscreen or a tablet
//! (`nowayos_usb::report`). A device is not trusted, so the descriptor is
//! the fuzzer's bytes, or a touchscreen's, a tablet's or a ten-contact
//! screen's with some bytes changed and cut to any length, and the reports
//! are the fuzzer's too. Checks:
//!
//! * parsing never panics and keeps every bound: at most 32 fields
//!   recorded, each 1 to 32 bits, none past the end of its report, and
//!   every report at most 64 bytes, its ID's byte included;
//! * a layout it finds has X and Y with a range, in one report;
//! * no report panics or makes more than 4 events; every position is on
//!   the screen, of any size the fuzzer picks; a touch starts only when
//!   none is in progress, moves and ends only while one is, and buttons go
//!   down only when up and up only when down; when the device goes, a
//!   touch in progress ends and every button is released.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_input::{InputEvent, InputLimits, TouchPhase};
use nowayos_usb::report::{
    AbsolutePointer, Layout, MAX_ABSOLUTE_EVENTS, MAX_FIELDS, REPORT_BITS, parse,
};

/// A single-touch touchscreen: report ID 2, tip switch, contact ID, X and Y.
const TOUCHSCREEN: [u8; 57] = [
    0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x22, 0xA1, 0x02, 0x09, 0x42, 0x09, 0x32,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03, 0x09, 0x51,
    0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x26, 0xFF,
    0x0F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0xC0, 0xC0,
];

/// A tablet: three buttons, padding, X and Y from 0 to 32767, a wheel - QEMU's
/// `usb-tablet`'s descriptor, as the USB host PD read it from the device.
const TABLET: [u8; 74] = [
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00, 0x46, 0xFF, 0x7F,
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x35, 0x00,
    0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0,
];

/// A ten-contact screen, as `nowayos_usb::report`'s tests lay it out:
/// report 1, a Finger collection for each contact - tip switch, padding,
/// contact identifier, X and Y - then the contact count.
fn multi_touch() -> Vec<u8> {
    let mut descriptor = vec![0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x01];
    for _ in 0..10 {
        descriptor.extend_from_slice(&[
            0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, 0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01,
            0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x09, 0x51, 0x25, 0x0F, 0x75, 0x08,
            0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x26, 0xFF, 0x0F, 0x75,
            0x10, 0x95, 0x02, 0x81, 0x02, 0xC0,
        ]);
    }
    descriptor.extend_from_slice(&[
        0x05, 0x0D, 0x09, 0x54, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xC0,
    ]);
    descriptor
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; 600];
    let screen = multi_touch();
    let len = match input.byte().checked_rem(4).unwrap_or(0) {
        0 => {
            let n = usize::from(input.u16()).min(buf.len());
            for byte in buf.iter_mut().take(n) {
                *byte = input.byte();
            }
            n
        }
        pick => {
            let base: &[u8] = match pick {
                1 => &TOUCHSCREEN,
                2 => &TABLET,
                _ => &screen,
            };
            for (to, from) in buf.iter_mut().zip(base) {
                *to = *from;
            }
            for _ in 0..input.byte().checked_rem(6).unwrap_or(0) {
                let at = usize::from(input.u16())
                    .checked_rem(base.len())
                    .unwrap_or(0);
                if let Some(byte) = buf.get_mut(at) {
                    *byte = input.byte();
                }
            }
            usize::from(input.u16()).min(base.len())
        }
    };
    let descriptor = buf.get(..len).unwrap_or_default();
    let Ok(fields) = parse(descriptor) else {
        return;
    };
    assert!(fields.as_slice().len() <= MAX_FIELDS);
    for field in fields.as_slice() {
        assert!((1..=32).contains(&field.bits));
        assert!(u32::from(field.bit).saturating_add(u32::from(field.bits)) <= REPORT_BITS);
        let report_bits = u32::from(fields.report_bytes(field.report_id)).saturating_mul(8);
        assert!(u32::from(field.bit).saturating_add(u32::from(field.bits)) <= report_bits);
    }
    for id in 0..=u8::MAX {
        assert!(fields.report_bytes(id) <= 64);
    }
    let Some(layout) = Layout::find(&fields) else {
        return;
    };
    assert!(layout.x.max > layout.x.min && layout.y.max > layout.y.min);
    assert!(layout.x.report_id == layout.y.report_id);
    assert_eq!(layout.report_bytes, fields.report_bytes(layout.report_id));
    let width = input.u16().max(1);
    let height = input.u16().max(1);
    let Some(limits) = InputLimits::new(width, height) else {
        return;
    };
    let mut pointer = AbsolutePointer::new(layout, limits);
    let mut touching = false;
    let mut buttons = [false; 3];
    let check = |event: InputEvent, touching: &mut bool, buttons: &mut [bool; 3]| match event {
        InputEvent::PointerMove { x, y } => {
            assert!(x >= 0 && x < i32::from(width) && y >= 0 && y < i32::from(height));
        }
        InputEvent::Touch { id, x, y, phase } => {
            assert_eq!(id, 0);
            assert!(x >= 0 && x < i32::from(width) && y >= 0 && y < i32::from(height));
            match phase {
                TouchPhase::Start => {
                    assert!(!*touching, "a touch started twice");
                    *touching = true;
                }
                TouchPhase::Move => assert!(*touching, "a move with no touch"),
                TouchPhase::End => {
                    assert!(*touching, "an end with no touch");
                    *touching = false;
                }
                TouchPhase::Cancel => assert!(*touching, "a cancel with no touch"),
            }
        }
        InputEvent::PointerButton { button, down } => {
            let state = buttons.get_mut(usize::from(button));
            assert!(
                state.as_ref().is_some_and(|state| **state != down),
                "{event:?}"
            );
            if let Some(state) = state {
                *state = down;
            }
        }
        _ => assert!(
            matches!(
                event,
                InputEvent::PointerMove { .. }
                    | InputEvent::Touch { .. }
                    | InputEvent::PointerButton { .. }
            ),
            "{event:?} from a pointer"
        ),
    };
    let mut report = [0u8; 64];
    for _ in 0..16 {
        if input.is_empty() {
            break;
        }
        let n = usize::from(input.byte())
            .checked_rem(report.len())
            .unwrap_or(0);
        for byte in report.iter_mut().take(n) {
            *byte = input.byte();
        }
        let mut count = 0usize;
        pointer.report(report.get(..n).unwrap_or_default(), |event| {
            count = count.saturating_add(1);
            check(event, &mut touching, &mut buttons);
        });
        assert!(count <= MAX_ABSOLUTE_EVENTS);
    }
    pointer.release_all(|event| check(event, &mut touching, &mut buttons));
    assert!(!touching, "a touch left in progress");
    assert!(buttons.iter().all(|down| !down), "a button left down");
});
