// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-tls-test-small-arena`: a test instrument, in
//! `systems/tls-test-small-arena.system`.
//!
//! `nowayos-tls-test`'s handshake with the largest Certificate message, on
//! an arena of [`SMALL_ARENA`] bytes - below that handshake's peak, above
//! what dlmalloc takes for its first segment (64 KiB; a smaller arena
//! refuses the very first request) - so the arena refuses a request part
//! way through the handshake, the refusal is reported, and Rust's
//! allocation-error handler stops the PD. It must never print a handshake
//! that completed or failed: it never carries on. Seeded, like
//! `nowayos-tls-test`, with a fixed public test value. A test instrument:
//! never in a demo or product system.

#![no_std]
#![no_main]

use core::alloc::Layout;
use nowayos_entropy::SEED_LEN;
use nowayos_tls_loopback::{Certificate, run};
use rustls::crypto::{GetRandomFailed, SecureRandom};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The arena: below the largest handshake's peak in a PD (264 KiB,
/// `systems/tls-test.expect`), so that the handshake starts and then runs
/// out.
const SMALL_ARENA: usize = 256 * 1024;

nowayos_heap::declare_heap!(HEAP, SMALL_ARENA, Some(report_refusal));

/// The seed for this test only, and public.
const TEST_SEED: [u8; SEED_LEN] = *b"nowayos tls-test seed, 32 bytes.";

/// The PD's generator, as rustls's random source.
#[derive(Debug)]
struct PdRandom;

impl SecureRandom for PdRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        nowayos_entropy::fill(buf).map_err(|_| GetRandomFailed)
    }
}

static RANDOM: PdRandom = PdRandom;

/// The arena refused a request: report it. The allocation error that
/// follows stops the PD.
fn report_refusal(layout: Layout, in_use: usize) {
    debug_println!("NOWAYOS: tls-test-small-arena: the arena refused an allocation");
    debug_println!(
        "NOWAYOS: tls-test-small-arena: {} bytes asked for with {in_use} of {SMALL_ARENA} in use",
        layout.size()
    );
}

#[protection_domain]
fn init() -> SmallArenaPd {
    debug_println!("NOWAYOS: tls-test-small-arena: starting; arena {SMALL_ARENA} bytes");
    let mut seed = TEST_SEED;
    if nowayos_entropy::seed(&mut seed).is_err() {
        debug_println!("NOWAYOS: tls-test-small-arena: the generator refused its seed");
        return SmallArenaPd;
    }
    debug_println!("NOWAYOS: tls-test-small-arena: largest handshake starting");
    match run(&RANDOM, Certificate::Largest) {
        Ok(_) => debug_println!("NOWAYOS: tls-test-small-arena: largest handshake completed"),
        Err(failure) => {
            debug_println!("NOWAYOS: tls-test-small-arena: largest handshake failed: {failure}");
        }
    }
    SmallArenaPd
}

/// The PD, were it ever to get here.
struct SmallArenaPd;

impl Handler for SmallArenaPd {
    type Error = Infallible;
}
