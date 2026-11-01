// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-budget-probe`: a test instrument for time isolation,
//! in `systems/budget-test.system` and `budget-test-widened.system`. Each
//! PD that runs it acts by its name:
//!
//! * **`heartbeat`** beats every [`BEAT_MS`] ms on the timer PD's
//!   timeouts, and says whether each beat came on time - within
//!   [`ON_TIME_MS`] ms of when it was due - or how late. After its
//!   [`LOOPER_AFTER`]th beat it tells `looper` to start; after
//!   [`BEATS`] beats it says whether every one was on time, and stops.
//! * **`looper`**, above it, waits to be told, then loops for ever. With a
//!   budget below its period the kernel stops it at its budget each
//!   period, so the heartbeat below it keeps its time; with its budget
//!   widened to its period it starves every PD below it, and the
//!   heartbeat stops.
//!
//! * **`smp_heartbeat`** (`systems/smp-test.system`) first asks the
//!   kernel to list the threads on its core, then tells `spinner_1` to do
//!   the same, which tells the next, around a ring back to it; then it
//!   beats as the heartbeat does, and after its [`LOOPER_AFTER`]th beat
//!   tells the spinners, around the same ring, to spin.
//! * **`spinner_N`**, on a core of its own: told once, it lists its core's
//!   threads and passes the word on; told again, it passes the word on and
//!   spins for ever. Each waits for its turn, so no two cores print at once.
//!
//! Never in a demo or product system.

#![no_std]
#![no_main]

#[allow(
    unsafe_code,
    reason = "the census's one debug system call, which rust-sel4 does not wrap"
)]
mod census;

use nowayos_timer_api::Client;
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// How often the heartbeat beats, in milliseconds.
const BEAT_MS: u64 = 100;

/// How late a beat may come and still be on time, in milliseconds: the
/// looper's budget, and QEMU's emulation, with room to spare.
const ON_TIME_MS: u64 = 20;

/// The beat after which `looper` is told to start.
const LOOPER_AFTER: u64 = 3;

/// How many beats the heartbeat makes.
const BEATS: u64 = 20;

/// A millisecond in nanoseconds.
const MS: u64 = 1_000_000;

/// The heartbeat's timeout id.
const BEAT: u64 = 1;

/// The heartbeat's channel to the timer PD.
const TIMER: Client = Client::new(Channel::new(0));

/// The heartbeat's channel to `looper`; `looper`'s to the heartbeat. In
/// the SMP ring, each PD's channel to the next.
const PARTNER: Channel = Channel::new(1);

/// In the SMP ring, a spinner's channel from the PD before it.
const FROM_PREVIOUS: Channel = Channel::new(0);

/// In the SMP ring, the heartbeat's channel from the last spinner.
const RING_BACK: Channel = Channel::new(2);

#[protection_domain]
fn init() -> ProbePd {
    let name = pd_name().unwrap_or("?");
    let mut pd = ProbePd {
        name,
        start: 0,
        beats: 0,
        late: 0,
        told: 0,
        beating: false,
    };
    if name == "heartbeat" {
        pd.start_beating();
    } else if name == "smp_heartbeat" {
        census::list_this_core(name);
        PARTNER.notify();
    } else if !name.starts_with("spinner_") {
        debug_println!("NOWAYOS: budget-probe: {name}: waiting to be told to loop");
    }
    pd
}

struct ProbePd {
    name: &'static str,
    /// When the heartbeat started, in nanoseconds.
    start: u64,
    /// Beats so far.
    beats: u64,
    /// Beats that came late.
    late: u64,
    /// How often a spinner has been told: once to list its core, then to spin.
    told: u32,
    /// Whether the SMP heartbeat has started beating.
    beating: bool,
}

impl ProbePd {
    /// The heartbeat's first timeout, and the line that says it is beating.
    fn start_beating(&mut self) {
        let name = self.name;
        match TIMER.now() {
            Ok(now) if TIMER.set_timeout(BEAT, BEAT_MS * MS).is_ok() => {
                self.start = now.as_nanos();
                self.beating = true;
                debug_println!("NOWAYOS: budget-probe: {name}: beating every {BEAT_MS} ms");
            }
            _ => debug_println!("NOWAYOS: budget-probe: {name}: gave up: the timer refused"),
        }
    }

    /// One beat: when it came against when it was due.
    fn beat(&mut self) {
        while let Ok(Some(_)) = TIMER.expired() {}
        let Ok(now) = TIMER.now() else {
            debug_println!(
                "NOWAYOS: budget-probe: {}: gave up: the timer refused",
                self.name
            );
            return;
        };
        self.beats = self.beats.saturating_add(1);
        let due = self
            .start
            .saturating_add(self.beats.saturating_mul(BEAT_MS * MS));
        let late_ms = now.as_nanos().saturating_sub(due) / MS;
        if late_ms <= ON_TIME_MS {
            debug_println!(
                "NOWAYOS: budget-probe: {}: beat {}: on time",
                self.name,
                self.beats
            );
        } else {
            self.late = self.late.saturating_add(1);
            debug_println!(
                "NOWAYOS: budget-probe: {}: beat {}: late by {late_ms} ms",
                self.name,
                self.beats
            );
        }
        if self.beats == LOOPER_AFTER {
            PARTNER.notify();
        }
        if self.beats >= BEATS {
            if self.late == 0 {
                debug_println!(
                    "NOWAYOS: budget-probe: {}: {BEATS} beats, every one on time",
                    self.name
                );
            } else {
                debug_println!(
                    "NOWAYOS: budget-probe: {}: {BEATS} beats, {} late",
                    self.name,
                    self.late
                );
            }
            return;
        }
        // The next beat, from when it is due, not from now: lateness never
        // piles up.
        let next = due.saturating_add(BEAT_MS * MS);
        let wait = next.saturating_sub(now.as_nanos()).max(1);
        if TIMER.set_timeout(BEAT, wait).is_err() {
            debug_println!(
                "NOWAYOS: budget-probe: {}: gave up: the timer refused",
                self.name
            );
        }
    }
}

impl Handler for ProbePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if self.name == "heartbeat" || self.name == "smp_heartbeat" {
            if channels.contains(RING_BACK) && self.name == "smp_heartbeat" && !self.beating {
                self.start_beating();
            }
            if channels.contains(TIMER.channel()) && self.beating {
                self.beat();
            }
        } else if self.name.starts_with("spinner_") {
            if channels.contains(FROM_PREVIOUS) {
                self.told = self.told.saturating_add(1);
                if self.told == 1 {
                    census::list_this_core(self.name);
                    PARTNER.notify();
                } else {
                    debug_println!("NOWAYOS: budget-probe: {}: spinning from now on", self.name);
                    PARTNER.notify();
                    loop {
                        core::hint::spin_loop();
                    }
                }
            }
        } else if channels.contains(Channel::new(0)) {
            debug_println!("NOWAYOS: budget-probe: {}: looping from now on", self.name);
            loop {
                core::hint::spin_loop();
            }
        }
        Ok(())
    }
}
