// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: the clock's two records decoded from
//! any byte string up to their length and one more never panic, overflow or
//! read out of bounds, and whatever decodes has its earliest no later than
//! its latest and encodes back to the same bytes - a reading from message
//! registers too; an anchor's interval
//! carried to any Instant never panics or overflows and moves exactly as
//! documented - so, with its latest known, its width grows by twice the
//! drift - and a later Instant never moves its earliest back nor learns a
//! latest; the clock takes any sample and reads any Instant without a panic,
//! takes only samples that pass every check, leaves itself as it was when it
//! refuses one, and never reads below the floor; and a date check passes
//! exactly when the certificate is valid for the whole interval.
//!
//! **The drift, by its properties.** [`drift`] divides by 9,999, and Kani's
//! solver cannot finish a proof that compares two such divisions - the
//! interval at a later Instant against an earlier one, or the same division
//! done twice: two 64-bit divisions did not finish in 15 minutes, two 32-bit
//! ones take about a minute. So every proof here runs the arithmetic one of
//! two ways. The proofs on `drift` itself compare nothing that it computes:
//! no panic, no overflow, the rules every interval keeps. The proofs that
//! compare - the exact amounts, a later Instant against an earlier one, a
//! sample against the floor - pass the arithmetic, through its `_by` form,
//! an abstract drift ([`AnyDrift`]): any function that is 0 exactly at 0,
//! never more than the elapsed time, and never falls nor rises faster than
//! it, which `ceil(elapsed / 9,999)` is, by arithmetic. They hold for every
//! such function, `drift` among them. [`drift_steps_by_at_most_one`] checks
//! those properties on `drift` itself for elapsed times below 2^32 ns, and
//! the property tests (`tests/properties.rs`) at any width. Two proofs that
//! compare widths directly ran for over five minutes even so - an anchor's
//! at two Instants, and the clock's readings, which carry two anchors at
//! once - so that a later Instant never gives a narrower interval rests on
//! the exact amounts proved here and on the property tests, which check it
//! for anchors and for the clock.
//!
//! Every proof is unwound ten times: the decoders read at most six reserved
//! bytes in a loop, the abstract drift checks at most eight earlier values,
//! and nothing else here loops. Kani checks that bound is enough.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use core::cell::{Cell, RefCell};

use nowayos_ipc::{HEADER_LEN, Header, Message, decode, decode_exact, encode};

use crate::record::{READING_LEN, SAMPLE_LEN};
use crate::{
    Anchor, Clock, Instant, Reading, Sample, Source, Taken, Utc, UtcInterval, ValidityError, drift,
};

/// Both records are 40 bytes.
const RECORD_LEN: usize = 40;
const _: () = assert!(SAMPLE_LEN == RECORD_LEN && READING_LEN == RECORD_LEN);

/// Any byte string of at most `RECORD_LEN + 1` bytes: every record, short of
/// it and one byte past.
const INPUT_LEN: usize = RECORD_LEN + 1;

/// The two records are the same bytes, compared as five 8-byte words: no
/// loop that Kani would have to unwind.
fn same(a: &[u8; RECORD_LEN], b: &[u8; RECORD_LEN]) -> bool {
    let word = |bytes: &[u8; RECORD_LEN], index: usize| {
        let mut word = [0u8; 8];
        word.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
        u64::from_le_bytes(word)
    };
    (0..5).all(|index| word(a, index) == word(b, index))
}

/// `decode` and `decode_exact` of `M` return, whatever the input, and agree;
/// whatever decodes keeps `rules`.
fn any_input_keeps_rules<M: Message>(rules: fn(&M) -> bool) {
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    let bytes = &buf[..len];
    let prefix = decode::<M>(bytes);
    if let Ok(message) = &prefix {
        assert!(rules(message));
    }
    if let Ok(message) = decode_exact::<M>(bytes) {
        assert!(prefix.is_ok());
        assert!(rules(&message));
    }
}

/// Any payload behind `M`'s header: whatever `decode_exact` accepts keeps
/// `rules` and encodes back to exactly the same bytes.
fn any_payload_keeps_rules<M: Message>(rules: fn(&M) -> bool) {
    let mut record = [0u8; RECORD_LEN];
    let header = Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: M::PAYLOAD_LEN as u16,
    };
    assert!(header.write(&mut record).is_ok());
    let payload: [u8; RECORD_LEN - HEADER_LEN] = kani::any();
    record[HEADER_LEN..].copy_from_slice(&payload);
    if let Ok(message) = decode_exact::<M>(&record) {
        assert!(rules(&message));
        let mut out = [0u8; RECORD_LEN];
        assert!(encode(&message, &mut out) == Ok(RECORD_LEN));
        assert!(same(&out, &record));
    }
}

