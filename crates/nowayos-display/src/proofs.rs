// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: each display-contract record — [`Configure`],
//! [`Present`] and [`PresentDone`] — decoded from any byte string up to
//! [`MAX_RECORD_LEN`] bytes and one more never panics, overflows or reads
//! out of bounds; and from any payload, what it accepts keeps the
//! contract's rules and encodes back to exactly the bytes it came from, so
//! every message has one encoding.
//!
//! Each record has two proofs. The first gives any input at all to
//! `decode` and `decode_exact` and checks only that they return. The
//! second puts any payload behind the record's own header, the one input
//! that reaches the payload's decoder whole (nowayos-ipc's proofs show
//! `decode` hands a payload's decoder exactly the record's payload), and
//! checks what it accepts.
//!
//! Every proof is unwound ten times: `Present`'s decoder loops over at most
//! eight rectangles and reads at most eight zero bytes at a time, and nothing
//! here loops over the input's length. Kani checks that bound is enough.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, decode, decode_exact, encode, encoded_len};

use crate::{Configure, MAX_DAMAGE_RECTS, MAX_RECORD_LEN, Present, PresentDone};

/// Any byte string of at most `MAX_RECORD_LEN + 1` bytes: every record of
/// the contract, short of it, and one byte past the longest.
const INPUT_LEN: usize = MAX_RECORD_LEN + 1;

/// `decode` and `decode_exact` of `M` return, whatever the input.
fn any_input_returns<M: Message>() {
    let buf: [u8; INPUT_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= INPUT_LEN);
    let bytes = &buf[..len];
    let prefix = decode::<M>(bytes).is_ok();
    let exact = decode_exact::<M>(bytes).is_ok();
    assert!(!exact || prefix);
}

/// The two records are the same bytes, compared as five 16-byte words:
/// no loop that Kani would have to unwind.
fn same(a: &[u8; MAX_RECORD_LEN], b: &[u8; MAX_RECORD_LEN]) -> bool {
    const _: () = assert!(MAX_RECORD_LEN == 5 * 16);
    let word = |bytes: &[u8; MAX_RECORD_LEN], index: usize| {
        let mut word = [0u8; 16];
        word.copy_from_slice(&bytes[index * 16..index * 16 + 16]);
        u128::from_le_bytes(word)
    };
    (0..5).all(|index| word(a, index) == word(b, index))
}

/// Any payload behind `M`'s header: whatever `decode_exact` accepts keeps
/// `rules` and encodes back to the same bytes.
fn any_payload_keeps_rules<M: Message>(rules: fn(&M) -> bool) {
    let mut record = [0u8; MAX_RECORD_LEN];
    let len = encoded_len::<M>();
    let header = Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: M::PAYLOAD_LEN as u16,
    };
    assert!(header.write(&mut record).is_ok());
    let payload: [u8; MAX_RECORD_LEN - HEADER_LEN] = kani::any();
    record[HEADER_LEN..len].copy_from_slice(&payload[..M::PAYLOAD_LEN]);
    if let Ok(message) = decode_exact::<M>(&record[..len]) {
        assert!(rules(&message));
        // Both buffers are zero past `len`, so whole records compare.
        let mut out = [0u8; MAX_RECORD_LEN];
        assert!(encode(&message, &mut out) == Ok(len));
        assert!(same(&out, &record));
    }
}

#[kani::proof]
#[kani::unwind(10)]
fn configure_any_input() {
    any_input_returns::<Configure>();
}

/// A configuration accepted from the display PD describes a real,
/// addressable framebuffer: neither side zero, rows long enough; and a
/// device scale of 4 to 16 quarter pixels a layout unit, 1 to 4.
#[kani::proof]
#[kani::unwind(10)]
fn configure_any_payload() {
    any_payload_keeps_rules::<Configure>(|configure| {
        let layout = configure.layout();
        let quarters = configure.scale().quarters();
        layout.width() > 0
            && layout.height() > 0
            && u64::from(layout.stride())
                >= u64::from(layout.width()) * u64::from(layout.format().bytes_per_pixel())
            && (4..=16).contains(&quarters)
    });
}

#[kani::proof]
#[kani::unwind(10)]
fn present_any_input() {
    any_input_returns::<Present>();
}

/// A present accepted from the UI names at most `MAX_DAMAGE_RECTS`
/// rectangles, none of them empty.
#[kani::proof]
#[kani::unwind(10)]
fn present_any_payload() {
    any_payload_keeps_rules::<Present>(|present| {
        present.damage().len() <= MAX_DAMAGE_RECTS
            && present.damage().iter().all(|rect| !rect.is_empty())
    });
}

#[kani::proof]
#[kani::unwind(10)]
fn present_done_any_input() {
    any_input_returns::<PresentDone>();
}

/// A present-done record is only its sequence number.
#[kani::proof]
#[kani::unwind(10)]
fn present_done_any_payload() {
    any_payload_keeps_rules::<PresentDone>(|_| true);
}
