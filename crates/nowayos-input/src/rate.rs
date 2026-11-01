// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`RateLimiter`]: a token bucket driven by caller-supplied ticks.

/// Bounds how fast events are accepted: a token bucket holding up to
/// `burst` tokens, earning one token every `ticks_per_token` ticks.
///
/// Each accepted event spends a token; with none left, events are refused
/// and counted. A flood from a faulty or hostile device therefore costs the
/// input PD a counter increment per event, not a queue slot downstream.
///
/// Time is whatever monotonic tick the caller has (a timer PD's counter, a
/// frame count); this crate has no clock. A tick that goes backwards (for
/// example after the tick source restarts) earns nothing and restarts the
/// count from the new tick, so input is never locked out while the source
/// catches up with its old value; jitter smaller than `ticks_per_token`
/// therefore earns nothing either. Very large jumps simply refill the
/// bucket. None of this can panic or overflow.
///
/// ```
/// use nowayos_input::RateLimiter;
///
/// // Bursts of 3, then one event per 10 ticks.
/// let mut limiter = RateLimiter::new(3, 10).unwrap();
/// assert!(limiter.allow(0) && limiter.allow(0) && limiter.allow(0));
/// assert!(!limiter.allow(5)); // bucket empty
/// assert!(limiter.allow(10)); // one token earned
/// assert_eq!(limiter.dropped(), 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimiter {
    burst: u32,
    tokens: u32,
    ticks_per_token: u64,
    /// Tick up to which tokens have been credited; `None` until first use.
    credited_until: Option<u64>,
    dropped: u32,
}

impl RateLimiter {
    /// A full bucket of `burst` tokens, refilled at one token per
    /// `ticks_per_token` ticks. Returns `None` if either is zero.
    #[must_use]
    pub const fn new(burst: u32, ticks_per_token: u64) -> Option<Self> {
        if burst == 0 || ticks_per_token == 0 {
            return None;
        }
        Some(Self {
            burst,
            tokens: burst,
            ticks_per_token,
            credited_until: None,
            dropped: 0,
        })
    }

    /// Whether an event arriving at tick `now` may pass. Spends a token if
    /// so; counts a drop if not.
    pub fn allow(&mut self, now: u64) -> bool {
        self.refill(now);
        if let Some(left) = self.tokens.checked_sub(1) {
            self.tokens = left;
            true
        } else {
            self.dropped = self.dropped.saturating_add(1);
            false
        }
    }

    /// Whether `events` events arriving together at tick `now` may all
    /// pass: spends that many tokens if so, and none if not. A refusal is
    /// not counted as a drop - the caller holds the events back and asks
    /// again later - so a sender can meter whole groups of events (a key
    /// press with its Shift) below a receiver's own limiter.
    pub fn try_spend(&mut self, now: u64, events: u32) -> bool {
        self.refill(now);
        if let Some(left) = self.tokens.checked_sub(events) {
            self.tokens = left;
            true
        } else {
            false
        }
    }

    /// Tokens available right now (as of the last call).
    #[must_use]
    pub const fn tokens(&self) -> u32 {
        self.tokens
    }

