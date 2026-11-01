// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stack takes a DHCP lease through its link from a test DHCP server
//! standing where QEMU's would be, and survives arbitrary frames and
//! descriptors from the driver's side.
//!
//! The server's frames are built by hand, byte by byte (RFC 791, 768, 2131
//! and 2132), not with smoltcp's own `wire` module, so the stack is not
//! tested against itself.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::num::NonZeroU64;
use std::sync::atomic::AtomicU8;

use nowayos_ip::{Event, Lease, Link, Queues, RxMemory, Stack, StackSeed, TxMemory, storage};
use nowayos_net::arp::{Ipv4, Mac};
use nowayos_net::{BUFFER_SIZE, Consumer, Descriptor, InMemoryQueue, MAX_FRAME_LEN, Producer};
use nowayos_time::Instant;
use proptest::prelude::*;

const N: usize = 8;
type Queue = InMemoryQueue<N>;

const OUR_MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
const SERVER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
const SERVER: [u8; 4] = [10, 0, 2, 2];
const OFFERED: [u8; 4] = [10, 0, 2, 15];
const MS: u64 = 1_000_000;
const SECOND: u64 = 1_000 * MS;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;

/// Both sides' queues and buffers.
struct Shared {
    rx_free: Queue,
    rx_active: Queue,
    tx_free: Queue,
    tx_active: Queue,
    rx: Vec<AtomicU8>,
    tx: Vec<AtomicU8>,
}

impl Shared {
    fn new() -> Self {
        Self {
            rx_free: Queue::new(),
            rx_active: Queue::new(),
            tx_free: Queue::new(),
            tx_active: Queue::new(),
            rx: (0..N * BUFFER_SIZE).map(|_| AtomicU8::new(0)).collect(),
            tx: (0..N * BUFFER_SIZE).map(|_| AtomicU8::new(0)).collect(),
        }
    }

    fn link(&self) -> Link<&Queue, &[AtomicU8], &[AtomicU8], N> {
        Link::new(
            Queues {
                rx_free: &self.rx_free,
                rx_active: &self.rx_active,
                tx_free: &self.tx_free,
                tx_active: &self.tx_active,
            },
            &self.rx[..],
            &self.tx[..],
        )
        .unwrap()
    }
}

/// The driver's side: it passes frames in and takes frames out.
struct Driver<'a> {
    rx_free: Consumer<&'a Queue>,
    rx_active: Producer<&'a Queue>,
    tx_free: Producer<&'a Queue>,
    tx_active: Consumer<&'a Queue>,
    shared: &'a Shared,
}

impl<'a> Driver<'a> {
    fn new(shared: &'a Shared) -> Self {
        Self {
            rx_free: Consumer::new(&shared.rx_free).unwrap(),
            rx_active: Producer::new(&shared.rx_active).unwrap(),
            tx_free: Producer::new(&shared.tx_free).unwrap(),
            tx_active: Consumer::new(&shared.tx_active).unwrap(),
            shared,
        }
    }

    /// Passes `frame` to the stack in an empty buffer it gave the driver.
    fn deliver(&mut self, frame: &[u8]) {
        let offset = self.rx_free.dequeue().unwrap().unwrap().io_or_offset;
        let mut memory = &self.shared.rx[..];
        assert!(memory.store(offset as usize, frame));
        self.rx_active
            .enqueue(Descriptor::new(offset, frame.len() as u16))
            .unwrap();
    }

    /// Every frame the stack has sent; their buffers given back.
    fn sent(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while let Some(descriptor) = self.tx_active.dequeue().unwrap() {
            let mut frame = vec![0u8; usize::from(descriptor.len)];
            assert!(
                self.shared.tx[..].load(descriptor.io_or_offset as usize, &mut frame),
                "a descriptor inside the transmit buffers"
            );
            self.tx_free
                .enqueue(Descriptor::new(descriptor.io_or_offset, 0))
                .unwrap();
            frames.push(frame);
        }
        frames
    }
}

/// The ones' complement sum of `bytes` (RFC 1071), as an IPv4 header holds it.
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
        sum += u32::from(word);
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A DHCP message of `kind` from the server to the client `xid` names,
/// offering `yiaddr`.
fn reply(kind: u8, xid: [u8; 4], yiaddr: [u8; 4]) -> Vec<u8> {
    let mut bootp = vec![0u8; 240];
    bootp[0] = 2; // BOOTREPLY
    bootp[1] = 1; // Ethernet
    bootp[2] = 6;
    bootp[4..8].copy_from_slice(&xid);
    bootp[16..20].copy_from_slice(&yiaddr);
    bootp[20..24].copy_from_slice(&SERVER);
    bootp[28..34].copy_from_slice(&OUR_MAC.0);
    bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
    let options: &[&[u8]] = &[
        &[53, 1, kind],
        &[54, 4, SERVER[0], SERVER[1], SERVER[2], SERVER[3]],
        &[51, 4, 0, 1, 0x51, 0x80], // 86,400 s
        &[1, 4, 255, 255, 255, 0],
        &[3, 4, SERVER[0], SERVER[1], SERVER[2], SERVER[3]],
        &[6, 4, 10, 0, 2, 3],
        &[255],
    ];
    for option in options {
        bootp.extend_from_slice(option);
    }
    udp_frame(&bootp)
}

