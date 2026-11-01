// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system registers and barriers a protection domain may use: the
//! counter, its frequency, the instruction and data barriers, and cleaning
//! the data cache to memory.
//!
//! Written from the Arm Architecture Reference Manual for A-profile
//! architecture (Arm DDI 0487): CNTPCT_EL0, CNTFRQ_EL0, CNTP_CVAL_EL0,
//! CNTP_CTL_EL0, ISB, DSB and DC CVAC.
//!
//! seL4 lets a PD read the **physical** counter on both NoWayOS boards: the
//! Microkit 2.3.1 kernels for `qemu_virt_aarch64` and `rpi5b_2gb` are built
//! with `KernelArmExportPCNTUser`, and neither with the virtual counter. The
//! physical timer's compare and control registers - [`set_timer`] and
//! [`stop_timer`] - are exported on QEMU only (`KernelArmExportPTMRUser`):
//! on the Pi 5 they fault the PD. Only the timer PD uses them, and
//! only it holds the timer's interrupt.
//!
//! On the host - `make pds-test` and Miri, never a PD - the counter is a
//! stand-in that moves forward one tick per read, at a stand-in frequency:
//! Linux does not let a process read CNTPCT_EL0 (it traps), and Miri runs no
//! inline assembly. QEMU runs the real instructions (`systems/pl011-demo`).

/// The counter's value: CNTPCT_EL0, read after an ISB so the read is not
/// taken early. Counts up at [`counter_frequency`] ticks per second from an
/// unspecified start, and never goes back.
#[must_use]
pub fn counter() -> u64 {
    imp::counter()
}

/// The counter's frequency in ticks per second: CNTFRQ_EL0, as the boot
/// firmware set it.
#[must_use]
pub fn counter_frequency() -> u64 {
    imp::counter_frequency()
}

/// Arms the physical timer: its interrupt asserts once [`counter`] reaches
/// `deadline`, and stays asserted until the timer is set again or stopped.
/// Writes CNTP_CVAL_EL0, then CNTP_CTL_EL0 with ENABLE set and IMASK clear.
/// QEMU only (see the module documentation).
pub fn set_timer(deadline: u64) {
    imp::set_timer(deadline);
}

/// Stops the physical timer, so its interrupt does not assert: CNTP_CTL_EL0
/// with ENABLE clear. QEMU only (see the module documentation).
pub fn stop_timer() {
    imp::stop_timer();
}

/// Instruction synchronisation barrier (ISB): later instructions are fetched
/// only after it.
pub fn isb() {
    imp::isb();
}

/// Data synchronisation barrier (DSB SY): every memory access before it,
/// device registers included, completes before any instruction after it.
pub fn dsb() {
    imp::dsb();
}

/// The smallest data cache line [`clean_data_cache`] assumes: 64 bytes, the
/// Cortex-A53's and Cortex-A76's (QEMU's `virt` and the Pi 5).
pub const CACHE_LINE: usize = 64;

/// Cleans the data cache lines holding the `len` bytes at virtual address
/// `vaddr` to the point of coherency - DC CVAC on each [`CACHE_LINE`], then a
/// DSB - so that a device reading memory, such as the display hardware
/// scanning out a framebuffer mapped cached, sees what was written.
/// Nothing in memory changes but where it is held. EL0 may clean by address
/// only when the kernel allows it (SCTLR_EL1.UCI); where it does not, or
/// where an address is not mapped in this PD, the instruction faults and
/// seL4 stops the PD - never undefined behaviour.
pub fn clean_data_cache(vaddr: usize, len: usize) {
    imp::clean_data_cache(vaddr, len);
}

#[cfg(target_os = "none")]
mod imp {
    //! A PD on seL4.

    use core::arch::asm;

    pub(super) fn counter() -> u64 {
        let value: u64;
        // SAFETY: reads a system register that seL4 exports to user level on
        // every NoWayOS board (KernelArmExportPCNTUser), into a register the
        // asm declares; touches no memory and no stack, and the ISB only
        // orders execution.
        unsafe {
            asm!("isb", "mrs {}, cntpct_el0", out(reg) value, options(nomem, nostack, preserves_flags));
        }
        value
    }

    pub(super) fn counter_frequency() -> u64 {
        let value: u64;
        // SAFETY: CNTFRQ_EL0 is readable at EL0 whenever a counter is
        // exported (as above); touches no memory and no stack.
        unsafe {
            asm!("mrs {}, cntfrq_el0", out(reg) value, options(nomem, nostack, preserves_flags));
        }
        value
    }

