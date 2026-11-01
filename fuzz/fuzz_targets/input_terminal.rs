// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: a terminal's bytes through `nowayos_input::terminal` and the
//! input broker - the development route from the debug UART to the UI.
//!
//! The fuzzer's bytes are a stream from the terminal, cut into bursts: each
//! burst's length comes first, then its bytes, and the line goes quiet
//! (`end_of_input`) after each. Checks: nothing panics; drop counts only
//! grow; every press is a key the decoder sends, and its events are that
//! key down and up, inside Left Shift if Shift is held; the end of input
//! makes nothing but Escape and leaves the decoder between keys; and every
//! event passes the broker's checks on an 800x480 screen, each counted.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_input::broker::{Broker, Verdict};
use nowayos_input::terminal::{Decoder, Press};
use nowayos_input::{InputEvent, InputLimits, KeyCode};

/// A key the decoder sends: the main block (letters, digits, punctuation,
/// Enter, Escape, Backspace, Tab, Space; 0x04 to 0x38 but 0x32) or an
/// arrow; with Shift, a key that types a character, or Tab.
fn a_terminal_key(press: Press) -> bool {
    let usage = press.key().usage();
    let main_block = (0x04..=0x38).contains(&usage) && usage != 0x32;
    let arrow = (0x4F..=0x52).contains(&usage);
    let shiftable = main_block && !(0x28..=0x2A).contains(&usage) && usage != 0x2C;
    (main_block || arrow) && (!press.shift() || shiftable)
}

/// `press`'s events, as it gives them, are its key down and up, inside Left
/// Shift if Shift is held.
fn whole(press: Press) -> bool {
    let key = press.key();
    let events: Vec<InputEvent> = press.events().collect();
    let expected: &[InputEvent] = if press.shift() {
        &[
            InputEvent::KeyDown(KeyCode::LEFT_SHIFT),
            InputEvent::KeyDown(key),
            InputEvent::KeyUp(key),
            InputEvent::KeyUp(KeyCode::LEFT_SHIFT),
        ]
    } else {
        &[InputEvent::KeyDown(key), InputEvent::KeyUp(key)]
    };
    events == expected && events.len() == press.event_count()
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut decoder = Decoder::new();
    let limits = InputLimits::new(800, 480);
    // A broker that never runs out of tokens: every event is checked.
    let mut broker = limits.and_then(|limits| Broker::<1>::new(limits, u32::MAX, 1));
    let mut sent = 0u32;
    let mut now = 0u64;
    let mut admit = |press: Press| {
        assert!(a_terminal_key(press), "{press:?}");
        assert!(whole(press), "{press:?}");
        for event in press.events() {
            now = now.saturating_add(1);
            sent = sent.saturating_add(1);
            if let Some(broker) = broker.as_mut() {
                let verdict = broker.admit(0, event, now);
                assert_eq!(verdict, Verdict::Forward(event), "{event:?}");
            }
        }
    };
    while !input.is_empty() {
        let burst = usize::from(input.byte());
        for _ in 0..burst {
            if input.is_empty() {
                break;
            }
            let before = decoder.dropped();
            for press in decoder.push(input.byte()) {
                admit(press);
            }
            let after = decoder.dropped();
            assert!(
                after.unknown_bytes >= before.unknown_bytes
                    && after.unknown_sequences >= before.unknown_sequences
                    && after.cut_short >= before.cut_short,
                "drop counts went down: {before:?} to {after:?}"
            );
        }
        for press in decoder.end_of_input() {
            assert_eq!(press, Press::new(KeyCode::ESCAPE, false));
            admit(press);
        }
        assert!(!decoder.pending());
    }
    if let Some(broker) = broker {
        assert_eq!(broker.counts(0).unwrap_or_default().forwarded, sent);
    }
});
