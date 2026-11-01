// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a source tells the clock: a [`Sample`], and the checks it must
//! pass.

use crate::{Anchor, Instant, NANOS_PER_SECOND, TimeError, Utc, drift};

/// One measurement of wall-clock time from a source: UTC was no earlier than
/// `earliest` and no later than `latest` when the answer arrived, at
/// `received`, to a request sent at `sent`.
///
/// Plain data, as a source makes it or the record carries it: the clock
/// checks it before it believes it ([`Sample::check`]). On a source's ring
/// it is a record ([`record`](crate::record)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sample {
    /// UTC was no earlier than this.
    pub earliest: Utc,
    /// UTC was no later than this.
    pub latest: Utc,
    /// When the request that brought it was sent, in monotonic time.
    pub sent: Instant,
    /// When its answer arrived, in monotonic time: the Instant it was
    /// measured at.
    pub received: Instant,
}

impl Sample {
    /// The sample a `Date` gives: a response's `Date` is
    /// whole seconds of the server's clock, stamped somewhere between the
    /// request being sent and the answer arriving, so when it arrived UTC was
    /// between `date` and `date` plus one second plus the round trip and its
    /// [`drift`]: the round trip is monotonic time, measured by a counter
    /// that may run slow by up to the drift bound, as
    /// [`Anchor::interval_at`] widens by it too.
    ///
    /// # Errors
    ///
    /// [`TimeError::ReceivedBeforeSent`] if `received` is before `sent`;
    /// [`TimeError::OutOfRange`] if the latest is after [`Utc::MAX`].
    pub fn from_date(date: Utc, sent: Instant, received: Instant) -> Result<Self, TimeError> {
        let round_trip = received
            .checked_since(sent)
            .ok_or(TimeError::ReceivedBeforeSent { sent, received })?;
        let latest = date
            .checked_add_nanos(NANOS_PER_SECOND)
            .and_then(|second| second.checked_add_nanos(round_trip))
            .and_then(|trip| trip.checked_add_nanos(drift(round_trip)))
            .ok_or(TimeError::OutOfRange)?;
        Ok(Self {
            earliest: date,
            latest,
            sent,
            received,
        })
    }

    /// Nanoseconds from the request to its answer, or `None` if the answer
    /// is before the request.
    #[must_use]
    pub const fn round_trip(&self) -> Option<u64> {
        self.received.checked_since(self.sent)
    }

    /// Checks the sample on its own, against the floor and against `now`,
    /// in this order:
    ///
    /// 1. its earliest is not after its latest;
    /// 2. its answer did not arrive before its request was sent;
    /// 3. it was not measured at an Instant still to come: `received` is no
    ///    later than `now`;
    /// 4. it is no narrower than its round trip: a source cannot know the
    ///    time more closely than the time its answer took to come;
    /// 5. its earliest is not before the floor at the Instant it was
    ///    measured - the floor's earliest moved on to `received`, or, for a
    ///    sample measured before the clock started, the build time itself.
    ///
    /// # Errors
    ///
    /// The first check it fails: [`TimeError::EarliestAfterLatest`],
    /// [`TimeError::ReceivedBeforeSent`], [`TimeError::FromTheFuture`],
    /// [`TimeError::NarrowerThanRoundTrip`] or [`TimeError::BeforeFloor`].
    pub fn check(&self, floor: &Anchor, now: Instant) -> Result<(), TimeError> {
        self.check_by(floor, now, drift)
    }

