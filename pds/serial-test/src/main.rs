// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-serial-test`: a test instrument standing in for a serial
//! driver's client, in `systems/virtio-console-test.system`.
//!
//! At start it sends the driver one deliberately malformed record, which the
//! driver must refuse and count, then two lines of text through the transmit
//! ring (`nowayos-serial`'s chunks) - the second with a bell and a tab - and
//! notifies the driver. QEMU loops the console's output back to its input
//! (`systems/virtio-console-test.qemu`), and this PD prints each line it
//! receives - printable ASCII only, anything else as `.` - so the expect file
//! checks both directions. After taking chunks off the receive ring it
//! notifies the driver, which then takes more from the device.
//!
//! Never in a demo or product system (`tools/grant-rules.toml`).

#![no_std]
#![no_main]

use nowayos_ipc::{Consumer, Producer};
use nowayos_region::{RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer};
use nowayos_serial::{Chunk, RING_SLOT, RING_SLOTS, chunks};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The channel to the driver.
const DRIVER: Channel = Channel::new(0);

/// The lines sent, and received back.
const LINES: &[u8] = b"a line from the guest, out through the console and back\ncontrol characters: \x07 a bell, \t a tab\n";

/// The longest line printed whole; a longer one is printed in pieces.
const MAX_LINE: usize = 96;

#[protection_domain]
fn init() -> SerialTestPd {
    debug_println!("NOWAYOS: serial-test: starting");
    // The transmit ring: messages read-write, the driver's control
    // read-only. The receive ring: control read-write, messages read-only.
    let rings = read_write(0)
        .and_then(|messages| ring_producer(messages, read_only(0)?))
        .and_then(|transmit| {
            let receive = ring_consumer(read_write(1)?, read_only(1)?)?;
            Ok((transmit, receive))
        });
    let (transmit, receive) = match rings {
        Ok(rings) => rings,
        Err(error) => {
            debug_println!("NOWAYOS: serial-test: gave up: the rings: {error}");
            return SerialTestPd {
                receive: None,
                line: [0; MAX_LINE],
                len: 0,
            };
        }
    };
    if send(&transmit) {
        DRIVER.notify();
        debug_println!("NOWAYOS: serial-test: sent two lines");
    } else {
        debug_println!("NOWAYOS: serial-test: gave up: the transmit ring is full");
    }
    SerialTestPd {
        receive: Some(receive),
        line: [0; MAX_LINE],
        len: 0,
    }
}

/// Sends the malformed record, then the lines; whether all of it fitted.
fn send(transmit: &RingTx<RING_SLOTS, RING_SLOT>) -> bool {
    let Ok(mut ring) = Producer::new(transmit) else {
        return false;
    };
    // A record of another kind: a header claiming kind 9, version 1.
    let mut malformed = [0u8; 8];
    if let Some((kind, _)) = malformed.split_first_chunk_mut::<2>() {
        *kind = 9u16.to_le_bytes();
    }
    if ring.try_send_bytes(&malformed).is_err() {
        return false;
    }
    chunks(LINES).all(|chunk| ring.try_send(&chunk).is_ok())
}

/// The test client: the receive ring and the line being received.
struct SerialTestPd {
    receive: Option<RingRx<RING_SLOTS, RING_SLOT>>,
    line: [u8; MAX_LINE],
    len: usize,
}

impl SerialTestPd {
    /// Adds `byte` to the line, printing it at a line break or when full.
    fn take(&mut self, byte: u8) {
        if byte == b'\n' || self.len == MAX_LINE {
            self.print();
        }
        if byte != b'\n'
            && let Some(slot) = self.line.get_mut(self.len)
        {
            *slot = if byte == b' ' || byte.is_ascii_graphic() {
                byte
            } else {
                b'.'
            };
            self.len = self.len.saturating_add(1);
        }
    }

    fn print(&mut self) {
        let text = self
            .line
            .get(..self.len)
            .and_then(|line| core::str::from_utf8(line).ok())
            .unwrap_or("?");
        debug_println!("NOWAYOS: serial-test: received: {text}");
        self.len = 0;
    }
}

impl Handler for SerialTestPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if !channels.contains(DRIVER) {
            return Ok(());
        }
        let Some(receive) = self.receive.take() else {
            return Ok(());
        };
        if let Ok(mut ring) = Consumer::new(&receive) {
            while let Ok(Some(chunk)) = ring.try_recv::<Chunk>() {
                for &byte in chunk.bytes() {
                    self.take(byte);
                }
            }
        }
        self.receive = Some(receive);
        // Room again in the receive ring: the driver may take more.
        DRIVER.notify();
        Ok(())
    }
}
