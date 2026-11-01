// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the P-256 check. Key pairs are made at test
//! time from each case's own random bytes and never stored. A signature a
//! key makes is accepted, and one bit changed anywhere - in the message, the
//! signature or the public key - gets it refused; malformed keys and
//! signatures are refused with their typed errors; and nothing panics,
//! whatever the input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_signature::{
    DIGEST_LEN, Error, PUBLIC_KEY_LEN, PublicKey, SIGNATURE_LEN, Sha256, Signature, check,
    known_answer, sha256, verify, verify_digest,
};
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer;
use proptest::prelude::*;

/// P-256's group order, n, big-endian.
const ORDER: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xBC, 0xE6, 0xFA, 0xAD, 0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63, 0x25, 0x51,
];

/// P-256's field prime, p, big-endian.
const PRIME: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// A signing key from a case's random bytes: any scalar from 1 to n - 1.
fn signing_key() -> impl Strategy<Value = SigningKey> {
    any::<[u8; 32]>().prop_filter_map("a scalar from 1 to n - 1", |bytes| {
        SigningKey::from_slice(&bytes).ok()
    })
}

/// The key's public half, SEC1 uncompressed.
fn public_key(signing_key: &SigningKey) -> [u8; PUBLIC_KEY_LEN] {
    signing_key
        .verifying_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .unwrap()
}

/// The key's signature over `message`, r then s.
fn sign(signing_key: &SigningKey, message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let signature: p256::ecdsa::Signature = signing_key.sign(message);
    signature.to_bytes().into()
}

/// n - `value`, big-endian, for a value from 1 to n - 1: a signature's twin
/// s.
fn order_minus(value: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for at in (0..32).rev() {
        let difference = i16::from(ORDER[at]) - i16::from(value[at]) - borrow;
        borrow = i16::from(difference < 0);
        out[at] = u8::try_from(difference.rem_euclid(256)).unwrap();
    }
    out
}

/// The signature with s replaced by n - s.
fn twin(signature: &[u8; SIGNATURE_LEN]) -> [u8; SIGNATURE_LEN] {
    let mut twin = *signature;
    twin[32..].copy_from_slice(&order_minus(&signature[32..]));
    twin
}

/// `bytes` with bit `bit` changed.
fn flipped(bytes: &[u8], bit: usize) -> Vec<u8> {
    let mut bytes = bytes.to_vec();
    bytes[bit / 8] ^= 1 << (bit % 8);
    bytes
}

/// 32 big-endian bytes plus a small number, saturating at all ones.
fn plus(value: &[u8; 32], add: u8) -> [u8; 32] {
    let mut out = *value;
    let mut carry = u16::from(add);
    for byte in out.iter_mut().rev() {
        let sum = u16::from(*byte) + carry;
        *byte = (sum & 0xFF) as u8;
        carry = sum >> 8;
    }
    if carry == 0 { out } else { [0xFF; 32] }
}

