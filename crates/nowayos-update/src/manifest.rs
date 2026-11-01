// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update manifest.
//!
//! An update is a whole system image and a manifest the image key signs.
//! The manifest says which release the image is: the product it is for, its
//! version and **security version** - a counter that rises whenever a
//! release fixes something an older one must not come back with - the
//! image's length and SHA-256, the trust-list version it needs, whether it
//! is a development build, and its **reference values**: the measurements
//! a verifier expects, each a SHA-256. Checking the signature against the
//! trust list, the security version against the rollback index and the
//! image against its hash, and deciding whether to install it, are the
//! update PD's: this module decodes, so that nothing about an update is
//! acted on before its bytes are checked.
//!
//! The reference values are kept **opaque**: each program image's and the
//! system description's SHA-256, in an order and with a meaning measured
//! boot will define. The manifest only carries them, so that the verifier
//! learns them before any device runs the release.
//!
//! # Wire layout
//!
//! A `nowayos-ipc` record of kind [`Manifest::KIND`] (2), layout version 1,
//! whose payload is exactly [`Manifest::PAYLOAD_LEN`] (1216) bytes:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 1 | The product name's length, 1 to [`MAX_PRODUCT_LEN`] |
//! | 1 | 1 | The development mark: 1 for a development build, else 0 |
//! | 2 | 6 | The version: major, minor and patch, each a `u16` |
//! | 8 | [`MAX_PRODUCT_LEN`] | The product name, then zeros |
//! | 40 | 4 | The security version |
//! | 44 | 4 | The trust-list version the release needs: a device must hold this one or a later one |
//! | 48 | 8 | The image's length in bytes, at least 1 |
//! | 56 | 32 | The image's SHA-256 |
//! | 88 | 1 | The number of reference values, 0 to [`MAX_REFERENCE_VALUES`] |
//! | 89 | 7 | Reserved, zero |
//! | 96 | 32 × [`MAX_REFERENCE_VALUES`] | The reference values, each a SHA-256 |
//! | 1120 | 96 | The image key's signature: the key's ID ([`KEY_ID_LEN`] bytes), then the signature ([`SIGNATURE_LEN`] bytes) |
//!
//! A product name is lowercase ASCII letters, digits and `-`
//! ([`is_product_character`]). Every byte of the name past its length, and
//! of a reference value past their number, is zero. The signature covers the
//! record's first [`SIGNED_LEN`] bytes: its header and every field before the
//! signature ([`ManifestContents`]).
//!
//! # What the decoder refuses
//!
//! Besides a malformed frame ([`ManifestError::Frame`]), in the order the
//! fields are read: a product name with no character, or more than
//! [`MAX_PRODUCT_LEN`]; a development mark other than 0 or 1; a character
//! of the name outside the allowed ones, or a byte past the name that is not
//! zero (the first of either, in byte order); an image length of zero; more
//! than [`MAX_REFERENCE_VALUES`] reference values; a reserved byte, or a byte
//! past the reference values, that is not zero.

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer};

use crate::{DIGEST_LEN, KEY_ID_LEN, NamedSignature, SIGNATURE_LEN, record};

/// The most characters a product name has.
pub const MAX_PRODUCT_LEN: usize = 32;

/// The most reference values one manifest carries: a system's program
/// images and its system description. The largest designed so far needs
/// 18: a remote-display station's thirteen PDs, the update and storage PDs
/// and a block driver, the entropy PD, and the
/// system description; 32 leaves room. A later layout version may carry
/// more.
pub const MAX_REFERENCE_VALUES: usize = 32;

/// Reserved bytes after the number of reference values.
const REFERENCES_RESERVED: usize = 7;

/// Where each field starts in the signed fields.
const DEVELOPMENT_AT: usize = 1;
const VERSION_AT: usize = 2;
const PRODUCT_AT: usize = 8;
const SECURITY_VERSION_AT: usize = PRODUCT_AT + MAX_PRODUCT_LEN;
const TRUST_LIST_VERSION_AT: usize = SECURITY_VERSION_AT + 4;
const IMAGE_LEN_AT: usize = TRUST_LIST_VERSION_AT + 4;
const IMAGE_SHA256_AT: usize = IMAGE_LEN_AT + 8;
const REFERENCE_COUNT_AT: usize = IMAGE_SHA256_AT + DIGEST_LEN;
const REFERENCES_AT: usize = REFERENCE_COUNT_AT + 1 + REFERENCES_RESERVED;

