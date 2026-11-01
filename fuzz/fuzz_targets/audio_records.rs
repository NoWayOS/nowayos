// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the audio channel's records (`nowayos-audio`) -
//! any bytes decoded as a microphone's, a speaker's, a devices or a capture
//! record, any payload behind each one's own header - and records
//! taken off a lossy ring (`nowayos_audio::LossyConsumer`) whose producer
//! the fuzzer is: it sets the slots, and moves its index anywhere between the
//! consumer's loads and changes a slot in the middle of a copy.
//!
//! Checks: nothing panics; each decoder agrees with itself and whatever
//! decodes re-encodes to the bytes it came from (`check_decode`); a PCM
//! record holds one frame to `MAX_PCM` bytes of whole frames, and its
//! samples are its bytes; and the lossy consumer copies only slots inside
//! the ring, returns a record only when the producer index it loaded after
//! the copy still holds it in the window, and passes every record either
//! returned or counted lost.

#![no_main]

use core::cell::Cell;

use libfuzzer_sys::fuzz_target;
use nowayos_audio::{
    CAPTURE_KIND, Capture, Captured, DEVICES_KIND, Devices, LossyConsumer, MAX_PCM, PLAY_KIND,
    Play, SLOT_LEN,
};
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, frame};
use nowayos_ipc::{ConsumerMemory, Header, RingMemory, decode};

/// The slots of the ring the fuzzer writes.
const SLOTS: usize = 4;

/// The records the window holds: the slots less one.
const WINDOW: u32 = 3;

/// The most records one input takes off the ring.
const TAKES: usize = 16;

/// A PCM record that decoded keeps its rules.
fn check_pcm<const KIND: u16>(record: &nowayos_audio::Pcm<KIND>) {
    let len = record.bytes().len();
    let frame = record.format().frame_bytes();
    assert!(len >= frame && len <= MAX_PCM && len.is_multiple_of(frame));
    assert_eq!(record.frames().saturating_mul(frame), len);
    let samples = record.samples().count();
    assert_eq!(
        samples.saturating_mul(2),
        len,
        "samples that are not its bytes"
    );
}

/// A lossy ring's memory with the fuzzer as its producer.
struct Hostile<'a> {
    input: &'a Input<'a>,
    producer: Cell<u32>,
    consumer: Cell<u32>,
    slots: [Cell<[u8; SLOT_LEN]>; SLOTS],
    /// The producer index the consumer loaded last.
    last_load: Cell<u32>,
}

impl RingMemory<SLOT_LEN> for Hostile<'_> {
    fn capacity(&self) -> u32 {
        WINDOW.saturating_add(1)
    }

    fn load_producer_index(&self) -> u32 {
        let now = self.producer.get();
        let next = match self.input.byte() % 4 {
            0 => now,
            1 => now.wrapping_add(1),
            2 => now.wrapping_add(u32::from(self.input.byte())),
            _ => self.input.u32(),
        };
        self.producer.set(next);
        self.last_load.set(next);
        next
    }

    fn load_consumer_index(&self) -> u32 {
        self.consumer.get()
    }

    fn load_producer_epoch(&self) -> u32 {
        0
    }

    fn load_consumer_epoch(&self) -> u32 {
        0
    }
}

impl ConsumerMemory<SLOT_LEN> for Hostile<'_> {
    fn store_consumer_index(&self, index: u32) {
        self.consumer.set(index);
    }

    fn store_consumer_epoch(&self, _epoch: u32) {}

    fn read_slot(&self, slot: u32, out: &mut [u8; SLOT_LEN]) {
        let index = usize::try_from(slot).unwrap_or(usize::MAX);
        assert!(index < SLOTS, "a slot outside the ring: {slot}");
        if let Some(cell) = self.slots.get(index) {
            *out = cell.get();
        }
        // The producer writes over the second half mid-copy, now and then.
        if self.input.byte().is_multiple_of(4) {
            let torn: [u8; 32] = self.input.array();
            for (to, from) in out.iter_mut().skip(SLOT_LEN / 2).zip(torn) {
                *to = from;
            }
        }
    }
}

