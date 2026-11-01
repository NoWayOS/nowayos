// SPDX-License-Identifier: MIT OR Apache-2.0
//! Signature verification for certificates and the handshake's
//! CertificateVerify: ECDSA on P-256 and P-384 (`p256`/`p384` 0.14), RSA
//! PKCS#1 v1.5 and PSS (`rsa` 0.9), Ed25519 (`ed25519-dalek` 2) - the set,
//! and the mapping from TLS signature schemes, of rustls's own providers.
//!
//! Each algorithm takes what rustls-webpki hands it: the public key as the
//! certificate's `subjectPublicKey` bit string, the signed message, and the
//! signature, all from the peer and so all hostile. Any of them malformed is
//! [`InvalidSignature`], never a panic (property tests, and the `tls_verify`
//! fuzz target).

use p256::ecdsa::signature::hazmat::PrehashVerifier as _;
use rsa::pkcs1::der::Decode;
use rsa::signature::Verifier;
use rsa::traits::PublicKeyParts;
use rustls::SignatureScheme;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{
    AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm, alg_id,
};
use sha2_010::Digest;

/// Every algorithm, and which serve each TLS signature scheme. For TLS 1.3
/// rustls uses the first algorithm a scheme maps to, so ECDSA's schemes
/// name their own curve first.
pub static SIGNATURE_VERIFICATION_ALGORITHMS: WebPkiSupportedAlgorithms =
    WebPkiSupportedAlgorithms {
        all: &[
            ECDSA_P256_SHA256,
            ECDSA_P256_SHA384,
            ECDSA_P384_SHA256,
            ECDSA_P384_SHA384,
            ED25519,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
        ],
        mapping: &[
            (
                SignatureScheme::ECDSA_NISTP384_SHA384,
                &[ECDSA_P384_SHA384, ECDSA_P256_SHA384],
            ),
            (
                SignatureScheme::ECDSA_NISTP256_SHA256,
                &[ECDSA_P256_SHA256, ECDSA_P384_SHA256],
            ),
            (SignatureScheme::ED25519, &[ED25519]),
            (SignatureScheme::RSA_PSS_SHA512, &[RSA_PSS_SHA512]),
            (SignatureScheme::RSA_PSS_SHA384, &[RSA_PSS_SHA384]),
            (SignatureScheme::RSA_PSS_SHA256, &[RSA_PSS_SHA256]),
            (SignatureScheme::RSA_PKCS1_SHA512, &[RSA_PKCS1_SHA512]),
            (SignatureScheme::RSA_PKCS1_SHA384, &[RSA_PKCS1_SHA384]),
            (SignatureScheme::RSA_PKCS1_SHA256, &[RSA_PKCS1_SHA256]),
        ],
    };

static ECDSA_P256_SHA256: &dyn SignatureVerificationAlgorithm = &Ecdsa {
    curve: Curve::P256,
    hash: Hash::Sha256,
};
static ECDSA_P256_SHA384: &dyn SignatureVerificationAlgorithm = &Ecdsa {
    curve: Curve::P256,
    hash: Hash::Sha384,
};
static ECDSA_P384_SHA256: &dyn SignatureVerificationAlgorithm = &Ecdsa {
    curve: Curve::P384,
    hash: Hash::Sha256,
};
static ECDSA_P384_SHA384: &dyn SignatureVerificationAlgorithm = &Ecdsa {
    curve: Curve::P384,
    hash: Hash::Sha384,
};
static ED25519: &dyn SignatureVerificationAlgorithm = &Ed25519;
static RSA_PSS_SHA256: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha256,
};
static RSA_PSS_SHA384: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha384,
};
static RSA_PSS_SHA512: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha512,
};
static RSA_PKCS1_SHA256: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha256,
};
static RSA_PKCS1_SHA384: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha384,
};
static RSA_PKCS1_SHA512: &dyn SignatureVerificationAlgorithm = &Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha512,
};

/// The digest a signature is over.
#[derive(Clone, Copy, Debug)]
enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

/// A NIST curve for ECDSA.
#[derive(Clone, Copy, Debug)]
enum Curve {
    P256,
    P384,
}

