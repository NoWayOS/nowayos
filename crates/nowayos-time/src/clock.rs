// SPDX-License-Identifier: MIT OR Apache-2.0
//! The clock PD's bookkeeping: the floor, the newest anchor, and the
//! Instant it was last given ([`Clock`]). Plain data, no seL4, so it is
//! tested on the host; the clock PD feeds it the timer's Instants and its
//! sources' samples.

use crate::{Anchor, Instant, Sample, Source, TimeError, Utc, UtcInterval, drift};

/// What [`Clock::take`] did with a valid sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// It was measured later than the anchor held, or there was none: it is
    /// the anchor now.
    Newer,
    /// It was measured no later than the anchor held, which stays: an older
    /// measurement never replaces a newer one.
    Older,
}

/// Wall-clock time as one clock PD holds it: the
/// floor, from the image's build time at the Instant the clock started; the
/// anchor of the newest valid sample, once there is one; and the latest
/// Instant it was given, so monotonic time cannot be made to go back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    floor: Anchor,
    newest: Option<Anchor>,
    seen: Instant,
}

impl Clock {
    /// A clock that knows only the floor: UTC is no earlier than
    /// `build_time` at `start`, the Instant it started.
    #[must_use]
    pub const fn new(build_time: Utc, start: Instant) -> Self {
        Self {
            floor: Anchor::floor(build_time, start),
            newest: None,
            seen: start,
        }
    }

    /// The image's build time: the floor.
    #[must_use]
    pub const fn build_time(&self) -> Utc {
        self.floor.interval().earliest()
    }

    /// The floor, as an anchor at the Instant the clock started.
    #[must_use]
    pub const fn floor(&self) -> &Anchor {
        &self.floor
    }

    /// The newest valid sample's anchor, if there has been one.
    #[must_use]
    pub const fn anchor(&self) -> Option<&Anchor> {
        self.newest.as_ref()
    }

    /// What the clock knows at `now`: the newest anchor moved on to `now`,
    /// never earlier than the floor moved on to `now`, with the anchor's
    /// source - or, before any sample, the floor alone, whose latest is not
    /// known. A later `now` never gives an earlier earliest or a narrower
    /// interval.
    ///
    /// # Errors
    ///
    /// [`TimeError::WentBack`] if `now` is before an Instant the clock was
    /// already given; nothing changes.
    pub fn read(&mut self, now: Instant) -> Result<UtcInterval, TimeError> {
        self.read_by(now, drift)
    }

    /// [`read`](Self::read), with the anchors moved on by `drift_of`
    /// ([`Anchor::interval_at`]'s note).
    pub(crate) fn read_by(
        &mut self,
        now: Instant,
        drift_of: impl Fn(u64) -> u64 + Copy,
    ) -> Result<UtcInterval, TimeError> {
        self.note(now)?;
        let floor = self.floor.interval_at_by(now, drift_of)?;
        let Some(anchor) = self.newest else {
            return Ok(floor);
        };
        let moved = anchor.interval_at_by(now, drift_of)?;
        let earliest = moved.earliest().max(floor.earliest());
        // The sample's earliest was no earlier than the floor when it was
        // measured, so the floor never passes its latest; were it ever to,
        // the latest would not be claimed - failing closed.
        Ok(UtcInterval::new(earliest, moved.latest(), moved.source())
            .unwrap_or(UtcInterval::from_earliest(earliest, moved.source())))
    }

    /// Takes a sample from `source`, at `now`: checks it ([`Sample::check`])
    /// and, if it was measured later than the anchor held, makes it the
    /// anchor.
    ///
    /// # Errors
    ///
    /// [`TimeError::WentBack`] if `now` is before an Instant the clock was
    /// already given; [`TimeError::NotASampleSource`] if `source` is the
    /// build time; or the check the sample failed. A refused sample leaves
    /// the clock as it was.
    pub fn take(
        &mut self,
        sample: &Sample,
        source: Source,
        now: Instant,
    ) -> Result<Taken, TimeError> {
        self.take_by(sample, source, now, drift)
    }

