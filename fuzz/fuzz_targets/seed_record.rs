// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the seed record a PD takes off its seed ring
//! (`nowayos-seed`) - any bytes decoded as a seed,
//! any payload behind a seed's own header, and records of the fuzzer's
//! making taken off a ring.
//!
//! Checks: nothing panics; the decoder agrees with itself and whatever
//! decodes re-encodes to the bytes it came from (`nowayos_fuzz::check_decode`);
//! a seed is never all zeros, and holds exactly the payload's bytes; and
//! each seed taken off the ring is taken once, in order, a refused record
//! consumed with it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, frame};
use nowayos_ipc::{HEADER_LEN, InMemoryRing, Producer, RecvError};
use nowayos_seed::{SEED_RECORD_LEN, SEED_RING_SLOT, SEED_RING_SLOTS, Seed, take};

/// The longest record one step puts on the ring, and one more.
const LEN_LIMIT: usize = SEED_RING_SLOT.saturating_add(1);

/// A seed that decoded keeps its rule and its payload.
fn check_seed(seed: &Seed, record: &[u8]) {
    assert!(
        seed.as_bytes().iter().any(|byte| *byte != 0),
        "a seed of zeros"
    );
    assert_eq!(
        record.get(HEADER_LEN..SEED_RECORD_LEN),
        Some(seed.as_bytes().as_slice()),
        "a seed that is not its payload"
    );
    let _ = seed.fold_u64();
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    match input.byte() % 3 {
        // Any bytes.
        0 => {
            let bytes = input.rest();
            if let Some(seed) = check_decode::<Seed>(bytes) {
                check_seed(&seed, bytes);
            }
        }
        // Any payload behind a seed's header.
        1 => {
            let mut buf = [0u8; FRAME_MAX];
            let record = frame::<Seed>(input.rest(), &mut buf);
            if let Some(seed) = check_decode::<Seed>(record) {
                check_seed(&seed, record);
            }
        }
        // Records of the fuzzer's making on a ring, each a slot's worth of
        // bytes, taken off one at a time.
        _ => {
            let ring: InMemoryRing<SEED_RING_SLOTS, SEED_RING_SLOT> = InMemoryRing::new();
            let Ok(mut producer) = Producer::new(&ring) else {
                return;
            };
            let mut sent = 0usize;
            while !input.is_empty() && sent < SEED_RING_SLOTS {
                let mut buf = [0u8; FRAME_MAX];
                let len = usize::from(input.byte()) % LEN_LIMIT;
                let payload: [u8; SEED_RING_SLOT] = input.array();
                let record = if input.byte().is_multiple_of(2) {
                    frame::<Seed>(payload.get(..len).unwrap_or_default(), &mut buf)
                } else {
                    payload.get(..len).unwrap_or_default()
                };
                if producer.try_send_bytes(record).is_ok() {
                    sent = sent.saturating_add(1);
                }
            }
            let mut taken = 0usize;
            loop {
                match take(&ring) {
                    Ok(Some(seed)) => assert!(seed.as_bytes().iter().any(|byte| *byte != 0)),
                    Ok(None) => break,
                    Err(error) => assert!(
                        matches!(error, RecvError::Decode(_)),
                        "an honest ring refused: {error:?}"
                    ),
                }
                taken = taken.saturating_add(1);
                assert!(taken <= sent, "more taken off the ring than was sent");
            }
            assert_eq!(taken, sent, "a record left on the ring");
        }
    }
});
