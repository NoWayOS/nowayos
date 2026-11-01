// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-virtio-console`: the virtio console driver PD, both ways, for
//! NoWayOS's own test system (`systems/virtio-console-test.system`). The
//! driver itself is this crate's library; a product system runs it from a
//! program of its own, with the directions it needs.

#![no_std]
#![no_main]

use nowayos_virtio_console::{Directions, VirtioConsolePd};
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> VirtioConsolePd {
    nowayos_virtio_console::init(Directions::Both)
}
