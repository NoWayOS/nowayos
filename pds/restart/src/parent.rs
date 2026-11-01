// SPDX-License-Identifier: MIT OR Apache-2.0
//! A parent's side of a restart: starting a child that a fault stopped again
//! at its restart entry (see the crate documentation).
//!
//! Written from the seL4 16.0.0 reference manual (`seL4_TCB_ReadRegisters`,
//! `seL4_TCB_WriteRegisters` with `resume`, which restarts a thread blocked
//! on its fault reply and drops that reply) and rust-sel4 v5.0.0's `sel4`
//! and `sel4-microkit` crates.

use core::fmt;

use sel4::UserContext;
use sel4_microkit::Child;

use crate::RESTART;
use crate::image::Header;

/// Why a child could not be restarted.
#[derive(Debug)]
pub enum RestartError {
    /// seL4 refused to read or write the child's registers.
    Kernel(sel4::Error),
}

impl fmt::Display for RestartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(error) => write!(f, "seL4 refused the child's registers: {error:?}"),
        }
    }
}

/// Every register in seL4's user context on this architecture: a restart
/// writes them all.
const REGISTERS: sel4::Word =
    match size_of::<sel4::sys::seL4_UserContext>().checked_div(size_of::<sel4::Word>()) {
        Some(count) => count as sel4::Word,
        None => 0,
    };

/// Restarts `child`, which a fault stopped, at its restart entry, from
/// `image`, the header of its restart image: every register zero except
/// the program counter (the restart entry), the stack pointer (the one the
/// child first started with), its processor state (kept as it was) and
/// `x0` ([`RESTART`]).
///
/// seL4 drops the child's unanswered fault message, so the parent must not
/// reply to it: return `Ok(None)` from `Handler::fault`.
///
/// # Errors
///
/// [`RestartError::Kernel`] if seL4 refuses to read or write the child's
/// registers; the child stays stopped.
pub fn restart(child: Child, image: &Header) -> Result<(), RestartError> {
    let tcb = child.tcb();
    let current = tcb
        .tcb_read_registers(false, REGISTERS)
        .map_err(RestartError::Kernel)?;
    let mut fresh = UserContext::default();
    *fresh.pc_mut() = image.entry;
    *fresh.sp_mut() = image.stack_top;
    *fresh.spsr_mut() = *current.spsr();
    *fresh.gpr_mut(0) = RESTART;
    tcb.tcb_write_registers(true, REGISTERS, &mut fresh)
        .map_err(RestartError::Kernel)
}
