// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-uart-input`: the program NoWayOS's systems run this input PD
//! from. The PD itself is this crate's library.

#![no_std]
#![no_main]

use nowayos_uart_input::UartInputPd;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> UartInputPd {
    nowayos_uart_input::init()
}
