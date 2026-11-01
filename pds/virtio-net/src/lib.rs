// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-virtio-net`: the NIC driver PD for QEMU's virtio-net card.
//! It passes Ethernet frames between
//! the card and one client - the network stack, or a test instrument in
//! its place - through sDDF's network queues, and never sees a frame's
//! bytes.
//!
//! It finds the card among the virtio-mmio transports it is granted,
//! negotiates `VIRTIO_NET_F_MAC` and reads the card's MAC address
//! (`nowayos-hw`'s `virtio`), and sets up the card's receive and transmit
//! queues in its own DMA regions (`virtio_net`). It holds the card's
//! registers, its interrupt and that DMA memory, and the physical addresses
//! of the client's two data regions - the receive and the transmit buffers
//! - which it hands to the card but never maps.
//!
//! The client gives it empty receive buffers and frames to send, through
//! the receive free and transmit active queues; it gives back received
//! frames and sent buffers, through the receive active and transmit free
//! queues (`nowayos_net::queue`). Neither the client nor the card is
//! trusted:
//!
//! * every descriptor from the client is copied out of its queue and
//!   checked before it is used: inside its data region, on a buffer
//!   boundary, a frame to send from an Ethernet header to 1514 bytes, and a
//!   buffer the driver does not hold already (`nowayos_net::DataRegion`,
//!   `BufferSet`). One that fails is dropped and counted, and its buffer is
//!   the client's loss;
//! * a client index that would put more than a queue's capacity in flight
//!   is counted, and nothing is read from or written to that queue until it
//!   is back in range;
//! * every frame from the card is checked (`virtio_net::NetRx`): oversize
//!   and malformed frames are dropped and counted, and their buffers offered
//!   to the card again.
//!
//! The counts are reported whenever they change. Everything raw is in
//! `nowayos-hw`; this PD has no `unsafe` code (the workspace lints forbid
//! it). Written from the virtio 1.2 specification and sDDF 0.7.0's network
//! queue header. QEMU only: the Pi 5's Ethernet is RP1's, behind PCIe.

#![no_std]

use core::fmt;

use nowayos_hw::virtio::{F_VERSION_1, VirtioMmio};
use nowayos_hw::virtio_net::{
    DEVICE_ID, F_MAC, MAC_OFFSET, NetRx, NetTx, RECEIVE_QUEUE, Received, TRANSMIT_QUEUE,
};
use nowayos_hw::{HwError, device, dma_region, io_region};
use nowayos_net::arp::Mac;
use nowayos_net::{
    BUFFER_SIZE, BUFFERS, BufferSet, Consumer, DATA_REGION_LEN, DataRegion, Descriptor,
    ETHERNET_HEADER_LEN, MAX_FRAME_LEN, Producer, QueueError,
};
use nowayos_region::{NetQueue, RegionError, net_queue, read_write};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The device number the system description binds the transports' page to.
const TRANSPORTS: usize = 0;

/// The DMA regions of the receive and transmit queues: descriptors, rings
/// and virtio-net headers.
const RX_RING: usize = 0;
const TX_RING: usize = 1;

/// The I/O regions: the client's receive and transmit buffers, by physical
/// address only.
const RX_BUFFERS: usize = 0;
const TX_BUFFERS: usize = 1;

/// The queues with the client, read-write regions 0 to 3.
const RX_FREE: usize = 0;
const RX_ACTIVE: usize = 1;
const TX_FREE: usize = 2;
const TX_ACTIVE: usize = 3;

/// The channel to the client, and the card's interrupt.
const CLIENT: Channel = Channel::new(0);
const IRQ: Channel = Channel::new(1);

/// The transport whose interrupt the system description gives this PD:
/// QEMU puts the first `-device` on transport 31, index 7 of the page.
const TRANSPORT: usize = 7;

/// One queue with the client.
type Queue = NetQueue<BUFFERS>;

/// Starts the NIC driver PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> VirtioNetPd {
    debug_println!("NOWAYOS: virtio-net: starting");
    let card = match Card::start() {
        Ok(card) => Some(card),
        Err(error) => {
            debug_println!("NOWAYOS: virtio-net: gave up: {error}");
            None
        }
    };
    VirtioNetPd {
        card,
        dropped: Dropped::default(),
        reported: Counts::default(),
    }
}

/// Why the driver could not start.
enum StartError {
    Hw(HwError),
    Region(RegionError),
    Queue(QueueError),
    Features(u64),
    Transport(usize),
    Buffers,
}

impl From<HwError> for StartError {
    fn from(error: HwError) -> Self {
        Self::Hw(error)
    }
}

impl From<RegionError> for StartError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl From<QueueError> for StartError {
    fn from(error: QueueError) -> Self {
        Self::Queue(error)
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hw(error) => write!(f, "{error}"),
            Self::Region(error) => write!(f, "a queue's region: {error}"),
            Self::Queue(error) => write!(f, "a queue: {error}"),
            Self::Features(accepted) => write!(f, "features {accepted:#x} accepted"),
            Self::Transport(index) => write!(
                f,
                "the card is on transport {index}, whose interrupt this PD does not hold"
            ),
            Self::Buffers => write!(f, "no data region of {BUFFERS} buffers"),
        }
    }
}

