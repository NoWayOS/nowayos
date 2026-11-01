// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-net`'s network queues - each end over
//! shared memory its peer writes as it likes, between the end's reads and
//! during them - and `DataRegion`'s checks on every descriptor that comes
//! out.
//!
//! The fuzzer's first bytes choose which end is under test, the queue's
//! capacity (some invalid) and the data region's size; the rest are the
//! peer and the steps. Both sides map a queue read-write, so the peer
//! writes either index, the signal flag and any slot, at every load the end
//! makes and in the middle of every descriptor it copies, so a copy can be
//! torn.
//!
//! Against a hostile producer, the consumer must:
//!
//! * load the producer's index once a dequeue, and take a descriptor only
//!   when that index is 1 to `capacity` ahead of its own: reading exactly
//!   the slot its own index names, once, then publishing its index moved on
//!   by one - never anywhere else - and returning exactly the bytes it read;
//! * read nothing and write nothing when that index is more than `capacity`
//!   ahead, refusing it with the indices it saw, and nothing when it is
//!   level;
//! * never read its own index, or the flag, from the memory, and write only
//!   its own index, and 0 to the flag when it asks for a notification.
//!
//! Each descriptor it takes is checked with `DataRegion::buffer` and
//! `DataRegion::frame`, against the lengths a driver and its client use or
//! any, and the answer compared with the rules worked out here in 128-bit
//! arithmetic: accepted exactly when inside the region, on a buffer boundary
//! and, for a frame, within the lengths and no longer than a buffer - so the
//! frame lies inside the region - and refused with the error that says why.
//! A frame accepted is then taken into a `BufferSet`, as the PDs do, which
//! must refuse a buffer already held.
//!
//! Against a hostile consumer, the producer must load the consumer's index
//! once an enqueue, write exactly the descriptor's bytes into the slot its
//! own index names and then publish its index moved on by one, only when
//! fewer than `capacity` are in flight; write nothing when the queue is full
//! or the consumer's index puts more than `capacity` in flight; and notify
//! exactly when the flag it loads is 0, then writing 1 and nothing else.

#![no_main]

use core::cell::Cell;
use core::ops::RangeInclusive;

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_net::{
    BUFFER_SIZE, BufferSet, Consumer, DESCRIPTOR_LEN, DataRegion, Descriptor, DescriptorError,
    ETHERNET_HEADER_LEN, MAX_CAPACITY, MAX_FRAME_LEN, Producer, QueueError, QueueMemory,
};

/// Slots the memory holds. A queue claiming more reads zeros beyond them.
const ROOM: usize = 8;
/// Buffers a `BufferSet` here tracks; a region may have more.
const HELD: usize = 16;
/// The most steps one input drives.
const MAX_STEPS: usize = 1024;

/// Which end is under test; the other is the fuzzer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Consumer,
    Producer,
}

/// What the end under test did to the memory, counted since the last step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Done {
    tail_loads: u32,
    head_loads: u32,
    signal_loads: u32,
    tail_stores: u32,
    head_stores: u32,
    signal_stores: u32,
    reads: u32,
    writes: u32,
}

/// A queue's shared memory with one end driven by the fuzzer, and a record
/// of what the other end did with it.
struct HostileQueue<'a> {
    input: &'a Input<'a>,
    end: End,
    capacity: u16,
    tail: Cell<u16>,
    head: Cell<u16>,
    signal: Cell<u32>,
    slots: [Cell<[u8; DESCRIPTOR_LEN]>; ROOM],
    done: Cell<Done>,
    /// Every call the end made, numbered, to put them in order.
    calls: Cell<u32>,
    /// The values the end loaded last.
    loaded_tail: Cell<u16>,
    loaded_head: Cell<u16>,
    loaded_signal: Cell<u32>,
    /// The last values it stored, and the call each was.
    stored_tail: Cell<(u16, u32)>,
    stored_head: Cell<(u16, u32)>,
    stored_signal: Cell<u32>,
    /// The last descriptor it read or wrote: its slot, bytes and call.
    read: Cell<(u16, [u8; DESCRIPTOR_LEN], u32)>,
    written: Cell<(u16, [u8; DESCRIPTOR_LEN], u32)>,
}

