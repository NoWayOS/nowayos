// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-seed-consumer`: a test instrument, the entropy PD's one
//! consumer in `systems/entropy-test.system` and `entropy-test-no-rng`. It
//! stands in for a PD that makes keys or TLS handshakes:
//!
//! 1. Before its first seed it asks its generator (`nowayos-entropy`) for
//!    bytes, and is refused: it is not ready (rule 4).
//! 2. It takes a seed off its ring (`nowayos-seed`'s `take`, which moves its
//!    index past the slot before decoding), keys its generator with it, and
//!    checks that both copies of the seed it held are wiped.
//! 3. It draws 32 bytes and prints their SHA-256, never the bytes, then
//!    notifies the entropy PD that it has taken a seed.
//! 4. When the entropy PD next notifies, it checks the slot it took no
//!    longer holds what it took: the entropy PD has overwritten it. It
//!    compares the slot's SHA-256 then and now, and keeps nothing else of it.
//!
//! With no seed on its ring it says once that it is not ready, and waits.
//!
//! In `systems/entropy-rate-test.system` two more PDs run this
//! program, and each picks its script by PD name:
//!
//! * `polite_consumer` takes a seed every 200 ms - slower than its rate,
//!   `nowayos-seed`'s `SEEDS_PER_SECOND` - five times, timed by the timer
//!   PD, and must find one on its ring each time;
//! * `seed_flood` takes seeds as fast as it can for two seconds, notifying
//!   the entropy PD after every attempt, and must be held to its rate: no
//!   more than a ring's worth plus `SEEDS_PER_SECOND` a second, and at least
//!   half that rate, so its ring is refilled.
//!
//! A check that fails prints `gave up` and the reason, which the expect files
//! forbid. A test instrument: never in a demo or product system. No `unsafe`
//! code; its regions are bound through `nowayos-region`, and it reads the
//! counter through `nowayos-hw`.

#![no_std]
#![no_main]

use core::fmt;

use nowayos_bounded::Wipe;
use nowayos_entropy::Refusal;
use nowayos_hw::cpu;
use nowayos_ipc::{ConsumerMemory, RingMemory};
use nowayos_region::{RingRx, read_only, read_write, ring_consumer};
use nowayos_seed::{SEED_LEN, SEED_RING_SLOT, SEED_RING_SLOTS, SEEDS_PER_SECOND};
use nowayos_timer_api::Client;
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};
use sha2::{Digest, Sha256};

/// The channel to the entropy PD: it notifies when a seed waits, and is
/// notified when one has been taken.
const ENTROPY: Channel = Channel::new(0);

/// Its end of the seed ring: its index, read-write region 0; the seeds,
/// read-only region 0.
const SEED_CONTROL: usize = 0;
const SEED_MESSAGES: usize = 0;

/// The bytes it draws once seeded.
const DRAW: usize = 32;

/// `polite_consumer`'s timer, on channel 1, and how it spaces its takes.
const TIMER: Client = Client::new(Channel::new(1));
const POLITE_TAKES: u32 = 5;
const POLITE_GAP_NANOS: u64 = 200_000_000;

/// How long `seed_flood` takes seeds for.
const FLOOD_SECONDS: u32 = 2;

type SeedRing = RingRx<SEED_RING_SLOTS, SEED_RING_SLOT>;

/// Bytes in hexadecimal: only ever a hash here.
struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[protection_domain]
fn init() -> TestPd {
    match pd_name().unwrap_or("?") {
        "polite_consumer" => polite(),
        "seed_flood" => {
            flood();
            TestPd::Idle
        }
        _ => consumer(),
    }
}

/// Its seed ring, or why not.
fn seed_ring(tag: &str) -> Option<SeedRing> {
    match read_write(SEED_CONTROL)
        .and_then(|control| ring_consumer(control, read_only(SEED_MESSAGES)?))
    {
        Ok(ring) => Some(ring),
        Err(error) => {
            debug_println!("NOWAYOS: {tag}: gave up: its seed ring: {error}");
            None
        }
    }
}

/// Takes one seed and wipes it at once; whether one was there.
fn take_one(tag: &str, ring: &SeedRing) -> bool {
    match nowayos_seed::take(ring) {
        Ok(Some(mut seed)) => {
            seed.wipe();
            true
        }
        Ok(None) => false,
        Err(error) => {
            debug_println!("NOWAYOS: {tag}: a seed refused: {error}");
            false
        }
    }
}