/// The card and the queues: everything the driver moves frames with.
struct Card {
    mmio: VirtioMmio<'static>,
    rx: NetRx<BUFFERS>,
    tx: NetTx<BUFFERS>,
    rx_free: Consumer<Queue>,
    rx_active: Producer<Queue>,
    tx_active: Consumer<Queue>,
    tx_free: Producer<Queue>,
    /// Both data regions' buffers, for checking descriptors.
    buffers: DataRegion,
    /// Receive buffers the driver holds: with the card, or being passed on.
    rx_held: BufferSet<BUFFERS>,
    /// Transmit buffers the driver holds: with the card.
    tx_held: BufferSet<BUFFERS>,
}

/// What the client and the card did that was dropped.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Dropped {
    /// Descriptors from the client that failed their checks.
    descriptors: u32,
    /// Times a client index was out of range.
    indices: u32,
}

/// Who to notify after a round of work.
#[derive(Default)]
struct Wake {
    rx_queue: bool,
    tx_queue: bool,
    rx_active: bool,
    tx_free: bool,
}

impl Card {
    fn start() -> Result<Self, StartError> {
        let mut mmio = VirtioMmio::find(device(TRANSPORTS)?, DEVICE_ID)?;
        if mmio.index() != TRANSPORT {
            return Err(StartError::Transport(mmio.index()));
        }
        let accepted = mmio.negotiate(F_MAC)?;
        if accepted != F_VERSION_1 | F_MAC {
            return Err(StartError::Features(accepted));
        }
        let mut mac = [0u8; 6];
        mmio.read_config(MAC_OFFSET, &mut mac)?;
        let rx = NetRx::new(
            dma_region(RX_RING)?,
            io_region(RX_BUFFERS, DATA_REGION_LEN)?,
            BUFFER_SIZE,
        )?;
        let tx = NetTx::new(
            dma_region(TX_RING)?,
            io_region(TX_BUFFERS, DATA_REGION_LEN)?,
        )?;
        let rx_free = Consumer::new(net_queue(read_write(RX_FREE)?)?)?;
        let rx_active = Producer::new(net_queue(read_write(RX_ACTIVE)?)?)?;
        let tx_free = Producer::new(net_queue(read_write(TX_FREE)?)?)?;
        let tx_active = Consumer::new(net_queue(read_write(TX_ACTIVE)?)?)?;
        let buffers = DataRegion::new(BUFFERS).ok_or(StartError::Buffers)?;
        // Everything is bound: only now may the card use its queues.
        mmio.set_up_queue(RECEIVE_QUEUE, rx.queue())?;
        mmio.set_up_queue(TRANSMIT_QUEUE, tx.queue())?;
        mmio.driver_ok();
        let card = Self {
            mmio,
            rx,
            tx,
            rx_free,
            rx_active,
            tx_active,
            tx_free,
            buffers,
            rx_held: BufferSet::none(),
            tx_held: BufferSet::none(),
        };
        debug_println!(
            "NOWAYOS: virtio-net: ready on transport {}, MAC {}",
            card.mmio.index(),
            Mac(mac)
        );
        Ok(card)
    }

    /// One round: frames from the card to the client, sent buffers back to
    /// the client, empty buffers and frames from the client to the card.
    fn service(&mut self, dropped: &mut Dropped) -> Wake {
        let mut wake = Wake::default();
        self.receive(dropped, &mut wake);
        self.reclaim(dropped, &mut wake);
        self.provide(dropped, &mut wake);
        self.transmit(dropped, &mut wake);
        // sDDF's protocol: ask the client for a notification, then look
        // once more, for what it queued without one.
        self.rx_free.request_signal();
        self.tx_active.request_signal();
        self.provide(dropped, &mut wake);
        self.transmit(dropped, &mut wake);
        wake
    }