impl<'a> HostileQueue<'a> {
    fn new(input: &'a Input<'a>, end: End) -> Self {
        let capacity = match input.byte() % 8 {
            0 => 0,
            1 => 3,
            2 => MAX_CAPACITY,
            3 => input.u16(),
            4 => 1,
            5 => 2,
            6 => 4,
            _ => 8,
        };
        Self {
            input,
            end,
            capacity,
            tail: Cell::new(input.u16()),
            head: Cell::new(input.u16()),
            signal: Cell::new(input.u32()),
            slots: [const { Cell::new([0; DESCRIPTOR_LEN]) }; ROOM],
            done: Cell::new(Done::default()),
            calls: Cell::new(0),
            loaded_tail: Cell::new(0),
            loaded_head: Cell::new(0),
            loaded_signal: Cell::new(0),
            stored_tail: Cell::new((0, 0)),
            stored_head: Cell::new((0, 0)),
            stored_signal: Cell::new(0),
            read: Cell::new((0, [0; DESCRIPTOR_LEN], 0)),
            written: Cell::new((0, [0; DESCRIPTOR_LEN], 0)),
        }
    }

    fn cell(&self, slot: u16) -> Option<&Cell<[u8; DESCRIPTOR_LEN]>> {
        self.slots.get(usize::from(slot))
    }

    /// Counts one call of the end's, returning its number.
    fn call(&self, count: impl FnOnce(&mut Done)) -> u32 {
        let mut done = self.done.get();
        count(&mut done);
        self.done.set(done);
        let call = self.calls.get().wrapping_add(1);
        self.calls.set(call);
        call
    }

    /// Takes what the end did since the last step, starting the count again.
    fn take_done(&self) -> Done {
        self.done.replace(Done::default())
    }

    /// A descriptor's 16 bytes as the fuzzer makes them: often a buffer of
    /// the region or near it, with a frame's usual lengths, so that many are
    /// accepted; or any bytes, owner id and padding included.
    fn descriptor_bytes(&self) -> [u8; DESCRIPTOR_LEN] {
        if self.input.byte().is_multiple_of(4) {
            return self.input.array();
        }
        let buffer = u64::from(self.input.byte() % 20);
        let mut offset = buffer.saturating_mul(u64::try_from(BUFFER_SIZE).unwrap_or(u64::MAX));
        if self.input.byte().is_multiple_of(4) {
            offset = offset.wrapping_add(u64::from(self.input.u16()));
        }
        let len = match self.input.byte() % 6 {
            0 => 0,
            1 => 14,
            2 => 1514,
            3 => 2048,
            4 => self.input.u16(),
            _ => 60_u16.saturating_add(u16::from(self.input.byte())),
        };
        let mut bytes = Descriptor::new(offset, len).to_bytes();
        if self.input.byte().is_multiple_of(8)
            && let Some(rest) = bytes.get_mut(10..)
        {
            rest.copy_from_slice(&self.input.array::<6>());
        }
        bytes
    }

