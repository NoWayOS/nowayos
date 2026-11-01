// SPDX-License-Identifier: MIT OR Apache-2.0
//! The clock's two records, as `nowayos-ipc` messages: the [`Sample`] a
//! source sends on its ring to the clock PD, and the [`Reading`] the clock PD
//! answers "what is the time?" with. Each is validated field by field when
//! decoded, so whatever decodes has its earliest no later than its latest,
//! and every value has one encoding.
//!
//! Integers are little-endian; times are `u64` nanoseconds - [`Utc`] since
//! 1970-01-01T00:00:00Z, [`Instant`] since the counter started.
//!
//! **Sample** ([`SAMPLE_KIND`], version 1, 32 bytes of payload):
//!
//! | Offset | Size | Field | Rule |
//! | --- | --- | --- | --- |
//! | 0 | 8 | `earliest` (Utc) | |
//! | 8 | 8 | `latest` (Utc) | not before `earliest` |
//! | 16 | 8 | `sent` (Instant) | |
//! | 24 | 8 | `received` (Instant) | not before `sent` |
//!
//! **Reading** ([`READING_KIND`], version 1, 32 bytes of payload):
//!
//! | Offset | Size | Field | Rule |
//! | --- | --- | --- | --- |
//! | 0 | 8 | `earliest` (Utc) | |
//! | 8 | 8 | `latest` (Utc) | not before `earliest` when known; 0 when not |
//! | 16 | 8 | `at` (Instant) | the Instant it was read at |
//! | 24 | 1 | `source` | 1 the build time, 2 the backend ([`Source::tag`]) |
//! | 25 | 1 | `known` | 1 if `latest` is known, else 0; never 1 for the build time |
//! | 26 | 6 | reserved | zero |
//!
//! **In message registers.** The clock PD answers "what is the time?" by
//! protected procedure call, whose message registers are 64-bit words, so a
//! record also travels as [`RECORD_WORDS`] words: its 40 bytes, eight to a
//! word, little-endian ([`Reading::to_words`], [`Reading::from_words`]; the
//! same for a [`Sample`], which the clock refuses by call). Decoding
//! from words is decoding the same bytes with [`decode_exact`]: any other
//! number of words is refused, short as `Truncated`, long as
//! `TrailingBytes`.

use nowayos_ipc::{
    DecodeError, EncodeError, Message, Reader, Writer, decode_exact, encode, encoded_len,
};

use crate::{Instant, Sample, Source, Utc, UtcInterval};

/// Message kind of a [`Sample`] on a source's ring to the clock PD.
pub const SAMPLE_KIND: u16 = 0x0401;

/// Message kind of a [`Reading`], the clock PD's answer.
pub const READING_KIND: u16 = 0x0402;

/// A sample record's length: header and payload.
pub const SAMPLE_LEN: usize = encoded_len::<Sample>();

/// A reading record's length: header and payload.
pub const READING_LEN: usize = encoded_len::<Reading>();

/// The slots in a source's ring to the clock PD: samples are rare, so a few
/// are plenty.
pub const SAMPLE_RING_SLOTS: usize = 4;

/// The size of each slot in a source's ring: room for one sample record.
pub const SAMPLE_RING_SLOT: usize = 64;

// A sample fits a slot, and the ring's capacity is a power of two, as
// nowayos-ipc's ring requires.
const _: () = assert!(SAMPLE_LEN <= SAMPLE_RING_SLOT && SAMPLE_RING_SLOTS.is_power_of_two());

/// A record's length in message registers: five 64-bit words, its 40 bytes
/// little-endian.
pub const RECORD_WORDS: usize = 5;

/// Both records' length in bytes: exactly [`RECORD_WORDS`] words.
const RECORD_LEN: usize = 40;
const _: () = assert!(
    SAMPLE_LEN == RECORD_LEN && READING_LEN == RECORD_LEN && RECORD_WORDS * 8 == RECORD_LEN
);

