// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-virtio-input`: the driver PD for QEMU's virtio input devices:
//! it turns QEMU's keyboard and tablet events into validated
//! canonical [`InputEvent`]s.
//!
//! It finds its devices among the virtio-mmio transports it was granted and
//! negotiates their features through `nowayos-hw`'s virtio layer,
//! and sets up each device's event queue in its own DMA region. On
//! each device's interrupt it copies the events the device returned out of
//! DMA memory, translates them (`nowayos_input::evdev`) and sends each
//! canonical event to its consumer through that device's own shared-memory
//! ring - the pointer ring and the keyboard ring - so the input broker can
//! hold each device to its own rate.
//!
//! The devices are not trusted: an event of an unknown type or code, a value
//! out of range, or one that does not fit the ring is dropped and counted,
//! never forwarded and never a panic; the counts are reported whenever they
//! change.
//!
//! Tablet positions are scaled to the screen the system description gives
//! it (`nowayos_region::screen`, read-only region 7).
//!
//! Written from the virtio 1.2 specification. Everything raw - registers and
//! device memory - is in `nowayos-hw`; this PD has no `unsafe` code (the
//! workspace lints forbid it). QEMU only: the Pi 5 has no virtio.

#![no_std]

use nowayos_hw::virtio::{DEVICE_INPUT, F_VERSION_1, VirtioMmio, VirtioPage};
use nowayos_hw::virtqueue::RxQueue;
use nowayos_hw::{HwError, device, dma_region};
use nowayos_input::InputEvent;
use nowayos_input::evdev::{DropCounts, EVENT_LEN, Translator};
use nowayos_ipc::Producer;
use nowayos_region::{RingTx, read_only, read_write, ring_producer, screen};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The device number the system description binds the transports' page to.
const TRANSPORTS: usize = 0;

/// The input devices this driver takes: QEMU's keyboard and tablet.
const DEVICES: usize = 2;

/// The event queue: virtio-input's queue 0, of `EVENTS` buffers each holding
/// one 8-byte event (section 5.8.6: type, code, value).
const EVENT_QUEUE: u32 = 0;
const EVENTS: usize = 16;

/// The channel to the consumer, on which only the driver notifies.
const CONSUMER: Channel = Channel::new(0);

/// Each device's interrupt channel, by the transport it is on: QEMU puts the
/// first `-device` on transport 31 (index 7 of the page) and the next on 30.
const IRQS: [(usize, Channel); DEVICES] = [(6, Channel::new(1)), (7, Channel::new(2))];

/// The largest screen the tablet's positions are scaled to.
const MAX_SIDE: u32 = 8192;

/// Each device's ring to the consumer: `RING_SLOTS` slots of `RING_SLOT`
/// bytes (the consumer uses the same).
const RING_SLOTS: usize = 4;
const RING_SLOT: usize = 32;

/// One input device: its transport, event queue and translator, the channel
/// its interrupt arrives on, and its ring to the consumer.
struct InputDevice {
    mmio: VirtioMmio<'static>,
    events: RxQueue<EVENTS>,
    translator: Translator,
    irq: Channel,
    ring: RingTx<RING_SLOTS, RING_SLOT>,
}

/// Starts the input driver PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> VirtioInputPd {
    debug_println!("NOWAYOS: virtio-input: starting");
    let mut pd = VirtioInputPd {
        devices: [None, None],
        ring_full: 0,
        reported: (DropCounts::default(), 0, 0),
    };
    match start(&mut pd) {
        Ok(()) => {}
        Err(error) => debug_println!("NOWAYOS: virtio-input: gave up: {error}"),
    }
    pd
}

