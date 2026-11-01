// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system timer of Broadcom's BCM2835 family - BCM2712's, on the
//! Raspberry Pi 5: its register block and a safe, typed view of it.
//!
//! Written from Broadcom's BCM2835 ARM Peripherals (2012): chapter 12,
//! System Timer (the registers CS, CLO, CHI and C0 to C3, and how a match
//! works), and chapter 7, Interrupts, whose table gives the ARM system timer
//! matches 1 and 3 only. No other source. BCM2712 has no public
//! peripherals datasheet; its device tree names this block compatible with
//! the BCM2835's (`brcm,bcm2835-system-timer`, at 1 MHz), and the board file
//! (`platforms/rpi5b_2gb.toml`, `system_timer`) gives its window and
//! interrupts with their source.
//!
//! The block is a free-running 64-bit counter at [`FREQUENCY`] - CLO, its
//! low word, and CHI, its high word, both read only - and four 32-bit
//! compare channels. When CLO comes to equal a channel's compare value, the
//! channel's match bit in CS is set and its interrupt asserted, until
//! software writes 1 to that bit; writing 0 to a bit leaves it. A channel
//! cannot be disabled: its compare register always compares.
//!
//! Channels 0 and 2 belong to the VideoCore's firmware, and share this
//! page and CS with the ARM's, so this view reaches channels 1 and 3 only
//! ([`Channel`]): C0 and C2 are padding, never accessed, and clearing a
//! match writes that channel's bit alone. Reading CS, CLO, CHI or a compare
//! register has no side effect.
//!
//! The block has no identification registers, so nothing is checked before
//! a view is handed out: `tools/check-system` ensures, when the image is
//! built, that the device symbol is bound to the board's `system_timer`
//! window, and the timer PD checks at start-up that the counter moves.

use core::mem::{offset_of, size_of};

use safe_mmio::fields::{ReadPure, ReadPureWrite};
use safe_mmio::{UniqueMmioPointer, field, field_shared};

use crate::device::RegisterBlock;
use crate::{Device, HwError};

/// The counter's frequency: 1 MHz, a tick a microsecond (BCM2835 ARM
/// Peripherals, chapter 12; the Pi 5's device tree, `clock-frequency =
/// <1000000>`).
pub const FREQUENCY: u64 = 1_000_000;

/// The system timer's registers, as chapter 12 lays them out. The
/// VideoCore's compare registers and the rest of the page are padding,
/// never accessed.
#[repr(C)]
pub(crate) struct Registers {
    /// 0x00 CS: match bits M0 to M3 (bits 0 to 3). Writing 1 to a bit clears
    /// that channel's match and its interrupt; 0 leaves it.
    cs: ReadPureWrite<u32>,
    /// 0x04 CLO: the counter's low 32 bits.
    clo: ReadPure<u32>,
    /// 0x08 CHI: the counter's high 32 bits.
    chi: ReadPure<u32>,
    /// 0x0C C0: the VideoCore's channel 0.
    _c0: u32,
    /// 0x10 C1: channel 1's compare value.
    c1: ReadPureWrite<u32>,
    /// 0x14 C2: the VideoCore's channel 2.
    _c2: u32,
    /// 0x18 C3: channel 3's compare value.
    c3: ReadPureWrite<u32>,
    /// 0x1C-0xFFC: nothing.
    _reserved_01c: [u32; 1017],
}

// Every offset the view uses, as chapter 12 gives it: a layout mistake fails
// the build.
const _: () = {
    assert!(offset_of!(Registers, cs) == 0x00);
    assert!(offset_of!(Registers, clo) == 0x04);
    assert!(offset_of!(Registers, chi) == 0x08);
    assert!(offset_of!(Registers, _c0) == 0x0C);
    assert!(offset_of!(Registers, c1) == 0x10);
    assert!(offset_of!(Registers, _c2) == 0x14);
    assert!(offset_of!(Registers, c3) == 0x18);
    assert!(size_of::<Registers>() == 0x1000);
};

// SAFETY: `repr(C)`; every register at the offset chapter 12 gives (asserted
// above) and 32 bits wide. The wrappers match the datasheet: reading CS,
// CLO, CHI or a compare register has no side effect (`ReadPure`,
// `ReadPureWrite`); writing CS clears match bits and writing a compare
// register sets when its interrupt asserts - neither touches memory, so
// both are safe at any time. The block does no DMA. The VideoCore's compare
// registers are padding, never accessed.
unsafe impl RegisterBlock for Registers {}

