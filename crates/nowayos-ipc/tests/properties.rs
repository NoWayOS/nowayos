// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the codec and the ring: round-trips, rejection of
//! malformed input (never a panic), and full / empty / wrap-around
//! behaviour checked against a `VecDeque` model.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::VecDeque;

use nowayos_ipc::{
    Consumer, ConsumerMemory, DecodeError, EncodeError, HEADER_LEN, InMemoryRing, Message,
    Producer, ProducerMemory, Reader, RecvError, SendError, Writer, decode, decode_exact, encode,
    encoded_len,
};
use proptest::prelude::*;

/// A message with every field shape the codec supports, plus padding.
#[derive(Debug, Clone, PartialEq)]
struct Sample {
    tag: u8,
    flag: bool,
    small: u16,
    signed: i32,
    wide: u64,
    raw: [u8; 3],
}

impl Message for Sample {
    const KIND: u16 = 0x7E57;
    const VERSION: u16 = 2;
    const PAYLOAD_LEN: usize = 1 + 1 + 2 + 4 + 8 + 3 + 1;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u8(self.tag)?;
        out.write_bool(self.flag)?;
        out.write_u16(self.small)?;
        out.write_i32(self.signed)?;
        out.write_u64(self.wide)?;
        out.write_bytes(&self.raw)?;
        out.write_zeros(1)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let tag = input.read_u8()?;
        if tag > 200 {
            return Err(DecodeError::InvalidValue { field: "tag" });
        }
        let flag = input.read_bool()?;
        let small = input.read_u16()?;
        let signed = input.read_i32()?;
        let wide = input.read_u64()?;
        let raw = input.read_array()?;
        input.read_zeros(1)?;
        Ok(Self {
            tag,
            flag,
            small,
            signed,
            wide,
            raw,
        })
    }
}

fn sample() -> impl Strategy<Value = Sample> {
    (
        0u8..=200,
        any::<bool>(),
        any::<u16>(),
        any::<i32>(),
        any::<u64>(),
        any::<[u8; 3]>(),
    )
        .prop_map(|(tag, flag, small, signed, wide, raw)| Sample {
            tag,
            flag,
            small,
            signed,
            wide,
            raw,
        })
}

const RECORD: usize = encoded_len::<Sample>();

fn encode_sample(s: &Sample) -> [u8; RECORD] {
    let mut buf = [0u8; RECORD];
    assert_eq!(encode(s, &mut buf).unwrap(), RECORD);
    buf
}

proptest! {
    #[test]
    fn round_trip(s in sample()) {
        let bytes = encode_sample(&s);
        prop_assert_eq!(decode_exact::<Sample>(&bytes), Ok(s));
    }

    /// Arbitrary bytes never panic the decoder; and anything it accepts
    /// re-encodes to exactly the same bytes (one encoding per value).
    #[test]
    fn arbitrary_bytes_never_panic_and_accepted_input_is_canonical(
        bytes in proptest::collection::vec(any::<u8>(), 0..64)
    ) {
        if let Ok(s) = decode::<Sample>(&bytes) {
            let again = encode_sample(&s);
            prop_assert_eq!(&bytes[..RECORD], &again[..]);
        }
    }

    /// A valid header followed by arbitrary payload bytes: still no panic,
    /// and the same canonicality holds. (Random bytes rarely get past the
    /// header, so this aims the fuzzing at the payload decoder.)
    #[test]
    fn arbitrary_payload_behind_valid_header(payload in any::<[u8; Sample::PAYLOAD_LEN]>()) {
        let mut bytes = encode_sample(&Sample {
            tag: 0, flag: false, small: 0, signed: 0, wide: 0, raw: [0; 3],
        });
        bytes[HEADER_LEN..].copy_from_slice(&payload);
        if let Ok(s) = decode_exact::<Sample>(&bytes) {
            prop_assert_eq!(encode_sample(&s), bytes);
        }
    }

    #[test]
    fn every_truncation_is_rejected(s in sample(), cut in 0..RECORD) {
        let bytes = encode_sample(&s);
        let result = decode::<Sample>(&bytes[..cut]);
        let is_truncated = matches!(result, Err(DecodeError::Truncated { .. }));
        prop_assert!(is_truncated);
    }

    #[test]
    fn any_single_bit_flip_in_the_header_is_rejected(s in sample(), bit in 0..(HEADER_LEN * 8)) {
        let mut bytes = encode_sample(&s);
        bytes[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(decode::<Sample>(&bytes).is_err());
    }

    /// Random send / receive sequences against a model, starting the free-
    /// running indices near `u32::MAX` so they wrap mid-test.
    #[test]
    fn ring_matches_queue_model(
        start in prop_oneof![Just(0u32), Just(u32::MAX - 5), any::<u32>()],
        ops in proptest::collection::vec(any::<bool>(), 0..200),
        seed in any::<u64>(),
    ) {
        const CAPACITY: usize = 4;
        let ring: InMemoryRing<CAPACITY, 32> = InMemoryRing::new();
        ring.store_producer_index(start);
        ring.store_consumer_index(start);
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        let mut model: VecDeque<Sample> = VecDeque::new();
        for (step, send) in ops.into_iter().enumerate() {
            if send {
                let s = Sample {
                    tag: (step % 200) as u8,
                    flag: step % 2 == 0,
                    small: step as u16,
                    signed: -(step as i32),
                    wide: seed.wrapping_add(step as u64),
                    raw: [step as u8; 3],
                };
                let result = tx.try_send(&s);
                if model.len() < CAPACITY {
                    prop_assert_eq!(result, Ok(()));
                    model.push_back(s);
                } else {
                    prop_assert_eq!(result, Err(SendError::Full));
                }
            } else {
                prop_assert_eq!(rx.try_recv::<Sample>(), Ok(model.pop_front()));
            }
            prop_assert_eq!(rx.pending(), Ok(model.len() as u32));
            prop_assert_eq!(tx.free_slots(), Ok((CAPACITY - model.len()) as u32));
        }
    }

    /// Whatever garbage a hostile producer writes into a slot, the consumer
    /// either decodes a valid message or reports an error — and moves on.
    #[test]
    fn consumer_survives_garbage_slots(slots in proptest::collection::vec(any::<[u8; 32]>(), 1..8)) {
        let ring: InMemoryRing<8, 32> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        for slot in &slots {
            tx.try_send_bytes(slot).unwrap();
        }
        for _ in &slots {
            match rx.try_recv::<Sample>() {
                Ok(Some(_)) | Err(RecvError::Decode(_)) => {}
                other => prop_assert!(false, "unexpected {:?}", other),
            }
        }
        prop_assert_eq!(rx.try_recv::<Sample>(), Ok(None));
    }
}
