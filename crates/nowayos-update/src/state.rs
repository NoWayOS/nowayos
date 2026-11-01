// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update state: which slot boots, whether a trial is pending,
//! the rollback index and the trust list held - kept so that a reset or a
//! power cut in the middle of a write leaves the old state or the new one,
//! never a mix, and no write lowers the rollback index or the trust list's
//! version.
//!
//! # Two copies
//!
//! The state is kept through [`StateStorage`], a small interface a storage
//! PD can serve later: two areas, [`StateCopy::A`] and
//! [`StateCopy::B`], each [`COPY_LEN`] bytes. Each copy holds, in order:
//!
//! | Offset | Length | Field |
//! | --- | --- | --- |
//! | 0 | 8 | The generation, `u64`, never 0: one more at each write |
//! | 8 | [`STATE_RECORD_LEN`] | The [`StateRecord`]: slots, rollback index, whether a trust list is held |
//! | 64 | [`trust_list::RECORD_LEN`] | The held trust list's record, or zeros if none is held |
//! | 64 + that | 4 | A CRC-32 (IEEE) of everything before it |
//!
//! [`StateStore::read`] takes the copy with the higher generation of those
//! whose CRC checks and that decode. [`StateStore::write`] writes the next
//! generation over the *other* copy, so the copy that counts is never the
//! one being written: a write cut short anywhere leaves a copy whose CRC
//! does not check, and the old state is read. **The CRC finds a torn write;
//! it is no security check.** Storage is untrusted when read back, so each
//! copy is decoded like any input, and a reading error is passed on, never
//! taken as a copy lost - lest an older copy count and a write lower the
//! index.
//!
//! # Never lower, never a default
//!
//! [`StateStore::write`] refuses a state whose rollback index, or held trust
//! list's version, is lower than the state read: no caller can lower either.
//! With neither copy readable, [`StateStore::read`] reports
//! [`StoreError::Lost`] and puts no default in its place, and every write
//! is refused: what a device does then is open (the update PD starts no update and
//! reports it). The first state is written whole, both copies, by whatever
//! provisions the device ([`encode_copy`]).

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer, encode};

use crate::trust_list::{self, TrustList, TrustListError};
use crate::{KEY_ID_LEN, record};

/// The state record's kind on the update PD's records (1 the trust list, 2
/// the manifest, 3 a chunk).
pub const STATE_KIND: u16 = 4;

/// The state record's layout version.
pub const STATE_VERSION: u16 = 1;

/// The state record's payload length.
const PAYLOAD_LEN: usize = 16 + KEY_ID_LEN;

/// The state record's length, header included.
pub const STATE_RECORD_LEN: usize = HEADER_LEN + PAYLOAD_LEN;

/// Where the state record starts in a copy.
const RECORD_AT: usize = 8;
/// Where the held trust list starts in a copy.
const LIST_AT: usize = RECORD_AT + STATE_RECORD_LEN;
/// Where the CRC starts in a copy.
const CRC_AT: usize = LIST_AT + trust_list::RECORD_LEN;

/// The length of one copy of the state.
pub const COPY_LEN: usize = CRC_AT + 4;

/// A boot slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The first slot.
    A,
    /// The second slot.
    B,
}

impl Slot {
    /// The other slot.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::A => 1,
            Self::B => 2,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::A),
            2 => Some(Self::B),
            _ => None,
        }
    }
}

/// A slot marked for one trial boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Trial {
    /// The slot to boot once, on trial: never the slot that boots.
    pub slot: Slot,
    /// Its manifest's security version, which becomes the rollback index
    /// once the trial passes its health check: never below the index.
    pub security_version: u32,
    /// The ID of the image key that signed its manifest: a trust list that
    /// removes the key unmarks the trial.
    pub image_key: [u8; KEY_ID_LEN],
}

/// The fixed part of the update state, as its own record: what the
/// bootloader - here, the QEMU harness - reads.
///
/// Wire layout, a `nowayos-ipc` record of kind [`STATE_KIND`], 48 bytes of
/// payload: the slot that boots (`u8`: `1` A, `2` B); the trial slot (`u8`:
/// `0` none, else as the first, and never the same); whether a trust list
/// is held (`u8`: `0` no, `1` yes); a reserved zero byte; the rollback index
/// (`u32`); the trial's security version (`u32`: `0` with no trial, else at
/// least the index); four reserved zero bytes; the trial's image key ID
/// ([`KEY_ID_LEN`] bytes, zeros with no trial).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateRecord {
    /// The slot that boots: the last marked good.
    pub boots: Slot,
    /// The slot marked for one trial boot, if any.
    pub trial: Option<Trial>,
    /// The lowest security version the device accepts.
    pub rollback_index: u32,
    /// Whether a trust list is held beside the record.
    pub holds_trust_list: bool,
}

