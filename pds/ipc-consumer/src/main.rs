// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-ipc-consumer`: the receiving side of the IPC demonstration
//! (`systems/ipc-demo.system`).
//!
//! Stands in for a UI PD receiving canonical input events. On each
//! notification from the producer it drains the ring: every record is
//! **copied** out of shared memory into private memory before anything looks
//! at it, then validated (header, kind, version, length, every payload field,
//! screen limits) and decoded, and reported on the debug console. A malformed
//! record is rejected, reported and consumed, so it cannot wedge the queue.
//! When the ring is empty the consumer notifies the producer, which sends
//! the next batch.
//!
//! It prints only while it holds the turn: the producer, above it,
//! prints first and hands the turn over with its notification, so this PD's
//! start-up line waits for the first one. A real UART takes about 8 ms a
//! line at 115200 baud, longer than the producer's budget allows it at a
//! stretch, and the kernel's debug output is not whole lines across PDs: had
//! this PD printed at start-up, while the producer was held mid-line by its
//! budget, their characters would interleave (they did, on the Pi 5).
//!
//! Capabilities (from the system description): the ring's messages region
//! **read-only** — this PD cannot change a record the producer wrote — its
//! control region read-write, and one channel to the producer. No `unsafe`,
//! no heap, no devices.

#![no_std]
#![no_main]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use nowayos_input::InputLimits;
use nowayos_ipc::Consumer;
use nowayos_ipc_demo::{
    CONSUMER_TAG, RING_SLOT, RING_SLOTS, Tally, receive, report, screen_limits,
};
use nowayos_region::{RegionError, RingRx, read_only, read_write, ring_consumer};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The channel to the producer (`id` of this PD's `<end>` in the system
/// description).
const PRODUCER: Channel = Channel::new(0);

/// This PD's end of the ring.
type Ring = RingRx<RING_SLOTS, RING_SLOT>;

#[protection_domain]
fn init() -> ConsumerPd {
    let mut pd = ConsumerPd {
        ring: None,
        limits: screen_limits(),
        received: 0,
        tally: Tally::default(),
        announced: false,
    };
    // Nothing is printed here unless the PD cannot start: its start-up line
    // waits for the turn (`announce`). A failure is printed at once - the
    // turn may never come - after the start-up line it would have followed.
    match bind_ring() {
        Ok(ring) => pd.ring = Some(ring),
        Err(err) => {
            pd.announce();
            debug_println!("{CONSUMER_TAG} cannot start: {err}");
        }
    }
    if pd.limits.is_none() {
        pd.announce();
        debug_println!("{CONSUMER_TAG} cannot start: invalid screen limits");
        pd.ring = None;
    }
    pd
}

/// Binds the ring: control region read-write (`nowayos_rw_region_0`),
/// messages region read-only (`nowayos_ro_region_0`).
fn bind_ring() -> Result<Ring, RegionError> {
    ring_consumer(read_write(0)?, read_only(0)?)
}

/// The consumer's state between notifications.
struct ConsumerPd {
    /// `None` if the ring could not be bound, or failed: nothing more is
    /// received.
    ring: Option<Ring>,
    /// The screen and device limits every event is validated against.
    limits: Option<InputLimits>,
    /// Records received so far, valid or not.
    received: u32,
    /// Records accepted and rejected so far.
    tally: Tally,
    /// Whether the start-up line has been printed.
    announced: bool,
}

impl ConsumerPd {
    /// Prints the start-up line, once.
    fn announce(&mut self) {
        if !self.announced {
            self.announced = true;
            debug_println!(
                "{CONSUMER_TAG} starting; ring of {RING_SLOTS} slots of {RING_SLOT} bytes, messages region read-only here"
            );
        }
    }

    /// Receives until the ring is empty, then tells the producer.
    fn drain(&mut self) {
        let Self {
            ring,
            limits,
            received,
            tally,
            ..
        } = self;
        let (Some(memory), Some(limits)) = (ring.as_ref(), limits.as_ref()) else {
            return;
        };
        // A fresh Consumer each time: it re-reads our own index from the
        // control region, which only this PD can write, and checks the
        // producer's index against it before touching any slot.
        let mut rx = match Consumer::new(memory) {
            Ok(rx) => rx,
            Err(err) => {
                debug_println!("{CONSUMER_TAG} ring failed: {err}; stopping");
                *ring = None;
                return;
            }
        };
        loop {
            // The copy: `record` is private memory; the producer cannot
            // change it while it is validated and decoded.
            match rx.try_recv_record() {
                Ok(Some(record)) => {
                    let outcome = receive(&record, limits);
                    tally.count(&outcome);
                    debug_println!("{CONSUMER_TAG} {}", report(*received, &outcome, tally));
                    *received = received.saturating_add(1);
                }
                Ok(None) => break,
                Err(err) => {
                    debug_println!("{CONSUMER_TAG} ring failed: {err}; stopping");
                    *ring = None;
                    return;
                }
            }
        }
        // Drained: the producer may continue.
        PRODUCER.notify();
    }
}

impl Handler for ConsumerPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        // The producer's notification hands this PD the turn: the producer
        // has printed its lines and waits.
        self.announce();
        if channels.contains(PRODUCER) {
            self.drain();
        } else {
            debug_println!(
                "{CONSUMER_TAG} ignoring notification on unexpected channels {}",
                channels.display()
            );
        }
        Ok(())
    }
}