#[test]
fn openssls_signature_is_accepted_and_one_bit_changed_is_refused() {
    let (key, message, signature) = (
        known_answer::PUBLIC_KEY,
        known_answer::MESSAGE,
        known_answer::SIGNATURE,
    );
    assert_eq!(check(&key, message, &signature), Ok(()));
    // Its s is above half the order, and its twin, s below it, is accepted
    // too.
    assert!(order_minus(&signature[32..]).as_slice() < &signature[32..]);
    assert_eq!(check(&key, message, &twin(&signature)), Ok(()));
    let public = PublicKey::from_sec1(&key).unwrap();
    assert_eq!(public.id(), known_answer::KEY_ID);
    assert_eq!(public.to_sec1(), key);
    assert_eq!(
        Signature::from_bytes(&signature).unwrap().to_bytes(),
        signature
    );
    assert_eq!(
        verify_digest(
            &public,
            &sha256(message),
            &Signature::from_bytes(&signature).unwrap()
        ),
        Ok(())
    );
    // The bits the test PD changes (systems/signature-test.expect).
    assert_eq!(
        check(&key, &flipped(message, 0), &signature),
        Err(Error::Mismatch)
    );
    assert_eq!(
        check(&key, message, &flipped(&signature, 8 * SIGNATURE_LEN - 8)),
        Err(Error::Mismatch)
    );
    assert_eq!(
        check(&flipped(&key, 8 * PUBLIC_KEY_LEN - 8), message, &signature),
        Err(Error::KeyNotOnCurve)
    );
    // Every other bit too.
    for bit in 0..8 * message.len() {
        assert!(check(&key, &flipped(message, bit), &signature).is_err());
    }
    for bit in 0..8 * SIGNATURE_LEN {
        assert!(check(&key, message, &flipped(&signature, bit)).is_err());
    }
    for bit in 0..8 * PUBLIC_KEY_LEN {
        assert!(check(&flipped(&key, bit), message, &signature).is_err());
    }
}

#[test]
fn the_point_at_infinity_and_short_keys_are_refused() {
    assert_eq!(PublicKey::from_sec1(&[0]), Err(Error::KeyAtInfinity));
    assert_eq!(
        PublicKey::from_sec1(&[]),
        Err(Error::KeyLength { found: 0 })
    );
    let compressed = [2u8; 33];
    assert_eq!(
        PublicKey::from_sec1(&compressed),
        Err(Error::KeyLength { found: 33 })
    );
    // (0, 0), which some encodings use for infinity, is not on the curve.
    let mut zero = [0u8; PUBLIC_KEY_LEN];
    zero[0] = 4;
    assert_eq!(PublicKey::from_sec1(&zero), Err(Error::KeyNotOnCurve));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn a_signature_is_accepted_and_any_bit_changed_is_refused(
        signing_key in signing_key(),
        message in prop::collection::vec(any::<u8>(), 1..200),
        bit in any::<prop::sample::Index>(),
    ) {
        let key = public_key(&signing_key);
        let signature = sign(&signing_key, &message);
        prop_assert_eq!(check(&key, &message, &signature), Ok(()));
        let public = PublicKey::from_sec1(&key).unwrap();
        let parsed = Signature::from_bytes(&signature).unwrap();
        prop_assert_eq!(verify(&public, &message, &parsed), Ok(()));
        prop_assert_eq!(verify_digest(&public, &sha256(&message), &parsed), Ok(()));
        // Its twin, r and n - s, is accepted too, whichever half s is in.
        prop_assert_eq!(check(&key, &message, &twin(&signature)), Ok(()));
        let message_bit = bit.index(8 * message.len());
        prop_assert_eq!(check(&key, &flipped(&message, message_bit), &signature), Err(Error::Mismatch));
        let signature_bit = bit.index(8 * SIGNATURE_LEN);
        prop_assert!(check(&key, &message, &flipped(&signature, signature_bit)).is_err());
        let key_bit = bit.index(8 * PUBLIC_KEY_LEN);
        prop_assert!(check(&flipped(&key, key_bit), &message, &signature).is_err());
    }

    #[test]
    fn another_keys_signature_is_refused(
        signer in signing_key(),
        other in signing_key(),
        message in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        prop_assume!(signer != other);
        let signature = sign(&signer, &message);
        prop_assert_eq!(check(&public_key(&other), &message, &signature), Err(Error::Mismatch));
    }
}

