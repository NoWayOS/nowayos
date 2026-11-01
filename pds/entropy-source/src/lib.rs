// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-entropy-source`: the entropy PD. It
//! alone owns the device's random source - in QEMU, virtio-rng on transport
//! 8, fed from the host's `/dev/urandom`, the page the board file names
//! `rng` - and turns what it reads into seeds for up to [`MAX_CONSUMERS`]
//! consumers:
//!
//! 1. Every byte read passes SP 800-90B's two continuous health tests, and
//!    the first 1,024 are tested and discarded (`nowayos-rng-health`'s
//!    [`Conditioner`]). Each [`Entropy::bytes_per_seed`]
//!    tested bytes after them become one 256-bit seed, SHA-256 over them.
//! 2. Seeds travel one way, on each consumer's own seed ring: this PD writes
//!    its messages region, the consumer only its index (`nowayos-seed`'s
//!    [`give`], rule 3). Each ring is filled at start, then refilled at no
//!    more than [`SEEDS_PER_SECOND`], measured on the counter: a consumer
//!    that takes seeds as fast as it can drains neither the source nor the
//!    others' rings. One seed made while no ring may take it is held for the
//!    next; any more are wiped.
//! 3. When a consumer has taken a seed and notifies, the slot it took is
//!    overwritten ([`overwrite_taken`]), so no taken seed lingers on a ring,
//!    and its ring is refilled as its rate allows.
//! 4. A failed health test latches: no seed is handed out again, the test
//!    that failed is reported (never the bytes), and the PD faults on
//!    purpose (rule 6), so that its supervisor stops it and resets the
//!    device, and every PD starts again and takes fresh seeds.
//!
//! At start, before any PD it seeds can run (it runs above them), it polls
//! the device until the start-up tests are over and every ring is full, so
//! each consumer finds a seed waiting; after that it works on the device's
//! interrupt. With no device on transport 8 it hands out nothing, and its
//! consumers stay not ready (rule 4).
//!
//! It keeps no seed it has handed out, holds no key, calls no PD and has no
//! channel to a `ui` PD (the grants check's `entropy` role). Written from
//! OASIS VIRTIO 1.2 section 5.4 only: one queue, `requestq`, of
//! device-writable buffers the device fills with random bytes. Everything
//! raw is in `nowayos-hw`; this PD has no `unsafe` code. QEMU only for now:
//! the Pi 5's RNG200 comes with its driver (`needs-pi`).

#![no_std]

use nowayos_bounded::Wipe;
use nowayos_hw::virtio::{DEVICE_ENTROPY, F_VERSION_1, VirtioMmio, VirtioPage};
use nowayos_hw::virtqueue::RxQueue;
use nowayos_hw::{HwError, cpu, device, dma_region};
use nowayos_ipc::{Producer, RingMemory};
use nowayos_region::{RingTx, read_only, read_write, ring_producer};
use nowayos_rng_health::{Conditioner, Entropy, Failure, STARTUP_BYTES};
use nowayos_seed::{
    SEED_LEN, SEED_RING_SLOT, SEED_RING_SLOTS, SEEDS_PER_SECOND, Seed, give, overwrite_taken,
};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The device number the system binds the source's page to: the page the
/// board file names `rng`.
const RNG: usize = 0;

/// The DMA region its request queue and buffers live in.
const QUEUE_REGION: usize = 0;

/// The source's transport, as an index in its page: transport 8, the
/// page's first.
const TRANSPORT_8: usize = 0;

/// virtio-rng's one queue, `requestq` (section 5.4.2), of `BUFFERS` buffers
/// of `BUFFER_LEN` bytes, each request filled with random bytes: one seed's
/// worth each at 8 bits a byte.
const REQUESTS: u32 = 0;
const BUFFERS: usize = 4;
const BUFFER_LEN: usize = 40;

/// The min-entropy claimed for the source: QEMU's virtio-rng hands on the
/// host's own generator.
const ENTROPY: Entropy = Entropy::H8;