/// A slot's starting bytes: a record of each kind, the payload the
/// fuzzer's, or the fuzzer's bytes as they are.
fn slot(input: &Input<'_>) -> [u8; SLOT_LEN] {
    let mut bytes = [0u8; SLOT_LEN];
    let payload: [u8; 64] = input.array();
    let mut buf = [0u8; FRAME_MAX];
    let record: &[u8] = match input.byte() % 5 {
        0 => frame::<Captured>(&payload, &mut buf),
        1 => frame::<Play>(&payload, &mut buf),
        2 => frame::<Devices>(&payload, &mut buf),
        3 => frame::<Capture>(&payload, &mut buf),
        _ => &payload,
    };
    for (to, from) in bytes.iter_mut().zip(record) {
        *to = *from;
    }
    bytes
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    match input.byte() % 3 {
        // Any bytes, as each record.
        0 => {
            let bytes = input.rest();
            if let Some(record) = check_decode::<Captured>(bytes) {
                check_pcm(&record);
            }
            if let Some(record) = check_decode::<Play>(bytes) {
                check_pcm(&record);
            }
            let _ = check_decode::<Devices>(bytes);
            let _ = check_decode::<Capture>(bytes);
        }
        // Any payload behind a record's own header.
        1 => {
            let mut buf = [0u8; FRAME_MAX];
            let pick = input.byte() % 4;
            let rest = input.rest();
            match pick {
                0 => {
                    if let Some(record) =
                        check_decode::<Captured>(frame::<Captured>(rest, &mut buf))
                    {
                        check_pcm(&record);
                    }
                }
                1 => {
                    if let Some(record) = check_decode::<Play>(frame::<Play>(rest, &mut buf)) {
                        check_pcm(&record);
                    }
                }
                2 => {
                    let _ = check_decode::<Devices>(frame::<Devices>(rest, &mut buf));
                }
                _ => {
                    let _ = check_decode::<Capture>(frame::<Capture>(rest, &mut buf));
                }
            }
        }
        // Records off a lossy ring the fuzzer writes.
        _ => {
            let memory = Hostile {
                input: &input,
                producer: Cell::new(input.u32()),
                consumer: Cell::new(input.u32()),
                slots: [
                    Cell::new(slot(&input)),
                    Cell::new(slot(&input)),
                    Cell::new(slot(&input)),
                    Cell::new(slot(&input)),
                ],
                last_load: Cell::new(0),
            };
            let Ok(mut rx) = LossyConsumer::<_, SLOT_LEN>::new(&memory) else {
                return;
            };
            for _ in 0..TAKES {
                if input.is_empty() {
                    break;
                }
                let (before, lost) = (memory.consumer.get(), rx.lost());
                let record = rx.try_recv_record();
                let after = memory.consumer.get();
                let read = u32::from(record.is_some());
                let passed = after.wrapping_sub(before);
                let counted = rx.lost().wrapping_sub(lost);
                assert_eq!(
                    u64::from(passed),
                    counted.wrapping_add(u64::from(read)) & u64::from(u32::MAX),
                    "a record passed neither read nor counted lost"
                );
                let Some(record) = record else {
                    assert!(memory.last_load.get().wrapping_sub(after) <= WINDOW);
                    continue;
                };
                assert!(
                    memory.last_load.get().wrapping_sub(after.wrapping_sub(1)) <= WINDOW,
                    "a record kept that the producer may have been writing"
                );
                match Header::parse(&record).map(|header| header.kind) {
                    Ok(PLAY_KIND) => {
                        if let Ok(play) = decode::<Play>(&record) {
                            check_pcm(&play);
                        }
                    }
                    Ok(DEVICES_KIND) => {
                        let _ = decode::<Devices>(&record);
                    }
                    Ok(CAPTURE_KIND) => {
                        let _ = decode::<Capture>(&record);
                    }
                    _ => {
                        if let Ok(captured) = decode::<Captured>(&record) {
                            check_pcm(&captured);
                        }
                    }
                }
            }
        }
    }
});
