// SPDX-License-Identifier: MIT OR Apache-2.0
//! The signed trust list.
//!
//! The root key signs only the trust list: which public keys may sign
//! images, configuration and the next trust list, each with its purpose,
//! and the list's own version. A device takes a new list only if its
//! version is higher than the one it holds; a root change is a list signed
//! by both the old root and the new one; and a development list is marked
//! as one, so that production refuses it. Those decisions, and checking the
//! signatures, are the update PD's: this module decodes, so that nothing
//! about a list is used before its bytes are checked.
//!
//! # Wire layout
//!
//! A `nowayos-ipc` record of kind [`TrustList::KIND`] (1), layout version 1,
//! whose payload is exactly [`TrustList::PAYLOAD_LEN`] (784) bytes:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 4 | The list's version |
//! | 4 | 1 | The development mark: 1 for a development list, else 0 |
//! | 5 | 1 | The number of keys, 1 to [`MAX_KEYS`] |
//! | 6 | 2 | Reserved, zero |
//! | 8 | 72 × [`MAX_KEYS`] | The keys, each: its purpose's code (1 byte), 7 reserved bytes, the key ([`KEY_LEN`] bytes) |
//! | 584 | 1 | The number of signatures, 1 or 2 |
//! | 585 | 7 | Reserved, zero |
//! | 592 | 96 × [`MAX_SIGNATURES`] | The signatures, each: the signing key's ID ([`KEY_ID_LEN`] bytes), then the signature ([`SIGNATURE_LEN`] bytes) |
//!
//! Every byte of an entry past its count is zero. The signatures cover the
//! record's first [`SIGNED_LEN`] bytes: its header and every field before
//! the number of signatures ([`TrustListContents`]).
//!
//! # What the decoder refuses
//!
//! Besides a malformed frame ([`TrustListError::Frame`]), in the order the
//! fields are read: a development mark other than 0 or 1; no key, or more
//! than [`MAX_KEYS`]; a reserved byte of the header that is not zero; then,
//! key entry by entry, a used entry's unknown purpose code, then its
//! reserved bytes not zero, or any non-zero byte of an unused entry; no key
//! that may sign the next trust list; one key listed twice, or for two
//! purposes (no key signs two kinds of thing); no
//! signature, or more than [`MAX_SIGNATURES`]; a reserved byte after the
//! number of signatures, or a byte of an unused signature entry, that is not
//! zero; two signatures naming the same key.

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer};

use crate::{KEY_ID_LEN, KEY_LEN, NamedSignature, SIGNATURE_LEN, record};

/// The most keys one trust list names.
pub const MAX_KEYS: usize = 8;

/// The most signatures one trust list carries: the root key's, or during a
/// root change the old root's and the new root's.
pub const MAX_SIGNATURES: usize = 2;

/// Reserved bytes in a key entry, after its purpose.
const KEY_RESERVED: usize = 7;

/// The length of one key entry: purpose, reserved bytes, key.
const KEY_ENTRY_LEN: usize = 1 + KEY_RESERVED + KEY_LEN;

/// The length of one signature entry: key ID, signature.
const SIGNATURE_ENTRY_LEN: usize = KEY_ID_LEN + SIGNATURE_LEN;

/// Reserved bytes after the number of signatures.
const SIGNATURES_RESERVED: usize = 7;

/// The length of the signed fields: version, mark, number of keys, reserved
/// bytes, keys.
const CONTENTS_LEN: usize = 4 + 1 + 1 + 2 + KEY_ENTRY_LEN * MAX_KEYS;

/// The length of the signatures: their number, reserved bytes, entries.
const SIGNATURES_LEN: usize = 1 + SIGNATURES_RESERVED + SIGNATURE_ENTRY_LEN * MAX_SIGNATURES;

/// The length of the payload: the signed fields, then the signatures.
const PAYLOAD_LEN: usize = CONTENTS_LEN + SIGNATURES_LEN;

/// The bytes a trust list's signatures cover: the record's header and its
/// signed fields, the record's first 592 bytes.
pub const SIGNED_LEN: usize = HEADER_LEN + CONTENTS_LEN;

