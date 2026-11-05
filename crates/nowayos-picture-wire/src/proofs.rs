// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: any payload decodes to a [`Decode`] or [`Decoded`] whose
//! encoding is exactly those bytes, or to a typed error - never a panic or
//! an overflow - and a picture a [`Decoded`] names always fits the pixel
//! region.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, encode};

use crate::{Decode, Decoded, PIXEL_BYTES, pixel_len};

/// The record length of both: an 8-byte payload.
const RECORD_LEN: usize = HEADER_LEN + 8;

/// A record of `M`'s kind with `payload` behind its header.
fn record<M: Message>(payload: [u8; 8]) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    let header = Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: 8,
    };
    assert!(header.write(&mut record).is_ok());
    record[HEADER_LEN..].copy_from_slice(&payload);
    record
}

#[kani::proof]
#[kani::unwind(17)]
fn any_decode_payload_is_its_one_encoding() {
    let bytes = record::<Decode>(kani::any());
    if let Ok(request) = nowayos_ipc::decode_exact::<Decode>(&bytes) {
        let mut out = [0u8; RECORD_LEN];
        assert!(encode(&request, &mut out) == Ok(RECORD_LEN));
        assert!(out == bytes);
    }
}

#[kani::proof]
#[kani::unwind(17)]
fn any_decoded_payload_is_its_one_encoding_and_fits_the_region() {
    let bytes = record::<Decoded>(kani::any());
    if let Ok(answer) = nowayos_ipc::decode_exact::<Decoded>(&bytes) {
        let mut out = [0u8; RECORD_LEN];
        assert!(encode(&answer, &mut out) == Ok(RECORD_LEN));
        assert!(out == bytes);
        if let Ok((width, height)) = answer.outcome {
            assert!(pixel_len(width, height) <= PIXEL_BYTES);
        }
    }
}
