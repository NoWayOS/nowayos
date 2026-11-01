// SPDX-License-Identifier: MIT OR Apache-2.0
//! An interval measured at an Instant, and how it moves on as monotonic
//! time passes: [`Anchor`], at the drift bound, [`DRIFT_PPM`].

use crate::{Instant, Source, TimeError, Utc, UtcInterval};

/// The drift bound: the most the counter that gives monotonic time may run
/// fast or slow against true time, in parts per million.
/// A stated figure: 100 ppm, until a board's datasheet gives a tighter one.
pub const DRIFT_PPM: u64 = 100;

/// Nanoseconds of measured elapsed time per nanosecond of drift at
/// [`DRIFT_PPM`]: (1,000,000 - 100) / 100 ([`drift`]'s note).
pub(crate) const ELAPSED_PER_DRIFT: u64 = 9_999;

/// The drift the bound allows over `elapsed` nanoseconds of monotonic time,
/// as measured, in nanoseconds, rounded up: `ceil(elapsed / 9,999)`. Never
/// more than `elapsed`.
///
/// The bound is against true time, but only the measured time is known: a
/// counter 100 ppm slow measures `m` for a true `m * 1,000,000 / 999,900`,
/// which is `m` plus `m / 9,999`, and one 100 ppm fast measures `m` for `m`
/// less `m / 10,001`. Dividing by 9,999 covers both; dividing by 10,000
/// would leave a slow counter's latest early by up to 1 ns in 10^8.
#[must_use]
pub const fn drift(elapsed: u64) -> u64 {
    elapsed.div_ceil(ELAPSED_PER_DRIFT)
}

/// An interval of wall-clock time as measured at an [`Instant`]: the floor
/// at the clock's start, or a sample at the Instant its answer arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Anchor {
    at: Instant,
    interval: UtcInterval,
}

impl Anchor {
    /// `interval`, as measured at `at`.
    #[must_use]
    pub const fn new(at: Instant, interval: UtcInterval) -> Self {
        Self { at, interval }
    }

    /// The floor: at `at`, the Instant the clock started,
    /// UTC is no earlier than `build_time`, since the device runs an image
    /// only after it is built; its latest is not known.
    #[must_use]
    pub const fn floor(build_time: Utc, at: Instant) -> Self {
        Self {
            at,
            interval: UtcInterval::from_earliest(build_time, Source::BuildTime),
        }
    }

    /// The Instant it was measured at.
    #[must_use]
    pub const fn at(&self) -> Instant {
        self.at
    }

    /// The interval as measured.
    #[must_use]
    pub const fn interval(&self) -> UtcInterval {
        self.interval
    }

    /// The interval at `now`: `earliest` moved on by the elapsed time less
    /// its [`drift`], and `latest` by the elapsed time plus it - or not
    /// known, if it was not known or would pass [`Utc::MAX`]. `earliest`
    /// stops at [`Utc::MAX`], still no later than the true time. A later
    /// `now` never gives an earlier `earliest` or a narrower interval.
    ///
    /// # Errors
    ///
    /// [`TimeError::WentBack`] if `now` is before the anchor's Instant: an
    /// interval is never carried back in time.
    pub fn interval_at(&self, now: Instant) -> Result<UtcInterval, TimeError> {
        self.interval_at_by(now, drift)
    }

    /// [`interval_at`](Self::interval_at), with the drift over the elapsed
    /// time given by `drift_of`. NoWayOS's code always passes [`drift`];
    /// Kani's proofs pass any function that meets its specification
    /// (`proofs.rs`), so that the solver never has to reason about a
    /// division, and prove [`drift`] meets it on its own.
    pub(crate) fn interval_at_by(
        &self,
        now: Instant,
        drift_of: impl Fn(u64) -> u64,
    ) -> Result<UtcInterval, TimeError> {
        let elapsed = now
            .checked_since(self.at)
            .ok_or(TimeError::WentBack { now, seen: self.at })?;
        let drift = drift_of(elapsed);
        // The drift is never more than the elapsed time, so this never
        // saturates.
        let earliest = self
            .interval
            .earliest()
            .saturating_add_nanos(elapsed.saturating_sub(drift));
        let latest = self.interval.latest().and_then(|latest| {
            latest
                .checked_add_nanos(elapsed)
                .and_then(|moved| moved.checked_add_nanos(drift))
        });
        // The anchor's earliest is no later than its latest, and its latest
        // moves on further than its earliest, so the new interval keeps
        // that; were it ever not to, the latest would not be claimed -
        // failing closed.
        let source = self.interval.source();
        Ok(UtcInterval::new(earliest, latest, source)
            .unwrap_or(UtcInterval::from_earliest(earliest, source)))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]
mod tests {
    use super::*;
    use crate::NANOS_PER_SECOND;

