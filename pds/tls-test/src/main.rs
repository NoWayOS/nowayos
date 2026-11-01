// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-tls-test`: a test instrument, in
//! `systems/tls-test.system`.
//!
//! A rustls client and a rustls server, both on `nowayos-tls-crypto`, run a
//! TLS 1.3 handshake inside this one PD, joined by memory buffers
//! (`nowayos-tls-loopback`). The server presents an Ed25519 raw public key
//! made at start from the PD's own generator (`nowayos-entropy`), seeded
//! with [`TEST_SEED`] - a fixed, public value for this test image only - so
//! no key exists anywhere but in this PD's memory while it runs. The client
//! pins that key and checks the server's signature with it; then data
//! crosses both ways.
//!
//! It runs twice: with the plain Certificate message, and with the largest
//! rustls accepts. Each time it prints the suite, the group and the arena's
//! peak use. Then once more with the client pinning another key (the
//! server's with one bit changed), which the client must refuse. The arena is `nowayos_tls_loopback::ARENA_BYTES`, set from the
//! host's measurement of the larger handshake with its margin; this shows
//! it holds both on aarch64-sel4, where dlmalloc's own overhead counts too.
//! A test instrument: never in a demo or product system.

#![no_std]
#![no_main]

use core::alloc::Layout;
use nowayos_entropy::SEED_LEN;
use nowayos_tls_loopback::{ARENA_BYTES, Certificate, run, run_pinning_another_key};
use rustls::crypto::{GetRandomFailed, SecureRandom};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

nowayos_heap::declare_heap!(HEAP, ARENA_BYTES, Some(report_refusal));

/// The seed for this test only, and public: in a PD, a seed from the
/// entropy PD.
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
    debug_println!("NOWAYOS: tls-test: the arena refused an allocation");
    debug_println!(
        "NOWAYOS: tls-test: {} bytes asked for with {in_use} of {ARENA_BYTES} in use",
        layout.size()
    );
}

#[protection_domain]
fn init() -> TlsTestPd {
    debug_println!("NOWAYOS: tls-test: starting; arena {ARENA_BYTES} bytes");
    let mut seed = TEST_SEED;
    if nowayos_entropy::seed(&mut seed).is_err() {
        debug_println!("NOWAYOS: tls-test: the generator refused its seed");
        return TlsTestPd;
    }
    for (name, certificate) in [
        ("plain", Certificate::Plain),
        ("largest", Certificate::Largest),
    ] {
        HEAP.reset_peak();
        debug_println!("NOWAYOS: tls-test: {name} handshake starting");
        match run(&RANDOM, certificate) {
            Ok(report) => {
                debug_println!(
                    "NOWAYOS: tls-test: {name} handshake: TLS 1.3, {:?}, {:?}, Certificate payload {} bytes",
                    report.suite,
                    report.group,
                    report.certificate_payload
                );
                debug_println!(
                    "NOWAYOS: tls-test: {name} handshake: server's raw public key checked; data crossed both ways"
                );
                debug_println!(
                    "NOWAYOS: tls-test: {name} handshake: arena peak {} bytes",
                    HEAP.peak()
                );
            }
            Err(failure) => debug_println!("NOWAYOS: tls-test: {name} handshake failed: {failure}"),
        }
    }
    match run_pinning_another_key(&RANDOM) {
        Ok(_) => debug_println!("NOWAYOS: tls-test: another key pinned: accepted"),
        Err(failure) => debug_println!("NOWAYOS: tls-test: another key pinned: refused: {failure}"),
    }
    debug_println!(
        "NOWAYOS: tls-test: done; {} refusals by the arena",
        HEAP.refusals()
    );
    TlsTestPd
}

/// The PD after its handshakes: it has no channels, so nothing more to do.
struct TlsTestPd;

impl Handler for TlsTestPd {
    type Error = Infallible;
}
