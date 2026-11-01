// SPDX-License-Identifier: MIT OR Apache-2.0
//! Decoding the virtual-memory fault reports seL4 sends to a fault handler.
//!
//! When a protection domain (PD) touches memory its capabilities do not
//! grant — an address nothing is mapped at, or a write to a read-only
//! mapping — the MMU raises an abort, seL4 stops the PD and sends its fault
//! handler a *VM fault* message. The handler is the PD's parent, if the
//! system description gives it one, or else the Microkit monitor. The message carries the faulting instruction's
//! address (IP), the data address, whether the fault was an instruction
//! fetch, and a fault status register (FSR) value.
//!
//! On AArch64 the FSR is the abort's Exception Syndrome Register value,
//! whose layout is defined by the Arm Architecture Reference Manual:
//!
//! | Bits | Field | Meaning here |
//! | --- | --- | --- |
//! | 31:26 | EC, exception class | `0x24`/`0x25` data abort, `0x20`/`0x21` instruction abort |
//! | 6 | WnR (data aborts) | 1 if the access was a write |
//! | 5:0 | DFSC / IFSC, fault status code | the kind of fault and the translation level it was found at |
//!
//! [`VmFault`] turns a report into a stable description — [`Access`] and
//! [`Cause`], with [`VmFault::summary`] for a log line that does not change
//! from build to build — and keeps the raw values for
//! [`VmFault::detail`]. It does not depend on seL4, so it is tested on the
//! host; a fault-handling PD copies the four values out of the `sel4` crate's
//! fault type.
//!
//! Provisional: for the isolation test. The fault policy
//! products use is `nowayos-fault-policy`'s, run by `pds/supervisor`.

#![cfg_attr(not(test), no_std)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use core::fmt;

/// Exception class of a data abort taken from a lower exception level (a
/// PD at EL0).
const EC_DATA_ABORT_LOWER: u8 = 0x24;
/// Exception class of a data abort taken at the same exception level.
const EC_DATA_ABORT_SAME: u8 = 0x25;
/// Exception class of an instruction abort taken from a lower exception
/// level.
const EC_INSTRUCTION_ABORT_LOWER: u8 = 0x20;
/// Exception class of an instruction abort taken at the same exception
/// level.
const EC_INSTRUCTION_ABORT_SAME: u8 = 0x21;

/// Extracts `width` bits of `value` starting at bit `shift`, as a byte.
/// Callers use a `width` of at most 8 and a `shift` below 64.
const fn bits(value: u64, shift: u32, width: u32) -> u8 {
    let mask = match 1u64.checked_shl(width) {
        Some(bit) => bit.wrapping_sub(1),
        None => u64::MAX,
    };
    let shifted = match value.checked_shr(shift) {
        Some(shifted) => shifted,
        None => 0,
    };
    (shifted & mask) as u8
}

/// What the faulting access was trying to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A load.
    Read,
    /// A store.
    Write,
    /// Fetching an instruction to execute.
    InstructionFetch,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "data read",
            Self::Write => "data write",
            Self::InstructionFetch => "instruction fetch",
        })
    }
}

/// Why the MMU refused the access: the fault status code, decoded.
///
/// `level` is the translation-table level at which the fault was found. It
/// depends on the page-table layout (for example on whether seL4 runs as a
/// hypervisor), so [`Cause::kind`] leaves it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The address is wider than the translation regime allows.
    AddressSize {
        /// Translation level (0 to 3).
        level: u8,
    },
    /// Nothing is mapped at the address.
    Translation {
        /// Translation level (0 to 3).
        level: u8,
    },
    /// The mapping's access flag is clear.
    AccessFlag {
        /// Translation level (0 to 3).
        level: u8,
    },
    /// Something is mapped, but its permissions do not allow this access
    /// (for example a write to a read-only mapping).
    Permission {
        /// Translation level (0 to 3).
        level: u8,
    },
    /// The memory system reported an external abort.
    SynchronousExternalAbort,
    /// The access was misaligned.
    Alignment,
    /// Any other fault status code.
    Other(u8),
}

impl Cause {
    /// Decodes a 6-bit fault status code (DFSC or IFSC).
    #[must_use]
    pub const fn from_status_code(code: u8) -> Self {
        let level = code & 0b11;
        match code & 0x3f {
            0x00..=0x03 => Self::AddressSize { level },
            0x04..=0x07 => Self::Translation { level },
            0x08..=0x0b => Self::AccessFlag { level },
            0x0c..=0x0f => Self::Permission { level },
            0x10 => Self::SynchronousExternalAbort,
            0x21 => Self::Alignment,
            other => Self::Other(other),
        }
    }