/// The most consumers: consumer N's seed ring is read-write region N (its
/// messages) and read-only region N (its index), and it is on channel N,
/// notified when a seed waits and notifying back when it has taken one.
pub const MAX_CONSUMERS: usize = 2;

/// The source's interrupt.
const IRQ: Channel = Channel::new(8);

/// How long the start-up may wait for the source, in seconds.
const STARTUP_SECONDS: u64 = 5;

type SeedRing = RingTx<SEED_RING_SLOTS, SEED_RING_SLOT>;

/// The random source: its transport and request queue.
struct Source {
    mmio: VirtioMmio<'static>,
    requests: RxQueue<BUFFERS>,
}

/// One consumer's ring and what this PD knows of it.
struct Ring {
    ring: SeedRing,
    channel: Channel,
    /// The consumer's index when its taken seeds were last overwritten.
    mark: u32,
    /// The seeds its rate allows now, up to a ring's worth.
    allowance: u32,
    /// The counter's value the allowance was last brought up to.
    allowed_at: u64,
    said_full: bool,
    said_overwritten: bool,
}

impl Ring {
    /// The ring's free slots: none if its indices are impossible.
    fn free(&self) -> u32 {
        Producer::new(&self.ring)
            .and_then(|producer| producer.free_slots())
            .unwrap_or(0)
    }

    /// Brings the allowance up to the time: one seed each
    /// `1 / SEEDS_PER_SECOND` s since it was last, up to a ring's worth.
    fn accrue(&mut self) {
        let per_seed = cpu::counter_frequency()
            .checked_div(u64::from(SEEDS_PER_SECOND))
            .unwrap_or(0)
            .max(1);
        let now = cpu::counter();
        let earned = now
            .saturating_sub(self.allowed_at)
            .checked_div(per_seed)
            .unwrap_or(0);
        if earned == 0 {
            return;
        }
        let ring_full = u32::try_from(SEED_RING_SLOTS).unwrap_or(u32::MAX);
        self.allowance = u32::try_from(earned)
            .unwrap_or(u32::MAX)
            .saturating_add(self.allowance)
            .min(ring_full);
        self.allowed_at = self
            .allowed_at
            .saturating_add(earned.saturating_mul(per_seed));
        if self.allowance == ring_full {
            self.allowed_at = now;
        }
    }

    /// Whether it may take a seed now.
    fn wants(&mut self) -> bool {
        self.accrue();
        self.allowance > 0 && self.free() > 0
    }

    /// Puts `seed` on the ring if it wants one; whether it did.
    fn take(&mut self, seed: &Seed) -> bool {
        if !self.wants() {
            return false;
        }
        match give(&self.ring, seed) {
            Ok(true) => {
                self.allowance = self.allowance.saturating_sub(1);
                self.channel.notify();
                true
            }
            Ok(false) => false,
            Err(error) => {
                debug_println!("NOWAYOS: entropy: a seed ring: {error}");
                false
            }
        }
    }
}

/// Starts the entropy PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> EntropyPd {
    debug_println!("NOWAYOS: entropy: starting");
    let mut pd = EntropyPd {
        source: None,
        rings: [None, None],
        conditioner: Conditioner::new(ENTROPY),
        held: None,
        said_started: false,
    };
    for (index, slot) in pd.rings.iter_mut().enumerate() {
        match bind_ring(index) {
            Ok(ring) => *slot = Some(ring),
            Err(nowayos_region::RegionError::NotMapped { .. }) if index > 0 => break,
            Err(error) => {
                debug_println!("NOWAYOS: entropy: gave up: seed ring {index}: {error}");
                return pd;
            }
        }
    }
    match start() {
        Ok(source) => {
            debug_println!(
                "NOWAYOS: entropy: virtio-rng on transport 8, claimed to give {} bits a byte",
                ENTROPY.bits()
            );
            pd.source = Some(source);
        }
        Err(Start::Hw(HwError::NoVirtioDevice { .. })) => {
            debug_println!(
                "NOWAYOS: entropy: no random source on transport 8: it hands out no seeds"
            );
            return pd;
        }
        Err(Start::Hw(error)) => {
            debug_println!("NOWAYOS: entropy: gave up: the random source: {error}");
            return pd;
        }
        Err(Start::Transport(index)) => {
            debug_println!(
                "NOWAYOS: entropy: gave up: the random source is on the page's transport {index}, not transport 8"
            );
            return pd;
        }
        Err(Start::Features(accepted)) => {
            debug_println!("NOWAYOS: entropy: gave up: features {accepted:#x} accepted");
            return pd;
        }
    }
    pd.fill_at_start();
    pd
}

