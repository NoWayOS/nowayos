// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-input-broker`: the input broker PD's program, for NoWayOS's
//! systems. The PD itself is this crate's library.

#![no_std]
#![no_main]

use nowayos_input_broker::BrokerPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> BrokerPd {
    nowayos_input_broker::init()
}
