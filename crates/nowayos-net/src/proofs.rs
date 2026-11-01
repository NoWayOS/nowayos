// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: the checks each end of a network queue makes on
//! what its peer writes to the shared memory, for every value the peer can
//! write there.
//!
//! * A descriptor decodes from any 16 bytes without a panic, and back to the
//!   same offset and length.
//! * [`DataRegion`] accepts a descriptor only inside the region, on a buffer
//!   boundary and - for a frame - no longer than a buffer and within the
//!   lengths asked for, and refuses every other with the error that says
//!   why: so a frame it accepts lies wholly inside the region.
//! * `dequeue` loads the producer's index once, and reads nothing - no
//!   descriptor, no index written - when that index puts more than the
//!   capacity in flight; otherwise it reads exactly the slot its own index
//!   names, before it publishes its new index. `enqueue` likewise writes
//!   nothing when the consumer's index puts more than the capacity in flight,
//!   or the queue is full. Neither end ever reads its own index from shared
//!   memory: each has it privately (`Producer::at` and `Consumer::at` set it
//!   anywhere, so the proofs cover every index of both ends).
//! * Whatever the flag holds, the producer notifies only when it is 0, and
//!   writes it only to 1; the consumer writes it only to 0.
//!
//! The peer's memory is [`Hostile`]: every load returns any value, every
//! descriptor read any bytes, and what the end under proof does to it is
//! recorded. Every loop has a constant bound, so no unwinding bound is
//! needed.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them, and
//! `tools/kani-mutation` checks that removing `DataRegion::buffer`'s bounds
//! check, or `dequeue`'s in-flight check, makes one fail.

use core::cell::Cell;

use crate::queue::{
    BUFFER_SIZE, Consumer, DESCRIPTOR_LEN, DataRegion, Descriptor, DescriptorError, MAX_CAPACITY,
    Producer, QueueError, QueueMemory,
};

/// A queue's shared memory written by a hostile peer: what the end under
/// proof loads is anything at all, and what it does is recorded, in order.
struct Hostile {
    capacity: u16,
    /// Every call the end made, counted: the order of the recorded ones.
    calls: Cell<u8>,
    tail_loads: Cell<u8>,
    loaded_tail: Cell<u16>,
    head_loads: Cell<u8>,
    loaded_head: Cell<u16>,
    signal_loads: Cell<u8>,
    loaded_signal: Cell<u32>,
    /// Stores of each index, the last value stored and the call it was.
    tail_stores: Cell<u8>,
    stored_tail: Cell<(u16, u8)>,
    head_stores: Cell<u8>,
    stored_head: Cell<(u16, u8)>,
    signal_stores: Cell<u8>,
    stored_signal: Cell<u32>,
    /// Descriptors read and written: the last slot, its bytes, its call.
    reads: Cell<u8>,
    read: Cell<(u16, [u8; DESCRIPTOR_LEN], u8)>,
    writes: Cell<u8>,
    written: Cell<(u16, [u8; DESCRIPTOR_LEN], u8)>,
}

impl Hostile {
    fn new(capacity: u16) -> Self {
        Self {
            capacity,
            calls: Cell::new(0),
            tail_loads: Cell::new(0),
            loaded_tail: Cell::new(0),
            head_loads: Cell::new(0),
            loaded_head: Cell::new(0),
            signal_loads: Cell::new(0),
            loaded_signal: Cell::new(0),
            tail_stores: Cell::new(0),
            stored_tail: Cell::new((0, 0)),
            head_stores: Cell::new(0),
            stored_head: Cell::new((0, 0)),
            signal_stores: Cell::new(0),
            stored_signal: Cell::new(0),
            reads: Cell::new(0),
            read: Cell::new((0, [0; DESCRIPTOR_LEN], 0)),
            writes: Cell::new(0),
            written: Cell::new((0, [0; DESCRIPTOR_LEN], 0)),
        }
    }

    /// Counts a call and returns its number.
    fn call(&self) -> u8 {
        let call = self.calls.get() + 1;
        self.calls.set(call);
        call
    }

