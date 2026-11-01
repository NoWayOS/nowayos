// SPDX-License-Identifier: MIT OR Apache-2.0
//! The counts as a test system gives them to its supervisor: a
//! **test-only stand-in** for the store still to come. A system has no
//! store yet, so a test system prefills a read-only region with the counts
//! at build time (`tools/board-system`, `test-counts-*.bin`), and the grants
//! check refuses that prefill outside a test system (`tools/check-system`
//! rule 12). Being read-only, the stand-in stores nothing: every boot reads
//! the same counts. The store still to come replaces it, with a record of its own,
//! which gets its fuzz target and proofs with that store.
//!
//! The region holds what a read of the store returned: a little-endian
//! `u32`, the number of bytes read, then those bytes - the counts' record:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 1 | version, [`VERSION`] |
//! | 1 | 1 | the restart count |
//! | 2 | 1 | the sensitive count |
//! | 3 | 4 | the check: CRC-32 (IEEE 802.3, the one zlib computes) of bytes 0 to 2, little-endian |
//!
//! [`read`] tells apart counts that are missing (no bytes were read), cut
//! short, of an unknown version, too long, or failing their check
//! ([`Unreadable`]): each is counts that cannot be read or checked, which
//! [`decide`](crate::decide) treats as the limit reached.

use core::fmt;

use crate::Counts;

/// The record's version.
pub const VERSION: u8 = 1;

/// The record's length.
pub const RECORD_LEN: usize = 7;

/// The bytes a stand-in holds: the length word and one record. A reader
/// copies at least these out of the region and passes them to [`read`];
/// bytes past them never matter.
pub const STAND_IN_LEN: usize = 4 + RECORD_LEN;

/// The longest input Kani's proof of [`read`] covers (`src/proofs.rs`): one
/// byte past a stand-in, which shows that bytes past it never matter. A
/// reader passes [`read`] no more than this - the supervisor passes the 12
/// bytes of three whole words.
pub const PROVEN_LEN: usize = STAND_IN_LEN + 1;

/// Why counts cannot be read or checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// No bytes: nothing was ever stored.
    Missing,
    /// Fewer bytes than a record: a write cut short.
    CutShort {
        /// The bytes there are.
        len: usize,
    },
    /// A version this code does not know.
    UnknownVersion {
        /// The version the record gives.
        version: u8,
    },
    /// More bytes than a record.
    TooLong {
        /// The bytes the read returned.
        len: usize,
    },
    /// The check does not match the counts.
    CheckFailed,
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no record"),
            Self::CutShort { len } => {
                write!(f, "the record is cut short: {len} of {RECORD_LEN} bytes")
            }
            Self::UnknownVersion { version } => write!(f, "record version {version} is unknown"),
            Self::TooLong { len } => write!(f, "the record is too long: {len} bytes"),
            Self::CheckFailed => f.write_str("the record fails its check"),
        }
    }
}

/// CRC-32 as IEEE 802.3 and zlib define it (reflected, polynomial
/// `0xEDB8_8320`, all ones in and out): `crc32(b"123456789")` is
/// `0xCBF4_3926`. A check against a damaged record, not against an attacker.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let low_bit = crc & 1 == 1;
            crc = crc.wrapping_shr(1);
            if low_bit {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}

/// The record of `counts`.
#[must_use]
pub fn encode(counts: Counts) -> [u8; RECORD_LEN] {
    let body = [VERSION, counts.restarts, counts.sensitive];
    let [c0, c1, c2, c3] = crc32(&body).to_le_bytes();
    [VERSION, counts.restarts, counts.sensitive, c0, c1, c2, c3]
}

