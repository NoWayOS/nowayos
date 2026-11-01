// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checks ECDSA P-256 signatures over SHA-256 inside a protection domain:
//! the signatures the signing authority makes over trust lists,
//! update manifests and configuration (Algorithms), and
//! ES256 tokens, which use the same algorithm.
//!
//! **Verification only.** Nothing here holds or uses a private key, so
//! nothing needs randomness: no `getrandom`, and no wait for the
//! entropy PD. The arithmetic is RustCrypto's `p256` crate with its default
//! features off - no `std`, `alloc`, `getrandom` or PEM - and `sha2`. `p256`
//! has no verify-only feature: its `ecdsa` feature compiles signing (RFC
//! 6979's deterministic nonces) too, which this crate never calls. Its
//! README warns, at 0.14.0 as before, that its curve arithmetic has never
//! been independently audited.
//!
//! # What it takes
//!
//! * A **public key** in SEC1's uncompressed form only: `0x04`, then x, then
//!   y, [`PUBLIC_KEY_LEN`] bytes - or the two coordinates alone
//!   ([`PublicKey::from_coordinates`]), as a trust list carries them. One
//!   encoding per key, so keys compare by their bytes. Refused, each with
//!   its [`Error`]: any other length, any other first byte, the point at
//!   infinity (SEC1's single `0x00`), and coordinates that are not a point
//!   on the curve (including a coordinate not below the field's prime).
//! * A **signature** as r then s, big-endian, [`SIGNATURE_LEN`] bytes, as an
//!   HSM's PKCS#11 interface returns it (`CKM_ECDSA`). Refused: any other
//!   length, and r or s that is zero or not below the group order.
//!   Signatures with s above half the order are accepted, as OpenSSL and
//!   PKCS#11 tokens make them - so with each signature its twin, r and
//!   n - s, is accepted too: a signature's bytes never identify what was
//!   signed.
//!
//! A check fails with [`Error::Mismatch`] when the signature is well
//! formed but not one the key made over the message. Nothing panics on any
//! input: property tests, and a fuzz target in `make fuzz`.
//!
//! [`sha256`] and [`Sha256`] give the digest the signatures are over, for
//! images too long to hold at once, and a key's ID ([`PublicKey::id`]).
//!
//! # Status
//!
//! Provisional: ahead of the update PD.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use core::fmt;

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use sha2::Digest;

pub mod known_answer;

/// The length of a public key in SEC1's uncompressed form: `0x04`, x, y.
pub const PUBLIC_KEY_LEN: usize = 65;

/// The length of a public key's two coordinates, x then y.
pub const COORDINATES_LEN: usize = 64;

/// The length of a signature: r then s.
pub const SIGNATURE_LEN: usize = 64;

/// The length of a SHA-256 digest, and of a key ID.
pub const DIGEST_LEN: usize = 32;

/// SEC1's first byte for an uncompressed point.
const UNCOMPRESSED: u8 = 0x04;

/// P-256's group order, n, big-endian (SEC 2, section 2.4.2; FIPS 186-5).
const ORDER: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xBC, 0xE6, 0xFA, 0xAD, 0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63, 0x25, 0x51,
];

/// Why a key, a signature or a check was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The public key is not [`PUBLIC_KEY_LEN`] bytes long.
    KeyLength {
        /// Its length.
        found: usize,
    },
    /// The public key's first byte is not `0x04`: only the uncompressed
    /// form is taken.
    KeyPrefix {
        /// Its first byte.
        found: u8,
    },
    /// The public key is the point at infinity (SEC1's single `0x00`).
    KeyAtInfinity,
    /// The public key's coordinates are not a point on P-256.
    KeyNotOnCurve,
    /// The signature is not [`SIGNATURE_LEN`] bytes long.
    SignatureLength {
        /// Its length.
        found: usize,
    },
    /// The signature's r is zero.
    RIsZero,
    /// The signature's r is not below the group order.
    RNotBelowOrder,
    /// The signature's s is zero.
    SIsZero,
    /// The signature's s is not below the group order.
    SNotBelowOrder,
    /// The signature is well formed but is not one the key made over this
    /// message.
    Mismatch,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyLength { found } => {
                write!(f, "a public key of {found} bytes, not {PUBLIC_KEY_LEN}")
            }
            Self::KeyPrefix { found } => {
                write!(f, "a public key starting {found:#04x}, not 0x04")
            }
            Self::KeyAtInfinity => f.write_str("the public key is the point at infinity"),
            Self::KeyNotOnCurve => f.write_str("the public key is not a point on P-256"),
            Self::SignatureLength { found } => {
                write!(f, "a signature of {found} bytes, not {SIGNATURE_LEN}")
            }
            Self::RIsZero => f.write_str("the signature's r is zero"),
            Self::RNotBelowOrder => f.write_str("the signature's r is not below the group order"),
            Self::SIsZero => f.write_str("the signature's s is zero"),
            Self::SNotBelowOrder => f.write_str("the signature's s is not below the group order"),
            Self::Mismatch => f.write_str("the signature does not match the key and message"),
        }
    }
}

