// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update check-in: what a device tells its Update Server.
//!
//! A device asks its server what to install by sending a **check-in**: what
//! it is (product and board), what it runs (the version, the security
//! version and the hash of the running release's manifest), the trust list
//! it holds, how its last update ended, and a fresh **nonce** from its
//! random source. The server's answer, a deployment record
//! ([`crate::deployment`]), echoes the nonce, so the device knows the answer
//! was made for this check-in and not replayed. The server reads a check-in
//! from a device it does not trust: this module decodes, so that nothing
//! about one is used before its bytes are checked.
//!
//! A check-in is not signed: whatever it claims, the server can only answer
//! with releases their own signatures authorise.
//!
//! # Wire layout
//!
//! A `nowayos-ipc` record of kind [`CheckIn::KIND`] (5), layout version 1,
//! whose payload is exactly [`CheckIn::PAYLOAD_LEN`] (184) bytes:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 1 | The product name's length, 1 to [`MAX_PRODUCT_LEN`] |
//! | 1 | 1 | The board name's length, 1 to [`MAX_BOARD_LEN`] |
//! | 2 | 6 | The running release's version: major, minor and patch, each a `u16` |
//! | 8 | 32 | The product name, then zeros |
//! | 40 | 32 | The board name, then zeros |
//! | 72 | 4 | The running release's security version |
//! | 76 | 4 | The version of the trust list held |
//! | 80 | 2 | How the last update ended: 0 for nothing to report, else a [`crate::updater::Refusal`] code |
//! | 82 | 6 | Reserved, zero |
//! | 88 | 32 | The SHA-256 of the running release's manifest's signed bytes |
//! | 120 | 32 | The nonce |
//! | 152 | 32 | The device's ID |
//!
//! Names are as the manifest's ([`crate::manifest`]). Integers are
//! little-endian.
//!
//! # What the decoder refuses
//!
//! Besides a malformed frame ([`CheckInError::Frame`]), in the order the
//! fields are read: a product name with no character or too many; a board
//! name with no character or too many; a character of the product name
//! outside the allowed ones, or a byte past it that is not zero; the same
//! for the board name; a reserved byte that is not zero.

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

use crate::DIGEST_LEN;
use crate::manifest::{MAX_BOARD_LEN, MAX_PRODUCT_LEN, Version};
use crate::names::{self, NAME_LEN, Name, NameFault};
use crate::record;

/// The length of a nonce.
pub const NONCE_LEN: usize = 32;

/// The length of a device's ID.
pub const DEVICE_ID_LEN: usize = 32;

/// Where each field starts in the payload.
const VERSION_AT: usize = 2;
const PRODUCT_AT: usize = 8;
const BOARD_AT: usize = PRODUCT_AT + NAME_LEN;
const SECURITY_VERSION_AT: usize = BOARD_AT + NAME_LEN;
const TRUST_LIST_VERSION_AT: usize = SECURITY_VERSION_AT + 4;
const LAST_RESULT_AT: usize = TRUST_LIST_VERSION_AT + 4;
const RESERVED: usize = 6;
const MANIFEST_AT: usize = LAST_RESULT_AT + 2 + RESERVED;
const NONCE_AT: usize = MANIFEST_AT + DIGEST_LEN;
const DEVICE_AT: usize = NONCE_AT + NONCE_LEN;

/// The length of the payload.
const PAYLOAD_LEN: usize = DEVICE_AT + DEVICE_ID_LEN;

const _: () = assert!(MANIFEST_AT == 88 && PAYLOAD_LEN == 184);

/// What a check-in says, as plain data, for [`CheckIn::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckInFields<'a> {
    /// The product the device runs.
    pub product: &'a str,
    /// The board it is.
    pub board: &'a str,
    /// The running release's version.
    pub version: Version,
    /// The running release's security version.
    pub security_version: u32,
    /// The version of the trust list held.
    pub trust_list_version: u32,
    /// How the last update ended: 0 for nothing to report, else a
    /// [`crate::updater::Refusal`] code.
    pub last_result: u16,
    /// The SHA-256 of the running release's manifest's signed bytes.
    pub running_manifest: [u8; DIGEST_LEN],
    /// A fresh nonce, from the device's random source.
    pub nonce: [u8; NONCE_LEN],
    /// The device's ID.
    pub device: [u8; DEVICE_ID_LEN],
}