    /// Frames the card has received: passed to the client, or - dropped by
    /// `NetRx`, or with no room - offered to the card again.
    fn receive(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            let offset = match self.rx.receive() {
                Ok(Some(Received::Frame { offset, len })) => {
                    let descriptor = Descriptor::new(as_u64(offset), as_u16(len));
                    match self.rx_active.enqueue(descriptor) {
                        Ok(()) => {
                            self.rx_held.remove(index(offset));
                            wake.rx_active = true;
                            continue;
                        }
                        Err(_) => {
                            dropped.indices = dropped.indices.saturating_add(1);
                            offset
                        }
                    }
                }
                Ok(Some(Received::Dropped { offset })) => offset,
                Ok(None) => return,
                Err(error) => {
                    debug_println!("NOWAYOS: virtio-net: receive queue: {error}");
                    return;
                }
            };
            // The driver still holds the buffer: give it back to the card.
            if matches!(self.rx.offer(offset), Ok(true)) {
                wake.rx_queue = true;
            } else {
                self.rx_held.remove(index(offset));
            }
        }
    }

    /// Transmit buffers the card has sent: back to the client.
    fn reclaim(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            match self.tx.reclaim() {
                Ok(Some(offset)) => {
                    self.tx_held.remove(index(offset));
                    if self
                        .tx_free
                        .enqueue(Descriptor::new(as_u64(offset), 0))
                        .is_ok()
                    {
                        wake.tx_free = true;
                    } else {
                        dropped.indices = dropped.indices.saturating_add(1);
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    debug_println!("NOWAYOS: virtio-net: transmit queue: {error}");
                    return;
                }
            }
        }
    }

    /// Empty receive buffers from the client: checked, then offered to the
    /// card.
    fn provide(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            let descriptor = match self.rx_free.dequeue() {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => return,
                Err(_) => {
                    dropped.indices = dropped.indices.saturating_add(1);
                    return;
                }
            };
            let buffer = match self.buffers.buffer(descriptor) {
                Ok(buffer) if self.rx_held.insert(buffer) => buffer,
                // Outside the region, off a buffer boundary, or a buffer
                // the driver already holds.
                _ => {
                    dropped.descriptors = dropped.descriptors.saturating_add(1);
                    continue;
                }
            };
            let offered = self
                .buffers
                .offset_of(buffer)
                .is_some_and(|offset| matches!(self.rx.offer(offset), Ok(true)));
            if offered {
                wake.rx_queue = true;
            } else {
                self.rx_held.remove(buffer);
                dropped.descriptors = dropped.descriptors.saturating_add(1);
            }
        }
    }

    /// Frames from the client: checked, then given to the card to send.
    fn transmit(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            let descriptor = match self.tx_active.dequeue() {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => return,
                Err(_) => {
                    dropped.indices = dropped.indices.saturating_add(1);
                    return;
                }
            };
            let frame = match self
                .buffers
                .frame(descriptor, ETHERNET_HEADER_LEN..=MAX_FRAME_LEN)
            {
                Ok(frame) if self.tx_held.insert(frame.buffer) => frame,
                // Outside the region, off a buffer boundary, a length out
                // of range, or a buffer already being sent.
                _ => {
                    dropped.descriptors = dropped.descriptors.saturating_add(1);
                    continue;
                }
            };
            if matches!(self.tx.send(frame.offset, frame.len), Ok(true)) {
                wake.tx_queue = true;
            } else {
                self.tx_held.remove(frame.buffer);
                dropped.descriptors = dropped.descriptors.saturating_add(1);
            }
        }
    }
}

/// The buffer at `offset`, which came from this PD's own checks.
fn index(offset: usize) -> usize {
    offset.checked_div(BUFFER_SIZE).unwrap_or(usize::MAX)
}

/// An offset in a data region, as a descriptor holds it.
fn as_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// A frame's length, as a descriptor holds it (at most 1514).
fn as_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// The driver: the card, if it started, what it has dropped, and the
/// counts last reported.
pub struct VirtioNetPd {
    card: Option<Card>,
    dropped: Dropped,
    reported: Counts,
}

/// Everything dropped so far, as reported.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    oversize: u32,
    malformed: u32,
    descriptors: u32,
    indices: u32,
    used: u32,
}

impl VirtioNetPd {
    /// Reports what was dropped, if it changed.
    fn report(&mut self) {
        let Some(card) = &self.card else {
            return;
        };
        let now = Counts {
            oversize: card.rx.oversize(),
            malformed: card.rx.malformed(),
            descriptors: self.dropped.descriptors,
            indices: self.dropped.indices,
            used: card.rx.rejected().saturating_add(card.tx.rejected()),
        };
        if now != self.reported {
            self.reported = now;
            debug_println!(
                "NOWAYOS: virtio-net: dropped so far: {} oversize frames, {} malformed frames, {} bad descriptors, {} corrupt queue indices, {} bad used entries",
                now.oversize,
                now.malformed,
                now.descriptors,
                now.indices,
                now.used
            );
        }
    }
}

impl Handler for VirtioNetPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let Some(card) = self.card.as_mut() else {
            return Ok(());
        };
        if channels.contains(IRQ) {
            card.mmio.acknowledge_interrupt();
            if IRQ.irq_ack().is_err() {
                debug_println!("NOWAYOS: virtio-net: could not acknowledge the interrupt");
            }
        }
        // Whatever woke the driver, it looks at every queue.
        let wake = card.service(&mut self.dropped);
        if wake.rx_queue {
            card.mmio.notify(RECEIVE_QUEUE);
        }
        if wake.tx_queue {
            card.mmio.notify(TRANSMIT_QUEUE);
        }
        let rx = wake.rx_active && card.rx_active.signal_wanted();
        let tx = wake.tx_free && card.tx_free.signal_wanted();
        if rx || tx {
            CLIENT.notify();
        }
        self.report();
        Ok(())
    }
}
