// SPDX-License-Identifier: MIT OR Apache-2.0
//! Canonical input events as text: what the page sends, and what a session
//! file keeps.
//!
//! One event is a few words - `kd 40` (key down, HID usage 40), `ku 40`,
//! `pm 320 200` (pointer to x, y in the frame's pixels), `pb 0 1` (button 0
//! down), `wh -1` (wheel, one notch towards the user), `ts 0 320 200`
//! (touch 0 starts; `tm` moves, `te` ends, `tc` is cancelled). A batch is
//! events separated by `;` or new lines: what an input PD delivers before
//! the application settles.
//!
//! The text comes from a browser: anything that is not exactly an event is
//! refused, and a batch is bounded before it is read.

use core::fmt::Write as _;

use nowayos_input::{InputEvent, KeyCode, TouchPhase};

/// The most events in one batch.
pub const MAX_BATCH: usize = 256;
/// The longest event, in bytes.
pub const MAX_EVENT: usize = 48;

/// Why a batch was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchError {
    /// More than [`MAX_BATCH`] events.
    TooMany,
    /// An event longer than [`MAX_EVENT`] bytes, or not an event.
    NotAnEvent,
}

/// One event from its text, or `None` if it is not exactly one.
#[must_use]
pub fn parse_event(text: &str) -> Option<InputEvent> {
    if text.len() > MAX_EVENT {
        return None;
    }
    let mut words = text.split_ascii_whitespace();
    let kind = words.next()?;
    let mut word = || words.next();
    let event = match kind {
        "kd" => InputEvent::KeyDown(key(word()?)?),
        "ku" => InputEvent::KeyUp(key(word()?)?),
        "pm" => InputEvent::PointerMove {
            x: word()?.parse().ok()?,
            y: word()?.parse().ok()?,
        },
        "pb" => InputEvent::PointerButton {
            button: word()?.parse().ok()?,
            down: match word()? {
                "1" => true,
                "0" => false,
                _ => return None,
            },
        },
        "wh" => InputEvent::Wheel {
            notches: word()?.parse().ok()?,
        },
        "ts" | "tm" | "te" | "tc" => InputEvent::Touch {
            id: word()?.parse().ok()?,
            x: word()?.parse().ok()?,
            y: word()?.parse().ok()?,
            phase: match kind {
                "ts" => TouchPhase::Start,
                "tm" => TouchPhase::Move,
                "te" => TouchPhase::End,
                _ => TouchPhase::Cancel,
            },
        },
        _ => return None,
    };
    if words.next().is_some() {
        return None;
    }
    Some(event)
}

/// A key from its HID usage in decimal.
fn key(word: &str) -> Option<KeyCode> {
    KeyCode::new(word.parse().ok()?)
}

/// A batch from its text: events separated by `;` or new lines, empty
/// pieces ignored.
///
/// # Errors
///
/// A [`BatchError`] if there are too many events or one is not an event;
/// then no event of the batch is used.
pub fn parse_batch(text: &str) -> Result<Vec<InputEvent>, BatchError> {
    let mut events = Vec::new();
    for piece in text.split([';', '\n', '\r']) {
        if piece.trim().is_empty() {
            continue;
        }
        if events.len() >= MAX_BATCH {
            return Err(BatchError::TooMany);
        }
        events.push(parse_event(piece).ok_or(BatchError::NotAnEvent)?);
    }
    Ok(events)
}