proptest! {
    #[test]
    fn keys_of_the_wrong_length_or_prefix_are_refused(
        bytes in prop::collection::vec(any::<u8>(), 0..2 * PUBLIC_KEY_LEN),
        prefix in any::<u8>(),
        coordinates in prop::collection::vec(any::<u8>(), 64),
    ) {
        if bytes.len() != PUBLIC_KEY_LEN && bytes != [0] {
            prop_assert_eq!(
                PublicKey::from_sec1(&bytes),
                Err(Error::KeyLength { found: bytes.len() })
            );
        }
        let mut key = vec![prefix];
        key.extend_from_slice(&coordinates);
        let expected = if prefix == 4 { Error::KeyNotOnCurve } else { Error::KeyPrefix { found: prefix } };
        // Random coordinates are a point on the curve with probability
        // about 2^-256.
        prop_assert_eq!(PublicKey::from_sec1(&key), Err(expected));
    }

    #[test]
    fn coordinates_not_below_the_prime_are_refused(
        signing_key in signing_key(),
        add in any::<u8>(),
        in_x in any::<bool>(),
    ) {
        let mut key = public_key(&signing_key);
        let at = if in_x { 1 } else { 33 };
        key[at..at + 32].copy_from_slice(&plus(&PRIME, add));
        prop_assert_eq!(PublicKey::from_sec1(&key), Err(Error::KeyNotOnCurve));
    }

    #[test]
    fn signatures_out_of_range_are_refused(
        r in any::<[u8; 32]>(),
        s in any::<[u8; 32]>(),
        add in any::<u8>(),
        length in 0usize..2 * SIGNATURE_LEN,
    ) {
        let valid = |scalar: &[u8; 32]| scalar != &[0; 32] && scalar < &ORDER;
        let mut signature = [0u8; SIGNATURE_LEN];
        signature[..32].copy_from_slice(&r);
        signature[32..].copy_from_slice(&s);
        let expected = if r == [0; 32] {
            Err(Error::RIsZero)
        } else if !valid(&r) {
            Err(Error::RNotBelowOrder)
        } else if s == [0; 32] {
            Err(Error::SIsZero)
        } else if !valid(&s) {
            Err(Error::SNotBelowOrder)
        } else {
            Ok(())
        };
        prop_assert_eq!(Signature::from_bytes(&signature).map(|_| ()), expected);
        // Zero and n + add, in r and in s.
        let good = if valid(&r) { r } else { [1; 32] };
        for (bad, error) in [([0u8; 32], Error::RIsZero), (plus(&ORDER, add), Error::RNotBelowOrder)] {
            signature[..32].copy_from_slice(&bad);
            signature[32..].copy_from_slice(&good);
            prop_assert_eq!(Signature::from_bytes(&signature), Err(error));
        }
        for (bad, error) in [([0u8; 32], Error::SIsZero), (plus(&ORDER, add), Error::SNotBelowOrder)] {
            signature[..32].copy_from_slice(&good);
            signature[32..].copy_from_slice(&bad);
            prop_assert_eq!(Signature::from_bytes(&signature), Err(error));
        }
        if length != SIGNATURE_LEN {
            prop_assert_eq!(
                Signature::from_bytes(&vec![1; length]),
                Err(Error::SignatureLength { found: length })
            );
        }
    }

    #[test]
    fn any_input_is_checked_or_refused_without_a_panic(
        key in prop::collection::vec(any::<u8>(), 0..80),
        message in prop::collection::vec(any::<u8>(), 0..80),
        signature in prop::collection::vec(any::<u8>(), 0..80),
    ) {
        let _ = check(&key, &message, &signature);
    }

    #[test]
    fn hashing_in_pieces_is_hashing_at_once(
        bytes in prop::collection::vec(any::<u8>(), 0..300),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..5),
    ) {
        let mut cuts: Vec<usize> = cuts.iter().map(|cut| cut.index(bytes.len() + 1)).collect();
        cuts.sort_unstable();
        let mut hasher = Sha256::new();
        let mut from = 0;
        for cut in cuts {
            hasher.update(&bytes[from..cut]);
            from = cut;
        }
        hasher.update(&bytes[from..]);
        let digest: [u8; DIGEST_LEN] = hasher.finish();
        prop_assert_eq!(digest, sha256(&bytes));
    }
}
