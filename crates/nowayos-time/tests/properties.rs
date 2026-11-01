// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests: an anchor carried forward never overflows, moves its
//! earliest back or narrows; the clock takes only samples that pass every
//! check and is unchanged by one it refuses; a date check passes exactly
//! for a certificate valid over the whole interval; and the records decode
//! any bytes to a typed error or a value whose earliest is not after its
//! latest - never a panic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_ipc::{Message, decode, decode_exact, encode};
use nowayos_time::record::{READING_LEN, SAMPLE_LEN};
use nowayos_time::{
    Anchor, Clock, Instant, NANOS_PER_SECOND, Reading, Sample, Source, Taken, TimeError, Utc,
    UtcInterval, ValidityError, drift,
};
use proptest::prelude::*;

/// Any interval that keeps the rules: the build time with no latest, or the
/// backend with any latest not before its earliest, or none.
fn interval() -> impl Strategy<Value = UtcInterval> {
    (any::<u64>(), any::<bool>(), any::<u64>(), any::<bool>()).prop_map(
        |(earliest, backend, width, known)| {
            let earliest = Utc::from_unix_nanos(earliest);
            if backend {
                let latest = known.then(|| earliest.saturating_add_nanos(width));
                UtcInterval::new(earliest, latest, Source::Backend).unwrap()
            } else {
                UtcInterval::from_earliest(earliest, Source::BuildTime)
            }
        },
    )
}

/// Times near a realistic floor, so that the interesting checks are reached
/// as often as the edges.
fn near(base: u64, spread: u64) -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => (0..spread).prop_map(move |offset| base.saturating_add(offset)),
        1 => (0..spread).prop_map(move |offset| base.saturating_sub(offset)),
        1 => any::<u64>(),
    ]
}

const FLOOR: u64 = 1_800_000_000 * NANOS_PER_SECOND;
const DAY: u64 = 86_400 * NANOS_PER_SECOND;

fn sample() -> impl Strategy<Value = Sample> {
    (
        near(FLOOR, 3 * DAY),
        0..5 * NANOS_PER_SECOND,
        near(0, 100 * NANOS_PER_SECOND),
        0..2 * NANOS_PER_SECOND,
    )
        .prop_map(|(earliest, width, sent, round_trip)| Sample {
            earliest: Utc::from_unix_nanos(earliest),
            latest: Utc::from_unix_nanos(earliest.saturating_add(width)),
            sent: Instant::from_nanos(sent),
            received: Instant::from_nanos(sent.saturating_add(round_trip)),
        })
}