    /// CNTP_CTL_EL0.ENABLE (bit 0); IMASK (bit 1) clear.
    const ENABLE: u64 = 1;

    pub(super) fn set_timer(deadline: u64) {
        // SAFETY: writes the physical timer's compare value and then its
        // control register (enabled, interrupt not masked), which seL4
        // exports to user level on qemu_virt_aarch64
        // (KernelArmExportPTMRUser); the ISB makes the new setting take
        // effect before the PD goes on. Touches no memory and no stack. On a
        // board that does not export the timer the write faults the PD -
        // stopped by seL4, never undefined behaviour - and only the timer PD,
        // granted the timer's interrupt in a QEMU system, calls this.
        unsafe {
            asm!(
                "msr cntp_cval_el0, {deadline}",
                "msr cntp_ctl_el0, {enable}",
                "isb",
                deadline = in(reg) deadline,
                enable = in(reg) ENABLE,
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    pub(super) fn stop_timer() {
        // SAFETY: as `set_timer`: writes only the physical timer's control
        // register (disabled), then an ISB.
        unsafe {
            asm!(
                "msr cntp_ctl_el0, xzr",
                "isb",
                options(nomem, nostack, preserves_flags)
            );
        }
    }

    pub(super) fn isb() {
        // SAFETY: a barrier: changes no register or memory the compiler
        // relies on, and is allowed at EL0.
        unsafe {
            asm!("isb", options(nostack, preserves_flags));
        }
    }

    pub(super) fn dsb() {
        // SAFETY: a barrier: changes no register or memory the compiler
        // relies on, and is allowed at EL0. Not `nomem`: it orders memory
        // accesses, so the compiler must not move them across it.
        unsafe {
            asm!("dsb sy", options(nostack, preserves_flags));
        }
    }

    pub(super) fn clean_data_cache(vaddr: usize, len: usize) {
        let end = vaddr.saturating_add(len);
        let mut line = vaddr & !(super::CACHE_LINE - 1);
        while line < end {
            // SAFETY: DC CVAC writes one cache line back to memory: it
            // changes no memory's contents, no register and not the stack,
            // so nothing the compiler relies on (not `nomem`: it must not be
            // moved across the stores it publishes). An address not mapped
            // in this PD, or a kernel that does not let EL0 clean by
            // address, makes it fault, and seL4 stops the PD.
            unsafe {
                asm!("dc cvac, {line}", line = in(reg) line, options(nostack, preserves_flags));
            }
            line = line.saturating_add(super::CACHE_LINE);
        }
        dsb();
    }
}

#[cfg(not(target_os = "none"))]
mod imp {
    //! The host: stand-ins, with no inline assembly (see the module
    //! documentation).

    use core::sync::atomic::{AtomicU64, Ordering, fence};

    /// The stand-in frequency: 62.5 MHz, QEMU's `virt` counter.
    const FREQUENCY: u64 = 62_500_000;

    static TICKS: AtomicU64 = AtomicU64::new(0);

    /// The deadline the stand-in timer was last set to; `u64::MAX` when
    /// stopped. Nothing fires on the host.
    pub(super) static DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);

    pub(super) fn set_timer(deadline: u64) {
        DEADLINE.store(deadline, Ordering::Relaxed);
    }

    pub(super) fn stop_timer() {
        DEADLINE.store(u64::MAX, Ordering::Relaxed);
    }

    pub(super) fn counter() -> u64 {
        TICKS.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    pub(super) const fn counter_frequency() -> u64 {
        FREQUENCY
    }

    pub(super) fn isb() {
        fence(Ordering::SeqCst);
    }

    pub(super) fn dsb() {
        fence(Ordering::SeqCst);
    }

    /// The host has no cache to clean for a device: a fence.
    pub(super) fn clean_data_cache(_vaddr: usize, _len: usize) {
        fence(Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_moves_forward_at_a_known_frequency() {
        isb();
        let before = counter();
        dsb();
        assert!(counter() > before);
        assert!(counter_frequency() > 0);
        clean_data_cache(0x1000, 4096);
    }

    #[test]
    fn the_stand_in_timer_records_its_deadline() {
        set_timer(1234);
        assert_eq!(
            imp::DEADLINE.load(core::sync::atomic::Ordering::Relaxed),
            1234
        );
        stop_timer();
        assert_eq!(
            imp::DEADLINE.load(core::sync::atomic::Ordering::Relaxed),
            u64::MAX
        );
    }
}
