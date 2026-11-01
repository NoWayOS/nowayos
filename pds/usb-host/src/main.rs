// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-usb-host`: the USB host PD's program, for NoWayOS's systems.
//! The PD itself is this crate's library.

#![no_std]
#![no_main]

use nowayos_usb_host::UsbHost;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> UsbHost {
    nowayos_usb_host::init()
}