    #[test]
    fn the_drift_is_100_ppm_of_true_time_rounded_up() {
        assert_eq!(ELAPSED_PER_DRIFT, (1_000_000 - DRIFT_PPM) / DRIFT_PPM);
        assert_eq!(drift(0), 0);
        assert_eq!(drift(1), 1);
        assert_eq!(drift(9_999), 1);
        assert_eq!(drift(10_000), 2);
        assert_eq!(drift(NANOS_PER_SECOND), 100_011);
        assert_eq!(drift(u64::MAX), u64::MAX / 9_999 + 1);
    }

    #[test]
    fn the_drift_covers_a_counter_100_ppm_slow_or_fast() {
        // A counter 100 ppm slow measures `measured` for a true time of
        // measured * 1,000,000 / 999,900; one 100 ppm fast, for
        // measured * 1,000,000 / 1,000,100. Either way the true time is
        // within the drift of the measured time.
        for measured in [
            1,
            9_999,
            10_000,
            999_900,
            NANOS_PER_SECOND,
            86_400 * NANOS_PER_SECOND,
            u64::MAX / 2,
            u64::MAX,
        ] {
            let d = u128::from(drift(measured));
            let m = u128::from(measured);
            let slow = (m * 1_000_000).div_ceil(999_900);
            let fast = m * 1_000_000 / 1_000_100;
            assert!(slow <= m + d, "{measured}: slow {slow}, drift {d}");
            assert!(fast + d >= m, "{measured}: fast {fast}, drift {d}");
        }
        // Dividing by 10,000 would not: a day carried on a counter 100 ppm
        // slow leaves the latest early.
        let day = u128::from(86_400 * NANOS_PER_SECOND);
        assert!((day * 1_000_000).div_ceil(999_900) > day + day.div_ceil(10_000));
    }

    #[test]
    fn an_anchor_moves_on_and_widens_at_the_drift_bound() {
        let at = Instant::from_nanos(5 * NANOS_PER_SECOND);
        let earliest = Utc::from_unix_seconds(1_800_000_000).unwrap();
        let latest = earliest.checked_add_nanos(NANOS_PER_SECOND).unwrap();
        let anchor = Anchor::new(
            at,
            UtcInterval::new(earliest, Some(latest), Source::Backend).unwrap(),
        );
        assert_eq!(anchor.interval_at(at).unwrap(), anchor.interval());
        // 300 ms on: 30.004 µs of drift each way.
        assert_eq!(drift(300_000_000), 30_004);
        let now = at.checked_add_nanos(300_000_000).unwrap();
        let moved = anchor.interval_at(now).unwrap();
        assert_eq!(
            moved.earliest(),
            earliest.checked_add_nanos(300_000_000 - 30_004).unwrap()
        );
        assert_eq!(
            moved.latest(),
            latest.checked_add_nanos(300_000_000 + 30_004)
        );
        assert_eq!(moved.source(), Source::Backend);
        assert_eq!(
            anchor.interval_at(Instant::from_nanos(1)),
            Err(TimeError::WentBack {
                now: Instant::from_nanos(1),
                seen: at
            })
        );
    }

    #[test]
    fn the_floor_never_learns_a_latest() {
        let floor = Anchor::floor(
            Utc::from_unix_seconds(1_800_000_000).unwrap(),
            Instant::from_nanos(0),
        );
        let later = floor.interval_at(Instant::from_nanos(u64::MAX)).unwrap();
        assert_eq!(later.latest(), None);
        assert_eq!(later.source(), Source::BuildTime);
        assert!(later.earliest() > floor.interval().earliest());
    }

    #[test]
    fn past_the_largest_utc_the_latest_is_not_known() {
        let near_the_end = Utc::MAX.checked_sub_nanos(10).unwrap();
        let anchor = Anchor::new(
            Instant::from_nanos(0),
            UtcInterval::new(near_the_end, Some(near_the_end), Source::Backend).unwrap(),
        );
        let moved = anchor.interval_at(Instant::from_nanos(1_000)).unwrap();
        assert_eq!(moved.earliest(), Utc::MAX);
        assert_eq!(moved.latest(), None);
        assert_eq!(moved.source(), Source::Backend);
    }
}
