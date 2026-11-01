// SPDX-License-Identifier: MIT OR Apache-2.0
//! Monotonic time: [`Instant`], moved here from `nowayos-timer-api`, which
//! re-exports it.

/// A point in monotonic time: nanoseconds since the timer started counting,
/// at boot.
///
/// Every reading the timer PD gives is at least the one before it, for every
/// client: monotonic time never goes back, and is never set. It knows
/// nothing of the date - only the clock PD relates it to wall-clock time
/// ([`Utc`](crate::Utc)). A `u64` of nanoseconds lasts 584 years.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    /// The instant `nanos` nanoseconds after the timer started.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Nanoseconds since the timer started.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// The instant a counter reading of `ticks`, at `frequency` ticks per
    /// second, stands for: rounded down to a whole nanosecond, so a counter
    /// that never goes back gives instants that never go back. 0 if the
    /// frequency is 0; the largest instant if the result does not fit.
    #[must_use]
    pub fn from_ticks(ticks: u64, frequency: u64) -> Self {
        let nanos = u128::from(ticks)
            .checked_mul(1_000_000_000)
            .and_then(|scaled| scaled.checked_div(u128::from(frequency)))
            .map_or(0, |nanos| u64::try_from(nanos).unwrap_or(u64::MAX));
        Self(nanos)
    }

    /// Nanoseconds from `earlier` to `self`, or 0 if `earlier` is later.
    #[must_use]
    pub const fn saturating_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Nanoseconds from `earlier` to `self`, or `None` if `earlier` is
    /// later.
    #[must_use]
    pub const fn checked_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }

    /// The instant `nanos` nanoseconds after this one, or `None` if it does
    /// not fit.
    #[must_use]
    pub const fn checked_add_nanos(self, nanos: u64) -> Option<Self> {
        match self.0.checked_add(nanos) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test code: tests compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn ticks_become_nanoseconds_rounded_down() {
        assert_eq!(
            Instant::from_ticks(62_500_000, 62_500_000).as_nanos(),
            1_000_000_000
        );
        assert_eq!(Instant::from_ticks(1, 62_500_000).as_nanos(), 16);
        assert_eq!(Instant::from_ticks(3, 3).as_nanos(), 1_000_000_000);
        assert_eq!(Instant::from_ticks(u64::MAX, 1).as_nanos(), u64::MAX);
        assert_eq!(
            Instant::from_ticks(54_000_000 * 3600, 54_000_000).as_nanos(),
            3_600_000_000_000
        );
        assert_eq!(Instant::from_ticks(5, 0).as_nanos(), 0);
        let mut previous = Instant::from_ticks(0, 54_000_000);
        for ticks in (0..10_000).map(|n| n * 7) {
            let now = Instant::from_ticks(ticks, 54_000_000);
            assert!(now >= previous);
            previous = now;
        }
    }

    #[test]
    fn differences_never_wrap() {
        let later = Instant::from_nanos(1_500);
        let earlier = Instant::from_nanos(500);
        assert_eq!(later.saturating_since(earlier), 1_000);
        assert_eq!(earlier.saturating_since(later), 0);
        assert_eq!(later.checked_since(earlier), Some(1_000));
        assert_eq!(earlier.checked_since(later), None);
        assert_eq!(earlier.checked_add_nanos(1_000), Some(later));
        assert_eq!(Instant::from_nanos(u64::MAX).checked_add_nanos(1), None);
    }
}