    /// The peer does something, as the fuzzer's next byte says.
    fn peer_acts(&self) {
        match self.input.byte() % 10 {
            // An honest producer's enqueue, or an honest consumer's dequeue.
            0 | 1 => match self.end {
                End::Consumer => {
                    let tail = self.tail.get();
                    if let Some(cell) = self.cell(tail & self.capacity.wrapping_sub(1)) {
                        cell.set(self.descriptor_bytes());
                    }
                    self.tail.set(tail.wrapping_add(1));
                }
                End::Producer => self.head.set(self.head.get().wrapping_add(1)),
            },
            // Either index anywhere, or moved a little either way.
            2 => self.tail.set(self.input.u16()),
            3 => self.head.set(self.input.u16()),
            4 => self.tail.set(
                self.tail
                    .get()
                    .wrapping_add(u16::from(self.input.byte()))
                    .wrapping_sub(128),
            ),
            5 => self.head.set(
                self.head
                    .get()
                    .wrapping_add(u16::from(self.input.byte()))
                    .wrapping_sub(128),
            ),
            // The flag, to anything: most often 0 or 1.
            6 => self.signal.set(match self.input.byte() % 3 {
                0 => 0,
                1 => 1,
                _ => self.input.u32(),
            }),
            // A slot, published or not, to a descriptor of its making, or
            // one byte of it changed.
            7 => {
                if let Some(cell) = self.cell(u16::from(self.input.byte()) % 8) {
                    cell.set(self.descriptor_bytes());
                }
            }
            8 => {
                if let Some(cell) = self.cell(u16::from(self.input.byte()) % 8) {
                    let mut bytes = cell.get();
                    if let Some(byte) =
                        bytes.get_mut(usize::from(self.input.byte()) % DESCRIPTOR_LEN)
                    {
                        *byte = self.input.byte();
                    }
                    cell.set(bytes);
                }
            }
            _ => {}
        }
    }
}

impl QueueMemory for HostileQueue<'_> {
    fn capacity(&self) -> u16 {
        self.capacity
    }

    fn load_tail(&self) -> u16 {
        assert_eq!(self.end, End::Consumer, "the producer loaded its own index");
        self.call(|done| done.tail_loads = done.tail_loads.wrapping_add(1));
        self.peer_acts();
        let tail = self.tail.get();
        self.loaded_tail.set(tail);
        tail
    }

    fn load_head(&self) -> u16 {
        assert_eq!(self.end, End::Producer, "the consumer loaded its own index");
        self.call(|done| done.head_loads = done.head_loads.wrapping_add(1));
        self.peer_acts();
        let head = self.head.get();
        self.loaded_head.set(head);
        head
    }

    fn store_tail(&self, tail: u16) {
        assert_eq!(
            self.end,
            End::Producer,
            "the consumer wrote the producer's index"
        );
        let call = self.call(|done| done.tail_stores = done.tail_stores.wrapping_add(1));
        self.tail.set(tail);
        self.stored_tail.set((tail, call));
        self.peer_acts();
    }

    fn store_head(&self, head: u16) {
        assert_eq!(
            self.end,
            End::Consumer,
            "the producer wrote the consumer's index"
        );
        let call = self.call(|done| done.head_stores = done.head_stores.wrapping_add(1));
        self.head.set(head);
        self.stored_head.set((head, call));
        self.peer_acts();
    }

    fn load_signal(&self) -> u32 {
        assert_eq!(self.end, End::Producer, "the consumer loaded the flag");
        self.call(|done| done.signal_loads = done.signal_loads.wrapping_add(1));
        self.peer_acts();
        let signal = self.signal.get();
        self.loaded_signal.set(signal);
        signal
    }

    fn store_signal(&self, value: u32) {
        self.call(|done| done.signal_stores = done.signal_stores.wrapping_add(1));
        self.signal.set(value);
        self.stored_signal.set(value);
    }

    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]) {
        assert_eq!(self.end, End::Consumer, "the producer read a descriptor");
        assert!(
            slot < self.capacity,
            "read_descriptor({slot}) in a queue of {}",
            self.capacity
        );
        // The producer may write while the slot is copied: the copy tears.
        let tear = usize::from(self.input.byte()) % DESCRIPTOR_LEN;
        for (at, byte) in out.iter_mut().enumerate() {
            if at == tear {
                self.peer_acts();
            }
            *byte = self
                .cell(slot)
                .and_then(|cell| cell.get().get(at).copied())
                .unwrap_or(0);
        }
        let call = self.call(|done| done.reads = done.reads.wrapping_add(1));
        self.read.set((slot, *out, call));
        self.peer_acts();
    }

    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]) {
        assert_eq!(self.end, End::Producer, "the consumer wrote a descriptor");
        assert!(
            slot < self.capacity,
            "write_descriptor({slot}) in a queue of {}",
            self.capacity
        );
        if let Some(cell) = self.cell(slot) {
            cell.set(*bytes);
        }
        let call = self.call(|done| done.writes = done.writes.wrapping_add(1));
        self.written.set((slot, *bytes, call));
        self.peer_acts();
    }
}

