// SPDX-License-Identifier: MIT OR Apache-2.0
//! A PCI function's configuration space, through its page of a PCI
//! Express host bridge's ECAM window: enough to place the function's
//! registers where its board file says and let it work.
//!
//! Written from the PCI-SIG's *PCI Local Bus Specification*, revision 3.0
//! (chapter 6: the type 0 configuration header, the Command register, Base
//! Address Registers and how software sizes and places them) and the *PCI
//! Express Base Specification*'s Enhanced Configuration Access Mechanism
//! (each function's 4 KiB of configuration space at bus << 20 | device <<
//! 15 | function << 12 in the window). No other source.
//!
//! A board whose firmware enumerates PCI - the Raspberry Pi 5's U-Boot -
//! leaves each function's BARs placed and its decoding on, and a driver
//! never touches configuration space. QEMU's `virt` machine has no such
//! firmware, so there the xHCI PD places its controller itself: it is
//! granted the function's configuration page, and the board file gives the
//! bus address for BAR 0 (`pci_bar0`, carried in the device's facts), which
//! is where the window the PD maps decodes. QEMU only today.
//!
//! The page is viewed as plain words with no safe access; this module reads
//! the header and writes only the Command register and BAR 0 and 1, each
//! access stated safe where it is made.

use safe_mmio::UniqueMmioPointer;

use crate::{Device, HwError};

/// Configuration space offsets (PCI 3.0, figure 6-1), in bytes.
const ID: usize = 0x00;
const COMMAND: usize = 0x04;
const CLASS: usize = 0x08;
const HEADER: usize = 0x0C;
const BAR0: usize = 0x10;
const BAR1: usize = 0x14;

/// Command register bits (section 6.2.2): Memory Space, Bus Master.
const MEMORY_SPACE: u32 = 1 << 1;
const BUS_MASTER: u32 = 1 << 2;
/// Interrupt Disable: legacy INTx off - the driver polls.
const INTX_DISABLE: u32 = 1 << 10;

/// A function's identity, read from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Vendor ID.
    pub vendor: u16,
    /// Device ID.
    pub device: u16,
    /// Class code: base class, subclass and programming interface, as one
    /// 24-bit value (0x0C0330: an xHCI USB controller).
    pub class: u32,
}

/// One PCI function's configuration space. Make one with
/// [`PciFunction::new`].
#[derive(Debug)]
pub struct PciFunction {
    words: UniqueMmioPointer<'static, [u32]>,
}

impl PciFunction {
    /// Views `device` - one function's 4 KiB page of an ECAM window - and
    /// checks a function is there with a type 0 header.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::PciFunction`] if no
    /// function answers (vendor ID all ones) or its header is not type 0.
    pub fn new(device: Device) -> Result<Self, HwError> {
        let mut function = Self {
            words: device.view_words()?,
        };
        if function.read(ID)? & 0xFFFF == 0xFFFF {
            return Err(HwError::PciFunction {
                reason: "no function answers in the page granted",
            });
        }
        if (function.read(HEADER)? >> 16) & 0x7F != 0 {
            return Err(HwError::PciFunction {
                reason: "not a type 0 configuration header",
            });
        }
        Ok(function)
    }

    /// The function's vendor, device and class.
    ///
    /// # Errors
    ///
    /// None in practice: the header is inside the page.
    pub fn identity(&mut self) -> Result<Identity, HwError> {
        let id = self.read(ID)?;
        Ok(Identity {
            vendor: (id & 0xFFFF) as u16,
            device: (id >> 16) as u16,
            class: self.read(CLASS)? >> 8,
        })
    }