proptest! {
    #[test]
    fn an_anchor_never_goes_back_or_narrows(
        interval in interval(),
        at: u64,
        first in any::<u64>(),
        step in any::<u64>(),
    ) {
        let anchor = Anchor::new(Instant::from_nanos(at), interval);
        let first = at.saturating_add(first % (u64::MAX - at).max(1));
        let second = first.saturating_add(step);
        let one = anchor.interval_at(Instant::from_nanos(first)).unwrap();
        let two = anchor.interval_at(Instant::from_nanos(second)).unwrap();
        let elapsed = second - at;
        prop_assert_eq!(
            two.earliest(),
            interval.earliest().saturating_add_nanos(elapsed - drift(elapsed))
        );
        prop_assert_eq!(
            two.latest(),
            interval.latest().and_then(|l| l.checked_add_nanos(elapsed)?.checked_add_nanos(drift(elapsed)))
        );
        prop_assert!(two.earliest() >= one.earliest());
        if let Some(width) = two.width() {
            prop_assert!(width >= one.width().unwrap());
            prop_assert!(two.latest().unwrap() >= two.earliest());
        }
        if one.latest().is_none() {
            prop_assert_eq!(two.latest(), None);
        }
        if interval.source() == Source::BuildTime {
            prop_assert_eq!(two.latest(), None);
        }
    }

    #[test]
    fn the_clock_takes_only_samples_that_pass_every_check(
        build in near(FLOOR, DAY),
        start in near(0, 10 * NANOS_PER_SECOND),
        samples in proptest::collection::vec((sample(), 0..3 * NANOS_PER_SECOND), 1..8),
    ) {
        let mut clock = Clock::new(Utc::from_unix_nanos(build), Instant::from_nanos(start));
        let mut now = start;
        for (sample, wait) in samples {
            now = now.saturating_add(wait);
            let before = clock.clone();
            let checked = sample.check(before.floor(), Instant::from_nanos(now));
            match clock.take(&sample, Source::Backend, Instant::from_nanos(now)) {
                Ok(taken) => {
                    prop_assert_eq!(checked, Ok(()));
                    let held = before.anchor().map(Anchor::at);
                    let newer = held.is_none_or(|at| sample.received > at);
                    prop_assert_eq!(taken, if newer { Taken::Newer } else { Taken::Older });
                    if newer {
                        prop_assert_eq!(clock.anchor().unwrap().at(), sample.received);
                    } else {
                        prop_assert_eq!(clock.anchor(), before.anchor());
                    }
                }
                Err(err) => {
                    prop_assert_eq!(Err(err), checked);
                    prop_assert_eq!(&clock, &before);
                }
            }
            let reading = clock.read(Instant::from_nanos(now)).unwrap();
            let floor = clock.floor().interval_at(Instant::from_nanos(now)).unwrap();
            prop_assert!(reading.earliest() >= floor.earliest());
            if let Some(latest) = reading.latest() {
                prop_assert!(latest >= reading.earliest());
            }
        }
    }

    #[test]
    fn readings_without_new_samples_never_go_back_or_narrow(
        build in near(FLOOR, DAY),
        sample in sample(),
        steps in proptest::collection::vec(0..u64::MAX / 64, 1..8),
    ) {
        let mut clock = Clock::new(Utc::from_unix_nanos(build), Instant::from_nanos(0));
        let now = sample.received;
        let _ = clock.take(&sample, Source::Backend, now);
        let mut last = clock.read(now).unwrap();
        let mut at = now.as_nanos();
        for step in steps {
            at = at.saturating_add(step);
            let reading = clock.read(Instant::from_nanos(at)).unwrap();
            prop_assert!(reading.earliest() >= last.earliest());
            match (last.width(), reading.width()) {
                (Some(before), Some(after)) => prop_assert!(after >= before),
                (None, after) => prop_assert_eq!(after, None),
                (Some(_), None) => {}
            }
            prop_assert_eq!(reading.source(), last.source());
            last = reading;
        }
        if at > 0 {
            let went_back = matches!(
                clock.read(Instant::from_nanos(at - 1)),
                Err(TimeError::WentBack { .. })
            );
            prop_assert!(went_back);
        }
    }

    #[test]
    fn a_date_gives_its_sample(date in near(FLOOR, DAY), sent: u64, round_trip in 0..10 * NANOS_PER_SECOND) {
        let date = Utc::from_unix_nanos(date);
        let received = sent.saturating_add(round_trip);
        let sample = Sample::from_date(date, Instant::from_nanos(sent), Instant::from_nanos(received));
        let round_trip = received - sent;
        // The round trip's drift at 100 ppm of true time, rounded up,
        // computed here apart from `drift`: a measured m is at most
        // m * 1,000,000 / 999,900 of true time.
        let latest = date
            .checked_add_nanos(NANOS_PER_SECOND)
            .and_then(|t| t.checked_add_nanos(round_trip))
            .and_then(|t| t.checked_add_nanos(round_trip.div_ceil(9_999)));
        match latest {
            Some(latest) => {
                let sample = sample.unwrap();
                prop_assert_eq!((sample.earliest, sample.latest), (date, latest));
                prop_assert_eq!(sample.round_trip(), Some(round_trip));
            }
            None => prop_assert_eq!(sample, Err(TimeError::OutOfRange)),
        }
    }

    #[test]
    fn a_date_check_passes_only_for_the_whole_interval(
        interval in interval(),
        not_before: u64,
        not_after: u64,
    ) {
        let (not_before, not_after) = (Utc::from_unix_nanos(not_before), Utc::from_unix_nanos(not_after));
        let result = interval.check_validity(not_before, not_after);
        match interval.latest() {
            None => prop_assert_eq!(result, Err(ValidityError::TimeNotKnown)),
            Some(latest) => prop_assert_eq!(
                result.is_ok(),
                not_before <= interval.earliest() && latest <= not_after
            ),
        }
    }

    #[test]
    fn sample_records_decode_safely(bytes in proptest::collection::vec(any::<u8>(), 0..=SAMPLE_LEN + 1)) {
        record_decodes_safely::<Sample>(&bytes, |sample| {
            sample.earliest <= sample.latest && sample.sent <= sample.received
        })?;
    }

    #[test]
    fn sample_payloads_decode_safely(payload in proptest::collection::vec(any::<u8>(), 32)) {
        record_decodes_safely::<Sample>(&framed::<Sample>(&payload), |sample| {
            sample.earliest <= sample.latest && sample.sent <= sample.received
        })?;
    }

    #[test]
    fn reading_records_decode_safely(bytes in proptest::collection::vec(any::<u8>(), 0..=READING_LEN + 1)) {
        record_decodes_safely::<Reading>(&bytes, reading_rules)?;
    }

    #[test]
    fn reading_payloads_decode_safely(payload in proptest::collection::vec(any::<u8>(), 32), source in 0u8..4, known in 0u8..3) {
        let mut payload = payload;
        payload[24] = source;
        payload[25] = known;
        payload[26..].fill(0);
        record_decodes_safely::<Reading>(&framed::<Reading>(&payload), reading_rules)?;
    }

    #[test]
    fn samples_and_readings_round_trip(sample in sample(), interval in interval(), at: u64) {
        if sample.earliest <= sample.latest && sample.sent <= sample.received {
            let mut buf = [0u8; 64];
            let len = encode(&sample, &mut buf).unwrap();
            prop_assert_eq!(decode_exact::<Sample>(&buf[..len]), Ok(sample));
        }
        let reading = Reading { interval, at: Instant::from_nanos(at) };
        let mut buf = [0u8; 64];
        let len = encode(&reading, &mut buf).unwrap();
        prop_assert_eq!(decode_exact::<Reading>(&buf[..len]), Ok(reading));
    }
}

