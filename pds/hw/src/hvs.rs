// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where the Raspberry Pi 5's display is in its scan: the line the
//! display hardware is on, read-only, so the display PD can copy a frame's
//! rectangle into the framebuffer while the scan is elsewhere, and a moving
//! window does not tear.
//!
//! The scan is BCM2712's HVS, the hardware that reads the framebuffer and
//! feeds the HDMI output, which the firmware sets up and drives; nothing
//! here writes to it. Broadcom documents none of it, and the firmware has
//! no "wait for vertical sync" call on the Pi 5 (its property tag
//! 0x0004_800e goes unanswered: tried from U-Boot's prompt, 2026-10-21).
//! The facts here are from two sources:
//!
//! * the pinned firmware's device tree (`platforms/rpi5b_2gb.toml`, `hvs`):
//!   `/hvs@107c580000`, `brcm,bcm2712-hvs`, 0x1_a000 bytes of registers;
//! * the board itself, read from U-Boot's prompt on 2026-10-21 with the
//!   desk's monitor at 1920 x 1080, 60 Hz, and no other source: the page
//!   holds three display channels, 0x40 bytes apart from 0x100. A channel's
//!   first word reads 0x877f_0437 for the monitor's (bit 31 set, bits
//!   27:16 1919, bits 11:0 1079: its size less one) and 0x827f_01df for the
//!   second, unconnected HDMI output's 640 x 480; the third reads 0. The
//!   word at 0x18 into a channel changes from read to read: sixty reads of
//!   the monitor's gave bits 21:16 counting up by 6 or 7 a read and
//!   wrapping at 64 (frames, at 60 Hz: a read took about 117 ms) and bits
//!   11:0 anywhere from 8 to 1074 (the line); bit 14 was always set. The
//!   pixel valve the channel feeds (`pixelvalve@7c410000` in the device
//!   tree) holds 1080 active lines, 4 front porch, 5 sync and 36 back
//!   porch: 1125 lines a frame.
//!
//! Reading either word changed nothing on the monitor, read again and
//! again from U-Boot's prompt: the view reads only those two words of each
//! channel and never writes. Which channel feeds the monitor is found by
//! its size ([`Beam::new`]), not assumed.

use core::mem::{offset_of, size_of};

use safe_mmio::fields::ReadPure;
use safe_mmio::{UniqueMmioPointer, field_shared};

use crate::device::RegisterBlock;
use crate::{Device, HwError};

/// Display channels in the page.
pub const CHANNELS: usize = 3;

/// A channel's first word: bit 31, the channel enabled.
const ENABLED: u32 = 1 << 31;
/// Its width less one, bits 27:16.
const WIDTH_SHIFT: u32 = 16;
/// Its height less one, bits 11:0.
const SIZE_MASK: u32 = 0xfff;
/// A channel's status word: the frame, bits 21:16, counting at the refresh
/// rate and wrapping at 64.
const FRAME_SHIFT: u32 = 16;
const FRAME_MASK: u32 = 0x3f;
/// The line, bits 11:0.
const LINE_MASK: u32 = 0xfff;

/// One display channel's words, as read on the board.
#[repr(C)]
pub(crate) struct ChannelRegisters {
    /// 0x00: enabled (bit 31), width less one (27:16), height less one (11:0).
    control: ReadPure<u32>,
    /// 0x04-0x14: not read.
    _reserved_04: [u32; 5],
    /// 0x18: the frame (21:16) and the line (11:0) the scan is on.
    status: ReadPure<u32>,
    /// 0x1c-0x3c: not read.
    _reserved_1c: [u32; 9],
}

/// The HVS's first page: the three display channels from 0x100. The rest is
/// padding, never accessed.
#[repr(C)]
pub(crate) struct Registers {
    /// 0x000-0x0fc: not read.
    _reserved_000: [u32; 64],
    /// 0x100-0x1bc: the display channels.
    channels: [ChannelRegisters; CHANNELS],
    /// 0x1c0-0xffc: not read.
    _reserved_1c0: [u32; 912],
}

