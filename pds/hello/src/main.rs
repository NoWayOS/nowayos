// Follows the minimal protection-domain shape of rust-sel4 v5.0.0's
// crates/examples/microkit/hello example, whose notice is kept here:
//
// Copyright 2023, Colias Group, LLC
//
// SPDX-License-Identifier: BSD-2-Clause

//! `nowayos-hello`: the first protection domain.
//!
//! The smallest useful NoWayOS PD: it prints two fixed lines on the seL4 debug
//! console and then waits for events it will never receive. It proves the
//! toolchain, rust-sel4, the Microkit SDK and the boot path end to end, first on
//! QEMU (`qemu_virt_aarch64`) and later on the Raspberry Pi 5.
//!
//! No heap (no global allocator is declared, so any allocation fails to link),
//! no `unsafe` of its own, no devices. The debug console exists only in the
//! Microkit `debug` configuration.

#![no_std]
#![no_main]

use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// First line on the console; `systems/hello.expect` checks for it.
const STARTING: &str = "NOWAYOS: rust pd starting";

/// Second line on the console; `systems/hello.expect` checks for it.
const HELLO: &str = "NOWAYOS: hello from rust";

#[protection_domain]
fn init() -> HelloPd {
    debug_println!("{STARTING}");
    debug_println!("{HELLO}");
    HelloPd
}

/// Event handler for the hello PD. It has no channels, so the default
/// `Handler` behaviour (nothing to do) is all it needs.
struct HelloPd;

impl Handler for HelloPd {
    type Error = Infallible;
}