fn reading_rules(reading: &Reading) -> bool {
    match reading.interval.latest() {
        Some(latest) => {
            reading.interval.earliest() <= latest && reading.interval.source() != Source::BuildTime
        }
        None => true,
    }
}

/// `payload` behind a valid header for `M`.
fn framed<M: Message>(payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0u8; 8];
    nowayos_ipc::Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: u16::try_from(M::PAYLOAD_LEN).unwrap(),
    }
    .write(&mut bytes)
    .unwrap();
    bytes.extend_from_slice(payload);
    bytes
}

/// Decoding `bytes` as `M` never panics; `decode` and `decode_exact` agree;
/// whatever decodes keeps `rules` and re-encodes to the bytes it came from.
fn record_decodes_safely<M: Message + PartialEq + std::fmt::Debug>(
    bytes: &[u8],
    rules: impl Fn(&M) -> bool,
) -> Result<(), TestCaseError> {
    let prefix = decode::<M>(bytes);
    let exact = decode_exact::<M>(bytes);
    if let Ok(message) = &exact {
        prop_assert_eq!(prefix.as_ref().ok(), Some(message));
    }
    if let Ok(message) = prefix {
        prop_assert!(rules(&message));
        let mut out = [0u8; 64];
        let len = encode(&message, &mut out).unwrap();
        prop_assert_eq!(&out[..len], &bytes[..len]);
    }
    Ok(())
}
