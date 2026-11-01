// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-tls-crypto`'s signature verification - any
//! public key, message and signature, as rustls-webpki hands them over from
//! a server's certificates and CertificateVerify, to each algorithm the
//! provider verifies with: ECDSA on P-256 and P-384, RSA PKCS#1 v1.5 and
//! PSS, Ed25519.
//!
//! Checks: nothing panics, whatever the input; and each algorithm gives the
//! same answer twice.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_tls_crypto::SIGNATURE_VERIFICATION_ALGORITHMS;

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    // Which algorithm (or all of them), then the lengths of the key and the
    // signature; the message is the rest.
    let choice = usize::from(input.byte());
    let key_len = usize::from(input.u16());
    let signature_len = usize::from(input.u16());
    let rest = input.rest();
    let (public_key, rest) = rest.split_at(key_len.min(rest.len()));
    let (signature, message) = rest.split_at(signature_len.min(rest.len()));
    let all = SIGNATURE_VERIFICATION_ALGORITHMS.all;
    let chosen = match all.get(choice) {
        Some(one) => core::slice::from_ref(one),
        None => all,
    };
    for algorithm in chosen {
        let first = algorithm.verify_signature(public_key, message, signature);
        let again = algorithm.verify_signature(public_key, message, signature);
        assert_eq!(first.is_ok(), again.is_ok(), "{algorithm:?}");
    }
});
