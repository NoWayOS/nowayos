// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-update-pd`: the update PD as `systems/update-test.system`
//! and `systems/update-trial-r*.system` run it - for the product `kiosk`,
//! release 1.0.0, on QEMU's board -
//! over the library ([`nowayos_update_pd`]); the storage PD says which slot
//! runs and how long a slot is.

#![no_std]
#![no_main]

use nowayos_update::Version;
use nowayos_update_pd::{Settings, UpdatePd};
use sel4_microkit::protection_domain;

#[protection_domain]
fn init() -> UpdatePd {
    nowayos_update_pd::init(Settings {
        product: "kiosk",
        board: "qemu_virt_aarch64",
        version: Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
    })
}
