// SPDX-License-Identifier: MIT OR Apache-2.0
//! Wall-clock time: [`Utc`], and what a device knows of it, a
//! [`UtcInterval`] with its [`Source`].

use core::fmt;

use crate::ValidityError;

/// Nanoseconds in a second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// A point in wall-clock time: UTC as nanoseconds since
/// 1970-01-01T00:00:00Z, leap seconds not counted - Unix time, as a
/// certificate's dates, HTTP's `Date` and a build's `SOURCE_DATE_EPOCH` count
/// it, at a finer grain.
///
/// It holds a time; it does not say the device knows it. What a device knows
/// is a [`UtcInterval`]. The latest time it can hold, [`Utc::MAX`], is in
/// 2554.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utc(u64);

impl Utc {
    /// The latest time a `Utc` can hold: 2554-07-21T23:34:33.709551615Z.
    pub const MAX: Self = Self(u64::MAX);

    /// The time `nanos` nanoseconds after 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn from_unix_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// The time `seconds` whole seconds after 1970-01-01T00:00:00Z, or
    /// `None` if it is after [`Utc::MAX`].
    #[must_use]
    pub const fn from_unix_seconds(seconds: u64) -> Option<Self> {
        match seconds.checked_mul(NANOS_PER_SECOND) {
            Some(nanos) => Some(Self(nanos)),
            None => None,
        }
    }

    /// Nanoseconds since 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn as_unix_nanos(self) -> u64 {
        self.0
    }

    /// Whole seconds since 1970-01-01T00:00:00Z, rounded down.
    #[must_use]
    pub const fn as_unix_seconds(self) -> u64 {
        // NANOS_PER_SECOND is not zero.
        match self.0.checked_div(NANOS_PER_SECOND) {
            Some(seconds) => seconds,
            None => 0,
        }
    }

    /// The time `nanos` nanoseconds later, or `None` if it is after
    /// [`Utc::MAX`].
    #[must_use]
    pub const fn checked_add_nanos(self, nanos: u64) -> Option<Self> {
        match self.0.checked_add(nanos) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    /// The time `nanos` nanoseconds later, or [`Utc::MAX`] if that is later
    /// still.
    #[must_use]
    pub const fn saturating_add_nanos(self, nanos: u64) -> Self {
        Self(self.0.saturating_add(nanos))
    }

    /// The time `nanos` nanoseconds earlier, or `None` if it is before
    /// 1970.
    #[must_use]
    pub const fn checked_sub_nanos(self, nanos: u64) -> Option<Self> {
        match self.0.checked_sub(nanos) {
            Some(difference) => Some(Self(difference)),
            None => None,
        }
    }

    /// Nanoseconds from `earlier` to `self`, or 0 if `earlier` is later.
    #[must_use]
    pub const fn saturating_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// Where the wall-clock time in a [`UtcInterval`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Source {
    /// The floor alone: the image's build time, moved on by the monotonic
    /// time since the clock started. Its interval's `latest` is never known.
    BuildTime,
    /// The pinned backend's `Date`, over its TLS connection - in a test system, from a test PD in the TLS PD's place.
    Backend,
}

impl Source {
    /// The source's number in a record.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::BuildTime => 1,
            Self::Backend => 2,
        }
    }

    /// The source a record's number names, or `None`.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::BuildTime),
            2 => Some(Self::Backend),
            _ => None,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BuildTime => "the build time",
            Self::Backend => "the backend",
        })
    }
}

/// What a device knows of wall-clock time: UTC is no earlier than
/// [`earliest`](Self::earliest) and no later than
/// [`latest`](Self::latest), when that is known, as [`source`](Self::source)
/// gave it.
///
/// Always `earliest <= latest`, and while the source is
/// [`Source::BuildTime`] the latest is not known: the constructors refuse
/// anything else, so every interval keeps both rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UtcInterval {
    earliest: Utc,
    latest: Option<Utc>,
    source: Source,
}

impl UtcInterval {
    /// The interval from `earliest` to `latest` (`None`: not known), from
    /// `source`; `None` if `latest` is before `earliest`, or if the source
    /// is the build time and `latest` is given.
    #[must_use]
    pub const fn new(earliest: Utc, latest: Option<Utc>, source: Source) -> Option<Self> {
        match (latest, source) {
            (Some(_), Source::BuildTime) => None,
            (Some(latest), _) if latest.0 < earliest.0 => None,
            _ => Some(Self {
                earliest,
                latest,
                source,
            }),
        }
    }

    /// The interval from `earliest` to a latest that is not known.
    #[must_use]
    pub const fn from_earliest(earliest: Utc, source: Source) -> Self {
        Self {
            earliest,
            latest: None,
            source,
        }
    }

    /// UTC is no earlier than this.
    #[must_use]
    pub const fn earliest(&self) -> Utc {
        self.earliest
    }

    /// UTC is no later than this; `None` while it is not known.
    #[must_use]
    pub const fn latest(&self) -> Option<Utc> {
        self.latest
    }