// Every offset the view uses, as found on the board: a layout mistake fails
// the build.
const _: () = {
    assert!(offset_of!(ChannelRegisters, control) == 0x00);
    assert!(offset_of!(ChannelRegisters, status) == 0x18);
    assert!(size_of::<ChannelRegisters>() == 0x40);
    assert!(offset_of!(Registers, channels) == 0x100);
    assert!(size_of::<Registers>() == 0x1000);
};

// SAFETY: `repr(C)`; every word the view reads at the offset found on the
// board (asserted above) and 32 bits wide. Reading a channel's first word
// or its status word has no side effect (`ReadPure`): read again and again
// from U-Boot's prompt with the monitor showing, nothing on it changed. No
// field can be written, and the block's DMA - its reads of the framebuffer
// - is the firmware's to set up, through words the view never reaches.
unsafe impl RegisterBlock for Registers {}

/// The size a channel's first word gives, if the channel is enabled:
/// width, height.
#[must_use]
pub const fn channel_size(control: u32) -> Option<(u32, u32)> {
    if control & ENABLED == 0 {
        return None;
    }
    let width = ((control >> WIDTH_SHIFT) & SIZE_MASK).saturating_add(1);
    let height = (control & SIZE_MASK).saturating_add(1);
    Some((width, height))
}

/// Where the scan is, from a channel's status word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanPosition {
    /// The line, from 0 at the top.
    pub line: u32,
    /// The frame, counting at the refresh rate and wrapping at 64.
    pub frame: u32,
}

/// The position a channel's status word gives.
#[must_use]
pub const fn position_of(status: u32) -> ScanPosition {
    ScanPosition {
        line: status & LINE_MASK,
        frame: (status >> FRAME_SHIFT) & FRAME_MASK,
    }
}

/// Whether a scan on `line` is among `rows` - from `top` to before
/// `bottom` - or within `margin` lines above them, so that copying into
/// them now could show part of the old rows and part of the new in one
/// frame. A copy started anywhere else, going down at least as fast as the
/// scan, stays ahead of it.
#[must_use]
pub const fn in_the_way(line: u32, top: u32, bottom: u32, margin: u32) -> bool {
    line.saturating_add(margin) >= top && line < bottom
}

/// Whether a scan on `line` has just passed row `top`: no more than
/// `slack` lines below it. A copy slower than the scan, started from `top`
/// now, follows it down and is not caught until the scan comes round again,
/// once the copy has written as many rows as its speed allows in one
/// refresh.
#[must_use]
pub const fn just_past(line: u32, top: u32, slack: u32) -> bool {
    line >= top && line < top.saturating_add(slack)
}

/// How many lines the scan, on `line`, goes down to reach `target`, of
/// the `lines` it reports a frame before it wraps to the top.
#[must_use]
pub const fn lines_until(line: u32, target: u32, lines: u32) -> u32 {
    if target >= line {
        target.saturating_sub(line)
    } else {
        lines.saturating_sub(line).saturating_add(target)
    }
}

/// How a wait for the scan ended ([`wait_until`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The scan was where it was wanted at once.
    AtOnce,
    /// The scan was where it was wanted after a wait.
    Waited,
    /// The scan was not there by the deadline.
    Late,
}

/// Waits until the scan, whose line `line` reads, is somewhere `ready`
/// accepts - clear of some rows ([`in_the_way`]) or just past their top
/// ([`just_past`]) - or until `now` reads `deadline` or later: a scan that
/// stops, or a counter that never moves on, delays a copy by a bounded
/// time, never forever.
///
/// Between two reads of the line it watches only `now`, for at least one
/// tick and as many as `rest` gives for the line last read: the CPU's
/// counter costs the bus nothing, and the display hardware, which the
/// firmware drives too, is read a few times a wait, not millions of times
/// a second. Read without a pause, for minutes on end, the firmware
/// stopped answering its mailbox (twice, 2026-10-21).
pub fn wait_until(
    mut line: impl FnMut() -> u32,
    mut now: impl FnMut() -> u64,
    ready: impl Fn(u32) -> bool,
    rest: impl Fn(u32) -> u64,
    deadline: u64,
) -> Wait {
    let mut seen = line();
    if ready(seen) {
        return Wait::AtOnce;
    }
    loop {
        let until = now().saturating_add(rest(seen).max(1)).min(deadline);
        loop {
            let at = now();
            if at >= deadline {
                return Wait::Late;
            }
            if at >= until {
                break;
            }
            core::hint::spin_loop();
        }
        seen = line();
        if ready(seen) {
            return Wait::Waited;
        }
    }
}

