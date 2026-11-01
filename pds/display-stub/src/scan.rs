// SPDX-License-Identifier: MIT OR Apache-2.0
//! Copying in step with the display's scan: where the system
//! grants the display hardware's scan position (device 1: the Pi 5's
//! `hvs`), a frame's rectangles are copied into the board's framebuffer,
//! top rectangle first, only when the scan cannot overtake the copy, so a
//! frame never shows half old and half new - a dragged window does not
//! tear:
//!
//! * **Leading** the scan, when the copy goes down the rows faster than the
//!   scan does: once the scan is clear of every row the frame covers, the
//!   copy starts ahead of it and stays ahead.
//! * **Chasing** it, when the copy is slower: once the scan has just passed
//!   the frame's top row, the copy follows it down; the scan pulls away and
//!   comes round again only after the copy has written as many rows as its
//!   speed allows in one refresh.
//!
//! The PD measures how fast it copies, frame by frame, and chases until it
//! knows. Every wait is bounded: a scan that does not come where it is
//! wanted within [`WAIT_AT_MOST_US`] is copied past, and said once. Every
//! [`REPORT_EVERY`] frames the PD says how it waited and how fast it
//! copied - counts and times only, nothing about what the rectangles hold.

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::hvs::{Beam, Wait, in_the_way, just_past, lines_until, wait_until};
use nowayos_hw::{HwError, device};
use sel4_microkit::debug_println;

/// The device number the system description binds the scan position to,
/// when it grants one.
const SCAN: usize = 1;

/// Leading: lines above a frame's rows the scan must also be clear of, as
/// the hardware reads the framebuffer a little ahead of the line it reports.
const MARGIN: u32 = 16;

/// Chasing: how far past the frame's top row the scan may be when the copy
/// starts - wide enough that a wait reading the line in a loop never misses
/// it.
const SLACK: u32 = 32;

/// The longest a frame waits for the scan: three refreshes at 60 Hz, more
/// than one at any refresh rate a monitor offers.
const WAIT_AT_MOST_US: u64 = 50_000;

/// Frames between reports.
const REPORT_EVERY: u32 = 1000;

/// How long the start-up look at the scan may take, at most, and the pause
/// between its reads of the line.
const LOOK_AT_MOST_US: u64 = 200_000;
const LOOK_PAUSE_US: u64 = 50;

/// Copy speeds are kept in sixteenths of a counter tick a row.
const SIXTEENTHS: u64 = 16;

/// The scan, how fast this PD copies, and what frames have waited since the
/// last report.
pub struct Scan {
    beam: Beam,
    /// Counter ticks a refresh takes, the lines the scan reports, and a
    /// line's pixels.
    period: u64,
    lines: u32,
    width: u32,
    /// Sixteenths of a tick a copied full-width row takes, averaged over
    /// frames.
    per_row: Option<u64>,
    frames: u32,
    led: u32,
    chased: u32,
    at_once: u32,
    waited: u32,
    late: u32,
    waited_ticks: u64,
    longest_ticks: u64,
    said_late: bool,
}

impl Scan {
    /// The scan of the screen `width` x `height`, if the system grants it;
    /// `None`, silently, if it grants none; `None`, saying why, if it
    /// cannot be read. Says what it found: the channel, the frame rate and
    /// the lines it saw.
    pub fn bind(width: u32, height: u32) -> Option<Self> {
        let beam = match device(SCAN).and_then(|device| Beam::new(device, width, height)) {
            Ok(beam) => beam,
            Err(HwError::NotMapped { .. }) => return None,
            Err(error) => {
                debug_println!("NOWAYOS: display-stub: copies do not wait for the scan: {error}");
                return None;
            }
        };
        debug_println!(
            "NOWAYOS: display-stub: copies keep in step with the scan, on display channel {}",
            beam.channel()
        );
        let (period, lowest, highest) = look(&beam);
        debug_println!(
            "NOWAYOS: display-stub: the scan: a frame every {} us, lines {lowest} to {highest} seen",
            microseconds(period)
        );
        Some(Self {
            beam,
            period,
            lines: highest.saturating_add(1),
            width,
            per_row: None,
            frames: 0,
            led: 0,
            chased: 0,
            at_once: 0,
            waited: 0,
            late: 0,
            waited_ticks: 0,
            longest_ticks: 0,
            said_late: false,
        })
    }

    /// Whether this PD copies a whole screen in under nine tenths of a
    /// refresh, as measured: then it leads the scan; otherwise, or not yet
    /// measured, it chases it.
    fn leads(&self) -> bool {
        self.per_row.is_some_and(|per_row| {
            per_row
                .saturating_mul(u64::from(self.lines))
                .saturating_mul(10)
                < self.period.saturating_mul(SIXTEENTHS).saturating_mul(9)
        })
    }

