// SPDX-License-Identifier: MIT OR Apache-2.0
//! The program starts at the restart entry (`nowayos-restart`), not
//! at rust-sel4's `_start`, which the entry runs once it has checked or
//! restored the PD's memory.

fn main() {
    println!("cargo::rustc-link-arg-bins=--entry=nowayos_restart_entry");
}
