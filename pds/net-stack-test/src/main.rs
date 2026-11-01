// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-net-stack-test`: the test instrument of the network stack PD's
//! systems. Two PDs run this program and pick their script by PD
//! name:
//!
//! * `seed_source` stands in for the entropy PD: at
//!   start-up it writes one seed on the stack's seed ring - a **fixed test
//!   seed, never random**, so it may run only in a test system
//!   (`tools/grant-rules.toml`, `[test_only]`) - and notifies the stack.
//! * `driver_stand_in` stands in for a hostile NIC driver
//!   (`systems/net-stack-bad-frames.system`): it holds the driver's ends of
//!   the four queues, maps the receive buffers read-write - as the card
//!   writes them - and the transmit buffers read-only. When the stack first
//!   notifies it, it checks the stack's first frame is a DHCP discover from
//!   the stack's MAC address, takes every receive buffer, and passes back,
//!   on purpose, two oversize frames (1515 and 2048 bytes), three malformed
//!   descriptors (a frame shorter than an Ethernet header, an offset outside
//!   the buffers, one off a buffer boundary) and two frames that pass the
//!   stack's checks but that smoltcp cannot use (an unknown EtherType; an
//!   IPv4 header longer than its frame). The stack must drop and count the
//!   first five, hand the last two to smoltcp, and carry on. Every frame
//!   the stack sends is given back.
//!
//! A check that fails prints `gave up` and the reason, which the expect
//! files forbid. No `unsafe` code; regions are bound through
//! `nowayos-region`.

#![no_std]
#![no_main]

use nowayos_display::FrameMemory;
use nowayos_ipc::Producer as RingProducer;
use nowayos_net::arp::Mac;
use nowayos_net::{
    BUFFER_SIZE, BUFFERS, Consumer, DATA_REGION_LEN, Descriptor, MAX_FRAME_LEN, Producer,
};
use nowayos_region::{
    Frame, NetQueue, ReadOnly, ReadWrite, RegionError, RingTx, net_queue, read_only, read_write,
    ring_producer,
};
use nowayos_seed::{SEED_LEN, SEED_RING_SLOT, SEED_RING_SLOTS, Seed};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// The one channel either script has: to the stack.
const STACK: Channel = Channel::new(0);

/// The fixed test seed: never random.
const TEST_SEED: [u8; SEED_LEN] = *b"nowayos test seed: never random!";

/// The stack's MAC address, as its PD fixes it.
const STACK_MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

/// The driver's ends of the queues, read-write regions 0 to 3; the receive
/// buffers, read-write region 4; the transmit buffers, read-only region 0.
const RX_FREE: usize = 0;
const RX_ACTIVE: usize = 1;
const TX_FREE: usize = 2;
const TX_ACTIVE: usize = 3;
const RX_BUFFERS: usize = 4;
const TX_BUFFERS: usize = 0;

/// The seed ring's messages (read-write region 0) and control (read-only
/// region 0), in `seed_source`.
const SEED_MESSAGES: usize = 0;
const SEED_CONTROL: usize = 0;

/// One queue.
type Queue = NetQueue<BUFFERS>;

#[protection_domain]
fn init() -> TestPd {
    let name = pd_name().unwrap_or("?");
    debug_println!("NOWAYOS: net-stack-test {name}: starting");
    let stand_in = match name {
        "seed_source" => {
            if let Err(error) = send_seed() {
                debug_println!("NOWAYOS: net-stack-test {name}: gave up: {error}");
            }
            None
        }
        "driver_stand_in" => match StandIn::bind() {
            Ok(stand_in) => Some(stand_in),
            Err(error) => {
                debug_println!("NOWAYOS: net-stack-test {name}: gave up: {error}");
                None
            }
        },
        _ => {
            debug_println!("NOWAYOS: net-stack-test {name}: gave up: no script for this PD");
            None
        }
    };
    TestPd { stand_in }
}

