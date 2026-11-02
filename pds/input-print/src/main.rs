// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-input-print`: a test instrument standing in for the UI. It
//! receives canonical input events through shared-memory rings - each
//! ring's messages region read-only here - and prints each on the debug
//! console, so that `make qemu-test` can check what an input driver
//! or the input broker sends.
//!
//! Which rings it reads depends on its PD name. Run as `ui`, in the UI's
//! place beside the input broker, it reads the UI's input ring
//! (`nowayos_input::broker::UI_RING_SLOTS` slots). Otherwise it reads a
//! driver's rings directly - as many as its system grants, up to [`RINGS`].
//! It drains them in order on any notification. Every record is copied out
//! and validated by `nowayos-ipc` before it is looked at; one that fails is
//! reported and consumed. Test-only: never in a product image. No `unsafe`,
//! no heap, no devices.

#![no_std]
#![no_main]

use nowayos_input::broker::{UI_RING_SLOT, UI_RING_SLOTS};
use nowayos_input::{InputEvent, TouchPhase};
use nowayos_ipc::{Consumer, ConsumerMemory, RecvError};
use nowayos_region::{RegionError, RingRx, read_only, read_write, ring_consumer};
use sel4_microkit::{ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain};

/// Each ring from a driver: the driver's slots and size.
const RING_SLOTS: usize = 4;
const RING_SLOT: usize = 32;

/// The most rings: one for each pair of regions `nowayos-region` binds.
const RINGS: usize = 4;

#[protection_domain]
fn init() -> PrintPd {
    let mut pd = PrintPd {
        ui: None,
        drivers: [None, None, None, None],
    };
    // Ring N: its control region read-write (`nowayos_rw_region_N`), its
    // messages region read-only (`nowayos_ro_region_N`).
    if matches!(pd_name(), Ok("ui")) {
        match read_write(0).and_then(|control| ring_consumer(control, read_only(0)?)) {
            Ok(ring) => pd.ui = Some(ring),
            Err(error) => debug_println!("NOWAYOS: input-print: gave up: the UI's ring: {error}"),
        }
        return pd;
    }
    // A driver's rings: the first region not granted ends the list.
    for (index, slot) in pd.drivers.iter_mut().enumerate() {
        match read_write(index).and_then(|control| ring_consumer(control, read_only(index)?)) {
            Ok(ring) => *slot = Some(ring),
            Err(RegionError::NotMapped { .. }) if index > 0 => break,
            Err(error) => {
                debug_println!("NOWAYOS: input-print: gave up: ring {index}: {error}");
                break;
            }
        }
    }
    pd
}

/// The consumer: its end of the UI's input ring, or of each driver ring
/// (`None` if not granted or failed).
struct PrintPd {
    ui: Option<RingRx<UI_RING_SLOTS, UI_RING_SLOT>>,
    drivers: [Option<RingRx<RING_SLOTS, RING_SLOT>>; RINGS],
}

impl Handler for PrintPd {
    type Error = Infallible;

    fn notified(&mut self, _channels: ChannelSet) -> Result<(), Self::Error> {
        drain(0, &mut self.ui);
        for (index, slot) in self.drivers.iter_mut().enumerate() {
            drain(index, slot);
        }
        Ok(())
    }
}

/// Prints every event waiting in ring `index`; gives the ring up if it is
/// broken.
fn drain<M: ConsumerMemory<SLOT>, const SLOT: usize>(index: usize, slot: &mut Option<M>) {
    let Some(memory) = slot.as_ref() else {
        return;
    };
    let Ok(mut rx) = Consumer::new(memory) else {
        debug_println!("NOWAYOS: input-print: gave up: ring {index} is broken");
        *slot = None;
        return;
    };
    loop {
        match rx.try_recv::<InputEvent>() {
            Ok(Some(event)) => print(&event),
            Ok(None) => break,
            Err(RecvError::Decode(error)) => {
                debug_println!("NOWAYOS: input-print: refused a record: {error}");
            }
            // Nothing was consumed: the ring is unusable.
            Err(error) => {
                debug_println!("NOWAYOS: input-print: gave up: ring {index}: {error}");
                *slot = None;
                break;
            }
        }
    }
}

fn print(event: &InputEvent) {
    match event {
        InputEvent::KeyDown(key) => {
            debug_println!("NOWAYOS: input-print: key down {:#04x}", key.usage());
        }
        InputEvent::KeyUp(key) => {
            debug_println!("NOWAYOS: input-print: key up {:#04x}", key.usage())
        }
        InputEvent::PointerMove { x, y } => {
            debug_println!("NOWAYOS: input-print: pointer at ({x}, {y})");
        }
        InputEvent::PointerButton { button, down } => debug_println!(
            "NOWAYOS: input-print: button {button} {}",
            if *down { "down" } else { "up" }
        ),
        InputEvent::Touch { id, x, y, phase } => {
            let phase = match phase {
                TouchPhase::Start => "start",
                TouchPhase::Move => "move",
                TouchPhase::End => "end",
                TouchPhase::Cancel => "cancel",
            };
            debug_println!("NOWAYOS: input-print: touch {id} {phase} at ({x}, {y})");
        }
        InputEvent::Wheel { notches } => {
            debug_println!("NOWAYOS: input-print: wheel {notches:+}");
        }
    }
}
