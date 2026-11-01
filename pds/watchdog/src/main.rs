// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-watchdog`: the watchdog PD's program for NoWayOS's own test
//! systems. The watchdog itself is this crate's library; this program
//! names the PDs a test system's watchdog watches and runs it, as a
//! product's own program does for a product system.
//!
//! One program serves every test system with a watchdog, so it picks the
//! PDs it watches - and, on the Pi 5, the PM watchdog - by its own PD name,
//! which the system description gives it: `watchdog` (QEMU's stand-in),
//! `pm_watchdog` (the Pi's PM watchdog and three probes) or `run_watchdog`
//! (the Pi's PM watchdog, no PD watched: a test system's run length). A
//! watchdog with another name watches nothing, and so never services the
//! hardware watchdog.

#![no_std]
#![no_main]

use nowayos_watchdog::{WatchdogPd, Watched};
use sel4_microkit::{pd_name, protection_domain};

/// `systems/watchdog-test.system`: the supervisor and three test PDs,
/// highest priority first, each on its own heartbeat channel; the
/// hardware watchdog's stand-in on channel 4.
static WATCHDOG_TEST: [Watched; 4] = [
    Watched::new(0, "watchdog_supervisor"),
    Watched::new(1, "upper"),
    Watched::new(2, "looper"),
    Watched::new(3, "lower"),
];

/// `systems/watchdog-test.system`'s channel to the stand-in.
const WATCHDOG_TEST_HARDWARE: usize = 4;

/// `systems/rpi5b_2gb/pm-watchdog-test.system`: three test PDs,
/// highest priority first, each on its own heartbeat channel; the
/// ticker's ticks on channel 3, and the PM watchdog as device 0.
static PM_WATCHDOG_TEST: [Watched; 3] = [
    Watched::new(0, "upper"),
    Watched::new(1, "looper"),
    Watched::new(2, "lower"),
];

/// `pm-watchdog-test`'s channel to the ticker.
const PM_WATCHDOG_TEST_TICKS: usize = 3;

/// A Pi test system's run length (`run_watchdog`): the ticker's
/// ticks on channel 0, and the PM watchdog as device 0.
const RUN_TICKS: usize = 0;

/// The PM watchdog: `nowayos-hw`'s device 0 in each Pi system.
const PM_DEVICE: usize = 0;

#[protection_domain]
fn init() -> WatchdogPd {
    match pd_name() {
        Ok("watchdog") => nowayos_watchdog::init(&WATCHDOG_TEST, WATCHDOG_TEST_HARDWARE),
        Ok("pm_watchdog") => {
            nowayos_watchdog::init_pm(&PM_WATCHDOG_TEST, PM_WATCHDOG_TEST_TICKS, PM_DEVICE)
        }
        Ok("run_watchdog") => nowayos_watchdog::init_pm_ticks_only(RUN_TICKS, PM_DEVICE),
        _ => nowayos_watchdog::init(&[], WATCHDOG_TEST_HARDWARE),
    }
}
