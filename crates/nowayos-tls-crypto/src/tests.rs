// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit and property tests: the random source's failures reach the
//! handshake, key shares a peer sends are checked, records round-trip and
//! one changed bit is refused, no private key loads, and no verification
//! algorithm panics on any input. The OpenSSL interop and negative tests
//! are in `tests/openssl.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

extern crate std;

use alloc::vec;
use alloc::vec::Vec;
use std::io::Read;

use proptest::prelude::*;
use rustls::crypto::cipher::{
    AeadKey, InboundOpaqueMessage, Iv, OutboundChunks, OutboundPlainMessage,
};
use rustls::crypto::{GetRandomFailed, SecureRandom};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ContentType, Error, NamedGroup, PeerMisbehaved, ProtocolVersion};

use super::*;

/// The operating system's random source, for tests only.
#[derive(Debug)]
struct OsRandom;

impl SecureRandom for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(buf))
            .map_err(|_| GetRandomFailed)
    }
}

/// A random source that always fails.
#[derive(Debug)]
struct NoRandom;

impl SecureRandom for NoRandom {
    fn fill(&self, _buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        Err(GetRandomFailed)
    }
}

/// A random source that gives only `0xFF` bytes: never a P-256 private key,
/// which must be below the group order.
#[derive(Debug)]
struct AllOnes;

impl SecureRandom for AllOnes {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        buf.fill(0xFF);
        Ok(())
    }
}

static RANDOM: OsRandom = OsRandom;

fn group(
    random: &'static dyn SecureRandom,
    name: NamedGroup,
) -> &'static dyn rustls::crypto::SupportedKxGroup {
    *provider(random)
        .kx_groups
        .iter()
        .find(|group| group.name() == name)
        .unwrap()
}

#[test]
fn offers_tls13_suites_and_groups_in_order() {
    let provider = provider(&RANDOM);
    let suites: Vec<_> = provider.cipher_suites.iter().map(|s| s.suite()).collect();
    assert_eq!(
        suites,
        [
            rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
            rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
            rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
        ]
    );
    assert!(provider.cipher_suites.iter().all(|s| s.tls13().is_some()));
    let groups: Vec<_> = provider.kx_groups.iter().map(|g| g.name()).collect();
    assert_eq!(groups, [NamedGroup::X25519, NamedGroup::secp256r1]);
}

#[test]
fn a_failing_random_source_fails_the_key_exchange() {
    for name in [NamedGroup::X25519, NamedGroup::secp256r1] {
        let error = group(&NoRandom, name).start().err().unwrap();
        assert_eq!(error, Error::FailedToGetRandomBytes, "{name:?}");
    }
}

#[test]
fn p256_gives_up_on_a_source_that_never_gives_a_private_key() {
    let error = group(&AllOnes, NamedGroup::secp256r1)
        .start()
        .err()
        .unwrap();
    assert_eq!(error, Error::FailedToGetRandomBytes);
}

#[test]
fn both_ends_of_each_group_agree() {
    for name in [NamedGroup::X25519, NamedGroup::secp256r1] {
        let group = group(&RANDOM, name);
        let (a, b) = (group.start().unwrap(), group.start().unwrap());
        assert_eq!(a.group(), name);
        let a_public = a.pub_key().to_vec();
        let b_public = b.pub_key().to_vec();
        assert_ne!(a_public, b_public);
        let ab = a.complete(&b_public).unwrap();
        let ba = b.complete(&a_public).unwrap();
        assert_eq!(ab.secret_bytes(), ba.secret_bytes(), "{name:?}");
    }
}

fn refuses(name: NamedGroup, peer: &[u8]) {
    let exchange = group(&RANDOM, name).start().unwrap();
    assert_eq!(
        exchange.complete(peer).err(),
        Some(Error::PeerMisbehaved(PeerMisbehaved::InvalidKeyShare)),
        "{name:?} accepted {peer:02x?}"
    );
}

#[test]
fn x25519_refuses_a_share_of_the_wrong_length_or_of_small_order() {
    refuses(NamedGroup::X25519, &[]);
    refuses(NamedGroup::X25519, &[9; 31]);
    refuses(NamedGroup::X25519, &[9; 33]);
    // Points of small order give the all-zero shared secret (RFC 7748,
    // section 6.1; RFC 8446, section 7.4.2): zero, one, and the point of
    // order eight.
    refuses(NamedGroup::X25519, &[0; 32]);
    let mut one = [0u8; 32];
    one[0] = 1;
    refuses(NamedGroup::X25519, &one);
    let order_eight: [u8; 32] = [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ];
    refuses(NamedGroup::X25519, &order_eight);
}

#[test]
fn p256_refuses_a_share_that_is_not_an_uncompressed_point_on_the_curve() {
    let valid = group(&RANDOM, NamedGroup::secp256r1).start().unwrap();
    let share = valid.pub_key().to_vec();
    assert_eq!(share.len(), 65);
    refuses(NamedGroup::secp256r1, &[]);
    refuses(NamedGroup::secp256r1, &share[..64]);
    // The same point, compressed: TLS 1.3 allows only the uncompressed form.
    let mut compressed = vec![0x02 | (share[64] & 1)];
    compressed.extend_from_slice(&share[1..33]);
    refuses(NamedGroup::secp256r1, &compressed);
    // Wrong first byte, the point at infinity, and a point off the curve.
    let mut hybrid = share.clone();
    hybrid[0] = 0x06;
    refuses(NamedGroup::secp256r1, &hybrid);
    refuses(NamedGroup::secp256r1, &[0]);
    let mut off_curve = share.clone();
    off_curve[64] ^= 1;
    refuses(NamedGroup::secp256r1, &off_curve);
    refuses(NamedGroup::secp256r1, &[0x04; 65]);
}

