// SPDX-License-Identifier: MIT OR Apache-2.0
//! What NoWayOS's fuzz targets share (`fuzz_targets/`, run by `make fuzz`):
//! [`Input`], which turns the fuzzer's bytes into the values a target
//! needs, and the checks every record decoder must pass, whatever the bytes
//! ([`check_header`], [`check_decode`]).
//!
//! A target's checks are assertions: a failed one aborts the run like a
//! panic in the code under test, and libFuzzer keeps the input that caused
//! it. A crash, once fixed, becomes an ordinary regression test in the crate
//! that had the bug.

#![forbid(unsafe_code)]

use core::cell::Cell;
use core::fmt::Debug;

use nowayos_ipc::{
    DecodeError, HEADER_LEN, Header, Message, decode, decode_exact, encode, encoded_len,
};

/// The fuzzer's bytes, read front to back. Once they run out every read
/// gives zeros, so a target never has to stop half way through a step.
///
/// Reads take `&self`, so that a memory view driven by the fuzzer, whose
/// trait methods take `&self`, can read from it too.
#[derive(Debug)]
pub struct Input<'a> {
    data: &'a [u8],
    read: Cell<usize>,
}

impl<'a> Input<'a> {
    /// The input `data`, from its first byte.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            read: Cell::new(0),
        }
    }

    /// Whether every byte has been read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read.get() >= self.data.len()
    }

    /// Every byte not yet read; after this the input is empty.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        let rest = self.data.get(self.read.get()..).unwrap_or_default();
        self.read.set(self.data.len());
        rest
    }

    /// The next byte, or 0.
    #[must_use]
    pub fn byte(&self) -> u8 {
        let at = self.read.get();
        match self.data.get(at) {
            Some(&byte) => {
                self.read.set(at.saturating_add(1));
                byte
            }
            None => 0,
        }
    }

    /// The next `N` bytes, zeros once the input runs out.
    #[must_use]
    pub fn array<const N: usize>(&self) -> [u8; N] {
        let mut out = [0u8; N];
        for byte in &mut out {
            *byte = self.byte();
        }
        out
    }

    /// The next two bytes as a little-endian `u16`.
    #[must_use]
    pub fn u16(&self) -> u16 {
        u16::from_le_bytes(self.array())
    }

    /// The next four bytes as a little-endian `u32`.
    #[must_use]
    pub fn u32(&self) -> u32 {
        u32::from_le_bytes(self.array())
    }

    /// The next four bytes as a little-endian `i32`.
    #[must_use]
    pub fn i32(&self) -> i32 {
        i32::from_le_bytes(self.array())
    }
}

/// The longest record [`frame`] builds, and the most [`check_decode`]
/// re-encodes: room for the update records (`nowayos-update`), the longest
/// a fuzz target decodes.
pub const FRAME_MAX: usize = 2048;

/// `payload` behind a valid header for `M` - its kind, version and payload
/// length - in `buf`, cut to [`FRAME_MAX`] bytes: so that a target reaches
/// `M`'s payload checks from its first input, without first having to find
/// the header's three values.
pub fn frame<'b, M: Message>(payload: &[u8], buf: &'b mut [u8; FRAME_MAX]) -> &'b [u8] {
    let header = Header {
        kind: M::KIND,
        version: M::VERSION,
        payload_len: u16::try_from(M::PAYLOAD_LEN).unwrap_or(u16::MAX),
    };
    let Some((head, body)) = buf.split_at_mut_checked(HEADER_LEN) else {
        return &[];
    };
    if header.write(head).is_err() {
        return &[];
    }
    let len = payload.len().min(body.len());
    for (to, from) in body.iter_mut().zip(payload) {
        *to = *from;
    }
    buf.get(..HEADER_LEN.saturating_add(len))
        .unwrap_or_default()
}

/// Parses the header at the start of `bytes` and checks it: it fails only
/// for fewer than [`HEADER_LEN`] bytes or a reserved field that is not zero,
/// and a header that parses writes back as exactly the bytes it came from.
pub fn check_header(bytes: &[u8]) -> Option<Header> {
    let parsed = Header::parse(bytes);
    let first = bytes.first_chunk::<HEADER_LEN>();
    let consistent = match (&parsed, first) {
        (Ok(header), Some(first)) => {
            let mut out = [0u8; HEADER_LEN];
            header.write(&mut out).is_ok()
                && out == *first
                && HEADER_LEN.checked_add(usize::from(header.payload_len))
                    == Some(header.record_len())
        }
        (Err(DecodeError::Truncated { .. }), None) => true,
        (Err(DecodeError::ReservedNotZero), Some(first)) => {
            first.last_chunk::<2>() != Some(&[0, 0])
        }
        _ => false,
    };
    assert!(consistent, "Header::parse gave {parsed:?} for {bytes:02x?}");
    parsed.ok()
}

/// Decodes `bytes` as an `M` with both [`decode`] and [`decode_exact`] and
/// checks what every record type must satisfy, whatever the bytes: they
/// agree - `decode_exact` is `decode` plus "nothing after the record" - and
/// a record that decodes re-encodes to exactly the bytes it came from, so
/// every value has one encoding (message rules).
/// Returns the message, if one decodes.
pub fn check_decode<M: Message + Debug + PartialEq>(bytes: &[u8]) -> Option<M> {
    let prefix = decode::<M>(bytes);
    let exact = decode_exact::<M>(bytes);
    let len = encoded_len::<M>();
    let consistent = match (&prefix, &exact) {
        (Ok(first), Ok(second)) => first == second && bytes.len() == len,
        (Ok(_), Err(DecodeError::TrailingBytes { unread })) => {
            *unread > 0 && bytes.len().checked_sub(len) == Some(*unread)
        }
        (Err(first), Err(second)) => first == second,
        _ => false,
    };
    assert!(
        consistent,
        "decode gave {prefix:?} but decode_exact {exact:?}, for {bytes:02x?}"
    );
    let message = prefix.ok()?;
    assert!(
        len <= FRAME_MAX,
        "a {len}-byte record is longer than FRAME_MAX"
    );
    let mut out = [0u8; FRAME_MAX];
    assert_eq!(
        encode(&message, &mut out),
        Ok(len),
        "{message:?} does not re-encode"
    );
    assert_eq!(
        out.get(..len),
        bytes.get(..len),
        "{message:?} re-encodes to other bytes than it came from"
    );
    Some(message)
}
