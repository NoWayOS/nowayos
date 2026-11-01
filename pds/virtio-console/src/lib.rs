// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-virtio-console`: the serial driver PD for QEMU's virtio console,
//! a serial port without a UART.
//!
//! It finds the console among the virtio-mmio transports it is granted and
//! negotiates no optional feature - one port, port 0, with its receive queue
//! (0) and transmit queue (1) - through `nowayos-hw`'s virtio layer, and
//! passes a byte stream between the device and one client through
//! `nowayos-serial`'s rings: the client's bytes from the transmit ring to the
//! transmit queue, the device's from the receive queue to the receive ring.
//!
//! **Only the directions it is given.** [`init`] takes [`Directions`]: a
//! driver that only transmits sets up no receive queue and binds no receive
//! ring, and one that only receives sets up no transmit queue and binds no
//! transmit ring - and the system description maps it no memory for them.
//! A one-way link is one way in the driver too.
//!
//! **Never blocks, never trusts.** A chunk from the client waits in the
//! driver only until the device has a free buffer; the client learns that
//! only from the ring's free slots. Received bytes stay in the device's
//! buffers until the receive ring has room, so a slow client holds the
//! device back rather than losing bytes inside the unit. The client and the
//! device are both untrusted: a malformed chunk, or a used entry naming no
//! buffer the device holds, is dropped and counted, never followed and never
//! a panic, and the counts are reported whenever they change.
//!
//! Resources, by number, as the system description binds them:
//!
//! | What | Binding |
//! | --- | --- |
//! | The page of virtio-mmio transports | device 0; the console on transport 7 (QEMU's first virtio device, transport 31) |
//! | Its interrupt | channel [`DEVICE_IRQ`] |
//! | The receive queue, in DMA memory | DMA region 0 (receiving only) |
//! | The transmit queue, in DMA memory | DMA region 1 (transmitting only) |
//! | The transmit ring from the client | read-write region 0 (its control), read-only region 0 (its messages) |
//! | The receive ring to the client | read-write region 1 (its messages), read-only region 1 (its control) |
//! | The client | channel [`CLIENT`]: it notifies when it has sent, or taken, chunks; the driver notifies it when it has chunks to take |
//!
//! Written from the virtio 1.2 specification, section 5.3 (the console
//! device: its ID, queues and feature bits, and that a driver puts only
//! device-readable buffers on a transmit queue and device-writable ones on a
//! receive queue) and section 4.2 (virtio over MMIO), and QEMU's
//! documentation of its `virtio-serial-device` and `virtconsole` devices. No
//! other source. Everything raw is in `nowayos-hw`; this crate has no
//! `unsafe` code. QEMU only: boards carry their links on UARTs.

#![no_std]

use core::fmt;

use nowayos_hw::virtio::{DEVICE_CONSOLE, F_VERSION_1, VirtioMmio, VirtioPage};
use nowayos_hw::virtqueue::{RxQueue, TxQueue};
use nowayos_hw::{HwError, device, dma_region};
use nowayos_ipc::{Consumer, Producer, RecvError};
use nowayos_region::{
    RegionError, RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer,
};
use nowayos_serial::{CHUNK_LEN, Chunk, RING_SLOT, RING_SLOTS};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The client's channel.
pub const CLIENT: Channel = Channel::new(0);

/// The channel the device's interrupt arrives on.
pub const DEVICE_IRQ: Channel = Channel::new(1);

/// The device number the system description binds the transports' page to.
const TRANSPORTS: usize = 0;

/// The transport the console must be on: QEMU puts the first virtio device
/// on transport 31, index 7 of the page of transports 24 to 31, whose
/// interrupt the system description gives on [`DEVICE_IRQ`].
const TRANSPORT: usize = 7;

/// Port 0's queues (section 5.3.2).
const RECEIVE_QUEUE: u32 = 0;
const TRANSMIT_QUEUE: u32 = 1;

/// Buffers in each queue, each holding one chunk.
const QUEUE_LEN: usize = 8;

/// The DMA regions and rings, by number (see the crate documentation).
const RECEIVE_DMA: usize = 0;
const TRANSMIT_DMA: usize = 1;
const TRANSMIT_RING: usize = 0;
const RECEIVE_RING: usize = 1;

/// Which ways a driver carries bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directions {
    /// Only the client's bytes out to the device: no receive queue, no
    /// receive ring.
    Transmit,
    /// Only the device's bytes in to the client: no transmit queue, no
    /// transmit ring.
    Receive,
    /// Both ways.
    Both,
}

