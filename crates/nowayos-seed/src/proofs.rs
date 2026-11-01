// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: the seed record's decoder - `decode` and
//! `decode_exact` - given any byte string up to [`SEED_RING_SLOT`] bytes
//! never panics, overflows or reads out of bounds; and from any payload
//! behind its own header it accepts exactly the payloads that are not all
//! zero, keeping their bytes, and encodes back to the same record, so a seed
//! has one encoding. The fold to 64 bits never panics or overflows.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, decode, decode_exact, encode};

use crate::{SEED_LEN, SEED_RECORD_LEN, SEED_RING_SLOT, Seed};

/// `buf`'s first `len` bytes, for any `len` up to `N`.
fn any_prefix<const N: usize>(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any_where(|len: &usize| *len <= N);
    &buf[..len]
}

/// Any bytes up to a whole slot: a record, short of one, or longer.
#[kani::proof]
#[kani::unwind(66)]
fn decoding_any_bytes_never_panics() {
    let buf: [u8; SEED_RING_SLOT] = kani::any();
    let bytes = any_prefix(&buf);
    if let Ok(seed) = decode::<Seed>(bytes) {
        assert!(bytes.len() >= SEED_RECORD_LEN);
        assert!(seed.as_bytes().iter().any(|byte| *byte != 0));
    }
    if decode_exact::<Seed>(bytes).is_ok() {
        assert!(bytes.len() == SEED_RECORD_LEN);
    }
}

/// Any payload behind the seed's own header: accepted exactly when it is
/// not all zero, with its bytes kept, and encoded back to the same record.
#[kani::proof]
#[kani::unwind(66)]
fn any_payload_is_a_seed_exactly_when_not_all_zero() {
    let mut record = [0u8; SEED_RECORD_LEN];
    let header = Header {
        kind: Seed::KIND,
        version: Seed::VERSION,
        payload_len: SEED_LEN as u16,
    };
    assert!(header.write(&mut record).is_ok());
    let payload: [u8; SEED_LEN] = kani::any();
    record[HEADER_LEN..].copy_from_slice(&payload);
    let zero = payload.iter().all(|byte| *byte == 0);
    match decode_exact::<Seed>(&record) {
        Ok(seed) => {
            assert!(!zero);
            assert!(seed.as_bytes() == &payload);
            let mut again = [0u8; SEED_RECORD_LEN];
            assert!(encode(&seed, &mut again) == Ok(SEED_RECORD_LEN));
            assert!(again == record);
        }
        Err(_) => assert!(zero),
    }
}

/// The fold to 64 bits never panics or overflows, for any seed.
#[kani::proof]
#[kani::unwind(34)]
fn folding_never_panics() {
    let bytes: [u8; SEED_LEN] = kani::any();
    if let Some(seed) = Seed::new(bytes) {
        let _ = seed.fold_u64();
    }
}
