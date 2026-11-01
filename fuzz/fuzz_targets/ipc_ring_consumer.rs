// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-ipc`'s ring `Consumer` over shared memory that a
//! hostile producer changes between the consumer's reads, and during them.
//!
//! The fuzzer's bytes are the producer: its index, its restart epoch and the
//! slots, changed at every load the consumer makes and in the middle of every
//! slot it copies, so a copy can be torn. They also set the memory's starting
//! contents and the capacity the ring claims (some invalid), and choose what
//! the consumer does next, starting again as after a restart included.
//! Whatever the producer does, the consumer must:
//!
//! * never panic, and copy only the slot its own index names, inside the
//!   ring;
//! * copy each record exactly once, before looking at it: what it returns or
//!   decodes is that copy, whatever the slot holds afterwards (no double
//!   fetch);
//! * take a record only when the producer index it has just loaded is 1 to
//!   `capacity` records ahead of its own, and then move its own index on by
//!   exactly one - for a malformed record too - or, when it skips what is in
//!   flight, to that producer index: never anywhere else;
//! * refuse a producer index more than `capacity` ahead, touching nothing.

#![no_main]

use core::cell::Cell;

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{Input, check_decode};
use nowayos_input::InputEvent;
use nowayos_input::broker::UI_RING_SLOT;
use nowayos_ipc::{
    Consumer, ConsumerMemory, EpochChange, EpochWatch, Header, MAX_CAPACITY, Message, RecvError,
    RingError, RingMemory, decode,
};

/// Bytes per slot: the UI's input ring's.
const SLOT: usize = UI_RING_SLOT;
/// Slots the memory holds. A ring claiming more reads zeros beyond them.
const ROOM: usize = 8;
/// The most steps one input drives.
const MAX_STEPS: usize = 1024;

/// A ring's shared memory with its producer side driven by the fuzzer, and a
/// record of what the consumer did with it.
struct HostileRing<'a> {
    input: &'a Input<'a>,
    capacity: u32,
    producer_index: Cell<u32>,
    producer_epoch: Cell<u32>,
    consumer_index: Cell<u32>,
    consumer_epoch: Cell<u32>,
    slots: [Cell<[u8; SLOT]>; ROOM],
    /// Slots the consumer has copied.
    copies: Cell<u32>,
    /// The slot it copied last, and the bytes it got.
    last_copy: Cell<Option<(u32, [u8; SLOT])>>,
    /// The producer index it loaded last.
    last_load: Cell<u32>,
}

/// The consumer's side before a step, to compare with after it.
#[derive(Clone, Copy, Debug)]
struct Before {
    index: u32,
    copies: u32,
}

impl<'a> HostileRing<'a> {
    /// The memory as the fuzzer sets it up: the capacity the ring claims, the
    /// consumer's index and the producer's, a few records ahead or anywhere.
    fn new(input: &'a Input<'a>) -> Self {
        let capacity = match input.byte() % 8 {
            0 => 0,
            1 => 3,
            2 => MAX_CAPACITY,
            3 => input.u32(),
            4 => 1,
            5 => 2,
            6 => 4,
            _ => 8,
        };
        let consumer_index = input.u32();
        let producer_index = match input.byte() {
            0xFF => input.u32(),
            ahead => consumer_index.wrapping_add(u32::from(ahead % 16)),
        };
        Self {
            input,
            capacity,
            producer_index: Cell::new(producer_index),
            producer_epoch: Cell::new(input.u32()),
            consumer_index: Cell::new(consumer_index),
            consumer_epoch: Cell::new(input.u32()),
            slots: [const { Cell::new([0; SLOT]) }; ROOM],
            copies: Cell::new(0),
            last_copy: Cell::new(None),
            last_load: Cell::new(0),
        }
    }

    fn cell(&self, slot: u32) -> Option<&Cell<[u8; SLOT]>> {
        usize::try_from(slot)
            .ok()
            .and_then(|slot| self.slots.get(slot))
    }