/// The length of a whole trust list record, header included: 792 bytes.
pub const RECORD_LEN: usize = HEADER_LEN + PAYLOAD_LEN;

/// Where the key entries start in the signed fields, and the signature
/// entries in the signatures.
const KEYS_AT: usize = 8;
const SIGNATURES_AT: usize = 8;

const _: () = assert!(CONTENTS_LEN == 584 && RECORD_LEN == 792 && PAYLOAD_LEN <= 0xFFFF);

/// What a listed key may sign (one key per purpose).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// The next trust list: a root key. Every list names at least one.
    TrustList,
    /// Each release's system image and its update manifest.
    Image,
    /// A device's configuration, when it is shipped apart from the image.
    Configuration,
}

impl Purpose {
    /// The purpose's code on the wire.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::TrustList => 1,
            Self::Image => 2,
            Self::Configuration => 3,
        }
    }

    /// The purpose a code names, or `None` for any other byte.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::TrustList),
            2 => Some(Self::Image),
            3 => Some(Self::Configuration),
            _ => None,
        }
    }
}

/// One key a trust list names, and what it may sign. Plain data: a list
/// checks that no key appears twice, and the signature check that each is a
/// point on the curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListedKey {
    /// What the key may sign.
    pub purpose: Purpose,
    /// The P-256 public key: x then y, big-endian (the crate documentation).
    pub key: [u8; KEY_LEN],
}

/// Why a trust list was refused. Decoding refuses with the first problem
/// found, in the order the module documentation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrustListError {
    /// The record's frame, as `nowayos-ipc` checks it: too short, another
    /// kind, version or payload length, the header's reserved field set, or
    /// bytes after the record.
    Frame(DecodeError),
    /// A reserved byte, or a byte of an entry past its count, is not zero.
    ReservedNotZero,
    /// The development mark is neither 0 nor 1.
    DevelopmentMark {
        /// The mark's byte.
        found: u8,
    },
    /// The list names no key.
    NoKey,
    /// The list names more than [`MAX_KEYS`] keys.
    TooManyKeys {
        /// The number of keys it names.
        count: usize,
    },
    /// A key's purpose code is none of [`Purpose`]'s.
    UnknownPurpose {
        /// The key's entry, from 0.
        entry: usize,
        /// The code.
        code: u8,
    },
    /// No key may sign the next trust list.
    NoTrustListKey,
    /// The same key is listed twice, for the same purpose.
    KeyListedTwice {
        /// The first entry naming it, from 0.
        first: usize,
        /// The second.
        second: usize,
    },
    /// The same key is listed for two purposes: no key signs two kinds of
    /// thing.
    KeyForTwoPurposes {
        /// The first entry naming it, from 0.
        first: usize,
        /// The second.
        second: usize,
    },
    /// The list carries no signature.
    NoSignature,
    /// The list carries more than [`MAX_SIGNATURES`] signatures.
    TooManySignatures {
        /// The number of signatures it carries.
        count: usize,
    },
    /// Two signatures name the same key.
    SignerNamedTwice,
}

impl TrustListError {
    /// The error `nowayos-ipc` reports for the same input: a frame's error
    /// as it is, a reserved byte as [`DecodeError::NonZeroPadding`], and
    /// every other rule as [`DecodeError::InvalidValue`] naming its field.
    #[must_use]
    pub const fn to_decode_error(self) -> DecodeError {
        match self {
            Self::Frame(error) => error,
            Self::ReservedNotZero => DecodeError::NonZeroPadding,
            Self::DevelopmentMark { .. } => DecodeError::InvalidValue {
                field: "development",
            },
            Self::NoKey | Self::TooManyKeys { .. } => {
                DecodeError::InvalidValue { field: "key count" }
            }
            Self::UnknownPurpose { .. } => DecodeError::InvalidValue { field: "purpose" },
            Self::NoTrustListKey | Self::KeyListedTwice { .. } | Self::KeyForTwoPurposes { .. } => {
                DecodeError::InvalidValue { field: "keys" }
            }
            Self::NoSignature | Self::TooManySignatures { .. } => DecodeError::InvalidValue {
                field: "signature count",
            },
            Self::SignerNamedTwice => DecodeError::InvalidValue {
                field: "signatures",
            },
        }
    }
}

