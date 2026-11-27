// SPDX-License-Identifier: MIT OR Apache-2.0
//! The deployment record: an Update Server's signed answer to a check-in.
//!
//! The image key signs **what** may run; a deployment key, held online by
//! the Update Server, signs **when and on which device** a release runs. A
//! device whose trust list names a deployment key installs a manifest only
//! when a deployment record signed by that key names it, so no single key
//! or server installs anything. Each record echoes the nonce of the
//! check-in it answers ([`crate::checkin`]), so a device knows the answer is
//! fresh - made after it asked - with no trusted clock; "current", that there
//! is nothing to install, is signed the same way. Which records to act on,
//! and checking the signature, are the update PD's: this module decodes, so
//! that nothing about a record is used before its bytes are checked.
//!
//! # Wire layout
//!
//! A `nowayos-ipc` record of kind [`Deployment::KIND`] (6), layout version 1,
//! whose payload is exactly [`Deployment::PAYLOAD_LEN`] (232) bytes:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 1 | The product name's length, 1 to [`MAX_PRODUCT_LEN`] |
//! | 1 | 1 | The board name's length, 1 to [`MAX_BOARD_LEN`] |
//! | 2 | 1 | The instruction: 1 current, 2 install |
//! | 3 | 1 | Reserved, zero |
//! | 4 | 4 | The cohort the device is in, for the server's own records |
//! | 8 | 32 | The product name, then zeros |
//! | 40 | 32 | The board name, then zeros |
//! | 72 | 32 | The nonce of the check-in it answers |
//! | 104 | 32 | To install: the SHA-256 of the manifest's signed bytes; zeros for current |
//! | 136 | 96 | The deployment key's signature: the key's ID ([`KEY_ID_LEN`] bytes), then the signature ([`SIGNATURE_LEN`] bytes) |
//!
//! Names are as the manifest's ([`crate::manifest`]). The signature covers
//! the record's first [`SIGNED_LEN`] bytes: its header and every field before
//! the signature ([`DeploymentContents`]).
//!
//! # What the decoder refuses
//!
//! Besides a malformed frame ([`DeploymentError::Frame`]), in the order the
//! fields are read: a product name with no character or too many; a board
//! name with no character or too many; an instruction code that names no
//! instruction; a reserved byte that is not zero; a character of either
//! name outside the allowed ones, or a byte past it that is not zero; a
//! "current" with a manifest hash that is not zeros.

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer};

use crate::checkin::NONCE_LEN;
use crate::manifest::{MAX_BOARD_LEN, MAX_PRODUCT_LEN};
use crate::names::{self, NAME_LEN, Name, NameFault};
use crate::{DIGEST_LEN, KEY_ID_LEN, NamedSignature, SIGNATURE_LEN, record};

/// Where each field starts in the signed fields.
const INSTRUCTION_AT: usize = 2;
const COHORT_AT: usize = 4;
const PRODUCT_AT: usize = 8;
const BOARD_AT: usize = PRODUCT_AT + NAME_LEN;
const NONCE_AT: usize = BOARD_AT + NAME_LEN;
const MANIFEST_AT: usize = NONCE_AT + NONCE_LEN;

/// The length of the signed fields: everything before the signature.
const CONTENTS_LEN: usize = MANIFEST_AT + DIGEST_LEN;

/// The length of the signature: key ID, signature.
const SIGNATURE_BLOCK_LEN: usize = KEY_ID_LEN + SIGNATURE_LEN;

/// The length of the payload: the signed fields, then the signature.
const PAYLOAD_LEN: usize = CONTENTS_LEN + SIGNATURE_BLOCK_LEN;

/// The bytes a deployment record's signature covers: its header and its
/// signed fields, the record's first 144 bytes.
pub const SIGNED_LEN: usize = HEADER_LEN + CONTENTS_LEN;

const _: () = assert!(CONTENTS_LEN == 136 && PAYLOAD_LEN == 232);

/// What a deployment record tells the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Instruction {
    /// Nothing to install: the device runs the release it should.
    Current,
    /// Install the release whose manifest's signed bytes have this SHA-256.
    Install([u8; DIGEST_LEN]),
}

impl Instruction {
    const fn code(self) -> u8 {
        match self {
            Self::Current => 1,
            Self::Install(_) => 2,
        }
    }

    const fn hash(self) -> [u8; DIGEST_LEN] {
        match self {
            Self::Current => [0; DIGEST_LEN],
            Self::Install(hash) => hash,
        }
    }
}