    /// The producer does something, as the fuzzer's next byte says.
    fn producer_acts(&self) {
        match self.input.byte() % 8 {
            // Publishes a record as an honest producer does: an input
            // event's header and any payload, or any bytes at all.
            0 | 1 => {
                let index = self.producer_index.get();
                let mut record: [u8; SLOT] = self.input.array();
                if self.input.byte().is_multiple_of(2) {
                    let header = Header {
                        kind: InputEvent::KIND,
                        version: InputEvent::VERSION,
                        payload_len: u16::try_from(InputEvent::PAYLOAD_LEN).unwrap_or(0),
                    };
                    let _ = header.write(&mut record);
                }
                if let Some(cell) = self.cell(index & self.capacity.wrapping_sub(1)) {
                    cell.set(record);
                }
                self.producer_index.set(index.wrapping_add(1));
            }
            // Moves its index anywhere.
            2 => self.producer_index.set(self.input.u32()),
            // Moves its index a little, either way.
            3 => self.producer_index.set(
                self.producer_index
                    .get()
                    .wrapping_add(u32::from(self.input.byte()))
                    .wrapping_sub(128),
            ),
            // Changes one byte of a slot, published or not.
            4 => {
                let slot = u32::from(self.input.byte() % 8);
                let at = usize::from(self.input.byte()) % SLOT;
                if let Some(cell) = self.cell(slot) {
                    let mut bytes = cell.get();
                    if let Some(byte) = bytes.get_mut(at) {
                        *byte = self.input.byte();
                    }
                    cell.set(bytes);
                }
            }
            // Changes its restart epoch.
            5 => self.producer_epoch.set(self.input.u32()),
            _ => {}
        }
    }

    fn before(&self) -> Before {
        Before {
            index: self.consumer_index.get(),
            copies: self.copies.get(),
        }
    }

    /// Records in flight by the producer index the consumer loaded last.
    fn ahead(&self, before: Before) -> u32 {
        self.last_load.get().wrapping_sub(before.index)
    }

    /// Nothing copied, and the consumer's index where it was.
    fn untouched(&self, before: Before) -> bool {
        self.copies.get() == before.copies && self.consumer_index.get() == before.index
    }

    /// The copy, if exactly one slot was copied - the one the consumer's own
    /// index named, with a producer index 1 to `capacity` ahead - and the
    /// consumer's index moved on by one.
    fn took_one(&self, before: Before) -> Option<[u8; SLOT]> {
        let (slot, bytes) = self.last_copy.get()?;
        (self.copies.get() == before.copies.wrapping_add(1)
            && slot == before.index & self.capacity.wrapping_sub(1)
            && (1..=self.capacity).contains(&self.ahead(before))
            && self.consumer_index.get() == before.index.wrapping_add(1))
        .then_some(bytes)
    }

    /// A refusal of the producer index the consumer loaded last, more than
    /// `capacity` ahead, with nothing touched.
    fn refused(&self, before: Before, error: RingError) -> bool {
        error
            == RingError::CorruptIndices {
                producer: self.last_load.get(),
                consumer: before.index,
            }
            && self.ahead(before) > self.capacity
            && self.untouched(before)
    }
}

impl RingMemory<SLOT> for HostileRing<'_> {
    fn capacity(&self) -> u32 {
        self.capacity
    }

    fn load_producer_index(&self) -> u32 {
        self.producer_acts();
        let index = self.producer_index.get();
        self.last_load.set(index);
        index
    }

    fn load_consumer_index(&self) -> u32 {
        self.consumer_index.get()
    }

    fn load_producer_epoch(&self) -> u32 {
        self.producer_acts();
        self.producer_epoch.get()
    }

    fn load_consumer_epoch(&self) -> u32 {
        self.consumer_epoch.get()
    }
}

impl ConsumerMemory<SLOT> for HostileRing<'_> {
    fn store_consumer_index(&self, index: u32) {
        self.consumer_index.set(index);
    }

    fn store_consumer_epoch(&self, epoch: u32) {
        self.consumer_epoch.set(epoch);
    }

    fn read_slot(&self, slot: u32, out: &mut [u8; SLOT]) {
        assert!(
            slot < self.capacity,
            "read_slot({slot}) in a ring of {} slots",
            self.capacity
        );
        // The producer may write while the slot is copied: the copy tears.
        let tear = usize::from(self.input.byte()) % SLOT;
        for (at, byte) in out.iter_mut().enumerate() {
            if at == tear {
                self.producer_acts();
            }
            *byte = self
                .cell(slot)
                .and_then(|cell| cell.get().get(at).copied())
                .unwrap_or(0);
        }
        self.copies.set(self.copies.get().wrapping_add(1));
        self.last_copy.set(Some((slot, *out)));
        self.producer_acts();
    }
}

