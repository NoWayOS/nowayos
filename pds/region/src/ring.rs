// SPDX-License-Identifier: MIT OR Apache-2.0
//! The two ends of a `nowayos-ipc` ring over two Microkit memory regions.
//!
//! [`nowayos_ipc::Producer`] and [`nowayos_ipc::Consumer`] implement the ring
//! protocol over a side's view of the shared memory ([`ProducerMemory`],
//! [`ConsumerMemory`]). This module provides those views for real Microkit
//! regions, split so that each side writes only what it owns:
//!
//! * the **messages** region holds the producer index and the slots. It is
//!   mapped read-write into the producer ([`RingTx`]) and read-only into the
//!   consumer ([`RingRx`]);
//! * the **control** region holds the consumer index. It is mapped read-write
//!   into the consumer and read-only into the producer.
//!
//! Layout of the messages region (native byte order, AArch64 little-endian):
//! the `u32` producer index at offset 0, then `SLOTS` slots of `SLOT` bytes
//! from offset 4, then the producer's `u32` restart epoch at the next
//! multiple of 4. Layout of the control region: the `u32` consumer index at
//! offset 0, then the consumer's `u32` restart epoch at offset 4 (each side's
//! epoch: `nowayos_ipc`'s ring documentation, "Restart epochs"). Both sides
//! must use the same `SLOTS` and `SLOT`.
//!
//! # Memory ordering
//!
//! As `nowayos_ipc`'s ring documentation requires: the index a side owns is
//! stored with release ordering, and the other side's index is loaded with
//! acquire semantics before any slot is touched. Slot bytes are relaxed
//! atomics. The peer's index lives in memory mapped read-only here, and Rust
//! only guarantees *relaxed* atomic loads on read-only memory, so the acquire
//! is a relaxed load followed by an acquire fence — the same synchronisation.
//!
//! # Each end is one side only
//!
//! [`RingTx`] implements only [`ProducerMemory`] and [`RingRx`] only
//! [`ConsumerMemory`], so the wrong end of a ring cannot be attached — a
//! consumer over the producer's view does not compile:
//!
//! ```compile_fail,E0277
//! fn wrong_side(tx: &nowayos_region::RingTx<4, 32>) {
//!     let _ = nowayos_ipc::Consumer::new(tx);
//! }
//! ```
//!
//! ```compile_fail,E0277
//! fn wrong_side(rx: &nowayos_region::RingRx<4, 32>) {
//!     let _ = nowayos_ipc::Producer::new(rx);
//! }
//! ```
//!
//! while the right ones do:
//!
//! ```
//! fn right_sides(tx: &nowayos_region::RingTx<4, 32>, rx: &nowayos_region::RingRx<4, 32>) {
//!     let _ = nowayos_ipc::Producer::new(tx);
//!     let _ = nowayos_ipc::Consumer::new(rx);
//! }
//! ```

use core::sync::atomic::{AtomicU32, Ordering, fence};

use nowayos_ipc::{ConsumerMemory, ProducerMemory, RingMemory};

use crate::RegionError;
use crate::mapping::{ControlLayout, MessageLayout, ReadOnly, ReadWrite, Region};

/// The producer's end: the messages region read-write, the control region
/// read-only. Use it with [`nowayos_ipc::Producer`]; it is not a
/// [`ConsumerMemory`].
pub struct RingTx<const SLOTS: usize, const SLOT: usize> {
    messages: &'static MessageLayout<SLOTS, SLOT>,
    control: &'static ControlLayout,
}

/// The consumer's end: the control region read-write, the messages region
/// read-only. Use it with [`nowayos_ipc::Consumer`]; it is not a
/// [`ProducerMemory`].
pub struct RingRx<const SLOTS: usize, const SLOT: usize> {
    control: &'static ControlLayout,
    messages: &'static MessageLayout<SLOTS, SLOT>,
}

/// The producer's end of a ring of `SLOTS` slots of `SLOT` bytes.
///
/// # Errors
///
/// [`RegionError::Overlap`] if the regions overlap; otherwise any error from
/// viewing a region (too small, misaligned).
pub fn ring_producer<const SLOTS: usize, const SLOT: usize>(
    messages: Region<ReadWrite>,
    control: Region<ReadOnly>,
) -> Result<RingTx<SLOTS, SLOT>, RegionError> {
    check_disjoint(
        messages.vaddr(),
        messages.size(),
        control.vaddr(),
        control.size(),
    )?;
    Ok(RingTx {
        messages: messages.view()?,
        control: control.view()?,
    })
}