/// Why a state record, or a copy of the state, was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// The record's frame or a reserved byte: what `nowayos-ipc` would
    /// report.
    Frame(DecodeError),
    /// A slot's code is not one.
    UnknownSlot {
        /// The code read.
        code: u8,
    },
    /// The trial slot is the slot that boots.
    TrialIsBooting,
    /// The trial's security version is below the rollback index, or a
    /// security version is given with no trial.
    TrialVersion,
    /// An image key ID is given with no trial.
    TrialKey,
    /// Whether a trust list is held is neither `0` nor `1`.
    UnknownHeldMark {
        /// The code read.
        code: u8,
    },
    /// The copy's CRC does not check: a torn write, or damage.
    Crc,
    /// The copy's generation is 0.
    ZeroGeneration,
    /// The held trust list does not decode.
    TrustList(TrustListError),
    /// No trust list is held, and its bytes are not all zero.
    NotZero,
}

impl From<DecodeError> for StateError {
    fn from(error: DecodeError) -> Self {
        Self::Frame(error)
    }
}

impl core::fmt::Display for StateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "malformed state record: {error}"),
            Self::UnknownSlot { code } => write!(f, "unknown slot code {code}"),
            Self::TrialIsBooting => f.write_str("the trial slot is the slot that boots"),
            Self::TrialVersion => f.write_str("a trial's security version below the index"),
            Self::TrialKey => f.write_str("an image key with no trial"),
            Self::UnknownHeldMark { code } => write!(f, "unknown held-list mark {code}"),
            Self::Crc => f.write_str("the copy's CRC does not check"),
            Self::ZeroGeneration => f.write_str("a generation of 0"),
            Self::TrustList(error) => write!(f, "the held trust list: {error:?}"),
            Self::NotZero => f.write_str("no trust list held, but its bytes are not zero"),
        }
    }
}

impl core::error::Error for StateError {}

impl StateRecord {
    /// Decodes `bytes` - copied out of storage first - as exactly one state
    /// record.
    ///
    /// # Errors
    ///
    /// The first problem found: the frame, then each field in order.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        record::decode_exact(bytes, Self::read)
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, StateError> {
        let boots = input.read_u8()?;
        let trial = input.read_u8()?;
        let held = input.read_u8()?;
        record::zeros::<1>(input)?;
        let rollback_index = input.read_u32()?;
        let security_version = input.read_u32()?;
        record::zeros::<4>(input)?;
        let image_key: [u8; KEY_ID_LEN] = input.read_array()?;
        let boots = Slot::from_code(boots).ok_or(StateError::UnknownSlot { code: boots })?;
        let trial = match trial {
            0 if security_version != 0 => return Err(StateError::TrialVersion),
            0 if image_key != [0; KEY_ID_LEN] => return Err(StateError::TrialKey),
            0 => None,
            code => {
                let slot = Slot::from_code(code).ok_or(StateError::UnknownSlot { code })?;
                Some(Trial {
                    slot,
                    security_version,
                    image_key,
                })
            }
        };
        let holds_trust_list = match held {
            0 => false,
            1 => true,
            code => return Err(StateError::UnknownHeldMark { code }),
        };
        let record = Self {
            boots,
            trial,
            rollback_index,
            holds_trust_list,
        };
        record.check()?;
        Ok(record)
    }

    /// The rules a record keeps beyond its codes.
    const fn check(&self) -> Result<(), StateError> {
        match self.trial {
            Some(trial) if trial.slot.code() == self.boots.code() => {
                Err(StateError::TrialIsBooting)
            }
            Some(trial) if trial.security_version < self.rollback_index => {
                Err(StateError::TrialVersion)
            }
            _ => Ok(()),
        }
    }
}