/// Why a check-in was refused. Decoding refuses with the first problem
/// found, in the order the module documentation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckInError {
    /// The record's frame, as `nowayos-ipc` checks it.
    Frame(DecodeError),
    /// A reserved byte, or a byte past a name, is not zero.
    ReservedNotZero,
    /// The product name has no character, or more than
    /// [`MAX_PRODUCT_LEN`].
    ProductNameLength {
        /// The length the record gives.
        len: usize,
    },
    /// The board name has no character, or more than [`MAX_BOARD_LEN`].
    BoardNameLength {
        /// The length the record gives.
        len: usize,
    },
    /// A character of the product name is not allowed.
    ProductNameCharacter {
        /// Its place in the name, from 0.
        at: usize,
        /// The byte.
        byte: u8,
    },
    /// A character of the board name is not allowed.
    BoardNameCharacter {
        /// Its place in the name, from 0.
        at: usize,
        /// The byte.
        byte: u8,
    },
}

impl CheckInError {
    /// The error `nowayos-ipc` reports for the same input.
    #[must_use]
    pub const fn to_decode_error(self) -> DecodeError {
        match self {
            Self::Frame(error) => error,
            Self::ReservedNotZero => DecodeError::NonZeroPadding,
            Self::ProductNameLength { .. } | Self::ProductNameCharacter { .. } => {
                DecodeError::InvalidValue { field: "product" }
            }
            Self::BoardNameLength { .. } | Self::BoardNameCharacter { .. } => {
                DecodeError::InvalidValue { field: "board" }
            }
        }
    }

    const fn product(fault: NameFault) -> Self {
        match fault {
            NameFault::Length(len) => Self::ProductNameLength { len },
            NameFault::Character { at, byte } => Self::ProductNameCharacter { at, byte },
            NameFault::NotZero => Self::ReservedNotZero,
        }
    }

    const fn board(fault: NameFault) -> Self {
        match fault {
            NameFault::Length(len) => Self::BoardNameLength { len },
            NameFault::Character { at, byte } => Self::BoardNameCharacter { at, byte },
            NameFault::NotZero => Self::ReservedNotZero,
        }
    }
}

impl From<DecodeError> for CheckInError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NonZeroPadding => Self::ReservedNotZero,
            other => Self::Frame(other),
        }
    }
}

impl fmt::Display for CheckInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "check-in record: {error}"),
            Self::ReservedNotZero => f.write_str("a reserved byte of the check-in is not zero"),
            Self::ProductNameLength { len } => write!(
                f,
                "the check-in's product name is {len} characters, not 1 to {MAX_PRODUCT_LEN}"
            ),
            Self::BoardNameLength { len } => write!(
                f,
                "the check-in's board name is {len} characters, not 1 to {MAX_BOARD_LEN}"
            ),
            Self::ProductNameCharacter { at, byte } => write!(
                f,
                "character {at} of the check-in's product name is {byte:#04x}, not allowed"
            ),
            Self::BoardNameCharacter { at, byte } => write!(
                f,
                "character {at} of the check-in's board name is {byte:#04x}, not allowed"
            ),
        }
    }
}

impl core::error::Error for CheckInError {}

/// A check-in. It holds its payload's bytes, checked when it was made - by
/// [`CheckIn::new`] or by decoding - and reads what it says from them, so
/// it encodes as exactly the bytes it came from.
#[derive(Clone, PartialEq, Eq)]
pub struct CheckIn {
    payload: [u8; PAYLOAD_LEN],
}

impl CheckIn {
    /// A check-in saying `fields`.
    ///
    /// # Errors
    ///
    /// The first rule a name breaks, as decoding would refuse it.
    pub fn new(fields: &CheckInFields<'_>) -> Result<Self, CheckInError> {
        let (product_len, product) = Name::Product
            .field(fields.product)
            .map_err(CheckInError::product)?;
        let (board_len, board) = Name::Board
            .field(fields.board)
            .map_err(CheckInError::board)?;
        let version = fields.version;
        let bytes = [product_len, board_len]
            .into_iter()
            .chain(version.major.to_le_bytes())
            .chain(version.minor.to_le_bytes())
            .chain(version.patch.to_le_bytes())
            .chain(product)
            .chain(board)
            .chain(fields.security_version.to_le_bytes())
            .chain(fields.trust_list_version.to_le_bytes())
            .chain(fields.last_result.to_le_bytes())
            .chain([0; RESERVED])
            .chain(fields.running_manifest)
            .chain(fields.nonce)
            .chain(fields.device);
        let mut payload = [0u8; PAYLOAD_LEN];
        for (slot, byte) in payload.iter_mut().zip(bytes) {
            *slot = byte;
        }
        check_payload(&payload)?;
        Ok(Self { payload })
    }

