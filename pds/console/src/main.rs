// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-console`: a test instrument that prints other PDs' lines on the
//! board's debug UART through its own registers - for test systems
//! run in Microkit's release configuration, whose kernel prints nothing.
//!
//! It takes lines from up to [`RINGS`] shared-memory rings (each record one
//! line of text, its messages region read-only here), copies each out,
//! refuses anything but printable ASCII, and writes the line to the UART,
//! then notifies the ring's sender on the same channel number, so a sender
//! can wait until its lines are out (the supervisor resets the board only
//! once its report is printed). It prints its own start-up line, which marks
//! each boot in the log.
//!
//! It holds the debug UART's registers (`debug_uart`, through `nowayos-hw`'s
//! PL011 view), the board's facts about it (read-only region 5, prefilled
//! from `device-debug_uart.bin`: which PL011 it is, and how many bytes
//! of its page are registers - 0x200 on the Pi 5, whose UART has no
//! identification registers to read) and its rings; no interrupt. No
//! `unsafe` code. Test-only: never in a product image.

#![no_std]
#![no_main]

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::device;
use nowayos_hw::pl011::{Pl011, TxFull};
use nowayos_ipc::Consumer;
use nowayos_region::{RegionError, RingRx, device_facts, read_only, read_write, ring_consumer};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The device number the system description binds the UART to.
const UART: usize = 0;

/// The most rings, each from one sender, on channel N for ring N.
const RINGS: usize = 4;

/// Each ring's slots, each one line of text.
const SLOTS: usize = 8;
const LINE: usize = 128;

/// The longest one byte may wait for room in the transmit FIFO.
const SEND_TIMEOUT_MS: u64 = 10;

#[protection_domain]
fn init() -> ConsolePd {
    let mut pd = ConsolePd {
        uart: None,
        rings: [None, None, None, None],
        timeout: counter_frequency()
            .checked_mul(SEND_TIMEOUT_MS)
            .map_or(u64::MAX, |ticks| ticks / 1000),
    };
    match device_facts() {
        Ok(facts) => {
            match device(UART)
                .and_then(|uart| Pl011::new(uart, facts.compatible(), facts.registers()))
            {
                Ok(uart) => pd.uart = Some(uart),
                Err(error) => debug_println!("NOWAYOS: console: gave up: the UART: {error}"),
            }
        }
        Err(error) => debug_println!("NOWAYOS: console: gave up: the UART's facts: {error}"),
    }
    // Ring N: its control region read-write (`nowayos_rw_region_N`), its
    // messages read-only (`nowayos_ro_region_N`). The first not granted ends
    // the list.
    for (index, slot) in pd.rings.iter_mut().enumerate() {
        match read_write(index).and_then(|control| ring_consumer(control, read_only(index)?)) {
            Ok(ring) => *slot = Some(ring),
            Err(RegionError::NotMapped { .. }) => break,
            Err(error) => {
                debug_println!("NOWAYOS: console: ring {index}: {error}");
                break;
            }
        }
    }
    pd.print(b"NOWAYOS: console: starting");
    pd
}

/// The console: the UART (`None` if it could not be bound), its rings and
/// how long one byte may wait.
struct ConsolePd {
    uart: Option<Pl011<'static>>,
    rings: [Option<RingRx<SLOTS, LINE>>; RINGS],
    timeout: u64,
}

impl ConsolePd {
    /// Writes `line` and a line break to the UART; gives the UART up if it
    /// stays full for the timeout.
    fn print(&mut self, line: &[u8]) {
        let Some(uart) = self.uart.as_mut() else {
            return;
        };
        for &byte in line.iter().chain(b"\r\n") {
            let start = counter();
            loop {
                match uart.try_send(byte) {
                    Ok(()) => break,
                    Err(TxFull) if counter().wrapping_sub(start) < self.timeout => {
                        core::hint::spin_loop();
                    }
                    Err(TxFull) => {
                        self.uart = None;
                        return;
                    }
                }
            }
        }
    }
}

/// The text of a line record: its bytes up to the first zero, if they are
/// all printable ASCII.
fn text(record: &[u8]) -> Option<&[u8]> {
    let end = record
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(record.len());
    let text = record.get(..end)?;
    text.iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        .then_some(text)
}

impl Handler for ConsolePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        for index in 0..RINGS {
            let channel = Channel::new(index);
            if !channels.contains(channel) {
                continue;
            }
            let mut lines = [[0u8; LINE]; SLOTS];
            let mut count = 0usize;
            if let Some(Some(memory)) = self.rings.get(index)
                && let Ok(mut rx) = Consumer::new(memory)
            {
                for slot in &mut lines {
                    match rx.try_recv_record() {
                        Ok(Some(record)) => {
                            slot.copy_from_slice(record.as_bytes());
                            count = count.saturating_add(1);
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
            for record in lines.iter().take(count) {
                match text(record) {
                    Some(line) => self.print(line),
                    None => {
                        self.print(b"NOWAYOS: console: refused a line that is not printable ASCII")
                    }
                }
            }
            channel.notify();
        }
        Ok(())
    }
}
