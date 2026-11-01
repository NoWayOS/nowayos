// SPDX-License-Identifier: MIT OR Apache-2.0
//! The census: the kernel's own list of the threads on this core,
//! the only `unsafe` code in this crate.
//!
//! A PD cannot read which core it runs on: AArch64 gives EL0 no register
//! that says (MPIDR_EL1 is EL1's), and Microkit patches no symbol for it.
//! The kernel knows: a debug kernel's `seL4_DebugDumpScheduler` prints the
//! threads bound to the calling thread's core - each one's name, state,
//! priority and core - and rust-sel4 does not wrap it, so this is the one
//! system call, as the Microkit 2.3.1 SDK's libsel4 makes it
//! (`sel4/arch/syscalls.h`). A release kernel has no such call, and a PD
//! that makes it there takes an unknown-syscall fault: the probe runs only
//! in debug configurations.

use core::arch::asm;

use sel4_microkit::debug_println;

/// `seL4_SysDebugDumpScheduler`, from the Microkit 2.3.1 SDK's
/// `sel4/syscall.h` (present when the kernel is built with printing).
const SYS_DEBUG_DUMP_SCHEDULER: isize = -13;

/// Print a line naming the PD, then have the kernel list the threads on
/// this core.
pub(crate) fn list_this_core(name: &str) {
    debug_println!("NOWAYOS: budget-probe: {name}: the kernel's threads on this core:");
    // SAFETY: one `svc #0` with x7 holding a debug system call's number,
    // exactly as libsel4's `arm_sys_send_recv` makes it: the kernel reads
    // no argument, prints its scheduler's list on the debug console and
    // returns, touching no memory of this PD. x0 to x6 carry arguments and
    // results in seL4's AArch64 calling convention, so all are marked as
    // overwritten; the stack is untouched.
    unsafe {
        asm!(
            "svc #0",
            in("x7") SYS_DEBUG_DUMP_SCHEDULER,
            inout("x0") 0usize => _,
            inout("x1") 0usize => _,
            inout("x2") 0usize => _,
            inout("x3") 0usize => _,
            inout("x4") 0usize => _,
            inout("x5") 0usize => _,
            inout("x6") 0usize => _,
            options(nostack),
        );
    }
}
