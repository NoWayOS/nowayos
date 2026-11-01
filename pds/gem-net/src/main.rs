// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-gem-net`: the GEM NIC driver PD's program, for NoWayOS's systems. The PD itself is
//! this crate's library, so a product's system can run it from a program of
//! its own.

#![no_std]
#![no_main]

use nowayos_gem_net::GemNetPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> GemNetPd {
    nowayos_gem_net::init()
}
