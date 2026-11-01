// SPDX-License-Identifier: MIT OR Apache-2.0
//! The input broker's decisions: one stream of canonical events from
//! several input sources, each event checked against the screen and each
//! source held to its own rate.
//!
//! The broker PD copies every record out of its sources' rings and decodes
//! it (`nowayos-ipc` refuses malformed records before this module sees
//! them); then [`Broker::admit`] decides, per source:
//!
//! 1. the event is normalised against the screen ([`InputLimits::normalise`]):
//!    positions clamped onto it, an impossible button or touch contact
//!    refused and counted as invalid;
//! 2. the source's own token bucket ([`RateLimiter`]) lets it through, or
//!    it is dropped and counted as over the rate.
//!
//! So a hostile or faulty device can flood or confuse only its own source:
//! the others keep their own buckets and are unaffected. The PD counts what
//! this module never sees - malformed records, and events that passed but
//! did not fit the UI's ring - with [`Broker::record_malformed`] and
//! [`Broker::record_undelivered`], so each source's counts are in one place.

use crate::{InputError, InputEvent, InputLimits, RateLimiter};

/// Slots in the ring through which the UI receives its input - from the
/// broker, or from a scripted input PD: room for a burst of events between
/// frames. The UI's side is `nowayos-ui-loop`'s `INPUT_SLOTS`.
pub const UI_RING_SLOTS: usize = 64;

/// Bytes per slot of the UI's input ring: an `InputEvent` record (an 8-byte
/// header and a 12-byte payload), rounded up. The UI's side is
/// `nowayos-ui-loop`'s `INPUT_SLOT`.
pub const UI_RING_SLOT: usize = 32;

/// What a source has sent the broker, and what became of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceCounts {
    /// Events forwarded to the UI.
    pub forwarded: u32,
    /// Events dropped because the source was over its rate.
    pub rate_limited: u32,
    /// Events refused by the screen limits.
    pub invalid: u32,
    /// Records refused before decoding (malformed): counted by the PD.
    pub malformed: u32,
    /// Events that passed but did not fit the UI's ring: counted by the PD.
    pub undelivered: u32,
}

impl SourceCounts {
    /// Everything dropped, whatever the reason: when this changes, the PD
    /// has something to report.
    #[must_use]
    pub const fn dropped(&self) -> (u32, u32, u32, u32) {
        (
            self.rate_limited,
            self.invalid,
            self.malformed,
            self.undelivered,
        )
    }
}

/// What [`Broker::admit`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Forward this event - normalised - to the UI.
    Forward(InputEvent),
    /// The source is over its rate: dropped and counted.
    RateLimited,
    /// The event breaks the screen limits: dropped and counted.
    Invalid(InputError),
    /// No such source.
    UnknownSource,
}

/// The broker's state for `SOURCES` input sources.
#[derive(Clone, Copy, Debug)]
pub struct Broker<const SOURCES: usize> {
    limits: InputLimits,
    limiters: [RateLimiter; SOURCES],
    counts: [SourceCounts; SOURCES],
}

impl<const SOURCES: usize> Broker<SOURCES> {
    /// A broker for a screen with `limits`, each source allowed bursts of
    /// `burst` events, refilled at one event per `ticks_per_token` ticks of
    /// the caller's clock. `None` if either is zero.
    #[must_use]
    pub const fn new(limits: InputLimits, burst: u32, ticks_per_token: u64) -> Option<Self> {
        let Some(limiter) = RateLimiter::new(burst, ticks_per_token) else {
            return None;
        };
        Some(Self {
            limits,
            limiters: [limiter; SOURCES],
            counts: [SourceCounts {
                forwarded: 0,
                rate_limited: 0,
                invalid: 0,
                malformed: 0,
                undelivered: 0,
            }; SOURCES],
        })
    }