/// A record of `plaintext` sealed by `aead` under `key`, as sequence 3.
fn seal(
    aead: &dyn rustls::crypto::cipher::Tls13AeadAlgorithm,
    key: [u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    let record = aead
        .encrypter(AeadKey::from(key), Iv::from([7u8; 12]))
        .encrypt(
            OutboundPlainMessage {
                typ: ContentType::ApplicationData,
                version: ProtocolVersion::TLSv1_2,
                payload: OutboundChunks::Single(plaintext),
            },
            3,
        )?;
    Ok(record.encode())
}

/// Opens `record` (header included) as sequence `seq`.
fn open(
    aead: &dyn rustls::crypto::cipher::Tls13AeadAlgorithm,
    key: [u8; 32],
    record: &mut [u8],
    seq: u64,
) -> Result<(ContentType, Vec<u8>), Error> {
    let opened = aead
        .decrypter(AeadKey::from(key), Iv::from([7u8; 12]))
        .decrypt(
            InboundOpaqueMessage::new(
                ContentType::ApplicationData,
                ProtocolVersion::TLSv1_2,
                &mut record[5..],
            ),
            seq,
        )?;
    Ok((opened.typ, opened.payload.to_vec()))
}

#[test]
fn records_round_trip_and_one_changed_bit_is_refused() {
    let plaintext = b"nowayos record";
    // rustls's AeadKey is public only at 32 bytes, so AES-128-GCM is
    // covered by the OpenSSL tests and, below, by refusing a key of the
    // wrong length.
    for suite in [TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256] {
        let aead = suite.tls13().unwrap().aead_alg;
        assert_eq!(aead.key_len(), 32);
        let mut key = [0u8; 32];
        RANDOM.fill(&mut key).unwrap();
        let record = seal(aead, key, plaintext).unwrap();
        // Header, plaintext, content type, tag.
        assert_eq!(record.len(), 5 + plaintext.len() + 1 + 16);
        let encrypter = aead.encrypter(AeadKey::from(key), Iv::from([7u8; 12]));
        assert_eq!(
            encrypter.encrypted_payload_len(plaintext.len()),
            record.len() - 5
        );

        let opened = open(aead, key, &mut record.clone(), 3).unwrap();
        assert_eq!(opened, (ContentType::ApplicationData, plaintext.to_vec()));

        // Every bit of the payload, the tag included; then the wrong
        // sequence number; then a record shorter than a tag.
        for bit in 0..(record.len() - 5) * 8 {
            let mut bad = record.clone();
            bad[5 + bit / 8] ^= 1 << (bit % 8);
            assert_eq!(
                open(aead, key, &mut bad, 3).err(),
                Some(Error::DecryptError),
                "bit {bit}"
            );
        }
        assert_eq!(
            open(aead, key, &mut record.clone(), 4).err(),
            Some(Error::DecryptError)
        );
        assert_eq!(
            open(aead, key, &mut [0u8; 20], 3).err(),
            Some(Error::DecryptError)
        );
    }
}

#[test]
fn a_key_of_the_wrong_length_fails_every_record_without_panicking() {
    let aead = TLS13_AES_128_GCM_SHA256.tls13().unwrap().aead_alg;
    assert_eq!(aead.key_len(), 16);
    assert_eq!(seal(aead, [1; 32], b"x").err(), Some(Error::EncryptError));
    assert_eq!(
        open(aead, [1; 32], &mut [0u8; 40], 0).err(),
        Some(Error::DecryptError)
    );
}

#[test]
fn traffic_keys_are_never_extracted() {
    for suite in provider(&RANDOM).cipher_suites {
        let aead = suite.tls13().unwrap().aead_alg;
        assert!(
            aead.extract_keys(AeadKey::from([1; 32]), Iv::from([0u8; 12]))
                .is_err()
        );
    }
}

#[test]
fn no_private_key_loads() {
    let provider = provider(&RANDOM);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(vec![0x30, 0x00]));
    match provider.key_provider.load_private_key(key) {
        Err(Error::General(reason)) => assert_eq!(reason, NO_PRIVATE_KEYS),
        other => panic!("{other:?}"),
    }
}

proptest! {
    /// No verification algorithm panics on any public key, message and
    /// signature, and none accepts random ones.
    #[test]
    fn verification_never_panics_on_any_input(
        public_key in proptest::collection::vec(any::<u8>(), 0..600),
        message in proptest::collection::vec(any::<u8>(), 0..64),
        signature in proptest::collection::vec(any::<u8>(), 0..600),
        first in prop_oneof![Just(None), Just(Some(0x04u8)), Just(Some(0x30u8))],
    ) {
        let mut public_key = public_key;
        if let (Some(byte), Some(slot)) = (first, public_key.first_mut()) {
            *slot = byte;
        }
        for algorithm in SIGNATURE_VERIFICATION_ALGORITHMS.all {
            prop_assert!(algorithm.verify_signature(&public_key, &message, &signature).is_err());
        }
    }
}
