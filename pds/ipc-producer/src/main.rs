// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-ipc-producer`: the sending side of the IPC demonstration
//! (`systems/ipc-demo.system`).
//!
//! Stands in for an input PD: it sends the fixed synthetic sequence in
//! `nowayos-ipc-demo` through a `nowayos-ipc` ring in shared memory and
//! notifies the consumer. The ring holds four records, so the producer sends
//! until the ring is full, then waits: the consumer notifies back once it
//! has drained the ring, and the producer continues.
//!
//! It prints its lines before it notifies the consumer, which prints only
//! once notified: the notification hands over the turn, so the two
//! never print at once, even when this PD's budget holds it mid-line on a
//! real UART (about 8 ms a line at 115200 baud).
//!
//! Capabilities (from the system description): the ring's messages region
//! read-write, its control region read-only, and one channel to the
//! consumer. No `unsafe` (the regions are bound through `nowayos-region`),
//! no heap, no devices.

#![no_std]
#![no_main]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use nowayos_ipc::{Producer, SendError};
use nowayos_ipc_demo::{PRODUCER_TAG, RING_SLOT, RING_SLOTS, SEQUENCE, send};
use nowayos_region::{RegionError, RingTx, read_only, read_write, ring_producer};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The channel to the consumer (`id` of this PD's `<end>` in the system
/// description).
const CONSUMER: Channel = Channel::new(0);

/// This PD's end of the ring.
type Ring = RingTx<RING_SLOTS, RING_SLOT>;

#[protection_domain]
fn init() -> ProducerPd {
    debug_println!(
        "{PRODUCER_TAG} starting; ring of {RING_SLOTS} slots of {RING_SLOT} bytes, messages region read-write here"
    );
    let mut pd = ProducerPd {
        ring: None,
        next: 0,
    };
    match bind_ring() {
        Ok(ring) => {
            pd.ring = Some(ring);
            pd.send_batch();
        }
        Err(err) => debug_println!("{PRODUCER_TAG} cannot start: {err}"),
    }
    pd
}

/// Binds the ring: messages region read-write (`nowayos_rw_region_0`),
/// control region read-only (`nowayos_ro_region_0`).
fn bind_ring() -> Result<Ring, RegionError> {
    ring_producer(read_write(0)?, read_only(0)?)
}

/// The producer's state between notifications.
struct ProducerPd {
    /// `None` if the ring could not be bound, or failed: nothing more is
    /// sent.
    ring: Option<Ring>,
    /// Index in `SEQUENCE` of the next record to send.
    next: usize,
}

impl ProducerPd {
    /// Sends records until the ring is full or the sequence is done, then
    /// notifies the consumer if anything was sent.
    fn send_batch(&mut self) {
        let Self { ring, next } = self;
        let Some(memory) = ring.as_ref() else {
            return;
        };
        if *next >= SEQUENCE.len() {
            return;
        }
        // A fresh Producer each time: it re-reads our own index from the
        // messages region, which only this PD can write.
        let mut tx = match Producer::new(memory) {
            Ok(tx) => tx,
            Err(err) => {
                debug_println!("{PRODUCER_TAG} ring failed: {err}; stopping");
                *ring = None;
                return;
            }
        };
        let first = *next;
        while let Some(record) = SEQUENCE.get(*next) {
            match send(&mut tx, record) {
                Ok(()) => *next = next.saturating_add(1),
                Err(SendError::Full) => break,
                Err(err) => {
                    debug_println!("{PRODUCER_TAG} send failed: {err}; stopping");
                    *ring = None;
                    break;
                }
            }
        }
        if *next > first {
            debug_println!(
                "{PRODUCER_TAG} sent records {first}..={}",
                next.saturating_sub(1)
            );
            if *next == SEQUENCE.len() {
                debug_println!("{PRODUCER_TAG} all {} records sent", SEQUENCE.len());
            }
            // Printed first, then the turn handed over: the consumer prints
            // only once notified.
            CONSUMER.notify();
        }
    }
}

impl Handler for ProducerPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(CONSUMER) {
            // The consumer has drained the ring: carry on.
            self.send_batch();
        } else {
            debug_println!(
                "{PRODUCER_TAG} ignoring notification on unexpected channels {}",
                channels.display()
            );
        }
        Ok(())
    }
}
