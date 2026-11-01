// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-clock`: the clock PD's program, for NoWayOS's systems. The PD
//! itself is this crate's library.

#![no_std]
#![no_main]

use nowayos_clock::ClockPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> ClockPd {
    nowayos_clock::init()
}