    /// Decides what happens to `event` from `source`, arriving at tick `now`.
    pub fn admit(&mut self, source: usize, event: InputEvent, now: u64) -> Verdict {
        let (Some(limiter), Some(counts)) =
            (self.limiters.get_mut(source), self.counts.get_mut(source))
        else {
            return Verdict::UnknownSource;
        };
        let event = match self.limits.normalise(event) {
            Ok(event) => event,
            Err(error) => {
                counts.invalid = counts.invalid.saturating_add(1);
                return Verdict::Invalid(error);
            }
        };
        if !limiter.allow(now) {
            counts.rate_limited = counts.rate_limited.saturating_add(1);
            return Verdict::RateLimited;
        }
        counts.forwarded = counts.forwarded.saturating_add(1);
        Verdict::Forward(event)
    }

    /// Counts a record from `source` refused before decoding.
    pub fn record_malformed(&mut self, source: usize) {
        if let Some(counts) = self.counts.get_mut(source) {
            counts.malformed = counts.malformed.saturating_add(1);
        }
    }

    /// Counts an event from `source` that [`admit`](Self::admit) forwarded
    /// but that did not fit the UI's ring: it moves from `forwarded` to
    /// `undelivered`.
    pub fn record_undelivered(&mut self, source: usize) {
        if let Some(counts) = self.counts.get_mut(source) {
            counts.forwarded = counts.forwarded.saturating_sub(1);
            counts.undelivered = counts.undelivered.saturating_add(1);
        }
    }

    /// What `source` has sent so far.
    #[must_use]
    pub fn counts(&self, source: usize) -> Option<SourceCounts> {
        self.counts.get(source).copied()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::KeyCode;

    fn broker() -> Broker<2> {
        Broker::new(InputLimits::new(800, 480).unwrap(), 4, 10).unwrap()
    }

    #[test]
    fn events_are_normalised_against_the_screen() {
        let mut b = broker();
        assert_eq!(
            b.admit(0, InputEvent::PointerMove { x: 900, y: -5 }, 0),
            Verdict::Forward(InputEvent::PointerMove { x: 799, y: 0 })
        );
        assert!(matches!(
            b.admit(
                0,
                InputEvent::PointerButton {
                    button: 9,
                    down: true
                },
                0
            ),
            Verdict::Invalid(InputError::UnknownButton { button: 9 })
        ));
        assert_eq!(b.counts(0).unwrap().invalid, 1);
        assert_eq!(
            b.admit(5, InputEvent::KeyDown(KeyCode::A), 0),
            Verdict::UnknownSource
        );
    }

    #[test]
    fn a_flood_from_one_source_leaves_the_other_alone() {
        let mut b = broker();
        let key = InputEvent::KeyDown(KeyCode::Z);
        let forwarded = (0..100)
            .filter(|_| matches!(b.admit(1, key, 0), Verdict::Forward(_)))
            .count();
        assert_eq!(
            forwarded, 4,
            "the burst, then nothing until the bucket refills"
        );
        assert_eq!(b.counts(1).unwrap().rate_limited, 96);
        for _ in 0..4 {
            assert!(matches!(
                b.admit(0, InputEvent::KeyDown(KeyCode::A), 0),
                Verdict::Forward(_)
            ));
        }
        assert_eq!(b.counts(0).unwrap().rate_limited, 0);
        assert!(
            matches!(b.admit(1, key, 10), Verdict::Forward(_)),
            "one token refilled"
        );
        b.record_malformed(1);
        b.record_undelivered(1);
        let counts = b.counts(1).unwrap();
        assert_eq!(
            (counts.forwarded, counts.malformed, counts.undelivered),
            (4, 1, 1)
        );
        assert_eq!(counts.dropped(), (96, 0, 1, 1));
        assert_eq!(
            b.counts(0).unwrap().dropped(),
            (0, 0, 0, 0),
            "source 0 unaffected"
        );
        b.record_malformed(7);
        assert_eq!(b.counts(7), None);
    }

    proptest! {
        #[test]
        fn whatever_arrives_the_forwarded_events_are_on_the_screen(x in any::<i32>(), y in any::<i32>(), button in any::<u8>(), source in 0usize..3, now in any::<u64>()) {
            let mut b = broker();
            for event in [InputEvent::PointerMove { x, y }, InputEvent::PointerButton { button, down: true }] {
                if let Verdict::Forward(forwarded) = b.admit(source, event, now) {
                    prop_assert_eq!(b.limits.validate(forwarded), Ok(forwarded));
                }
            }
        }
    }
}
