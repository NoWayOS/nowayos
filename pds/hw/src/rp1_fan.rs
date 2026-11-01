// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Raspberry Pi 5's fan: its PWM on RP1's PWM1 channel 3, out
//! on RP1's GPIO 45 (`FAN_PWM`) to the fan connector, and the speed RP1's
//! own firmware measures on GPIO 29 (`FAN_TACH`).
//!
//! The firmware leaves the fan off: GPIO 45 a plain output (`SYS_RIO`)
//! driven high, PWM1's clock stopped. [`start`] turns the clock on, hands
//! the pin to PWM1 and runs channel 3 at full speed; [`Fan::set_duty`]
//! then sets the speed, and [`Fan::rpm`] reads it.
//!
//! The facts, all checked from U-Boot's prompt on the board (2026-10-16):
//!
//! * **The pins** - the pinned device tree's `gpio-line-names` (GPIO 29
//!   `FAN_TACH`, 45 `FAN_PWM`) and its `cooling_fan` node: `pwm-fan` on
//!   `pwm@9c000` (PWM1) channel 3, a 41,566 ns period, inverted. RP1's GPIO
//!   banks hold 28, 6 and 20 pins, so GPIO 45 is bank 2's pin 11 (`STATUS`
//!   at 0x58, `CTRL` at 0x5c of `io_bank2`, bank 0's layout in *RP1
//!   Peripherals* section 3.1.4). Function 0 is PWM1's channel 3. Driven
//!   low the fan runs; high, it stops.
//! * **PWM1** - *RP1 Peripherals* (2023-11-07) section 3.4: `GLOBAL_CTRL`
//!   (`SET_UPDATE`, the channel enables), and channel 3's `CTRL`, `RANGE`
//!   and `DUTY` at 0x44, 0x48 and 0x50. Channel 3 counts only once its clock
//!   runs: `INTR`'s `CHAN3_RELOAD` sets then, never before.
//!   third-party description : `CLK_PWM1_CTRL` at
//!   0x84 (`ENABLE` bit 11, the auxiliary source in bits 9:5, 2 the 50 MHz
//!   crystal), its integer and fractional dividers at 0x88 and 0x8c. Read
//!   on the board: disabled, dividers 1 and 0. Enabled from the crystal, a
//!   channel ranged at 50,000,000 ran a 1 Hz square wave (measured 0.998 s):
//!   50 MHz, so [`RANGE`] 2,078 is the device tree's 41,566 ns, 24.06 kHz.
//! * **The speed** - RP1's firmware counts the tachometer itself and leaves
//!   the fan's speed in rpm in PWM1's `CHAN2_PHASE` (0x3c), channel 2 being
//!   otherwise unused: 0 after a long stop, about 2,440, 4,800, 6,630 and
//!   9,100 rpm at 25, 50, 75 and 100% duty.
//!
//! It writes `CLK_PWM1_CTRL`, GPIO 45's `CTRL`, PWM1's `GLOBAL_CTRL` (only
//! channel 3's enable and the update), channel 3's `CTRL`, `RANGE` and
//! `DUTY`, and `INTR`'s `CHAN3_RELOAD`; nothing else on the three pages.
//!
//! [`Curve`] is the device tree's cooling map, for a fan PD to follow.

use safe_mmio::UniqueMmioPointer;

use crate::{Device, HwError};

/// Channel 3's range: the device tree's 41,566 ns period at PWM1's 50 MHz,
/// 24.06 kHz. A duty of `RANGE` is full speed, 0 stops the fan.
pub const RANGE: u32 = 2078;

// ---- RP1's clocks_main page -------------------------------------------------

/// `CLK_PWM1_CTRL`.
const CLK_PWM1_CTRL: usize = 0x84;
/// `CLK_PWM1_DIV_INT`.
const CLK_PWM1_DIV_INT: usize = 0x88;
/// `CLK_PWM1_DIV_FRAC`.
const CLK_PWM1_DIV_FRAC: usize = 0x8c;
/// `CLK_PWM1_CTRL`'s `ENABLE`.
const CLK_ENABLE: u32 = 1 << 11;
/// `CLK_PWM1_CTRL`'s auxiliary source field, bits 9:5.
const CLK_AUXSRC: u32 = 0x1f << 5;
/// The auxiliary source 2: the 50 MHz crystal.
const CLK_AUXSRC_XOSC: u32 = 2 << 5;