    /// Places BAR 0 - a memory BAR, 32 or 64 bits - at bus address
    /// `address`, after sizing it and checking it fits in `window` bytes,
    /// and turns on memory decoding and bus mastering, with legacy
    /// interrupts off. Returns the BAR's size.
    ///
    /// # Errors
    ///
    /// [`HwError::PciFunction`] if BAR 0 is an I/O BAR, is larger than
    /// `window`, cannot take `address` (misaligned, or above 4 GiB for a
    /// 32-bit BAR), or does not read back as `address`.
    pub fn place_bar0(&mut self, address: u64, window: usize) -> Result<u64, HwError> {
        let bad = |reason| HwError::PciFunction { reason };
        let command = self.read(COMMAND)? & 0xFFFF;
        // Decoding off while the BAR is sized and moved (section 6.2.5.1).
        self.write(COMMAND, command & !(MEMORY_SPACE | BUS_MASTER))?;
        let original = self.read(BAR0)?;
        if original & 1 != 0 {
            return Err(bad("BAR 0 is an I/O BAR"));
        }
        let wide = (original >> 1) & 0x3 == 0x2;
        // Size it: write all ones, read back which address bits stick.
        self.write(BAR0, u32::MAX)?;
        let low_mask = self.read(BAR0)? & !0xF;
        let high_mask = if wide {
            self.write(BAR1, u32::MAX)?;
            self.read(BAR1)?
        } else {
            u32::MAX
        };
        let mask = (u64::from(high_mask) << 32) | u64::from(low_mask);
        let size = (!mask).wrapping_add(1);
        if low_mask == 0 || size == 0 || u64::try_from(window).map_or(true, |window| size > window)
        {
            return Err(bad("BAR 0 is larger than its window"));
        }
        if address & (size.wrapping_sub(1)) != 0 || (!wide && address > u64::from(u32::MAX)) {
            return Err(bad("BAR 0 cannot take the address given"));
        }
        self.write(BAR0, (address as u32 & !0xF) | (original & 0xF))?;
        if wide {
            self.write(BAR1, (address >> 32) as u32)?;
        }
        let placed = (u64::from(if wide { self.read(BAR1)? } else { 0 }) << 32)
            | u64::from(self.read(BAR0)? & !0xF);
        if placed != address {
            return Err(bad("BAR 0 did not take the address given"));
        }
        self.write(COMMAND, command | MEMORY_SPACE | BUS_MASTER | INTX_DISABLE)?;
        Ok(size)
    }

    /// The configuration word at byte `offset` of the header.
    fn read(&mut self, offset: usize) -> Result<u32, HwError> {
        let mut word = self.words.get(offset / 4).ok_or(HwError::TooSmall {
            size: offset,
            needed: offset.saturating_add(4),
        })?;
        // SAFETY: `read_unsafe` needs a read that is safe to make. A
        // configuration read of the type 0 header (offsets 0x00 to 0x14,
        // the only ones this module passes) has no side effect (PCI 3.0,
        // section 6.1), and the index is inside the page (`get`).
        Ok(unsafe { word.read_unsafe() })
    }

    /// Writes the configuration word at byte `offset`: only ever the
    /// Command register or BAR 0 and 1.
    fn write(&mut self, offset: usize, value: u32) -> Result<(), HwError> {
        if !matches!(offset, COMMAND | BAR0 | BAR1) {
            return Err(HwError::PciFunction {
                reason: "a write outside the Command register and BAR 0",
            });
        }
        let mut word = self.words.get(offset / 4).ok_or(HwError::TooSmall {
            size: offset,
            needed: offset.saturating_add(4),
        })?;
        // SAFETY: `write_unsafe` needs a write that is safe to make. Only
        // the Command register and BAR 0 and 1 are written (checked above):
        // the first switches the function's decoding and bus mastering - the
        // function then reads and writes memory only where its own registers
        // point it, which only its driver writes - and the BARs move its
        // register window within the host bridge's window, which nothing but
        // this PD maps. No other memory is touched by the write itself.
        unsafe { word.write_unsafe(value) };
        Ok(())
    }
}

