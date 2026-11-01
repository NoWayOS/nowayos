// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-isolation-bystander`: the PD that must keep running in NoWayOS's
//! isolation tests (`systems/isolation-test.system`, as `display_side` in
//! `systems/ui-isolation.system`, as `broker_side` in
//! `systems/input-isolation.system` and as `driver_side` in
//! `systems/net-isolation.system`).
//!
//! It owns a private page (mapped read-write in this PD only:
//! `bystander_private` at `0x4000_0000` in `isolation-test`,
//! `display_private` at `0x6000_0000` in `ui-isolation`, `driver_ring` at
//! `0x4100_0000` in `input-isolation`, `driver_dma` at `0x300_0000` in
//! `net-isolation`) and writes a marker into it. A probe
//! tries to read or write that same virtual address in its own address
//! space, where nothing is mapped. After each probe fault the parent
//! notifies this PD; it checks that its marker is intact and answers. An
//! answer shows that the fault stopped only the probe: this PD, and its
//! memory, carried on.
//!
//! Capabilities: its private page, and one channel to the parent. No
//! `unsafe` (the page is bound through `nowayos-region`), no heap.

#![no_std]
#![no_main]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use nowayos_region::{ReadWrite, RegionError, Words, read_write};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: isolation-bystander:";

/// The channel to the parent.
const PARENT: Channel = Channel::new(0);

/// A synthetic marker kept in word 0 of the private page.
const MARKER: u32 = 0x5AFE_C0DE;

#[protection_domain]
fn init() -> BystanderPd {
    let page = match bind_page() {
        Ok((vaddr, page)) => {
            page.store(0, MARKER);
            debug_println!(
                "{TAG} starting; private page at {vaddr:#x} holds {:#010x}",
                page.load(0).unwrap_or(0)
            );
            Some(page)
        }
        Err(err) => {
            debug_println!("{TAG} cannot start: {err}");
            None
        }
    };
    BystanderPd { page, checks: 0 }
}

/// Binds the private page (`nowayos_rw_region_0`); returns its address and
/// its words.
fn bind_page() -> Result<(usize, Words<ReadWrite>), RegionError> {
    let region = read_write(0)?;
    let vaddr = region.vaddr();
    Ok((vaddr, region.into_words()?))
}

/// The bystander's state between notifications.
struct BystanderPd {
    /// The private page, if it could be bound.
    page: Option<Words<ReadWrite>>,
    /// Liveness checks answered.
    checks: u32,
}

impl Handler for BystanderPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(PARENT) {
            self.checks = self.checks.saturating_add(1);
            match self.page.as_ref().and_then(|page| page.load(0)) {
                Some(MARKER) => debug_println!(
                    "{TAG} still running (check {}); private page intact",
                    self.checks
                ),
                Some(other) => debug_println!(
                    "{TAG} still running (check {}); private page CHANGED to {other:#010x}",
                    self.checks
                ),
                None => debug_println!(
                    "{TAG} still running (check {}); no private page",
                    self.checks
                ),
            }
            PARENT.notify();
        } else {
            debug_println!(
                "{TAG} ignoring notification on unexpected channels {}",
                channels.display()
            );
        }
        Ok(())
    }
}