    /// Events refused since creation (or the last [`take_dropped`]),
    /// saturating at `u32::MAX`.
    ///
    /// [`take_dropped`]: Self::take_dropped
    #[must_use]
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Returns the drop count and resets it — for periodic logging.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::take(&mut self.dropped)
    }

    fn refill(&mut self, now: u64) {
        let Some(since) = self.credited_until else {
            self.credited_until = Some(now);
            return;
        };
        if now < since {
            // The tick went backwards (the source restarted or was reset):
            // nothing is earned, and time is counted from `now` from here
            // on. Keeping the old mark instead would refuse every event
            // until the new tick caught up with it.
            self.credited_until = Some(now);
            return;
        }
        let earned = now
            .saturating_sub(since)
            .checked_div(self.ticks_per_token)
            .unwrap_or(0);
        if earned == 0 {
            return;
        }
        let room = u64::from(self.burst.saturating_sub(self.tokens));
        if earned >= room {
            // Full: no credit carries over beyond a full bucket.
            self.tokens = self.burst;
            self.credited_until = Some(now);
        } else {
            // earned < room <= u32::MAX, so the conversion cannot fail.
            self.tokens = self
                .tokens
                .saturating_add(u32::try_from(earned).unwrap_or(u32::MAX));
            // Keep the fractional remainder for next time.
            self.credited_until =
                Some(since.saturating_add(earned.saturating_mul(self.ticks_per_token)));
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn rejects_degenerate_configuration() {
        assert_eq!(RateLimiter::new(0, 10), None);
        assert_eq!(RateLimiter::new(1, 0), None);
    }

    #[test]
    fn burst_then_steady_rate() {
        let mut r = RateLimiter::new(2, 100).unwrap();
        assert!(r.allow(1000));
        assert!(r.allow(1000));
        assert!(!r.allow(1050));
        assert!(r.allow(1100));
        assert!(!r.allow(1150));
        assert!(r.allow(1200));
        assert_eq!(r.dropped(), 2);
        assert_eq!(r.take_dropped(), 2);
        assert_eq!(r.dropped(), 0);
    }

    #[test]
    fn remainder_carries_over() {
        let mut r = RateLimiter::new(5, 10).unwrap();
        for _ in 0..5 {
            assert!(r.allow(0));
        }
        // 15 ticks earn one token with 5 ticks of credit left over...
        assert!(r.allow(15));
        assert!(!r.allow(15));
        // ...so the next token arrives at tick 20, not 25.
        assert!(r.allow(20));
    }

    #[test]
    fn backwards_time_earns_nothing_and_restarts_the_count() {
        let mut r = RateLimiter::new(1, 10).unwrap();
        assert!(r.allow(1_000));
        // The tick went backwards: no time has passed...
        assert!(!r.allow(5));
        // ...and time is counted again from the new tick.
        assert!(!r.allow(14));
        assert!(r.allow(15));
        assert!(!r.allow(15));
    }

    #[test]
    fn a_restarted_tick_source_does_not_lock_input_out() {
        // Regression (review 2026-10-01): after a restart of the tick source,
        // input was refused until the tick passed its old high-water mark —
        // about a day on a millisecond tick that had run for a day.
        let mut r = RateLimiter::new(4, 10).unwrap();
        for _ in 0..4 {
            assert!(r.allow(1_000_000));
        }
        assert!(!r.allow(1_000_000));
        // The source restarts at 0 and then runs normally: one event per
        // 1,000 ticks is well under the limit, so every one passes.
        let accepted = (0..999u64).filter(|i| r.allow(i * 1_000)).count();
        assert_eq!(accepted, 998, "all but the event at the restart itself");
    }

    #[test]
    fn jitter_smaller_than_a_token_earns_nothing() {
        // Credit is earned only by forward progress since the latest
        // backwards step, so a tick that jitters back and forth by less than
        // `ticks_per_token` never earns a token.
        let mut r = RateLimiter::new(1, 10).unwrap();
        assert!(r.allow(100));
        let mut accepted = 0;
        for _ in 0..10 {
            accepted += usize::from(r.allow(95)); // back: counting restarts at 95
            accepted += usize::from(r.allow(104)); // forward 9: no token yet
        }
        assert_eq!(accepted, 0);
        assert!(r.allow(105)); // forward 10 from 95: one token
    }

    #[test]
    fn huge_jumps_refill_without_overflow() {
        let mut r = RateLimiter::new(u32::MAX, 1).unwrap();
        assert!(r.allow(0));
        assert!(r.allow(u64::MAX));
        assert_eq!(r.tokens(), u32::MAX - 1);
        let mut s = RateLimiter::new(3, u64::MAX).unwrap();
        for _ in 0..3 {
            assert!(s.allow(0));
        }
        assert!(!s.allow(u64::MAX - 1));
        assert!(s.allow(u64::MAX));
    }

    #[test]
    fn try_spend_takes_a_whole_group_or_nothing() {
        let mut limiter = RateLimiter::new(5, 10).unwrap();
        assert!(limiter.try_spend(0, 4));
        assert!(!limiter.try_spend(0, 2), "one token left");
        assert_eq!(limiter.tokens(), 1);
        assert_eq!(limiter.dropped(), 0, "a refusal is not a drop");
        assert!(limiter.try_spend(10, 2), "one token earned");
        assert!(
            !limiter.try_spend(10, 6),
            "more than the burst never passes"
        );
    }

    proptest::proptest! {
        /// A sender metering whole presses at 60 events at once and
        /// 240 a second never sends an event a receiver allowing 64 at once
        /// and 250 a second refuses - at the Pi 5's counter frequency, for
        /// any times and presses of 2 or 4 events, the receiver seeing each
        /// event at the sender's tick or up to 2.1 ms later (113,400 ticks:
        /// the UART input PD's longest wait in a poll, and more).
        #[test]
        fn a_meter_below_the_receivers_rate_is_never_refused(
            steps in proptest::collection::vec((0u64..400_000, proptest::bool::ANY, 0u64..=113_400), 1..400)
        ) {
            const FREQUENCY: u64 = 54_000_000;
            let mut meter = RateLimiter::new(60, FREQUENCY / 240).unwrap();
            let mut receiver = RateLimiter::new(64, FREQUENCY / 250).unwrap();
            let (mut now, mut seen) = (0u64, 0u64);
            for (gap, shifted, lag) in steps {
                now = now.saturating_add(gap);
                let events = if shifted { 4 } else { 2 };
                if meter.try_spend(now, events) {
                    // The receiver's own clock: the same counter, read later.
                    seen = seen.max(now.saturating_add(lag));
                    for _ in 0..events {
                        proptest::prop_assert!(receiver.allow(seen));
                    }
                }
            }
        }
    }
}
