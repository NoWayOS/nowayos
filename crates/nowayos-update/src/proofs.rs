// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: for every byte string up to one byte
//! longer than its record, each update record's decoder -
//! [`TrustList::decode`], [`Manifest::decode`], [`Chunk::decode`] and
//! [`StateRecord::decode`] - never panics, overflows or
//! reads out of bounds, and accepts only a whole record; what the manifest
//! and chunk decoders accept keeps their rules on lengths and offsets.
//!
//! Each decoder keeps a record's checked bytes and reads what it says from
//! them, so a record it accepts encodes back to exactly those bytes; the
//! property tests and the fuzz targets (`update_trust_list`,
//! `update_manifest`, `update_chunk`) show that, that what it accepts keeps
//! every rule, and that it refuses exactly what `nowayos_ipc::decode_exact`
//! refuses.
//!
//! Every loop the decoders run is over a whole array or a constant range,
//! never over a slice whose length depends on the input, so Kani unrolls
//! each one exactly; each proof's bound covers its longest: 96 bytes
//! compared with zeros in the trust list, a manifest's 32-byte product name
//! and its 32 reference values, and a chunk's 488 bytes.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use crate::chunk::{self, CHUNK_LEN};
use crate::manifest::{self, MAX_PRODUCT_LEN, MAX_REFERENCE_VALUES, is_product_character};
use crate::state::{self, StateRecord};
use crate::trust_list::RECORD_LEN;
use crate::{Chunk, Manifest, TrustList};

/// Any byte string of at most this many bytes: a whole record, short of it,
/// and one byte past it.
const INPUT_LEN: usize = RECORD_LEN + 1;

/// Any input: the decoder returns, and accepts only a whole record.
#[kani::proof]
#[kani::unwind(100)]
fn trust_list_any_input() {
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    let decoded = TrustList::decode(&buf[..len]);
    assert!(decoded.is_err() || len == RECORD_LEN);
}

/// Any input up to 1225 bytes: the decoder returns, accepts only a whole
/// record, and a manifest it accepts names a product of 1 to 32 allowed
/// characters, a non-empty image and at most 32 reference values.
#[kani::proof]
#[kani::unwind(34)]
fn manifest_any_input() {
    const INPUT_LEN: usize = manifest::RECORD_LEN + 1;
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    match Manifest::decode(&buf[..len]) {
        Ok(manifest) => {
            assert!(len == manifest::RECORD_LEN);
            // The name's bytes, not `product()`, whose UTF-8 check Kani
            // cannot unwind in reasonable time; the property tests and the
            // fuzz target check that the two agree.
            let product = manifest.contents().product_bytes();
            assert!(!product.is_empty() && product.len() <= MAX_PRODUCT_LEN);
            let at: usize = kani::any_where(|at: &usize| *at < product.len());
            assert!(is_product_character(product[at]));
            assert!(manifest.image_len() != 0);
            assert!(manifest.contents().reference_value_count() <= MAX_REFERENCE_VALUES);
        }
        Err(_) => {}
    }
}

/// Any input up to 513 bytes: the decoder returns, accepts only a whole
/// record, and a chunk it accepts carries 1 to 488 bytes and ends where its
/// offset plus their number says, without overflow.
#[kani::proof]
#[kani::unwind(490)]
fn chunk_any_input() {
    const INPUT_LEN: usize = chunk::RECORD_LEN + 1;
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    match Chunk::decode(&buf[..len]) {
        Ok(chunk) => {
            assert!(len == chunk::RECORD_LEN);
            let count = chunk.bytes().len();
            assert!(count >= 1 && count <= CHUNK_LEN);
            let count = u64::try_from(count).unwrap_or(u64::MAX);
            assert!(chunk.offset().checked_add(count) == Some(chunk.end()));
        }
        Err(_) => {}
    }
}

/// Any input up to 57 bytes: the state record's decoder returns, accepts
/// only a whole record, and a record it accepts never puts the trial in the
/// slot that boots, nor below the rollback index. The copy around
/// it - its CRC, generation and held trust list - is the property tests'
/// and the `update_state` fuzz target's; the held list's decoder is
/// [`trust_list_any_input`]'s.
#[kani::proof]
#[kani::unwind(34)]
fn state_record_any_input() {
    const INPUT_LEN: usize = state::STATE_RECORD_LEN + 1;
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    match StateRecord::decode(&buf[..len]) {
        Ok(record) => {
            assert!(len == state::STATE_RECORD_LEN);
            if let Some(trial) = record.trial {
                assert!(trial.slot != record.boots);
                assert!(trial.security_version >= record.rollback_index);
            }
        }
        Err(_) => {}
    }
}