/// The length of the signed fields: everything before the signature.
const CONTENTS_LEN: usize = REFERENCES_AT + DIGEST_LEN * MAX_REFERENCE_VALUES;

/// The length of the signature: key ID, signature.
const SIGNATURE_BLOCK_LEN: usize = KEY_ID_LEN + SIGNATURE_LEN;

/// The length of the payload: the signed fields, then the signature.
const PAYLOAD_LEN: usize = CONTENTS_LEN + SIGNATURE_BLOCK_LEN;

/// The bytes a manifest's signature covers: the record's header and its
/// signed fields, the record's first 1128 bytes.
pub const SIGNED_LEN: usize = HEADER_LEN + CONTENTS_LEN;

/// The length of a whole manifest record, header included: 1224 bytes.
pub const RECORD_LEN: usize = HEADER_LEN + PAYLOAD_LEN;

const _: () = assert!(
    SECURITY_VERSION_AT == 40
        && REFERENCE_COUNT_AT == 88
        && REFERENCES_AT == 96
        && CONTENTS_LEN == 1120
        && RECORD_LEN == 1224
        && PAYLOAD_LEN <= 0xFFFF
);

/// Whether `byte` may be a character of a product name: a lowercase ASCII
/// letter, a digit or `-`.
#[must_use]
pub const fn is_product_character(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-')
}

