// SPDX-License-Identifier: MIT OR Apache-2.0
//! The network path's lowest boundary:
//! the queues between a NIC driver PD and
//! its one client, the network stack PD, and the Ethernet frames that cross
//! them.
//!
//! * [`queue`] - sDDF 0.7.0's network queues, byte for byte: a free and an
//!   active queue in each direction, over [`BUFFER_SIZE`]-byte buffers, with
//!   sDDF's signalling flag. sDDF's trust model is not adopted: each side
//!   keeps its own index privately and checks the other's, and
//!   [`DataRegion`] checks every descriptor before it is used.
//! * [`arp`] - an ARP request, and the parser for what comes back (RFC 826):
//!   the exchange the network test client makes. The network stack
//!   PD speaks ARP through smoltcp (`nowayos-ip`).
//!
//! The views of real Microkit regions need `unsafe` (they turn a mapped
//! address into atomics) and live in the approved-`unsafe` crate
//! `nowayos-region` (`net_queue`); the NIC side, which hands buffers to the
//! device by physical address and never maps them, is `nowayos-hw`'s. This
//! crate has no `unsafe` code.
//!
//! Provisional: the first NIC driver and its test client. The
//! network stack PD's link over these queues is `nowayos-ip`'s, and
//! the socket records between the stack PD and its clients are
//! `nowayos-socket`'s.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod arp;
pub mod queue;

#[cfg(kani)]
mod proofs;

pub use queue::{
    BUFFER_SIZE, BufferSet, Consumer, DESCRIPTOR_LEN, DataRegion, Descriptor, DescriptorError,
    Frame, InMemoryQueue, MAX_CAPACITY, Producer, QUEUE_HEADER_LEN, QueueError, QueueMemory,
    queue_len,
};

/// The bytes of an Ethernet II header: destination, source, EtherType.
pub const ETHERNET_HEADER_LEN: usize = 14;

/// The longest frame either side passes on: an Ethernet header and a
/// 1500-byte payload, without the frame check sequence (no VLAN tag, no
/// jumbo frames).
pub const MAX_FRAME_LEN: usize = 1514;

/// The buffers in each data region of a network test system, and the
/// capacity of each of its four queues: 128 buffers of [`BUFFER_SIZE`]
/// bytes (256 KiB), and queues of 128 descriptors, each in one page. The
/// driver and its client must agree, and so must the system description:
/// `tools/test_tools.py` checks each description's data regions against
/// [`DATA_REGION_LEN`].
pub const BUFFERS: usize = 128;

/// [`BUFFERS`], as the queues count it.
pub const CAPACITY: u16 = 128;

/// The size of each data region: [`BUFFERS`] buffers of [`BUFFER_SIZE`]
/// bytes.
pub const DATA_REGION_LEN: usize = 0x4_0000;

const _: () = {
    assert!(BUFFERS.is_power_of_two());
    assert!(CAPACITY as usize == BUFFERS);
    assert!(CAPACITY <= MAX_CAPACITY);
    assert!(matches!(
        BUFFERS.checked_mul(BUFFER_SIZE),
        Some(DATA_REGION_LEN)
    ));
    assert!(MAX_FRAME_LEN <= BUFFER_SIZE);
};
