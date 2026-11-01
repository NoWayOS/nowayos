// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-time`'s records - the sample a source sends the
//! clock PD on its ring, and the reading the clock PD answers with - decoded
//! from any bytes, and every sample that decodes offered to a clock at
//! Instants the fuzzer chooses, as the clock PD takes it.
//!
//! Checks: decoding never panics, agrees with itself and is canonical
//! (`nowayos_fuzz::check_decode`); whatever decodes has its earliest no later
//! than its latest, and a sample's answer no earlier than its request; the
//! same bytes as message registers - the words a reading crosses a protected
//! procedure call in - decode exactly as the bytes do; the
//! clock takes a sample exactly when it passes every check, and one it
//! refuses leaves it as it was; and its readings never fall below the
//! floor, never move their earliest back and never narrow.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::decode_exact;
use nowayos_time::{Clock, Instant, Reading, Sample, Source, Utc};

/// The next eight bytes as a little-endian `u64`.
fn u64_from(input: &Input<'_>) -> u64 {
    u64::from_le_bytes(input.array())
}

/// The most words offered as message registers: a record's five, and more.
const WORDS_MAX: usize = 8;

/// `record`'s whole words, as message registers carry them: `from_words`
/// gives exactly what `decode_exact` gives on the same bytes, and whatever
/// decodes gives back the same words.
fn check_words(record: &[u8]) {
    let mut all = [0u64; WORDS_MAX];
    let mut count = 0usize;
    for (word, chunk) in all.iter_mut().zip(record.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap_or_default());
        count = count.saturating_add(1);
    }
    let (Some(words), Some(bytes)) = (all.get(..count), record.get(..count.saturating_mul(8)))
    else {
        return;
    };
    let reading = Reading::from_words(words);
    assert_eq!(reading, decode_exact::<Reading>(bytes), "{words:x?}");
    if let Ok(reading) = reading {
        assert_eq!(reading.to_words().as_slice(), words, "{reading:?}");
    }
    let sample = Sample::from_words(words);
    assert_eq!(sample, decode_exact::<Sample>(bytes), "{words:x?}");
    if let Ok(sample) = sample {
        assert_eq!(sample.to_words().as_slice(), words, "{sample:?}");
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mode = input.byte();
    let build = Utc::from_unix_nanos(u64_from(&input));
    let start = u64_from(&input);
    let waits = [u64_from(&input), u64_from(&input)];
    let mut buf = [0u8; FRAME_MAX];
    let record = match mode % 3 {
        1 => frame::<Sample>(input.rest(), &mut buf),
        2 => frame::<Reading>(input.rest(), &mut buf),
        _ => input.rest(),
    };
    check_header(record);
    check_words(record);
    if let Some(reading) = check_decode::<Reading>(record) {
        let interval = reading.interval;
        assert!(
            interval
                .latest()
                .is_none_or(|latest| interval.earliest() <= latest
                    && interval.source() != Source::BuildTime),
            "{reading:?}"
        );
    }
    let Some(sample) = check_decode::<Sample>(record) else {
        return;
    };
    assert!(
        sample.earliest <= sample.latest && sample.sent <= sample.received,
        "{sample:?}"
    );
    let mut clock = Clock::new(build, Instant::from_nanos(start));
    let now = Instant::from_nanos(start.saturating_add(waits[0]));
    let before = clock.clone();
    let checked = sample.check(before.floor(), now);
    match clock.take(&sample, Source::Backend, now) {
        Ok(_) => assert_eq!(checked, Ok(()), "{sample:?} taken at {now:?}"),
        Err(refused) => {
            assert_eq!(Err(refused), checked, "{sample:?} at {now:?}");
            assert_eq!(clock, before, "a refused sample changed the clock");
        }
    }
    let later = Instant::from_nanos(now.as_nanos().saturating_add(waits[1]));
    let results = (
        clock.read(now),
        clock.read(later),
        clock.floor().interval_at(later),
    );
    assert!(
        results.0.is_ok() && results.1.is_ok() && results.2.is_ok(),
        "a clock is read at {now:?} and {later:?}: {results:?}"
    );
    let (Ok(one), Ok(two), Ok(floor)) = results else {
        return;
    };
    assert!(
        two.earliest() >= floor.earliest(),
        "{two:?} below {floor:?}"
    );
    assert!(two.earliest() >= one.earliest(), "{two:?} before {one:?}");
    match (one.width(), two.width()) {
        (Some(narrow), Some(wide)) => assert!(wide >= narrow, "{two:?} narrower than {one:?}"),
        (None, width) => assert_eq!(width, None, "{two:?} learned a latest"),
        (Some(_), None) => {}
    }
});