/// `seed_consumer`'s script.
fn consumer() -> TestPd {
    debug_println!("NOWAYOS: seed-consumer: starting");
    let ring = seed_ring("seed-consumer");
    refused_before_seeded();
    let mut pd = ConsumerPd {
        ring,
        taken: None,
        said_waiting: false,
        checked: false,
    };
    pd.try_seed();
    TestPd::Consumer(pd)
}

/// `polite_consumer`'s start: its first seed, then the timer armed.
fn polite() -> TestPd {
    const TAG: &str = "polite-consumer";
    debug_println!("NOWAYOS: {TAG}: starting");
    let Some(ring) = seed_ring(TAG) else {
        return TestPd::Idle;
    };
    let mut pd = Polite { ring, taken: 0 };
    pd.take();
    TestPd::Polite(pd)
}

/// `polite_consumer`: a seed every 200 ms.
struct Polite {
    ring: SeedRing,
    taken: u32,
}

impl Polite {
    /// Takes a seed, as it asks for one; the next is asked for in 200 ms.
    fn take(&mut self) {
        if !take_one("polite-consumer", &self.ring) {
            debug_println!(
                "NOWAYOS: polite-consumer: gave up: no seed on its ring when it asked, within its rate"
            );
            return;
        }
        self.taken = self.taken.saturating_add(1);
        ENTROPY.notify();
        if self.taken >= POLITE_TAKES {
            debug_println!(
                "NOWAYOS: polite-consumer: got every seed it asked for: {POLITE_TAKES}, one each 200 ms"
            );
        } else if TIMER.set_timeout(1, POLITE_GAP_NANOS).is_err() {
            debug_println!("NOWAYOS: polite-consumer: gave up: the timer refused a timeout");
        }
    }
}

/// `seed_flood`'s script: as many seeds as it can take in
/// [`FLOOD_SECONDS`], then whether it was held to its rate.
fn flood() {
    const TAG: &str = "seed-flood";
    debug_println!("NOWAYOS: {TAG}: starting");
    let Some(ring) = seed_ring(TAG) else {
        return;
    };
    let seconds = u64::from(FLOOD_SECONDS);
    let end = cpu::counter().saturating_add(cpu::counter_frequency().saturating_mul(seconds));
    let mut taken = 0u32;
    while cpu::counter() < end {
        if take_one(TAG, &ring) {
            taken = taken.saturating_add(1);
        }
        ENTROPY.notify();
    }
    let ring_full = u32::try_from(SEED_RING_SLOTS).unwrap_or(u32::MAX);
    let rate = SEEDS_PER_SECOND.saturating_mul(FLOOD_SECONDS);
    // One more than the rate for a seed earned as the time runs out.
    let most = ring_full.saturating_add(rate).saturating_add(1);
    let least = ring_full.saturating_add(rate / 2);
    debug_println!("NOWAYOS: {TAG}: took {taken} seeds in {FLOOD_SECONDS} s");
    if taken > most {
        debug_println!("NOWAYOS: {TAG}: gave up: more than its rate allows ({most})");
    } else if taken < least {
        debug_println!("NOWAYOS: {TAG}: gave up: its ring was not refilled at its rate ({least})");
    } else {
        debug_println!(
            "NOWAYOS: {TAG}: held to its rate: a ring's worth, then {SEEDS_PER_SECOND} seeds a second"
        );
    }
}

/// The test PD, as Microkit runs it: by its PD name's script.
enum TestPd {
    Consumer(ConsumerPd),
    Polite(Polite),
    Idle,
}

