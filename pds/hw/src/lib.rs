// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed, safe views of the hardware a driver protection domain uses: its
//! device's registers, the counter and the barriers.
//!
//! Every NoWayOS driver PD is `forbid(unsafe_code)`. The raw hardware access
//! a driver needs lives here instead, in one crate on the approved-`unsafe`
//! list (`tools/unsafe-allowlist.toml`), so "where is all the unsafe code?"
//! keeps a short answer:
//!
//! * [`device`] binds a device's register window from symbols the system
//!   description patches, as `nowayos-region` does for shared memory;
//! * a device module turns it into a typed view of that device's registers:
//!   [`pl011`] for the Arm PL011 UART, [`system_timer`] for the system
//!   timer of Broadcom's BCM2835 family (the Pi 5's BCM2712), reaching only
//!   the ARM's compare channels, [`fw_cfg`] for QEMU's test-only
//!   firmware configuration device (with [`ramfb`], QEMU's framebuffer
//!   display, set up through it), and [`virtio`] for virtio devices over
//!   memory-mapped I/O, found by ID and their features negotiated, with
//!   [`virtqueue`]'s queues in DMA memory, every used-ring entry copied out
//!   and checked before it is followed, and [`virtio_net`]'s receive and
//!   transmit queues for the network card; [`xhci`] for an xHCI USB host
//!   controller, its rings and contexts in its own DMA region, every
//!   event copied out and checked against what was queued, and [`pci`] for
//!   the PCI function a controller is, where no firmware placed it (QEMU);
//!   [`pm_watchdog`] for the watchdog of BCM2712's power management block,
//!   which it can arm but never stop; [`rp1_fan`] for the Pi 5's
//!   fan, on RP1's PWM1, [`hvs`] for where the Pi 5's display is in its
//!   scan, read-only, and [`vc_mailbox`] for the firmware's temperature
//!   through the VideoCore mailbox.
//!   Each register has a fixed type and
//!   offset, checked at compile time, so a driver cannot reach an offset
//!   outside the block; and whether a read has side effects is part of the
//!   type, so reading a FIFO needs `&mut`;
//! * [`dma_region`] binds DMA memory - a memory region with its virtual
//!   address, physical address and size - from patched symbols too. A driver
//!   reads and writes it by offset, and a device is only ever given an
//!   address [`DmaRegion::device_address`] computed and checked to lie inside
//!   it. A register that can start DMA is never reachable through a safe
//!   wrapper: a device module starts transfers itself, with such addresses;
//! * [`io_region`] binds an I/O region - memory another PD maps, which this
//!   PD hands to a device by physical address but never maps itself, such
//!   as a network client's packet buffers - and gives a device
//!   only addresses [`IoRegion::device_address`] checked to lie inside it;
//! * [`scanout`] views a framebuffer the firmware set up and the display
//!   hardware scans out - the Pi 5's, bound like a device's window from
//!   patched symbols - as aligned 64-bit words of memory;
//! * [`cpu`] reads the counter and its frequency, and issues barriers.
//!
//! Registers are read and written through `safe-mmio` (MIT OR Apache-2.0),
//! which never makes a Rust reference to device memory and, on AArch64, uses
//! explicit load and store instructions for each access.
//!
//! # Binding a device from the system description
//!
//! The crate owns a fixed set of symbols for the Microkit tool to patch. A
//! device's `<map>` names one pair:
//!
//! | Symbols (N = 0 to 3) | The `<map>` must be | Bind with |
//! | --- | --- | --- |
//! | `nowayos_device_N_vaddr`, `nowayos_device_N_size` | a device's register window: a `memory_region` with `phys_addr`, mapped `cached="false"` | [`device`]`(N)` |
//! | `nowayos_dma_N_vaddr`, `nowayos_dma_N_size`, and `nowayos_dma_N_paddr` on a `<setvar region_paddr>` naming the same region | a memory region without `phys_addr` (the tool places it) | [`dma_region`]`(N)` |
//! | `nowayos_io_N_paddr` on a `<setvar region_paddr>`, and no map in this PD | a memory region without `phys_addr`, mapped by another PD | [`io_region`]`(N, size)` |
//!
//! ```xml
//! <memory_region name="debug_uart_regs" size="@debug_uart" phys_addr="@debug_uart" />
//! ...
//! <map mr="debug_uart_regs" vaddr="0x200_0000" perms="rw" cached="false"
//!      setvar_vaddr="nowayos_device_0_vaddr" setvar_size="nowayos_device_0_size" />
//! ```
//!
//! A description in `systems/` names the device; `tools/board-system` fills in
//! each board's window from `platforms/BOARD.toml` when the image is built.
//!
//! The address, size and alignment are checked when the device is viewed;
//! that the memory really is that device's registers, uncached, mapped into
//! this PD only, cannot be checked from inside the PD. So
//! **`tools/check-system` checks, when the system image is built, that every
//! `nowayos_device_*` symbol sits on a map of a device's exact register
//! window**, named in the board file (`platforms/BOARD.toml`), uncached (but
//! a display PD's firmware framebuffer, which [`Scanout`] cleans to memory:
//! the grant `cached`), and that no other PD maps that device. A device module also checks the
//! identification registers before it hands out a view, so a description
//! that binds the wrong device fails at start-up, not on the first write.
//!
//! Each device can be bound once per PD and is viewed once, so there is only
//! ever one view of its registers. A driver cannot make a [`Device`] for an
//! address of its own choosing - only the patched symbols create one:
//!
//! ```compile_fail
//! let forged = nowayos_hw::Device { vaddr: 0x0900_0000, size: 0x1000 };
//! ```
//!
//! Nor can it name a register outside its device's block: a view's methods
//! are the only way to a register, and each register's offset is fixed in
//! the layout and asserted against the device's documentation at compile
//! time.
//!
//! # Testing
//!
//! On the host (`make pds-test`) the register views run against ordinary
//! memory standing in for a device, through `safe-mmio`'s real AArch64 load
//! and store instructions; the counter is a stand-in that only moves
//! forward. Under Miri (`make pds-miri`) only the binding and checking logic
//! runs: Miri cannot run inline assembly, which every register access uses.
//!
//! Provisional. The timer's compare and control
//! registers come with the timer PD, the Pi 5's system timer with
//! its Pi back end, virtqueues with the virtio drivers. Whether DMA memory must be mapped uncached is a
//! per-board question - QEMU's DMA is coherent - settled with the first
//! board driver.