// ---- RP1's io_bank2 page ----------------------------------------------------

/// GPIO 45's `STATUS`: bank 2's pin 11, 8 bytes a pin.
const GPIO45_STATUS: usize = 11 * 8;
/// GPIO 45's `CTRL`.
const GPIO45_CTRL: usize = 11 * 8 + 4;
/// `CTRL`'s `FUNCSEL`, bits 4:0, and the overrides `OUTOVER`, `OEOVER` and
/// `INOVER`, bits 13:12, 15:14 and 17:16: cleared, the pin is function 0,
/// PWM1's channel 3, driven as the PWM drives it.
const GPIO_FUNCSEL_AND_OVERRIDES: u32 = 0x1f | 0x3f << 12;
/// `STATUS`'s `OETOPAD`: the pad is driven.
const STATUS_OE_TO_PAD: u32 = 1 << 13;

// ---- RP1's PWM1 page --------------------------------------------------------

/// `GLOBAL_CTRL`.
const GLOBAL_CTRL: usize = 0x00;
/// `CHAN2_RANGE`: the fastest speed RP1's firmware has seen, in rpm.
const CHAN2_RANGE: usize = 0x38;
/// `CHAN2_PHASE`: the fan's speed as RP1's firmware measures it, in rpm.
const CHAN2_PHASE: usize = 0x3c;
/// `CHAN3_CTRL`.
const CHAN3_CTRL: usize = 0x44;
/// `CHAN3_RANGE`.
const CHAN3_RANGE: usize = 0x48;
/// `CHAN3_DUTY`.
const CHAN3_DUTY: usize = 0x50;
/// `INTR`, raw interrupts; `CHAN3_RELOAD` is write-1-to-clear.
const INTR: usize = 0x54;
/// `GLOBAL_CTRL`'s `SET_UPDATE`: apply the channel settings, self-clearing.
const SET_UPDATE: u32 = 1 << 31;
/// `GLOBAL_CTRL`'s `CHAN3_EN`.
const CHAN3_EN: u32 = 1 << 3;
/// `GLOBAL_CTRL`'s four channel enables.
const CHAN_ENABLES: u32 = 0xf;
/// Channel 3's `CTRL`: `MODE` 1, trailing-edge PWM; `INVERT`, as the device
/// tree's polarity asks, so a larger duty is a faster fan; `FIFO_POP_MASK`
/// as at reset; its own range and duty, no FIFO.
const CHAN3_TRAILING_EDGE_INVERTED: u32 = 0x109;
/// `INTR`'s `CHAN3_RELOAD`: channel 3's counter wrapped.
const CHAN3_RELOAD: u32 = 1 << 8;

/// The fan, running on PWM1's channel 3. Get one from [`start`].
#[derive(Debug)]
pub struct Fan {
    pwm: UniqueMmioPointer<'static, [u32]>,
    duty: u32,
}

/// What [`start`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    /// PWM1's clock was already running (another program set it up).
    pub clock_was_running: bool,
    /// GPIO 45's pad is driven: the PWM reaches the fan connector.
    pub pin_driven: bool,
}

