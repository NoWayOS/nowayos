// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-usb-camera`: a test instrument - the USB host PD
//! (`nowayos-usb-host`), started with `init_camera`, so it also streams the
//! first USB camera it finds: its Motion JPEG video, payload headers
//! removed, as whole frames into a ring a client PD reads, and every USB
//! audio interface as `nowayos-usb-tone` does. Its system, the Pi 5's
//! `usb-camera-test`, runs `nowayos-camera-test` as the client. Never in a
//! product's image.

#![no_std]
#![no_main]

use nowayos_usb_host::UsbHost;
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> UsbHost {
    nowayos_usb_host::init_camera()
}