/// The consumer's end of a ring of `SLOTS` slots of `SLOT` bytes.
///
/// # Errors
///
/// As for [`ring_producer`].
pub fn ring_consumer<const SLOTS: usize, const SLOT: usize>(
    control: Region<ReadWrite>,
    messages: Region<ReadOnly>,
) -> Result<RingRx<SLOTS, SLOT>, RegionError> {
    check_disjoint(
        control.vaddr(),
        control.size(),
        messages.vaddr(),
        messages.size(),
    )?;
    Ok(RingRx {
        control: control.view()?,
        messages: messages.view()?,
    })
}

/// Fails unless `[a, a + a_len)` and `[b, b + b_len)` are disjoint.
fn check_disjoint(a: usize, a_len: usize, b: usize, b_len: usize) -> Result<(), RegionError> {
    // In u128, where the ends cannot overflow: saturating at usize::MAX
    // would drop the last byte of the address space from a range that runs
    // off the end, and could hide an overlap there.
    let (a, b) = (a as u128, b as u128);
    let a_end = a.saturating_add(a_len as u128);
    let b_end = b.saturating_add(b_len as u128);
    if a_end <= b || b_end <= a {
        Ok(())
    } else {
        Err(RegionError::Overlap)
    }
}

/// An acquire load of a `u32` that may be mapped read-only in this PD.
///
/// Rust guarantees relaxed atomic loads of this size on read-only memory,
/// but no other orderings. A relaxed load followed by an acquire fence
/// synchronises with the release store it reads from, exactly as an acquire
/// load would.
pub(crate) fn load_acquire_shared(cell: &AtomicU32) -> u32 {
    let value = cell.load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    value
}

fn capacity_of(slots: usize) -> u32 {
    // A capacity that does not fit in u32 is reported as 0: invalid, and
    // rejected by nowayos_ipc.
    u32::try_from(slots).unwrap_or(0)
}

impl<const SLOTS: usize, const SLOT: usize> RingMemory<SLOT> for RingTx<SLOTS, SLOT> {
    fn capacity(&self) -> u32 {
        capacity_of(SLOTS)
    }

    fn load_producer_index(&self) -> u32 {
        // Our own index, in memory only we can write.
        self.messages.producer_index.load(Ordering::Acquire)
    }

    fn load_consumer_index(&self) -> u32 {
        load_acquire_shared(&self.control.consumer_index)
    }

    fn load_producer_epoch(&self) -> u32 {
        // Our own epoch, in memory only we can write.
        self.messages.producer_epoch.load(Ordering::Acquire)
    }

    fn load_consumer_epoch(&self) -> u32 {
        load_acquire_shared(&self.control.consumer_epoch)
    }
}

impl<const SLOTS: usize, const SLOT: usize> ProducerMemory<SLOT> for RingTx<SLOTS, SLOT> {
    fn store_producer_index(&self, index: u32) {
        self.messages.producer_index.store(index, Ordering::Release);
    }

    fn store_producer_epoch(&self, epoch: u32) {
        self.messages.producer_epoch.store(epoch, Ordering::Release);
    }

    fn write_slot(&self, slot: u32, record: &[u8; SLOT]) {
        let cells = usize::try_from(slot)
            .ok()
            .and_then(|slot| self.messages.slots.get(slot));
        if let Some(cells) = cells {
            for (cell, byte) in cells.iter().zip(record) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }
    }
}

impl<const SLOTS: usize, const SLOT: usize> RingMemory<SLOT> for RingRx<SLOTS, SLOT> {
    fn capacity(&self) -> u32 {
        capacity_of(SLOTS)
    }

    fn load_producer_index(&self) -> u32 {
        load_acquire_shared(&self.messages.producer_index)
    }

    fn load_consumer_index(&self) -> u32 {
        // Our own index, in memory only we can write.
        self.control.consumer_index.load(Ordering::Acquire)
    }

    fn load_producer_epoch(&self) -> u32 {
        load_acquire_shared(&self.messages.producer_epoch)
    }

    fn load_consumer_epoch(&self) -> u32 {
        // Our own epoch, in memory only we can write.
        self.control.consumer_epoch.load(Ordering::Acquire)
    }
}

impl<const SLOTS: usize, const SLOT: usize> ConsumerMemory<SLOT> for RingRx<SLOTS, SLOT> {
    fn store_consumer_index(&self, index: u32) {
        self.control.consumer_index.store(index, Ordering::Release);
    }

    fn store_consumer_epoch(&self, epoch: u32) {
        self.control.consumer_epoch.store(epoch, Ordering::Release);
    }