/// ECDSA on `curve` over `hash`. The key is an uncompressed SEC1 point, as
/// rustls's other providers require; the signature is DER (RFC 5480,
/// `Ecdsa-Sig-Value`). The digest is truncated or padded to the curve's
/// size as ECDSA defines.
#[derive(Debug)]
struct Ecdsa {
    curve: Curve,
    hash: Hash,
}

impl SignatureVerificationAlgorithm for Ecdsa {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        match self.curve {
            Curve::P256 => alg_id::ECDSA_P256,
            Curve::P384 => alg_id::ECDSA_P384,
        }
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match self.hash {
            Hash::Sha256 => alg_id::ECDSA_SHA256,
            Hash::Sha384 => alg_id::ECDSA_SHA384,
            Hash::Sha512 => alg_id::ECDSA_SHA512,
        }
    }

    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        if public_key.first() != Some(&SEC1_UNCOMPRESSED) {
            return Err(InvalidSignature);
        }
        let digest = Digested::of(self.hash, message);
        match self.curve {
            Curve::P256 => {
                let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let signature =
                    p256::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
                key.verify_prehash(digest.as_slice(), &signature)
                    .map_err(|_| InvalidSignature)
            }
            Curve::P384 => {
                let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let signature =
                    p384::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
                key.verify_prehash(digest.as_slice(), &signature)
                    .map_err(|_| InvalidSignature)
            }
        }
    }
}

/// SEC1's first byte of an uncompressed point.
const SEC1_UNCOMPRESSED: u8 = 0x04;

/// Ed25519 (RFC 8032), checked strictly: a non-canonical signature, or a
/// key or `R` of small order, is refused.
#[derive(Debug)]
struct Ed25519;

impl SignatureVerificationAlgorithm for Ed25519 {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }

    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let key: &[u8; ed25519_dalek::PUBLIC_KEY_LENGTH] =
            public_key.try_into().map_err(|_| InvalidSignature)?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(key).map_err(|_| InvalidSignature)?;
        let signature =
            ed25519_dalek::Signature::from_slice(signature).map_err(|_| InvalidSignature)?;
        key.verify_strict(message, &signature)
            .map_err(|_| InvalidSignature)
    }
}

/// RSA's signature padding.
#[derive(Clone, Copy, Debug)]
enum Padding {
    /// PKCS#1 v1.5 (RFC 8017, section 8.2).
    Pkcs1,
    /// PSS with MGF1 over the same hash and a salt as long as the hash
    /// (RFC 8017, section 8.1; RFC 8446, section 4.2.3).
    Pss,
}

/// The smallest RSA modulus accepted, in bits, as rustls's other providers.
const RSA_MIN_BITS: usize = 2048;

/// The largest RSA modulus accepted, in bits, as rustls's other providers.
/// It bounds the work one hostile key can ask for.
const RSA_MAX_BITS: usize = 8192;

/// RSA with `padding` over `hash`, on a key of [`RSA_MIN_BITS`] to
/// [`RSA_MAX_BITS`] bits with an odd public exponent from 3 to 2^33 - 1.
/// The key is PKCS#1's `RSAPublicKey` (RFC 8017, appendix A.1.1), under
/// `rsaEncryption` (RSA-PSS keys with parameters are not accepted, as
/// rustls's other providers). Only public-key operations, so RSA's timing
/// advisory (RUSTSEC-2023-0071) does not apply.
#[derive(Debug)]
struct Rsa {
    padding: Padding,
    hash: Hash,
}

impl SignatureVerificationAlgorithm for Rsa {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match (self.padding, self.hash) {
            (Padding::Pkcs1, Hash::Sha256) => alg_id::RSA_PKCS1_SHA256,
            (Padding::Pkcs1, Hash::Sha384) => alg_id::RSA_PKCS1_SHA384,
            (Padding::Pkcs1, Hash::Sha512) => alg_id::RSA_PKCS1_SHA512,
            (Padding::Pss, Hash::Sha256) => alg_id::RSA_PSS_SHA256,
            (Padding::Pss, Hash::Sha384) => alg_id::RSA_PSS_SHA384,
            (Padding::Pss, Hash::Sha512) => alg_id::RSA_PSS_SHA512,
        }
    }

    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let key = rsa_public_key(public_key)?;
        match (self.padding, self.hash) {
            (Padding::Pkcs1, Hash::Sha256) => pkcs1::<sha2_010::Sha256>(key, message, signature),
            (Padding::Pkcs1, Hash::Sha384) => pkcs1::<sha2_010::Sha384>(key, message, signature),
            (Padding::Pkcs1, Hash::Sha512) => pkcs1::<sha2_010::Sha512>(key, message, signature),
            (Padding::Pss, Hash::Sha256) => pss::<sha2_010::Sha256>(key, message, signature),
            (Padding::Pss, Hash::Sha384) => pss::<sha2_010::Sha384>(key, message, signature),
            (Padding::Pss, Hash::Sha512) => pss::<sha2_010::Sha512>(key, message, signature),
        }
    }
}