fn start(pd: &mut VirtioInputPd) -> Result<(), HwError> {
    // The screen the tablet's positions are scaled to.
    let (width, height) = match screen() {
        Ok((width, height))
            if (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height) =>
        {
            (width, height)
        }
        Ok((width, height)) => {
            debug_println!("NOWAYOS: virtio-input: gave up: a {width}x{height} screen");
            return Ok(());
        }
        Err(error) => {
            debug_println!("NOWAYOS: virtio-input: gave up: the screen's size: {error}");
            return Ok(());
        }
    };
    let mut page = VirtioPage::new(device(TRANSPORTS)?)?;
    // Devices are taken in transport order - the tablet on transport 30
    // (index 6), then the keyboard on 31 - and device N uses DMA region N
    // and ring N.
    for (slot, region) in pd.devices.iter_mut().zip(0..) {
        // Its ring to the consumer: the messages region read-write
        // (`nowayos_rw_region_N`), the control region read-only
        // (`nowayos_ro_region_N`).
        let ring = match read_write(region)
            .and_then(|messages| ring_producer(messages, read_only(region)?))
        {
            Ok(ring) => ring,
            Err(error) => {
                debug_println!(
                    "NOWAYOS: virtio-input: gave up: ring {region} to the consumer: {error}"
                );
                return Ok(());
            }
        };
        let mut mmio = page.take(DEVICE_INPUT)?;
        let accepted = mmio.negotiate(0)?;
        if accepted != F_VERSION_1 {
            debug_println!("NOWAYOS: virtio-input: gave up: features {accepted:#x} accepted");
            return Ok(());
        }
        let Some(&(_, irq)) = IRQS.iter().find(|(index, _)| *index == mmio.index()) else {
            debug_println!(
                "NOWAYOS: virtio-input: gave up: a device on transport {} has no interrupt",
                mmio.index()
            );
            return Ok(());
        };
        let mut events = RxQueue::<EVENTS>::new(dma_region(region)?, EVENT_LEN)?;
        mmio.set_up_queue(EVENT_QUEUE, &events)?;
        mmio.driver_ok();
        events.offer_all()?;
        mmio.notify(EVENT_QUEUE);
        *slot = Some(InputDevice {
            mmio,
            events,
            translator: Translator::new(width, height),
            irq,
            ring,
        });
    }
    let [Some(first), Some(second)] = &pd.devices else {
        return Ok(());
    };
    debug_println!(
        "NOWAYOS: virtio-input: {DEVICES} input devices ready, on transports {} and {}",
        first.mmio.index(),
        second.mmio.index()
    );
    Ok(())
}

/// The driver: its devices, each with its ring to the consumer, and what it
/// has dropped.
pub struct VirtioInputPd {
    devices: [Option<InputDevice>; DEVICES],
    /// Events that did not fit their ring.
    ring_full: u32,
    /// The drop counts last reported: translation, ring full, queue.
    reported: (DropCounts, u32, u32),
}

impl VirtioInputPd {
    /// Drains one device's event queue: each event copied out, translated
    /// and sent; the queue's buffers offered back.
    fn drain(&mut self, which: usize) -> Result<bool, HwError> {
        let Self {
            devices, ring_full, ..
        } = self;
        let Some(Some(device)) = devices.get_mut(which) else {
            return Ok(false);
        };
        device.mmio.acknowledge_interrupt();
        let Ok(mut tx) = Producer::new(&device.ring) else {
            return Ok(false);
        };
        let mut sent = false;
        let mut event = [0u8; EVENT_LEN];
        while let Some(len) = device.events.take(&mut event)? {
            if len != EVENT_LEN {
                continue;
            }
            device.translator.translate(&event, |canonical| {
                if tx.try_send::<InputEvent>(&canonical).is_ok() {
                    sent = true;
                } else {
                    *ring_full = ring_full.saturating_add(1);
                }
            });
        }
        device.mmio.notify(EVENT_QUEUE);
        Ok(sent)
    }

    /// Reports the drop counts if they changed.
    fn report(&mut self) {
        let mut dropped = DropCounts::default();
        let mut rejected = 0u32;
        for device in self.devices.iter().flatten() {
            let counts = device.translator.dropped();
            dropped.unknown_type = dropped.unknown_type.saturating_add(counts.unknown_type);
            dropped.unknown_code = dropped.unknown_code.saturating_add(counts.unknown_code);
            dropped.out_of_range = dropped.out_of_range.saturating_add(counts.out_of_range);
            rejected = rejected.saturating_add(device.events.rejected());
        }
        let now = (dropped, self.ring_full, rejected);
        if now != self.reported {
            self.reported = now;
            debug_println!(
                "NOWAYOS: virtio-input: dropped so far: {} unknown type, {} unknown code, {} out of range, {} ring full, {} malformed queue entries",
                dropped.unknown_type,
                dropped.unknown_code,
                dropped.out_of_range,
                self.ring_full,
                rejected
            );
        }
    }
}

impl Handler for VirtioInputPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let mut sent = false;
        for which in 0..DEVICES {
            let Some(Some(device)) = self.devices.get(which) else {
                continue;
            };
            let irq = device.irq;
            if !channels.contains(irq) {
                continue;
            }
            match self.drain(which) {
                Ok(true) => sent = true,
                Ok(false) => {}
                Err(error) => debug_println!("NOWAYOS: virtio-input: gave up on a device: {error}"),
            }
            if irq.irq_ack().is_err() {
                debug_println!("NOWAYOS: virtio-input: could not acknowledge an interrupt");
            }
        }
        self.report();
        if sent {
            CONSUMER.notify();
        }
        Ok(())
    }
}