/// Whether `capacity` is one a queue may have.
fn valid(capacity: u16) -> bool {
    capacity.is_power_of_two() && capacity <= MAX_CAPACITY
}

/// `slot` of `index` in a queue of `capacity`, worked out by remainder.
fn slot_of(index: u16, capacity: u16) -> u16 {
    index.checked_rem(capacity).unwrap_or(u16::MAX)
}

/// The rules a descriptor's buffer must keep, in 128-bit arithmetic: the
/// buffer it names, or the error that refuses it.
fn expected_buffer(buffers: usize, offset: u64) -> Result<usize, DescriptorError> {
    let size = u128::try_from(BUFFER_SIZE).unwrap_or(u128::MAX);
    let len = u128::try_from(buffers)
        .ok()
        .and_then(|buffers| buffers.checked_mul(size))
        .unwrap_or(u128::MAX);
    if u128::from(offset) >= len {
        return Err(DescriptorError::OutsideRegion { offset });
    }
    if u128::from(offset).checked_rem(size) != Some(0) {
        return Err(DescriptorError::NotOnBoundary { offset });
    }
    u128::from(offset)
        .checked_div(size)
        .and_then(|buffer| usize::try_from(buffer).ok())
        .ok_or(DescriptorError::OutsideRegion { offset })
}

/// Checks a descriptor the consumer took against the region, and takes a
/// frame it accepts into `held`, which `model` follows.
fn check_descriptor(
    region: DataRegion,
    descriptor: Descriptor,
    lengths: RangeInclusive<usize>,
    held: &mut BufferSet<HELD>,
    model: &mut [bool; HELD],
) {
    let buffer = expected_buffer(region.buffers(), descriptor.io_or_offset);
    assert_eq!(
        region.buffer(descriptor),
        buffer,
        "DataRegion::buffer, {descriptor:?} in {region:?}"
    );
    let len = usize::from(descriptor.len);
    let fits = lengths.contains(&len) && len <= BUFFER_SIZE;
    let frame = region.frame(descriptor, lengths.clone());
    match (buffer, frame) {
        (Ok(buffer), Ok(frame)) => {
            assert!(fits, "{frame:?} accepted outside {lengths:?}");
            assert_eq!((frame.buffer, frame.len), (buffer, len));
            assert_eq!(
                Ok(frame.offset),
                usize::try_from(descriptor.io_or_offset),
                "{frame:?}"
            );
            let end = frame.offset.checked_add(frame.len);
            assert!(
                end.is_some_and(|end| end <= region.len()),
                "{frame:?} runs past the region of {} bytes",
                region.len()
            );
            let fresh = model.get(frame.buffer).is_some_and(|held| !*held);
            assert_eq!(
                held.insert(frame.buffer),
                fresh,
                "BufferSet took {} again",
                frame.buffer
            );
            if let Some(held) = model.get_mut(frame.buffer) {
                *held = true;
            }
        }
        // Its buffer is fine, so only its length may be refused.
        (Ok(_), Err(error)) => assert!(
            !fits && matches!(error, DescriptorError::BadLength { len } if len == descriptor.len),
            "{descriptor:?} refused: {error:?}"
        ),
        (Err(error), frame) => assert_eq!(frame, Err(error), "{descriptor:?}"),
    }
}

/// The lengths a frame is checked against: a driver's and its client's,
/// any length at all, or any range.
fn lengths(input: &Input<'_>) -> RangeInclusive<usize> {
    match input.byte() % 4 {
        0 => 0..=usize::MAX,
        1 => usize::from(input.u16())..=usize::from(input.u16()),
        _ => ETHERNET_HEADER_LEN..=MAX_FRAME_LEN,
    }
}