impl From<DecodeError> for TrustListError {
    /// A reserved byte that is not zero, as the payload reader reports it,
    /// is [`TrustListError::ReservedNotZero`]; anything else is the frame's.
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NonZeroPadding => Self::ReservedNotZero,
            other => Self::Frame(other),
        }
    }
}

impl fmt::Display for TrustListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "trust list record: {error}"),
            Self::ReservedNotZero => f.write_str("a reserved byte of the trust list is not zero"),
            Self::DevelopmentMark { found } => {
                write!(
                    f,
                    "the trust list's development mark is {found}, not 0 or 1"
                )
            }
            Self::NoKey => f.write_str("the trust list names no key"),
            Self::TooManyKeys { count } => {
                write!(f, "the trust list names {count} keys, more than {MAX_KEYS}")
            }
            Self::UnknownPurpose { entry, code } => {
                write!(
                    f,
                    "key {entry} of the trust list has an unknown purpose, {code}"
                )
            }
            Self::NoTrustListKey => {
                f.write_str("the trust list names no key that may sign the next trust list")
            }
            Self::KeyListedTwice { first, second } => {
                write!(
                    f,
                    "keys {first} and {second} of the trust list are the same key"
                )
            }
            Self::KeyForTwoPurposes { first, second } => write!(
                f,
                "keys {first} and {second} of the trust list are one key with two purposes"
            ),
            Self::NoSignature => f.write_str("the trust list carries no signature"),
            Self::TooManySignatures { count } => write!(
                f,
                "the trust list carries {count} signatures, more than {MAX_SIGNATURES}"
            ),
            Self::SignerNamedTwice => {
                f.write_str("two of the trust list's signatures name the same key")
            }
        }
    }
}

impl core::error::Error for TrustListError {}

/// What a trust list's signatures cover: its version, its development mark
/// and its keys. It holds the fields' bytes, checked when it was made -
/// built by [`TrustListContents::new`] or decoded - and reads what it says
/// from them, so it keeps the module's rules and encodes as exactly the
/// bytes it came from.
#[derive(Clone, PartialEq, Eq)]
pub struct TrustListContents {
    fields: [u8; CONTENTS_LEN],
}

impl TrustListContents {
    /// The signed part of a trust list with this version, development mark
    /// and keys, in this order.
    ///
    /// # Errors
    ///
    /// [`TrustListError::NoKey`], [`TrustListError::TooManyKeys`],
    /// [`TrustListError::NoTrustListKey`],
    /// [`TrustListError::KeyListedTwice`] or
    /// [`TrustListError::KeyForTwoPurposes`], as decoding would refuse them.
    pub fn new(
        version: u32,
        development: bool,
        keys: &[ListedKey],
    ) -> Result<Self, TrustListError> {
        let count = check_key_count(keys.len())?;
        let head = version.to_le_bytes().into_iter().chain([
            u8::from(development),
            u8::try_from(count).unwrap_or(u8::MAX),
            0,
            0,
        ]);
        let entries = keys.iter().flat_map(|key| {
            [key.purpose.code()]
                .into_iter()
                .chain([0; KEY_RESERVED])
                .chain(key.key)
        });
        let mut fields = [0u8; CONTENTS_LEN];
        for (slot, byte) in fields.iter_mut().zip(head.chain(entries)) {
            *slot = byte;
        }
        check_contents(&fields)?;
        Ok(Self { fields })
    }