/// One of the ARM's compare channels: the only ones this view reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Channel 1: match bit M1, compare register C1, the second of the
    /// block's interrupts.
    One,
    /// Channel 3: match bit M3, compare register C3, the fourth of the
    /// block's interrupts.
    Three,
}

impl Channel {
    /// The channel's number, 1 or 3: its bit in CS and its index among the
    /// block's interrupts.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::One => 1,
            Self::Three => 3,
        }
    }

    /// The channel's match bit in CS.
    const fn bit(self) -> u32 {
        1 << self.number()
    }
}

/// The compare value was set, but the counter may have passed it before it
/// took effect: its match would then come only when the counter's low word
/// comes round again, about 71.6 minutes later. Arm again with a longer
/// delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missed {
    /// The compare value written.
    pub compare: u32,
    /// The counter's low word just after it was written.
    pub after: u32,
}

/// A safe view of the system timer's registers. Make one with
/// [`SystemTimer::new`].
#[derive(Debug)]
pub struct SystemTimer<'a> {
    regs: UniqueMmioPointer<'a, Registers>,
}

impl SystemTimer<'static> {
    /// Views `device` as the system timer.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device: it does not fit the block's 4 KiB,
    /// say.
    pub fn new(device: Device) -> Result<Self, HwError> {
        Ok(Self {
            regs: device.view()?,
        })
    }
}

impl<'a> SystemTimer<'a> {
    /// The counter's low word: CLO.
    #[must_use]
    pub fn low(&self) -> u32 {
        field_shared!(self.regs, clo).read()
    }

    /// The whole 64-bit counter, CHI and CLO read so that they belong
    /// together: CHI, then CLO, then CHI again; if CHI moved on in between,
    /// CLO is read again after the second CHI, which cannot move again for
    /// another 2^32 ticks.
    #[must_use]
    pub fn counter(&self) -> u64 {
        let high = field_shared!(self.regs, chi).read();
        let low = self.low();
        let again = field_shared!(self.regs, chi).read();
        let low = if again == high { low } else { self.low() };
        (u64::from(again) << 32) | u64::from(low)
    }

    /// CS, the match bits of every channel, the VideoCore's included: for a
    /// report; reading it changes nothing.
    #[must_use]
    pub fn status(&self) -> u32 {
        field_shared!(self.regs, cs).read()
    }

    /// Whether `channel`'s match bit is set: the counter reached its compare
    /// value and the match has not been cleared.
    #[must_use]
    pub fn matched(&self, channel: Channel) -> bool {
        self.status() & channel.bit() != 0
    }

    /// Clears `channel`'s match, and so its interrupt: writes its bit, and
    /// only its bit, to CS.
    pub fn clear_match(&mut self, channel: Channel) {
        field!(self.regs, cs).write(channel.bit());
    }

    /// `channel`'s compare value.
    #[must_use]
    pub fn compare(&self, channel: Channel) -> u32 {
        match channel {
            Channel::One => field_shared!(self.regs, c1).read(),
            Channel::Three => field_shared!(self.regs, c3).read(),
        }
    }

    /// Sets `channel`'s compare value: its match comes when CLO next equals
    /// `value`.
    pub fn set_compare(&mut self, channel: Channel, value: u32) {
        match channel {
            Channel::One => field!(self.regs, c1).write(value),
            Channel::Three => field!(self.regs, c3).write(value),
        }
    }

    /// Arms `channel` to match `delay` ticks from now: reads CLO, sets the
    /// compare value `delay` ticks on, reads it back (so the write has
    /// reached the block) and reads CLO again. Returns the compare value.
    ///
    /// # Errors
    ///
    /// [`Missed`] if the counter may have passed the compare value before
    /// it took effect and the channel has not matched (see [`missed`]).
    pub fn arm(&mut self, channel: Channel, delay: u32) -> Result<u32, Missed> {
        let before = self.low();
        let compare = before.wrapping_add(delay);
        self.set_compare(channel, compare);
        let _written = self.compare(channel);
        let after = self.low();
        if missed(before, after, delay, self.matched(channel)) {
            return Err(Missed { compare, after });
        }
        Ok(compare)
    }
}