/// Drives a consumer against the hostile producer.
fn consumer(input: &Input<'_>, queue: &HostileQueue<'_>, region: Option<DataRegion>) {
    let Ok(mut consumer) = Consumer::new(queue) else {
        assert!(
            !valid(queue.capacity),
            "a queue of {} refused",
            queue.capacity
        );
        return;
    };
    assert!(valid(queue.capacity), "a queue of {} taken", queue.capacity);
    let capacity = queue.capacity;
    // The consumer's own index, as it must keep it.
    let mut head = 0u16;
    let mut held = BufferSet::<HELD>::none();
    let mut model = [false; HELD];
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        queue.take_done();
        match input.byte() % 6 {
            0..=2 => {
                let result = consumer.dequeue();
                assert!(
                    matches!(result, Ok(_) | Err(QueueError::CorruptIndex { .. })),
                    "dequeue: {result:?}"
                );
                let done = queue.take_done();
                assert_eq!(done.tail_loads, 1, "the producer's index loaded {done:?}");
                let tail = queue.loaded_tail.get();
                let in_flight = tail.wrapping_sub(head);
                match result {
                    Ok(Some(descriptor)) => {
                        let (slot, bytes, read_at) = queue.read.get();
                        let (stored, stored_at) = queue.stored_head.get();
                        assert!(
                            (1..=capacity).contains(&in_flight),
                            "a descriptor taken with {in_flight} in flight"
                        );
                        assert_eq!(
                            done,
                            Done {
                                tail_loads: 1,
                                reads: 1,
                                head_stores: 1,
                                ..Done::default()
                            }
                        );
                        assert_eq!(slot, slot_of(head, capacity), "the slot read");
                        assert_eq!(stored, head.wrapping_add(1), "the index published");
                        assert!(read_at < stored_at, "the slot freed before it was read");
                        assert_eq!(descriptor, Descriptor::from_bytes(&bytes), "not its copy");
                        head = stored;
                        if let Some(region) = region {
                            check_descriptor(
                                region,
                                descriptor,
                                lengths(input),
                                &mut held,
                                &mut model,
                            );
                        }
                    }
                    Ok(None) => {
                        assert_eq!(in_flight, 0, "nothing taken with {in_flight} in flight");
                        assert_eq!(
                            done,
                            Done {
                                tail_loads: 1,
                                ..Done::default()
                            }
                        );
                    }
                    Err(QueueError::CorruptIndex {
                        tail: told,
                        head: own,
                    }) => {
                        assert!(in_flight > capacity, "{in_flight} in flight refused");
                        assert_eq!((told, own), (tail, head));
                        assert_eq!(
                            done,
                            Done {
                                tail_loads: 1,
                                ..Done::default()
                            }
                        );
                    }
                    Err(_) => {}
                }
            }
            3 => {
                consumer.request_signal();
                let done = queue.take_done();
                assert_eq!(
                    done,
                    Done {
                        signal_stores: 1,
                        ..Done::default()
                    }
                );
                assert_eq!(queue.stored_signal.get(), 0);
            }
            // A buffer handed on, as a driver hands one to its device.
            4 => {
                let buffer = usize::from(input.byte()) % HELD;
                let was = model.get(buffer).copied().unwrap_or(false);
                assert_eq!(held.remove(buffer), was);
                if let Some(held) = model.get_mut(buffer) {
                    *held = false;
                }
            }
            // The consumer starts again, as after a restart: from index 0,
            // whatever the memory holds.
            _ => {
                let again = Consumer::new(queue);
                assert!(
                    again.is_ok(),
                    "a queue of {capacity} refused the second time"
                );
                if let Ok(again) = again {
                    consumer = again;
                    head = 0;
                }
                assert_eq!(queue.take_done(), Done::default());
            }
        }
    }
}

