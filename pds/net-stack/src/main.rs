// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-net-stack`: the network stack PD's program, for NoWayOS's
//! systems. The PD itself is this crate's library, so a product's system can
//! run it from a program of its own.

#![no_std]
#![no_main]

use sel4_microkit::{NullHandler, protection_domain};

#[protection_domain]
fn init() -> NullHandler {
    nowayos_net_stack::init()
}