/// A decoded sample's rules: its earliest is not after its latest, nor its
/// answer before its request.
fn sample_rules(sample: &Sample) -> bool {
    sample.earliest <= sample.latest && sample.sent <= sample.received
}

/// A decoded reading's rules: when its latest is known, its earliest is not
/// after it; the build time never knows it.
fn reading_rules(reading: &Reading) -> bool {
    let interval = reading.interval;
    match interval.latest() {
        Some(latest) => interval.earliest() <= latest && interval.source() != Source::BuildTime,
        None => true,
    }
}

#[kani::proof]
#[kani::unwind(10)]
fn sample_any_input() {
    any_input_keeps_rules::<Sample>(sample_rules);
}

#[kani::proof]
#[kani::unwind(10)]
fn sample_any_payload() {
    any_payload_keeps_rules::<Sample>(sample_rules);
}

#[kani::proof]
#[kani::unwind(10)]
fn reading_any_input() {
    any_input_keeps_rules::<Reading>(reading_rules);
}

#[kani::proof]
#[kani::unwind(10)]
fn reading_any_payload() {
    any_payload_keeps_rules::<Reading>(reading_rules);
}

/// The most words the proof below offers: a record's five and one more.
const WORDS_MAX: usize = crate::record::RECORD_WORDS + 1;

/// A reading from any message registers - any words, up to a record's five
/// and one more: `Reading::from_words` returns, whatever they are,
/// exactly what `decode_exact` gives on the same bytes; whatever decodes
/// keeps the reading's rules and gives back the same five words.
#[kani::proof]
#[kani::unwind(10)]
fn reading_from_any_words() {
    let words: [u64; WORDS_MAX] = kani::any();
    let count: usize = kani::any_where(|count: &usize| *count <= WORDS_MAX);
    let mut bytes = [0u8; WORDS_MAX * 8];
    for (chunk, word) in bytes.chunks_exact_mut(8).zip(words.iter()) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    let decoded = Reading::from_words(&words[..count]);
    assert!(decoded == decode_exact::<Reading>(&bytes[..count * 8]));
    if let Ok(reading) = decoded {
        assert!(reading_rules(&reading));
        assert!(count == crate::record::RECORD_WORDS);
        let again = reading.to_words();
        assert!((0..crate::record::RECORD_WORDS).all(|index| again[index] == words[index]));
    }
}

/// Any source.
fn any_source() -> Source {
    if kani::any() {
        Source::Backend
    } else {
        Source::BuildTime
    }
}

/// Any interval: any earliest, any latest not before it (or none), any
/// source - the build time with no latest.
fn any_interval() -> UtcInterval {
    let earliest = Utc::from_unix_nanos(kani::any());
    let source = any_source();
    let latest = if kani::any() && source != Source::BuildTime {
        let latest: u64 = kani::any_where(|latest: &u64| *latest >= earliest.as_unix_nanos());
        Some(Utc::from_unix_nanos(latest))
    } else {
        None
    };
    let interval = UtcInterval::new(earliest, latest, source);
    assert!(interval.is_some());
    interval.unwrap_or(UtcInterval::from_earliest(earliest, source))
}

/// Any anchor.
fn any_anchor() -> Anchor {
    Anchor::new(Instant::from_nanos(kani::any()), any_interval())
}

/// Any sample.
fn any_sample() -> Sample {
    Sample {
        earliest: Utc::from_unix_nanos(kani::any()),
        latest: Utc::from_unix_nanos(kani::any()),
        sent: Instant::from_nanos(kani::any()),
        received: Instant::from_nanos(kani::any()),
    }
}

/// Any clock that knows only the floor.
fn any_clock() -> Clock {
    Clock::new(
        Utc::from_unix_nanos(kani::any()),
        Instant::from_nanos(kani::any()),
    )
}

/// The most elapsed times one proof asks an [`AnyDrift`] for.
const DRIFT_POINTS: usize = 8;

/// An abstract drift: asked for the drift over an elapsed time, it gives
/// any value that is 0 exactly when the time is, no more than the time, and
/// - against every value it gave before - neither less for a longer time
/// nor more by more than the extra time. `drift` is such a function.
struct AnyDrift {
    given: RefCell<[(u64, u64); DRIFT_POINTS]>,
    count: Cell<usize>,
}