/// Whether a compare value set `delay` ticks after a counter reading of
/// `before` may have been passed before it took effect, given the reading
/// `after` it was written and whether the channel has `matched` since.
///
/// The write took effect at some reading between `before` and `after`; if
/// fewer than `delay` ticks separate them, the counter was still short of
/// the compare value then, and the match is still to come. Otherwise it may
/// have been passed - unless the channel has matched, when its interrupt is
/// already asserted. A delay of 0 is always missed unless matched.
#[must_use]
pub const fn missed(before: u32, after: u32, delay: u32, matched: bool) -> bool {
    !matched && after.wrapping_sub(before) >= delay
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn a_match_is_missed_only_when_the_counter_may_have_passed_it() {
        // Written well before the compare value.
        assert!(!missed(100, 101, 10, false));
        // As many ticks passed as the delay: the counter may have reached
        // the compare value before it took effect...
        assert!(missed(100, 110, 10, false));
        assert!(missed(100, 200, 10, false));
        // ...unless the channel matched, so its interrupt is asserted.
        assert!(!missed(100, 200, 10, true));
        // Across the low word's wrap.
        assert!(!missed(u32::MAX - 2, 3, 10, false));
        assert!(missed(u32::MAX - 2, 7, 10, false));
        // A zero delay is never armed in time.
        assert!(missed(5, 5, 0, false));
        assert!(Channel::One.bit() == 0b0010 && Channel::Three.bit() == 0b1000);
    }

    #[cfg(not(miri))]
    mod registers {
        //! Ordinary memory stands in for the timer: the view reads and
        //! writes it through safe-mmio's real load and store instructions.
        //! Not under Miri, which cannot run them.

        use super::super::*;

        /// A register block with the counter at `counter` and match bits
        /// `cs`.
        fn timer(counter: u64, cs: u32) -> Registers {
            Registers {
                cs: ReadPureWrite(cs),
                clo: ReadPure(u32::try_from(counter & 0xFFFF_FFFF).unwrap()),
                chi: ReadPure(u32::try_from(counter >> 32).unwrap()),
                _c0: 0xC0C0_C0C0,
                c1: ReadPureWrite(0),
                _c2: 0xC2C2_C2C2,
                c3: ReadPureWrite(0),
                _reserved_01c: [0; 1017],
            }
        }

        fn view(registers: &mut Registers) -> SystemTimer<'_> {
            SystemTimer {
                regs: UniqueMmioPointer::from(registers),
            }
        }

        #[test]
        fn the_counter_reads_as_one_64_bit_value() {
            let mut registers = timer(0x0000_0012_8000_0001, 0);
            let timer = view(&mut registers);
            assert_eq!(timer.counter(), 0x0000_0012_8000_0001);
            assert_eq!(timer.low(), 0x8000_0001);
        }

        #[test]
        fn arming_sets_the_compare_value_delay_ticks_on() {
            let mut registers = timer(u64::from(u32::MAX) - 4, 0);
            let mut timer = view(&mut registers);
            // CLO does not move in ordinary memory, so the match is in time,
            // and the compare value wraps with the low word.
            assert_eq!(timer.arm(Channel::One, 10), Ok(5));
            assert_eq!(timer.compare(Channel::One), 5);
            assert_eq!(timer.arm(Channel::Three, 7), Ok(2));
            assert_eq!(timer.compare(Channel::Three), 2);
            // A zero delay cannot be armed in time.
            assert_eq!(
                timer.arm(Channel::One, 0),
                Err(Missed {
                    compare: u32::MAX - 4,
                    after: u32::MAX - 4
                })
            );
            // The VideoCore's channels are never touched.
            assert_eq!((registers._c0, registers._c2), (0xC0C0_C0C0, 0xC2C2_C2C2));
        }

        #[test]
        fn a_match_is_seen_and_cleared_by_its_own_bit_alone() {
            let mut registers = timer(0, 0b1111);
            let mut timer = view(&mut registers);
            assert!(timer.matched(Channel::One) && timer.matched(Channel::Three));
            assert_eq!(timer.status(), 0b1111);
            timer.clear_match(Channel::One);
            // CS is write-1-to-clear: exactly M1 is written, so M0, M2 and
            // M3 are left as they are.
            assert_eq!(registers.cs.0, 0b0010);
            let mut timer = view(&mut registers);
            timer.clear_match(Channel::Three);
            assert_eq!(registers.cs.0, 0b1000);
        }
    }
}