fn bind_ring(index: usize) -> Result<Ring, nowayos_region::RegionError> {
    let ring: SeedRing = ring_producer(read_write(index)?, read_only(index)?)?;
    let mark = ring.load_consumer_index();
    Ok(Ring {
        ring,
        channel: Channel::new(index),
        mark,
        allowance: u32::try_from(SEED_RING_SLOTS).unwrap_or(u32::MAX),
        allowed_at: cpu::counter(),
        said_full: false,
        said_overwritten: false,
    })
}

/// Why the source could not be started.
enum Start {
    Hw(HwError),
    /// It is on another transport of the page than transport 8.
    Transport(usize),
    /// The features accepted, if not only `VIRTIO_F_VERSION_1`.
    Features(u64),
}

impl From<HwError> for Start {
    fn from(error: HwError) -> Self {
        Self::Hw(error)
    }
}

/// The source set up and asked for its first bytes.
fn start() -> Result<Source, Start> {
    let mut page = VirtioPage::new(device(RNG)?)?;
    let mut mmio = page.take(DEVICE_ENTROPY)?;
    if mmio.index() != TRANSPORT_8 {
        return Err(Start::Transport(mmio.index()));
    }
    let accepted = mmio.negotiate(0)?;
    if accepted != F_VERSION_1 {
        return Err(Start::Features(accepted));
    }
    let mut requests = RxQueue::<BUFFERS>::new(dma_region(QUEUE_REGION)?, BUFFER_LEN)?;
    mmio.set_up_queue(REQUESTS, &requests)?;
    mmio.driver_ok();
    requests.offer_all()?;
    mmio.notify(REQUESTS);
    Ok(Source { mmio, requests })
}

/// The entropy PD, as Microkit runs it.
pub struct EntropyPd {
    source: Option<Source>,
    rings: [Option<Ring>; MAX_CONSUMERS],
    conditioner: Conditioner,
    /// One seed made while no ring could take it, for the next that can.
    held: Option<Seed>,
    said_started: bool,
}

impl EntropyPd {
    /// Polls the source until the start-up tests are over and every ring
    /// is full, or the source is silent for [`STARTUP_SECONDS`].
    fn fill_at_start(&mut self) {
        let wait = cpu::counter_frequency().saturating_mul(STARTUP_SECONDS);
        let deadline = cpu::counter().saturating_add(wait);
        while self.wanted() && self.source.is_some() {
            if cpu::counter() > deadline {
                debug_println!(
                    "NOWAYOS: entropy: the source gave too few bytes in {STARTUP_SECONDS} s: the rings are not full yet"
                );
                break;
            }
            if self.read_source() {
                self.ask();
            }
        }
        self.top_up();
    }

    /// Whether more seeds are wanted: none held, or a ring that may take
    /// one now.
    fn wanted(&mut self) -> bool {
        let some_ring_wants = self.rings.iter_mut().flatten().any(Ring::wants);
        self.held.is_none() || some_ring_wants
    }

    /// Asks the source for more while seeds are wanted.
    fn ask(&mut self) {
        if self.wanted()
            && let Some(source) = self.source.as_mut()
        {
            source.mmio.notify(REQUESTS);
        }
    }

