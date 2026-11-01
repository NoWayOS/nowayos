// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: for every byte string up to 80 bytes, the display
//! contract's `Present`, [`Header::parse`], [`decode`] and [`decode_exact`]
//! never panic, overflow or read out of bounds; they fail only with the
//! typed errors they document; and what they accept is exactly the record's
//! own bytes. A crate with longer records proves its own decoders at their
//! full length.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them, and then a
//! copy of this crate with a length check removed, whose proofs must fail
//! (`tools/kani-mutation`).

use crate::{
    DecodeError, EncodeError, HEADER_LEN, Header, Message, Reader, Writer, decode, decode_exact,
};

/// The longest record these generic proofs cover: the display contract's
/// `Present`, 8 bytes of header and 72 of payload
/// (`nowayos_display::MAX_RECORD_LEN`, whose own proofs cover each display
/// record at its full length). A crate with longer records proves its own
/// decoders, which run `Header::parse` and `Reader`, at their full length:
/// `nowayos-update`'s trust list, manifest and bundle chunk over 793, 1225
/// and 513 bytes, and `nowayos-socket`'s records over 257.
const LONGEST_RECORD: usize = 80;

/// The message kind the test messages below use.
const KIND: u16 = 0x7e57;

/// Any prefix of `buf`: a byte string of any length up to `N`, with any
/// contents.
fn any_prefix<const N: usize>(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any_where(|len: &usize| *len <= N);
    &buf[..len]
}

/// A message whose payload is `N` bytes of any value: the decoders' own
/// checks, with nothing of a real payload's validation in front of them.
struct Opaque<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Message for Opaque<N> {
    const KIND: u16 = KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = N;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_bytes(&self.bytes)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            bytes: input.read_array()?,
        })
    }
}

/// `Header::parse` accepts exactly the inputs of at least eight bytes whose
/// reserved field is zero, and a header it accepts writes back as the same
/// eight bytes.
#[kani::proof]
#[kani::unwind(9)]
fn header_parse_any_input() {
    let buf: [u8; LONGEST_RECORD] = kani::any();
    let bytes = any_prefix(&buf);
    match Header::parse(bytes) {
        Ok(header) => {
            assert!(bytes.len() >= HEADER_LEN);
            assert!(bytes[6] == 0 && bytes[7] == 0);
            let mut out = [0u8; HEADER_LEN];
            assert!(header.write(&mut out).is_ok());
            assert!(out[..] == bytes[..HEADER_LEN]);
            assert!(header.record_len() == HEADER_LEN + usize::from(header.payload_len));
        }
        Err(DecodeError::Truncated { .. }) => assert!(bytes.len() < HEADER_LEN),
        Err(DecodeError::ReservedNotZero) => {
            assert!(bytes.len() >= HEADER_LEN && (bytes[6] != 0 || bytes[7] != 0));
        }
        Err(_) => panic!("Header::parse failed with an error it does not document"),
    }
}

/// `decode` and `decode_exact` of an `N`-byte payload, for every input up to
/// [`LONGEST_RECORD`]: a decoded message is the record's own payload, behind
/// a header naming its kind, version and length; `decode_exact` accepts only
/// an input that is exactly one record; and the two agree.
fn decode_any_input<const N: usize>() {
    let buf: [u8; LONGEST_RECORD] = kani::any();
    let bytes = any_prefix(&buf);
    let total = HEADER_LEN + N;
    let prefix = decode::<Opaque<N>>(bytes);
    if let Ok(message) = &prefix {
        assert!(bytes.len() >= total);
        let header = Header::parse(bytes);
        assert!(matches!(
            header,
            Ok(Header { kind: KIND, version: 1, payload_len }) if usize::from(payload_len) == N
        ));
        assert!(message.bytes[..] == bytes[HEADER_LEN..total]);
    }
    match decode_exact::<Opaque<N>>(bytes) {
        Ok(message) => {
            assert!(bytes.len() == total);
            assert!(matches!(&prefix, Ok(first) if first.bytes == message.bytes));
        }
        Err(DecodeError::TrailingBytes { unread }) => {
            assert!(prefix.is_ok());
            assert!(unread > 0 && unread == bytes.len() - total);
        }
        Err(_) => assert!(prefix.is_err()),
    }
}

/// A record with no payload.
#[kani::proof]
#[kani::unwind(82)]
fn decode_any_input_empty_payload() {
    decode_any_input::<0>();
}

/// A 4-byte payload, as `PresentDone`'s.
#[kani::proof]
#[kani::unwind(82)]
fn decode_any_input_4_byte_payload() {
    decode_any_input::<4>();
}

/// A 12-byte payload, as `InputEvent`'s and `Configure`'s.
#[kani::proof]
#[kani::unwind(82)]
fn decode_any_input_12_byte_payload() {
    decode_any_input::<12>();
}

/// The longest payload: `Present`'s 72 bytes.
#[kani::proof]
#[kani::unwind(82)]
fn decode_any_input_longest_payload() {
    decode_any_input::<{ LONGEST_RECORD - HEADER_LEN }>();
}

/// Every `Reader` read, at any position in any buffer up to
/// [`LONGEST_RECORD`], either returns bytes from inside the buffer and moves
/// on by exactly their number, or fails with `Truncated` and moves nowhere.
#[kani::proof]
#[kani::unwind(82)]
fn reader_reads_stay_in_bounds() {
    let buf: [u8; LONGEST_RECORD] = kani::any();
    let bytes = any_prefix(&buf);
    let mut reader = Reader::new(bytes);
    let skip: usize = kani::any_where(|skip: &usize| *skip <= LONGEST_RECORD);
    let skipped = reader.read_bytes(skip).is_ok();
    let start = reader.position();
    assert!(start <= bytes.len());
    let count: usize = kani::any();
    match reader.read_bytes(count) {
        Ok(read) => {
            assert!(read.len() == count);
            assert!(reader.position() == start + count);
            assert!(read == &bytes[start..start + count]);
        }
        Err(DecodeError::Truncated { .. }) => {
            assert!(reader.position() == start);
            assert!(count > bytes.len() - start);
        }
        Err(_) => panic!("read_bytes failed with an error it does not document"),
    }
    assert!(skipped == (skip <= bytes.len()));
}