/// `payload` from the server's port 67 to the client's port 68, broadcast,
/// in IPv4 (no UDP checksum, which IPv4 allows) in Ethernet.
fn udp_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&OUR_MAC.0);
    frame.extend_from_slice(&SERVER_MAC);
    frame.extend_from_slice(&[0x08, 0x00]);
    let total = (20 + 8 + payload.len()) as u16;
    let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, 17, 0, 0];
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    ip.extend_from_slice(&SERVER);
    ip.extend_from_slice(&[255, 255, 255, 255]);
    let sum = checksum(&ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&67u16.to_be_bytes());
    frame.extend_from_slice(&68u16.to_be_bytes());
    frame.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(payload);
    frame
}

/// A DHCP message the stack sent: its type, its transaction id, and the
/// address it asks for, if any.
fn dhcp_sent(frame: &[u8]) -> Option<(u8, [u8; 4], Option<[u8; 4]>)> {
    if frame.get(12..14)? != [0x08, 0x00] || *frame.get(23)? != 17 {
        return None;
    }
    let ihl = usize::from(frame.get(14)? & 0x0f) * 4;
    let udp = frame.get(14 + ihl..)?;
    if udp.get(2..4)? != 67u16.to_be_bytes() {
        return None;
    }
    let bootp = udp.get(8..)?;
    let xid: [u8; 4] = bootp.get(4..8)?.try_into().ok()?;
    let mut options = bootp.get(240..)?;
    let (mut kind, mut requested) = (None, None);
    while let [code, rest @ ..] = options {
        match code {
            255 => break,
            0 => options = rest,
            _ => {
                let (&len, rest) = rest.split_first()?;
                let value = rest.get(..usize::from(len))?;
                match code {
                    53 => kind = value.first().copied(),
                    50 => requested = value.try_into().ok(),
                    _ => {}
                }
                options = &rest[usize::from(len)..];
            }
        }
    }
    Some((kind?, xid, requested))
}

fn seed() -> StackSeed {
    StackSeed::from_non_zero(NonZeroU64::new(0x5eed_5eed_5eed_5eed).unwrap())
}

fn at(nanos: u64) -> Instant {
    Instant::from_nanos(nanos)
}

#[test]
fn the_stack_takes_a_lease_from_a_dhcp_server() {
    let shared = Shared::new();
    let mut link = shared.link();
    let mut room = storage();
    let mut stack = Stack::new(OUR_MAC, seed(), &mut link, &mut room, at(0)).unwrap();
    link.start();
    let mut driver = Driver::new(&shared);
    assert_eq!(stack.lease(), None);

    // DISCOVER, broadcast.
    assert_eq!(stack.poll(&mut link, at(MS)), None);
    let sent = driver.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(&sent[0][..6], &[0xff; 6], "a broadcast");
    let (kind, xid, _) = dhcp_sent(&sent[0]).unwrap();
    assert_eq!(kind, DISCOVER);

    // OFFER: the stack asks for the address offered.
    driver.deliver(&reply(OFFER, xid, OFFERED));
    assert_eq!(stack.poll(&mut link, at(2 * MS)), None);
    let sent = driver.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(dhcp_sent(&sent[0]), Some((REQUEST, xid, Some(OFFERED))));

    // ACK: the lease.
    driver.deliver(&reply(ACK, xid, OFFERED));
    let lease = Lease {
        address: Ipv4(OFFERED),
        prefix_len: 24,
        gateway: Some(Ipv4(SERVER)),
    };
    assert_eq!(
        stack.poll(&mut link, at(3 * MS)),
        Some(Event::Leased(lease))
    );
    assert_eq!(stack.lease(), Some(lease));
    assert_eq!(lease.to_string(), "10.0.2.15/24, gateway 10.0.2.2");
    // Nothing is due until the lease is to be renewed, half-way through.
    let delay = stack.poll_delay(at(3 * MS)).unwrap();
    assert!(delay >= 43_000 * SECOND, "{delay} ns");
    let counts = link.counts();
    assert_eq!((counts.received, counts.sent), (2, 2));
    assert_eq!(counts.dropped(), Default::default());
}