impl Message for StateRecord {
    const KIND: u16 = STATE_KIND;
    const VERSION: u16 = STATE_VERSION;
    const PAYLOAD_LEN: usize = PAYLOAD_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        if self.check().is_err() {
            return Err(EncodeError::InvalidValue { field: "trial" });
        }
        let (trial, security_version, image_key) = match self.trial {
            Some(trial) => (trial.slot.code(), trial.security_version, trial.image_key),
            None => (0, 0, [0; KEY_ID_LEN]),
        };
        out.write_u8(self.boots.code())?;
        out.write_u8(trial)?;
        out.write_u8(u8::from(self.holds_trust_list))?;
        out.write_zeros(1)?;
        out.write_u32(self.rollback_index)?;
        out.write_u32(security_version)?;
        out.write_zeros(4)?;
        out.write_bytes(&image_key)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Self::read(input).map_err(|error| match error {
            StateError::Frame(error) => error,
            StateError::UnknownSlot { .. } | StateError::TrialIsBooting => {
                DecodeError::InvalidValue { field: "slot" }
            }
            StateError::UnknownHeldMark { .. } => DecodeError::InvalidValue { field: "held" },
            StateError::TrialKey => DecodeError::InvalidValue { field: "image_key" },
            _ => DecodeError::InvalidValue {
                field: "security_version",
            },
        })
    }
}

/// The whole update state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateState {
    /// The slot that boots: the last marked good.
    pub boots: Slot,
    /// The slot marked for one trial boot, if any.
    pub trial: Option<Trial>,
    /// The lowest security version the device accepts.
    pub rollback_index: u32,
    /// The trust list the device took last, if any; otherwise the trust
    /// anchors in the running image are all it has.
    pub trust_list: Option<TrustList>,
}

impl UpdateState {
    /// The held trust list's version: 0 if none is held.
    #[must_use]
    pub fn trust_list_version(&self) -> u32 {
        self.trust_list.as_ref().map_or(0, TrustList::version)
    }

    const fn record(&self) -> StateRecord {
        StateRecord {
            boots: self.boots,
            trial: self.trial,
            rollback_index: self.rollback_index,
            holds_trust_list: self.trust_list.is_some(),
        }
    }
}

/// `state` as one copy, with `generation`: what [`StateStore::write`]
/// writes, and what provisions a device's first state, in both copies.
///
/// # Errors
///
/// [`StoreError::Invalid`] for a generation of 0 or a state that breaks a
/// record's rules (a trial in the slot that boots, or below the index).
pub fn encode_copy(generation: u64, state: &UpdateState) -> Result<[u8; COPY_LEN], StoreError> {
    if generation == 0 {
        return Err(StoreError::Invalid(StateError::ZeroGeneration));
    }
    let record = state.record();
    record.check().map_err(StoreError::Invalid)?;
    let mut copy = [0u8; COPY_LEN];
    let (head, rest) = copy.split_at_mut(RECORD_AT);
    head.copy_from_slice(&generation.to_le_bytes());
    let (record_bytes, rest) = rest.split_at_mut(STATE_RECORD_LEN);
    let (list_bytes, _) = rest.split_at_mut(trust_list::RECORD_LEN);
    let invalid = |_| {
        StoreError::Invalid(StateError::Frame(DecodeError::InvalidValue {
            field: "state",
        }))
    };
    encode(&record, record_bytes).map_err(invalid)?;
    if let Some(list) = &state.trust_list {
        encode(list, list_bytes).map_err(invalid)?;
    }
    let (body, crc) = copy.split_at_mut(CRC_AT);
    crc.copy_from_slice(&crc32(body).to_le_bytes());
    Ok(copy)
}

/// Decodes one copy of the state, as read back from storage: its CRC, its
/// generation, its record and the trust list it holds.
///
/// # Errors
///
/// The first problem found, as [`StateError`] lists them.
pub fn decode_copy(copy: &[u8; COPY_LEN]) -> Result<(u64, UpdateState), StateError> {
    let (body, crc) = copy.split_at(CRC_AT);
    let crc: [u8; 4] = crc.try_into().map_err(|_| StateError::Crc)?;
    if crc32(body) != u32::from_le_bytes(crc) {
        return Err(StateError::Crc);
    }
    let (generation, rest) = body.split_at(RECORD_AT);
    let generation: [u8; 8] = generation.try_into().map_err(|_| StateError::Crc)?;
    let generation = u64::from_le_bytes(generation);
    if generation == 0 {
        return Err(StateError::ZeroGeneration);
    }
    let (record, list) = rest.split_at(STATE_RECORD_LEN);
    let record = StateRecord::decode(record)?;
    let trust_list = if record.holds_trust_list {
        Some(TrustList::decode(list).map_err(StateError::TrustList)?)
    } else if list.iter().all(|&byte| byte == 0) {
        None
    } else {
        return Err(StateError::NotZero);
    };
    Ok((
        generation,
        UpdateState {
            boots: record.boots,
            trial: record.trial,
            rollback_index: record.rollback_index,
            trust_list,
        },
    ))
}

