// SPDX-License-Identifier: MIT OR Apache-2.0
//! The clock PD's interface: wall-clock time for other PDs.
//!
//! The clock PD holds wall-clock time as `nowayos-time` describes it: an
//! interval from the image's build time - the floor - to a latest that is
//! not known until a source answers, carried on in monotonic time at the
//! drift bound ([`Clock`]). It holds no key and parses no network data.
//!
//! **Reading the time.** A PD asks by **protected procedure call** on its
//! channel, as it asks the timer PD: `pp="true"` on the reader's end, and the
//! clock PD at the higher priority, as Microkit requires. A call carries a
//! message label and up to [`MAX_WORDS`] message registers (machine words),
//! and so does the reply:
//!
//! | Request | Label | Words | Reply when served (label 0) |
//! | --- | --- | --- | --- |
//! | [`Request::Read`] | [`READ`] | none | the [`Reading`] record as five words ([`Reading::to_words`]): the interval, its source and the Instant it was read at |
//!
//! Before any sample the reading's source is the build time and its latest
//! is not known, so every date check refuses ([`UtcInterval::check_validity`]).
//! A request the clock PD cannot serve gets a reply whose label is an error
//! code, with the details in its words; [`Message::read_reply`] decodes a
//! reply into a typed result, the reading validated field by field
//! ([`Reading::from_words`]).
//!
//! **Moving the time.** Only a source moves it - the TLS PD, with the
//! pinned backend's `Date`, or in a test system a test PD in its place - and
//! only through a ring into the clock PD that the system description grants
//! it: it writes a [`Sample`] record (`nowayos_time::record`) and notifies.
//! [`take_samples`] is the clock PD's side: each record copied out, decoded,
//! checked and kept as the anchor only if it is the newest. **No request sets
//! the time:** a sample offered by call ([`OFFER`]) is refused with
//! [`ClockError::NotASource`], whatever it carries.
//!
//! The protocol and the clock's service ([`serve`], [`take_samples`]) are
//! plain data and need no seL4, so they are tested on the host. The `client`
//! feature adds [`Client`], which makes the call from inside a PD.
//!
//! Provisional: when the TLS PD becomes the source.

#![cfg_attr(not(test), no_std)]

use core::fmt;

use nowayos_ipc::{Consumer, ConsumerMemory, DecodeError, Record, RingError};
use nowayos_time::record::{RECORD_WORDS, SAMPLE_RING_SLOT, SAMPLE_RING_SLOTS};
pub use nowayos_time::{Clock, Instant, Reading, Sample, Source, Taken, TimeError, UtcInterval};

#[cfg(feature = "client")]
mod client;

#[cfg(feature = "client")]
pub use client::Client;

/// The most message registers a request or reply uses: a reading's record.
pub const MAX_WORDS: usize = RECORD_WORDS;

/// The label of [`Request::Read`].
pub const READ: u64 = 1;

/// The label of a sample offered by call: always refused
/// ([`ClockError::NotASource`]), whatever its words. A sample comes only on a
/// source's ring.
pub const OFFER: u64 = 2;

/// Reply label: the request was served.
const OK: u64 = 0;
/// Error reply labels, one per [`ClockError`] the clock PD sends.
const UNKNOWN_REQUEST: u64 = 1;
const BAD_LENGTH: u64 = 2;
const NOT_A_SOURCE: u64 = 3;
const NO_TIME: u64 = 4;

/// What a PD can ask the clock PD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Request {
    /// The time now, as the clock knows it.
    Read,
}