/// Starts the fan at full speed: PWM1's clock on from the crystal, GPIO 45
/// handed to PWM1's channel 3, the channel ranged at [`RANGE`] with a duty
/// of [`RANGE`], and enabled. Consumes RP1's `clocks_main` page (0x4001_8000),
/// `io_bank2` (0x400d_8000) and PWM1 (0x4009_c000); keeps the last.
///
/// # Errors
///
/// Any error of viewing a page; [`HwError::Fan`] for a page too small, or
/// for a clock divider that is not the 1 the firmware leaves.
pub fn start(clocks: Device, io_bank2: Device, pwm: Device) -> Result<(Fan, Started), HwError> {
    let mut clocks = clocks.view_words()?;
    let mut io = io_bank2.view_words()?;
    let mut pwm = pwm.view_words()?;
    if read(&mut clocks, CLK_PWM1_DIV_INT)? != 1 || read(&mut clocks, CLK_PWM1_DIV_FRAC)? != 0 {
        return Err(HwError::Fan {
            reason: "PWM1's clock divider is not 1",
        });
    }
    let clock = read(&mut clocks, CLK_PWM1_CTRL)?;
    let clock_was_running = clock & CLK_ENABLE != 0;
    if !clock_was_running {
        // The source first, the clock still off; then on.
        let source = (clock & !CLK_AUXSRC) | CLK_AUXSRC_XOSC;
        write(&mut clocks, CLK_PWM1_CTRL, source)?;
        write(&mut clocks, CLK_PWM1_CTRL, source | CLK_ENABLE)?;
    }
    write(&mut pwm, CHAN3_CTRL, CHAN3_TRAILING_EDGE_INVERTED)?;
    write(&mut pwm, CHAN3_RANGE, RANGE)?;
    write(&mut pwm, CHAN3_DUTY, RANGE)?;
    let enables = read(&mut pwm, GLOBAL_CTRL)? & CHAN_ENABLES;
    write(&mut pwm, GLOBAL_CTRL, enables | CHAN3_EN | SET_UPDATE)?;
    write(&mut pwm, INTR, CHAN3_RELOAD)?;
    let ctrl = read(&mut io, GPIO45_CTRL)?;
    write(&mut io, GPIO45_CTRL, ctrl & !GPIO_FUNCSEL_AND_OVERRIDES)?;
    let pin_driven = read(&mut io, GPIO45_STATUS)? & STATUS_OE_TO_PAD != 0;
    Ok((
        Fan { pwm, duty: RANGE },
        Started {
            clock_was_running,
            pin_driven,
        },
    ))
}

impl Fan {
    /// Sets the duty, 0 (stopped) to [`RANGE`] (full speed); more is taken
    /// as [`RANGE`]. It takes effect at the channel's next wrap.
    ///
    /// # Errors
    ///
    /// [`HwError::Fan`] for a page too small (never, once started).
    pub fn set_duty(&mut self, duty: u32) -> Result<(), HwError> {
        let duty = duty.min(RANGE);
        write(&mut self.pwm, CHAN3_DUTY, duty)?;
        self.duty = duty;
        Ok(())
    }

    /// The duty last set.
    #[must_use]
    pub const fn duty(&self) -> u32 {
        self.duty
    }

    /// The fan's speed in rpm, as RP1's firmware measured it last.
    ///
    /// # Errors
    ///
    /// [`HwError::Fan`] for a page too small (never, once started).
    pub fn rpm(&mut self) -> Result<u32, HwError> {
        read(&mut self.pwm, CHAN2_PHASE)
    }

    /// The fastest speed RP1's firmware has measured, in rpm.
    ///
    /// # Errors
    ///
    /// [`HwError::Fan`] for a page too small (never, once started).
    pub fn fastest_rpm(&mut self) -> Result<u32, HwError> {
        read(&mut self.pwm, CHAN2_RANGE)
    }

    /// Whether channel 3 has wrapped since the last call (or [`start`]):
    /// whether PWM1's clock runs. Clears the flag.
    ///
    /// # Errors
    ///
    /// [`HwError::Fan`] for a page too small (never, once started).
    pub fn counting(&mut self) -> Result<bool, HwError> {
        let wrapped = read(&mut self.pwm, INTR)? & CHAN3_RELOAD != 0;
        write(&mut self.pwm, INTR, CHAN3_RELOAD)?;
        Ok(wrapped)
    }
}

// ---- The cooling curve ------------------------------------------------------

/// The device tree's `cpu-thermal` trip points, in thousandths of a degree:
/// tepid, warm, hot and very hot. Each turns the fan up a level.
pub const TRIPS: [i32; 4] = [50_000, 60_000, 67_500, 75_000];