/// The CRC-32 of IEEE 802.3 (reflected, polynomial `0xEDB88320`), bit by
/// bit: finds a torn write, and nothing more.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// One of the state's two copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateCopy {
    /// The first.
    A,
    /// The second.
    B,
}

impl StateCopy {
    /// The other copy.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Storage refused a read or a write, or a write was cut short. It says no
/// more: a storage PD reports its own faults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageError;

/// Where the state is kept: two areas of [`COPY_LEN`] bytes, which a
/// storage PD serves, or a test's memory.
pub trait StateStorage {
    /// Reads copy `copy` whole into `out`.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if it could not be read.
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError>;

    /// Writes `bytes` over copy `copy`. A reset or power cut may stop it
    /// after any number of bytes.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if it was not written whole.
    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError>;
}

/// Which of a state's rules a write would break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lowered {
    /// The rollback index would fall.
    RollbackIndex {
        /// The index read.
        held: u32,
        /// The index offered.
        offered: u32,
    },
    /// The held trust list's version would fall, or the list be dropped.
    TrustList {
        /// The version read.
        held: u32,
        /// The version offered.
        offered: u32,
    },
}

/// Why the store could not read or write the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// Neither copy checks and decodes: the state is lost. Nothing is put
    /// in its place, and no write is made.
    Lost,
    /// Storage failed.
    Storage(StorageError),
    /// The state offered would lower the rollback index or the trust list.
    Lower(Lowered),
    /// The state offered breaks a record's rules.
    Invalid(StateError),
    /// The generation can go no higher.
    Exhausted,
}

/// The update state over its storage; see the [module](self).
#[derive(Debug)]
pub struct StateStore<S> {
    storage: S,
}

/// The copy that counts.
struct Current {
    copy: StateCopy,
    generation: u64,
    state: UpdateState,
}

impl<S: StateStorage> StateStore<S> {
    /// The store over `storage`.
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    /// The storage, back.
    pub fn into_inner(self) -> S {
        self.storage
    }

    /// The state: the newest copy that checks and decodes.
    ///
    /// # Errors
    ///
    /// [`StoreError::Lost`] if neither does; [`StoreError::Storage`] if
    /// either could not be read.
    pub fn read(&mut self) -> Result<UpdateState, StoreError> {
        self.current().map(|current| current.state)
    }

    /// Writes `state` as the next generation, over the copy that does not
    /// count, so a write cut short leaves the state as it was.
    ///
    /// # Errors
    ///
    /// [`StoreError::Lower`] if it would lower the rollback index or the
    /// held trust list's version; [`StoreError::Lost`] with no state to
    /// compare it with; [`StoreError::Invalid`], [`StoreError::Exhausted`]
    /// or [`StoreError::Storage`]. Refused, nothing is written.
    pub fn write(&mut self, state: &UpdateState) -> Result<(), StoreError> {
        let current = self.current()?;
        let held = &current.state;
        if state.rollback_index < held.rollback_index {
            return Err(StoreError::Lower(Lowered::RollbackIndex {
                held: held.rollback_index,
                offered: state.rollback_index,
            }));
        }
        let (held_list, offered_list) = (held.trust_list_version(), state.trust_list_version());
        if offered_list < held_list || (held.trust_list.is_some() && state.trust_list.is_none()) {
            return Err(StoreError::Lower(Lowered::TrustList {
                held: held_list,
                offered: offered_list,
            }));
        }
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(StoreError::Exhausted)?;
        let bytes = encode_copy(generation, state)?;
        self.storage
            .write(current.copy.other(), &bytes)
            .map_err(StoreError::Storage)
    }

    fn current(&mut self) -> Result<Current, StoreError> {
        let mut bytes = [0u8; COPY_LEN];
        let mut best: Option<Current> = None;
        for copy in [StateCopy::A, StateCopy::B] {
            self.storage
                .read(copy, &mut bytes)
                .map_err(StoreError::Storage)?;
            let Ok((generation, state)) = decode_copy(&bytes) else {
                continue;
            };
            // Equal generations, which no write makes: the first copy.
            if best
                .as_ref()
                .is_none_or(|best| generation > best.generation)
            {
                best = Some(Current {
                    copy,
                    generation,
                    state,
                });
            }
        }
        best.ok_or(StoreError::Lost)
    }
}