/// Why a request was refused, or a reply could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockError {
    /// The clock PD has no request with this label.
    UnknownRequest {
        /// The label sent.
        label: u64,
    },
    /// The request had the wrong number of words.
    BadLength {
        /// The request's label.
        label: u64,
        /// The number of words sent.
        words: usize,
    },
    /// A sample was offered by call: samples come only on a source's ring,
    /// which the system description grants. The time is unchanged.
    NotASource,
    /// The clock PD could not read monotonic time from the timer PD, or it
    /// went back: it gives no reading rather than a wrong one.
    NoTime,
    /// A reply did not have the shape a reading's reply must have.
    MalformedReply {
        /// The reply's label.
        label: u64,
        /// The number of words in it.
        words: usize,
    },
    /// A reply's reading did not decode, or broke a rule of its record.
    BadReading(DecodeError),
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRequest { label } => write!(f, "unknown request {label}"),
            Self::BadLength { label, words } => {
                write!(f, "request {label} sent {words} word(s), the wrong number")
            }
            Self::NotASource => f.write_str("samples come only on a source's ring"),
            Self::NoTime => f.write_str("the clock could not read monotonic time"),
            Self::MalformedReply { label, words } => {
                write!(f, "malformed reply: label {label}, {words} word(s)")
            }
            Self::BadReading(err) => write!(f, "the reading does not decode: {err}"),
        }
    }
}

/// A request or reply as it crosses the channel: a label and up to
/// [`MAX_WORDS`] words. A message that arrived with more words than that
/// keeps only the count, so it is refused rather than cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    label: u64,
    words: [u64; MAX_WORDS],
    count: usize,
}

impl Message {
    /// A message with `label` and `words`; `None` if there are more than
    /// [`MAX_WORDS`] words.
    #[must_use]
    pub fn new(label: u64, words: &[u64]) -> Option<Self> {
        let mut message = Self {
            label,
            words: [0; MAX_WORDS],
            count: words.len(),
        };
        message.words.get_mut(..words.len())?.copy_from_slice(words);
        Some(message)
    }

    /// A message with at most [`MAX_WORDS`] words, which this crate builds
    /// itself (if there were more, it would have none).
    fn fixed(label: u64, words: &[u64]) -> Self {
        Self::new(label, words).unwrap_or(Self {
            label,
            words: [0; MAX_WORDS],
            count: 0,
        })
    }

    /// A message as received: `label`, `count` words, and the first
    /// [`MAX_WORDS`] of them read by `word(index)`. A count above
    /// `MAX_WORDS` is kept, so the message is refused, not truncated.
    #[must_use]
    pub fn received(label: u64, count: usize, mut word: impl FnMut(usize) -> u64) -> Self {
        let mut words = [0; MAX_WORDS];
        for (index, slot) in words.iter_mut().enumerate().take(count) {
            *slot = word(index);
        }
        Self {
            label,
            words,
            count,
        }
    }

    /// The message that asks `request`.
    #[must_use]
    pub fn request_for(request: Request) -> Self {
        match request {
            Request::Read => Self::fixed(READ, &[]),
        }
    }

    /// The message that offers `sample` by call - which the clock PD always
    /// refuses: for a test that it does.
    #[must_use]
    pub fn offer(sample: &Sample) -> Self {
        Self::fixed(OFFER, &sample.to_words())
    }

    /// The label.
    #[must_use]
    pub const fn label(&self) -> u64 {
        self.label
    }

    /// The words, or `None` if the message arrived with more than
    /// [`MAX_WORDS`].
    #[must_use]
    pub fn words(&self) -> Option<&[u64]> {
        self.words.get(..self.count)
    }

    /// The number of words the message has (or arrived with).
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The request this message carries.
    ///
    /// # Errors
    ///
    /// [`ClockError::NotASource`] for a sample offered by call, whatever its
    /// words; [`ClockError::UnknownRequest`] for an unknown label;
    /// [`ClockError::BadLength`] for the wrong number of words.
    pub fn request(&self) -> Result<Request, ClockError> {
        match (self.label, self.words()) {
            (READ, Some([])) => Ok(Request::Read),
            (READ, _) => Err(ClockError::BadLength {
                label: READ,
                words: self.count,
            }),
            (OFFER, _) => Err(ClockError::NotASource),
            (label, _) => Err(ClockError::UnknownRequest { label }),
        }
    }