/// Each trip's hysteresis: a level is left only below its trip less this.
pub const HYSTERESIS: i32 = 5_000;

/// The device tree's `cooling-levels`, out of 255: off, then the four trips'.
pub const LEVELS: [u32; 5] = [0, 75, 125, 175, 250];

/// The fan's level, 0 (off) to 4, following the temperature as the device
/// tree's cooling map does: a trip passed turns it up, and it comes down a
/// level only once the temperature is [`HYSTERESIS`] below that trip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Curve {
    level: usize,
}

impl Curve {
    /// Starts at level 0.
    #[must_use]
    pub const fn new() -> Self {
        Self { level: 0 }
    }

    /// The level for a temperature, in thousandths of a degree.
    pub fn update(&mut self, millidegrees: i32) -> usize {
        while let Some(trip) = TRIPS.get(self.level) {
            if millidegrees < *trip {
                break;
            }
            self.level = self.level.saturating_add(1);
        }
        while let Some(trip) = self.level.checked_sub(1).and_then(|below| TRIPS.get(below)) {
            if millidegrees >= trip.saturating_sub(HYSTERESIS) {
                break;
            }
            self.level = self.level.saturating_sub(1);
        }
        self.level
    }

    /// The level now.
    #[must_use]
    pub const fn level(&self) -> usize {
        self.level
    }

    /// A level's duty, out of [`RANGE`].
    #[must_use]
    pub fn duty(level: usize) -> u32 {
        let of_255 = LEVELS.get(level).copied().unwrap_or(255);
        of_255.saturating_mul(RANGE) / 255
    }
}

// ---- Word access ------------------------------------------------------------

fn word(at: usize) -> Result<usize, HwError> {
    if at.is_multiple_of(4) {
        Ok(at / 4)
    } else {
        Err(HwError::Fan {
            reason: "a register off a word",
        })
    }
}

fn read(regs: &mut UniqueMmioPointer<'static, [u32]>, at: usize) -> Result<u32, HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Fan {
        reason: "a page too small",
    })?;
    // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
    // page (`get` checked the index), of one of this module's registers -
    // PWM1's clock control and dividers, GPIO 45's STATUS and CTRL, PWM1's
    // GLOBAL_CTRL, INTR and channels 2 and 3 - none of which changes state
    // when read (RP1 Peripherals tables 7, 8 and 26 to 47; the clock's on
    // the board: read twice, the same).
    Ok(unsafe { register.read_unsafe() })
}