    /// [`check`](Self::check), with the floor moved on by `drift_of`
    /// ([`Anchor::interval_at`]'s note).
    pub(crate) fn check_by(
        &self,
        floor: &Anchor,
        now: Instant,
        drift_of: impl Fn(u64) -> u64,
    ) -> Result<(), TimeError> {
        if self.earliest > self.latest {
            return Err(TimeError::EarliestAfterLatest {
                earliest: self.earliest,
                latest: self.latest,
            });
        }
        let round_trip = self.round_trip().ok_or(TimeError::ReceivedBeforeSent {
            sent: self.sent,
            received: self.received,
        })?;
        if self.received > now {
            return Err(TimeError::FromTheFuture {
                received: self.received,
                now,
            });
        }
        let width = self.latest.saturating_since(self.earliest);
        if width < round_trip {
            return Err(TimeError::NarrowerThanRoundTrip { width, round_trip });
        }
        // Measured before the clock started, the sample still came while
        // this image ran: after its build.
        let floor_then = floor
            .interval_at_by(self.received, drift_of)
            .map_or(floor.interval().earliest(), |interval| interval.earliest());
        if self.earliest < floor_then {
            return Err(TimeError::BeforeFloor {
                earliest: self.earliest,
                floor: floor_then,
            });
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

    const MS: u64 = 1_000_000;
    const DAY: u64 = 86_400 * NANOS_PER_SECOND;

    fn build_time() -> Utc {
        Utc::from_unix_seconds(1_800_000_000).unwrap()
    }

    fn floor() -> Anchor {
        Anchor::floor(build_time(), Instant::from_nanos(10 * NANOS_PER_SECOND))
    }

    fn day_after() -> Sample {
        let date = build_time().checked_add_nanos(DAY).unwrap();
        let sent = Instant::from_nanos(20 * NANOS_PER_SECOND);
        Sample::from_date(date, sent, sent.checked_add_nanos(40 * MS).unwrap()).unwrap()
    }

    #[test]
    fn a_date_gives_date_to_date_plus_one_second_plus_the_round_trip_and_its_drift() {
        let sample = day_after();
        let date = build_time().checked_add_nanos(DAY).unwrap();
        assert_eq!(sample.earliest, date);
        // 40 ms of round trip, at 100 ppm, is 4.001 us of drift.
        assert_eq!(drift(40 * MS), 4_001);
        assert_eq!(
            sample.latest,
            date.checked_add_nanos(NANOS_PER_SECOND + 40 * MS + 4_001)
                .unwrap()
        );
        assert_eq!(sample.round_trip(), Some(40 * MS));
        assert_eq!(
            sample.received,
            Instant::from_nanos(20 * NANOS_PER_SECOND + 40 * MS)
        );
        let (sent, received) = (Instant::from_nanos(2), Instant::from_nanos(1));
        assert_eq!(
            Sample::from_date(date, sent, received),
            Err(TimeError::ReceivedBeforeSent { sent, received })
        );
        let late = Utc::MAX.checked_sub_nanos(NANOS_PER_SECOND).unwrap();
        assert_eq!(
            Sample::from_date(late, received, sent),
            Err(TimeError::OutOfRange)
        );
    }

    #[test]
    fn a_valid_sample_passes() {
        let now = Instant::from_nanos(30 * NANOS_PER_SECOND);
        assert_eq!(day_after().check(&floor(), now), Ok(()));
        // Measured at `now` itself is not still to come.
        let sample = day_after();
        assert_eq!(sample.check(&floor(), sample.received), Ok(()));
    }

    #[test]
    fn each_bad_sample_is_refused_with_its_own_error() {
        let now = Instant::from_nanos(30 * NANOS_PER_SECOND);
        let good = day_after();

        let backwards = Sample {
            latest: good.earliest,
            earliest: good.latest,
            ..good
        };
        assert!(matches!(
            backwards.check(&floor(), now),
            Err(TimeError::EarliestAfterLatest { .. })
        ));

        let reversed = Sample {
            sent: good.received,
            received: good.sent,
            ..good
        };
        assert!(matches!(
            reversed.check(&floor(), now),
            Err(TimeError::ReceivedBeforeSent { .. })
        ));

        let future = Sample {
            sent: now,
            received: now.checked_add_nanos(1).unwrap(),
            latest: good
                .earliest
                .checked_add_nanos(2 * NANOS_PER_SECOND)
                .unwrap(),
            ..good
        };
        assert_eq!(
            future.check(&floor(), now),
            Err(TimeError::FromTheFuture {
                received: future.received,
                now
            })
        );

        let narrow = Sample {
            latest: good.earliest.checked_add_nanos(40 * MS - 1).unwrap(),
            ..good
        };
        assert_eq!(
            narrow.check(&floor(), now),
            Err(TimeError::NarrowerThanRoundTrip {
                width: 40 * MS - 1,
                round_trip: 40 * MS
            })
        );
        // Exactly as wide as its round trip is enough.
        let exact = Sample {
            latest: good.earliest.checked_add_nanos(40 * MS).unwrap(),
            ..good
        };
        assert_eq!(exact.check(&floor(), now), Ok(()));

        let early = Sample::from_date(
            build_time().checked_sub_nanos(DAY).unwrap(),
            good.sent,
            good.received,
        )
        .unwrap();
        assert!(matches!(
            early.check(&floor(), now),
            Err(TimeError::BeforeFloor { .. })
        ));
    }

    #[test]
    fn the_floor_is_checked_at_the_instant_the_sample_was_measured() {
        let floor = floor();
        let received = Instant::from_nanos(110 * NANOS_PER_SECOND);
        // 100 s after the clock started, the floor has moved on by 100 s
        // less 10.001001 ms of drift.
        assert_eq!(drift(100 * NANOS_PER_SECOND), 10_001_001);
        let floor_then = build_time()
            .checked_add_nanos(100 * NANOS_PER_SECOND - 10_001_001)
            .unwrap();
        let at = |earliest: Utc| Sample {
            earliest,
            latest: earliest.checked_add_nanos(NANOS_PER_SECOND).unwrap(),
            sent: received,
            received,
        };
        assert_eq!(at(floor_then).check(&floor, received), Ok(()));
        assert_eq!(
            at(floor_then.checked_sub_nanos(1).unwrap()).check(&floor, received),
            Err(TimeError::BeforeFloor {
                earliest: floor_then.checked_sub_nanos(1).unwrap(),
                floor: floor_then,
            })
        );
        // A sample measured before the clock started is held to the build
        // time itself.
        let before_start = Sample {
            sent: Instant::from_nanos(1),
            received: Instant::from_nanos(2),
            ..at(build_time())
        };
        assert_eq!(before_start.check(&floor, received), Ok(()));
        let before_build = Sample {
            earliest: build_time().checked_sub_nanos(1).unwrap(),
            ..before_start
        };
        assert!(matches!(
            before_build.check(&floor, received),
            Err(TimeError::BeforeFloor { .. })
        ));
    }
}