/// Drives a producer against the hostile consumer.
fn producer(input: &Input<'_>, queue: &HostileQueue<'_>) {
    let Ok(mut producer) = Producer::new(queue) else {
        assert!(
            !valid(queue.capacity),
            "a queue of {} refused",
            queue.capacity
        );
        return;
    };
    assert!(valid(queue.capacity), "a queue of {} taken", queue.capacity);
    let capacity = queue.capacity;
    let mut tail = 0u16;
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        queue.take_done();
        match input.byte() % 4 {
            0 | 1 => {
                let descriptor = Descriptor::from_bytes(&queue.descriptor_bytes());
                let result = producer.enqueue(descriptor);
                assert!(
                    matches!(
                        result,
                        Ok(()) | Err(QueueError::Full | QueueError::CorruptIndex { .. })
                    ),
                    "enqueue: {result:?}"
                );
                let done = queue.take_done();
                assert_eq!(done.head_loads, 1, "the consumer's index loaded {done:?}");
                let head = queue.loaded_head.get();
                let in_flight = tail.wrapping_sub(head);
                match result {
                    Ok(()) => {
                        let (slot, bytes, written_at) = queue.written.get();
                        let (stored, stored_at) = queue.stored_tail.get();
                        assert!(in_flight < capacity, "written with {in_flight} in flight");
                        assert_eq!(
                            done,
                            Done {
                                head_loads: 1,
                                writes: 1,
                                tail_stores: 1,
                                ..Done::default()
                            }
                        );
                        assert_eq!(slot, slot_of(tail, capacity), "the slot written");
                        assert_eq!(bytes, descriptor.to_bytes(), "the bytes written");
                        assert_eq!(stored, tail.wrapping_add(1), "the index published");
                        assert!(written_at < stored_at, "published before it was written");
                        tail = stored;
                    }
                    Err(QueueError::Full) => {
                        assert_eq!(in_flight, capacity, "full with {in_flight} in flight");
                        assert_eq!(
                            done,
                            Done {
                                head_loads: 1,
                                ..Done::default()
                            }
                        );
                    }
                    Err(QueueError::CorruptIndex {
                        tail: own,
                        head: told,
                    }) => {
                        assert!(in_flight > capacity, "{in_flight} in flight refused");
                        assert_eq!((own, told), (tail, head));
                        assert_eq!(
                            done,
                            Done {
                                head_loads: 1,
                                ..Done::default()
                            }
                        );
                    }
                    Err(_) => {}
                }
            }
            2 => {
                let result = producer.in_flight();
                let done = queue.take_done();
                assert_eq!(
                    done,
                    Done {
                        head_loads: 1,
                        ..Done::default()
                    }
                );
                let in_flight = tail.wrapping_sub(queue.loaded_head.get());
                if in_flight > capacity {
                    assert!(
                        matches!(result, Err(QueueError::CorruptIndex { .. })),
                        "{result:?}"
                    );
                } else {
                    assert_eq!(result, Ok(in_flight));
                }
            }
            _ => {
                let wanted = producer.signal_wanted();
                let done = queue.take_done();
                let zero = queue.loaded_signal.get() == 0;
                assert_eq!(
                    wanted,
                    zero,
                    "notified for flag {}",
                    queue.loaded_signal.get()
                );
                assert_eq!(
                    done,
                    Done {
                        signal_loads: 1,
                        signal_stores: u32::from(wanted),
                        ..Done::default()
                    }
                );
                if wanted {
                    assert_eq!(queue.stored_signal.get(), 1);
                }
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let end = if input.byte().is_multiple_of(2) {
        End::Consumer
    } else {
        End::Producer
    };
    let buffers = match input.byte() % 6 {
        0 => usize::from(input.u16()),
        1 => usize::MAX,
        2 => 128,
        _ => usize::from(input.byte() % 16),
    };
    let region = DataRegion::new(buffers);
    assert_eq!(
        region.map(|region| (region.buffers(), region.len())),
        buffers
            .checked_mul(BUFFER_SIZE)
            .filter(|_| buffers > 0)
            .map(|len| (buffers, len)),
        "a region of {buffers} buffers"
    );
    let queue = HostileQueue::new(&input, end);
    match end {
        End::Consumer => consumer(&input, &queue, region),
        End::Producer => producer(&input, &queue),
    }
});