/// `message`'s record as [`RECORD_WORDS`] little-endian words. A clock
/// record always encodes into its 40 bytes; were that ever to fail, the
/// words would be zero, which no decoder accepts (a record of kind 0).
fn words_of<M: Message>(message: &M) -> [u64; RECORD_WORDS] {
    let mut bytes = [0u8; RECORD_LEN];
    let mut words = [0u64; RECORD_WORDS];
    if encode(message, &mut bytes).is_ok() {
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
            let mut le = [0u8; 8];
            le.copy_from_slice(chunk);
            *word = u64::from_le_bytes(le);
        }
    }
    words
}

/// The record of `M` that `words` carry, decoded and validated exactly as
/// [`decode_exact`] decodes the same bytes: fewer than [`RECORD_WORDS`]
/// words are `Truncated`, more are `TrailingBytes` once the record itself
/// decodes.
fn from_words_of<M: Message>(words: &[u64]) -> Result<M, DecodeError> {
    let mut bytes = [0u8; RECORD_LEN];
    for (chunk, word) in bytes.chunks_exact_mut(8).zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    let len = words.len().min(RECORD_WORDS).saturating_mul(8);
    let message = decode_exact::<M>(bytes.get(..len).unwrap_or(&[]))?;
    match words.len().saturating_sub(RECORD_WORDS) {
        0 => Ok(message),
        extra => Err(DecodeError::TrailingBytes {
            unread: extra.saturating_mul(8),
        }),
    }
}

impl Sample {
    /// The sample's record as [`RECORD_WORDS`] message registers (the
    /// module's note): only a test PD sends one by call, to see it refused.
    #[must_use]
    pub fn to_words(&self) -> [u64; RECORD_WORDS] {
        words_of(self)
    }

    /// The sample whose record `words` carry, validated field by field.
    ///
    /// # Errors
    ///
    /// As [`decode_exact`] on the same bytes: the wrong kind, version or
    /// length, a field out of its range - or too few words, or too many.
    pub fn from_words(words: &[u64]) -> Result<Self, DecodeError> {
        from_words_of(words)
    }
}

impl Reading {
    /// The reading's record as [`RECORD_WORDS`] message registers: the
    /// clock PD's reply to "what is the time?".
    #[must_use]
    pub fn to_words(&self) -> [u64; RECORD_WORDS] {
        words_of(self)
    }

    /// The reading whose record `words` carry, validated field by field:
    /// whatever decodes has its earliest no later than its latest, and from
    /// the build time no latest at all.
    ///
    /// # Errors
    ///
    /// As [`Sample::from_words`].
    pub fn from_words(words: &[u64]) -> Result<Self, DecodeError> {
        from_words_of(words)
    }
}

/// The clock PD's answer to "what is the time?": what it knew at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reading {
    /// The interval.
    pub interval: UtcInterval,
    /// The Instant the clock read it at.
    pub at: Instant,
}

impl Message for Sample {
    const KIND: u16 = SAMPLE_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 32;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u64(self.earliest.as_unix_nanos())?;
        out.write_u64(self.latest.as_unix_nanos())?;
        out.write_u64(self.sent.as_nanos())?;
        out.write_u64(self.received.as_nanos())
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let earliest = Utc::from_unix_nanos(input.read_u64()?);
        let latest = Utc::from_unix_nanos(input.read_u64()?);
        let sent = Instant::from_nanos(input.read_u64()?);
        let received = Instant::from_nanos(input.read_u64()?);
        if latest < earliest {
            return Err(DecodeError::InvalidValue { field: "latest" });
        }
        if received < sent {
            return Err(DecodeError::InvalidValue { field: "received" });
        }
        Ok(Self {
            earliest,
            latest,
            sent,
            received,
        })
    }
}

