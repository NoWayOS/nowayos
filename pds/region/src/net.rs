// SPDX-License-Identifier: MIT OR Apache-2.0
//! sDDF's network queues over Microkit regions: the view
//! `nowayos_net`'s queue ends run over.
//!
//! A network queue is one region that both the NIC driver and its client
//! map read-write: each writes its own index, and both write the flag
//! (`nowayos_net::queue`, "Trust"). [`net_queue`] views such a region as
//! sDDF's `net_queue_t` - every field an atomic, so whatever the peer
//! writes, and whenever, is a valid value - and [`NetQueue`] implements
//! [`QueueMemory`] over it: the indices with acquire and release ordering,
//! the flag sequentially consistent, a descriptor copied byte by byte.
//! [`nowayos_net::Producer`] and [`nowayos_net::Consumer`] keep each side's
//! index privately, and check the peer's.
//!
//! ```no_run
//! use nowayos_net::Consumer;
//! use nowayos_region::{net_queue, read_write};
//!
//! // The driver's end of the receive free queue: nowayos_rw_region_0.
//! let rx_free = read_write(0)
//!     .ok()
//!     .and_then(|region| net_queue::<128>(region).ok())
//!     .and_then(|queue| Consumer::new(queue).ok());
//! # let _ = rx_free;
//! ```

use core::mem::{offset_of, size_of};
use core::sync::atomic::Ordering;

use nowayos_net::queue::{HEAD_OFFSET, SIGNAL_OFFSET, TAIL_OFFSET};
use nowayos_net::{DESCRIPTOR_LEN, QUEUE_HEADER_LEN, QueueMemory, queue_len};

use crate::RegionError;
use crate::mapping::{NetQueueLayout, ReadWrite, Region};

// The view's fields sit where sDDF's `net_queue_t` has them.
const _: () = {
    assert!(offset_of!(NetQueueLayout<128>, tail) == TAIL_OFFSET);
    assert!(offset_of!(NetQueueLayout<128>, head) == HEAD_OFFSET);
    assert!(offset_of!(NetQueueLayout<128>, signal) == SIGNAL_OFFSET);
    assert!(offset_of!(NetQueueLayout<128>, descriptors) == QUEUE_HEADER_LEN);
    assert!(matches!(queue_len(128), Some(len) if len == size_of::<NetQueueLayout<128>>()));
    assert!(matches!(queue_len(1), Some(len) if len == size_of::<NetQueueLayout<1>>()));
};

/// One network queue of `CAPACITY` descriptors over a read-write region:
/// give it to a [`nowayos_net::Producer`] or a [`nowayos_net::Consumer`].
pub struct NetQueue<const CAPACITY: usize> {
    layout: &'static NetQueueLayout<CAPACITY>,
}

/// Views read-write `region` as a network queue of `CAPACITY` descriptors.
///
/// # Errors
///
/// Any error of viewing the region: too small for the queue, misaligned.
pub fn net_queue<const CAPACITY: usize>(
    region: Region<ReadWrite>,
) -> Result<NetQueue<CAPACITY>, RegionError> {
    Ok(NetQueue {
        layout: region.view()?,
    })
}

impl<const CAPACITY: usize> QueueMemory for NetQueue<CAPACITY> {
    fn capacity(&self) -> u16 {
        // A capacity that does not fit is reported as 0: the queue ends
        // refuse it.
        u16::try_from(CAPACITY).unwrap_or(0)
    }

    fn load_tail(&self) -> u16 {
        self.layout.tail.load(Ordering::Acquire)
    }

    fn load_head(&self) -> u16 {
        self.layout.head.load(Ordering::Acquire)
    }

    fn store_tail(&self, tail: u16) {
        self.layout.tail.store(tail, Ordering::Release);
    }

    fn store_head(&self, head: u16) {
        self.layout.head.store(head, Ordering::Release);
    }

    fn load_signal(&self) -> u32 {
        self.layout.signal.load(Ordering::SeqCst)
    }

    fn store_signal(&self, value: u32) {
        self.layout.signal.store(value, Ordering::SeqCst);
    }

    fn read_descriptor(&self, slot: u16, out: &mut [u8; DESCRIPTOR_LEN]) {
        if let Some(cells) = self.layout.descriptors.get(usize::from(slot)) {
            for (byte, cell) in out.iter_mut().zip(cells) {
                *byte = cell.load(Ordering::Relaxed);
            }
        }
    }

    fn write_descriptor(&self, slot: u16, bytes: &[u8; DESCRIPTOR_LEN]) {
        if let Some(cells) = self.layout.descriptors.get(usize::from(slot)) {
            for (cell, byte) in cells.iter().zip(bytes) {
                cell.store(*byte, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! Test pages stand in for the region, viewed twice through the same
    //! layout - once per side, as the driver and its client map it. These
    //! run under Miri too.

    use nowayos_net::{Consumer, Descriptor, Producer, QueueError};

    use super::*;
    use crate::mapping::testing::TestPage;

    #[test]
    fn a_queue_is_laid_out_byte_for_byte_as_sddfs() {
        let page = TestPage::fresh();
        let mut producer =
            Producer::new(net_queue::<128>(Region::test_page(page)).unwrap()).unwrap();
        producer
            .enqueue(Descriptor::new(0x0807_0605_0403_0201, 0x0a09))
            .unwrap();
        assert_eq!([0, 1].map(|i| page.byte(i)), [1, 0], "tail at 0");
        assert_eq!([2, 3].map(|i| page.byte(i)), [0, 0], "head at 2");
        assert!(producer.signal_wanted());
        assert_eq!(
            [4, 5, 6, 7].map(|i| page.byte(i)),
            [1, 0, 0, 0],
            "flag at 4"
        );
        let descriptor: [u8; 16] = core::array::from_fn(|i| page.byte(8 + i));
        assert_eq!(
            descriptor,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn the_two_sides_exchange_descriptors_through_one_region() {
        let page = TestPage::fresh();
        let mut producer =
            Producer::new(net_queue::<128>(Region::test_page(page)).unwrap()).unwrap();
        let mut consumer =
            Consumer::new(net_queue::<128>(Region::test_page(page)).unwrap()).unwrap();
        for i in 0..128 {
            producer.enqueue(Descriptor::new(i * 2048, 0)).unwrap();
        }
        assert_eq!(
            producer.enqueue(Descriptor::default()),
            Err(QueueError::Full)
        );
        for i in 0..128 {
            assert_eq!(consumer.dequeue(), Ok(Some(Descriptor::new(i * 2048, 0))));
        }
        assert_eq!(consumer.dequeue(), Ok(None));
        assert_eq!([2, 3].map(|i| page.byte(i)), [128, 0], "head at 2");
    }

    #[test]
    fn a_region_too_small_for_the_queue_is_refused() {
        let page = TestPage::fresh();
        assert_eq!(
            net_queue::<256>(Region::test_page(page)).err(),
            Some(RegionError::TooSmall {
                size: 4096,
                needed: 8 + 16 * 256
            })
        );
    }
}
