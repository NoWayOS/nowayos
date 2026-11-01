// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-usb-tone`: a test instrument - the USB host PD
//! (`nowayos-usb-host`), started with `init_test_tone`, so it also streams
//! every USB audio interface it finds: a 440 Hz tone to a speaker, a
//! microphone's packets counted and never kept. Its systems -
//! `usb-audio-test` in QEMU, `usb-stream-test` on the Pi 5 - check each
//! stream's intervals served and missed. Never in a product's image.

#![no_std]
#![no_main]

use nowayos_usb_host::UsbHost;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> UsbHost {
    nowayos_usb_host::init_test_tone()
}