fn write(
    regs: &mut UniqueMmioPointer<'static, [u32]>,
    at: usize,
    value: u32,
) -> Result<(), HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Fan {
        reason: "a page too small",
    })?;
    // SAFETY: `write_unsafe` needs a write that is safe to make: inside the
    // page (`get`), and only ever one of this module's writes - PWM1's clock
    // on, GPIO 45 handed to PWM1, PWM1's channel 3 set up and enabled, its
    // wrap flag cleared - a clock and a pin's waveform, which start no DMA
    // and touch no memory.
    unsafe { register.write_unsafe(value) };
    Ok(())
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

    fn page(words: &'static [AtomicU32]) -> Device {
        Device::for_test(words.as_ptr().expose_provenance(), words.len() * 4)
    }

    fn leak(fill: impl Fn(usize) -> u32) -> &'static [AtomicU32] {
        Vec::leak((0..1024).map(|at| AtomicU32::new(fill(at * 4))).collect())
    }

    #[test]
    fn start_turns_the_clock_on_and_runs_channel_3_at_full_speed() {
        // As the firmware leaves them (U-Boot's prompt, 2026-10-16).
        let clocks = leak(|at| match at {
            CLK_PWM1_CTRL => 0x0100_0000,
            CLK_PWM1_DIV_INT | 0x90 => 1,
            _ => 0,
        });
        let io = leak(|at| match at {
            GPIO45_STATUS => 0x0abe_3300,
            GPIO45_CTRL => 0x85,
            _ => 0x9f,
        });
        let pwm = leak(|at| match at {
            GLOBAL_CTRL => 0x2, // channel 1 on: left so
            CHAN2_PHASE => 4800,
            CHAN3_CTRL | 0x14 | 0x24 | 0x34 => 0x100,
            INTR => 0x114,
            _ => 0,
        });
        let (mut fan, started) = start(page(clocks), page(io), page(pwm)).unwrap();
        assert!(!started.clock_was_running);
        assert!(started.pin_driven);
        assert_eq!(
            clocks[CLK_PWM1_CTRL / 4].load(Ordering::Relaxed),
            0x0100_0840
        );
        assert_eq!(
            io[GPIO45_CTRL / 4].load(Ordering::Relaxed),
            0x80,
            "funcsel 0, filter kept"
        );
        assert_eq!(pwm[CHAN3_CTRL / 4].load(Ordering::Relaxed), 0x109);
        assert_eq!(pwm[CHAN3_RANGE / 4].load(Ordering::Relaxed), RANGE);
        assert_eq!(pwm[CHAN3_DUTY / 4].load(Ordering::Relaxed), RANGE);
        assert_eq!(pwm[GLOBAL_CTRL / 4].load(Ordering::Relaxed), 0x8000_000a);
        assert_eq!(fan.rpm().unwrap(), 4800);
        fan.set_duty(RANGE + 1).unwrap();
        assert_eq!(fan.duty(), RANGE);
        fan.set_duty(1039).unwrap();
        assert_eq!(pwm[CHAN3_DUTY / 4].load(Ordering::Relaxed), 1039);
        // Nothing else on the pages touched.
        for (at, word) in io.iter().enumerate() {
            if at * 4 != GPIO45_CTRL && at * 4 != GPIO45_STATUS {
                assert_eq!(word.load(Ordering::Relaxed), 0x9f, "io word {at}");
            }
        }
    }

    #[test]
    fn a_running_clock_is_left_as_it_is() {
        let clocks = leak(|at| match at {
            CLK_PWM1_CTRL => 0x1100_0840,
            CLK_PWM1_DIV_INT => 1,
            _ => 0,
        });
        let (_, started) = start(page(clocks), page(leak(|_| 0)), page(leak(|_| 0))).unwrap();
        assert!(started.clock_was_running);
        assert_eq!(
            clocks[CLK_PWM1_CTRL / 4].load(Ordering::Relaxed),
            0x1100_0840
        );
    }

    #[test]
    fn a_divider_not_1_is_refused_before_anything_is_written() {
        let clocks = leak(|at| if at == CLK_PWM1_DIV_INT { 4 } else { 0 });
        let pwm = leak(|_| 0);
        let error = start(page(clocks), page(leak(|_| 0)), page(pwm)).unwrap_err();
        assert!(matches!(error, HwError::Fan { .. }));
        assert!(pwm.iter().all(|word| word.load(Ordering::Relaxed) == 0));
    }

    #[test]
    fn the_curve_follows_the_trips_up_and_their_hysteresis_down() {
        let mut curve = Curve::new();
        assert_eq!(curve.update(45_000), 0);
        assert_eq!(curve.update(50_000), 1);
        assert_eq!(curve.update(46_000), 1, "within the hysteresis");
        assert_eq!(curve.update(44_999), 0);
        assert_eq!(curve.update(80_000), 4, "every trip passed at once");
        assert_eq!(curve.update(70_001), 4);
        assert_eq!(curve.update(69_999), 3);
        assert_eq!(curve.update(20_000), 0, "every level left at once");
        assert_eq!(curve.update(i32::MIN), 0);
        assert_eq!(curve.update(i32::MAX), 4);
    }

    #[test]
    fn levels_scale_to_the_range() {
        assert_eq!(Curve::duty(0), 0);
        assert_eq!(Curve::duty(1), 75 * RANGE / 255);
        assert_eq!(Curve::duty(4), 250 * RANGE / 255);
        assert_eq!(Curve::duty(9), RANGE);
    }
}
