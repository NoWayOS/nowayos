// SPDX-License-Identifier: MIT OR Apache-2.0
//! The watchdog of BCM2712's power management block (PM), on the
//! Raspberry Pi 5: armed, and kept from firing, by the watchdog PD
//! only.
//!
//! Broadcom documents no part of the PM block publicly: BCM2835 ARM
//! Peripherals (2012) has no chapter on it (searched: no RSTC, WDOG or
//! power management registers), and BCM2712 has no peripherals datasheet.
//! The facts here are from three sources, none of them GPL:
//!
//! * the pinned firmware's device tree (`platforms/rpi5b_2gb.toml`,
//!   `pm_watchdog`): the block at 0x10_7d20_0000, `brcm,bcm2712-pm`, 0x308
//!   bytes of registers;
//! * FreeBSD's BCM2835 watchdog driver, `sys/arm/broadcom/bcm2835/
//!   bcm2835_wdog.c` (BSD-2-Clause, Copyright (c) 2012 Alexander Rybalko),
//!   read for its register facts only - no code is taken from it: RSTC,
//!   RSTS and WDOG at 0x1c, 0x20 and 0x24 of the block; a write takes
//!   effect only with the password 0x5a in its top byte; WDOG's time in
//!   bits 19:0, the seconds shifted left 16; RSTC's reset configuration in
//!   bits 5:4, 0b10 for a full reset when WDOG runs out;
//! * the board itself (`tools/pi-run`, 2026-10-07): RSTC reads 0x202 at
//!   every boot; WDOG written as 0x5a0c_0000 reset the board 12 s later,
//!   and read back counting down; RSTC written as 0x5a00_0222 armed it.
//!
//! The view arms the watchdog for a number of whole seconds, 1 to
//! [`MAX_TIMEOUT_SECS`], and reads what is left; **it cannot stop the
//! watchdog** - no write it makes turns the reset off - so a watchdog PD
//! that stops servicing always ends in a reset. It reads and writes RSTC,
//! RSTS and WDOG only; the rest of the page - the block's power domain and
//! reset controls - is never touched, though it is mapped.

use safe_mmio::UniqueMmioPointer;

use crate::{Device, HwError};

/// The longest time the watchdog can be armed for: WDOG's 20 bits hold
/// just under 16 seconds of 65,536 ticks.
pub const MAX_TIMEOUT_SECS: u32 = 15;

/// RSTC, the reset control register.
const RSTC: usize = 0x1c;
/// RSTS, the reset status register.
const RSTS: usize = 0x20;
/// WDOG, the watchdog's time.
const WDOG: usize = 0x24;
/// The password a write must carry in its top byte.
const PASSWORD: u32 = 0x5a << 24;
/// WDOG's time field.
const WDOG_TIME: u32 = 0x000f_ffff;
/// WDOG's ticks a second.
const TICKS_PER_SECOND: u32 = 1 << 16;
/// RSTC's reset configuration field, bits 5:4.
const RSTC_CONFIG: u32 = 0b11 << 4;
/// RSTC's reset configuration for a full reset when WDOG runs out.
const RSTC_FULL_RESET: u32 = 0b10 << 4;
/// RSTC's other bits: what a write keeps (the password byte is replaced).
const RSTC_KEPT: u32 = !(RSTC_CONFIG | 0xff00_0000);

/// The PM block's watchdog. Make one with [`PmWatchdog::new`].
#[derive(Debug)]
pub struct PmWatchdog {
    regs: UniqueMmioPointer<'static, [u32]>,
}

/// The watchdog's registers, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmState {
    /// RSTC.
    pub rstc: u32,
    /// RSTS.
    pub rsts: u32,
    /// WDOG's time left, in 1/65,536 s.
    pub ticks_left: u32,
}

impl PmWatchdog {
    /// Views `device` as the PM block's watchdog.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::Watchdog`] for a window
    /// that does not reach WDOG.
    pub fn new(device: Device) -> Result<Self, HwError> {
        let mut view = Self {
            regs: device.view_words()?,
        };
        view.read(WDOG)?;
        Ok(view)
    }

    /// Arms the watchdog to reset the board `seconds` from now, unless
    /// armed again first - the servicing: WDOG written with the time, then
    /// RSTC's reset configuration set to a full reset, its other bits kept.
    ///
    /// # Errors
    ///
    /// [`HwError::Watchdog`] for 0 seconds or more than
    /// [`MAX_TIMEOUT_SECS`]: nothing is written.
    pub fn service(&mut self, seconds: u32) -> Result<(), HwError> {
        if !(1..=MAX_TIMEOUT_SECS).contains(&seconds) {
            return Err(HwError::Watchdog {
                reason: "a timeout of 0 or over 15 s",
            });
        }
        let ticks = seconds.saturating_mul(TICKS_PER_SECOND) & WDOG_TIME;
        self.write(WDOG, PASSWORD | ticks)?;
        let rstc = self.read(RSTC)?;
        self.write(RSTC, PASSWORD | (rstc & RSTC_KEPT) | RSTC_FULL_RESET)
    }