    /// The cause without its translation level: stable across page-table
    /// layouts, for log lines a test checks.
    #[must_use]
    pub const fn kind(&self) -> CauseKind {
        CauseKind(*self)
    }
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressSize { level }
            | Self::Translation { level }
            | Self::AccessFlag { level }
            | Self::Permission { level } => write!(f, "{}, level {level}", self.kind()),
            _ => write!(f, "{}", self.kind()),
        }
    }
}

/// [`Cause`] displayed without its translation level. See [`Cause::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CauseKind(Cause);

impl fmt::Display for CauseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Cause::AddressSize { .. } => f.write_str("address size fault"),
            Cause::Translation { .. } => f.write_str("translation fault (nothing mapped there)"),
            Cause::AccessFlag { .. } => f.write_str("access flag fault"),
            Cause::Permission { .. } => {
                f.write_str("permission fault (the mapping does not allow it)")
            }
            Cause::SynchronousExternalAbort => f.write_str("synchronous external abort"),
            Cause::Alignment => f.write_str("alignment fault"),
            Cause::Other(code) => write!(f, "fault status code {code:#04x}"),
        }
    }
}

/// One VM fault report, as seL4 delivers it to the faulting PD's handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmFault {
    ip: u64,
    addr: u64,
    fsr: u64,
    instruction_fetch: bool,
}

impl VmFault {
    /// A report with the faulting instruction's address `ip`, the faulting
    /// address `addr`, the fault status register value `fsr`, and whether the
    /// fault was an instruction fetch (seL4's "prefetch fault" flag).
    #[must_use]
    pub const fn new(ip: u64, addr: u64, fsr: u64, instruction_fetch: bool) -> Self {
        Self {
            ip,
            addr,
            fsr,
            instruction_fetch,
        }
    }

    /// The faulting instruction's address.
    #[must_use]
    pub const fn ip(&self) -> u64 {
        self.ip
    }

    /// The address the access was made to.
    #[must_use]
    pub const fn addr(&self) -> u64 {
        self.addr
    }

    /// The raw fault status register value.
    #[must_use]
    pub const fn fsr(&self) -> u64 {
        self.fsr
    }

    /// The exception class (FSR bits 31:26).
    #[must_use]
    pub const fn exception_class(&self) -> u8 {
        bits(self.fsr, 26, 6)
    }

    /// The fault status code (FSR bits 5:0).
    #[must_use]
    pub const fn status_code(&self) -> u8 {
        bits(self.fsr, 0, 6)
    }

    /// Whether the exception class is a data abort.
    #[must_use]
    pub const fn is_data_abort(&self) -> bool {
        matches!(
            self.exception_class(),
            EC_DATA_ABORT_LOWER | EC_DATA_ABORT_SAME
        )
    }

    /// What the access was trying to do.
    #[must_use]
    pub const fn access(&self) -> Access {
        let instruction_abort = matches!(
            self.exception_class(),
            EC_INSTRUCTION_ABORT_LOWER | EC_INSTRUCTION_ABORT_SAME
        );
        if self.instruction_fetch || instruction_abort {
            Access::InstructionFetch
        } else if self.is_data_abort() && bits(self.fsr, 6, 1) == 1 {
            Access::Write
        } else {
            Access::Read
        }
    }

    /// Why the access was refused.
    #[must_use]
    pub const fn cause(&self) -> Cause {
        Cause::from_status_code(self.status_code())
    }

    /// A one-line description that depends only on what was accessed and
    /// why it failed — not on code addresses or page-table layout — so a test
    /// can check it: `data read at 0x40000000: translation fault (nothing
    /// mapped there)`.
    #[must_use]
    pub const fn summary(&self) -> Summary<'_> {
        Summary(self)
    }

    /// The raw values and full decoding, for a second log line:
    /// `ip 0x..., fsr 0x... (exception class 0x24, fault status 0x06:
    /// translation fault ..., level 2)`.
    #[must_use]
    pub const fn detail(&self) -> Detail<'_> {
        Detail(self)
    }
}

/// See [`VmFault::summary`].
#[derive(Debug, Clone, Copy)]
pub struct Summary<'a>(&'a VmFault);

impl fmt::Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fault = self.0;
        write!(
            f,
            "{} at {:#x}: {}",
            fault.access(),
            fault.addr,
            fault.cause().kind()
        )
    }
}