impl Message for Reading {
    const KIND: u16 = READING_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 32;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let interval = &self.interval;
        out.write_u64(interval.earliest().as_unix_nanos())?;
        out.write_u64(interval.latest().map_or(0, Utc::as_unix_nanos))?;
        out.write_u64(self.at.as_nanos())?;
        out.write_u8(interval.source().tag())?;
        out.write_bool(interval.is_known())?;
        out.write_zeros(6)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let earliest = Utc::from_unix_nanos(input.read_u64()?);
        let latest = input.read_u64()?;
        let at = Instant::from_nanos(input.read_u64()?);
        let source = Source::from_tag(input.read_u8()?)
            .ok_or(DecodeError::InvalidValue { field: "source" })?;
        let known = input.read_bool()?;
        input.read_zeros(6)?;
        let latest = match (known, latest) {
            (true, nanos) => Some(Utc::from_unix_nanos(nanos)),
            (false, 0) => None,
            (false, _) => return Err(DecodeError::InvalidValue { field: "latest" }),
        };
        let interval = UtcInterval::new(earliest, latest, source).ok_or(
            if source == Source::BuildTime && known {
                DecodeError::InvalidValue { field: "known" }
            } else {
                DecodeError::InvalidValue { field: "latest" }
            },
        )?;
        Ok(Self { interval, at })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{decode, decode_exact, encode};

    use super::*;
    use crate::NANOS_PER_SECOND;

    fn sample() -> Sample {
        let date = Utc::from_unix_seconds(1_800_086_400).unwrap();
        Sample::from_date(
            date,
            Instant::from_nanos(7),
            Instant::from_nanos(40_000_007),
        )
        .unwrap()
    }

    fn reading(known: bool) -> Reading {
        let earliest = Utc::from_unix_seconds(1_800_000_000).unwrap();
        let interval = if known {
            UtcInterval::new(
                earliest,
                earliest.checked_add_nanos(NANOS_PER_SECOND),
                Source::Backend,
            )
            .unwrap()
        } else {
            UtcInterval::from_earliest(earliest, Source::BuildTime)
        };
        Reading {
            interval,
            at: Instant::from_nanos(123_456_789),
        }
    }

