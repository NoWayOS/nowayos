// SPDX-License-Identifier: MIT OR Apache-2.0
//! Key exchange: X25519 (`x25519-dalek` 2) and P-256 ECDH (`p256` 0.13),
//! each ephemeral private key drawn from the caller's random source.

use alloc::boxed::Box;
use alloc::string::String;

use p256::elliptic_curve::sec1::ToSec1Point;
use rustls::crypto::{ActiveKeyExchange, SecureRandom, SharedSecret, SupportedKxGroup};
use rustls::{NamedGroup, PeerMisbehaved};
use zeroize::Zeroizing;

/// The length of an X25519 key, public or private, and of its shared secret.
const X25519_LEN: usize = 32;

/// The length of a P-256 key share: SEC1's uncompressed form, `0x04`, x, y
/// (RFC 8446, section 4.2.8.2: the only form TLS 1.3 allows).
const P256_SHARE_LEN: usize = 65;

/// SEC1's first byte of an uncompressed point.
const SEC1_UNCOMPRESSED: u8 = 0x04;

/// How many times P-256 draws a private key before giving up: a draw is
/// refused only when it is zero or not below the group order, about once in
/// 2^32 draws, so a source that fails this often is not random.
const P256_DRAWS: usize = 8;

/// The X25519 group (RFC 7748).
#[derive(Debug)]
pub(crate) struct X25519 {
    random: &'static dyn SecureRandom,
}

impl X25519 {
    pub(crate) fn new(random: &'static dyn SecureRandom) -> Self {
        Self { random }
    }
}

impl SupportedKxGroup for X25519 {
    /// Draws 32 bytes from the random source as the private key (X25519
    /// clamps them itself). Fails, with no key made, if the source fails.
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, rustls::Error> {
        let mut bytes = Zeroizing::new([0u8; X25519_LEN]);
        self.random.fill(bytes.as_mut())?;
        let private = x25519_dalek::StaticSecret::from(*bytes);
        let public = x25519_dalek::PublicKey::from(&private);
        Ok(Box::new(X25519Exchange { private, public }))
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// An X25519 exchange in progress. The private key is wiped when dropped.
struct X25519Exchange {
    private: x25519_dalek::StaticSecret,
    public: x25519_dalek::PublicKey,
}

impl ActiveKeyExchange for X25519Exchange {
    /// Refuses a peer's share that is not 32 bytes, and one that makes the
    /// all-zero shared secret (a small-order point), as RFC 8446 (section
    /// 7.4.2) requires: [`PeerMisbehaved::InvalidKeyShare`].
    fn complete(self: Box<Self>, peer_pub_key: &[u8]) -> Result<SharedSecret, rustls::Error> {
        let peer: [u8; X25519_LEN] = peer_pub_key
            .try_into()
            .map_err(|_| rustls::Error::from(PeerMisbehaved::InvalidKeyShare))?;
        let shared = self
            .private
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(PeerMisbehaved::InvalidKeyShare.into());
        }
        Ok(SharedSecret::from(shared.as_bytes().as_slice()))
    }

    fn pub_key(&self) -> &[u8] {
        self.public.as_bytes()
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// The P-256 group (`secp256r1`).
#[derive(Debug)]
pub(crate) struct P256 {
    random: &'static dyn SecureRandom,
}

impl P256 {
    pub(crate) fn new(random: &'static dyn SecureRandom) -> Self {
        Self { random }
    }
}

impl SupportedKxGroup for P256 {
    /// Draws a private key from the random source: 32 bytes, drawn again
    /// when they are zero or not below the group order. Fails, with no key
    /// made, if the source fails or after [`P256_DRAWS`] refused draws.
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, rustls::Error> {
        for _ in 0..P256_DRAWS {
            let mut bytes = Zeroizing::new(p256::FieldBytes::default());
            self.random.fill(bytes.as_mut_slice())?;
            if let Ok(private) = p256::SecretKey::from_bytes(&bytes) {
                let point = private.public_key().to_sec1_point(false);
                let public = point.as_bytes().try_into().map_err(|_| {
                    rustls::Error::General(String::from("a P-256 public key is 65 bytes"))
                })?;
                return Ok(Box::new(P256Exchange { private, public }));
            }
        }
        Err(rustls::Error::FailedToGetRandomBytes)
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }
}

/// A P-256 exchange in progress. The private key is wiped when dropped.
struct P256Exchange {
    private: p256::SecretKey,
    public: [u8; P256_SHARE_LEN],
}

impl ActiveKeyExchange for P256Exchange {
    /// Refuses a peer's share that is not an uncompressed point on the
    /// curve, or is the point at infinity: [`PeerMisbehaved::InvalidKeyShare`].
    /// The shared secret is the x-coordinate (RFC 8446, section 7.4.2).
    fn complete(self: Box<Self>, peer_pub_key: &[u8]) -> Result<SharedSecret, rustls::Error> {
        if peer_pub_key.len() != P256_SHARE_LEN || peer_pub_key.first() != Some(&SEC1_UNCOMPRESSED)
        {
            return Err(PeerMisbehaved::InvalidKeyShare.into());
        }
        let peer = p256::PublicKey::from_sec1_bytes(peer_pub_key)
            .map_err(|_| rustls::Error::from(PeerMisbehaved::InvalidKeyShare))?;
        let shared = p256::ecdh::diffie_hellman(self.private.to_nonzero_scalar(), peer.as_affine());
        Ok(SharedSecret::from(shared.raw_secret_bytes().as_slice()))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }
}
