// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-display-ramfb`: the ramfb display PD's program, for NoWayOS's systems. The PD itself is
//! this crate's library, so a product's system can run it from a program of
//! its own.

#![no_std]
#![no_main]

use nowayos_display_ramfb::DisplayPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> DisplayPd {
    nowayos_display_ramfb::init()
}
