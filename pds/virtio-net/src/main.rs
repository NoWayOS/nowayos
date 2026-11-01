// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-virtio-net`: the virtio-net NIC driver PD's program, for NoWayOS's systems. The PD itself is
//! this crate's library, so a product's system can run it from a program of
//! its own.

#![no_std]
#![no_main]

use nowayos_virtio_net::VirtioNetPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> VirtioNetPd {
    nowayos_virtio_net::init()
}