    /// RSTC, RSTS and the time left, as read now: for a report.
    ///
    /// # Errors
    ///
    /// [`HwError::Watchdog`] for a window too small (checked when made).
    pub fn state(&mut self) -> Result<PmState, HwError> {
        Ok(PmState {
            rstc: self.read(RSTC)?,
            rsts: self.read(RSTS)?,
            ticks_left: self.read(WDOG)? & WDOG_TIME,
        })
    }

    fn read(&mut self, at: usize) -> Result<u32, HwError> {
        let mut register = self.regs.get(at / 4).ok_or(HwError::Watchdog {
            reason: "a window that does not reach WDOG",
        })?;
        // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
        // window (`get` checked the index), of RSTC, RSTS or WDOG only, which
        // reading leaves as they were (read back on the board from U-Boot's
        // prompt, 2026-10-07: WDOG counting down undisturbed).
        Ok(unsafe { register.read_unsafe() })
    }

    fn write(&mut self, at: usize, value: u32) -> Result<(), HwError> {
        let mut register = self.regs.get(at / 4).ok_or(HwError::Watchdog {
            reason: "a window that does not reach WDOG",
        })?;
        // SAFETY: `write_unsafe` needs a write that is safe to make: inside
        // the window (`get`), and only ever WDOG's time or RSTC's reset
        // configuration from `service` - at worst a reset of the board, never
        // a write to memory, and never one that turns the watchdog off.
        unsafe { register.write_unsafe(value) };
        Ok(())
    }
}

// Not under Miri: the registers are read and written through safe-mmio's
// inline assembly, which Miri cannot run; make pds-test runs these on the
// host.
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

    fn block(rstc: u32) -> &'static [AtomicU32] {
        let words: &'static [AtomicU32] = Box::leak(
            (0..1024)
                .map(|_| AtomicU32::new(0x5555_5555))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        words[RSTC / 4].store(rstc, Ordering::Relaxed);
        words[WDOG / 4].store(0, Ordering::Relaxed);
        words
    }

    fn view(words: &'static [AtomicU32]) -> PmWatchdog {
        PmWatchdog::new(Device::for_test(
            words.as_ptr().expose_provenance(),
            words.len() * 4,
        ))
        .unwrap()
    }

    #[test]
    fn servicing_writes_what_armed_the_board_by_hand_and_nothing_else() {
        let words = block(0x0000_0202);
        let mut pm = view(words);
        pm.service(12).unwrap();
        // As written by hand on the board (tools/pi-run): WDOG 0x5a0c0000,
        // RSTC 0x5a000222 from the 0x202 it reads at boot.
        assert_eq!(words[WDOG / 4].load(Ordering::Relaxed), 0x5a0c_0000);
        assert_eq!(words[RSTC / 4].load(Ordering::Relaxed), 0x5a00_0222);
        pm.service(6).unwrap();
        assert_eq!(words[WDOG / 4].load(Ordering::Relaxed), 0x5a06_0000);
        // Armed again from RSTC as it now reads: the same configuration.
        assert_eq!(words[RSTC / 4].load(Ordering::Relaxed), 0x5a00_0222);
        for (index, word) in words.iter().enumerate() {
            if index != RSTC / 4 && index != WDOG / 4 {
                assert_eq!(word.load(Ordering::Relaxed), 0x5555_5555, "word {index}");
            }
        }
    }

    #[test]
    fn the_reset_configuration_alone_changes_in_rstc() {
        let words = block(0xffff_ffff);
        view(words).service(15).unwrap();
        assert_eq!(words[RSTC / 4].load(Ordering::Relaxed), 0x5aff_ffef);
        assert_eq!(words[WDOG / 4].load(Ordering::Relaxed), 0x5a0f_0000);
    }

    #[test]
    fn a_timeout_out_of_range_writes_nothing() {
        let words = block(0x0000_0202);
        let mut pm = view(words);
        for seconds in [0, 16, u32::MAX] {
            assert!(matches!(pm.service(seconds), Err(HwError::Watchdog { .. })));
        }
        assert_eq!(words[RSTC / 4].load(Ordering::Relaxed), 0x0000_0202);
        assert_eq!(words[WDOG / 4].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_state_reads_the_three_registers() {
        let words = block(0x0000_0202);
        words[RSTS / 4].store(0x1000, Ordering::Relaxed);
        words[WDOG / 4].store(0x5a03_8000, Ordering::Relaxed);
        assert_eq!(
            view(words).state().unwrap(),
            PmState {
                rstc: 0x202,
                rsts: 0x1000,
                ticks_left: 0x3_8000
            }
        );
    }

    #[test]
    fn a_window_short_of_wdog_is_refused() {
        let words: &'static [AtomicU32] = Box::leak(
            (0..9)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let short = Device::for_test(words.as_ptr().expose_provenance(), words.len() * 4);
        assert!(matches!(
            PmWatchdog::new(short),
            Err(HwError::Watchdog { .. })
        ));
    }
}