    /// Decodes `bytes` as exactly one check-in record.
    ///
    /// # Errors
    ///
    /// The first problem found, as the module documentation lists them.
    pub fn decode(bytes: &[u8]) -> Result<Self, CheckInError> {
        record::decode_exact(bytes, Self::read)
    }

    fn array<const N: usize>(&self, at: usize) -> [u8; N] {
        self.payload
            .get(at..)
            .and_then(<[u8]>::first_chunk::<N>)
            .copied()
            .unwrap_or([0; N])
    }

    fn byte(&self, at: usize) -> u8 {
        self.payload.get(at).copied().unwrap_or(0)
    }

    /// The product the device runs.
    #[must_use]
    pub fn product(&self) -> &str {
        names::as_str(
            self.payload.get(PRODUCT_AT..).unwrap_or_default(),
            self.byte(0),
        )
    }

    /// The board it is.
    #[must_use]
    pub fn board(&self) -> &str {
        names::as_str(
            self.payload.get(BOARD_AT..).unwrap_or_default(),
            self.byte(1),
        )
    }

    /// The running release's version.
    #[must_use]
    pub fn version(&self) -> Version {
        Version {
            major: u16::from_le_bytes(self.array(VERSION_AT)),
            minor: u16::from_le_bytes(self.array(VERSION_AT.saturating_add(2))),
            patch: u16::from_le_bytes(self.array(VERSION_AT.saturating_add(4))),
        }
    }

    /// The running release's security version.
    #[must_use]
    pub fn security_version(&self) -> u32 {
        u32::from_le_bytes(self.array(SECURITY_VERSION_AT))
    }

    /// The version of the trust list held.
    #[must_use]
    pub fn trust_list_version(&self) -> u32 {
        u32::from_le_bytes(self.array(TRUST_LIST_VERSION_AT))
    }

    /// How the last update ended: 0 for nothing to report, else a
    /// [`crate::updater::Refusal`] code.
    #[must_use]
    pub fn last_result(&self) -> u16 {
        u16::from_le_bytes(self.array(LAST_RESULT_AT))
    }

    /// The SHA-256 of the running release's manifest's signed bytes.
    #[must_use]
    pub fn running_manifest(&self) -> [u8; DIGEST_LEN] {
        self.array(MANIFEST_AT)
    }

    /// The nonce, which the answer must echo.
    #[must_use]
    pub fn nonce(&self) -> [u8; NONCE_LEN] {
        self.array(NONCE_AT)
    }

    /// The device's ID.
    #[must_use]
    pub fn device(&self) -> [u8; DEVICE_ID_LEN] {
        self.array(DEVICE_AT)
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, CheckInError> {
        let payload = input.read_array()?;
        check_payload(&payload)?;
        Ok(Self { payload })
    }
}

impl fmt::Debug for CheckIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckIn")
            .field("product", &self.product())
            .field("board", &self.board())
            .field("version", &self.version())
            .field("security_version", &self.security_version())
            .field("trust_list_version", &self.trust_list_version())
            .field("last_result", &self.last_result())
            .finish_non_exhaustive()
    }
}

impl Message for CheckIn {
    const KIND: u16 = 5;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.payload)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(CheckInError::to_decode_error)
    }
}

/// Checks the payload's bytes in the order the module documentation gives;
/// every read is of a fixed size, which keeps Kani's proofs small.
fn check_payload(payload: &[u8; PAYLOAD_LEN]) -> Result<(), CheckInError> {
    let mut input = Reader::new(payload);
    let product_len = Name::Product
        .check_len(usize::from(input.read_u8()?))
        .map_err(CheckInError::product)?;
    let board_len = Name::Board
        .check_len(usize::from(input.read_u8()?))
        .map_err(CheckInError::board)?;
    input.read_array::<6>()?;
    let product: [u8; NAME_LEN] = input.read_array()?;
    Name::Product
        .check_field(product_len, &product)
        .map_err(CheckInError::product)?;
    let board: [u8; NAME_LEN] = input.read_array()?;
    Name::Board
        .check_field(board_len, &board)
        .map_err(CheckInError::board)?;
    // Security version, trust-list version, last result: any numbers.
    input.read_array::<10>()?;
    record::zeros::<RESERVED>(&mut input)?;
    input.read_array::<{ DIGEST_LEN + NONCE_LEN + DEVICE_ID_LEN }>()?;
    input.finish()?;
    Ok(())
}
