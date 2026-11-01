// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-input-script`: an input PD that plays a script.
//!
//! It sends the clickable demo's session (`nowayos-demo`'s `SESSION`) to the
//! UI PD as canonical `InputEvent` records, one step at a time: each time
//! the UI says it is ready - its last frame has been shown - the next step's
//! events go into the ring, and the UI is notified. Step 0 has no input: it
//! is the UI's first frame. Before step [`MALFORMED_AT`]'s events it sends
//! one deliberately malformed record (a valid header, an event tag no event
//! has), which the UI must reject and get past.
//!
//! It stands in for a real input PD, so its role is `input`; it holds no
//! device. No `unsafe` code.

#![no_std]
#![no_main]

use nowayos_demo::SESSION;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ipc::{HEADER_LEN, Producer, encode};
use nowayos_region::{RingTx, read_only, read_write, ring_producer};
use nowayos_ui_loop::{INPUT_SLOT, INPUT_SLOTS};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The channel to the UI PD.
const UI: Channel = Channel::new(0);

/// The step before whose events the malformed record goes.
const MALFORMED_AT: usize = 3;

/// An event tag no `InputEvent` has.
const MALFORMED_TAG: u8 = 0x7F;

#[protection_domain]
fn init() -> InputPd {
    debug_println!(
        "NOWAYOS: input-script: starting; {} steps to play",
        SESSION.len()
    );
    let ring = match read_write(0).and_then(|messages| ring_producer(messages, read_only(0)?)) {
        Ok(ring) => Some(ring),
        Err(error) => {
            debug_println!("NOWAYOS: input-script: gave up: {error}");
            None
        }
    };
    InputPd { ring, next: 1 }
}

/// The input PD: its ring to the UI (`None` once it has given up) and the
/// next step to send.
struct InputPd {
    ring: Option<RingTx<INPUT_SLOTS, INPUT_SLOT>>,
    next: usize,
}

impl InputPd {
    /// Sends step `self.next`'s events, then notifies the UI.
    fn send_step(&mut self) -> Result<(), &'static str> {
        let ring = self.ring.as_ref().ok_or("no ring")?;
        let Some(step) = SESSION.get(self.next) else {
            debug_println!("NOWAYOS: input-script: session done");
            return Ok(());
        };
        let mut tx = Producer::new(ring).map_err(|_| "the ring to the UI is broken")?;
        if self.next == MALFORMED_AT {
            tx.try_send_bytes(&malformed()?)
                .map_err(|_| "could not send the malformed record")?;
        }
        for event in step.events {
            tx.try_send(event)
                .map_err(|_| "a step does not fit the ring")?;
        }
        self.next = self.next.saturating_add(1);
        UI.notify();
        Ok(())
    }
}

/// A well-formed header announcing an input event, followed by a payload
/// whose event tag is [`MALFORMED_TAG`].
fn malformed() -> Result<[u8; INPUT_SLOT], &'static str> {
    let mut bytes = [0_u8; INPUT_SLOT];
    encode(&InputEvent::KeyDown(KeyCode::A), &mut bytes).map_err(|_| "could not encode")?;
    let tag = bytes.get_mut(HEADER_LEN).ok_or("record too short")?;
    *tag = MALFORMED_TAG;
    Ok(bytes)
}

impl Handler for InputPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(UI)
            && self.ring.is_some()
            && let Err(why) = self.send_step()
        {
            debug_println!("NOWAYOS: input-script: gave up: {why}");
            self.ring = None;
        }
        Ok(())
    }
}
