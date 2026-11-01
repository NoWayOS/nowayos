// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: each audio record's decoder, given any byte string
//! up to its record's length and one more, never panics, overflows or reads
//! out of bounds; and each record, from any payload behind its own header,
//! accepts only what keeps its rules - a rate and channels in range, a
//! known flag, one frame to [`MAX_PCM`] bytes of whole frames with zeros
//! after them, a device's format or none - and encodes back to exactly the
//! bytes it came from, so every record has one encoding.
//!
//! That samples have only zeros after them is shown apart from the
//! decoder's own check: a record accepted encodes to the same bytes as the
//! one `Pcm::from_bytes` makes from its samples. The sample area is checked
//! sixteen bytes at a time, so the decoder's longest loop runs
//! [`MAX_PCM`] / 16 = 60 times whatever the input, and the comparison here
//! at most 992 / 16 = 62: every proof is unwound 64 times, and Kani checks
//! that the bound is enough.
//!
//! And the lossy ring's two ends (`lossy`): whatever indices a producer
//! publishes, the consumer copies only slots inside the ring, keeps a
//! record only inside the window the index loaded after its copy places,
//! and passes every record either read or counted lost; the producer writes
//! inside the ring and moves on by one a record. Its threaded race test and
//! the `audio_records` fuzz target cover the rest.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use nowayos_ipc::{HEADER_LEN, Header, Message, decode_exact, encode, encoded_len};

use crate::{
    CAPTURED_KIND, Capture, Captured, Devices, Format, MAX_CHANNELS, MAX_PCM, MAX_RATE, MIN_RATE,
    PLAY_KIND, Pcm, Play,
};

/// A PCM record's bytes, rounded up to sixteen: the buffer its proofs use.
const PCM_BUF: usize = 992;

/// A devices record's bytes, rounded up to sixteen.
const DEVICES_BUF: usize = 32;

const _: () = {
    assert!(encoded_len::<Captured>() <= PCM_BUF && PCM_BUF % 16 == 0);
    assert!(encoded_len::<Devices>() <= DEVICES_BUF);
    assert!(encoded_len::<Capture>() <= DEVICES_BUF);
};

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

/// A format in range.
fn format_in_range(format: Format) -> bool {
    (MIN_RATE..=MAX_RATE).contains(&format.rate())
        && (1..=MAX_CHANNELS).contains(&format.channels())
}

/// A PCM record's rules, its padding shown against a fresh record made
/// from its samples.
fn pcm_keeps_rules<const KIND: u16>(record: &Pcm<KIND>) -> bool {
    let mut decoded = [0u8; PCM_BUF];
    let mut made = [0u8; PCM_BUF];
    let len = record.bytes().len();
    format_in_range(record.format())
        && len >= record.format().frame_bytes()
        && len <= MAX_PCM
        && len % record.format().frame_bytes() == 0
        && record.frames() >= 1
        && encode(record, &mut decoded) == Ok(encoded_len::<Pcm<KIND>>())
        && Pcm::<KIND>::from_bytes(
            record.format(),
            record.position(),
            record.discontinuity(),
            record.bytes(),
        )
        .is_some_and(|fresh| encode(&fresh, &mut made) == Ok(encoded_len::<Pcm<KIND>>()))
        && same(&decoded, &made)
}

/// The microphone's records, from any bytes: the decoder returns.
#[kani::proof]
#[kani::unwind(64)]
fn captured_any_input() {
    let buf: [u8; PCM_BUF] = kani::any();
    let bytes = any_prefix(&buf);
    if decode_exact::<Captured>(bytes).is_ok() {
        assert!(bytes.len() == encoded_len::<Captured>());
    }
}

/// The speaker's records, from any bytes: the decoder returns.
#[kani::proof]
#[kani::unwind(64)]
fn play_any_input() {
    let buf: [u8; PCM_BUF] = kani::any();
    let bytes = any_prefix(&buf);
    if decode_exact::<Play>(bytes).is_ok() {
        assert!(bytes.len() == encoded_len::<Play>());
    }
}

/// A microphone record accepted keeps its rules and has one encoding.
#[kani::proof]
#[kani::unwind(64)]
fn captured_any_payload() {
    any_payload_keeps_rules::<Captured, PCM_BUF>(pcm_keeps_rules::<CAPTURED_KIND>);
}

/// As for [`captured_any_payload`], on the ring to the speaker.
#[kani::proof]
#[kani::unwind(64)]
fn play_any_payload() {
    any_payload_keeps_rules::<Play, PCM_BUF>(pcm_keeps_rules::<PLAY_KIND>);
}