/// A read-only view of the display channel that scans out the board's
/// framebuffer. Make one with [`Beam::new`].
#[derive(Debug)]
pub struct Beam {
    regs: UniqueMmioPointer<'static, Registers>,
    channel: usize,
}

impl Beam {
    /// Views `device` as the HVS's first page and finds the enabled channel
    /// whose size is `width` x `height`, the framebuffer's.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::Scan`] if no enabled
    /// channel is the framebuffer's size: no monitor, or another mode.
    pub fn new(device: Device, width: u32, height: u32) -> Result<Self, HwError> {
        let regs: UniqueMmioPointer<'static, Registers> = device.view()?;
        let channel = (0..CHANNELS)
            .find(|&index| {
                field_shared!(regs, channels)
                    .get(index)
                    .map(|channel| channel_size(field_shared!(channel, control).read()))
                    == Some(Some((width, height)))
            })
            .ok_or(HwError::Scan {
                reason: "no display channel is scanning out a screen of the framebuffer's size",
            })?;
        Ok(Self { regs, channel })
    }

    /// The channel scanning out the framebuffer, 0 to 2.
    #[must_use]
    pub const fn channel(&self) -> usize {
        self.channel
    }

    /// Where the scan is now.
    #[must_use]
    pub fn position(&self) -> ScanPosition {
        let status = field_shared!(self.regs, channels)
            .get(self.channel)
            .map_or(0, |channel| field_shared!(channel, status).read());
        position_of(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monitors_channel_reads_as_1920_by_1080_and_the_idle_ones_as_theirs() {
        assert_eq!(channel_size(0x877f_0437), Some((1920, 1080)));
        assert_eq!(channel_size(0x827f_01df), Some((640, 480)));
        assert_eq!(channel_size(0), None);
        assert_eq!(channel_size(0x077f_0437), None, "bit 31 clear: not enabled");
    }

    #[test]
    fn a_status_word_gives_the_line_and_the_frame() {
        assert_eq!(
            position_of(0x0096_41c5),
            ScanPosition {
                line: 453,
                frame: 22
            }
        );
        assert_eq!(
            position_of(0x00b4_43c6),
            ScanPosition {
                line: 966,
                frame: 52
            }
        );
        // Bit 23 was clear in two of sixty reads on the board: not the frame.
        assert_eq!(
            position_of(0x0029_42e7),
            ScanPosition {
                line: 743,
                frame: 41
            }
        );
        assert_eq!(position_of(0x00a9_42e7).frame, 41);
    }

    /// A counter going up by one each time it is read.
    fn ticking() -> impl FnMut() -> u64 {
        let mut clock = 0_u64;
        move || {
            clock = clock.saturating_add(1);
            clock
        }
    }

    #[test]
    fn a_copy_waits_for_the_scan_to_leave_its_rows_and_no_longer() {
        // Clear at once: the clock is never read.
        let mut asked = 0_u32;
        let clear = |line| !in_the_way(line, 100, 200, 8);
        let wait = wait_until(
            || 500,
            || {
                asked = asked.saturating_add(1);
                0
            },
            clear,
            |_| 1,
            10,
        );
        assert_eq!((wait, asked), (Wait::AtOnce, 0));
        // The scan on the rows, then past them.
        let mut lines = [150, 180, 199, 200].into_iter();
        let wait = wait_until(|| lines.next().unwrap_or(0), ticking(), clear, |_| 1, 100);
        assert_eq!(wait, Wait::Waited);
    }

    #[test]
    fn a_slow_copy_waits_for_the_scan_to_pass_its_top_row() {
        // Rows from 300, a slack of 32: the scan in blank, at the top, then just past 300.
        let mut lines = [1080, 0, 150, 299, 310].into_iter();
        let wait = wait_until(
            || lines.next().unwrap_or(0),
            ticking(),
            |line| just_past(line, 300, 32),
            |_| 1,
            100,
        );
        assert_eq!(wait, Wait::Waited);
        assert!(just_past(300, 300, 32));
        assert!(just_past(331, 300, 32));
        assert!(!just_past(332, 300, 32));
        assert!(!just_past(299, 300, 32));
        assert!(
            !just_past(u32::MAX, u32::MAX, 32),
            "no overflow at the counter's top"
        );
    }

    #[test]
    fn a_scan_that_never_moves_on_is_copied_past_at_the_deadline() {
        let mut clock = 0_u64;
        let wait = wait_until(
            || 150,
            || {
                clock = clock.saturating_add(1);
                clock
            },
            |line| !in_the_way(line, 100, 200, 8),
            |_| 7,
            1000,
        );
        assert_eq!(wait, Wait::Late);
        assert_eq!(
            clock, 1000,
            "read until it reached the deadline, and no further"
        );
    }

    #[test]
    fn between_reads_only_the_counter_is_watched() {
        // A scan of 1125 lines, ten ticks a line, from line 0; a copy wanting it just past
        // row 900. Resting three quarters of the way each time, the line is read a few times,
        // not once a tick.
        let clock = core::cell::Cell::new(0_u64);
        let mut reads = 0_u32;
        let wait = wait_until(
            || {
                reads = reads.saturating_add(1);
                let line = clock
                    .get()
                    .checked_div(10)
                    .and_then(|line| line.checked_rem(1125));
                line.and_then(|line| u32::try_from(line).ok()).unwrap_or(0)
            },
            || {
                clock.set(clock.get().saturating_add(1));
                clock.get()
            },
            |line| just_past(line, 900, 32),
            |line| {
                let away = lines_until(line, 900, 1125)
                    .saturating_mul(3)
                    .checked_div(4);
                u64::from(away.unwrap_or(0)).saturating_mul(10)
            },
            1_000_000,
        );
        assert_eq!(wait, Wait::Waited);
        assert!(reads <= 20, "{reads} reads of the line");
        assert!(
            clock.get() >= 9000 && clock.get() < 9320,
            "copied at {}",
            clock.get()
        );
    }

    #[test]
    fn the_lines_to_go_wrap_at_the_frames_end() {
        assert_eq!(lines_until(100, 200, 1081), 100);
        assert_eq!(lines_until(300, 300, 1081), 0);
        assert_eq!(lines_until(500, 100, 1081), 681);
        // In vertical blank, the line reads 1080.
        assert_eq!(lines_until(1080, 200, 1081), 201);
        assert_eq!(lines_until(u32::MAX, 0, 1081), 0, "no overflow");
    }

    #[test]
    fn the_scan_is_in_the_way_only_on_the_rows_or_just_above_them() {
        // Rows 100 to 199, a margin of 8.
        assert!(!in_the_way(0, 100, 200, 8));
        assert!(!in_the_way(91, 100, 200, 8));
        assert!(in_the_way(92, 100, 200, 8));
        assert!(in_the_way(150, 100, 200, 8));
        assert!(in_the_way(199, 100, 200, 8));
        assert!(!in_the_way(200, 100, 200, 8));
        assert!(!in_the_way(1124, 100, 200, 8));
        // From the top: only the rows themselves and none above.
        assert!(in_the_way(0, 0, 1080, 8));
        assert!(!in_the_way(1080, 0, 1080, 8));
        // No overflow at the counter's top.
        assert!(!in_the_way(u32::MAX, 0, 1080, 8));
    }
}
