// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-ipc`'s record decoding - `Header::parse`, `decode`
//! and `decode_exact` - on any bytes, as one protection domain decodes what
//! another wrote.
//!
//! Two record types stand in for every message: `Probe` reads one of each
//! kind of field the payload `Reader` offers, and `Empty` has no payload.
//! Two inputs in three are framed behind a valid header for one of them, so
//! the payload checks are fuzzed from the first run. Checks
//! (`nowayos_fuzz::check_header`, `check_decode`): nothing panics; a header
//! fails only when short or with its reserved field set, and writes back as
//! the same bytes; `decode` and `decode_exact` agree; and whatever decodes
//! re-encodes to exactly the bytes it came from.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, check_decode, check_header, frame};
use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

/// One field of each kind the payload reader has, then padding and bytes.
#[derive(Debug, PartialEq)]
struct Probe {
    byte: u8,
    flag: bool,
    short: u16,
    word: u32,
    long: u64,
    signed: i32,
    tail: [u8; 5],
}

impl Message for Probe {
    const KIND: u16 = 0x7F01;
    const VERSION: u16 = 2;
    const PAYLOAD_LEN: usize = 28;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u8(self.byte)?;
        out.write_bool(self.flag)?;
        out.write_u16(self.short)?;
        out.write_u32(self.word)?;
        out.write_u64(self.long)?;
        out.write_i32(self.signed)?;
        out.write_zeros(3)?;
        out.write_bytes(&self.tail)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let byte = input.read_u8()?;
        let flag = input.read_bool()?;
        let short = input.read_u16()?;
        let word = input.read_u32()?;
        let long = input.read_u64()?;
        let signed = input.read_i32()?;
        input.read_zeros(3)?;
        let tail = input.read_array()?;
        Ok(Self {
            byte,
            flag,
            short,
            word,
            long,
            signed,
            tail,
        })
    }
}

/// A record with no payload.
#[derive(Debug, PartialEq)]
struct Empty;

impl Message for Empty {
    const KIND: u16 = 0x7F02;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 0;

    fn encode_payload(&self, _out: &mut Writer<'_>) -> Result<(), EncodeError> {
        Ok(())
    }

    fn decode_payload(_input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
}

fuzz_target!(|data: &[u8]| {
    let (mode, rest) = data.split_first().unwrap_or((&0, &[]));
    let mut buf = [0u8; FRAME_MAX];
    let record = match mode % 3 {
        1 => frame::<Probe>(rest, &mut buf),
        2 => frame::<Empty>(rest, &mut buf),
        _ => rest,
    };
    check_header(record);
    check_decode::<Probe>(record);
    check_decode::<Empty>(record);
});