#[test]
fn a_lease_never_renewed_is_lost() {
    let shared = Shared::new();
    let mut link = shared.link();
    let mut room = storage();
    let mut stack = Stack::new(OUR_MAC, seed(), &mut link, &mut room, at(0)).unwrap();
    link.start();
    let mut driver = Driver::new(&shared);
    stack.poll(&mut link, at(MS));
    let (_, xid, _) = dhcp_sent(&driver.sent()[0]).unwrap();
    driver.deliver(&reply(OFFER, xid, OFFERED));
    stack.poll(&mut link, at(2 * MS));
    driver.sent();
    driver.deliver(&reply(ACK, xid, OFFERED));
    assert!(matches!(
        stack.poll(&mut link, at(3 * MS)),
        Some(Event::Leased(_))
    ));
    // The server never answers again: past the lease's end, it is gone.
    let mut lost = false;
    let mut now = 3 * MS;
    for _ in 0..200 {
        now += 1_000 * SECOND;
        if stack.poll(&mut link, at(now)) == Some(Event::Lost) {
            lost = true;
            break;
        }
        driver.sent();
    }
    assert!(lost, "lost by {now} ns");
    assert_eq!(stack.lease(), None);
}

#[test]
fn an_offer_for_another_transaction_or_of_no_unicast_address_takes_nothing() {
    let shared = Shared::new();
    let mut link = shared.link();
    let mut room = storage();
    let mut stack = Stack::new(OUR_MAC, seed(), &mut link, &mut room, at(0)).unwrap();
    link.start();
    let mut driver = Driver::new(&shared);
    stack.poll(&mut link, at(MS));
    let (_, xid, _) = dhcp_sent(&driver.sent()[0]).unwrap();
    let other = [xid[0] ^ 1, xid[1], xid[2], xid[3]];
    driver.deliver(&reply(OFFER, other, OFFERED));
    driver.deliver(&reply(ACK, other, OFFERED));
    for address in [[255, 255, 255, 255], [224, 0, 0, 1], [0, 0, 0, 0]] {
        driver.deliver(&reply(OFFER, xid, address));
    }
    assert_eq!(stack.poll(&mut link, at(2 * MS)), None);
    assert!(driver.sent().is_empty(), "no REQUEST for any of them");
    assert_eq!(stack.lease(), None);
    assert_eq!(link.counts().received, 5);
}

#[test]
fn a_hardware_address_that_is_not_unicast_is_refused() {
    let shared = Shared::new();
    let mut link = shared.link();
    for mac in [Mac([0xff; 6]), Mac([0x01, 0, 0x5e, 0, 0, 1])] {
        let mut room = storage();
        assert!(matches!(
            Stack::new(mac, seed(), &mut link, &mut room, at(0)),
            Err(nowayos_ip::StackError::NotUnicast(_))
        ));
    }
}

/// One thing the driver's side does: a frame of these bytes in a buffer it
/// holds, or a descriptor of its own making.
#[derive(Clone, Debug)]
enum Step {
    Frame(Vec<u8>),
    Descriptor(u64, u16),
    Wait(u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => proptest::collection::vec(any::<u8>(), 0..MAX_FRAME_LEN + 64).prop_map(Step::Frame),
        2 => (any::<u64>(), any::<u16>()).prop_map(|(o, l)| Step::Descriptor(o, l)),
        1 => any::<u16>().prop_map(Step::Wait),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the driver passes - frames of any bytes and length, and
    /// descriptors of its own making - the stack never panics, every frame
    /// it sends is a whole Ethernet frame, every buffer the driver passes
    /// back goes straight back to it, and the stack never gives it a buffer
    /// it already has.
    #[test]
    fn whatever_the_driver_passes_the_stack_keeps_its_buffers(steps in proptest::collection::vec(step(), 0..24)) {
        let shared = Shared::new();
        let mut link = shared.link();
        let mut room = storage();
        let mut stack = Stack::new(OUR_MAC, seed(), &mut link, &mut room, at(0)).unwrap();
        link.start();
        let mut driver = Driver::new(&shared);
        let mut driver_has = std::collections::BTreeSet::new();
        let mut now = 0u64;
        for step in steps {
            while let Some(descriptor) = driver.rx_free.dequeue().unwrap() {
                prop_assert!(driver_has.insert(descriptor.io_or_offset), "a buffer given twice");
            }
            match step {
                Step::Frame(bytes) => {
                    if let Some(offset) = driver_has.pop_first() {
                        let len = bytes.len().min(BUFFER_SIZE);
                        let mut memory = &shared.rx[..];
                        prop_assert!(memory.store(offset as usize, &bytes[..len]));
                        driver.rx_active.enqueue(Descriptor::new(offset, len as u16)).unwrap();
                    }
                }
                Step::Descriptor(offset, len) => {
                    // A buffer the driver held is passed back, whatever the
                    // descriptor says of it.
                    if driver.rx_active.enqueue(Descriptor::new(offset, len)).is_ok() {
                        driver_has.remove(&offset);
                    }
                }
                Step::Wait(ms) => now += u64::from(ms) * MS,
            }
            now += MS;
            let _ = stack.poll(&mut link, at(now));
            link.service();
            for frame in driver.sent() {
                prop_assert!((14..=MAX_FRAME_LEN).contains(&frame.len()));
            }
            prop_assert_eq!(link.held().0, 0, "every buffer passed back went back");
        }
    }
}