/// See [`VmFault::detail`].
#[derive(Debug, Clone, Copy)]
pub struct Detail<'a>(&'a VmFault);

impl fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fault = self.0;
        write!(
            f,
            "ip {:#x}, fsr {:#x} (exception class {:#04x}, fault status {:#04x}: {})",
            fault.ip,
            fault.fsr,
            fault.exception_class(),
            fault.status_code(),
            fault.cause()
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::*;

    /// An FSR for a data abort from EL0 with the given fault status code.
    fn data_abort(code: u8, write: bool) -> u64 {
        (u64::from(EC_DATA_ABORT_LOWER) << 26)
            | (1 << 25)
            | (u64::from(write) << 6)
            | u64::from(code)
    }

    #[test]
    fn an_unmapped_read_is_a_translation_fault() {
        let fault = VmFault::new(0x20_1234, 0x4000_0000, data_abort(0x06, false), false);
        assert_eq!(fault.exception_class(), 0x24);
        assert!(fault.is_data_abort());
        assert_eq!(fault.access(), Access::Read);
        assert_eq!(fault.cause(), Cause::Translation { level: 2 });
        assert_eq!(
            fault.summary().to_string(),
            "data read at 0x40000000: translation fault (nothing mapped there)"
        );
        assert_eq!(
            fault.detail().to_string(),
            "ip 0x201234, fsr 0x92000006 (exception class 0x24, fault status 0x06: \
             translation fault (nothing mapped there), level 2)"
        );
    }

    #[test]
    fn a_write_to_a_read_only_page_is_a_permission_fault() {
        let fault = VmFault::new(0x20_2000, 0x5000_0000, data_abort(0x0f, true), false);
        assert_eq!(fault.access(), Access::Write);
        assert_eq!(fault.cause(), Cause::Permission { level: 3 });
        assert_eq!(
            fault.summary().to_string(),
            "data write at 0x50000000: permission fault (the mapping does not allow it)"
        );
    }

    #[test]
    fn instruction_fetches_are_recognised_either_way() {
        // Instruction abort from EL0, translation fault level 3.
        let by_class = VmFault::new(0x9000, 0x9000, (0x20 << 26) | 0x07, false);
        assert_eq!(by_class.access(), Access::InstructionFetch);
        // seL4's prefetch flag alone is enough.
        let by_flag = VmFault::new(0x9000, 0x9000, data_abort(0x07, true), true);
        assert_eq!(by_flag.access(), Access::InstructionFetch);
    }

    #[test]
    fn the_level_is_left_out_of_the_summary() {
        for level in 0..4 {
            let fault = VmFault::new(0, 0x1000, data_abort(0x04 | level, false), false);
            assert_eq!(
                fault.summary().to_string(),
                "data read at 0x1000: translation fault (nothing mapped there)"
            );
            assert_eq!(fault.cause(), Cause::Translation { level });
        }
    }

    #[test]
    fn every_status_code_decodes_without_panicking() {
        for code in 0..=0x3f_u8 {
            for write in [false, true] {
                let fault = VmFault::new(u64::MAX, u64::MAX, data_abort(code, write), false);
                assert_eq!(fault.status_code(), code);
                assert_eq!(
                    fault.access(),
                    if write { Access::Write } else { Access::Read }
                );
                assert!(!fault.summary().to_string().is_empty());
                assert!(!fault.detail().to_string().is_empty());
            }
        }
        let named = [0x00, 0x04, 0x08, 0x0c, 0x10, 0x21];
        for code in 0..=0x3f_u8 {
            let other = matches!(Cause::from_status_code(code), Cause::Other(_));
            let known = code <= 0x10 || code == 0x21;
            assert_eq!(other, !known, "code {code:#04x}");
        }
        assert!(
            named
                .iter()
                .all(|&code| !matches!(Cause::from_status_code(code), Cause::Other(_)))
        );
    }

    #[test]
    fn wild_values_are_reported_not_trusted() {
        // Every bit set: exception class 0x3f is not an abort, so bit 6 is
        // not read as "write".
        let fault = VmFault::new(u64::MAX, u64::MAX, u64::MAX, false);
        assert_eq!(fault.exception_class(), 0x3f);
        assert_eq!(fault.access(), Access::Read);
        assert_eq!(fault.cause(), Cause::Other(0x3f));
        assert_eq!(
            fault.summary().to_string(),
            "data read at 0xffffffffffffffff: fault status code 0x3f"
        );
    }
}