    /// Waits until a frame covering the monitor's rows `rows` (from the
    /// first to before the last) can be copied from the top down without
    /// the scan overtaking it - leading or chasing it - at most
    /// [`WAIT_AT_MOST_US`].
    pub fn wait_for(&mut self, rows: (u32, u32)) {
        let (top, bottom) = rows;
        let started = counter();
        let deadline = started.saturating_add(ticks(WAIT_AT_MOST_US));
        let beam = &self.beam;
        let line = || beam.position().line;
        // Between reads of the line, three quarters of the lines still to go
        // by the counter: the scan is read a few times a wait.
        let lines = self.lines;
        let per_line = self.period.checked_div(u64::from(lines)).unwrap_or(0);
        let rest = move |away: u32| {
            u64::from(away.saturating_mul(3).checked_div(4).unwrap_or(0)).saturating_mul(per_line)
        };
        let wait = if self.leads() {
            self.led = self.led.saturating_add(1);
            wait_until(
                line,
                counter,
                |line| !in_the_way(line, top, bottom, MARGIN),
                |line| rest(lines_until(line, bottom, lines)),
                deadline,
            )
        } else {
            self.chased = self.chased.saturating_add(1);
            wait_until(
                line,
                counter,
                |line| just_past(line, top, SLACK),
                |line| rest(lines_until(line, top, lines)),
                deadline,
            )
        };
        let took = counter().saturating_sub(started);
        match wait {
            Wait::AtOnce => self.at_once = self.at_once.saturating_add(1),
            Wait::Waited => self.waited = self.waited.saturating_add(1),
            Wait::Late => {
                self.late = self.late.saturating_add(1);
                if !self.said_late {
                    debug_println!(
                        "NOWAYOS: display-stub: the scan did not come where a copy of rows {top} to {bottom} wanted it within {WAIT_AT_MOST_US} us: copied anyway"
                    );
                    self.said_late = true;
                }
            }
        }
        if wait != Wait::AtOnce {
            self.waited_ticks = self.waited_ticks.saturating_add(took);
            self.longest_ticks = self.longest_ticks.max(took);
        }
    }

    /// Counts a frame copied: `pixels` of the monitor written in `took`
    /// counter ticks, which the copy speed - a full-width row's time - is
    /// averaged from.
    pub fn copied(&mut self, pixels: u64, took: u64) {
        if pixels > 0 {
            let sample = took
                .saturating_mul(SIXTEENTHS)
                .saturating_mul(u64::from(self.width))
                .checked_div(pixels)
                .unwrap_or(0);
            self.per_row = Some(self.per_row.map_or(sample, |per_row| {
                per_row
                    .saturating_mul(7)
                    .saturating_add(sample)
                    .checked_div(8)
                    .unwrap_or(sample)
            }));
        }
        self.frames = self.frames.saturating_add(1);
        if self.frames >= REPORT_EVERY {
            self.report();
        }
    }

    fn report(&mut self) {
        let mean = self
            .waited_ticks
            .checked_div(u64::from(self.waited.saturating_add(self.late)))
            .unwrap_or(0);
        // Full-width rows a millisecond: the copy's, and the scan's.
        let copying = self
            .per_row
            .and_then(|per_row| {
                counter_frequency()
                    .saturating_mul(SIXTEENTHS)
                    .checked_div(per_row.saturating_mul(1000))
            })
            .unwrap_or(0);
        let scanning = u64::from(self.lines)
            .saturating_mul(counter_frequency())
            .checked_div(self.period.saturating_mul(1000))
            .unwrap_or(0);
        debug_println!(
            "NOWAYOS: display-stub: the last {} frames: {} leading the scan, {} chasing it; {} at once, {} after waiting (mean {} us, longest {} us), {} late; copying {copying} rows a ms, the scan {scanning}",
            self.frames,
            self.led,
            self.chased,
            self.at_once,
            self.waited,
            microseconds(mean),
            microseconds(self.longest_ticks),
            self.late
        );
        self.frames = 0;
        self.led = 0;
        self.chased = 0;
        self.at_once = 0;
        self.waited = 0;
        self.late = 0;
        self.waited_ticks = 0;
        self.longest_ticks = 0;
    }
}

/// Watches the scan for a few frames, reading it every [`LOOK_PAUSE_US`]:
/// the counter ticks a frame takes (0 if the frame never moved on), and the
/// lowest and highest lines read.
fn look(beam: &Beam) -> (u64, u32, u32) {
    const FRAMES: u64 = 4;
    let started = counter();
    let deadline = started.saturating_add(ticks(LOOK_AT_MOST_US));
    let pause = ticks(LOOK_PAUSE_US);
    let first = beam.position();
    let (mut lowest, mut highest) = (first.line, first.line);
    let mut frame = first.frame;
    let mut changes = 0_u64;
    let mut since = None;
    let mut now = started;
    while now < deadline && changes <= FRAMES {
        let position = beam.position();
        lowest = lowest.min(position.line);
        highest = highest.max(position.line);
        now = counter();
        if position.frame != frame {
            frame = position.frame;
            if since.is_none() {
                since = Some(now);
            } else {
                changes = changes.saturating_add(1);
            }
        }
        let next = now.saturating_add(pause);
        while counter() < next {
            core::hint::spin_loop();
        }
    }
    let period = since
        .filter(|_| changes > 0)
        .and_then(|since| now.saturating_sub(since).checked_div(changes))
        .unwrap_or(0);
    (period, lowest, highest)
}

/// `us` microseconds in counter ticks.
fn ticks(us: u64) -> u64 {
    us.saturating_mul(counter_frequency())
        .checked_div(1_000_000)
        .unwrap_or(0)
}

/// `ticks` of the counter in whole microseconds.
fn microseconds(ticks: u64) -> u64 {
    ticks
        .saturating_mul(1_000_000)
        .checked_div(counter_frequency())
        .unwrap_or(0)
}
