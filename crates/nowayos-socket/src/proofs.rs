// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: each ring's decoder - [`Request::decode`] and
//! [`Report::decode`], with their `decode_exact` forms - given any byte
//! string up to [`SLOT_LEN`] bytes and one more, never panics, overflows or
//! reads out of bounds; and each record, from any payload behind its own
//! header, accepts only what keeps its rules - a port and a unicast
//! destination, a length from 1 to [`MAX_DATA`] with zeros after it, a known
//! failure reason - and encodes back to exactly the bytes it came from, so
//! every record has one encoding.
//!
//! That data has only zeros after it is shown apart from the decoder's own
//! check: a data record accepted encodes to the same bytes as the one
//! `Data::new` makes from its bytes. Without that, a decoder that let any
//! padding through would still pass, as it keeps the padding and encodes it
//! back.
//!
//! The data records' padding is checked sixteen bytes at a time, so the
//! decoders' longest loop runs [`MAX_DATA`] / 16 = 61 times whatever the
//! input, and the comparison here at most [`SLOT_LEN`] / 16 = 62 times:
//! every proof is unwound 64 times, and Kani checks that the bound is
//! enough.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, decode_exact, encode, encoded_len};

use crate::allow::{ALLOW_LIST_MAX_LEN, AllowList, MAX_DESTINATIONS};
use crate::records::{
    Close, Closed, Connect, Connected, Data, Failed, MAX_DATA, RECEIVED_KIND, ReceivedData, Report,
    Request, SEND_KIND, SLOT_LEN, SendData, is_unicast_destination,
};

/// Any byte string of at most this many bytes: every record, short of it,
/// and one byte past the longest.
const INPUT_LEN: usize = SLOT_LEN + 1;

/// The longest of the short records ([`Connect`]), rounded up to a word:
/// the buffer their proofs use.
const SHORT: usize = 16;

/// `buf`'s first `len` bytes, for any `len` up to `N`.
fn any_prefix<const N: usize>(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any_where(|len: &usize| *len <= N);
    &buf[..len]
}

/// Sixteen bytes as one little-endian word.
fn word(bytes: &[u8]) -> u128 {
    let mut word = [0u8; 16];
    word.copy_from_slice(bytes);
    u128::from_le_bytes(word)
}

/// The two buffers are the same bytes, compared as sixteen-byte words.
fn same<const N: usize>(a: &[u8; N], b: &[u8; N]) -> bool {
    const {
        assert!(N % 16 == 0);
    }
    (0..N / 16).all(|index| {
        let at = index * 16;
        word(&a[at..at + 16]) == word(&b[at..at + 16])
    })
}

/// Any payload behind `M`'s own header, in a zeroed buffer of `N` bytes:
/// whatever `decode_exact` accepts keeps `rules` and encodes back to the
/// same bytes.
fn any_payload_keeps_rules<M: Message, const N: usize>(rules: fn(&M) -> bool) {
    let len = encoded_len::<M>();
    assert!(len <= N);
    let mut record = [0u8; N];
    let header = Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: M::PAYLOAD_LEN as u16,
    };
    assert!(header.write(&mut record).is_ok());
    let payload: [u8; N] = kani::any();
    record[HEADER_LEN..len].copy_from_slice(&payload[HEADER_LEN..len]);
    if let Ok(message) = decode_exact::<M>(&record[..len]) {
        assert!(rules(&message));
        // Both buffers are zero past `len`, so whole buffers compare.
        let mut out = [0u8; N];
        assert!(encode(&message, &mut out) == Ok(len));
        assert!(same(&out, &record));
    }
}

/// The requests ring's decoders return, whatever the input: `decode_exact`
/// runs `decode` on every input, then accepts only a whole record.
#[kani::proof]
#[kani::unwind(64)]
fn request_any_input() {
    let buf: [u8; INPUT_LEN] = kani::any();
    let bytes = any_prefix(&buf);
    if let Ok(request) = Request::decode_exact(bytes) {
        assert!(bytes.len() == request.encoded_len());
    }
}

/// The reports ring's decoders return, whatever the input.
#[kani::proof]
#[kani::unwind(64)]
fn report_any_input() {
    let buf: [u8; INPUT_LEN] = kani::any();
    let bytes = any_prefix(&buf);
    if let Ok(report) = Report::decode_exact(bytes) {
        assert!(bytes.len() == report.encoded_len());
    }
}

/// A connect accepted names a port and a destination a TCP connection can
/// have.
#[kani::proof]
#[kani::unwind(64)]
fn connect_any_payload() {
    any_payload_keeps_rules::<Connect, SHORT>(|connect| {
        connect.port() != 0 && is_unicast_destination(connect.address())
    });
}

/// A data record's rules: 1 to `MAX_DATA` bytes, and only zeros after them.
/// The second is shown apart from the decoder's own padding check: the
/// record encodes to the same bytes as the one [`Data::new`] makes from its
/// bytes, which fills the rest of the area with zeros.
fn data_keeps_rules<const KIND: u16>(data: &Data<KIND>) -> bool {
    let mut decoded = [0u8; SLOT_LEN];
    let mut made = [0u8; SLOT_LEN];
    (1..=MAX_DATA).contains(&data.len())
        && data.bytes().len() == data.len()
        && encode(data, &mut decoded) == Ok(SLOT_LEN)
        && Data::<KIND>::new(data.connection(), data.bytes())
            .is_some_and(|fresh| encode(&fresh, &mut made) == Ok(SLOT_LEN))
        && same(&decoded, &made)
}

/// Data accepted carries 1 to `MAX_DATA` bytes, and nothing after them.
#[kani::proof]
#[kani::unwind(64)]
fn send_data_any_payload() {
    any_payload_keeps_rules::<SendData, SLOT_LEN>(data_keeps_rules::<SEND_KIND>);
}

/// As for [`send_data_any_payload`], on the reports ring.
#[kani::proof]
#[kani::unwind(64)]
fn received_data_any_payload() {
    any_payload_keeps_rules::<ReceivedData, SLOT_LEN>(data_keeps_rules::<RECEIVED_KIND>);
}

/// The records that only name their connection have one encoding.
#[kani::proof]
#[kani::unwind(64)]
fn connection_records_any_payload() {
    any_payload_keeps_rules::<Close, SHORT>(|_| true);
    any_payload_keeps_rules::<Connected, SHORT>(|_| true);
    any_payload_keeps_rules::<Closed, SHORT>(|_| true);
}

/// A failure accepted gives one of the six reasons.
#[kani::proof]
#[kani::unwind(64)]
fn failed_any_payload() {
    any_payload_keeps_rules::<Failed, SHORT>(|failed| (1..=6).contains(&failed.reason().code()));
}

/// The allow list's decoder, given any bytes up to its longest list
/// and one word more, never panics, overflows or reads out of bounds, and a
/// list it accepts holds 1 to `MAX_DESTINATIONS` destinations, each a
/// unicast destination on a port that is not 0. Its loops run at most
/// `MAX_DESTINATIONS` times, and the duplicate check as many again within:
/// unwound 10.
#[kani::proof]
#[kani::unwind(10)]
fn allow_list_any_bytes() {
    let buf: [u8; ALLOW_LIST_MAX_LEN + 4] = kani::any();
    if let Ok(list) = AllowList::decode(any_prefix(&buf)) {
        assert!(list.len() >= 1 && list.len() <= MAX_DESTINATIONS);
        for destination in list.iter() {
            assert!(is_unicast_destination(destination.address));
            assert!(destination.port != 0);
            assert!(list.permits(destination.address, destination.port));
        }
    }
}
