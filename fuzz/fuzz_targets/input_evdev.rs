// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: a virtio input device's events through `nowayos_input::evdev`
//! and the input broker - the path every key, click and pointer move takes
//! from an untrusted device to the UI.
//!
//! The fuzzer's bytes are the screen, the broker's rate and a stream of
//! 8-byte device events, each from one of two sources or an unknown third,
//! at ticks that move on or jump back. Checks: nothing panics; drop counts
//! only grow; whatever the translator makes is a key HID defines, a button
//! from 0 to 2 or a position on the screen; the broker forwards only events
//! within the screen's limits, and each source's counts add up to what it
//! sent.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_input::broker::{Broker, Verdict};
use nowayos_input::evdev::{EVENT_LEN, Translator};
use nowayos_input::{InputEvent, InputLimits, KeyCode};

/// The broker's sources: the keyboard and the tablet, as in QEMU.
const SOURCES: usize = 2;

/// Whether `position` lies on an axis of `size` pixels (0 counts as 1, as
/// the translator puts everything at 0 on an axis with no pixels).
fn on_axis(position: i32, size: u16) -> bool {
    u32::try_from(position).is_ok_and(|position| position < u32::from(size.max(1)))
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let (width, height) = (input.u16(), input.u16());
    let (burst, ticks_per_token) = (u32::from(input.byte()), u64::from(input.u16()));
    let mut translator = Translator::new(u32::from(width), u32::from(height));
    let limits = InputLimits::new(width, height);
    let mut broker =
        limits.and_then(|limits| Broker::<SOURCES>::new(limits, burst, ticks_per_token));
    let mut sent = [0u32; SOURCES];
    let mut now = 0u64;
    while !input.is_empty() {
        let source = usize::from(input.byte() % 3);
        now = match input.byte() {
            0 => now.saturating_sub(u64::from(input.u16())),
            step => now.saturating_add(u64::from(step)),
        };
        let before = translator.dropped();
        let translated = translator.translate(&input.array::<EVENT_LEN>());
        let after = translator.dropped();
        assert!(
            after.unknown_type >= before.unknown_type
                && after.unknown_code >= before.unknown_code
                && after.out_of_range >= before.out_of_range,
            "drop counts went down: {before:?} to {after:?}"
        );
        let Some(event) = translated else {
            continue;
        };
        let valid = match event {
            InputEvent::KeyDown(key) | InputEvent::KeyUp(key) => {
                KeyCode::new(key.usage()) == Some(key)
            }
            InputEvent::PointerButton { button, .. } => button <= 2,
            InputEvent::PointerMove { x, y } => on_axis(x, width) && on_axis(y, height),
            InputEvent::Touch { .. } => false,
        };
        assert!(valid, "{event:?} on a {width}x{height} screen");
        let (Some(broker), Some(limits)) = (broker.as_mut(), limits) else {
            continue;
        };
        let verdict = broker.admit(source, event, now);
        let consistent = match (verdict, sent.get_mut(source)) {
            (Verdict::UnknownSource, None) => true,
            (Verdict::UnknownSource, Some(_)) | (_, None) => false,
            (verdict, Some(count)) => {
                *count = count.saturating_add(1);
                match verdict {
                    Verdict::Forward(forwarded) => limits.validate(forwarded) == Ok(forwarded),
                    Verdict::Invalid(error) => limits.normalise(event) == Err(error),
                    _ => true,
                }
            }
        };
        assert!(consistent, "{event:?} from source {source}: {verdict:?}");
    }
    if let Some(broker) = broker {
        for (source, sent) in sent.into_iter().enumerate() {
            let counts = broker.counts(source).unwrap_or_default();
            let total = counts
                .forwarded
                .saturating_add(counts.rate_limited)
                .saturating_add(counts.invalid);
            assert_eq!(total, sent, "source {source}: {counts:?}");
        }
    }
});