/// A batch as text, events separated by `;`: what [`parse_batch`] reads
/// back to the same events.
#[must_use]
pub fn format_batch(events: &[InputEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if !text.is_empty() {
            text.push(';');
        }
        // Writing to a String cannot fail.
        let _ = match *event {
            InputEvent::KeyDown(key) => write!(text, "kd {}", key.usage()),
            InputEvent::KeyUp(key) => write!(text, "ku {}", key.usage()),
            InputEvent::PointerMove { x, y } => write!(text, "pm {x} {y}"),
            InputEvent::PointerButton { button, down } => {
                write!(text, "pb {button} {}", u8::from(down))
            }
            InputEvent::Wheel { notches } => write!(text, "wh {notches}"),
            InputEvent::Touch { id, x, y, phase } => {
                let kind = match phase {
                    TouchPhase::Start => "ts",
                    TouchPhase::Move => "tm",
                    TouchPhase::End => "te",
                    TouchPhase::Cancel => "tc",
                };
                write!(text, "{kind} {id} {x} {y}")
            }
        };
    }
    text
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed step fails the test"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn each_kind_of_event_reads() {
        assert_eq!(
            parse_event("kd 40"),
            Some(InputEvent::KeyDown(KeyCode::ENTER))
        );
        assert_eq!(parse_event("ku 43"), Some(InputEvent::KeyUp(KeyCode::TAB)));
        assert_eq!(
            parse_event("pm 320 -5"),
            Some(InputEvent::PointerMove { x: 320, y: -5 })
        );
        assert_eq!(
            parse_event("pb 0 1"),
            Some(InputEvent::PointerButton {
                button: 0,
                down: true
            })
        );
        assert_eq!(
            parse_event("wh -1"),
            Some(InputEvent::Wheel { notches: -1 })
        );
        assert_eq!(
            parse_event("te 1 10 20"),
            Some(InputEvent::Touch {
                id: 1,
                x: 10,
                y: 20,
                phase: TouchPhase::End
            })
        );
    }

    #[test]
    fn what_is_not_exactly_an_event_is_refused() {
        for text in [
            "",
            "kd",
            "kd 0",
            "kd 3",
            "kd 40 1",
            "kd x",
            "pb 0 2",
            "pm 1",
            "wh 200",
            "zz 1",
            "kd 99999999999999999999",
        ] {
            assert_eq!(parse_event(text), None, "{text:?}");
        }
        assert_eq!(parse_event(&"kd 40 ".repeat(20)), None);
    }

    #[test]
    fn a_batch_is_bounded_and_all_or_nothing() {
        assert_eq!(parse_batch("kd 40;ku 40\n\npm 1 2\r\n").unwrap().len(), 3);
        assert_eq!(parse_batch("kd 40;nonsense"), Err(BatchError::NotAnEvent));
        let many = "kd 40;".repeat(MAX_BATCH + 1);
        assert_eq!(parse_batch(&many), Err(BatchError::TooMany));
        assert_eq!(
            parse_batch(&"kd 40;".repeat(MAX_BATCH)).unwrap().len(),
            MAX_BATCH
        );
    }

    fn any_event() -> impl Strategy<Value = InputEvent> {
        let key = (0x04u16..=0xA4).prop_map(|usage| KeyCode::new(usage).unwrap());
        prop_oneof![
            key.clone().prop_map(InputEvent::KeyDown),
            key.prop_map(InputEvent::KeyUp),
            (any::<i32>(), any::<i32>()).prop_map(|(x, y)| InputEvent::PointerMove { x, y }),
            (any::<u8>(), any::<bool>())
                .prop_map(|(button, down)| InputEvent::PointerButton { button, down }),
            any::<i8>().prop_map(|notches| InputEvent::Wheel { notches }),
            (any::<u8>(), any::<i32>(), any::<i32>(), 0u8..4).prop_map(|(id, x, y, phase)| {
                InputEvent::Touch {
                    id,
                    x,
                    y,
                    phase: [
                        TouchPhase::Start,
                        TouchPhase::Move,
                        TouchPhase::End,
                        TouchPhase::Cancel,
                    ][usize::from(phase)],
                }
            }),
        ]
    }

    proptest! {
        #[test]
        fn a_batch_written_reads_back(events in proptest::collection::vec(any_event(), 0..40)) {
            prop_assert_eq!(parse_batch(&format_batch(&events)).unwrap(), events);
        }

        #[test]
        fn any_text_is_read_or_refused(text in ".{0,400}") {
            let _ = parse_batch(&text);
            let _ = parse_event(&text);
        }
    }
}
