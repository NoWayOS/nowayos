// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-net-client`: a test instrument standing in for the network
//! stack PD: the virtio-net driver's one client, holding what the
//! stack will hold - the four queues with the driver, the
//! receive buffers read-only and its transmit buffers - and nothing of the
//! card's.
//!
//! At start-up it gives the driver every receive buffer, then sends, on
//! purpose, five descriptors the driver must drop and count: one outside
//! the transmit buffers, one off a buffer boundary, one frame too long, one
//! too short, and its ARP request's buffer a second time while the first is
//! still being sent. Between the last two it sends a real frame: an ARP
//! request from 10.0.2.15 for 10.0.2.2, the gateway of QEMU's user-mode
//! network (`systems/net-test.qemu` sets both addresses, and gives the card
//! the MAC address this PD uses).
//!
//! Each frame the driver passes back is checked - in its data region, on a
//! buffer boundary, from an Ethernet header to 1514 bytes, a buffer this PD
//! had given the driver - then copied out of the shared receive buffer
//! before it is parsed (`nowayos_net::arp`), and its buffer given back.
//! When the gateway's ARP reply arrives, well formed and addressed to this
//! PD, it prints the gateway's MAC address; any other frame is ignored.
//!
//! Never in a demo or product system (`tools/grant-rules.toml`: it
//! misbehaves on purpose). No `unsafe` code; regions are bound through
//! `nowayos-region`.

#![no_std]
#![no_main]

use nowayos_display::FrameMemory;
use nowayos_net::arp::{self, Ipv4, Mac, Operation};
use nowayos_net::{
    BUFFER_SIZE, BUFFERS, BufferSet, Consumer, DATA_REGION_LEN, DataRegion, Descriptor,
    ETHERNET_HEADER_LEN, MAX_FRAME_LEN, Producer,
};
use nowayos_region::{
    Frame, NetQueue, ReadOnly, ReadWrite, RegionError, net_queue, read_only, read_write,
};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: net-client:";

/// The queues with the driver, read-write regions 0 to 3.
const RX_FREE: usize = 0;
const RX_ACTIVE: usize = 1;
const TX_FREE: usize = 2;
const TX_ACTIVE: usize = 3;

/// The receive buffers (read-only region 0) and the transmit buffers
/// (read-write region 4).
const RX_BUFFERS: usize = 0;
const TX_BUFFERS: usize = 4;

/// The channel to the driver.
const DRIVER: Channel = Channel::new(0);

/// This PD's addresses, and the gateway's: `systems/net-test.qemu` gives
/// the card this MAC address and QEMU's user-mode network these addresses.
const OUR_MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
const OUR_IP: Ipv4 = Ipv4([10, 0, 2, 15]);
const GATEWAY: Ipv4 = Ipv4([10, 0, 2, 2]);

/// The transmit buffer the ARP request goes in.
const REQUEST_BUFFER: usize = 0;

/// One queue with the driver.
type Queue = NetQueue<BUFFERS>;

#[protection_domain]
fn init() -> ClientPd {
    let client = match Client::bind() {
        Ok(mut client) => {
            debug_println!("{TAG} starting: {OUR_IP} at {OUR_MAC}");
            client.start();
            Some(client)
        }
        Err(error) => {
            debug_println!("{TAG} gave up: {error}");
            None
        }
    };
    ClientPd { client }
}

/// Everything the client holds.
struct Client {
    rx_free: Producer<Queue>,
    rx_active: Consumer<Queue>,
    tx_free: Consumer<Queue>,
    tx_active: Producer<Queue>,
    rx_buffers: Frame<ReadOnly>,
    tx_buffers: Frame<ReadWrite>,
    /// Both data regions' buffers, for checking descriptors.
    buffers: DataRegion,
    /// Receive buffers this PD holds (none, once all are given).
    rx_held: BufferSet<BUFFERS>,
    /// Transmit buffers this PD holds.
    tx_held: BufferSet<BUFFERS>,
    /// Whether the gateway has answered.
    answered: bool,
}

impl Client {
    fn bind() -> Result<Self, BindError> {
        let rx_buffers = read_only(RX_BUFFERS)?.into_frame(DATA_REGION_LEN)?;
        let tx_buffers = read_write(TX_BUFFERS)?.into_frame(DATA_REGION_LEN)?;
        if rx_buffers.len() != DATA_REGION_LEN || tx_buffers.len() != DATA_REGION_LEN {
            return Err(BindError::Size);
        }
        Ok(Self {
            rx_free: Producer::new(net_queue(read_write(RX_FREE)?)?)
                .map_err(|_| BindError::Queue)?,
            rx_active: Consumer::new(net_queue(read_write(RX_ACTIVE)?)?)
                .map_err(|_| BindError::Queue)?,
            tx_free: Consumer::new(net_queue(read_write(TX_FREE)?)?)
                .map_err(|_| BindError::Queue)?,
            tx_active: Producer::new(net_queue(read_write(TX_ACTIVE)?)?)
                .map_err(|_| BindError::Queue)?,
            rx_buffers,
            tx_buffers,
            buffers: DataRegion::new(BUFFERS).ok_or(BindError::Size)?,
            rx_held: BufferSet::all(),
            tx_held: BufferSet::all(),
            answered: false,
        })
    }

