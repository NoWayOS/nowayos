// SPDX-License-Identifier: MIT OR Apache-2.0
//! The forbidden accesses: the only `unsafe` code in this crate.
//!
//! Each function makes one memory access that this PD's capabilities do not
//! allow. seL4 is expected to raise a VM fault on the access instruction and
//! hand the fault to the parent PD, which never resumes this PD — so in a
//! correctly isolated system neither function returns. They return only if
//! isolation is broken, and the caller then reports that.
//!
//! Each access is one inline-assembly instruction, emitted as written and
//! never removed or reordered — not `read_volatile` or `write_volatile`,
//! whose contract requires that an access to memory outside Rust's
//! allocations not trap, while these are meant to. They target memory where
//! no Rust value of this PD lives: the program image, the stack and the IPC
//! buffer are elsewhere in the address space (see the system descriptions).

use core::arch::asm;

/// An address a probe reads or writes, where nothing should be mapped in its
/// own address space (`systems/ui-isolation-widened.system` maps one on
/// purpose). Only these addresses are ever accessed, so the safety arguments
/// below cover each of them; keep each in step with the systems that use it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Unmapped {
    /// `0x4000_0000`: where the bystander's private page is mapped in the
    /// bystander's address space (`systems/isolation-test.system`).
    BystanderPage,
    /// `0x6000_0000`: where `display_side`, standing in for the display's
    /// private state, maps its private page (`systems/ui-isolation.system`).
    DisplayPrivatePage,
    /// `0x0900_0000`: the physical address of the registers of QEMU `virt`'s
    /// PL011 UART (`debug_uart` in `platforms/qemu_virt_aarch64.toml`), read as a
    /// virtual address (`systems/ui-isolation.system`).
    UartRegisters,
    /// `0x4100_0000`: where the input broker maps the messages of its first
    /// driver ring, the tablet's (`systems/input-broker-test.system`), and
    /// where `broker_side` keeps its private page
    /// (`systems/input-isolation.system`).
    DriverRing,
    /// `0x300_0000`: where the virtio-net driver maps its receive queue's
    /// DMA memory (`systems/net-test.system`), and where `driver_side`
    /// keeps its private page (`systems/net-isolation.system`).
    DriverDma,
}

impl Unmapped {
    /// The address this probe reads.
    pub(crate) const fn address(self) -> usize {
        match self {
            Self::BystanderPage => 0x4000_0000,
            Self::DisplayPrivatePage => 0x6000_0000,
            Self::UartRegisters => 0x0900_0000,
            Self::DriverRing => 0x4100_0000,
            Self::DriverDma => 0x300_0000,
        }
    }
}

/// Reads a word at `target`'s address.
pub(crate) fn read_unmapped(target: Unmapped) -> u32 {
    let address = target.address();
    let value: u32;
    // SAFETY: one aligned 32-bit load from a non-null address where the
    // probe that reads it binds nothing, so no Rust value lives there. It is
    // meant to trap: seL4 sends the fault to the parent, which suspends this
    // thread and never replies, so the block never returns, which an `asm!`
    // block may do. If isolation is broken it returns a word, possibly one
    // another PD writes (`ui-isolation-widened`); any bit pattern is a valid
    // `u32`.
    unsafe {
        asm!(
            "ldr {value:w}, [{address}]",
            address = in(reg) address,
            value = out(reg) value,
            options(nostack, readonly, preserves_flags),
        );
    }
    value
}

/// Writes `value` to the word at `target`'s address.
pub(crate) fn write_unmapped(target: Unmapped, value: u32) {
    let address = target.address();
    // SAFETY: one aligned 32-bit store to a non-null address where the probe
    // that writes it binds nothing, so no Rust value of this PD lives there.
    // It is meant to trap: seL4 sends the fault to the parent, which
    // suspends this thread and never replies. If isolation is broken it
    // changes a word no Rust code in this PD reads or writes; the caller
    // reports it.
    unsafe {
        asm!(
            "str {value:w}, [{address}]",
            address = in(reg) address,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

/// Writes `value` to the word at `vaddr`, the start of one of this PD's
/// read-only regions (patched into a `nowayos_ro_region_N_vaddr` symbol by
/// the Microkit tool).
pub(crate) fn write_read_only(vaddr: usize, value: u32) {
    // SAFETY: one aligned 32-bit store to `vaddr`, which is non-null
    // (`nowayos_region` refuses 0 when binding) and 4-byte aligned (checked
    // when the probe viewed the region as words). The mapping is read-only,
    // so the store is meant to trap, and seL4 stops this thread there for
    // good. If isolation is broken it changes one word of a region this PD
    // views only as atomics, on this thread, after its own load finished;
    // the caller reports it.
    unsafe {
        asm!(
            "str {value:w}, [{vaddr}]",
            vaddr = in(reg) vaddr,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}
