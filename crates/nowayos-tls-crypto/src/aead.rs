// SPDX-License-Identifier: MIT OR Apache-2.0
//! TLS 1.3's record protection over RustCrypto's AEADs (`aead` 0.5):
//! AES-128-GCM and AES-256-GCM (`aes-gcm` 0.10) and ChaCha20-Poly1305
//! (`chacha20poly1305` 0.10).
//!
//! One generic type serves all three: every TLS 1.3 AEAD has a 12-byte
//! nonce and a 16-byte tag, and the record layer around it (RFC 8446,
//! section 5.2) is the same. The same type can carry TLS 1.2's AEAD suites
//! later, with a `Tls12AeadAlgorithm` of its own.

use alloc::boxed::Box;
use core::marker::PhantomData;

use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::{AeadCore, AeadInPlace, Buffer, KeyInit, KeySizeUser, Nonce};
use rustls::crypto::cipher::{
    AeadKey, BorrowedPayload, InboundOpaqueMessage, InboundPlainMessage, Iv, MessageDecrypter,
    MessageEncrypter, Nonce as TlsNonce, OutboundOpaqueMessage, OutboundPlainMessage,
    PrefixedPayload, Tls13AeadAlgorithm, UnsupportedOperationError, make_tls13_aad,
};
use rustls::{ConnectionTrafficSecrets, ContentType, ProtocolVersion};

/// The length of every TLS 1.3 AEAD's tag.
const TAG_LEN: usize = 16;

/// How many records one AES-GCM key may protect (RFC 8446, section 5.5:
/// 2^24.5 full-size records; rustls's own providers use 2^24). rustls
/// updates the key, or closes the connection, before it is reached.
pub(crate) const GCM_CONFIDENTIALITY_LIMIT: u64 = 1 << 24;

/// A TLS 1.3 AEAD algorithm made of the RustCrypto AEAD `A`.
pub(crate) struct Aead<A>(PhantomData<fn() -> A>);

pub(crate) static AES_128_GCM: Aead<aes_gcm::Aes128Gcm> = Aead(PhantomData);
pub(crate) static AES_256_GCM: Aead<aes_gcm::Aes256Gcm> = Aead(PhantomData);
pub(crate) static CHACHA20_POLY1305: Aead<chacha20poly1305::ChaCha20Poly1305> = Aead(PhantomData);

impl<A> Aead<A>
where
    A: AeadInPlace + AeadCore<NonceSize = U12, TagSize = U16> + KeyInit + Send + Sync + 'static,
{
    /// The record protection for `key` and `iv`. rustls always passes a key
    /// of [`Tls13AeadAlgorithm::key_len`] bytes; were it ever another
    /// length, every record would fail rather than anything panic.
    fn cipher(key: &AeadKey, iv: Iv) -> Box<Tls13Cipher<A>> {
        Box::new(Tls13Cipher {
            cipher: A::new_from_slice(key.as_ref()).ok(),
            iv,
        })
    }
}

impl<A> Tls13AeadAlgorithm for Aead<A>
where
    A: AeadInPlace + AeadCore<NonceSize = U12, TagSize = U16> + KeyInit + Send + Sync + 'static,
{
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Self::cipher(&key, iv)
    }

    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Self::cipher(&key, iv)
    }

    fn key_len(&self) -> usize {
        <A as KeySizeUser>::key_size()
    }

    /// Refused: traffic keys never leave the provider (no kernel TLS here).
    fn extract_keys(
        &self,
        _key: AeadKey,
        _iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Err(UnsupportedOperationError)
    }
}

/// One direction's record protection: the AEAD keyed for it (`None` only if
/// rustls ever passed a key of the wrong length) and its IV.
struct Tls13Cipher<A> {
    cipher: Option<A>,
    iv: Iv,
}

impl<A> Tls13Cipher<A>
where
    A: AeadCore<NonceSize = U12>,
{
    /// The nonce for record `seq`: the IV XOR the sequence number
    /// (RFC 8446, section 5.3).
    fn nonce(&self, seq: u64) -> Nonce<A> {
        Nonce::<A>::from(TlsNonce::new(&self.iv, seq).0)
    }
}

impl<A> MessageEncrypter for Tls13Cipher<A>
where
    A: AeadInPlace + AeadCore<NonceSize = U12, TagSize = U16> + Send + Sync,
{
    fn encrypt(
        &mut self,
        m: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, rustls::Error> {
        let cipher = self.cipher.as_ref().ok_or(rustls::Error::EncryptError)?;
        let total_len = self.encrypted_payload_len(m.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        payload.extend_from_chunks(&m.payload);
        payload.extend_from_slice(&m.typ.to_array());
        let nonce = self.nonce(seq);
        let aad = make_tls13_aad(total_len);
        cipher
            .encrypt_in_place(&nonce, &aad, &mut EncryptBuffer(&mut payload))
            .map_err(|_| rustls::Error::EncryptError)?;
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        ))
    }

    /// The plaintext, its one content-type byte and the tag.
    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len.saturating_add(1).saturating_add(TAG_LEN)
    }
}

impl<A> MessageDecrypter for Tls13Cipher<A>
where
    A: AeadInPlace + AeadCore<NonceSize = U12, TagSize = U16> + Send + Sync,
{
    /// Opens a record in place. Any failure - a record shorter than a tag,
    /// a tag that does not match - is [`rustls::Error::DecryptError`], on
    /// which rustls sends `bad_record_mac` and ends the connection; the
    /// record's bytes are never handed on.
    fn decrypt<'a>(
        &mut self,
        mut m: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, rustls::Error> {
        let cipher = self.cipher.as_ref().ok_or(rustls::Error::DecryptError)?;
        let nonce = self.nonce(seq);
        let aad = make_tls13_aad(m.payload.len());
        cipher
            .decrypt_in_place(&nonce, &aad, &mut DecryptBuffer(&mut m.payload))
            .map_err(|_| rustls::Error::DecryptError)?;
        m.into_tls13_unpadded_message()
    }
}

/// An outgoing record's payload as the `aead` crate's [`Buffer`]: the tag is
/// appended to it.
struct EncryptBuffer<'a>(&'a mut PrefixedPayload);

impl AsRef<[u8]> for EncryptBuffer<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl AsMut<[u8]> for EncryptBuffer<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut()
    }
}

impl Buffer for EncryptBuffer<'_> {
    fn extend_from_slice(&mut self, other: &[u8]) -> aes_gcm::aead::Result<()> {
        self.0.extend_from_slice(other);
        Ok(())
    }

    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}

/// An incoming record's payload as the `aead` crate's [`Buffer`]: opening it
/// only ever shortens it (by the tag).
struct DecryptBuffer<'a, 'p>(&'a mut BorrowedPayload<'p>);

impl AsRef<[u8]> for DecryptBuffer<'_, '_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

impl AsMut<[u8]> for DecryptBuffer<'_, '_> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0
    }
}

impl Buffer for DecryptBuffer<'_, '_> {
    /// Never called by `decrypt_in_place`; refused if it ever were.
    fn extend_from_slice(&mut self, _other: &[u8]) -> aes_gcm::aead::Result<()> {
        Err(aes_gcm::aead::Error)
    }

    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}