impl core::error::Error for Error {}

/// A P-256 public key that is a point on the curve, other than the point at
/// infinity.
#[derive(Clone, Debug)]
pub struct PublicKey {
    key: p256::ecdsa::VerifyingKey,
    /// Its SEC1 uncompressed encoding, as it was taken.
    sec1: [u8; PUBLIC_KEY_LEN],
}

impl PublicKey {
    /// Takes a public key in SEC1's uncompressed form, `0x04`, x, y.
    ///
    /// # Errors
    ///
    /// [`Error::KeyAtInfinity`] for SEC1's encoding of the point at
    /// infinity; [`Error::KeyLength`] for any other length but
    /// [`PUBLIC_KEY_LEN`]; [`Error::KeyPrefix`] for a first byte other than
    /// `0x04`; [`Error::KeyNotOnCurve`] if the coordinates are not a point
    /// on the curve.
    pub fn from_sec1(bytes: &[u8]) -> Result<Self, Error> {
        let sec1: [u8; PUBLIC_KEY_LEN] = match bytes.try_into() {
            Ok(sec1) => sec1,
            Err(_) if bytes == [0] => return Err(Error::KeyAtInfinity),
            Err(_) => return Err(Error::KeyLength { found: bytes.len() }),
        };
        if let Some(&found) = sec1.first().filter(|&&first| first != UNCOMPRESSED) {
            return Err(Error::KeyPrefix { found });
        }
        let key =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| Error::KeyNotOnCurve)?;
        Ok(Self { key, sec1 })
    }

    /// Takes a public key's two coordinates, x then y, as a trust list
    /// carries them.
    ///
    /// # Errors
    ///
    /// [`Error::KeyNotOnCurve`] if they are not a point on the curve.
    pub fn from_coordinates(coordinates: &[u8; COORDINATES_LEN]) -> Result<Self, Error> {
        let mut sec1 = [UNCOMPRESSED; PUBLIC_KEY_LEN];
        if let Some(xy) = sec1.get_mut(1..) {
            xy.copy_from_slice(coordinates);
        }
        Self::from_sec1(&sec1)
    }

    /// The key in SEC1's uncompressed form.
    #[must_use]
    pub const fn to_sec1(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.sec1
    }

    /// The key's ID: the SHA-256 of its SEC1 uncompressed encoding - the
    /// value of the `subjectPublicKey` bit string in its X.509
    /// `SubjectPublicKeyInfo`, as RFC 7093's first method hashes it,
    /// untruncated. What a signature names its key by in NoWayOS's update
    /// records.
    #[must_use]
    pub fn id(&self) -> [u8; DIGEST_LEN] {
        sha256(&self.sec1)
    }
}

impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        self.sec1 == other.sec1
    }
}

impl Eq for PublicKey {}

/// A signature whose r and s are each from 1 to the group order less one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature(p256::ecdsa::Signature);