impl AnyDrift {
    const fn new() -> Self {
        Self {
            given: RefCell::new([(0, 0); DRIFT_POINTS]),
            count: Cell::new(0),
        }
    }

    /// Whether `d` over `elapsed` and `other_d` over `other` can both be
    /// such a function's.
    fn consistent(elapsed: u64, d: u64, other: u64, other_d: u64) -> bool {
        if elapsed <= other {
            d <= other_d && other_d - d <= other - elapsed
        } else {
            other_d <= d && d - other_d <= elapsed - other
        }
    }

    fn drift(&self, elapsed: u64) -> u64 {
        let d: u64 = kani::any();
        kani::assume(d <= elapsed && (d == 0) == (elapsed == 0));
        let count = self.count.get();
        assert!(count < DRIFT_POINTS);
        let mut given = self.given.borrow_mut();
        for &(other, other_d) in &given[..count] {
            kani::assume(Self::consistent(elapsed, d, other, other_d));
        }
        given[count] = (elapsed, d);
        self.count.set(count + 1);
        d
    }
}

/// `drift` over any elapsed time below 2^32 ns, against the drift one
/// nanosecond later: the same or one more, no more than the time, and 0
/// exactly at 0 - the properties [`AnyDrift`] stands for, below 2^32 ns; at
/// any 64-bit width they rest on the property tests.
#[kani::proof]
fn drift_steps_by_at_most_one() {
    let elapsed: u64 = kani::any_where(|elapsed: &u64| *elapsed < 1 << 32);
    let (d, next) = (drift(elapsed), drift(elapsed + 1));
    assert!(next == d || next == d + 1);
    assert!(d <= elapsed && (d == 0) == (elapsed == 0));
}

/// Any anchor carried to any Instant with `drift` itself: never a panic or
/// an overflow; refused exactly when the Instant is before the anchor's;
/// otherwise its source kept, its latest unknown if it was, and its
/// earliest no later than its latest.
#[kani::proof]
fn anchor_carried_to_any_instant() {
    let anchor = any_anchor();
    let now = Instant::from_nanos(kani::any());
    let Ok(moved) = anchor.interval_at(now) else {
        assert!(now < anchor.at());
        return;
    };
    assert!(now >= anchor.at());
    let interval = anchor.interval();
    assert!(moved.source() == interval.source());
    assert!(interval.latest().is_some() || moved.latest().is_none());
    if let Some(latest) = moved.latest() {
        assert!(moved.earliest() <= latest);
    }
}

/// Any anchor carried to any later Instant: its earliest moves on by
/// exactly the elapsed time less the drift (stopping at `Utc::MAX`), and its
/// latest by the elapsed time plus it (unknown past `Utc::MAX`).
#[kani::proof]
#[kani::unwind(10)]
fn anchor_moves_on_exactly_by_the_drift() {
    let anchor = any_anchor();
    let at = anchor.at().as_nanos();
    let now: u64 = kani::any_where(|now: &u64| *now >= at);
    let drifts = AnyDrift::new();
    let Ok(moved) =
        anchor.interval_at_by(Instant::from_nanos(now), |elapsed| drifts.drift(elapsed))
    else {
        panic!("an anchor is carried to any later Instant");
    };
    let elapsed = now - at;
    assert!(drifts.count.get() == 1);
    let (asked, d) = drifts.given.borrow()[0];
    assert!(asked == elapsed);
    let interval = anchor.interval();
    let earliest = interval.earliest().as_unix_nanos();
    assert!(moved.earliest().as_unix_nanos() == earliest.saturating_add(elapsed - d));
    let latest = interval
        .latest()
        .and_then(|latest| latest.as_unix_nanos().checked_add(elapsed))
        .and_then(|latest| latest.checked_add(d));
    assert!(moved.latest().map(Utc::as_unix_nanos) == latest);
}

/// An anchor carried to two later Instants: the later one never gives an
/// earlier earliest, nor a latest once it is unknown. That it is never
/// narrower follows from [`anchor_moves_on_exactly_by_the_drift`]: with the
/// latest known, the width is the anchor's plus twice the drift, which never
/// falls.
#[kani::proof]
#[kani::unwind(10)]
fn anchor_never_goes_back() {
    let anchor = any_anchor();
    let at = anchor.at().as_nanos();
    let first: u64 = kani::any_where(|first: &u64| *first >= at);
    let second: u64 = kani::any_where(|second: &u64| *second >= first);
    let drifts = AnyDrift::new();
    let drift_of = |elapsed| drifts.drift(elapsed);
    let (Ok(one), Ok(two)) = (
        anchor.interval_at_by(Instant::from_nanos(first), drift_of),
        anchor.interval_at_by(Instant::from_nanos(second), drift_of),
    ) else {
        panic!("an anchor is carried to any later Instant");
    };
    assert!(two.earliest() >= one.earliest());
    assert!(one.latest().is_some() || two.latest().is_none());
}