    /// The list's version: a device takes a list only if this is higher
    /// than its own list's.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.fields
            .first_chunk::<4>()
            .map_or(0, |bytes| u32::from_le_bytes(*bytes))
    }

    /// Whether this is a development list, which production refuses.
    #[must_use]
    pub fn is_development(&self) -> bool {
        self.fields.get(4) == Some(&1)
    }

    /// How many keys the list names: from 1 to [`MAX_KEYS`].
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.fields.get(5).map_or(0, |count| usize::from(*count))
    }

    /// The keys, in the order the list names them.
    pub fn keys(&self) -> impl Iterator<Item = ListedKey> + '_ {
        self.fields
            .get(KEYS_AT..)
            .unwrap_or_default()
            .chunks_exact(KEY_ENTRY_LEN)
            .take(self.key_count())
            .filter_map(|entry| {
                let (code, rest) = entry.split_first()?;
                Some(ListedKey {
                    purpose: Purpose::from_code(*code)?,
                    key: rest.get(KEY_RESERVED..)?.try_into().ok()?,
                })
            })
    }

    /// The bytes the list's signatures cover: the record's header, then
    /// these fields, exactly as the record carries them.
    #[must_use]
    pub fn signed_bytes(&self) -> [u8; SIGNED_LEN] {
        let mut out = [0u8; SIGNED_LEN];
        let bytes = record::header_bytes::<TrustList>()
            .into_iter()
            .chain(self.fields);
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = byte;
        }
        out
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, TrustListError> {
        let fields = input.read_array()?;
        check_contents(&fields)?;
        Ok(Self { fields })
    }
}

impl fmt::Debug for TrustListContents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustListContents")
            .field("version", &self.version())
            .field("development", &self.is_development())
            .field("keys", &DebugList(|| self.keys()))
            .finish()
    }
}

/// The signed trust list: its [`TrustListContents`] and one or two
/// signatures, each naming its key. Like its contents, it holds its bytes,
/// checked when it was made - by [`TrustList::new`] or by decoding;
/// whether its signatures are good is for the signature check to say.
#[derive(Clone, PartialEq, Eq)]
pub struct TrustList {
    contents: TrustListContents,
    signatures: [u8; SIGNATURES_LEN],
}

impl TrustList {
    /// A trust list of `contents`, signed with `signatures`.
    ///
    /// # Errors
    ///
    /// [`TrustListError::NoSignature`],
    /// [`TrustListError::TooManySignatures`] or
    /// [`TrustListError::SignerNamedTwice`], as decoding would refuse them.
    pub fn new(
        contents: TrustListContents,
        signatures: &[NamedSignature],
    ) -> Result<Self, TrustListError> {
        let count = check_signature_count(signatures.len())?;
        let head = [u8::try_from(count).unwrap_or(u8::MAX)]
            .into_iter()
            .chain([0; SIGNATURES_RESERVED]);
        let entries = signatures
            .iter()
            .flat_map(|signature| signature.key_id.into_iter().chain(signature.signature));
        let mut block = [0u8; SIGNATURES_LEN];
        for (slot, byte) in block.iter_mut().zip(head.chain(entries)) {
            *slot = byte;
        }
        check_signatures(&block)?;
        Ok(Self {
            contents,
            signatures: block,
        })
    }

    /// Decodes `bytes` - copied out of shared memory first - as exactly one
    /// trust list record.
    ///
    /// # Errors
    ///
    /// The first problem found, as the module documentation lists them.
    pub fn decode(bytes: &[u8]) -> Result<Self, TrustListError> {
        record::decode_exact(bytes, Self::read)
    }

    /// What the signatures cover.
    #[must_use]
    pub const fn contents(&self) -> &TrustListContents {
        &self.contents
    }

    /// The list's version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.contents.version()
    }

    /// Whether this is a development list.
    #[must_use]
    pub fn is_development(&self) -> bool {
        self.contents.is_development()
    }

    /// The keys, in the order the list names them.
    pub fn keys(&self) -> impl Iterator<Item = ListedKey> + '_ {
        self.contents.keys()
    }

    /// How many signatures the list carries: 1 or 2.
    #[must_use]
    pub fn signature_count(&self) -> usize {
        self.signatures
            .first()
            .map_or(0, |count| usize::from(*count))
    }

    /// The signatures, in the order the list carries them.
    pub fn signatures(&self) -> impl Iterator<Item = NamedSignature> + '_ {
        self.signatures
            .get(SIGNATURES_AT..)
            .unwrap_or_default()
            .chunks_exact(SIGNATURE_ENTRY_LEN)
            .take(self.signature_count())
            .filter_map(|entry| {
                let (key_id, signature) = entry.split_first_chunk::<KEY_ID_LEN>()?;
                Some(NamedSignature {
                    key_id: *key_id,
                    signature: signature.try_into().ok()?,
                })
            })
    }

    /// The bytes the signatures cover: the record's first [`SIGNED_LEN`]
    /// bytes.
    #[must_use]
    pub fn signed_bytes(&self) -> [u8; SIGNED_LEN] {
        self.contents.signed_bytes()
    }

    /// Reads the signed fields, then the signatures, each copied whole into
    /// an array of its own length and checked there: two reads that can run
    /// past the input's end, and every field's place fixed.
    fn read(input: &mut Reader<'_>) -> Result<Self, TrustListError> {
        let contents = TrustListContents::read(input)?;
        let signatures = input.read_array()?;
        check_signatures(&signatures)?;
        Ok(Self {
            contents,
            signatures,
        })
    }
}