impl Signature {
    /// Takes a signature as r then s, big-endian, [`SIGNATURE_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`Error::SignatureLength`] for any other length; [`Error::RIsZero`],
    /// [`Error::RNotBelowOrder`], [`Error::SIsZero`] or
    /// [`Error::SNotBelowOrder`] for a value out of range, r first.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let (r, s) = bytes
            .split_first_chunk::<32>()
            .and_then(|(r, s)| Some((r, <&[u8; 32]>::try_from(s).ok()?)))
            .ok_or(Error::SignatureLength { found: bytes.len() })?;
        let r_error = scalar_error(r, Error::RIsZero, Error::RNotBelowOrder);
        if let Some(error) = r_error {
            return Err(error);
        }
        let s_error = scalar_error(s, Error::SIsZero, Error::SNotBelowOrder);
        if let Some(error) = s_error {
            return Err(error);
        }
        // `p256` checks the same range again; it cannot refuse what passed
        // above, and if it ever did, the refusal still stands.
        p256::ecdsa::Signature::from_scalars(*r, *s)
            .map(Self)
            .map_err(|_| Error::SNotBelowOrder)
    }

    /// The signature as r then s, big-endian.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIGNATURE_LEN] {
        self.0.to_bytes().into()
    }
}

/// The error for a scalar that is zero or not below the group order, if it
/// is either.
fn scalar_error(scalar: &[u8; 32], zero: Error, too_large: Error) -> Option<Error> {
    if scalar.iter().all(|byte| *byte == 0) {
        Some(zero)
    } else if scalar >= &ORDER {
        // Big-endian bytes of one length compare as the numbers they are.
        Some(too_large)
    } else {
        None
    }
}

/// Checks that `signature` is one `key` made over `message`, hashed with
/// SHA-256.
///
/// # Errors
///
/// [`Error::Mismatch`] if it is not.
pub fn verify(key: &PublicKey, message: &[u8], signature: &Signature) -> Result<(), Error> {
    key.key
        .verify(message, &signature.0)
        .map_err(|_| Error::Mismatch)
}

/// Checks that `signature` is one `key` made over a message whose SHA-256
/// is `digest`: for a message hashed as it arrives, such as an image.
///
/// # Errors
///
/// [`Error::Mismatch`] if it is not.
pub fn verify_digest(
    key: &PublicKey,
    digest: &[u8; DIGEST_LEN],
    signature: &Signature,
) -> Result<(), Error> {
    key.key
        .verify_prehash(digest, &signature.0)
        .map_err(|_| Error::Mismatch)
}

/// Takes `key` (SEC1 uncompressed) and `signature` (r then s) and checks
/// the signature over `message`, in one step.
///
/// # Errors
///
/// The key's error, then the signature's, then [`Error::Mismatch`].
pub fn check(key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), Error> {
    let key = PublicKey::from_sec1(key)?;
    let signature = Signature::from_bytes(signature)?;
    verify(&key, message, &signature)
}

/// The SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> [u8; DIGEST_LEN] {
    sha2::Sha256::digest(bytes).into()
}

/// SHA-256 over bytes that arrive in pieces.
#[derive(Clone, Debug, Default)]
pub struct Sha256(sha2::Sha256);

impl Sha256 {
    /// A digest of nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `bytes` to what is hashed.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The SHA-256 of everything added.
    #[must_use]
    pub fn finish(self) -> [u8; DIGEST_LEN] {
        self.0.finalize().into()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn the_order_is_p256s() {
        // n - 1 is the largest r or s; n itself is refused.
        let mut largest = ORDER;
        largest[31] -= 1;
        assert_eq!(
            scalar_error(&largest, Error::RIsZero, Error::RNotBelowOrder),
            None
        );
        assert_eq!(
            scalar_error(&ORDER, Error::RIsZero, Error::RNotBelowOrder),
            Some(Error::RNotBelowOrder)
        );
    }

    #[test]
    fn sha256_matches_fips_180_4() {
        // FIPS 180-4's "abc" example, and the empty string.
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        let mut pieces = Sha256::new();
        pieces.update(b"a");
        pieces.update(b"");
        pieces.update(b"bc");
        assert_eq!(pieces.finish(), sha256(b"abc"));
        assert_eq!(sha256(b"")[..4], [0xe3, 0xb0, 0xc4, 0x42]);
    }

    #[test]
    fn every_error_says_what_it_is() {
        for error in [
            Error::KeyLength { found: 33 },
            Error::KeyPrefix { found: 2 },
            Error::KeyAtInfinity,
            Error::KeyNotOnCurve,
            Error::SignatureLength { found: 71 },
            Error::RIsZero,
            Error::RNotBelowOrder,
            Error::SIsZero,
            Error::SNotBelowOrder,
            Error::Mismatch,
        ] {
            assert!(!error.to_string().is_empty());
        }
    }
}