    /// Nothing written to the memory: no index, flag or descriptor.
    fn nothing_written(&self) -> bool {
        self.tail_stores.get() == 0
            && self.head_stores.get() == 0
            && self.signal_stores.get() == 0
            && self.writes.get() == 0
    }
}

impl QueueMemory for Hostile {
    fn capacity(&self) -> u16 {
        self.capacity
    }

    fn load_tail(&self) -> u16 {
        self.call();
        let tail = kani::any();
        self.tail_loads.set(self.tail_loads.get() + 1);
        self.loaded_tail.set(tail);
        tail
    }

    fn load_head(&self) -> u16 {
        self.call();
        let head = kani::any();
        self.head_loads.set(self.head_loads.get() + 1);
        self.loaded_head.set(head);
        head
    }

    fn store_tail(&self, tail: u16) {
        let call = self.call();
        self.tail_stores.set(self.tail_stores.get() + 1);
        self.stored_tail.set((tail, call));
    }

    fn store_head(&self, head: u16) {
        let call = self.call();
        self.head_stores.set(self.head_stores.get() + 1);
        self.stored_head.set((head, call));
    }

    fn load_signal(&self) -> u32 {
        self.call();
        let signal = kani::any();
        self.signal_loads.set(self.signal_loads.get() + 1);
        self.loaded_signal.set(signal);
        signal
    }

    fn store_signal(&self, value: u32) {
        self.call();
        self.signal_stores.set(self.signal_stores.get() + 1);
        self.stored_signal.set(value);
    }

    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]) {
        let call = self.call();
        assert!(slot < self.capacity, "a descriptor read outside the queue");
        *out = kani::any();
        self.reads.set(self.reads.get() + 1);
        self.read.set((slot, *out, call));
    }

    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]) {
        let call = self.call();
        assert!(
            slot < self.capacity,
            "a descriptor written outside the queue"
        );
        self.writes.set(self.writes.get() + 1);
        self.written.set((slot, *bytes, call));
    }
}

/// Whether `capacity` is one a queue may have.
fn valid(capacity: u16) -> bool {
    capacity.is_power_of_two() && capacity <= MAX_CAPACITY
}

/// Any 16 bytes decode to the offset and length in their first ten, and
/// encode back to those ten and six zeros.
#[kani::proof]
fn any_descriptor_bytes_decode() {
    let bytes: [u8; DESCRIPTOR_LEN] = kani::any();
    let descriptor = Descriptor::from_bytes(&bytes);
    let mut offset = 0u64;
    let mut at = 0;
    while at < 8 {
        offset |= u64::from(bytes[at]) << (8 * at);
        at += 1;
    }
    assert!(descriptor.io_or_offset == offset);
    assert!(descriptor.len == u16::from(bytes[8]) | (u16::from(bytes[9]) << 8));
    let again = descriptor.to_bytes();
    assert!(again[..10] == bytes[..10]);
    assert!(again[10..] == [0; 6]);
    assert!(Descriptor::from_bytes(&again) == descriptor);
}