/// A devices record, from any bytes, returns; one accepted gives each
/// device's format in range, or none, and has one encoding.
#[kani::proof]
#[kani::unwind(64)]
fn devices_any_input_and_payload() {
    let buf: [u8; DEVICES_BUF] = kani::any();
    let _ = decode_exact::<Devices>(any_prefix(&buf));
    any_payload_keeps_rules::<Devices, DEVICES_BUF>(|devices| {
        devices.speaker.is_none_or(format_in_range)
            && devices.microphone.is_none_or(format_in_range)
    });
}

/// A capture record, from any bytes, returns; one accepted has one
/// encoding - so no two byte strings ask the same thing.
#[kani::proof]
#[kani::unwind(64)]
fn capture_any_input_and_payload() {
    let buf: [u8; DEVICES_BUF] = kani::any();
    let _ = decode_exact::<Capture>(any_prefix(&buf));
    any_payload_keeps_rules::<Capture, DEVICES_BUF>(|_| true);
}

// ---- The lossy ring -----------------------------------------------

/// A lossy ring's memory as a hostile producer makes it: four slots of eight
/// bytes, each load of the producer index any value, each slot any bytes;
/// the consumer index stored is kept, and the last producer index loaded is
/// noted.
struct HostileProducer {
    consumer: core::cell::Cell<u32>,
    last_producer: core::cell::Cell<u32>,
}

impl nowayos_ipc::RingMemory<8> for HostileProducer {
    fn capacity(&self) -> u32 {
        4
    }
    fn load_producer_index(&self) -> u32 {
        let index = kani::any();
        self.last_producer.set(index);
        index
    }
    fn load_consumer_index(&self) -> u32 {
        self.consumer.get()
    }
    fn load_producer_epoch(&self) -> u32 {
        kani::any()
    }
    fn load_consumer_epoch(&self) -> u32 {
        kani::any()
    }
}

impl nowayos_ipc::ConsumerMemory<8> for HostileProducer {
    fn store_consumer_index(&self, index: u32) {
        self.consumer.set(index);
    }
    fn store_consumer_epoch(&self, _epoch: u32) {}
    fn read_slot(&self, slot: u32, out: &mut [u8; 8]) {
        assert!(slot < 4, "a slot outside the ring");
        *out = kani::any();
    }
}

/// Whatever indices a producer publishes, one `try_recv_record` from any
/// consumer index never panics or overflows and copies only slots inside
/// the ring; a record it returns was inside the window the producer index
/// loaded after its copy places - so not being written over - and when it
/// returns none the consumer is left inside the window of the last index it
/// loaded; and every record it passes is either the one returned or counted
/// lost (mod 2^32, as the indices run).
#[kani::proof]
#[kani::unwind(5)]
fn lossy_consumer_keeps_its_window_for_any_producer() {
    let start: u32 = kani::any();
    let memory = HostileProducer {
        consumer: core::cell::Cell::new(start),
        last_producer: core::cell::Cell::new(0),
    };
    let Ok(mut rx) = crate::LossyConsumer::<_, 8>::new(&memory) else {
        panic!("a capacity of 4 is refused");
    };
    let record = rx.try_recv_record();
    let index = memory.consumer.get();
    let last = memory.last_producer.get();
    let read = u32::from(record.is_some());
    if record.is_some() {
        assert!(last.wrapping_sub(index.wrapping_sub(1)) <= 3);
    } else {
        assert!(last.wrapping_sub(index) <= 3);
    }
    let passed = index.wrapping_sub(start);
    assert!(passed == (rx.lost() as u32).wrapping_add(read));
}

/// The lossy producer, from any index of its own and whatever the consumer
/// stores, publishes any record up to a slot long into a slot inside the
/// ring and moves its index on by exactly one; a longer one publishes
/// nothing.
#[kani::proof]
#[kani::unwind(10)]
fn lossy_producer_writes_inside_the_ring() {
    let ring: nowayos_ipc::InMemoryRing<4, 8> = nowayos_ipc::InMemoryRing::new();
    let start: u32 = kani::any();
    nowayos_ipc::ProducerMemory::store_producer_index(&ring, start);
    nowayos_ipc::ConsumerMemory::store_consumer_index(&ring, kani::any());
    let Ok(mut tx) = crate::LossyProducer::<_, 8>::new(&ring) else {
        panic!("a capacity of 4 is refused");
    };
    let bytes: [u8; 9] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= 9);
    let sent = tx.send_bytes(&bytes[..len]).is_ok();
    assert!(sent == (len <= 8));
    let moved = nowayos_ipc::RingMemory::load_producer_index(&ring).wrapping_sub(start);
    assert!(moved == u32::from(sent));
    assert!(tx.published() == nowayos_ipc::RingMemory::load_producer_index(&ring));
}
