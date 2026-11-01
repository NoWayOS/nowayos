// SPDX-License-Identifier: MIT OR Apache-2.0
//! Restarting one protection domain cleanly, without restarting the system.
//!
//! Microkit's own restart (`microkit_pd_restart`) only moves a PD's program
//! counter: its `.data` and `.bss` stay as the fault left them, and
//! rust-sel4's runtime, whose once-only set-up lives there, panics before
//! the PD's `init` runs again. A clean restart needs the
//! memory the program was loaded with. This crate puts it back:
//!
//! * **the restart image** ([`image`]): what the PD's writable segments held
//!   when its program was loaded, made at build time from its program image
//!   by `tools/restart-image` and mapped read-only into the PD (and its
//!   parent). The words the Microkit tool patches in as it builds the
//!   system image are listed as kept ranges;
//! * **the restart entry** (feature `entry`): a restartable PD's program
//!   starts at `nowayos_restart_entry`, a few instructions of assembly that
//!   call into this crate before rust-sel4's own `_start`. At a cold start it
//!   checks that the PD's writable memory is exactly the image - so the image
//!   is known to be this program's, and no patched word is missing from it -
//!   and at a restart it puts the image back and zeros `.bss`. Either way
//!   `_start` then runs on memory as the program was loaded: the runtime's
//!   once-only set-up (its panic hooks, the IPC buffer, thread-local
//!   storage) runs afresh, then the PD's `init`. [`started`] says which it
//!   was;
//! * **the parent's side** (feature `parent`): [`restart`] starts a child
//!   that a fault stopped again at its restart entry, with the stack pointer
//!   it first started with and [`RESTART`] in its first register. seL4 drops
//!   the child's unanswered fault message, so the parent sends no reply.
//!
//! A restarted PD's peers learn of it from its new restart epoch on each
//! ring it shares (`nowayos_ipc`'s ring documentation, "Restart epochs").
//!
//! What a restart does not reset: the PD's shared memory regions (its rings
//! resume from their indices), its stack (the runtime builds its thread-local
//! storage there afresh, and no code reads stack it has not written),
//! notifications pending for it, and the devices it drives. A driver that
//! is restarted must put its device back in a known state from `init`.
//!
//! Policy: restarting one PD on its own needs a decision the fault policy
//! has not taken yet. Until then only test systems do it, and
//! `tools/check-system` (rule 13) refuses a restart image in a product
//! system.

#![cfg_attr(not(test), no_std)]

pub mod image;

// The only module allowed `unsafe` code; everything else in the crate is
// safe. tools/check-unsafe lists exactly this file and lib.rs.
#[cfg(all(feature = "entry", target_os = "none"))]
#[allow(
    unsafe_code,
    reason = "the restart entry's assembly, the patched symbols of the restart image, and views of the PD's own writable segments, before the runtime starts"
)]
mod entry;

#[cfg(feature = "parent")]
mod parent;

#[cfg(all(feature = "entry", target_os = "none"))]
pub use entry::started;
pub use image::{Header, Image, ImageError};
#[cfg(feature = "parent")]
pub use parent::{RestartError, restart};

/// The value a parent puts in a child's first register (`x0`) when it
/// restarts it; Microkit starts every register at zero. The restart entry
/// accepts nothing else.
pub const RESTART: u64 = u64::from_be_bytes(*b"RESTART!");

/// How this start of the PD began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Start {
    /// The program did not start at the restart entry (its build.rs does not
    /// set `--entry=nowayos_restart_entry`).
    NotThroughEntry = 0,
    /// A cold start of a PD with no restart image mapped: it cannot be
    /// restarted.
    Cold = 1,
    /// A cold start, with the PD's writable memory checked against its
    /// restart image.
    ColdChecked = 2,
    /// A restart: the image put back, `.bss` zeroed.
    Restarted = 3,
}

impl Start {
    /// The start a stored value names.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Cold,
            2 => Self::ColdChecked,
            3 => Self::Restarted,
            _ => Self::NotThroughEntry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_survives_its_round_trip_through_a_byte() {
        for start in [
            Start::NotThroughEntry,
            Start::Cold,
            Start::ColdChecked,
            Start::Restarted,
        ] {
            assert_eq!(Start::from_u8(start as u8), start);
        }
        assert_eq!(Start::from_u8(200), Start::NotThroughEntry);
        assert_eq!(RESTART, 0x5245_5354_4152_5421);
    }
}
