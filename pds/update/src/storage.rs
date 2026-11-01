// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update PD's requests to the storage PD, whose channel reaches only
//! the two slots and the update state. A protected call on the update PD's [`STORAGE`] channel,
//! with the bytes in the one page the two PDs share:
//!
//! | Label | Request | Message registers | The page |
//! | --- | --- | --- | --- |
//! | 1 | Read a copy of the update state | the copy (0 A, 1 B) | the storage PD writes the copy's [`COPY_LEN`] bytes |
//! | 2 | Write a copy of the update state | the copy | the update PD has put the copy's bytes |
//! | 3 | Write image bytes to a slot | the slot (0 A, 1 B), the offset, the length | the update PD has put the bytes |
//! | 4 | Say how the device booted | none | unused |
//!
//! The reply's label is [`OK`], or [`REFUSED`] for a request the storage PD
//! will not do: an unknown label or copy or slot, or bytes past a slot's
//! end or the page's. Bytes are packed little-endian into the page's 32-bit
//! words. A boot request's reply carries two message registers: the slot
//! the bootloader booted (0 A, 1 B) - which the storage PD learns from the
//! bootloader's record, as the Pi's firmware leaves it beside the slots - and
//! a slot's length in bytes ([`BootFacts`]).
//!
//! Neither PD trusts what the other put in the page: the update PD
//! decodes a copy it reads like any input, and the storage PD checks every
//! request's bounds.
//!
//! [`STORAGE`]: crate::STORAGE

use nowayos_update::state::{COPY_LEN, Slot, StateCopy};

/// The shared page's length: the longest request, a copy of the state,
/// fits.
pub const WINDOW_LEN: usize = 4096;

const _: () = assert!(COPY_LEN <= WINDOW_LEN);

/// The reply to a request done.
pub const OK: u64 = 0;

/// The reply to a request refused.
pub const REFUSED: u64 = 1;

const READ_STATE: u64 = 1;
const WRITE_STATE: u64 = 2;
const WRITE_SLOT: u64 = 3;
const BOOT: u64 = 4;

/// A request to the storage PD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Read this copy of the update state into the page.
    ReadState(StateCopy),
    /// Write the page's copy of the update state over this copy.
    WriteState(StateCopy),
    /// Write the page's first `len` bytes at `offset` in `slot`.
    WriteSlot {
        /// The slot.
        slot: Slot,
        /// Where in it.
        offset: u64,
        /// How many bytes.
        len: u32,
    },
    /// Say which slot booted and how long a slot is ([`BootFacts`]).
    Boot,
}

/// How the device booted, as the storage PD says: the reply to
/// [`Request::Boot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootFacts {
    /// The slot the bootloader booted.
    pub slot: Slot,
    /// A slot's length in bytes.
    pub slot_len: u64,
}

impl BootFacts {
    /// The reply's two message registers.
    #[must_use]
    pub const fn words(&self) -> [u64; 2] {
        [slot_code(self.slot), self.slot_len]
    }

    /// The facts a reply carries, if they are any: a known slot and a
    /// length that is not 0.
    #[must_use]
    pub const fn received(words: [u64; 2]) -> Option<Self> {
        let [slot, slot_len] = words;
        match (slot_of(slot), slot_len) {
            (Some(slot), 1..) => Some(Self { slot, slot_len }),
            _ => None,
        }
    }
}

impl Request {
    /// The call's label.
    #[must_use]
    pub const fn label(&self) -> u64 {
        match self {
            Self::ReadState(_) => READ_STATE,
            Self::WriteState(_) => WRITE_STATE,
            Self::WriteSlot { .. } => WRITE_SLOT,
            Self::Boot => BOOT,
        }
    }

    /// The call's message registers.
    #[must_use]
    pub fn words(&self) -> [u64; 3] {
        match *self {
            Self::ReadState(copy) | Self::WriteState(copy) => [copy_code(copy), 0, 0],
            Self::WriteSlot { slot, offset, len } => [slot_code(slot), offset, u64::from(len)],
            Self::Boot => [0; 3],
        }
    }

    /// The request a call carries, if it is one.
    #[must_use]
    pub fn received(label: u64, words: [u64; 3]) -> Option<Self> {
        let [which, offset, len] = words;
        match label {
            READ_STATE => copy_of(which).map(Self::ReadState),
            WRITE_STATE => copy_of(which).map(Self::WriteState),
            WRITE_SLOT => Some(Self::WriteSlot {
                slot: slot_of(which)?,
                offset,
                len: u32::try_from(len).ok()?,
            }),
            BOOT => Some(Self::Boot),
            _ => None,
        }
    }
}

const fn copy_code(copy: StateCopy) -> u64 {
    match copy {
        StateCopy::A => 0,
        StateCopy::B => 1,
    }
}

const fn slot_code(slot: Slot) -> u64 {
    match slot {
        Slot::A => 0,
        Slot::B => 1,
    }
}

const fn copy_of(code: u64) -> Option<StateCopy> {
    match code {
        0 => Some(StateCopy::A),
        1 => Some(StateCopy::B),
        _ => None,
    }
}

const fn slot_of(code: u64) -> Option<Slot> {
    match code {
        0 => Some(Slot::A),
        1 => Some(Slot::B),
        _ => None,
    }
}