impl fmt::Debug for TrustList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustList")
            .field("contents", &self.contents)
            .field("signatures", &DebugList(|| self.signatures()))
            .finish()
    }
}

impl Message for TrustList {
    const KIND: u16 = 1;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.contents.fields)?;
        out.write_bytes(&self.signatures)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(TrustListError::to_decode_error)
    }
}

/// Lists what an iterator yields, for `Debug`.
struct DebugList<F>(F);

impl<F, I> fmt::Debug for DebugList<F>
where
    F: Fn() -> I,
    I: Iterator,
    I::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries((self.0)()).finish()
    }
}

/// A number of keys, if it is from 1 to `MAX_KEYS`.
fn check_key_count(count: usize) -> Result<usize, TrustListError> {
    match count {
        0 => Err(TrustListError::NoKey),
        count if count > MAX_KEYS => Err(TrustListError::TooManyKeys { count }),
        count => Ok(count),
    }
}

/// A number of signatures, if it is from 1 to `MAX_SIGNATURES`.
fn check_signature_count(count: usize) -> Result<usize, TrustListError> {
    match count {
        0 => Err(TrustListError::NoSignature),
        count if count > MAX_SIGNATURES => Err(TrustListError::TooManySignatures { count }),
        count => Ok(count),
    }
}

/// Checks the signed fields' bytes, in the order the module documentation
/// gives. Every entry is read the same way and then checked, and every loop
/// runs over a whole array: what is read, and how often a loop runs, never
/// depend on the input, which keeps Kani's proofs small.
fn check_contents(fields: &[u8; CONTENTS_LEN]) -> Result<(), TrustListError> {
    let mut input = Reader::new(fields);
    input.read_u32()?;
    match input.read_u8()? {
        0 | 1 => {}
        found => return Err(TrustListError::DevelopmentMark { found }),
    }
    let count = check_key_count(usize::from(input.read_u8()?))?;
    record::zeros::<2>(&mut input)?;
    let mut keys: [&[u8]; MAX_KEYS] = [&[]; MAX_KEYS];
    let mut purposes = [Purpose::TrustList; MAX_KEYS];
    for (entry, (key, purpose)) in keys.iter_mut().zip(purposes.iter_mut()).enumerate() {
        let code = input.read_u8()?;
        let reserved = input.read_bytes(KEY_RESERVED)?;
        *key = input.read_bytes(KEY_LEN)?;
        if entry < count {
            *purpose =
                Purpose::from_code(code).ok_or(TrustListError::UnknownPurpose { entry, code })?;
            if reserved != [0; KEY_RESERVED] {
                return Err(TrustListError::ReservedNotZero);
            }
        } else if code != 0 || reserved != [0; KEY_RESERVED] || *key != [0; KEY_LEN] {
            return Err(TrustListError::ReservedNotZero);
        }
    }
    input.finish()?;
    if !purposes
        .iter()
        .enumerate()
        .any(|(entry, purpose)| entry < count && *purpose == Purpose::TrustList)
    {
        return Err(TrustListError::NoTrustListKey);
    }
    for (first, (earlier, earlier_purpose)) in keys.iter().zip(&purposes).enumerate() {
        for (second, (later, later_purpose)) in keys.iter().zip(&purposes).enumerate() {
            if first < second && second < count && earlier == later {
                return Err(if earlier_purpose == later_purpose {
                    TrustListError::KeyListedTwice { first, second }
                } else {
                    TrustListError::KeyForTwoPurposes { first, second }
                });
            }
        }
    }
    Ok(())
}