    /// Copies out what the source has returned, and tests and conditions
    /// every byte; whether anything came back. A failed health test is a
    /// fault, on purpose.
    fn read_source(&mut self) -> bool {
        let Self {
            source,
            rings,
            conditioner,
            held,
            said_started,
        } = self;
        let Some(device) = source.as_mut() else {
            return false;
        };
        let mut buffer = [0u8; BUFFER_LEN];
        let mut took = false;
        let mut lost = false;
        loop {
            match device.requests.take(&mut buffer) {
                Ok(Some(len)) => {
                    took = true;
                    for &byte in buffer.get(..len).unwrap_or(&[]) {
                        match conditioner.feed(byte) {
                            Ok(Some(mut bytes)) => keep(held, rings, &mut bytes),
                            Ok(None) => {}
                            Err(failure) => {
                                buffer.wipe();
                                held.wipe();
                                fault_on_purpose(failure);
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    debug_println!("NOWAYOS: entropy: gave up on the source: {error}");
                    lost = true;
                    break;
                }
            }
        }
        buffer.wipe();
        if lost {
            *source = None;
        }
        if conditioner.started() && !*said_started {
            *said_started = true;
            debug_println!(
                "NOWAYOS: entropy: start-up tests passed over the first {STARTUP_BYTES} bytes, which were discarded"
            );
        }
        took
    }

    /// Overwrites what each consumer has taken, then refills its ring from
    /// the held seed as its rate allows.
    fn top_up(&mut self) {
        for (index, slot) in self.rings.iter_mut().enumerate() {
            let Some(ring) = slot.as_mut() else {
                continue;
            };
            let mark = overwrite_taken(&ring.ring, ring.mark);
            let overwritten = mark.wrapping_sub(ring.mark);
            ring.mark = mark;
            if overwritten > 0 && !ring.said_overwritten {
                ring.said_overwritten = true;
                debug_println!(
                    "NOWAYOS: entropy: seed ring {index}: {overwritten} taken seed(s) overwritten"
                );
            }
            if let Some(seed) = self.held.as_mut()
                && ring.take(seed)
            {
                seed.wipe();
                self.held = None;
            }
            if ring.free() == 0 && !ring.said_full {
                ring.said_full = true;
                debug_println!("NOWAYOS: entropy: seed ring {index} full: {SEED_RING_SLOTS} seeds");
            }
        }
    }
}

/// A seed just made: on the first ring that may take it, else held if none
/// is, else wiped. The bytes are wiped either way.
fn keep(held: &mut Option<Seed>, rings: &mut [Option<Ring>], bytes: &mut [u8; SEED_LEN]) {
    let seed = Seed::new(*bytes);
    bytes.wipe();
    let Some(mut seed) = seed else {
        return;
    };
    if held.is_none() && rings.iter_mut().flatten().any(|ring| ring.take(&seed)) {
        seed.wipe();
        return;
    }
    if held.is_none() {
        *held = Some(seed);
    } else {
        seed.wipe();
    }
}

/// A failed health test: reported, then a fault on purpose, so that the supervisor stops this PD, reports it and resets the
/// device. Nothing of the source's bytes is printed.
#[allow(
    clippy::panic,
    reason = "a failed health test is a fault on purpose, so that the fault policy applies; a panic reaches the supervisor as a fault, in debug and release (supervisor-panic)"
)]
fn fault_on_purpose(failure: Failure) -> ! {
    let test = match failure {
        Failure::RepetitionCount => "the repetition count test",
        Failure::AdaptiveProportion => "the adaptive proportion test",
    };
    debug_println!("NOWAYOS: entropy: health test failed: {test}: no more seeds");
    panic!("entropy: a health test failed: faulting on purpose");
}

impl Handler for EntropyPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(IRQ) {
            if let Some(source) = self.source.as_mut() {
                source.mmio.acknowledge_interrupt();
            }
            self.read_source();
            if IRQ.irq_ack().is_err() {
                debug_println!("NOWAYOS: entropy: could not acknowledge the source's interrupt");
            }
        }
        self.top_up();
        self.ask();
        Ok(())
    }
}