#![cfg_attr(not(test), no_std)]

// The only modules allowed `unsafe` code; everything else in the crate is
// safe. tools/check-unsafe lists exactly these files.
#[allow(
    unsafe_code,
    reason = "barriers and system-register reads are inline assembly"
)]
pub mod cpu;
#[allow(
    unsafe_code,
    reason = "declares the patched symbols and turns a patched address into a register view"
)]
mod device;
#[allow(
    unsafe_code,
    reason = "declares the patched symbols and turns a patched address into a view of atomic bytes"
)]
mod dma;
#[allow(
    unsafe_code,
    reason = "safe-mmio's field! projection expands to an unsafe block, and starting a DMA transfer is an unsafe write"
)]
pub mod fw_cfg;
#[allow(
    unsafe_code,
    reason = "a GEM MAC's registers are read and written through safe-mmio's unsafe word accesses; some writes start DMA"
)]
pub mod gem;
#[allow(
    unsafe_code,
    reason = "the HVS's register block's `unsafe impl`, and safe-mmio's field_shared! projections, which expand to an unsafe block"
)]
pub mod hvs;
#[allow(
    unsafe_code,
    reason = "a PCI function's configuration page is read and written through safe-mmio's unsafe word accesses"
)]
pub mod pci;
#[allow(
    unsafe_code,
    reason = "safe-mmio's field! projection expands to an unsafe block"
)]
pub mod pl011;
#[allow(
    unsafe_code,
    reason = "the PM watchdog's registers are read and written through safe-mmio's unsafe word accesses"
)]
pub mod pm_watchdog;
pub mod ramfb;
#[allow(
    unsafe_code,
    reason = "RP1's GPIO registers for the Ethernet PHY's reset are written through safe-mmio's unsafe word accesses"
)]
pub mod rp1;
#[allow(
    unsafe_code,
    reason = "RP1's PWM1, its clock and GPIO45's control are read and written through safe-mmio's unsafe word accesses"
)]
pub mod rp1_fan;
#[allow(
    unsafe_code,
    reason = "turns a patched address into a view of atomic words: the framebuffer the firmware scans out"
)]
pub mod scanout;
#[allow(
    unsafe_code,
    reason = "safe-mmio's field! projection expands to an unsafe block"
)]
pub mod system_timer;
#[allow(
    unsafe_code,
    reason = "the VideoCore mailbox's registers are read and written through safe-mmio's unsafe word accesses; a read pops its queue"
)]
pub mod vc_mailbox;
#[allow(
    unsafe_code,
    reason = "safe-mmio's field! projection expands to an unsafe block, and a status write is an unsafe write"
)]
pub mod virtio;
pub mod virtio_net;
pub mod virtqueue;
#[allow(
    unsafe_code,
    reason = "an xHCI controller's registers are read and written through safe-mmio's unsafe word accesses; some writes start DMA"
)]
pub mod xhci;

mod error;

pub use device::{DEVICES, Device, device};
pub use dma::{DMA_REGIONS, DmaRegion, IO_REGIONS, IoRegion, dma_region, io_region};
pub use error::HwError;
pub use scanout::Scanout;