/// Checks the signatures' bytes: their number, the reserved bytes, unused
/// entries all zero, and no two naming the same key - over the whole array,
/// as `check_contents`.
fn check_signatures(block: &[u8; SIGNATURES_LEN]) -> Result<(), TrustListError> {
    let mut input = Reader::new(block);
    let count = check_signature_count(usize::from(input.read_u8()?))?;
    record::zeros::<SIGNATURES_RESERVED>(&mut input)?;
    let mut key_ids: [&[u8]; MAX_SIGNATURES] = [&[]; MAX_SIGNATURES];
    for (entry, key_id) in key_ids.iter_mut().enumerate() {
        *key_id = input.read_bytes(KEY_ID_LEN)?;
        let signature = input.read_bytes(SIGNATURE_LEN)?;
        if entry >= count && (*key_id != [0; KEY_ID_LEN] || signature != [0; SIGNATURE_LEN]) {
            return Err(TrustListError::ReservedNotZero);
        }
    }
    input.finish()?;
    for (first, earlier) in key_ids.iter().enumerate() {
        for (second, later) in key_ids.iter().enumerate() {
            if first < second && second < count && earlier == later {
                return Err(TrustListError::SignerNamedTwice);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{decode_exact, encode};

    use super::*;

    fn key(fill: u8) -> [u8; KEY_LEN] {
        [fill; KEY_LEN]
    }

    fn signature(fill: u8) -> NamedSignature {
        NamedSignature {
            key_id: [fill; KEY_ID_LEN],
            signature: [fill.wrapping_add(1); SIGNATURE_LEN],
        }
    }

    fn sample() -> TrustList {
        let contents = TrustListContents::new(
            0x0102_0304,
            true,
            &[
                ListedKey {
                    purpose: Purpose::TrustList,
                    key: key(0xA1),
                },
                ListedKey {
                    purpose: Purpose::Image,
                    key: key(0xB2),
                },
                ListedKey {
                    purpose: Purpose::Configuration,
                    key: key(0xC3),
                },
            ],
        )
        .unwrap();
        TrustList::new(contents, &[signature(0x51)]).unwrap()
    }

    fn encoded(list: &TrustList) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        assert_eq!(encode(list, &mut out), Ok(RECORD_LEN));
        out
    }

    #[test]
    fn layout_is_exactly_as_documented() {
        let bytes = encoded(&sample());
        // Header: kind 1, version 1, payload length 784, reserved.
        assert_eq!(bytes[..8], [1, 0, 1, 0, 0x10, 0x03, 0, 0]);
        let payload = &bytes[HEADER_LEN..];
        assert_eq!(payload[..8], [4, 3, 2, 1, 1, 3, 0, 0]);
        for (entry, (code, fill)) in [(1u8, 0xA1u8), (2, 0xB2), (3, 0xC3)].iter().enumerate() {
            let at = 8 + entry * 72;
            assert_eq!(payload[at], *code);
            assert_eq!(payload[at + 1..at + 8], [0; 7]);
            assert_eq!(payload[at + 8..at + 72], [*fill; 64]);
        }
        assert!(payload[8 + 3 * 72..584].iter().all(|byte| *byte == 0));
        assert_eq!(payload[584..592], [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(payload[592..624], [0x51; 32]);
        assert_eq!(payload[624..688], [0x52; 64]);
        assert!(payload[688..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn round_trip_and_signed_bytes() {
        let list = sample();
        let bytes = encoded(&list);
        assert_eq!(TrustList::decode(&bytes), Ok(list.clone()));
        assert_eq!(decode_exact::<TrustList>(&bytes), Ok(list.clone()));
        assert_eq!(list.signed_bytes()[..], bytes[..SIGNED_LEN]);
        assert_eq!(record::header_bytes::<TrustList>()[..], bytes[..HEADER_LEN]);
        assert_eq!(list.version(), 0x0102_0304);
        assert!(list.is_development());
        assert_eq!(list.contents().key_count(), 3);
        let keys: Vec<ListedKey> = list.keys().collect();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[2].purpose, Purpose::Configuration);
        assert_eq!(keys[2].key, key(0xC3));
        assert_eq!(list.signature_count(), 1);
        assert_eq!(list.signatures().collect::<Vec<_>>(), [signature(0x51)]);
        assert!(format!("{list:?}").contains("version: 16909060"));
    }

    #[test]
    fn the_constructors_keep_the_rules() {
        let root = ListedKey {
            purpose: Purpose::TrustList,
            key: key(1),
        };
        let image = ListedKey {
            purpose: Purpose::Image,
            key: key(2),
        };
        assert_eq!(
            TrustListContents::new(1, false, &[]),
            Err(TrustListError::NoKey)
        );
        assert_eq!(
            TrustListContents::new(1, false, &[root; MAX_KEYS + 1]),
            Err(TrustListError::TooManyKeys {
                count: MAX_KEYS + 1
            })
        );
        assert_eq!(
            TrustListContents::new(1, false, &[image]),
            Err(TrustListError::NoTrustListKey)
        );
        assert_eq!(
            TrustListContents::new(1, false, &[root, image, root]),
            Err(TrustListError::KeyListedTwice {
                first: 0,
                second: 2
            })
        );
        let root_as_image = ListedKey {
            purpose: Purpose::Image,
            key: root.key,
        };
        assert_eq!(
            TrustListContents::new(1, false, &[image, root, root_as_image]),
            Err(TrustListError::KeyForTwoPurposes {
                first: 1,
                second: 2
            })
        );
        let contents = TrustListContents::new(1, false, &[root, image]).unwrap();
        assert_eq!(
            TrustList::new(contents.clone(), &[]),
            Err(TrustListError::NoSignature)
        );
        assert_eq!(
            TrustList::new(
                contents.clone(),
                &[signature(1), signature(2), signature(3)]
            ),
            Err(TrustListError::TooManySignatures { count: 3 })
        );
        assert_eq!(
            TrustList::new(contents, &[signature(4), signature(4)]),
            Err(TrustListError::SignerNamedTwice)
        );
    }

    #[test]
    fn errors_map_to_what_nowayos_ipc_reports() {
        let bytes = encoded(&sample());
        let mut bad = bytes;
        bad[HEADER_LEN + 4] = 2;
        assert_eq!(
            TrustList::decode(&bad),
            Err(TrustListError::DevelopmentMark { found: 2 })
        );
        assert_eq!(
            decode_exact::<TrustList>(&bad),
            Err(DecodeError::InvalidValue {
                field: "development"
            })
        );
        let mut bad = bytes;
        bad[HEADER_LEN + 6] = 1;
        assert_eq!(
            TrustList::decode(&bad),
            Err(TrustListError::ReservedNotZero)
        );
        assert_eq!(
            decode_exact::<TrustList>(&bad),
            Err(DecodeError::NonZeroPadding)
        );
        let mut bad = bytes;
        bad[6] = 1;
        assert_eq!(
            TrustList::decode(&bad),
            Err(TrustListError::Frame(DecodeError::ReservedNotZero))
        );
    }

    #[test]
    fn purposes_have_one_code_each() {
        for code in 0..=u8::MAX {
            if let Some(purpose) = Purpose::from_code(code) {
                assert_eq!(purpose.code(), code);
            } else {
                assert!(code == 0 || code > 3);
            }
        }
    }

    #[test]
    fn every_error_says_what_it_is() {
        let errors = [
            TrustListError::Frame(DecodeError::NonZeroPadding),
            TrustListError::ReservedNotZero,
            TrustListError::DevelopmentMark { found: 7 },
            TrustListError::NoKey,
            TrustListError::TooManyKeys { count: 9 },
            TrustListError::UnknownPurpose { entry: 1, code: 4 },
            TrustListError::NoTrustListKey,
            TrustListError::KeyListedTwice {
                first: 0,
                second: 1,
            },
            TrustListError::KeyForTwoPurposes {
                first: 0,
                second: 1,
            },
            TrustListError::NoSignature,
            TrustListError::TooManySignatures { count: 3 },
            TrustListError::SignerNamedTwice,
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(!error.to_decode_error().to_string().is_empty());
        }
    }
}