    fn read_slot(&self, slot: u32, out: &mut [u8; SLOT]) {
        let cells = usize::try_from(slot)
            .ok()
            .and_then(|slot| self.messages.slots.get(slot));
        if let Some(cells) = cells {
            for (byte, cell) in out.iter_mut().zip(cells) {
                *byte = cell.load(Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::thread;
    use std::vec::Vec;

    use nowayos_ipc::{
        Consumer, DecodeError, EncodeError, Message, Producer, Reader, RecvError, RingError,
        SendError, Writer,
    };

    use super::*;
    use crate::mapping::testing::TestPage;

    #[derive(Debug, PartialEq, Eq)]
    struct Seq(u32);

    impl Message for Seq {
        const KIND: u16 = 0x0042;
        const VERSION: u16 = 1;
        const PAYLOAD_LEN: usize = 4;

        fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
            out.write_u32(self.0)
        }

        fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
            input.read_u32().map(Self)
        }
    }

    /// Both ends of a 4-slot ring over two fresh test pages, and the pages.
    fn ring() -> (
        RingTx<4, 16>,
        RingRx<4, 16>,
        &'static TestPage,
        &'static TestPage,
    ) {
        let messages = TestPage::fresh();
        let control = TestPage::fresh();
        let tx = ring_producer(Region::test_page(messages), Region::test_page(control)).unwrap();
        let rx = ring_consumer(Region::test_page(control), Region::test_page(messages)).unwrap();
        (tx, rx, messages, control)
    }

    #[test]
    fn messages_cross_in_order_and_wrap_the_ring() {
        let (tx_mem, rx_mem, ..) = ring();
        let mut tx = Producer::new(&tx_mem).unwrap();
        let mut rx = Consumer::new(&rx_mem).unwrap();
        // 10 laps of the 4-slot ring, filling it completely each time.
        for lap in 0..10u32 {
            for i in 0..4 {
                tx.try_send(&Seq(lap * 4 + i)).unwrap();
            }
            assert_eq!(tx.try_send(&Seq(999)), Err(SendError::Full));
            for i in 0..4 {
                assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(lap * 4 + i))));
            }
            assert_eq!(rx.try_recv::<Seq>(), Ok(None));
        }
    }

    #[test]
    fn the_layout_is_as_documented() {
        let (tx_mem, rx_mem, messages, control) = ring();
        let mut tx = Producer::new(&tx_mem).unwrap();
        tx.try_send(&Seq(0x0A0B_0C0D)).unwrap();
        // Producer index 1 at offset 0 of the messages region.
        assert_eq!([0, 1, 2, 3].map(|i| messages.byte(i)), [1, 0, 0, 0]);
        // Slot 0 from offset 4: header (kind 0x42, version 1, length 4,
        // reserved 0), then the payload.
        let slot: Vec<u8> = (4..16).map(|i| messages.byte(i)).collect();
        assert_eq!(slot, [0x42, 0, 1, 0, 4, 0, 0, 0, 0x0D, 0x0C, 0x0B, 0x0A]);
        let mut rx = Consumer::new(&rx_mem).unwrap();
        assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(0x0A0B_0C0D))));
        // Consumer index 1 at offset 0 of the control region.
        assert_eq!([0, 1, 2, 3].map(|i| control.byte(i)), [1, 0, 0, 0]);
        // The restart epochs: the producer's after the 4 slots of 16 bytes
        // (offset 4 + 64), the consumer's at offset 4 of the control region.
        tx.begin_epoch();
        tx.begin_epoch();
        rx.begin_epoch();
        assert_eq!([68, 69, 70, 71].map(|i| messages.byte(i)), [2, 0, 0, 0]);
        assert_eq!([4, 5, 6, 7].map(|i| control.byte(i)), [1, 0, 0, 0]);
        assert_eq!((tx.peer_epoch(), rx.peer_epoch()), (1, 2));
    }

    #[test]
    fn each_side_writes_only_its_own_region() {
        // The other side's operations do not exist on a view (see the
        // compile_fail examples in the module documentation); what each
        // side does write lands only in the region it owns.
        let (tx_mem, rx_mem, messages, control) = ring();
        let mut tx = Producer::new(&tx_mem).unwrap();
        tx.try_send(&Seq(1)).unwrap();
        assert!(
            control.snapshot().iter().all(|&byte| byte == 0),
            "producer wrote control"
        );
        let before = messages.snapshot();
        let mut rx = Consumer::new(&rx_mem).unwrap();
        assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(1))));
        let after = messages.snapshot();
        assert_eq!(before, after, "consumer wrote messages");
        assert_eq!(control.byte(0), 1);
    }

    #[test]
    fn each_side_trusts_its_own_index_and_checks_the_other() {
        let (tx_mem, rx_mem, ..) = ring();
        let mut tx = Producer::new(&tx_mem).unwrap();
        tx.try_send(&Seq(1)).unwrap();
        // A hostile consumer claims to have taken records never sent: the
        // producer notices and publishes nothing.
        let hostile = RingRx::<4, 16> {
            control: rx_mem.control,
            messages: rx_mem.messages,
        };
        hostile.store_consumer_index(6);
        assert_eq!(
            tx.try_send(&Seq(2)),
            Err(SendError::Ring(RingError::CorruptIndices {
                producer: 1,
                consumer: 6
            }))
        );
        assert_eq!(tx_mem.load_producer_index(), 1);
        // A consumer attaching now sees the corrupt state too.
        assert!(Consumer::new(&rx_mem).is_err());
    }

    #[test]
    fn a_malformed_record_is_rejected_and_consumed() {
        let (tx_mem, rx_mem, ..) = ring();
        let mut tx = Producer::new(&tx_mem).unwrap();
        let mut rx = Consumer::new(&rx_mem).unwrap();
        tx.try_send_bytes(&[0x42, 0, 1, 0, 4, 0, 1, 0]).unwrap(); // reserved != 0
        tx.try_send(&Seq(5)).unwrap();
        assert_eq!(
            rx.try_recv::<Seq>(),
            Err(RecvError::Decode(DecodeError::ReservedNotZero))
        );
        assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(5))));
    }

    #[test]
    fn restarted_sides_resume_from_their_own_index() {
        let (tx_mem, rx_mem, ..) = ring();
        {
            let mut tx = Producer::new(&tx_mem).unwrap();
            for i in 0..3 {
                tx.try_send(&Seq(i)).unwrap();
            }
        }
        {
            let mut rx = Consumer::new(&rx_mem).unwrap();
            assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(0))));
        }
        // New Producer and Consumer values, as the PDs create per
        // notification: they carry on where the old ones stopped.
        let mut tx = Producer::new(&tx_mem).unwrap();
        let mut rx = Consumer::new(&rx_mem).unwrap();
        assert_eq!(tx.free_slots(), Ok(2));
        tx.try_send(&Seq(3)).unwrap();
        for i in 1..4 {
            assert_eq!(rx.try_recv::<Seq>(), Ok(Some(Seq(i))));
        }
    }

    #[test]
    fn overlapping_regions_are_refused() {
        let page = TestPage::fresh();
        let messages = Region::in_test_page(page, 0, 2048).unwrap();
        let control = Region::in_test_page(page, 1024, 64).unwrap();
        assert_eq!(
            ring_producer::<4, 16>(messages, control).err(),
            Some(RegionError::Overlap)
        );
        // Adjacent is fine.
        let messages = Region::in_test_page(page, 0, 2048).unwrap();
        let control = Region::in_test_page(page, 2048, 64).unwrap();
        assert!(ring_producer::<4, 16>(messages, control).is_ok());
    }

    #[test]
    fn a_region_too_small_for_the_ring_is_refused() {
        let messages = Region::in_test_page(TestPage::fresh(), 0, 68).unwrap();
        let control = Region::test_page(TestPage::fresh());
        // 4 + 4 * 16 + 4 = 72 bytes needed: the index, the slots, the epoch.
        assert_eq!(
            ring_producer::<4, 16>(messages, control).err(),
            Some(RegionError::TooSmall {
                size: 68,
                needed: 72
            })
        );
    }

    #[test]
    fn disjointness_boundaries() {
        assert!(check_disjoint(0, 10, 10, 10).is_ok());
        assert!(check_disjoint(10, 10, 0, 10).is_ok());
        assert!(check_disjoint(0, 11, 10, 10).is_err());
        assert!(check_disjoint(5, 1, 0, 10).is_err());
        assert!(check_disjoint(usize::MAX - 1, 10, 0, 10).is_ok());
        assert!(check_disjoint(usize::MAX - 1, 10, usize::MAX, 1).is_err());
    }

    #[test]
    fn two_threads_exchange_messages_in_order() {
        // The PDs run on one core today, but the ring must also be right on
        // a weakly ordered multi-core Arm (and under Miri's memory model).
        let count: u32 = if cfg!(miri) { 200 } else { 20_000 };
        let (tx_mem, rx_mem, ..) = ring();
        let producer = thread::spawn(move || {
            let mut tx = Producer::new(&tx_mem).unwrap();
            let mut next = 0;
            while next < count {
                match tx.try_send(&Seq(next)) {
                    Ok(()) => next += 1,
                    Err(SendError::Full) => thread::yield_now(),
                    Err(err) => panic!("send failed: {err}"),
                }
            }
        });
        let mut rx = Consumer::new(&rx_mem).unwrap();
        let mut expected = 0;
        while expected < count {
            match rx.try_recv::<Seq>() {
                Ok(Some(Seq(value))) => {
                    assert_eq!(value, expected);
                    expected += 1;
                }
                Ok(None) => thread::yield_now(),
                Err(err) => panic!("receive failed: {err}"),
            }
        }
        producer.join().unwrap();
    }
}