#[cfg(all(test, not(miri)))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! The real loads and stores, on ordinary memory standing in for a
    //! configuration page - inline assembly, so not under Miri: it keeps what is written, so a BAR "sizes" as
    //! whatever its address bits read back, which these tests set up.

    use super::*;

    #[repr(C, align(4096))]
    struct Page([u32; 1024]);

    fn page(words: &[(usize, u32)]) -> &'static mut Page {
        let page = Box::leak(Box::new(Page([0; 1024])));
        for &(offset, value) in words {
            page.0[offset / 4] = value;
        }
        page
    }

    fn function(page: &'static mut Page) -> Result<PciFunction, HwError> {
        let vaddr = core::ptr::from_mut(page).expose_provenance();
        PciFunction::new(Device::for_test(vaddr, 4096))
    }

    #[test]
    fn qemus_xhci_function_is_identified() {
        let mut function = function(page(&[(ID, 0x000D_1B36), (CLASS, 0x0C03_3001)])).unwrap();
        assert_eq!(
            function.identity().unwrap(),
            Identity {
                vendor: 0x1B36,
                device: 0x000D,
                class: 0x0C_0330
            }
        );
    }

    #[test]
    fn an_empty_slot_or_a_bridge_is_refused() {
        assert_eq!(
            function(page(&[(ID, 0xFFFF_FFFF)])).unwrap_err(),
            HwError::PciFunction {
                reason: "no function answers in the page granted"
            }
        );
        assert_eq!(
            function(page(&[(ID, 0x2712_14E4), (HEADER, 0x0001_0000)])).unwrap_err(),
            HwError::PciFunction {
                reason: "not a type 0 configuration header"
            }
        );
    }

    #[test]
    fn a_bar_is_sized_placed_and_decoding_turned_on() {
        // Ordinary memory keeps all ones when they are written, so this
        // 64-bit BAR (type 0b10) sizes as 16 bytes: a 16 KiB window holds it.
        let mut function = function_with(&[(ID, 0x000D_1B36), (COMMAND, 0x0010_0000), (BAR0, 0x4)]);
        assert_eq!(function.place_bar0(0x1000_4000, 0x4000), Ok(16));
        assert_eq!(function.read(BAR0).unwrap(), 0x1000_4004);
        assert_eq!(function.read(BAR1).unwrap(), 0);
        assert_eq!(
            function.read(COMMAND).unwrap() & 0xFFFF,
            MEMORY_SPACE | BUS_MASTER | INTX_DISABLE
        );
        // An address the BAR cannot take: off its alignment.
        assert_eq!(
            function.place_bar0(0x1000_4008, 0x4000),
            Err(HwError::PciFunction {
                reason: "BAR 0 cannot take the address given"
            })
        );
        // A 32-bit BAR above 4 GiB.
        let mut narrow = function_with(&[(ID, 0x000D_1B36), (BAR0, 0x0)]);
        assert_eq!(
            narrow.place_bar0(0x1_0000_0000, 0x4000),
            Err(HwError::PciFunction {
                reason: "BAR 0 cannot take the address given"
            })
        );
        // A window smaller than the BAR.
        let mut big = function_with(&[(ID, 0x000D_1B36), (BAR0, 0x4)]);
        assert_eq!(
            big.place_bar0(0x1000_0000, 8),
            Err(HwError::PciFunction {
                reason: "BAR 0 is larger than its window"
            })
        );
        // An I/O BAR.
        let mut io = function_with(&[(ID, 0x000D_1B36), (BAR0, 0x1)]);
        assert_eq!(
            io.place_bar0(0x1000_0000, 0x4000),
            Err(HwError::PciFunction {
                reason: "BAR 0 is an I/O BAR"
            })
        );
        // No write outside the Command register and the BARs.
        assert!(io.write(ID, 0).is_err());
    }

    fn function_with(words: &[(usize, u32)]) -> PciFunction {
        function(page(words)).unwrap()
    }
}