impl Directions {
    const fn transmits(self) -> bool {
        matches!(self, Self::Transmit | Self::Both)
    }

    const fn receives(self) -> bool {
        matches!(self, Self::Receive | Self::Both)
    }
}

impl fmt::Display for Directions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Transmit => "transmit only",
            Self::Receive => "receive only",
            Self::Both => "transmit and receive",
        })
    }
}

/// Why the driver gave up at start.
enum Failure {
    Device(HwError),
    Region(RegionError),
    Transport(usize),
    Features(u64),
}

impl From<HwError> for Failure {
    fn from(error: HwError) -> Self {
        Self::Device(error)
    }
}

impl From<RegionError> for Failure {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(f, "{error}"),
            Self::Region(error) => write!(f, "a ring to the client: {error}"),
            Self::Transport(index) => write!(
                f,
                "the console is on transport {index}, not {TRANSPORT}, whose interrupt it was given"
            ),
            Self::Features(accepted) => write!(f, "features {accepted:#x} accepted"),
        }
    }
}

/// The receiving half: the device's receive queue and the ring to the
/// client.
struct Receive {
    queue: RxQueue<QUEUE_LEN>,
    ring: RingTx<RING_SLOTS, RING_SLOT>,
}

/// The transmitting half: the ring from the client, the device's transmit
/// queue, and a chunk waiting for a free buffer.
struct Transmit {
    ring: RingRx<RING_SLOTS, RING_SLOT>,
    queue: TxQueue<QUEUE_LEN>,
    waiting: Option<Chunk>,
}

/// What the driver has refused, as last reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Refused {
    /// Records from the client that were not a valid chunk.
    malformed: u32,
    /// Used entries from the device that failed their checks.
    entries: u32,
    /// Times a ring's indices were impossible.
    rings: u32,
}

/// The driver PD. Make it with [`init`].
pub struct VirtioConsolePd {
    mmio: Option<VirtioMmio<'static>>,
    receive: Option<Receive>,
    transmit: Option<Transmit>,
    refused: Refused,
    reported: Refused,
}

/// Sets the console up for `directions` and returns the driver, reporting on
/// the debug console. If anything is missing or wrong it says why, and the
/// driver then does nothing.
#[must_use]
pub fn init(directions: Directions) -> VirtioConsolePd {
    debug_println!("NOWAYOS: virtio-console: starting");
    let mut pd = VirtioConsolePd {
        mmio: None,
        receive: None,
        transmit: None,
        refused: Refused::default(),
        reported: Refused::default(),
    };
    match pd.start(directions) {
        Ok(()) => {
            debug_println!("NOWAYOS: virtio-console: ready: {directions}, on transport {TRANSPORT}")
        }
        Err(failure) => {
            pd = VirtioConsolePd {
                mmio: None,
                receive: None,
                transmit: None,
                refused: Refused::default(),
                reported: Refused::default(),
            };
            debug_println!("NOWAYOS: virtio-console: gave up: {failure}");
        }
    }
    pd
}

impl VirtioConsolePd {
    fn start(&mut self, directions: Directions) -> Result<(), Failure> {
        let mut page = VirtioPage::new(device(TRANSPORTS)?)?;
        let mut mmio = page.take(DEVICE_CONSOLE)?;
        if mmio.index() != TRANSPORT {
            return Err(Failure::Transport(mmio.index()));
        }
        // No optional feature: one port, no size, no emergency write.
        let accepted = mmio.negotiate(0)?;
        if accepted != F_VERSION_1 {
            return Err(Failure::Features(accepted));
        }
        if directions.receives() {
            let ring = ring_producer(read_write(RECEIVE_RING)?, read_only(RECEIVE_RING)?)?;
            let queue = RxQueue::new(dma_region(RECEIVE_DMA)?, CHUNK_LEN)?;
            mmio.set_up_queue(RECEIVE_QUEUE, &queue)?;
            self.receive = Some(Receive { queue, ring });
        }
        if directions.transmits() {
            let ring = ring_consumer(read_write(TRANSMIT_RING)?, read_only(TRANSMIT_RING)?)?;
            let queue = TxQueue::new(dma_region(TRANSMIT_DMA)?, CHUNK_LEN)?;
            mmio.set_up_queue(TRANSMIT_QUEUE, &queue)?;
            self.transmit = Some(Transmit {
                ring,
                queue,
                waiting: None,
            });
        }
        mmio.driver_ok();
        if let Some(receive) = &mut self.receive {
            receive.queue.offer_all()?;
            mmio.notify(RECEIVE_QUEUE);
        }
        self.mmio = Some(mmio);
        Ok(())
    }