    /// Gives the driver every receive buffer, then the transmit
    /// descriptors: the malformed ones and the ARP request.
    fn start(&mut self) {
        for buffer in 0..BUFFERS {
            if let Some(offset) = self.buffers.offset_of(buffer)
                && self
                    .rx_free
                    .enqueue(Descriptor::new(as_u64(offset), 0))
                    .is_ok()
            {
                self.rx_held.remove(buffer);
            }
        }
        let mut request = [0u8; arp::MIN_FRAME_LEN];
        let Ok(len) = arp::request(OUR_MAC, OUR_IP, GATEWAY, &mut request) else {
            debug_println!("{TAG} gave up: cannot build the ARP request");
            return;
        };
        let request_offset = self.buffers.offset_of(REQUEST_BUFFER).unwrap_or(0);
        if !self
            .tx_buffers
            .store(request_offset, request.get(..len).unwrap_or_default())
        {
            debug_println!("{TAG} gave up: cannot write the ARP request");
            return;
        }
        let len = as_u16(len);
        let malformed_before = [
            // Outside the transmit buffers.
            Descriptor::new(as_u64(DATA_REGION_LEN), len),
            // Off a buffer boundary.
            Descriptor::new(as_u64(BUFFER_SIZE.saturating_add(100)), len),
            // Longer than a frame may be (buffer 2, given away).
            Descriptor::new(
                as_u64(BUFFER_SIZE.saturating_mul(2)),
                as_u16(MAX_FRAME_LEN.saturating_add(1)),
            ),
            // Shorter than an Ethernet header (buffer 3, given away).
            Descriptor::new(
                as_u64(BUFFER_SIZE.saturating_mul(3)),
                as_u16(ETHERNET_HEADER_LEN.saturating_sub(1)),
            ),
        ];
        let request = Descriptor::new(as_u64(request_offset), len);
        let mut sent = 0usize;
        for descriptor in malformed_before.into_iter().chain([request, request]) {
            if self.tx_active.enqueue(descriptor).is_ok() {
                sent = sent.saturating_add(1);
            }
        }
        for buffer in [REQUEST_BUFFER, 2, 3] {
            self.tx_held.remove(buffer);
        }
        debug_println!(
            "{TAG} sent 4 malformed descriptors, an ARP request (who has {GATEWAY}? tell {OUR_IP}) and the request's buffer again: {sent} descriptors"
        );
        // sDDF's protocol: notify the driver if it asked to be, on either
        // queue this PD filled.
        let rx = self.rx_free.signal_wanted();
        let tx = self.tx_active.signal_wanted();
        if rx || tx {
            DRIVER.notify();
        }
    }

    /// Takes in what the driver passed back - frames, then sent buffers -
    /// asks for a notification, and looks once more; then notifies the
    /// driver of the receive buffers given back, if it asked to be.
    fn service(&mut self) {
        let mut gave = false;
        for _ in 0..2 {
            gave |= self.receive();
            self.reclaim();
            self.rx_active.request_signal();
            self.tx_free.request_signal();
        }
        if gave && self.rx_free.signal_wanted() {
            DRIVER.notify();
        }
    }

    /// Each frame received: checked, copied out, parsed, and its buffer
    /// given back to the driver. Returns whether any buffer was.
    fn receive(&mut self) -> bool {
        let mut gave = false;
        while let Ok(Some(descriptor)) = self.rx_active.dequeue() {
            let frame = match self
                .buffers
                .frame(descriptor, ETHERNET_HEADER_LEN..=MAX_FRAME_LEN)
            {
                Ok(frame) if self.rx_held.insert(frame.buffer) => frame,
                // Not a frame in a buffer the driver held: ignored.
                _ => continue,
            };
            let mut copy = [0u8; MAX_FRAME_LEN];
            let bytes = copy.get_mut(..frame.len).unwrap_or_default();
            if self.rx_buffers.load(frame.offset, bytes) {
                self.answer(bytes);
            }
            if self
                .rx_free
                .enqueue(Descriptor::new(as_u64(frame.offset), 0))
                .is_ok()
            {
                self.rx_held.remove(frame.buffer);
                gave = true;
            }
        }
        gave
    }

    /// The gateway's reply, if `frame` is it: printed once.
    fn answer(&mut self, frame: &[u8]) {
        let Ok(reply) = arp::parse(frame) else {
            return;
        };
        let addressed_to_us = reply.destination == OUR_MAC
            && reply.target_mac == OUR_MAC
            && reply.target_ip == OUR_IP;
        let from_the_gateway = reply.sender_ip == GATEWAY && reply.source == reply.sender_mac;
        if reply.operation == Operation::Reply
            && addressed_to_us
            && from_the_gateway
            && !self.answered
        {
            self.answered = true;
            debug_println!("{TAG} {GATEWAY} is at {}", reply.sender_mac);
        }
    }

    /// Transmit buffers the driver has sent: this PD's again.
    fn reclaim(&mut self) {
        while let Ok(Some(descriptor)) = self.tx_free.dequeue() {
            if let Ok(buffer) = self.buffers.buffer(descriptor) {
                self.tx_held.insert(buffer);
            }
        }
    }
}

/// Why the client could not bind its regions.
enum BindError {
    Region(RegionError),
    Queue,
    Size,
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
            Self::Size => write!(f, "a data region is not {DATA_REGION_LEN} bytes"),
        }
    }
}

/// An offset in a data region, as a descriptor holds it.
fn as_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// A length, as a descriptor holds it.
fn as_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// The client, if it could bind its regions.
struct ClientPd {
    client: Option<Client>,
}

impl Handler for ClientPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if let Some(client) = self.client.as_mut()
            && channels.contains(DRIVER)
        {
            client.service();
        }
        Ok(())
    }
}