/// What a deployment record says, as plain data, for
/// [`DeploymentContents::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeploymentFields<'a> {
    /// The product of the device it answers.
    pub product: &'a str,
    /// The board of the device it answers.
    pub board: &'a str,
    /// The nonce of the check-in it answers.
    pub nonce: [u8; NONCE_LEN],
    /// The cohort the device is in.
    pub cohort: u32,
    /// What the device is to do.
    pub instruction: Instruction,
}

/// Why a deployment record was refused. Decoding refuses with the first
/// problem found, in the order the module documentation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeploymentError {
    /// The record's frame, as `nowayos-ipc` checks it.
    Frame(DecodeError),
    /// A reserved byte, a byte past a name, or a "current" record's
    /// manifest hash, is not zero.
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
    /// The instruction code names no instruction.
    UnknownInstruction {
        /// The code.
        code: u8,
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

impl DeploymentError {
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
            Self::UnknownInstruction { .. } => DecodeError::InvalidValue {
                field: "instruction",
            },
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

impl From<DecodeError> for DeploymentError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NonZeroPadding => Self::ReservedNotZero,
            other => Self::Frame(other),
        }
    }
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "deployment record: {error}"),
            Self::ReservedNotZero => {
                f.write_str("a reserved byte of the deployment record is not zero")
            }
            Self::ProductNameLength { len } => write!(
                f,
                "the deployment record's product name is {len} characters, not 1 to {MAX_PRODUCT_LEN}"
            ),
            Self::BoardNameLength { len } => write!(
                f,
                "the deployment record's board name is {len} characters, not 1 to {MAX_BOARD_LEN}"
            ),
            Self::UnknownInstruction { code } => {
                write!(
                    f,
                    "the deployment record's instruction is {code}, not 1 or 2"
                )
            }
            Self::ProductNameCharacter { at, byte } => write!(
                f,
                "character {at} of the deployment record's product name is {byte:#04x}, not allowed"
            ),
            Self::BoardNameCharacter { at, byte } => write!(
                f,
                "character {at} of the deployment record's board name is {byte:#04x}, not allowed"
            ),
        }
    }
}

impl core::error::Error for DeploymentError {}

/// What a deployment record's signature covers: every field but the
/// signature. It holds the fields' bytes, checked when it was made, and
/// reads what it says from them.
#[derive(Clone, PartialEq, Eq)]
pub struct DeploymentContents {
    fields: [u8; CONTENTS_LEN],
}

impl DeploymentContents {
    /// The signed part of a deployment record saying `fields`.
    ///
    /// # Errors
    ///
    /// The first rule a name breaks, as decoding would refuse it.
    pub fn new(fields: &DeploymentFields<'_>) -> Result<Self, DeploymentError> {
        let (product_len, product) = Name::Product
            .field(fields.product)
            .map_err(DeploymentError::product)?;
        let (board_len, board) = Name::Board
            .field(fields.board)
            .map_err(DeploymentError::board)?;
        let bytes = [product_len, board_len, fields.instruction.code(), 0]
            .into_iter()
            .chain(fields.cohort.to_le_bytes())
            .chain(product)
            .chain(board)
            .chain(fields.nonce)
            .chain(fields.instruction.hash());
        let mut out = [0u8; CONTENTS_LEN];
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = byte;
        }
        check_contents(&out)?;
        Ok(Self { fields: out })
    }

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

    /// The product of the device it answers.
    #[must_use]
    pub fn product(&self) -> &str {
        names::as_str(
            self.fields.get(PRODUCT_AT..).unwrap_or_default(),
            self.byte(0),
        )
    }

    /// The board of the device it answers.
    #[must_use]
    pub fn board(&self) -> &str {
        names::as_str(
            self.fields.get(BOARD_AT..).unwrap_or_default(),
            self.byte(1),
        )
    }

    /// The nonce of the check-in it answers.
    #[must_use]
    pub fn nonce(&self) -> [u8; NONCE_LEN] {
        self.array(NONCE_AT)
    }

    /// The cohort the device is in.
    #[must_use]
    pub fn cohort(&self) -> u32 {
        u32::from_le_bytes(self.array(COHORT_AT))
    }

    /// What the device is to do.
    #[must_use]
    pub fn instruction(&self) -> Instruction {
        if self.byte(INSTRUCTION_AT) == Instruction::Current.code() {
            Instruction::Current
        } else {
            Instruction::Install(self.array(MANIFEST_AT))
        }
    }

    /// The bytes the signature covers: the record's header, then these
    /// fields, exactly as the record carries them.
    #[must_use]
    pub fn signed_bytes(&self) -> [u8; SIGNED_LEN] {
        let mut out = [0u8; SIGNED_LEN];
        let bytes = record::header_bytes::<Deployment>()
            .into_iter()
            .chain(self.fields);
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = byte;
        }
        out
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, DeploymentError> {
        let fields = input.read_array()?;
        check_contents(&fields)?;
        Ok(Self { fields })
    }
}

