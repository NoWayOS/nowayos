// SPDX-License-Identifier: MIT OR Apache-2.0
//! RP1's GPIO, for one pin only: the Raspberry Pi 5's Ethernet PHY reset.
//! The firmware leaves the PHY held in reset - its reset line low,
//! the pad's output disabled and pulled down (read from U-Boot's prompt,
//! 2026-10-14) - and with it held, no PHY answers on MDIO. The NIC driver
//! PD drives the line high once, before it talks to the PHY.
//!
//! The line is RP1's GPIO 32, `ETH_RST_N`, active low (the pinned
//! firmware's device tree: `gpio-line-names`, and the ethernet node's
//! `phy-reset-gpios = <&rp1_gpio 32 GPIO_ACTIVE_LOW>`, reset for 5 ms). It
//! is in RP1's second GPIO bank, which *RP1 Peripherals* (2023-11-07)
//! places - `io_bank1` at 0x400d_4000, `pads_bank1` at 0x400f_4000 (table
//! 2) - but calls reserved for internal use (section 3.1). This module
//! assumes bank 1's registers are laid out as bank 0's, which the datasheet
//! documents (section 3.1.4: a pin's `STATUS` and `CTRL` at 8 bytes a pin,
//! its pad register at 4 bytes a pin after `VOLTAGE_SELECT`), with GPIO 28
//! first; reads and writes from U-Boot's prompt bore that out on the board:
//! pin 4 of bank 1 read low, and once driven high, the PHY answered.
//!
//! It writes two registers and nothing else: the pin's `CTRL`, with
//! `OUTOVER` and `OEOVER` driving the output high and enabled whatever the
//! pin's function (section 3.1.4, table 8), and its pad's, with the output
//! no longer disabled (`OD`, table 21). The other five pins of the bank -
//! the fan's tachometer, the PCIe wake and I2C lines among them - are never
//! touched, though the pages are this PD's.

use safe_mmio::UniqueMmioPointer;

use crate::{Device, HwError};

/// The PHY reset's pin within bank 1: GPIO 32, bank 1 starting at 28.
pub const PHY_RESET_PIN: usize = 4;

/// The pin's `CTRL` register in `io_bank1`: 8 bytes a pin, `STATUS` first.
const CTRL: usize = PHY_RESET_PIN * 8 + 4;
/// The pin's `STATUS` register in `io_bank1`.
const STATUS: usize = PHY_RESET_PIN * 8;
/// The pin's pad register in `pads_bank1`: after `VOLTAGE_SELECT`.
const PAD: usize = 4 + PHY_RESET_PIN * 4;

/// `CTRL`'s `OUTOVER`, bits 13:12, and `OEOVER`, bits 15:14.
const OVERRIDES: u32 = 0b1111 << 12;
/// `OUTOVER` 3, drive the output high; `OEOVER` 3, enable the output.
const DRIVE_HIGH: u32 = 0b11 << 12 | 0b11 << 14;
/// The pad's `OD`, output disable.
const PAD_OUTPUT_DISABLE: u32 = 1 << 7;
/// `STATUS`'s `OUTTOPAD`: the output the pad is given.
const STATUS_OUT_TO_PAD: u32 = 1 << 9;
/// `STATUS`'s `INFROMPAD`: the level at the pad.
const STATUS_IN_FROM_PAD: u32 = 1 << 17;

/// The PHY reset line before and after [`release_phy_reset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyReset {
    /// Whether the pad read high (reset released) before.
    pub was_released: bool,
    /// Whether it reads high now.
    pub released: bool,
}