    /// [`take`](Self::take), with the floor moved on by `drift_of`
    /// ([`Anchor::interval_at`]'s note).
    pub(crate) fn take_by(
        &mut self,
        sample: &Sample,
        source: Source,
        now: Instant,
        drift_of: impl Fn(u64) -> u64,
    ) -> Result<Taken, TimeError> {
        if now < self.seen {
            return Err(TimeError::WentBack {
                now,
                seen: self.seen,
            });
        }
        sample.check_by(&self.floor, now, drift_of)?;
        let interval = UtcInterval::new(sample.earliest, Some(sample.latest), source)
            .ok_or(TimeError::NotASampleSource)?;
        self.seen = now;
        if self
            .newest
            .is_some_and(|anchor| sample.received <= anchor.at())
        {
            return Ok(Taken::Older);
        }
        self.newest = Some(Anchor::new(sample.received, interval));
        Ok(Taken::Newer)
    }

    /// Records `now` as the latest Instant given, unless it is earlier than
    /// one already given.
    fn note(&mut self, now: Instant) -> Result<(), TimeError> {
        if now < self.seen {
            return Err(TimeError::WentBack {
                now,
                seen: self.seen,
            });
        }
        self.seen = now;
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
    use crate::{NANOS_PER_SECOND, ValidityError};

    const MS: u64 = 1_000_000;
    const DAY: u64 = 86_400 * NANOS_PER_SECOND;

    fn build_time() -> Utc {
        Utc::from_unix_seconds(1_800_000_000).unwrap()
    }

    fn instant(ms: u64) -> Instant {
        Instant::from_nanos(ms * MS)
    }

    fn date_sample(days_after_floor: u64, sent_ms: u64, received_ms: u64) -> Sample {
        let date = build_time()
            .checked_add_nanos(days_after_floor * DAY)
            .unwrap();
        Sample::from_date(date, instant(sent_ms), instant(received_ms)).unwrap()
    }

    #[test]
    fn before_any_sample_only_the_floor_is_known() {
        let mut clock = Clock::new(build_time(), instant(100));
        assert_eq!(clock.build_time(), build_time());
        let at_start = clock.read(instant(100)).unwrap();
        assert_eq!(
            at_start,
            UtcInterval::from_earliest(build_time(), Source::BuildTime)
        );
        // 300 ms on, less its drift: 30.004 us.
        let later = clock.read(instant(400)).unwrap();
        assert_eq!(
            later.earliest(),
            build_time().checked_add_nanos(300 * MS - 30_004).unwrap()
        );
        assert_eq!(later.latest(), None);
        assert_eq!(clock.anchor(), None);
        let not_before = build_time().checked_sub_nanos(DAY).unwrap();
        let not_after = build_time().checked_add_nanos(365 * DAY).unwrap();
        assert_eq!(
            later.check_validity(not_before, not_after),
            Err(ValidityError::TimeNotKnown)
        );
    }

    #[test]
    fn a_valid_sample_becomes_the_anchor_and_the_date_check_passes() {
        let mut clock = Clock::new(build_time(), instant(100));
        let sample = date_sample(1, 200, 240);
        assert_eq!(
            clock.take(&sample, Source::Backend, instant(250)),
            Ok(Taken::Newer)
        );
        let reading = clock.read(instant(250)).unwrap();
        assert_eq!(reading.source(), Source::Backend);
        let anchor = clock.anchor().unwrap();
        assert_eq!(anchor.at(), instant(240));
        assert_eq!(reading, anchor.interval_at(instant(250)).unwrap());
        let not_before = build_time().checked_sub_nanos(DAY).unwrap();
        let not_after = build_time().checked_add_nanos(365 * DAY).unwrap();
        assert_eq!(reading.check_validity(not_before, not_after), Ok(()));
    }

    #[test]
    fn a_refused_sample_leaves_the_time_as_it_was() {
        let mut clock = Clock::new(build_time(), instant(100));
        clock
            .take(&date_sample(1, 200, 240), Source::Backend, instant(250))
            .unwrap();
        let before = clock.clone();
        let refused = [
            date_sample(0, 300, 340),
            Sample {
                latest: date_sample(2, 300, 340).earliest,
                ..date_sample(2, 300, 340)
            },
            date_sample(2, 900, 1_000),
        ];
        let now = instant(500);
        assert!(matches!(
            clock.take(
                &Sample {
                    earliest: build_time().checked_sub_nanos(1).unwrap(),
                    ..refused[0]
                },
                Source::Backend,
                now
            ),
            Err(TimeError::BeforeFloor { .. })
        ));
        assert!(matches!(
            clock.take(&refused[1], Source::Backend, now),
            Err(TimeError::NarrowerThanRoundTrip { .. })
        ));
        assert!(matches!(
            clock.take(&refused[2], Source::Backend, now),
            Err(TimeError::FromTheFuture { .. })
        ));
        assert_eq!(clock, before);
        assert_eq!(
            clock.take(&date_sample(2, 300, 340), Source::BuildTime, now),
            Err(TimeError::NotASampleSource)
        );
        assert_eq!(clock, before);
    }

    #[test]
    fn only_a_later_measurement_replaces_the_anchor() {
        let mut clock = Clock::new(build_time(), instant(100));
        clock
            .take(&date_sample(1, 200, 240), Source::Backend, instant(250))
            .unwrap();
        let anchor = *clock.anchor().unwrap();
        // Measured at the same Instant, or earlier: kept out.
        assert_eq!(
            clock.take(&date_sample(2, 200, 240), Source::Backend, instant(260)),
            Ok(Taken::Older)
        );
        assert_eq!(
            clock.take(&date_sample(2, 150, 230), Source::Backend, instant(260)),
            Ok(Taken::Older)
        );
        assert_eq!(clock.anchor(), Some(&anchor));
        // Measured later: it replaces the anchor, even though it is earlier
        // in UTC than the anchor moved on - the newest measurement wins.
        let newer = date_sample(1, 250, 270);
        assert!(newer.earliest < anchor.interval_at(instant(270)).unwrap().earliest());
        assert_eq!(
            clock.take(&newer, Source::Backend, instant(270)),
            Ok(Taken::Newer)
        );
        assert_eq!(clock.anchor().unwrap().at(), instant(270));
    }

    #[test]
    fn monotonic_time_cannot_be_made_to_go_back() {
        let mut clock = Clock::new(build_time(), instant(100));
        clock.read(instant(500)).unwrap();
        assert_eq!(
            clock.read(instant(499)),
            Err(TimeError::WentBack {
                now: instant(499),
                seen: instant(500)
            })
        );
        assert_eq!(
            clock.take(&date_sample(1, 200, 240), Source::Backend, instant(400)),
            Err(TimeError::WentBack {
                now: instant(400),
                seen: instant(500)
            })
        );
        assert_eq!(
            Clock::new(build_time(), instant(100))
                .read(instant(99))
                .map(|_| ()),
            Err(TimeError::WentBack {
                now: instant(99),
                seen: instant(100)
            })
        );
    }

    #[test]
    fn the_reading_never_falls_below_the_floor() {
        // A sample right at the floor, read much later: the floor's own
        // projection is the bound wherever the two round differently.
        let mut clock = Clock::new(build_time(), instant(0));
        let at = instant(10_000);
        let floor_then = clock.floor().interval_at(at).unwrap().earliest();
        let sample = Sample {
            earliest: floor_then,
            latest: floor_then.checked_add_nanos(NANOS_PER_SECOND).unwrap(),
            sent: at,
            received: at,
        };
        clock.take(&sample, Source::Backend, at).unwrap();
        for ms in [10_000, 10_001, 12_345, 99_999, 1_000_000] {
            let reading = clock.read(instant(ms)).unwrap();
            let floor = clock.floor().interval_at(instant(ms)).unwrap().earliest();
            assert!(reading.earliest() >= floor);
            assert!(reading.latest().unwrap() >= reading.earliest());
        }
    }
}