    /// The reply that carries `result`. The clock PD never sends a
    /// [`ClockError::MalformedReply`] or [`ClockError::BadReading`]; were it
    /// to, the reply would say it has no time.
    #[must_use]
    pub fn reply_for(result: Result<Reading, ClockError>) -> Self {
        let wide = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        match result {
            Ok(reading) => Self::fixed(OK, &reading.to_words()),
            Err(ClockError::UnknownRequest { label }) => Self::fixed(UNKNOWN_REQUEST, &[label]),
            Err(ClockError::BadLength { label, words }) => {
                Self::fixed(BAD_LENGTH, &[label, wide(words)])
            }
            Err(ClockError::NotASource) => Self::fixed(NOT_A_SOURCE, &[]),
            Err(
                ClockError::NoTime | ClockError::MalformedReply { .. } | ClockError::BadReading(_),
            ) => Self::fixed(NO_TIME, &[]),
        }
    }

    /// Reads this message as the reply to [`Request::Read`]: the reading,
    /// validated field by field.
    ///
    /// # Errors
    ///
    /// The error the clock PD refused the request with;
    /// [`ClockError::BadReading`] if the reading does not decode; or
    /// [`ClockError::MalformedReply`] if the reply has the wrong shape.
    pub fn read_reply(&self) -> Result<Reading, ClockError> {
        match (self.label, self.words()) {
            (OK, Some(words)) if words.len() == RECORD_WORDS => {
                Reading::from_words(words).map_err(ClockError::BadReading)
            }
            _ => Err(self.refusal()),
        }
    }

    /// The error a refusal carries, or [`ClockError::MalformedReply`].
    fn refusal(&self) -> ClockError {
        let malformed = ClockError::MalformedReply {
            label: self.label,
            words: self.count,
        };
        let narrow = |value: u64| usize::try_from(value).ok();
        match (self.label, self.words()) {
            (UNKNOWN_REQUEST, Some(&[label])) => ClockError::UnknownRequest { label },
            (BAD_LENGTH, Some(&[label, words])) => {
                narrow(words).map_or(malformed, |words| ClockError::BadLength { label, words })
            }
            (NOT_A_SOURCE, Some([])) => ClockError::NotASource,
            (NO_TIME, Some([])) => ClockError::NoTime,
            _ => malformed,
        }
    }
}

/// Serves one call to the clock PD: the request in `request`, and `now`,
/// which reads monotonic time from the timer PD - asked only for a request
/// that is served, so a malformed call costs the timer nothing.
///
/// # Errors
///
/// The request's refusal ([`Message::request`]), or [`ClockError::NoTime`]
/// if `now` gives no Instant or one earlier than the clock was already given;
/// the clock is then unchanged.
pub fn serve(
    clock: &mut Clock,
    request: &Message,
    now: impl FnOnce() -> Option<Instant>,
) -> Result<Reading, ClockError> {
    match request.request()? {
        Request::Read => {
            let at = now().ok_or(ClockError::NoTime)?;
            let interval = clock.read(at).map_err(|_| ClockError::NoTime)?;
            Ok(Reading { interval, at })
        }
    }
}

/// What became of one record on a source's ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The sample passed every check ([`Clock::take`]): the anchor now if
    /// it was the newest.
    Taken(Taken),
    /// The record was refused, and the time is as it was.
    Refused(Refusal),
}

impl fmt::Display for Outcome {
    /// Fixed text, naming no time or Instant: `taken: ...` or `refused:
    /// ...; the time is as it was`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Taken(Taken::Newer) => f.write_str("taken: it is the newest"),
            Self::Taken(Taken::Older) => f.write_str(
                "valid, but measured no later than the anchor held: the time is as it was",
            ),
            Self::Refused(refusal) => write!(f, "refused: {refusal}; the time is as it was"),
        }
    }
}