/// The counts in `record`, exactly the bytes read.
///
/// # Errors
///
/// [`Unreadable`]: no bytes; a version other than [`VERSION`]; fewer or more
/// bytes than [`RECORD_LEN`]; or a check that does not match.
pub fn decode(record: &[u8]) -> Result<Counts, Unreadable> {
    let Some(&version) = record.first() else {
        return Err(Unreadable::Missing);
    };
    if version != VERSION {
        return Err(Unreadable::UnknownVersion { version });
    }
    let &[_, restarts, sensitive, c0, c1, c2, c3] = record else {
        return Err(if record.len() < RECORD_LEN {
            Unreadable::CutShort { len: record.len() }
        } else {
            Unreadable::TooLong { len: record.len() }
        });
    };
    if u32::from_le_bytes([c0, c1, c2, c3]) != crc32(&[version, restarts, sensitive]) {
        return Err(Unreadable::CheckFailed);
    }
    Ok(Counts {
        restarts,
        sensitive,
    })
}

/// The counts in a stand-in region's first bytes, copied out of it first:
/// the length word, then the record it counts.
///
/// # Errors
///
/// As for [`decode`]; [`Unreadable::Missing`] if there is no length word or
/// it is zero; [`Unreadable::TooLong`] if it counts more than a record, and
/// [`Unreadable::CutShort`] if `stand_in` holds fewer bytes than it counts.
pub fn read(stand_in: &[u8]) -> Result<Counts, Unreadable> {
    let Some((len, rest)) = stand_in.split_first_chunk::<4>() else {
        return Err(Unreadable::Missing);
    };
    let len = usize::try_from(u32::from_le_bytes(*len)).unwrap_or(usize::MAX);
    if len == 0 {
        return Err(Unreadable::Missing);
    }
    if len > RECORD_LEN {
        return Err(Unreadable::TooLong { len });
    }
    match rest.get(..len) {
        Some(record) => decode(record),
        None => Err(Unreadable::CutShort { len: rest.len() }),
    }
}

/// What a stand-in region holds for `counts`: the length word and the
/// record, as `tools/board-system` writes `test-counts-R-S.bin`.
#[must_use]
pub fn stand_in(counts: Counts) -> [u8; STAND_IN_LEN] {
    let mut bytes = [0; STAND_IN_LEN];
    let len = u32::try_from(RECORD_LEN).unwrap_or(0).to_le_bytes();
    for (slot, byte) in bytes.iter_mut().zip(len.into_iter().chain(encode(counts))) {
        *slot = byte;
    }
    bytes
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn the_check_is_zlibs_crc_32() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_record_reads_back_as_its_counts() {
        let counts = Counts {
            restarts: 3,
            sensitive: 1,
        };
        assert_eq!(decode(&encode(counts)), Ok(counts));
        assert_eq!(read(&stand_in(counts)), Ok(counts));
        // What tools/board-system writes for test-counts-3-1.bin.
        assert_eq!(
            stand_in(counts),
            [7, 0, 0, 0, 1, 3, 1, 0x70, 0xd0, 0xa9, 0xa2]
        );
    }

    #[test]
    fn each_way_counts_cannot_be_read_is_told_apart() {
        let record = encode(Counts::ZERO);
        assert_eq!(read(&[0; STAND_IN_LEN]), Err(Unreadable::Missing));
        assert_eq!(read(&[]), Err(Unreadable::Missing));
        let mut cut = stand_in(Counts::ZERO);
        cut[0] = 4;
        assert_eq!(read(&cut), Err(Unreadable::CutShort { len: 4 }));
        let mut version = record;
        version[0] = 2;
        assert_eq!(
            decode(&version),
            Err(Unreadable::UnknownVersion { version: 2 })
        );
        let mut long = stand_in(Counts::ZERO);
        long[0] = 8;
        assert_eq!(read(&long), Err(Unreadable::TooLong { len: 8 }));
        let mut damaged = record;
        damaged[6] ^= 1;
        assert_eq!(decode(&damaged), Err(Unreadable::CheckFailed));
        let mut flipped = record;
        flipped[1] ^= 1;
        assert_eq!(decode(&flipped), Err(Unreadable::CheckFailed));
    }
}
