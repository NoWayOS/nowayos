// SPDX-License-Identifier: MIT OR Apache-2.0
//! The loopback's own checks: both handshakes complete with data both ways,
//! the largest Certificate message is exactly rustls's limit and one byte
//! more is refused, a key other than the pinned one is refused, and a
//! failing random source stops the run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

extern crate std;

use std::io::Read;

use rustls::crypto::GetRandomFailed;

use super::*;

#[derive(Debug)]
struct OsRandom;

impl SecureRandom for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(buf))
            .map_err(|_| GetRandomFailed)
    }
}

#[derive(Debug)]
struct NoRandom;

impl SecureRandom for NoRandom {
    fn fill(&self, _buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        Err(GetRandomFailed)
    }
}

static RANDOM: OsRandom = OsRandom;

#[test]
fn a_plain_handshake_completes_and_carries_data_both_ways() {
    let report = run(&RANDOM, Certificate::Plain).unwrap();
    assert_eq!(report.suite, CipherSuite::TLS13_AES_256_GCM_SHA384);
    assert_eq!(report.group, NamedGroup::X25519);
    assert_eq!(report.certificate_payload, 1 + 3 + 3 + 44 + 2);
}

#[test]
fn the_largest_certificate_message_rustls_accepts_completes() {
    let report = run(&RANDOM, Certificate::Largest).unwrap();
    assert_eq!(report.certificate_payload, LARGEST_HANDSHAKE_PAYLOAD);
}

#[test]
fn one_byte_more_is_refused() {
    match run(&RANDOM, Certificate::TooLarge) {
        Err(Failure::Tls(End::Client, rustls::Error::InvalidMessage(message))) => {
            assert_eq!(message, rustls::InvalidMessage::HandshakePayloadTooLarge);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn another_key_than_the_pinned_one_is_refused() {
    match run_pinning_another_key(&RANDOM) {
        Err(Failure::Tls(End::Client, rustls::Error::InvalidCertificate(_))) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failing_random_source_stops_the_run() {
    assert!(matches!(
        run(&NoRandom, Certificate::Plain),
        Err(Failure::Random)
    ));
}
