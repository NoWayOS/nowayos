// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: each record's decoder - `decode` - given any byte
//! string up to one byte longer than its record never panics, overflows or
//! reads out of bounds, and what it accepts encodes back to the same
//! record; and the recogniser, given any input up to one byte longer than
//! the longest, never panics, and accepts an input only if it has the
//! client's form - 64 spaces, the client's context string, a zero byte,
//! then a 32- or 48-byte transcript hash, which it returns.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{Message, decode, encode, encoded_len};

use crate::{
    CLIENT_CONTEXT, INPUT_MAX, KeyCode, PADDING_LEN, PREFIX_LEN, SignAnswer, SignRequest, recognise,
};

/// `buf`'s first `len` bytes, for any `len` up to `N`.
fn any_prefix<const N: usize>(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any_where(|len: &usize| *len <= N);
    &buf[..len]
}

/// What `M`'s decoder accepts encodes back to the bytes it read.
fn decodes_then_round_trips<M: Message, const N: usize>(buf: &[u8; N]) {
    let bytes = any_prefix(buf);
    if let Ok(message) = decode::<M>(bytes) {
        let mut again = [0u8; N];
        let len = encoded_len::<M>();
        assert!(encode(&message, &mut again) == Ok(len));
        assert!(again[..len] == bytes[..len]);
    }
}

#[kani::proof]
#[kani::unwind(158)]
fn a_sign_request_from_any_bytes() {
    let buf: [u8; 156] = kani::any();
    decodes_then_round_trips::<SignRequest, 156>(&buf);
}

#[kani::proof]
#[kani::unwind(76)]
fn a_sign_answer_from_any_bytes() {
    let buf: [u8; 74] = kani::any();
    decodes_then_round_trips::<SignAnswer, 74>(&buf);
}

#[kani::proof]
#[kani::unwind(20)]
fn a_key_code_from_any_bytes() {
    let buf: [u8; 18] = kani::any();
    decodes_then_round_trips::<KeyCode, 18>(&buf);
}

#[kani::proof]
#[kani::unwind(150)]
fn the_recogniser_accepts_only_the_client_form() {
    let buf: [u8; INPUT_MAX + 1] = kani::any();
    let input = any_prefix(&buf);
    if let Ok(hash) = recognise(input) {
        assert!(input.len() == PREFIX_LEN + 32 || input.len() == PREFIX_LEN + 48);
        assert!(input[..PADDING_LEN].iter().all(|byte| *byte == b' '));
        assert!(&input[PADDING_LEN..PREFIX_LEN - 1] == CLIENT_CONTEXT);
        assert!(input[PREFIX_LEN - 1] == 0);
        assert!(hash.as_bytes() == &input[PREFIX_LEN..]);
    }
}