/// Writes the test seed on the stack's seed ring, and notifies the stack.
fn send_seed() -> Result<(), &'static str> {
    let ring: RingTx<SEED_RING_SLOTS, SEED_RING_SLOT> = ring_producer(
        read_write(SEED_MESSAGES).map_err(|_| "no seed ring")?,
        read_only(SEED_CONTROL).map_err(|_| "no seed ring")?,
    )
    .map_err(|_| "the seed ring's regions")?;
    let mut tx = RingProducer::new(&ring).map_err(|_| "the seed ring is broken")?;
    let seed = Seed::new(TEST_SEED).ok_or("the test seed is zero")?;
    tx.try_send(&seed).map_err(|_| "could not send the seed")?;
    debug_println!(
        "NOWAYOS: net-stack-test seed_source: a fixed test seed sent on the stack's seed ring"
    );
    STACK.notify();
    Ok(())
}

/// The hostile driver's state.
struct StandIn {
    rx_free: Consumer<Queue>,
    rx_active: Producer<Queue>,
    tx_free: Producer<Queue>,
    tx_active: Consumer<Queue>,
    rx_buffers: Frame<ReadWrite>,
    tx_buffers: Frame<ReadOnly>,
    /// Whether the stack's first frame was looked at.
    looked: bool,
    /// Whether the bad frames were passed back.
    done: bool,
}

/// Why the stand-in could not bind its regions.
enum BindError {
    Region(RegionError),
    Queue,
}

impl From<RegionError> for BindError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl core::fmt::Display for BindError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Region(error) => write!(f, "{error}"),
            Self::Queue => f.write_str("a queue's capacity"),
        }
    }
}

impl StandIn {
    fn bind() -> Result<Self, BindError> {
        Ok(Self {
            rx_free: Consumer::new(net_queue(read_write(RX_FREE)?)?)
                .map_err(|_| BindError::Queue)?,
            rx_active: Producer::new(net_queue(read_write(RX_ACTIVE)?)?)
                .map_err(|_| BindError::Queue)?,
            tx_free: Producer::new(net_queue(read_write(TX_FREE)?)?)
                .map_err(|_| BindError::Queue)?,
            tx_active: Consumer::new(net_queue(read_write(TX_ACTIVE)?)?)
                .map_err(|_| BindError::Queue)?,
            rx_buffers: read_write(RX_BUFFERS)?.into_frame(DATA_REGION_LEN)?,
            tx_buffers: read_only(TX_BUFFERS)?.into_frame(DATA_REGION_LEN)?,
            looked: false,
            done: false,
        })
    }

    /// What the stack sent, given back; then, once, the bad frames.
    fn service(&mut self) {
        while let Ok(Some(descriptor)) = self.tx_active.dequeue() {
            if !self.looked {
                self.looked = true;
                self.look_at(descriptor);
            }
            let _ = self
                .tx_free
                .enqueue(Descriptor::new(descriptor.io_or_offset, 0));
        }
        if !self.done {
            self.pass_back_bad_frames();
        }
        STACK.notify();
    }

    /// The stack's first frame must be a DHCP discover from its MAC.
    fn look_at(&self, descriptor: Descriptor) {
        let mut frame = [0u8; MAX_FRAME_LEN];
        let len = usize::from(descriptor.len).min(MAX_FRAME_LEN);
        let offset = usize::try_from(descriptor.io_or_offset).unwrap_or(usize::MAX);
        let Some(bytes) = frame.get_mut(..len) else {
            return;
        };
        if !self.tx_buffers.load(offset, bytes) {
            debug_println!(
                "NOWAYOS: net-stack-test driver_stand_in: gave up: the stack's first frame is outside its buffers"
            );
            return;
        }
        if is_dhcp_discover(bytes) {
            debug_println!(
                "NOWAYOS: net-stack-test driver_stand_in: the stack asks for a lease: a DHCP discover from {STACK_MAC}"
            );
        } else {
            debug_println!(
                "NOWAYOS: net-stack-test driver_stand_in: gave up: the stack's first frame is not a DHCP discover from {STACK_MAC}"
            );
        }
    }

