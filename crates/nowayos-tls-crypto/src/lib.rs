// SPDX-License-Identifier: MIT OR Apache-2.0
//! rustls's crypto provider for a protection domain, built from RustCrypto
//! crates: the glue the TLS PD's rustls
//! client runs on, with no C, no assembly and no `unsafe` code of its own.
//!
//! Modelled on rustls 0.23.45's `provider-example` (Apache-2.0 OR ISC OR
//! MIT, by the rustls project): its shape - one small type per rustls
//! trait, each wrapping one RustCrypto primitive - is adapted here, and its
//! code is derived from that example where the two match, under its MIT
//! licence, whose copyright and permission notice are kept in this crate's
//! `LICENSE-rustls-MIT`. What differs:
//! the suites, groups and algorithms below; the caller's random source in
//! place of the operating system's; the X25519 all-zero check; RSA keys held
//! to 2048-8192 bits; and no signing, no HPKE and no `unwrap`.
//!
//! # What it offers
//!
//! * **TLS 1.3 only**, as a client: `TLS13_AES_256_GCM_SHA384`,
//!   `TLS13_AES_128_GCM_SHA256` and `TLS13_CHACHA20_POLY1305_SHA256`, in
//!   rustls's own order of preference.
//! * **Key exchange:** X25519, then P-256 (`secp256r1`). The first is the one
//!   whose key share the ClientHello carries.
//! * **Signature verification only:** ECDSA on P-256 and P-384 (each with
//!   SHA-256 or SHA-384), RSA PKCS#1 v1.5 and RSA-PSS with SHA-256, -384 and
//!   -512 on keys of 2048 to 8192 bits, and Ed25519 - the same set and
//!   mapping as rustls's own providers ([`SIGNATURE_VERIFICATION_ALGORITHMS`]).
//!   Nothing here holds or uses a private key: [`provider`]'s key provider
//!   refuses every key, so RSA's timing advisory (RUSTSEC-2023-0071, which
//!   concerns private-key operations) does not reach this crate.
//! * **No HPKE** (its crates are MPL-2.0), no QUIC and no secret extraction:
//!   traffic keys never leave the provider.
//!
//! # Randomness
//!
//! The caller passes the random source, a [`SecureRandom`]: rustls draws its
//! ClientHello random from it, and the key-exchange groups their ephemeral
//! private keys. Nothing here calls `getrandom` or the operating system, and
//! when the source fails, the handshake fails (there is never a
//! fallback). `make nostd` is the guard: `getrandom` does not build for
//! `aarch64-unknown-none` without a back end, which `make unsafe-audit`
//! allows nowhere but the entropy PD.
//!
//! # TLS 1.2 later
//!
//! A product that needs TLS 1.2 may have it. The pieces here are what its ECDHE-GCM suites need too: the
//! AEAD glue is generic over the cipher (`aead::Aead`), and the hashes
//! and HMACs serve rustls's `PrfUsingHmac`. Turning them on is a
//! `Tls12AeadAlgorithm` for GCM (a 4-byte fixed IV and an 8-byte explicit
//! nonce), the two suites, and rustls's `tls12` feature: no rework of what
//! is here.
//!
//! # Status
//!
//! Provisional: ahead of the TLS PD (`systems/tls-test.system` runs it in a
//! PD).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;

use rustls::crypto::{CipherSuiteCommon, CryptoProvider, KeyProvider, SecureRandom, tls13};
use rustls::pki_types::PrivateKeyDer;
use rustls::{CipherSuite, SupportedCipherSuite, Tls13CipherSuite};

mod aead;
mod hash;
mod hmac;
mod kx;
mod verify;

pub use verify::SIGNATURE_VERIFICATION_ALGORITHMS;

/// rustls's crypto provider on RustCrypto, drawing every random byte from
/// `random`.
///
/// Its two key-exchange groups hold `random`, and rustls takes them as
/// `&'static` references, so each call allocates them once and never frees
/// them: call it once per protection domain, as rustls's own providers are
/// made once per process.
#[must_use]
pub fn provider(random: &'static dyn SecureRandom) -> CryptoProvider {
    let x25519: &'static kx::X25519 = Box::leak(Box::new(kx::X25519::new(random)));
    let p256: &'static kx::P256 = Box::leak(Box::new(kx::P256::new(random)));
    CryptoProvider {
        cipher_suites: vec![
            TLS13_AES_256_GCM_SHA384,
            TLS13_AES_128_GCM_SHA256,
            TLS13_CHACHA20_POLY1305_SHA256,
        ],
        kx_groups: vec![x25519, p256],
        signature_verification_algorithms: SIGNATURE_VERIFICATION_ALGORITHMS,
        secure_random: random,
        key_provider: &NoPrivateKeys,
    }
}

/// `TLS13_AES_256_GCM_SHA384`: AES-256-GCM, with SHA-384 for the transcript
/// and HKDF.
pub static TLS13_AES_256_GCM_SHA384: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
            hash_provider: &hash::SHA384,
            confidentiality_limit: aead::GCM_CONFIDENTIALITY_LIMIT,
        },
        hkdf_provider: &tls13::HkdfUsingHmac(&hmac::SHA384),
        aead_alg: &aead::AES_256_GCM,
        quic: None,
    });

/// `TLS13_AES_128_GCM_SHA256`: AES-128-GCM, with SHA-256 for the transcript
/// and HKDF.
pub static TLS13_AES_128_GCM_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
            hash_provider: &hash::SHA256,
            confidentiality_limit: aead::GCM_CONFIDENTIALITY_LIMIT,
        },
        hkdf_provider: &tls13::HkdfUsingHmac(&hmac::SHA256),
        aead_alg: &aead::AES_128_GCM,
        quic: None,
    });

/// `TLS13_CHACHA20_POLY1305_SHA256`: ChaCha20-Poly1305, with SHA-256 for the
/// transcript and HKDF.
pub static TLS13_CHACHA20_POLY1305_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
            hash_provider: &hash::SHA256,
            confidentiality_limit: u64::MAX,
        },
        hkdf_provider: &tls13::HkdfUsingHmac(&hmac::SHA256),
        aead_alg: &aead::CHACHA20_POLY1305,
        quic: None,
    });

/// The key provider: this crate verifies and never signs, so it loads no
/// private key - a client certificate or a server key fails here, with
/// [`NO_PRIVATE_KEYS`] as the reason.
#[derive(Debug)]
struct NoPrivateKeys;

/// Why [`provider`]'s key provider refused a private key.
pub const NO_PRIVATE_KEYS: &str = "nowayos-tls-crypto verifies only: it loads no private key";

impl KeyProvider for NoPrivateKeys {
    fn load_private_key(
        &self,
        _key_der: PrivateKeyDer<'static>,
    ) -> Result<Arc<dyn rustls::sign::SigningKey>, rustls::Error> {
        Err(rustls::Error::General(String::from(NO_PRIVATE_KEYS)))
    }
}

#[cfg(test)]
mod tests;
