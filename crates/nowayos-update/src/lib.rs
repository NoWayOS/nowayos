// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update records: what the update PD reads before it trusts anything
//! about an update.
//!
//! An update is a whole system image and a manifest an image key signs, and
//! which keys may sign images, configuration and the next trust list is the
//! **trust list**, which only the root key signs.
//! The update
//! PD receives each record from a PD it does not trust, copies it into its
//! own memory, and decodes it here; only a record that decodes is checked
//! further. Checking a record's signatures, and deciding whether to take
//! it, belong to the update PD's logic, not to this crate.
//!
//! The records so far:
//!
//! * [`TrustList`] - the signed trust list: its version, a development mark,
//!   up to [`trust_list::MAX_KEYS`] P-256 public keys, each with its
//!   [`Purpose`], and one or two signatures, each naming its key.
//! * [`Manifest`] - the update manifest: the product, the
//!   release's [`Version`] and security version, the image's length and
//!   SHA-256, the trust-list version it needs, a development mark and up to
//!   [`manifest::MAX_REFERENCE_VALUES`] reference values, kept opaque; then
//!   the image key's signature, naming its key.
//! * [`Chunk`] - up to [`chunk::CHUNK_LEN`] bytes of one [`Part`] of a
//!   bundle (the trust list, the manifest or the image) and their offset in
//!   it: the fixed-size record a bundle arrives in on the update PD's
//!   channel.
//! * [`state::UpdateState`] - the update state: which slot boots,
//!   a trial pending, the rollback index and the trust list held, kept in
//!   two copies over [`state::StateStorage`] so that a reset mid-write
//!   leaves the old state or the new one, and no write lowers the index or
//!   the trust list's version.
//! * [`updater::Updater`] - the update PD's decisions: which trust
//!   list to take, which manifest to accept, where the image goes and when
//!   it is marked good, driven by events, with the signature checks and
//!   storage the PD gives it.
//!
//! # The records' rules
//!
//! Each record is a `nowayos-ipc` record (its 8-byte header names its kind,
//! layout version and payload length) with a fixed-size payload, laid out
//! field by field: no general-purpose parser, no pointers, nothing whose
//! meaning depends on the sender. Integers are little-endian; keys, key IDs
//! and signatures are byte strings in their own big-endian encodings. Its
//! decoder refuses everything but the one encoding of each value - unused
//! entries and reserved bytes must be zero - so a record that decodes
//! encodes back to exactly the bytes it came from, and its signatures can be
//! checked over those bytes. Each record has its own typed error, whose
//! `Frame` variant carries what `nowayos-ipc` would report for a malformed
//! frame (too short, another kind, version or length, the header's reserved
//! field set, bytes after the record).
//!
//! `nowayos_ipc::decode_exact` decodes the same records, with
//! `nowayos-ipc`'s coarser errors; the two agree on every input (property
//! tests and fuzzing; Kani proves only that each decoder returns, never
//! panics, overflows or reads out of bounds, and accepts only a whole
//! record).
//!
//! # Keys and signatures
//!
//! * A **public key** is a P-256 point as its two coordinates, x then y,
//!   [`KEY_LEN`] bytes, big-endian: SEC1's uncompressed encoding without its
//!   leading `0x04`. A record does not check that the point is on the curve;
//!   the signature check does, when the key is used.
//! * A **key ID** names a key: the SHA-256 of its SEC1 uncompressed encoding
//!   (`0x04`, x, y: 65 bytes) - the value of the `subjectPublicKey` bit
//!   string in the key's X.509 `SubjectPublicKeyInfo`, as RFC 7093's first
//!   method hashes it, untruncated. [`KEY_ID_LEN`] bytes.
//! * A **signature** is ECDSA over P-256 with SHA-256: r then s,
//!   [`SIGNATURE_LEN`] bytes, big-endian, as an HSM's PKCS#11 interface
//!   returns it (`CKM_ECDSA`).
//!
//! # Status
//!
//! Provisional: ahead of the update PD. No expiry: a
//! list or manifest that expires needs trusted time, and would come as a new
//! layout version. The record kinds are the update PD's
//! channel's own: 1 the trust list, 2 the manifest, 3 a chunk; and 4 the
//! update state's record, which storage holds.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod chunk;
pub mod manifest;
#[cfg(kani)]
mod proofs;
mod record;
pub mod state;
pub mod trust_list;
pub mod updater;

pub use chunk::{Chunk, ChunkError, Part};
pub use manifest::{Manifest, ManifestContents, ManifestError, ManifestFields, Version};
pub use trust_list::{ListedKey, Purpose, TrustList, TrustListContents, TrustListError};

/// The length of a P-256 public key in a record: x then y, 32 bytes each.
pub const KEY_LEN: usize = 64;

/// The length of a key ID: a SHA-256.
pub const KEY_ID_LEN: usize = 32;

/// The length of an ECDSA P-256 signature: r then s, 32 bytes each.
pub const SIGNATURE_LEN: usize = 64;

/// The length of a SHA-256 digest: an image's hash, or a reference value.
pub const DIGEST_LEN: usize = 32;

/// A signature, naming the key that made it by its key ID (the crate
/// documentation, "Keys and signatures"). Plain data: what it names is
/// checked only when the signature is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedSignature {
    /// The signing key's ID: the SHA-256 of its SEC1 uncompressed encoding.
    pub key_id: [u8; KEY_ID_LEN],
    /// The signature: r then s, big-endian.
    pub signature: [u8; SIGNATURE_LEN],
}
