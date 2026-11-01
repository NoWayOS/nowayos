// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests: every event round-trips, arbitrary payloads never panic
//! the decoder, normalisation always lands within limits, and the rate
//! limiter never lets through more than its budget.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_input::{InputEvent, InputLimits, KeyCode, RateLimiter, TouchPhase};
use nowayos_ipc::{HEADER_LEN, Message, decode_exact, encode};
use proptest::prelude::*;

fn key() -> impl Strategy<Value = KeyCode> {
    prop_oneof![0x04u16..=0xA4, 0xE0u16..=0xE7].prop_map(|u| KeyCode::new(u).unwrap())
}

fn phase() -> impl Strategy<Value = TouchPhase> {
    prop_oneof![
        Just(TouchPhase::Start),
        Just(TouchPhase::Move),
        Just(TouchPhase::End),
        Just(TouchPhase::Cancel),
    ]
}

fn event() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        key().prop_map(InputEvent::KeyDown),
        key().prop_map(InputEvent::KeyUp),
        (any::<i32>(), any::<i32>()).prop_map(|(x, y)| InputEvent::PointerMove { x, y }),
        (any::<u8>(), any::<bool>())
            .prop_map(|(button, down)| InputEvent::PointerButton { button, down }),
        (any::<u8>(), any::<i32>(), any::<i32>(), phase())
            .prop_map(|(id, x, y, phase)| InputEvent::Touch { id, x, y, phase }),
    ]
}

const RECORD: usize = HEADER_LEN + InputEvent::PAYLOAD_LEN;

proptest! {
    #[test]
    fn round_trip(e in event()) {
        let mut buf = [0u8; RECORD];
        encode(&e, &mut buf).unwrap();
        prop_assert_eq!(decode_exact::<InputEvent>(&buf), Ok(e));
    }

    #[test]
    fn arbitrary_payloads_never_panic_and_are_canonical(payload in any::<[u8; 12]>()) {
        let mut buf = [0u8; RECORD];
        encode(&InputEvent::KeyDown(KeyCode::A), &mut buf).unwrap();
        buf[HEADER_LEN..].copy_from_slice(&payload);
        if let Ok(e) = decode_exact::<InputEvent>(&buf) {
            let mut again = [0u8; RECORD];
            encode(&e, &mut again).unwrap();
            prop_assert_eq!(again, buf);
        }
    }

    #[test]
    fn normalised_events_pass_validation(
        e in event(),
        w in 1u16..=4096,
        h in 1u16..=4096,
        buttons in 0u8..=8,
        touch in 0u8..=16,
    ) {
        let limits = InputLimits::new(w, h).unwrap().with_buttons(buttons).with_touch_points(touch);
        if let Ok(n) = limits.normalise(e) {
            prop_assert_eq!(limits.validate(n), Ok(n));
            prop_assert_eq!(limits.normalise(n), Ok(n)); // idempotent
        }
        if let Ok(v) = limits.validate(e) {
            prop_assert_eq!(v, e);
            prop_assert_eq!(limits.normalise(e), Ok(e));
        }
    }

    /// Over any sequence of non-decreasing ticks, accepted events never
    /// exceed the burst plus the tokens the elapsed time can earn.
    #[test]
    fn rate_limiter_respects_its_budget(
        burst in 1u32..=8,
        period in 1u64..=50,
        steps in proptest::collection::vec(0u64..=30, 1..200),
    ) {
        let mut limiter = RateLimiter::new(burst, period).unwrap();
        let mut now = 0u64;
        let mut accepted = 0u64;
        for step in &steps {
            now += step;
            if limiter.allow(now) {
                accepted += 1;
            }
        }
        prop_assert!(accepted <= u64::from(burst) + now / period);
        prop_assert_eq!(accepted + u64::from(limiter.dropped()), steps.len() as u64);
    }

    /// Ticks may also go backwards (a restarted tick source). Tokens are
    /// then earned only by forward movement, so accepted events never
    /// exceed the burst plus what the total forward movement can earn.
    #[test]
    fn rate_limiter_earns_only_from_forward_movement(
        burst in 1u32..=8,
        period in 1u64..=50,
        steps in proptest::collection::vec(-40i64..=30, 1..200),
    ) {
        let mut limiter = RateLimiter::new(burst, period).unwrap();
        let mut now = 1_000u64;
        let mut forward = 0u64;
        let mut accepted = 0u64;
        for step in &steps {
            let next = now.saturating_add_signed(*step);
            forward += next.saturating_sub(now);
            now = next;
            if limiter.allow(now) {
                accepted += 1;
            }
        }
        prop_assert!(accepted <= u64::from(burst) + forward / period);
    }
}