/// A release's version, in Semantic Versioning's `MAJOR.MINOR.PATCH`. Ordered as Semantic Versioning orders them; which version a
/// device may take is the security version's to say, not this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Changes for anything incompatible (from 1.0).
    pub major: u16,
    /// Changes for anything that changes behaviour or an interface.
    pub minor: u16,
    /// Changes for fixes only.
    pub patch: u16,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a manifest says, as plain data, for [`ManifestContents::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestFields<'a> {
    /// The product the release is for: 1 to [`MAX_PRODUCT_LEN`] lowercase
    /// ASCII letters, digits and `-`.
    pub product: &'a str,
    /// The release's version.
    pub version: Version,
    /// The release's security version: a device refuses one lower than its
    /// rollback index.
    pub security_version: u32,
    /// The trust-list version the release needs.
    pub trust_list_version: u32,
    /// Whether the image is a development build, which production refuses.
    pub development: bool,
    /// The image's length in bytes, at least 1.
    pub image_len: u64,
    /// The image's SHA-256.
    pub image_sha256: [u8; DIGEST_LEN],
    /// The reference values, at most [`MAX_REFERENCE_VALUES`]: opaque, in
    /// the order measured boot defines.
    pub reference_values: &'a [[u8; DIGEST_LEN]],
}

/// Why a manifest was refused. Decoding refuses with the first problem
/// found, in the order the module documentation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ManifestError {
    /// The record's frame, as `nowayos-ipc` checks it: too short, another
    /// kind, version or payload length, the header's reserved field set, or
    /// bytes after the record.
    Frame(DecodeError),
    /// A reserved byte, or a byte past the product name or the reference
    /// values, is not zero.
    ReservedNotZero,
    /// The product name has no character, or more than
    /// [`MAX_PRODUCT_LEN`].
    ProductNameLength {
        /// The length the manifest gives.
        len: usize,
    },
    /// The development mark is neither 0 nor 1.
    DevelopmentMark {
        /// The mark's byte.
        found: u8,
    },
    /// A character of the product name is not a lowercase ASCII letter, a
    /// digit or `-`.
    ProductNameCharacter {
        /// The character's place in the name, from 0.
        at: usize,
        /// The byte.
        byte: u8,
    },
    /// The image's length is zero.
    EmptyImage,
    /// The manifest carries more than [`MAX_REFERENCE_VALUES`] reference
    /// values.
    TooManyReferenceValues {
        /// The number it gives.
        count: usize,
    },
}

impl ManifestError {
    /// The error `nowayos-ipc` reports for the same input: a frame's error
    /// as it is, a reserved byte as [`DecodeError::NonZeroPadding`], and
    /// every other rule as [`DecodeError::InvalidValue`] naming its field.
    #[must_use]
    pub const fn to_decode_error(self) -> DecodeError {
        match self {
            Self::Frame(error) => error,
            Self::ReservedNotZero => DecodeError::NonZeroPadding,
            Self::ProductNameLength { .. } | Self::ProductNameCharacter { .. } => {
                DecodeError::InvalidValue { field: "product" }
            }
            Self::DevelopmentMark { .. } => DecodeError::InvalidValue {
                field: "development",
            },
            Self::EmptyImage => DecodeError::InvalidValue {
                field: "image length",
            },
            Self::TooManyReferenceValues { .. } => DecodeError::InvalidValue {
                field: "reference value count",
            },
        }
    }
}

impl From<DecodeError> for ManifestError {
    /// A reserved byte that is not zero, as the payload reader reports it,
    /// is [`ManifestError::ReservedNotZero`]; anything else is the frame's.
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NonZeroPadding => Self::ReservedNotZero,
            other => Self::Frame(other),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "update manifest record: {error}"),
            Self::ReservedNotZero => {
                f.write_str("a reserved byte of the update manifest is not zero")
            }
            Self::ProductNameLength { len } => write!(
                f,
                "the update manifest's product name is {len} characters, not 1 to {MAX_PRODUCT_LEN}"
            ),
            Self::DevelopmentMark { found } => write!(
                f,
                "the update manifest's development mark is {found}, not 0 or 1"
            ),
            Self::ProductNameCharacter { at, byte } => write!(
                f,
                "character {at} of the update manifest's product name is {byte:#04x}, \
                 not a lowercase letter, a digit or '-'"
            ),
            Self::EmptyImage => f.write_str("the update manifest's image is empty"),
            Self::TooManyReferenceValues { count } => write!(
                f,
                "the update manifest carries {count} reference values, more than {MAX_REFERENCE_VALUES}"
            ),
        }
    }
}

impl core::error::Error for ManifestError {}

/// What a manifest's signature covers: every field but the signature. It
/// holds the fields' bytes, checked when it was made - built by
/// [`ManifestContents::new`] or decoded - and reads what it says from them,
/// so it keeps the module's rules and encodes as exactly the bytes it came
/// from.
#[derive(Clone, PartialEq, Eq)]
pub struct ManifestContents {
    fields: [u8; CONTENTS_LEN],
}

impl ManifestContents {
    /// The signed part of a manifest saying `fields`.
    ///
    /// # Errors
    ///
    /// The first rule `fields` break, as decoding would refuse it:
    /// [`ManifestError::ProductNameLength`] or
    /// [`ManifestError::TooManyReferenceValues`], checked first, then
    /// [`ManifestError::ProductNameCharacter`] or
    /// [`ManifestError::EmptyImage`].
    pub fn new(fields: &ManifestFields<'_>) -> Result<Self, ManifestError> {
        let name_len = check_product_len(fields.product.len())?;
        let count = check_reference_count(fields.reference_values.len())?;
        let mut name = [0u8; MAX_PRODUCT_LEN];
        for (slot, byte) in name.iter_mut().zip(fields.product.as_bytes()) {
            *slot = *byte;
        }
        let mut references = [0u8; DIGEST_LEN * MAX_REFERENCE_VALUES];
        for (slot, byte) in references
            .iter_mut()
            .zip(fields.reference_values.iter().flatten())
        {
            *slot = *byte;
        }
        let version = fields.version;
        let bytes = [
            u8::try_from(name_len).unwrap_or(u8::MAX),
            u8::from(fields.development),
        ]
        .into_iter()
        .chain(version.major.to_le_bytes())
        .chain(version.minor.to_le_bytes())
        .chain(version.patch.to_le_bytes())
        .chain(name)
        .chain(fields.security_version.to_le_bytes())
        .chain(fields.trust_list_version.to_le_bytes())
        .chain(fields.image_len.to_le_bytes())
        .chain(fields.image_sha256)
        .chain([u8::try_from(count).unwrap_or(u8::MAX)])
        .chain([0; REFERENCES_RESERVED])
        .chain(references);
        let mut out = [0u8; CONTENTS_LEN];
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = byte;
        }
        check_contents(&out)?;
        Ok(Self { fields: out })
    }

    /// The `N` bytes at `at` in the signed fields; zeros past their end,
    /// which no caller here reaches.
    fn array<const N: usize>(&self, at: usize) -> [u8; N] {
        self.fields
            .get(at..)
            .and_then(<[u8]>::first_chunk::<N>)
            .copied()
            .unwrap_or([0; N])
    }

    fn byte(&self, at: usize) -> u8 {
        self.fields.get(at).copied().unwrap_or(0)
    }

    fn u16_at(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.array(at))
    }

    /// The product the release is for: lowercase ASCII letters, digits and
    /// `-`.
    #[must_use]
    pub fn product(&self) -> &str {
        core::str::from_utf8(self.product_bytes()).unwrap_or_default()
    }

    /// The product name's bytes: the same characters as
    /// [`ManifestContents::product`], without a UTF-8 check.
    #[must_use]
    pub fn product_bytes(&self) -> &[u8] {
        let len = usize::from(self.byte(0));
        self.fields
            .get(PRODUCT_AT..)
            .and_then(|rest| rest.get(..len))
            .unwrap_or_default()
    }

    /// The release's version.
    #[must_use]
    pub fn version(&self) -> Version {
        Version {
            major: self.u16_at(VERSION_AT),
            minor: self.u16_at(VERSION_AT.saturating_add(2)),
            patch: self.u16_at(VERSION_AT.saturating_add(4)),
        }
    }

    /// The release's security version: a device refuses one lower than its
    /// rollback index.
    #[must_use]
    pub fn security_version(&self) -> u32 {
        u32::from_le_bytes(self.array(SECURITY_VERSION_AT))
    }

    /// The trust-list version the release needs: a device must hold this
    /// one or a later one.
    #[must_use]
    pub fn trust_list_version(&self) -> u32 {
        u32::from_le_bytes(self.array(TRUST_LIST_VERSION_AT))
    }

    /// Whether the image is a development build, which production refuses.
    #[must_use]
    pub fn is_development(&self) -> bool {
        self.byte(DEVELOPMENT_AT) == 1
    }

    /// The image's length in bytes: at least 1.
    #[must_use]
    pub fn image_len(&self) -> u64 {
        u64::from_le_bytes(self.array(IMAGE_LEN_AT))
    }

    /// The image's SHA-256.
    #[must_use]
    pub fn image_sha256(&self) -> [u8; DIGEST_LEN] {
        self.array(IMAGE_SHA256_AT)
    }

    /// How many reference values the manifest carries: from 0 to
    /// [`MAX_REFERENCE_VALUES`].
    #[must_use]
    pub fn reference_value_count(&self) -> usize {
        usize::from(self.byte(REFERENCE_COUNT_AT))
    }

    /// The reference values, in the order the manifest carries them.
    pub fn reference_values(&self) -> impl Iterator<Item = [u8; DIGEST_LEN]> + '_ {
        self.fields
            .get(REFERENCES_AT..)
            .unwrap_or_default()
            .chunks_exact(DIGEST_LEN)
            .take(self.reference_value_count())
            .filter_map(|value| value.try_into().ok())
    }

    /// The bytes the manifest's signature covers: the record's header, then
    /// these fields, exactly as the record carries them.
    #[must_use]
    pub fn signed_bytes(&self) -> [u8; SIGNED_LEN] {
        let mut out = [0u8; SIGNED_LEN];
        let bytes = record::header_bytes::<Manifest>()
            .into_iter()
            .chain(self.fields);
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = byte;
        }
        out
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, ManifestError> {
        let fields = input.read_array()?;
        check_contents(&fields)?;
        Ok(Self { fields })
    }
}

impl fmt::Debug for ManifestContents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestContents")
            .field("product", &self.product())
            .field("version", &self.version())
            .field("security_version", &self.security_version())
            .field("trust_list_version", &self.trust_list_version())
            .field("development", &self.is_development())
            .field("image_len", &self.image_len())
            .field("reference_values", &self.reference_value_count())
            .finish_non_exhaustive()
    }
}

/// The signed update manifest: its [`ManifestContents`] and the image key's
/// signature, naming its key. Like its contents, it holds its bytes, checked
/// when it was made - by [`Manifest::new`] or by decoding; whether the
/// signature is good, and whether its key may sign images, is for the
/// signature check and the trust list to say.
#[derive(Clone, PartialEq, Eq)]
pub struct Manifest {
    contents: ManifestContents,
    signature: [u8; SIGNATURE_BLOCK_LEN],
}

impl Manifest {
    /// A manifest of `contents`, signed with `signature`.
    #[must_use]
    pub fn new(contents: ManifestContents, signature: &NamedSignature) -> Self {
        let mut block = [0u8; SIGNATURE_BLOCK_LEN];
        let bytes = signature.key_id.into_iter().chain(signature.signature);
        for (slot, byte) in block.iter_mut().zip(bytes) {
            *slot = byte;
        }
        Self {
            contents,
            signature: block,
        }
    }

    /// Decodes `bytes` - copied out of shared memory first - as exactly one
    /// manifest record.
    ///
    /// # Errors
    ///
    /// The first problem found, as the module documentation lists them.
    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        record::decode_exact(bytes, Self::read)
    }

    /// What the signature covers.
    #[must_use]
    pub const fn contents(&self) -> &ManifestContents {
        &self.contents
    }

    /// The product the release is for.
    #[must_use]
    pub fn product(&self) -> &str {
        self.contents.product()
    }

    /// The release's version.
    #[must_use]
    pub fn version(&self) -> Version {
        self.contents.version()
    }

    /// The release's security version.
    #[must_use]
    pub fn security_version(&self) -> u32 {
        self.contents.security_version()
    }

    /// The trust-list version the release needs.
    #[must_use]
    pub fn trust_list_version(&self) -> u32 {
        self.contents.trust_list_version()
    }

    /// Whether the image is a development build.
    #[must_use]
    pub fn is_development(&self) -> bool {
        self.contents.is_development()
    }

    /// The image's length in bytes: at least 1.
    #[must_use]
    pub fn image_len(&self) -> u64 {
        self.contents.image_len()
    }

    /// The image's SHA-256.
    #[must_use]
    pub fn image_sha256(&self) -> [u8; DIGEST_LEN] {
        self.contents.image_sha256()
    }

    /// The reference values, in the order the manifest carries them.
    pub fn reference_values(&self) -> impl Iterator<Item = [u8; DIGEST_LEN]> + '_ {
        self.contents.reference_values()
    }

    /// The image key's signature, naming its key.
    #[must_use]
    pub fn signature(&self) -> NamedSignature {
        let (key_id, signature) = self
            .signature
            .split_first_chunk::<KEY_ID_LEN>()
            .unwrap_or((&[0; KEY_ID_LEN], &[]));
        NamedSignature {
            key_id: *key_id,
            signature: signature.try_into().unwrap_or([0; SIGNATURE_LEN]),
        }
    }

    /// The bytes the signature covers: the record's first [`SIGNED_LEN`]
    /// bytes.
    #[must_use]
    pub fn signed_bytes(&self) -> [u8; SIGNED_LEN] {
        self.contents.signed_bytes()
    }

    /// Reads the signed fields, copied whole into an array of their own
    /// length and checked there, then the signature: two reads that can run
    /// past the input's end, and every field's place fixed.
    fn read(input: &mut Reader<'_>) -> Result<Self, ManifestError> {
        let contents = ManifestContents::read(input)?;
        let signature = input.read_array()?;
        Ok(Self {
            contents,
            signature,
        })
    }
}

impl fmt::Debug for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manifest")
            .field("contents", &self.contents)
            .field("signed_by", &self.signature().key_id)
            .finish_non_exhaustive()
    }
}

impl Message for Manifest {
    const KIND: u16 = 2;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.contents.fields)?;
        out.write_bytes(&self.signature)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(ManifestError::to_decode_error)
    }
}

/// A product name's length, if it is from 1 to `MAX_PRODUCT_LEN`.
fn check_product_len(len: usize) -> Result<usize, ManifestError> {
    if (1..=MAX_PRODUCT_LEN).contains(&len) {
        Ok(len)
    } else {
        Err(ManifestError::ProductNameLength { len })
    }
}

/// A number of reference values, if it is at most `MAX_REFERENCE_VALUES`.
fn check_reference_count(count: usize) -> Result<usize, ManifestError> {
    if count <= MAX_REFERENCE_VALUES {
        Ok(count)
    } else {
        Err(ManifestError::TooManyReferenceValues { count })
    }
}

/// Checks the signed fields' bytes, in the order the module documentation
/// gives. Every loop runs over a whole array, and what is read never depends
/// on the input, which keeps Kani's proofs small.
fn check_contents(fields: &[u8; CONTENTS_LEN]) -> Result<(), ManifestError> {
    let mut input = Reader::new(fields);
    let name_len = check_product_len(usize::from(input.read_u8()?))?;
    match input.read_u8()? {
        0 | 1 => {}
        found => return Err(ManifestError::DevelopmentMark { found }),
    }
    // The version: any three numbers.
    input.read_array::<6>()?;
    let name: [u8; MAX_PRODUCT_LEN] = input.read_array()?;
    for (at, byte) in name.iter().enumerate() {
        if at < name_len {
            if !is_product_character(*byte) {
                return Err(ManifestError::ProductNameCharacter { at, byte: *byte });
            }
        } else if *byte != 0 {
            return Err(ManifestError::ReservedNotZero);
        }
    }
    // The security and trust-list versions: any numbers.
    input.read_array::<8>()?;
    if input.read_u64()? == 0 {
        return Err(ManifestError::EmptyImage);
    }
    input.read_array::<DIGEST_LEN>()?;
    let count = check_reference_count(usize::from(input.read_u8()?))?;
    record::zeros::<REFERENCES_RESERVED>(&mut input)?;
    for entry in 0..MAX_REFERENCE_VALUES {
        let value: [u8; DIGEST_LEN] = input.read_array()?;
        if entry >= count && value != [0; DIGEST_LEN] {
            return Err(ManifestError::ReservedNotZero);
        }
    }
    input.finish()?;
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

    const REFERENCES: [[u8; DIGEST_LEN]; 2] = [[0xD1; DIGEST_LEN], [0xD2; DIGEST_LEN]];

    fn fields() -> ManifestFields<'static> {
        ManifestFields {
            product: "nowayos-test-1",
            version: Version {
                major: 0x0102,
                minor: 0x0304,
                patch: 0x0506,
            },
            security_version: 0x1112_1314,
            trust_list_version: 0x2122_2324,
            development: true,
            image_len: 0x3132_3334_3536_3738,
            image_sha256: [0xA5; DIGEST_LEN],
            reference_values: &REFERENCES,
        }
    }

    fn signature() -> NamedSignature {
        NamedSignature {
            key_id: [0x51; KEY_ID_LEN],
            signature: [0x52; SIGNATURE_LEN],
        }
    }

    fn sample() -> Manifest {
        Manifest::new(ManifestContents::new(&fields()).unwrap(), &signature())
    }

    fn encoded(manifest: &Manifest) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        assert_eq!(encode(manifest, &mut out), Ok(RECORD_LEN));
        out
    }

    #[test]
    fn layout_is_exactly_as_documented() {
        let bytes = encoded(&sample());
        // Header: kind 2, version 1, payload length 1216, reserved.
        assert_eq!(bytes[..8], [2, 0, 1, 0, 0xC0, 0x04, 0, 0]);
        let payload = &bytes[HEADER_LEN..];
        assert_eq!(payload[..8], [14, 1, 0x02, 0x01, 0x04, 0x03, 0x06, 0x05]);
        assert_eq!(&payload[8..22], b"nowayos-test-1");
        assert!(payload[22..40].iter().all(|byte| *byte == 0));
        assert_eq!(payload[40..44], [0x14, 0x13, 0x12, 0x11]);
        assert_eq!(payload[44..48], [0x24, 0x23, 0x22, 0x21]);
        assert_eq!(
            payload[48..56],
            [0x38, 0x37, 0x36, 0x35, 0x34, 0x33, 0x32, 0x31]
        );
        assert_eq!(payload[56..88], [0xA5; 32]);
        assert_eq!(payload[88..96], [2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(payload[96..128], [0xD1; 32]);
        assert_eq!(payload[128..160], [0xD2; 32]);
        assert!(payload[160..1120].iter().all(|byte| *byte == 0));
        assert_eq!(payload[1120..1152], [0x51; 32]);
        assert_eq!(payload[1152..1216], [0x52; 64]);
        assert_eq!(payload.len(), 1216);
    }

    #[test]
    fn round_trip_and_signed_bytes() {
        let manifest = sample();
        let bytes = encoded(&manifest);
        assert_eq!(Manifest::decode(&bytes), Ok(manifest.clone()));
        assert_eq!(decode_exact::<Manifest>(&bytes), Ok(manifest.clone()));
        assert_eq!(manifest.signed_bytes()[..], bytes[..SIGNED_LEN]);
        assert_eq!(record::header_bytes::<Manifest>()[..], bytes[..HEADER_LEN]);
        assert_eq!(manifest.product(), "nowayos-test-1");
        assert_eq!(manifest.version().to_string(), "258.772.1286");
        assert_eq!(manifest.security_version(), 0x1112_1314);
        assert_eq!(manifest.trust_list_version(), 0x2122_2324);
        assert!(manifest.is_development());
        assert_eq!(manifest.image_len(), 0x3132_3334_3536_3738);
        assert_eq!(manifest.image_sha256(), [0xA5; DIGEST_LEN]);
        assert_eq!(manifest.contents().reference_value_count(), 2);
        assert_eq!(manifest.reference_values().collect::<Vec<_>>(), REFERENCES);
        assert_eq!(manifest.signature(), signature());
        assert!(format!("{manifest:?}").contains("nowayos-test-1"));
    }

    #[test]
    fn the_constructor_keeps_the_rules() {
        let long = "a".repeat(MAX_PRODUCT_LEN + 1);
        let full = "z".repeat(MAX_PRODUCT_LEN);
        let cases: [(ManifestFields<'_>, Result<(), ManifestError>); 6] = [
            (
                ManifestFields {
                    product: "",
                    ..fields()
                },
                Err(ManifestError::ProductNameLength { len: 0 }),
            ),
            (
                ManifestFields {
                    product: &long,
                    ..fields()
                },
                Err(ManifestError::ProductNameLength {
                    len: MAX_PRODUCT_LEN + 1,
                }),
            ),
            (
                ManifestFields {
                    product: "Kiosk",
                    ..fields()
                },
                Err(ManifestError::ProductNameCharacter { at: 0, byte: b'K' }),
            ),
            (
                ManifestFields {
                    image_len: 0,
                    ..fields()
                },
                Err(ManifestError::EmptyImage),
            ),
            (
                ManifestFields {
                    reference_values: &[[1; DIGEST_LEN]; MAX_REFERENCE_VALUES + 1],
                    ..fields()
                },
                Err(ManifestError::TooManyReferenceValues {
                    count: MAX_REFERENCE_VALUES + 1,
                }),
            ),
            (
                ManifestFields {
                    product: &full,
                    reference_values: &[[1; DIGEST_LEN]; MAX_REFERENCE_VALUES],
                    ..fields()
                },
                Ok(()),
            ),
        ];
        for (fields, expected) in cases {
            assert_eq!(ManifestContents::new(&fields).map(|_| ()), expected);
        }
    }

    #[test]
    fn errors_map_to_what_nowayos_ipc_reports() {
        let bytes = encoded(&sample());
        let mut bad = bytes;
        bad[HEADER_LEN + DEVELOPMENT_AT] = 2;
        assert_eq!(
            Manifest::decode(&bad),
            Err(ManifestError::DevelopmentMark { found: 2 })
        );
        assert_eq!(
            decode_exact::<Manifest>(&bad),
            Err(DecodeError::InvalidValue {
                field: "development"
            })
        );
        let mut bad = bytes;
        bad[HEADER_LEN + REFERENCE_COUNT_AT + 1] = 1;
        assert_eq!(Manifest::decode(&bad), Err(ManifestError::ReservedNotZero));
        assert_eq!(
            decode_exact::<Manifest>(&bad),
            Err(DecodeError::NonZeroPadding)
        );
        let mut bad = bytes;
        bad[6] = 1;
        assert_eq!(
            Manifest::decode(&bad),
            Err(ManifestError::Frame(DecodeError::ReservedNotZero))
        );
    }

    #[test]
    fn product_characters_are_exactly_the_documented_ones() {
        let allowed: Vec<u8> = (0..=u8::MAX)
            .filter(|byte| is_product_character(*byte))
            .collect();
        let expected: Vec<u8> = (b'a'..=b'z').chain(b'0'..=b'9').chain([b'-']).collect();
        assert_eq!(allowed.len(), expected.len());
        assert!(expected.iter().all(|byte| allowed.contains(byte)));
    }

    #[test]
    fn every_error_says_what_it_is() {
        let errors = [
            ManifestError::Frame(DecodeError::NonZeroPadding),
            ManifestError::ReservedNotZero,
            ManifestError::ProductNameLength { len: 33 },
            ManifestError::DevelopmentMark { found: 7 },
            ManifestError::ProductNameCharacter { at: 3, byte: b'_' },
            ManifestError::EmptyImage,
            ManifestError::TooManyReferenceValues { count: 33 },
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(!error.to_decode_error().to_string().is_empty());
        }
    }
}