/// Releases the Ethernet PHY's reset: drives RP1's GPIO 32 high through
/// `io_bank1` (RP1's 0x400d_4000) and `pads_bank1` (0x400f_4000), both
/// consumed. The caller then waits for the PHY to come out of reset.
///
/// # Errors
///
/// Any error of viewing either page; [`HwError::Gem`] for a page too small.
pub fn release_phy_reset(io_bank: Device, pads_bank: Device) -> Result<PhyReset, HwError> {
    let mut io = io_bank.view_words()?;
    let mut pads = pads_bank.view_words()?;
    let was_released = read(&mut io, STATUS)? & STATUS_IN_FROM_PAD != 0;
    let ctrl = read(&mut io, CTRL)?;
    let pad = read(&mut pads, PAD)?;
    write(&mut io, CTRL, (ctrl & !OVERRIDES) | DRIVE_HIGH)?;
    write(&mut pads, PAD, pad & !PAD_OUTPUT_DISABLE)?;
    let status = read(&mut io, STATUS)?;
    Ok(PhyReset {
        was_released,
        released: status & STATUS_OUT_TO_PAD != 0,
    })
}

fn word(at: usize) -> Result<usize, HwError> {
    if at.is_multiple_of(4) {
        Ok(at / 4)
    } else {
        Err(HwError::Gem {
            reason: "an RP1 GPIO register off a word",
        })
    }
}

fn read(regs: &mut UniqueMmioPointer<'static, [u32]>, at: usize) -> Result<u32, HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Gem {
        reason: "an RP1 GPIO page too small",
    })?;
    // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
    // page (`get` checked the index), of the PHY reset pin's `STATUS`, `CTRL`
    // or pad register, none of which changes state when read (RP1
    // Peripherals tables 7, 8 and 21: read-only or read-write).
    Ok(unsafe { register.read_unsafe() })
}

fn write(
    regs: &mut UniqueMmioPointer<'static, [u32]>,
    at: usize,
    value: u32,
) -> Result<(), HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Gem {
        reason: "an RP1 GPIO page too small",
    })?;
    // SAFETY: `write_unsafe` needs a write that is safe to make: inside the
    // page (`get`), and only ever the PHY reset pin's `CTRL` (its output
    // overrides) or its pad register (output disable cleared), from this
    // module's two writes - a GPIO's level, which starts no DMA and touches
    // no memory.
    unsafe { register.write_unsafe(value) };
    Ok(())
}

// Not under Miri: the GPIO registers are read and written through
// safe-mmio's inline assembly, which Miri cannot run; make pds-test runs
// these on the host.
#[cfg(all(test, not(miri)))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test code: a failed check panics, and tests index and compute on known data"
    )]

    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn page(words: &'static [AtomicU32]) -> Device {
        Device::for_test(words.as_ptr().expose_provenance(), words.len() * 4)
    }

    #[test]
    fn drives_gpio_32_high_and_touches_nothing_else() {
        let io: &'static [AtomicU32] = Box::leak(
            (0..1024)
                .map(|_| AtomicU32::new(0x9F))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let pads: &'static [AtomicU32] = Box::leak(
            (0..1024)
                .map(|_| AtomicU32::new(0xD6))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        // As read on the board: the pin low.
        io[STATUS / 4].store(0x0440_0000, Ordering::Relaxed);
        let reset = release_phy_reset(page(io), page(pads)).unwrap();
        assert!(!reset.was_released);
        assert_eq!(io[CTRL / 4].load(Ordering::Relaxed), 0xF09F);
        assert_eq!(pads[PAD / 4].load(Ordering::Relaxed), 0x56);
        // As written by hand on the board: CTRL 0xf09f at 0x24, pad 0x56 at 0x14.
        assert_eq!((CTRL, PAD), (0x24, 0x14));
        for (index, word) in io.iter().enumerate() {
            if index != CTRL / 4 && index != STATUS / 4 {
                assert_eq!(word.load(Ordering::Relaxed), 0x9F, "io word {index}");
            }
        }
        for (index, word) in pads.iter().enumerate() {
            if index != PAD / 4 {
                assert_eq!(word.load(Ordering::Relaxed), 0xD6, "pad word {index}");
            }
        }
    }
}