/// Decodes PKCS#1's `RSAPublicKey` and holds it to the bounds above. DER's
/// rules (the `der` crate) refuse a negative or non-minimal integer.
fn rsa_public_key(public_key: &[u8]) -> Result<rsa::RsaPublicKey, InvalidSignature> {
    let decoded = rsa::pkcs1::RsaPublicKey::from_der(public_key).map_err(|_| InvalidSignature)?;
    let n = rsa::BigUint::from_bytes_be(decoded.modulus.as_bytes());
    let e = rsa::BigUint::from_bytes_be(decoded.public_exponent.as_bytes());
    let bits = n.bits();
    if !(RSA_MIN_BITS..=RSA_MAX_BITS).contains(&bits) || !is_odd(&n) || !is_odd(&e) {
        return Err(InvalidSignature);
    }
    // new_with_max_size refuses an exponent below 2 or above 2^33 - 1; with
    // the oddness above, that leaves 3 to 2^33 - 1.
    let key =
        rsa::RsaPublicKey::new_with_max_size(n, e, RSA_MAX_BITS).map_err(|_| InvalidSignature)?;
    if key.size().saturating_mul(8) < RSA_MIN_BITS {
        return Err(InvalidSignature);
    }
    Ok(key)
}

/// Whether `value`'s lowest bit is set.
fn is_odd(value: &rsa::BigUint) -> bool {
    value
        .to_bytes_le()
        .first()
        .is_some_and(|byte| byte & 1 == 1)
}

/// RSA PKCS#1 v1.5 with the hash `D`.
fn pkcs1<D>(
    key: rsa::RsaPublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), InvalidSignature>
where
    D: Digest + rsa::pkcs1::der::oid::AssociatedOid,
{
    let signature = rsa::pkcs1v15::Signature::try_from(signature).map_err(|_| InvalidSignature)?;
    rsa::pkcs1v15::VerifyingKey::<D>::new(key)
        .verify(message, &signature)
        .map_err(|_| InvalidSignature)
}