    fn bytes<M: Message>(message: &M) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let len = encode(message, &mut buf).unwrap();
        buf[..len].to_vec()
    }

    #[test]
    fn layouts_are_exactly_as_documented() {
        assert_eq!((SAMPLE_LEN, READING_LEN), (40, 40));
        let record = bytes(&sample());
        assert_eq!(record[..8], [0x01, 0x04, 1, 0, 32, 0, 0, 0]);
        assert_eq!(
            record[8..16],
            sample().earliest.as_unix_nanos().to_le_bytes()
        );
        assert_eq!(
            record[16..24],
            sample().latest.as_unix_nanos().to_le_bytes()
        );
        assert_eq!(record[24..32], 7u64.to_le_bytes());
        assert_eq!(record[32..40], 40_000_007u64.to_le_bytes());
        let known = bytes(&reading(true));
        assert_eq!(known[..8], [0x02, 0x04, 1, 0, 32, 0, 0, 0]);
        assert_eq!(known[24..32], 123_456_789u64.to_le_bytes());
        assert_eq!(known[32..40], [2, 1, 0, 0, 0, 0, 0, 0]);
        let unknown = bytes(&reading(false));
        assert_eq!(unknown[16..24], [0; 8]);
        assert_eq!(unknown[32..40], [1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn records_round_trip() {
        assert_eq!(decode_exact::<Sample>(&bytes(&sample())), Ok(sample()));
        for known in [true, false] {
            assert_eq!(
                decode_exact::<Reading>(&bytes(&reading(known))),
                Ok(reading(known))
            );
        }
    }

    #[test]
    fn a_sample_record_keeps_its_rules() {
        let good = bytes(&sample());
        let mut backwards = good.clone();
        backwards[8..16].copy_from_slice(&good[16..24]);
        backwards[16..24].copy_from_slice(&good[8..16]);
        assert_eq!(
            decode::<Sample>(&backwards),
            Err(DecodeError::InvalidValue { field: "latest" })
        );
        let mut reversed = good.clone();
        reversed[24..32].copy_from_slice(&good[32..40]);
        reversed[32..40].copy_from_slice(&good[24..32]);
        assert_eq!(
            decode::<Sample>(&reversed),
            Err(DecodeError::InvalidValue { field: "received" })
        );
    }

    #[test]
    fn a_reading_record_keeps_its_rules() {
        let known = bytes(&reading(true));
        let unknown = bytes(&reading(false));
        let with = |base: &[u8], at: usize, value: &[u8]| {
            let mut bytes = base.to_vec();
            bytes[at..at + value.len()].copy_from_slice(value);
            bytes
        };
        let invalid = |field| Err(DecodeError::InvalidValue { field });
        // An unknown source, or none.
        assert_eq!(
            decode::<Reading>(&with(&known, 32, &[0])),
            invalid("source")
        );
        assert_eq!(
            decode::<Reading>(&with(&known, 32, &[3])),
            invalid("source")
        );
        // `known` is 0 or 1.
        assert_eq!(decode::<Reading>(&with(&known, 33, &[2])), invalid("bool"));
        // A latest before the earliest.
        let early = Utc::from_unix_seconds(1_000)
            .unwrap()
            .as_unix_nanos()
            .to_le_bytes();
        assert_eq!(
            decode::<Reading>(&with(&known, 16, &early)),
            invalid("latest")
        );
        // An unknown latest is zero, so it has one encoding.
        assert_eq!(
            decode::<Reading>(&with(&unknown, 16, &[1])),
            invalid("latest")
        );
        // The build time never knows the latest.
        assert_eq!(decode::<Reading>(&with(&known, 32, &[1])), invalid("known"));
        // Reserved bytes are zero.
        assert_eq!(
            decode::<Reading>(&with(&known, 39, &[1])),
            Err(DecodeError::NonZeroPadding)
        );
    }

    #[test]
    fn records_travel_as_five_little_endian_words() {
        // The words are the record's bytes, eight to a word.
        for (words, record) in [
            (reading(true).to_words(), bytes(&reading(true))),
            (reading(false).to_words(), bytes(&reading(false))),
            (sample().to_words(), bytes(&sample())),
        ] {
            assert_eq!(words.len(), RECORD_WORDS);
            for (index, word) in words.iter().enumerate() {
                assert_eq!(word.to_le_bytes(), record[index * 8..index * 8 + 8]);
            }
        }
        // The first word is the header: kind, version and payload length.
        assert_eq!(reading(true).to_words()[0], 0x0000_0020_0001_0402);
        for known in [true, false] {
            assert_eq!(
                Reading::from_words(&reading(known).to_words()),
                Ok(reading(known))
            );
        }
        assert_eq!(Sample::from_words(&sample().to_words()), Ok(sample()));
    }

    #[test]
    fn words_decode_exactly_as_the_same_bytes() {
        let words = reading(true).to_words();
        // Too few words: the record is cut short.
        assert_eq!(
            Reading::from_words(&words[..4]),
            Err(DecodeError::Truncated {
                needed: 40,
                available: 32
            })
        );
        assert!(matches!(
            Reading::from_words(&[]),
            Err(DecodeError::Truncated { available: 0, .. })
        ));
        // Too many: refused once the record itself decodes.
        let mut long = [0u64; 7];
        long[..5].copy_from_slice(&words);
        assert_eq!(
            Reading::from_words(&long),
            Err(DecodeError::TrailingBytes { unread: 16 })
        );
        // A sample's words are not a reading's, and each field keeps its
        // rules: a latest before the earliest is refused.
        assert!(matches!(
            Reading::from_words(&sample().to_words()),
            Err(DecodeError::WrongKind { .. })
        ));
        let mut backwards = words;
        backwards.swap(1, 2);
        assert_eq!(
            Reading::from_words(&backwards),
            Err(DecodeError::InvalidValue { field: "latest" })
        );
        // Whatever the words, the result is decode_exact's on their bytes.
        for count in 0..=7 {
            let mut any = [0u64; 7];
            any[..5].copy_from_slice(&words);
            any[5] = 0x5555;
            let flat: Vec<u8> = any[..count].iter().flat_map(|w| w.to_le_bytes()).collect();
            assert_eq!(
                Reading::from_words(&any[..count]),
                decode_exact::<Reading>(&flat),
                "{count} words"
            );
        }
    }
}
