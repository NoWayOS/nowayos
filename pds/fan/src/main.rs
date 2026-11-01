// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-fan`: the fan PD's program, for NoWayOS's systems. The PD itself
//! is this crate's library, so a product's system can run it from a program
//! of its own.

#![no_std]
#![no_main]

use nowayos_fan::FanPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> FanPd {
    nowayos_fan::init()
}