    /// Where the time came from.
    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    /// Whether the time is known: whether `latest` is.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.latest.is_some()
    }

    /// Nanoseconds from `earliest` to `latest`, or `None` while `latest` is
    /// not known.
    #[must_use]
    pub const fn width(&self) -> Option<u64> {
        match self.latest {
            Some(latest) => Some(latest.saturating_since(self.earliest)),
            None => None,
        }
    }

    /// Checks a certificate's validity period against the interval: it
    /// passes only if the certificate is valid for the whole of it,
    /// `not_before <= earliest` and `latest <= not_after`.
    ///
    /// # Errors
    ///
    /// [`ValidityError::TimeNotKnown`] whenever `latest` is not known,
    /// whatever the dates; then [`ValidityError::NotYetValid`] if
    /// `not_before` is after `earliest`, and [`ValidityError::Expired`] if
    /// `not_after` is before `latest`.
    pub fn check_validity(&self, not_before: Utc, not_after: Utc) -> Result<(), ValidityError> {
        let latest = self.latest.ok_or(ValidityError::TimeNotKnown)?;
        if not_before > self.earliest {
            return Err(ValidityError::NotYetValid);
        }
        if latest > not_after {
            return Err(ValidityError::Expired);
        }
        Ok(())
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

    const DAY: u64 = 86_400 * NANOS_PER_SECOND;

    fn utc(seconds: u64) -> Utc {
        Utc::from_unix_seconds(seconds).unwrap()
    }

    #[test]
    fn seconds_and_nanoseconds() {
        assert_eq!(utc(1).as_unix_nanos(), NANOS_PER_SECOND);
        assert_eq!(Utc::from_unix_nanos(2_999_999_999).as_unix_seconds(), 2);
        assert_eq!(
            Utc::from_unix_seconds(18_446_744_073)
                .unwrap()
                .as_unix_seconds(),
            18_446_744_073
        );
        assert_eq!(Utc::from_unix_seconds(18_446_744_074), None);
        assert_eq!(Utc::MAX.checked_add_nanos(1), None);
        assert_eq!(Utc::MAX.saturating_add_nanos(1), Utc::MAX);
        assert_eq!(utc(0).checked_sub_nanos(1), None);
        assert_eq!(utc(2).saturating_since(utc(1)), NANOS_PER_SECOND);
        assert_eq!(utc(1).saturating_since(utc(2)), 0);
    }

    #[test]
    fn every_interval_keeps_its_rules() {
        let (early, late) = (utc(100), utc(200));
        assert!(UtcInterval::new(early, Some(late), Source::Backend).is_some());
        assert!(UtcInterval::new(early, Some(early), Source::Backend).is_some());
        assert_eq!(UtcInterval::new(late, Some(early), Source::Backend), None);
        assert_eq!(UtcInterval::new(early, Some(late), Source::BuildTime), None);
        let floor = UtcInterval::new(early, None, Source::BuildTime).unwrap();
        assert_eq!(floor, UtcInterval::from_earliest(early, Source::BuildTime));
        assert!(!floor.is_known());
        assert_eq!(floor.width(), None);
        let known = UtcInterval::new(early, Some(late), Source::Backend).unwrap();
        assert!(known.is_known());
        assert_eq!(known.width(), Some(100 * NANOS_PER_SECOND));
        assert_eq!(
            (known.earliest(), known.latest(), known.source()),
            (early, Some(late), Source::Backend)
        );
    }

    #[test]
    fn sources_have_one_number_each() {
        for source in [Source::BuildTime, Source::Backend] {
            assert_eq!(Source::from_tag(source.tag()), Some(source));
        }
        for tag in [0, 3, 255] {
            assert_eq!(Source::from_tag(tag), None);
        }
        assert_eq!(Source::BuildTime.to_string(), "the build time");
        assert_eq!(Source::Backend.to_string(), "the backend");
    }

    #[test]
    fn a_certificate_passes_only_if_valid_for_the_whole_interval() {
        let now = utc(1_800_000_000);
        let known = UtcInterval::new(
            now,
            now.checked_add_nanos(2 * NANOS_PER_SECOND),
            Source::Backend,
        )
        .unwrap();
        let latest = known.latest().unwrap();
        let day_before = now.checked_sub_nanos(DAY).unwrap();
        let year_after = now.checked_add_nanos(365 * DAY).unwrap();
        assert_eq!(known.check_validity(day_before, year_after), Ok(()));
        // The edges themselves pass: notBefore <= earliest, latest <= notAfter.
        assert_eq!(known.check_validity(now, latest), Ok(()));
        let one = |time: Utc, nanos: i64| {
            if nanos < 0 {
                time.checked_sub_nanos(nanos.unsigned_abs()).unwrap()
            } else {
                time.checked_add_nanos(nanos.unsigned_abs()).unwrap()
            }
        };
        assert_eq!(
            known.check_validity(one(now, 1), year_after),
            Err(ValidityError::NotYetValid)
        );
        assert_eq!(
            known.check_validity(day_before, one(latest, -1)),
            Err(ValidityError::Expired)
        );
        // Not known: refused whatever the dates, even a certificate valid
        // for all time.
        let floor = UtcInterval::from_earliest(now, Source::BuildTime);
        assert_eq!(
            floor.check_validity(Utc::from_unix_nanos(0), Utc::MAX),
            Err(ValidityError::TimeNotKnown)
        );
        assert_eq!(
            floor.check_validity(year_after, day_before),
            Err(ValidityError::TimeNotKnown)
        );
        let unknown = UtcInterval::from_earliest(now, Source::Backend);
        assert_eq!(
            unknown.check_validity(day_before, year_after),
            Err(ValidityError::TimeNotKnown)
        );
    }
}