/// Why a record on a source's ring was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It is not a sample: it failed to decode, or broke a rule of its
    /// record.
    Malformed(DecodeError),
    /// It is a sample the clock refuses: earlier than the floor, narrower
    /// than its round trip, measured at an Instant still to come, and the
    /// rest of [`Sample::check`].
    Time(TimeError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(err) => write!(f, "it is not a sample: {err}"),
            Self::Time(err) => write!(f, "{err}"),
        }
    }
}

/// Offers the sample in `record`, copied out of a source's ring, to `clock`
/// as from `source` at `now`.
#[must_use]
pub fn take_record<const SLOT: usize>(
    clock: &mut Clock,
    record: &Record<SLOT>,
    source: Source,
    now: Instant,
) -> Outcome {
    match record.decode::<Sample>() {
        Err(err) => Outcome::Refused(Refusal::Malformed(err)),
        Ok(sample) => match clock.take(&sample, source, now) {
            Ok(taken) => Outcome::Taken(taken),
            Err(err) => Outcome::Refused(Refusal::Time(err)),
        },
    }
}

/// Takes the samples waiting on a source's ring - at most
/// [`SAMPLE_RING_SLOTS`], its capacity, so a source cannot keep the clock PD
/// busy - each copied out of shared memory before it is decoded, then
/// checked and offered to `clock` as from `source` at `now`, the Instant the
/// clock PD read after the source's notification. `report` hears each
/// outcome, in order, and the count is returned.
///
/// # Errors
///
/// [`RingError`] if the ring's indices are impossible: the source wrote
/// what only the clock PD may, so nothing more is taken from it.
pub fn take_samples<R: ConsumerMemory<SAMPLE_RING_SLOT> + ?Sized>(
    clock: &mut Clock,
    ring: &R,
    source: Source,
    now: Instant,
    mut report: impl FnMut(Outcome),
) -> Result<usize, RingError> {
    let mut rx = Consumer::new(ring)?;
    let mut taken = 0usize;
    for _ in 0..SAMPLE_RING_SLOTS {
        let Some(record) = rx.try_recv_record()? else {
            break;
        };
        report(take_record(clock, &record, source, now));
        taken = taken.saturating_add(1);
    }
    Ok(taken)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use nowayos_ipc::{InMemoryRing, Producer, ProducerMemory};
    use nowayos_time::{NANOS_PER_SECOND, Utc, ValidityError};

    const MS: u64 = 1_000_000;
    const DAY: u64 = 86_400 * NANOS_PER_SECOND;

    type Ring = InMemoryRing<SAMPLE_RING_SLOTS, SAMPLE_RING_SLOT>;

    fn build_time() -> Utc {
        Utc::from_unix_seconds(1_800_000_000).unwrap()
    }

    fn instant(ms: u64) -> Instant {
        Instant::from_nanos(ms * MS)
    }

    fn clock() -> Clock {
        Clock::new(build_time(), instant(100))
    }

    fn read() -> Message {
        Message::request_for(Request::Read)
    }

    /// A sample of the floor plus `days`, over a 40 ms round trip ending at
    /// `received_ms`.
    fn day_sample(days: u64, received_ms: u64) -> Sample {
        let date = build_time().checked_add_nanos(days * DAY).unwrap();
        Sample::from_date(date, instant(received_ms - 40), instant(received_ms)).unwrap()
    }

    /// A certificate valid from a day before the build to a year after it.
    fn certificate() -> (Utc, Utc) {
        (
            build_time().checked_sub_nanos(DAY).unwrap(),
            build_time().checked_add_nanos(365 * DAY).unwrap(),
        )
    }

    #[test]
    fn a_read_round_trips_as_a_reading_in_five_words() {
        assert_eq!(read().request(), Ok(Request::Read));
        assert_eq!((read().label(), read().count()), (READ, 0));
        let mut clock = clock();
        let reading = serve(&mut clock, &read(), || Some(instant(150))).unwrap();
        assert_eq!(reading.at, instant(150));
        let reply = Message::reply_for(Ok(reading));
        assert_eq!((reply.label(), reply.count()), (OK, RECORD_WORDS));
        assert_eq!(reply.read_reply(), Ok(reading));
    }

    #[test]
    fn before_any_sample_the_time_is_not_known_and_a_date_check_refuses() {
        let mut clock = clock();
        let reading = serve(&mut clock, &read(), || Some(instant(150))).unwrap();
        let interval = reading.interval;
        assert_eq!(interval.source(), Source::BuildTime);
        assert!(!interval.is_known());
        assert!(interval.earliest() >= build_time());
        let (not_before, not_after) = certificate();
        assert_eq!(
            interval.check_validity(not_before, not_after),
            Err(ValidityError::TimeNotKnown)
        );
    }

    #[test]
    fn every_refusal_round_trips() {
        for error in [
            ClockError::UnknownRequest { label: 99 },
            ClockError::BadLength {
                label: READ,
                words: 1,
            },
            ClockError::NotASource,
            ClockError::NoTime,
        ] {
            assert_eq!(Message::reply_for(Err(error)).read_reply(), Err(error));
        }
        // The two errors only a reader makes are sent as no time.
        for error in [
            ClockError::MalformedReply { label: 0, words: 1 },
            ClockError::BadReading(DecodeError::NonZeroPadding),
        ] {
            assert_eq!(
                Message::reply_for(Err(error)).read_reply(),
                Err(ClockError::NoTime)
            );
        }
    }

    #[test]
    fn malformed_requests_are_refused_with_a_typed_error_and_ask_no_time() {
        let sample = day_sample(1, 200);
        let cases = [
            (
                Message::new(99, &[]).unwrap(),
                ClockError::UnknownRequest { label: 99 },
            ),
            (
                Message::new(0, &[]).unwrap(),
                ClockError::UnknownRequest { label: 0 },
            ),
            (
                Message::new(READ, &[1]).unwrap(),
                ClockError::BadLength {
                    label: READ,
                    words: 1,
                },
            ),
            (
                Message::received(READ, 200, |_| 0),
                ClockError::BadLength {
                    label: READ,
                    words: 200,
                },
            ),
            // A sample offered by call is refused whatever it carries.
            (Message::offer(&sample), ClockError::NotASource),
            (Message::new(OFFER, &[]).unwrap(), ClockError::NotASource),
            (Message::received(OFFER, 120, |_| 7), ClockError::NotASource),
        ];
        for (request, error) in cases {
            let mut clock = clock();
            let before = clock.clone();
            let mut asked = false;
            let result = serve(&mut clock, &request, || {
                asked = true;
                Some(instant(150))
            });
            assert_eq!(result, Err(error));
            assert!(!asked, "{error:?} asked the timer");
            assert_eq!(clock, before, "{error:?} changed the clock");
        }
    }

    #[test]
    fn an_offered_sample_leaves_the_time_unchanged() {
        let mut clock = clock();
        let offered = day_sample(1, 200);
        assert_eq!(
            Message::offer(&offered).words(),
            Some(&offered.to_words()[..])
        );
        assert_eq!(
            serve(&mut clock, &Message::offer(&offered), || Some(instant(250))),
            Err(ClockError::NotASource)
        );
        let reading = serve(&mut clock, &read(), || Some(instant(250))).unwrap();
        assert_eq!(reading.interval.source(), Source::BuildTime);
        assert_eq!(clock.anchor(), None);
    }

    #[test]
    fn no_monotonic_time_gives_no_reading() {
        let mut clock = clock();
        assert_eq!(serve(&mut clock, &read(), || None), Err(ClockError::NoTime));
        serve(&mut clock, &read(), || Some(instant(500))).unwrap();
        let before = clock.clone();
        // Monotonic time went back: refused, and the clock is as it was.
        assert_eq!(
            serve(&mut clock, &read(), || Some(instant(499))),
            Err(ClockError::NoTime)
        );
        assert_eq!(clock, before);
    }

    #[test]
    fn a_reply_of_the_wrong_shape_is_malformed() {
        let words = Reading {
            interval: UtcInterval::from_earliest(build_time(), Source::BuildTime),
            at: instant(1),
        }
        .to_words();
        for (label, words) in [
            (OK, &[][..]),
            (OK, &words[..4]),
            (7, &[1][..]),
            (BAD_LENGTH, &[1][..]),
            (NO_TIME, &[1][..]),
        ] {
            let reply = Message::new(label, words).unwrap();
            assert_eq!(
                reply.read_reply(),
                Err(ClockError::MalformedReply {
                    label,
                    words: words.len()
                })
            );
        }
        let oversized = Message::received(OK, MAX_WORDS + 1, |_| 3);
        assert_eq!(oversized.words(), None);
        assert!(matches!(
            oversized.read_reply(),
            Err(ClockError::MalformedReply { .. })
        ));
        // Five words that are not a reading: refused as one, typed.
        // A reserved byte set: the last of the fifth word.
        let mut bad = words;
        bad[4] |= 1 << 63;
        assert_eq!(
            Message::new(OK, &bad).unwrap().read_reply(),
            Err(ClockError::BadReading(DecodeError::NonZeroPadding))
        );
    }

    #[test]
    fn a_source_s_sample_moves_the_time_and_the_date_check_passes() {
        let ring = Ring::new();
        let mut clock = clock();
        let sample = day_sample(1, 240);
        Producer::new(&ring).unwrap().try_send(&sample).unwrap();
        let mut outcomes = Vec::new();
        let count = take_samples(
            &mut clock,
            &ring,
            Source::Backend,
            instant(250),
            |outcome| {
                outcomes.push(outcome);
            },
        );
        assert_eq!(count, Ok(1));
        assert_eq!(outcomes, [Outcome::Taken(Taken::Newer)]);
        let reading = serve(&mut clock, &read(), || Some(instant(300))).unwrap();
        assert_eq!(reading.interval.source(), Source::Backend);
        let (not_before, not_after) = certificate();
        assert_eq!(
            reading.interval.check_validity(not_before, not_after),
            Ok(())
        );
        // Empty now: nothing more is taken.
        assert_eq!(
            take_samples(&mut clock, &ring, Source::Backend, instant(300), |_| {
                panic!("nothing waits")
            }),
            Ok(0)
        );
    }

    #[test]
    fn each_bad_sample_is_refused_with_its_typed_error_and_leaves_the_time() {
        let ring = Ring::new();
        let mut clock = clock();
        Producer::new(&ring)
            .unwrap()
            .try_send(&day_sample(1, 240))
            .unwrap();
        take_samples(&mut clock, &ring, Source::Backend, instant(250), |_| {}).unwrap();
        let before = clock.clone();
        let now = instant(500);
        let early = day_sample(0, 400);
        let before_floor = Sample {
            earliest: build_time().checked_sub_nanos(DAY).unwrap(),
            ..early
        };
        let narrow = Sample {
            latest: day_sample(2, 400).earliest,
            ..day_sample(2, 400)
        };
        let future = day_sample(2, 1_500);
        let mut tx = Producer::new(&ring).unwrap();
        for sample in [before_floor, narrow, future] {
            tx.try_send(&sample).unwrap();
        }
        let mut outcomes = Vec::new();
        take_samples(&mut clock, &ring, Source::Backend, now, |outcome| {
            outcomes.push(outcome);
        })
        .unwrap();
        assert_eq!(outcomes.len(), 3);
        assert!(matches!(
            outcomes[0],
            Outcome::Refused(Refusal::Time(TimeError::BeforeFloor { .. }))
        ));
        assert!(matches!(
            outcomes[1],
            Outcome::Refused(Refusal::Time(TimeError::NarrowerThanRoundTrip { .. }))
        ));
        assert!(matches!(
            outcomes[2],
            Outcome::Refused(Refusal::Time(TimeError::FromTheFuture { .. }))
        ));
        assert_eq!(clock, before);
        assert_eq!(
            outcomes[0].to_string(),
            "refused: its earliest is before the floor; the time is as it was"
        );
        assert_eq!(
            Outcome::Taken(Taken::Newer).to_string(),
            "taken: it is the newest"
        );
    }

    #[test]
    fn a_record_that_is_not_a_sample_is_refused_as_malformed() {
        let ring = Ring::new();
        let mut clock = clock();
        let before = clock.clone();
        let reading = Reading {
            interval: UtcInterval::from_earliest(build_time(), Source::BuildTime),
            at: instant(1),
        };
        let mut tx = Producer::new(&ring).unwrap();
        tx.try_send(&reading).unwrap();
        tx.try_send_bytes(&[0xff; 12]).unwrap();
        let mut outcomes = Vec::new();
        take_samples(
            &mut clock,
            &ring,
            Source::Backend,
            instant(250),
            |outcome| {
                outcomes.push(outcome);
            },
        )
        .unwrap();
        assert!(matches!(
            outcomes[..],
            [
                Outcome::Refused(Refusal::Malformed(DecodeError::WrongKind { .. })),
                Outcome::Refused(Refusal::Malformed(_))
            ]
        ));
        assert_eq!(clock, before);
        assert!(
            Refusal::Malformed(DecodeError::NonZeroPadding)
                .to_string()
                .starts_with("it is not a sample: ")
        );
    }

    #[test]
    fn a_ring_drains_at_most_its_capacity_and_a_corrupt_one_is_refused() {
        let ring = Ring::new();
        let mut clock = clock();
        let mut tx = Producer::new(&ring).unwrap();
        for ms in 0..SAMPLE_RING_SLOTS as u64 {
            tx.try_send(&day_sample(1, 200 + ms)).unwrap();
        }
        let mut seen = 0;
        assert_eq!(
            take_samples(&mut clock, &ring, Source::Backend, instant(300), |_| {
                seen += 1
            }),
            Ok(SAMPLE_RING_SLOTS)
        );
        assert_eq!(seen, SAMPLE_RING_SLOTS);
        // A source that writes an impossible index: nothing is taken.
        ring.store_producer_index(1_000);
        let before = clock.clone();
        assert!(
            take_samples(&mut clock, &ring, Source::Backend, instant(300), |_| {
                panic!("a corrupt ring gives nothing")
            })
            .is_err()
        );
        assert_eq!(clock, before);
    }

    #[test]
    fn the_build_time_is_never_a_source_on_a_ring() {
        let ring = Ring::new();
        let mut clock = clock();
        Producer::new(&ring)
            .unwrap()
            .try_send(&day_sample(1, 240))
            .unwrap();
        let mut outcomes = Vec::new();
        take_samples(
            &mut clock,
            &ring,
            Source::BuildTime,
            instant(250),
            |outcome| {
                outcomes.push(outcome);
            },
        )
        .unwrap();
        assert_eq!(
            outcomes,
            [Outcome::Refused(Refusal::Time(TimeError::NotASampleSource))]
        );
    }

    #[test]
    fn errors_read_as_fixed_text() {
        assert_eq!(
            ClockError::NotASource.to_string(),
            "samples come only on a source's ring"
        );
        assert_eq!(
            ClockError::BadLength {
                label: READ,
                words: 1
            }
            .to_string(),
            "request 1 sent 1 word(s), the wrong number"
        );
        assert_eq!(
            ClockError::UnknownRequest { label: 99 }.to_string(),
            "unknown request 99"
        );
    }
}