/// RSA-PSS with the hash `D`, MGF1 over `D`, and a salt as long as `D`'s
/// output (rsa 0.9's `VerifyingKey::new`).
fn pss<D>(key: rsa::RsaPublicKey, message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature>
where
    D: Digest + sha2_010::digest::FixedOutputReset,
{
    let signature = rsa::pss::Signature::try_from(signature).map_err(|_| InvalidSignature)?;
    rsa::pss::VerifyingKey::<D>::new(key)
        .verify(message, &signature)
        .map_err(|_| InvalidSignature)
}

/// A message's digest, as ECDSA takes it.
enum Digested {
    Sha256(sha2_010::digest::Output<sha2_010::Sha256>),
    Sha384(sha2_010::digest::Output<sha2_010::Sha384>),
    Sha512(sha2_010::digest::Output<sha2_010::Sha512>),
}

impl Digested {
    fn of(hash: Hash, message: &[u8]) -> Self {
        match hash {
            Hash::Sha256 => Self::Sha256(sha2_010::Sha256::digest(message)),
            Hash::Sha384 => Self::Sha384(sha2_010::Sha384::digest(message)),
            Hash::Sha512 => Self::Sha512(sha2_010::Sha512::digest(message)),
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Sha256(digest) => digest.as_slice(),
            Self::Sha384(digest) => digest.as_slice(),
            Self::Sha512(digest) => digest.as_slice(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "test code: a failed check panics, and tests index and compute on known data"
    )]

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// DER's length octets.
    fn der_len(len: usize) -> Vec<u8> {
        if len < 0x80 {
            vec![len as u8]
        } else {
            let bytes: Vec<u8> = len
                .to_be_bytes()
                .into_iter()
                .skip_while(|b| *b == 0)
                .collect();
            let mut out = vec![0x80 | bytes.len() as u8];
            out.extend(bytes);
            out
        }
    }

    /// A DER INTEGER of the unsigned big-endian `value`.
    fn der_uint(value: &[u8]) -> Vec<u8> {
        let mut content = value.to_vec();
        if content.first().is_some_and(|b| b & 0x80 != 0) {
            content.insert(0, 0);
        }
        let mut out = vec![0x02];
        out.extend(der_len(content.len()));
        out.extend(content);
        out
    }

    /// PKCS#1's `RSAPublicKey` for modulus `n` and exponent `e`.
    fn rsa_key(n: &[u8], e: &[u8]) -> Vec<u8> {
        let mut content = der_uint(n);
        content.extend(der_uint(e));
        let mut out = vec![0x30];
        out.extend(der_len(content.len()));
        out.extend(content);
        out
    }

    /// An odd modulus of exactly `bits` bits (not a product of two primes,
    /// which the bounds do not look at).
    fn modulus(bits: usize) -> Vec<u8> {
        let mut n = vec![0xA5u8; bits.div_ceil(8)];
        let top = bits % 8;
        n[0] = if top == 0 {
            0x80 | n[0]
        } else {
            (n[0] | 1 << (top - 1)) & ((1 << top) - 1)
        };
        *n.last_mut().unwrap() |= 1;
        n
    }

    const F4: &[u8] = &[0x01, 0x00, 0x01];

    #[test]
    fn rsa_keys_are_held_to_2048_to_8192_bits() {
        assert!(rsa_public_key(&rsa_key(&modulus(2048), F4)).is_ok());
        assert!(rsa_public_key(&rsa_key(&modulus(3072), F4)).is_ok());
        assert!(rsa_public_key(&rsa_key(&modulus(8192), F4)).is_ok());
        assert!(rsa_public_key(&rsa_key(&modulus(1024), F4)).is_err());
        assert!(rsa_public_key(&rsa_key(&modulus(2047), F4)).is_err());
        assert!(rsa_public_key(&rsa_key(&modulus(8193), F4)).is_err());
    }

    #[test]
    fn rsa_keys_need_an_odd_modulus_and_an_odd_exponent_from_3() {
        let mut even = modulus(2048);
        *even.last_mut().unwrap() &= 0xFE;
        assert!(rsa_public_key(&rsa_key(&even, F4)).is_err());
        assert!(rsa_public_key(&rsa_key(&modulus(2048), &[3])).is_ok());
        for e in [
            &[0u8][..],
            &[1],
            &[2],
            &[0x01, 0x00, 0x00],
            &[0x02, 0x00, 0x00, 0x00, 0x01],
        ] {
            assert!(
                rsa_public_key(&rsa_key(&modulus(2048), e)).is_err(),
                "e = {e:02x?}"
            );
        }
        // 2^33 - 1 is the largest exponent; 2^33 + 1 is refused.
        assert!(rsa_public_key(&rsa_key(&modulus(2048), &[0x01, 0xFF, 0xFF, 0xFF, 0xFF])).is_ok());
        assert!(rsa_public_key(&rsa_key(&modulus(2048), &[0x02, 0x00, 0x00, 0x00, 0x01])).is_err());
    }

    #[test]
    fn rsa_keys_must_be_der() {
        let mut key = rsa_key(&modulus(2048), F4);
        assert!(rsa_public_key(&key[..key.len() - 1]).is_err());
        key.push(0);
        assert!(rsa_public_key(&key).is_err());
        // A negative modulus (no leading zero before a high bit).
        let n = modulus(2048);
        let mut negative = vec![0x30, 0x82, 0x01, 0x0A, 0x02, 0x82, 0x01, 0x00];
        negative.extend(&n);
        negative.extend([0x02, 0x03, 0x01, 0x00, 0x01]);
        assert!(rsa_public_key(&negative).is_err());
    }

    #[test]
    fn ecdsa_wants_an_uncompressed_point() {
        for algorithm in [ECDSA_P256_SHA256, ECDSA_P384_SHA384] {
            assert!(
                algorithm
                    .verify_signature(&[0x02; 33], b"m", &[0x30, 0x00])
                    .is_err()
            );
            assert!(algorithm.verify_signature(&[], b"m", &[]).is_err());
        }
    }
}
