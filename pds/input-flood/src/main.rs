// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-input-flood`: a test instrument standing in for a faulty or
//! hostile input driver - a touch screen's, as QEMU 7.2 has no multi-touch
//! device. At start-up it sends the input broker, through its ring, one
//! malformed record (a valid header, an event tag no event has), a pointer
//! position off the screen, a button the screen does not have, a touch
//! contact the screen does not have, one touch - its start and end - and
//! then [`FLOOD`] touch moves - far more than the broker lets one source
//! send at once.
//!
//! Its ring holds four records, so whenever the ring is full it notifies
//! the broker, which runs above it and drains the ring at once. The UI runs
//! above it too, so each batch is drained, and what passed is printed,
//! before this PD sends the next: the run is the same every time.
//!
//! Its role is `input`, the role of the driver it stands in for; it holds no
//! device. Test-only: it never reaches a demo or product system
//! (`tools/grant-rules.toml`). No `unsafe` code, no heap.

#![no_std]
#![no_main]

use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ipc::{HEADER_LEN, Producer, SendError, encode};
use nowayos_region::{RingTx, read_only, read_write, ring_producer};
use sel4_microkit::{Channel, Handler, Infallible, debug_println, protection_domain};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: input-flood:";

/// The channel to the broker, on which only this PD notifies.
const BROKER: Channel = Channel::new(0);

/// The ring to the broker: the broker's slots and size.
const RING_SLOTS: usize = 4;
const RING_SLOT: usize = 32;

/// How many touch moves the flood holds: contact 0 sliding to and fro.
const FLOOD: i32 = 200;

/// A contact number past the broker's ten.
const NO_SUCH_CONTACT: u8 = 10;

/// An event tag no `InputEvent` has.
const MALFORMED_TAG: u8 = 0x7F;

type Tx<'r> = Producer<'r, RingTx<RING_SLOTS, RING_SLOT>, RING_SLOT>;

#[protection_domain]
fn init() -> FloodPd {
    match flood() {
        Ok(()) => debug_println!("{TAG} done"),
        Err(why) => debug_println!("{TAG} gave up: {why}"),
    }
    FloodPd
}

fn flood() -> Result<(), &'static str> {
    // The ring: its messages region read-write (`nowayos_rw_region_0`), its
    // control region read-only (`nowayos_ro_region_0`).
    let ring = read_write(0)
        .and_then(|messages| ring_producer(messages, read_only(0)?))
        .map_err(|_| "the ring to the broker")?;
    let mut tx = Producer::new(&ring).map_err(|_| "the ring to the broker is broken")?;
    debug_println!(
        "{TAG} sending a malformed record, a pointer off the screen, a button the screen does not have, a touch contact it does not have, a touch and {FLOOD} touch moves"
    );
    send_bytes(&mut tx, &malformed()?)?;
    send(&mut tx, &InputEvent::PointerMove { x: 5000, y: -20 })?;
    send(
        &mut tx,
        &InputEvent::PointerButton {
            button: 9,
            down: true,
        },
    )?;
    let touch = |id, x, phase| InputEvent::Touch {
        id,
        x,
        y: 100,
        phase,
    };
    send(&mut tx, &touch(NO_SUCH_CONTACT, 100, TouchPhase::Start))?;
    send(&mut tx, &touch(0, 100, TouchPhase::Start))?;
    send(&mut tx, &touch(0, 100, TouchPhase::End))?;
    for n in 0..FLOOD {
        send(&mut tx, &touch(0, n.rem_euclid(100), TouchPhase::Move))?;
    }
    BROKER.notify();
    Ok(())
}

fn send(tx: &mut Tx<'_>, event: &InputEvent) -> Result<(), &'static str> {
    let mut bytes = [0_u8; RING_SLOT];
    let len = encode(event, &mut bytes).map_err(|_| "could not encode an event")?;
    send_bytes(tx, bytes.get(..len).ok_or("could not encode an event")?)
}

/// Sends one record. If the ring is full, notifies the broker - which runs
/// at once, above this PD, and drains it - and tries once more.
fn send_bytes(tx: &mut Tx<'_>, bytes: &[u8]) -> Result<(), &'static str> {
    for _ in 0..2 {
        match tx.try_send_bytes(bytes) {
            Ok(()) => return Ok(()),
            Err(SendError::Full) => BROKER.notify(),
            Err(_) => return Err("the ring to the broker is broken"),
        }
    }
    Err("the broker is not draining the ring")
}

/// A well-formed header announcing an input event, followed by a payload
/// whose event tag is [`MALFORMED_TAG`].
fn malformed() -> Result<[u8; RING_SLOT], &'static str> {
    let mut bytes = [0_u8; RING_SLOT];
    encode(&InputEvent::KeyDown(KeyCode::A), &mut bytes).map_err(|_| "could not encode")?;
    let tag = bytes.get_mut(HEADER_LEN).ok_or("record too short")?;
    *tag = MALFORMED_TAG;
    Ok(bytes)
}

/// Nothing to do after `init`: the flood is sent.
struct FloodPd;

impl Handler for FloodPd {
    type Error = Infallible;
}