/// A region of any number of buffers, any descriptor and any lengths: the
/// region accepts a buffer only inside it and on a boundary, and a frame
/// only there and no longer than a buffer, within the lengths; it refuses
/// every other descriptor with the error that says why.
#[kani::proof]
fn data_region_accepts_only_buffers_inside_it() {
    let buffers: usize = kani::any();
    let Some(region) = DataRegion::new(buffers) else {
        assert!(buffers == 0 || buffers.checked_mul(BUFFER_SIZE).is_none());
        return;
    };
    assert!(region.buffers() == buffers && !region.is_empty());
    assert!(Some(region.len()) == buffers.checked_mul(BUFFER_SIZE));
    let descriptor = Descriptor::new(kani::any(), kani::any());
    let offset = descriptor.io_or_offset;
    let inside = u128::from(offset) < region.len() as u128;
    let on_boundary = (offset & (BUFFER_SIZE as u64 - 1)) == 0;
    match region.buffer(descriptor) {
        Ok(buffer) => {
            assert!(inside, "a descriptor outside the region was accepted");
            assert!(
                on_boundary,
                "a descriptor off a buffer boundary was accepted"
            );
            assert!(buffer < buffers);
            assert!(buffer as u128 * BUFFER_SIZE as u128 == u128::from(offset));
        }
        Err(DescriptorError::OutsideRegion { offset: refused }) => {
            assert!(!inside && refused == offset);
        }
        Err(DescriptorError::NotOnBoundary { offset: refused }) => {
            assert!(inside && !on_boundary && refused == offset);
        }
        Err(DescriptorError::BadLength { .. }) => panic!("a buffer refused for its length"),
    }
    let shortest: usize = kani::any();
    let longest: usize = kani::any();
    let len = usize::from(descriptor.len);
    let fits = shortest <= len && len <= longest && len <= BUFFER_SIZE;
    match region.frame(descriptor, shortest..=longest) {
        Ok(frame) => {
            assert!(inside, "a frame outside the region was accepted");
            assert!(on_boundary && fits);
            assert!(frame.offset as u128 == u128::from(offset));
            assert!(frame.buffer < buffers && frame.buffer * BUFFER_SIZE == frame.offset);
            assert!(frame.len == len);
            // So the frame lies wholly inside the region.
            assert!(frame.offset + frame.len <= region.len());
        }
        Err(DescriptorError::OutsideRegion { offset: refused }) => {
            assert!(!inside && refused == offset);
        }
        Err(DescriptorError::NotOnBoundary { offset: refused }) => {
            assert!(inside && !on_boundary && refused == offset);
        }
        Err(DescriptorError::BadLength { len: refused }) => {
            assert!(inside && on_boundary && !fits && refused == descriptor.len);
        }
    }
}

/// Any capacity, any index of the consumer's own, any index the producer
/// writes and any descriptor bytes: `dequeue` loads the producer's index
/// once; with more than the capacity in flight it reads and writes
/// nothing; with none, the same; otherwise it reads exactly the slot its
/// own index names, then publishes the next index, and returns exactly the
/// bytes it read.
#[kani::proof]
fn dequeue_reads_only_what_is_in_flight() {
    let memory = Hostile::new(kani::any());
    let head: u16 = kani::any();
    let Ok(mut consumer) = Consumer::at(&memory, head) else {
        assert!(!valid(memory.capacity) && memory.nothing_written());
        return;
    };
    let capacity = memory.capacity;
    assert!(valid(capacity) && consumer.capacity() == capacity);
    let result = consumer.dequeue();
    assert!(
        memory.tail_loads.get() == 1,
        "the producer's index loaded twice"
    );
    assert!(memory.head_loads.get() == 0 && memory.signal_loads.get() == 0);
    assert!(memory.tail_stores.get() == 0 && memory.writes.get() == 0);
    assert!(memory.signal_stores.get() == 0);
    let tail = memory.loaded_tail.get();
    let in_flight = tail.wrapping_sub(head);
    match result {
        Err(QueueError::CorruptIndex {
            tail: told,
            head: own,
        }) => {
            assert!(in_flight > capacity && told == tail && own == head);
            assert!(
                memory.reads.get() == 0,
                "a descriptor read past the capacity"
            );
            assert!(memory.head_stores.get() == 0 && consumer.index() == head);
        }
        Err(_) => panic!("dequeue failed other than on a corrupt index"),
        Ok(None) => {
            assert!(in_flight == 0);
            assert!(memory.reads.get() == 0 && memory.head_stores.get() == 0);
            assert!(consumer.index() == head);
        }
        Ok(Some(descriptor)) => {
            assert!(in_flight >= 1, "a descriptor read from an empty queue");
            assert!(in_flight <= capacity, "a descriptor read past the capacity");
            let (slot, bytes, read_at) = memory.read.get();
            assert!(memory.reads.get() == 1 && slot == head % capacity);
            let (stored, stored_at) = memory.stored_head.get();
            assert!(memory.head_stores.get() == 1 && stored == head.wrapping_add(1));
            assert!(read_at < stored_at, "the slot freed before it was copied");
            assert!(consumer.index() == stored);
            assert!(descriptor == Descriptor::from_bytes(&bytes));
        }
    }
}

