// SPDX-License-Identifier: MIT OR Apache-2.0
//! The arena's size, from measurement: both handshakes - the plain
//! one a test PD runs, and the one whose Certificate message is the largest
//! rustls accepts - run under a counting global allocator, both ends in one
//! heap as in the PD, and each peak is printed and checked against the
//! figure recorded for it: a peak above its figure fails, as the arena,
//! set from the larger, might no longer hold it.
//!
//! One test in this file, so nothing else allocates while it measures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use std::io::Read;

use cap::Cap;
use nowayos_tls_loopback::{ARENA_BYTES, Certificate, LARGEST_PEAK, PLAIN_PEAK, run};
use rustls::crypto::{GetRandomFailed, SecureRandom};

#[global_allocator]
static HEAP: Cap<std::alloc::System> = Cap::new(std::alloc::System, usize::MAX);

#[derive(Debug)]
struct OsRandom;

impl SecureRandom for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(buf))
            .map_err(|_| GetRandomFailed)
    }
}

static RANDOM: OsRandom = OsRandom;

/// The peak bytes in use while `certificate`'s handshake ran, above what
/// was in use before it. The allocator's peak never resets, so the runs go
/// smallest first.
fn measure(certificate: Certificate) -> usize {
    let before = HEAP.allocated();
    let report = run(&RANDOM, certificate).unwrap();
    let peak = HEAP.max_allocated() - before;
    println!(
        "arena: {certificate:?} handshake (TLS 1.3 {:?} {:?}, Certificate payload {} bytes): peak {peak} bytes",
        report.suite, report.group, report.certificate_payload
    );
    peak
}

fn check(name: &str, measured: usize, recorded: usize) {
    assert!(
        measured <= recorded,
        "{name}: measured {measured} bytes, above the recorded {recorded}: raise it (and so ARENA_BYTES)"
    );
}

#[test]
fn the_arena_holds_the_largest_handshake_with_its_margin() {
    let plain = measure(Certificate::Plain);
    let largest = measure(Certificate::Largest);
    println!(
        "arena: ARENA_BYTES {ARENA_BYTES}: twice the largest peak {LARGEST_PEAK}, in whole pages"
    );
    assert!(largest > plain);
    check("plain", plain, PLAIN_PEAK);
    check("largest", largest, LARGEST_PEAK);
    const {
        assert!(ARENA_BYTES >= LARGEST_PEAK * 2);
        assert!(ARENA_BYTES < LARGEST_PEAK * 2 + 4096);
    }
}
