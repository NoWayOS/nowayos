// SPDX-License-Identifier: MIT OR Apache-2.0
//! rustls's HMAC trait over RustCrypto's `hmac` 0.12 and SHA-2: HMAC-SHA256
//! and HMAC-SHA384, for HKDF (TLS 1.3's key schedule) and, later, TLS 1.2's
//! PRF.

use alloc::boxed::Box;
use core::marker::PhantomData;

use hmac::digest::{KeyInit, OutputSizeUser};
use hmac::{Hmac, Mac};
use rustls::crypto::hmac::{Hmac as RustlsHmac, Key, Tag};

/// An HMAC type `M` as rustls's [`RustlsHmac`].
pub(crate) struct Sha2Hmac<M>(PhantomData<fn() -> M>);

pub(crate) static SHA256: Sha2Hmac<Hmac<sha2_010::Sha256>> = Sha2Hmac(PhantomData);
pub(crate) static SHA384: Sha2Hmac<Hmac<sha2_010::Sha384>> = Sha2Hmac(PhantomData);

impl<M> RustlsHmac for Sha2Hmac<M>
where
    M: Mac + KeyInit + Clone + Send + Sync + 'static,
{
    #[allow(
        clippy::expect_used,
        reason = "HMAC takes a key of any length (RFC 2104 hashes a long one and pads a short \
                  one), so hmac 0.12's new_from_slice never fails; a stand-in key would leave \
                  every TLS secret derived from it known, so an impossible failure stops the PD \
                  instead"
    )]
    fn with_key(&self, key: &[u8]) -> Box<dyn Key> {
        let mac = <M as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
        Box::new(HmacKey(mac))
    }

    fn hash_output_len(&self) -> usize {
        <M as OutputSizeUser>::output_size()
    }
}

/// An HMAC key, ready to sign.
struct HmacKey<M>(M);

impl<M> Key for HmacKey<M>
where
    M: Mac + Clone + Send + Sync + 'static,
{
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> Tag {
        let mut mac = self.0.clone();
        Mac::update(&mut mac, first);
        for part in middle {
            Mac::update(&mut mac, part);
        }
        Mac::update(&mut mac, last);
        Tag::new(&mac.finalize().into_bytes())
    }

    fn tag_len(&self) -> usize {
        <M as OutputSizeUser>::output_size()
    }
}