    /// Takes every receive buffer, then passes back the bad frames.
    fn pass_back_bad_frames(&mut self) {
        let mut buffers = [0u64; BUFFERS];
        let mut taken = 0usize;
        while let Ok(Some(descriptor)) = self.rx_free.dequeue() {
            if let Some(slot) = buffers.get_mut(taken) {
                *slot = descriptor.io_or_offset;
                taken = taken.saturating_add(1);
            }
        }
        if taken < 6 {
            return;
        }
        self.done = true;
        let buffer = |index: usize| buffers.get(index).copied().unwrap_or(u64::MAX);
        // An unknown EtherType (IEEE 802's local experimental one), and an
        // IPv4 header whose length (60 bytes) runs past its 34-byte frame.
        let mut unknown = [0x5au8; 60];
        unknown[..12].copy_from_slice(&[0xff; 12]);
        unknown[12..14].copy_from_slice(&[0x88, 0xb5]);
        let mut short_ip = [0u8; 34];
        short_ip[..6].copy_from_slice(&STACK_MAC.0);
        short_ip[12..14].copy_from_slice(&[0x08, 0x00]);
        short_ip[14] = 0x4f;
        let stored = self.store(buffer(4), &unknown) && self.store(buffer(5), &short_ip);
        if !stored {
            debug_println!(
                "NOWAYOS: net-stack-test driver_stand_in: gave up: could not write a frame"
            );
            return;
        }
        let descriptors = [
            // Oversize: a frame longer than 1514 bytes, and a whole buffer.
            Descriptor::new(buffer(0), as_u16(MAX_FRAME_LEN.saturating_add(1))),
            Descriptor::new(buffer(1), as_u16(BUFFER_SIZE)),
            // Malformed: shorter than an Ethernet header, outside the
            // buffers, off a buffer boundary.
            Descriptor::new(buffer(2), 13),
            Descriptor::new(as_u64(DATA_REGION_LEN), 60),
            Descriptor::new(buffer(3).saturating_add(1), 60),
            // Frames smoltcp cannot use.
            Descriptor::new(buffer(4), 60),
            Descriptor::new(buffer(5), 34),
        ];
        let mut passed = 0usize;
        for descriptor in descriptors {
            if self.rx_active.enqueue(descriptor).is_ok() {
                passed = passed.saturating_add(1);
            }
        }
        debug_println!(
            "NOWAYOS: net-stack-test driver_stand_in: took {taken} receive buffers; passed back {passed} descriptors: 2 oversize frames, 3 malformed and 2 frames smoltcp cannot use"
        );
    }

    /// Writes `frame` into the receive buffer at `offset`.
    fn store(&mut self, offset: u64, frame: &[u8]) -> bool {
        usize::try_from(offset).is_ok_and(|offset| self.rx_buffers.store(offset, frame))
    }
}

/// Whether `frame` is a DHCP discover from the stack's MAC address:
/// Ethernet from it, IPv4, UDP from port 68 to 67, a BOOTP request whose
/// message type (option 53) is 1.
fn is_dhcp_discover(frame: &[u8]) -> bool {
    let from_stack = frame.get(6..12) == Some(&STACK_MAC.0[..]);
    let ipv4_udp = frame.get(12..14) == Some(&[0x08, 0x00][..]) && frame.get(23) == Some(&17);
    let ihl = frame
        .get(14)
        .map_or(0, |byte| usize::from(byte & 0x0f).saturating_mul(4));
    let udp = frame.get(14usize.saturating_add(ihl)..).unwrap_or_default();
    let ports = udp.get(..4) == Some(&[0, 68, 0, 67][..]);
    let bootp = udp.get(8..).unwrap_or_default();
    let request = bootp.first() == Some(&1);
    let discover = bootp.get(240..).is_some_and(option_53_is_discover);
    from_stack && ipv4_udp && ports && request && discover
}

/// Whether DHCP options `options` hold message type 1, a discover.
fn option_53_is_discover(mut options: &[u8]) -> bool {
    while let [code, rest @ ..] = options {
        match code {
            255 => return false,
            0 => options = rest,
            _ => {
                let Some((&len, rest)) = rest.split_first() else {
                    return false;
                };
                let len = usize::from(len);
                if *code == 53 {
                    return len == 1 && rest.first() == Some(&1);
                }
                options = rest.get(len..).unwrap_or_default();
            }
        }
    }
    false
}

/// An offset in a data region, as a descriptor holds it.
fn as_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// A length, as a descriptor holds it.
fn as_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// The test PD: the stand-in, if this PD is one.
struct TestPd {
    stand_in: Option<StandIn>,
}

impl Handler for TestPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if let Some(stand_in) = self.stand_in.as_mut()
            && channels.contains(STACK)
        {
            stand_in.service();
        }
        Ok(())
    }
}