impl fmt::Debug for DeploymentContents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeploymentContents")
            .field("product", &self.product())
            .field("board", &self.board())
            .field("cohort", &self.cohort())
            .field("instruction", &self.instruction())
            .finish_non_exhaustive()
    }
}

/// The signed deployment record: its [`DeploymentContents`] and the
/// deployment key's signature, naming its key. Whether the signature is
/// good, and whether its key may sign deployment records, is for the
/// signature check and the trust list to say.
#[derive(Clone, PartialEq, Eq)]
pub struct Deployment {
    contents: DeploymentContents,
    signature: [u8; SIGNATURE_BLOCK_LEN],
}

impl Deployment {
    /// A deployment record of `contents`, signed with `signature`.
    #[must_use]
    pub fn new(contents: DeploymentContents, signature: &NamedSignature) -> Self {
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
    /// deployment record.
    ///
    /// # Errors
    ///
    /// The first problem found, as the module documentation lists them.
    pub fn decode(bytes: &[u8]) -> Result<Self, DeploymentError> {
        record::decode_exact(bytes, Self::read)
    }

    /// What the signature covers.
    #[must_use]
    pub const fn contents(&self) -> &DeploymentContents {
        &self.contents
    }

    /// The product of the device it answers.
    #[must_use]
    pub fn product(&self) -> &str {
        self.contents.product()
    }

    /// The board of the device it answers.
    #[must_use]
    pub fn board(&self) -> &str {
        self.contents.board()
    }

    /// The nonce of the check-in it answers.
    #[must_use]
    pub fn nonce(&self) -> [u8; NONCE_LEN] {
        self.contents.nonce()
    }

    /// The cohort the device is in.
    #[must_use]
    pub fn cohort(&self) -> u32 {
        self.contents.cohort()
    }

    /// What the device is to do.
    #[must_use]
    pub fn instruction(&self) -> Instruction {
        self.contents.instruction()
    }

    /// The deployment key's signature, naming its key.
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

    fn read(input: &mut Reader<'_>) -> Result<Self, DeploymentError> {
        let contents = DeploymentContents::read(input)?;
        let signature = input.read_array()?;
        Ok(Self {
            contents,
            signature,
        })
    }
}

impl fmt::Debug for Deployment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deployment")
            .field("contents", &self.contents)
            .field("signed_by", &self.signature().key_id)
            .finish_non_exhaustive()
    }
}

impl Message for Deployment {
    const KIND: u16 = 6;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.contents.fields)?;
        out.write_bytes(&self.signature)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(DeploymentError::to_decode_error)
    }
}

/// Checks the signed fields' bytes in the order the module documentation
/// gives; every read is of a fixed size, which keeps Kani's proofs small.
fn check_contents(fields: &[u8; CONTENTS_LEN]) -> Result<(), DeploymentError> {
    let mut input = Reader::new(fields);
    let product_len = Name::Product
        .check_len(usize::from(input.read_u8()?))
        .map_err(DeploymentError::product)?;
    let board_len = Name::Board
        .check_len(usize::from(input.read_u8()?))
        .map_err(DeploymentError::board)?;
    let code = input.read_u8()?;
    if !matches!(code, 1 | 2) {
        return Err(DeploymentError::UnknownInstruction { code });
    }
    record::zeros::<1>(&mut input)?;
    // The cohort: any number.
    input.read_array::<4>()?;
    let product: [u8; NAME_LEN] = input.read_array()?;
    Name::Product
        .check_field(product_len, &product)
        .map_err(DeploymentError::product)?;
    let board: [u8; NAME_LEN] = input.read_array()?;
    Name::Board
        .check_field(board_len, &board)
        .map_err(DeploymentError::board)?;
    input.read_array::<NONCE_LEN>()?;
    let hash: [u8; DIGEST_LEN] = input.read_array()?;
    if code == 1 && hash != [0; DIGEST_LEN] {
        return Err(DeploymentError::ReservedNotZero);
    }
    input.finish()?;
    Ok(())
}