    /// Moves what the device received to the receive ring, while the ring
    /// has room; whether anything was sent.
    fn receive(&mut self) -> Result<bool, HwError> {
        let (Some(mmio), Some(receive)) = (&mut self.mmio, &mut self.receive) else {
            return Ok(false);
        };
        let Ok(mut ring) = Producer::new(&receive.ring) else {
            self.refused.rings = self.refused.rings.saturating_add(1);
            return Ok(false);
        };
        let mut sent = false;
        let mut taken = false;
        let mut bytes = [0u8; CHUNK_LEN];
        while ring.free_slots().unwrap_or(0) > 0 {
            let Some(len) = receive.queue.take(&mut bytes)? else {
                break;
            };
            taken = true;
            // A buffer the device returned empty carries nothing. The ring
            // had a free slot, so a send fails only if the client made its
            // index impossible meanwhile.
            if let Some(chunk) = bytes.get(..len).and_then(Chunk::new) {
                if ring.try_send(&chunk).is_ok() {
                    sent = true;
                } else {
                    self.refused.rings = self.refused.rings.saturating_add(1);
                }
            }
        }
        if taken {
            mmio.notify(RECEIVE_QUEUE);
        }
        self.refused.entries = self.entries();
        Ok(sent)
    }

    /// Takes back the buffers the device has sent, then moves chunks from
    /// the transmit ring to the device while it has free buffers.
    fn transmit(&mut self) -> Result<(), HwError> {
        let (Some(mmio), Some(transmit)) = (&mut self.mmio, &mut self.transmit) else {
            return Ok(());
        };
        transmit.queue.reclaim()?;
        let Ok(mut ring) = Consumer::new(&transmit.ring) else {
            self.refused.rings = self.refused.rings.saturating_add(1);
            return Ok(());
        };
        let mut sent = false;
        loop {
            let chunk = match transmit.waiting.take() {
                Some(chunk) => chunk,
                None => match ring.try_recv::<Chunk>() {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(RecvError::Decode(_)) => {
                        self.refused.malformed = self.refused.malformed.saturating_add(1);
                        continue;
                    }
                    Err(_) => {
                        self.refused.rings = self.refused.rings.saturating_add(1);
                        break;
                    }
                },
            };
            if transmit.queue.send(chunk.bytes())? {
                sent = true;
            } else {
                transmit.waiting = Some(chunk);
                break;
            }
        }
        if sent {
            mmio.notify(TRANSMIT_QUEUE);
        }
        self.refused.entries = self.entries();
        Ok(())
    }

    /// The used entries both queues have refused.
    fn entries(&self) -> u32 {
        let receive = self.receive.as_ref().map_or(0, |r| r.queue.rejected());
        let transmit = self.transmit.as_ref().map_or(0, |t| t.queue.rejected());
        receive.saturating_add(transmit)
    }

    /// Reports what has been refused, if it changed.
    fn report(&mut self) {
        if self.refused != self.reported {
            self.reported = self.refused;
            let Refused {
                malformed,
                entries,
                rings,
            } = self.refused;
            debug_println!(
                "NOWAYOS: virtio-console: refused so far: {malformed} malformed chunk(s) from the client, {entries} malformed queue entries from the device, {rings} corrupt ring(s)"
            );
        }
    }
}

impl Handler for VirtioConsolePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let interrupted = channels.contains(DEVICE_IRQ);
        if interrupted && let Some(mmio) = &mut self.mmio {
            mmio.acknowledge_interrupt();
        }
        // Each step does only what there is to do, so every notification -
        // the device's or the client's - runs both.
        let delivered = match self.receive() {
            Ok(delivered) => delivered,
            Err(error) => {
                debug_println!("NOWAYOS: virtio-console: receiving: {error}");
                false
            }
        };
        if let Err(error) = self.transmit() {
            debug_println!("NOWAYOS: virtio-console: transmitting: {error}");
        }
        if interrupted && DEVICE_IRQ.irq_ack().is_err() {
            debug_println!("NOWAYOS: virtio-console: could not acknowledge an interrupt");
        }
        self.report();
        if delivered {
            CLIENT.notify();
        }
        Ok(())
    }
}