/// Any capacity, any index of the producer's own, any index the consumer
/// writes and any descriptor: `enqueue` loads the consumer's index once;
/// with more than the capacity in flight, or a full queue, it writes
/// nothing; otherwise it writes exactly the descriptor's bytes to the slot
/// its own index names, then publishes the next index.
#[kani::proof]
fn enqueue_writes_only_into_free_slots() {
    let memory = Hostile::new(kani::any());
    let tail: u16 = kani::any();
    let Ok(mut producer) = Producer::at(&memory, tail) else {
        assert!(!valid(memory.capacity) && memory.nothing_written());
        return;
    };
    let capacity = memory.capacity;
    assert!(valid(capacity) && producer.capacity() == capacity);
    let descriptor = Descriptor::new(kani::any(), kani::any());
    let result = producer.enqueue(descriptor);
    assert!(
        memory.head_loads.get() == 1,
        "the consumer's index loaded twice"
    );
    assert!(memory.tail_loads.get() == 0 && memory.signal_loads.get() == 0);
    assert!(memory.head_stores.get() == 0 && memory.reads.get() == 0);
    assert!(memory.signal_stores.get() == 0);
    let head = memory.loaded_head.get();
    let in_flight = tail.wrapping_sub(head);
    match result {
        Err(QueueError::CorruptIndex {
            tail: own,
            head: told,
        }) => {
            assert!(in_flight > capacity && own == tail && told == head);
            assert!(
                memory.nothing_written(),
                "a descriptor written past the capacity"
            );
            assert!(producer.index() == tail);
        }
        Err(QueueError::Full) => {
            assert!(in_flight == capacity && memory.nothing_written());
            assert!(producer.index() == tail);
        }
        Err(_) => panic!("enqueue failed other than on a corrupt index or a full queue"),
        Ok(()) => {
            assert!(
                in_flight < capacity,
                "a descriptor written into a full queue"
            );
            let (slot, bytes, written_at) = memory.written.get();
            assert!(memory.writes.get() == 1 && slot == tail % capacity);
            assert!(bytes == descriptor.to_bytes());
            let (stored, stored_at) = memory.stored_tail.get();
            assert!(memory.tail_stores.get() == 1 && stored == tail.wrapping_add(1));
            assert!(
                written_at < stored_at,
                "the index published before the descriptor"
            );
            assert!(producer.index() == stored);
        }
    }
}

/// Whatever the flag holds: the producer notifies exactly when it is 0, and
/// then writes 1 and nothing else; the consumer writes 0 and nothing else.
#[kani::proof]
fn the_signal_flag_is_only_a_hint() {
    let capacity: u16 = kani::any();
    kani::assume(valid(capacity));
    let memory = Hostile::new(capacity);
    let producer = Producer::new(&memory).ok().unwrap();
    let wanted = producer.signal_wanted();
    assert!(memory.signal_loads.get() == 1);
    assert!(wanted == (memory.loaded_signal.get() == 0));
    assert!(memory.signal_stores.get() == u8::from(wanted));
    if wanted {
        assert!(memory.stored_signal.get() == 1);
    }
    assert!(memory.tail_loads.get() == 0 && memory.head_loads.get() == 0);
    assert!(memory.tail_stores.get() == 0 && memory.head_stores.get() == 0);
    assert!(memory.reads.get() == 0 && memory.writes.get() == 0);

    let memory = Hostile::new(capacity);
    let consumer = Consumer::new(&memory).ok().unwrap();
    consumer.request_signal();
    assert!(memory.signal_stores.get() == 1 && memory.stored_signal.get() == 0);
    assert!(
        memory.calls.get() == 1,
        "the consumer touched more than the flag"
    );
}