/// Any sample, offered with `drift` itself to a clock that knows only the
/// floor, at any Instant: never a panic; a refusal leaves the clock as it
/// was; a sample taken is from the backend, and is the anchor now.
#[kani::proof]
fn clock_takes_any_sample() {
    let mut clock = any_clock();
    let before = clock.clone();
    let sample = any_sample();
    let source = any_source();
    let now = Instant::from_nanos(kani::any());
    match clock.take(&sample, source, now) {
        Ok(taken) => {
            assert!(taken == Taken::Newer && source == Source::Backend);
            assert!(
                clock
                    .anchor()
                    .is_some_and(|anchor| anchor.at() == sample.received
                        && anchor.interval().earliest() == sample.earliest
                        && anchor.interval().latest() == Some(sample.latest))
            );
        }
        Err(_) => assert!(clock == before),
    }
}

/// A sample the clock takes passed every check: its earliest not after its
/// latest, its answer not before its request nor after now, no narrower
/// than its round trip, and its earliest not before the floor where it was
/// measured.
#[kani::proof]
#[kani::unwind(10)]
fn clock_takes_only_samples_that_pass_every_check() {
    let mut clock = any_clock();
    let before = clock.clone();
    let sample = any_sample();
    let now = Instant::from_nanos(kani::any());
    let drifts = AnyDrift::new();
    let drift_of = |elapsed| drifts.drift(elapsed);
    if clock
        .take_by(&sample, Source::Backend, now, drift_of)
        .is_ok()
    {
        assert!(sample.earliest <= sample.latest);
        assert!(sample.sent <= sample.received && sample.received <= now);
        let width = sample.latest.as_unix_nanos() - sample.earliest.as_unix_nanos();
        assert!(width >= sample.received.as_nanos() - sample.sent.as_nanos());
        let floor = before.floor();
        let floor_then = floor
            .interval_at_by(sample.received, drift_of)
            .map_or(floor.interval().earliest(), |interval| interval.earliest());
        assert!(sample.earliest >= floor_then);
    }
}

/// A clock holding any sample it took, read with `drift` itself at any
/// Instant: never a panic; refused exactly before the Instant it took the
/// sample at; otherwise the sample's source, and its earliest no later than
/// its latest.
#[kani::proof]
fn clock_reads_any_instant() {
    let mut clock = any_clock();
    let sample = any_sample();
    let taken = Instant::from_nanos(kani::any());
    kani::assume(clock.take(&sample, Source::Backend, taken).is_ok());
    let now = Instant::from_nanos(kani::any());
    let Ok(reading) = clock.read(now) else {
        assert!(now < taken);
        return;
    };
    assert!(now >= taken);
    assert!(reading.source() == Source::Backend);
    if let Some(latest) = reading.latest() {
        assert!(reading.earliest() <= latest);
    }
}

/// Whatever the clock holds, it never reads below the floor.
#[kani::proof]
#[kani::unwind(10)]
fn clock_never_reads_below_the_floor() {
    let mut clock = any_clock();
    let drifts = AnyDrift::new();
    let drift_of = |elapsed| drifts.drift(elapsed);
    if kani::any() {
        let taken = Instant::from_nanos(kani::any());
        kani::assume(
            clock
                .take_by(&any_sample(), Source::Backend, taken, drift_of)
                .is_ok(),
        );
    }
    let now = Instant::from_nanos(kani::any());
    if let Ok(reading) = clock.read_by(now, drift_of) {
        let floor = clock.floor().interval_at_by(now, drift_of);
        assert!(floor.is_ok_and(|floor| reading.earliest() >= floor.earliest()));
    }
}

/// A date check passes exactly when the time is known and the certificate
/// is valid for the whole interval.
#[kani::proof]
fn validity_only_for_the_whole_interval() {
    let interval = any_interval();
    let not_before = Utc::from_unix_nanos(kani::any());
    let not_after = Utc::from_unix_nanos(kani::any());
    let result = interval.check_validity(not_before, not_after);
    match interval.latest() {
        None => assert!(result == Err(ValidityError::TimeNotKnown)),
        Some(latest) => {
            assert!(result.is_ok() == (not_before <= interval.earliest() && latest <= not_after))
        }
    }
}
