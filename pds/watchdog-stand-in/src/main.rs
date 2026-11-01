// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-watchdog-stand-in`: the watchdog stand-in test instrument's program, for NoWayOS's systems. The PD itself is
//! this crate's library, so a product's system can run it from a program of
//! its own.

#![no_std]
#![no_main]

use nowayos_watchdog_stand_in::StandInPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> StandInPd {
    nowayos_watchdog_stand_in::init()
}