/// Attaches a consumer, as a protection domain does when it starts, and
/// checks that it attaches only to a valid ring whose indices make sense.
fn attach<'r>(ring: &'r HostileRing<'r>) -> Option<Consumer<'r, HostileRing<'r>, SLOT>> {
    let before = ring.before();
    let valid = ring.capacity.is_power_of_two() && ring.capacity <= MAX_CAPACITY;
    let result = Consumer::new(ring);
    let consistent = match &result {
        Ok(_) => valid && ring.ahead(before) <= ring.capacity && ring.untouched(before),
        Err(RingError::InvalidCapacity { capacity }) => {
            !valid && *capacity == ring.capacity && ring.untouched(before)
        }
        Err(error) => valid && ring.refused(before, *error),
    };
    assert!(
        consistent,
        "attach: {:?}, capacity {}, {before:?}, producer loaded {}",
        result.as_ref().err(),
        ring.capacity,
        ring.last_load.get()
    );
    result.ok()
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let ring = HostileRing::new(&input);
    let Some(mut consumer) = attach(&ring) else {
        return;
    };
    let mut watch = EpochWatch::new();
    let mut seen = None;
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        let before = ring.before();
        let step = input.byte() % 7;
        let consistent = match step {
            0 => match consumer.try_recv_record() {
                Ok(Some(record)) => {
                    let bytes = record.as_bytes();
                    check_decode::<InputEvent>(bytes);
                    ring.took_one(before) == Some(*bytes)
                        && record.header() == Header::parse(bytes)
                        && record.decode::<InputEvent>() == decode(bytes)
                }
                Ok(None) => ring.ahead(before) == 0 && ring.untouched(before),
                Err(error) => ring.refused(before, error),
            },
            1 => match consumer.try_recv::<InputEvent>() {
                Ok(Some(event)) => ring
                    .took_one(before)
                    .is_some_and(|copy| decode(&copy) == Ok(event)),
                Err(RecvError::Decode(error)) => ring
                    .took_one(before)
                    .is_some_and(|copy| decode::<InputEvent>(&copy) == Err(error)),
                Ok(None) => ring.ahead(before) == 0 && ring.untouched(before),
                Err(RecvError::Ring(error)) => ring.refused(before, error),
                Err(_) => false,
            },
            2 => match consumer.pending() {
                Ok(pending) => {
                    pending == ring.ahead(before)
                        && pending <= ring.capacity
                        && ring.untouched(before)
                }
                Err(error) => ring.refused(before, error),
            },
            3 => match consumer.skip_in_flight() {
                Ok(skipped) => {
                    skipped == ring.ahead(before)
                        && skipped <= ring.capacity
                        && ring.copies.get() == before.copies
                        && ring.consumer_index.get() == ring.last_load.get()
                }
                Err(error) => ring.refused(before, error),
            },
            4 => {
                let next = match ring.consumer_epoch.get().wrapping_add(1) {
                    0 => 1,
                    next => next,
                };
                consumer.begin_epoch() == next
                    && ring.consumer_epoch.get() == next
                    && ring.untouched(before)
            }
            5 => {
                let epoch = consumer.peer_epoch();
                let expected = match seen {
                    None => EpochChange::First(epoch),
                    Some(last) if last == epoch => EpochChange::Same(epoch),
                    Some(last) => EpochChange::Changed {
                        from: last,
                        to: epoch,
                    },
                };
                seen = Some(epoch);
                watch.observe(epoch) == expected
                    && epoch == ring.producer_epoch.get()
                    && ring.untouched(before)
            }
            // The consumer starts again, as after a restart: from its own
            // index in the memory, with nothing seen of its peer.
            _ => {
                let Some(again) = attach(&ring) else {
                    return;
                };
                consumer = again;
                watch = EpochWatch::new();
                seen = None;
                true
            }
        };
        assert!(
            consistent,
            "step {step}: capacity {}, {before:?}, consumer index now {}, producer loaded {}, copies now {}",
            ring.capacity,
            ring.consumer_index.get(),
            ring.last_load.get(),
            ring.copies.get()
        );
    }
});