impl Handler for TestPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        match self {
            Self::Consumer(pd) => pd.notified(channels),
            Self::Polite(pd) => {
                if channels.contains(TIMER.channel()) {
                    loop {
                        match TIMER.expired() {
                            Ok(Some(_)) => pd.take(),
                            Ok(None) => break,
                            Err(_) => {
                                debug_println!(
                                    "NOWAYOS: polite-consumer: gave up: the timer refused a request"
                                );
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
            Self::Idle => Ok(()),
        }
    }
}

/// Asks for bytes before the first seed: refused, and nothing written.
fn refused_before_seeded() {
    let mut draw = [0u8; DRAW];
    match nowayos_entropy::fill(&mut draw) {
        Err(Refusal::NotSeeded) if draw == [0; DRAW] => {
            debug_println!(
                "NOWAYOS: seed-consumer: not ready: a draw before its first seed is refused"
            );
        }
        Err(refusal) => debug_println!(
            "NOWAYOS: seed-consumer: gave up: a draw before its first seed was refused as {refusal:?}, or written to"
        ),
        Ok(()) => {
            draw.wipe();
            debug_println!("NOWAYOS: seed-consumer: gave up: it drew before it was seeded");
        }
    }
}

/// The SHA-256 of a slot of the ring, as it is now.
fn slot_digest(ring: &SeedRing, slot: u32) -> [u8; 32] {
    let mut bytes = [0u8; SEED_RING_SLOT];
    ring.read_slot(slot, &mut bytes);
    let digest = Sha256::digest(bytes).into();
    bytes.wipe();
    digest
}

/// The consumer, as Microkit runs it.
struct ConsumerPd {
    ring: Option<SeedRing>,
    /// The slot it took its seed from, and that slot's SHA-256 then.
    taken: Option<(u32, [u8; 32])>,
    said_waiting: bool,
    /// Whether the slot it took has been checked.
    checked: bool,
}

impl ConsumerPd {
    /// Takes a seed if one waits and none has been taken: keys the
    /// generator, checks the copies are wiped, draws and notifies.
    fn try_seed(&mut self) {
        if self.taken.is_some() {
            return;
        }
        let Some(ring) = self.ring.as_ref() else {
            return;
        };
        let Some(mask) = ring.capacity().checked_sub(1) else {
            return;
        };
        let slot = ring.load_consumer_index() & mask;
        let before = slot_digest(ring, slot);
        let mut seed = match nowayos_seed::take(ring) {
            Ok(Some(seed)) => seed,
            Ok(None) => {
                if !self.said_waiting {
                    self.said_waiting = true;
                    debug_println!("NOWAYOS: seed-consumer: not ready: no seed on its ring");
                }
                return;
            }
            Err(error) => {
                debug_println!("NOWAYOS: seed-consumer: a seed refused: {error}");
                return;
            }
        };
        let mut bytes: [u8; SEED_LEN] = *seed.as_bytes();
        let keyed = nowayos_entropy::seed(&mut bytes);
        seed.wipe();
        match keyed {
            Ok(()) if bytes == [0; SEED_LEN] && seed.as_bytes() == &[0; SEED_LEN] => {
                debug_println!(
                    "NOWAYOS: seed-consumer: seeded: its generator keyed, and its copies of the seed wiped"
                )
            }
            Ok(()) => {
                bytes.wipe();
                debug_println!("NOWAYOS: seed-consumer: gave up: a copy of its seed was not wiped");
            }
            Err(refusal) => {
                bytes.wipe();
                debug_println!(
                    "NOWAYOS: seed-consumer: gave up: its generator refused the seed: {refusal:?}"
                );
                return;
            }
        }
        let mut draw = [0u8; DRAW];
        match nowayos_entropy::fill(&mut draw) {
            Ok(()) => {
                let digest: [u8; 32] = Sha256::digest(draw).into();
                draw.wipe();
                debug_println!(
                    "NOWAYOS: seed-consumer: drew {DRAW} bytes; their SHA-256 follows, never the bytes"
                );
                debug_println!("NOWAYOS: seed-consumer: {}", Hex(&digest));
            }
            Err(refusal) => {
                debug_println!(
                    "NOWAYOS: seed-consumer: gave up: seeded, a draw was refused: {refusal:?}"
                );
            }
        }
        self.taken = Some((slot, before));
        ENTROPY.notify();
    }

    /// Whether the slot it took still holds what it took.
    fn check_taken_slot(&mut self) {
        let (Some(ring), Some((slot, before))) = (self.ring.as_ref(), self.taken) else {
            return;
        };
        if self.checked {
            return;
        }
        self.checked = true;
        if slot_digest(ring, slot) == before {
            debug_println!(
                "NOWAYOS: seed-consumer: gave up: the slot it took still holds its seed"
            );
        } else {
            debug_println!("NOWAYOS: seed-consumer: the slot it took no longer holds its seed");
        }
    }
}

impl ConsumerPd {
    fn notified(&mut self, channels: ChannelSet) -> Result<(), Infallible> {
        if channels.contains(ENTROPY) {
            if self.taken.is_some() {
                self.check_taken_slot();
            } else {
                self.try_seed();
            }
        }
        Ok(())
    }
}
