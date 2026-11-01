// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-pl011`: the first driver protection domain.
//!
//! It drives the Arm PL011 UART through `nowayos-hw`'s typed view of its
//! registers, and has no `unsafe` code of its own (the workspace lints
//! forbid it). It binds device 0 - the UART's register window, which
//! `systems/pl011-demo.system` grants it uncached - with the board's facts
//! about it (read-only region 5, prefilled from `device-debug_uart.bin`:
//! which PL011 it is and how many bytes of its page are registers),
//! identifies it - by its identification registers where its window holds
//! them (QEMU's), by those facts where it does not (the Pi 5's) - and sends
//! two lines through the UART's own data register, polling the transmit
//! FIFO. Each byte waits at most [`SEND_TIMEOUT_MS`] on the counter, so a
//! UART that stops draining stops the demo with a message rather than
//! hanging the PD.
//!
//! Its other lines go through the seL4 debug console (`debug_println!`),
//! which the kernel writes to the same UART in Microkit's debug
//! configuration: both reach the same serial port, in program order.
//!
//! Polled and send-only: the UART's interrupt, receiving, and serving other
//! PDs come with the serial driver design.

#![no_std]
#![no_main]

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::pl011::{Pl011, TxFull};
use nowayos_hw::{HwError, device};
use nowayos_region::{RegionError, device_facts};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The device number the system description binds the UART to
/// (`nowayos_device_0_*`).
const UART: usize = 0;

/// Lines sent through the UART's own registers; `systems/pl011-demo.expect`
/// checks them.
const LINES: [&str; 2] = [
    "NOWAYOS: pl011: this line was written to the UART's own data register",
    "NOWAYOS: pl011: by a driver PD with no unsafe code",
];

/// The longest one byte may wait for room in the transmit FIFO.
const SEND_TIMEOUT_MS: u64 = 10;

#[protection_domain]
fn init() -> Pl011Pd {
    debug_println!("NOWAYOS: pl011: starting");
    match run() {
        Ok(()) => debug_println!("NOWAYOS: pl011: done"),
        Err(Failure::Facts(error)) => {
            debug_println!("NOWAYOS: pl011: gave up: the board's facts: {error}");
        }
        Err(Failure::Device(error)) => debug_println!("NOWAYOS: pl011: gave up: {error}"),
        Err(Failure::TransmitDisabled) => {
            debug_println!("NOWAYOS: pl011: gave up: the UART's transmitter is disabled");
        }
        Err(Failure::Stuck) => debug_println!(
            "NOWAYOS: pl011: gave up: no room in the transmit FIFO for {SEND_TIMEOUT_MS} ms"
        ),
    }
    Pl011Pd
}

/// Why the demo stopped early.
enum Failure {
    Facts(RegionError),
    Device(HwError),
    TransmitDisabled,
    Stuck,
}

fn run() -> Result<(), Failure> {
    let facts = device_facts().map_err(Failure::Facts)?;
    debug_println!(
        "NOWAYOS: pl011: the board's facts: {}, {:#x} bytes of registers",
        core::str::from_utf8(facts.compatible()).unwrap_or("?"),
        facts.registers()
    );
    let device = device(UART).map_err(Failure::Device)?;
    let mut uart =
        Pl011::new(device, facts.compatible(), facts.registers()).map_err(Failure::Device)?;
    match uart.identity() {
        Some(identity) => debug_println!(
            "NOWAYOS: pl011: its ID registers name part {:#05x} from designer {:#04x}, revision {}",
            identity.part,
            identity.designer,
            identity.revision
        ),
        None => debug_println!(
            "NOWAYOS: pl011: no ID registers in its window: identified by the board's facts"
        ),
    }
    debug_println!("NOWAYOS: pl011: found an Arm PL011");
    let control = uart.control();
    debug_println!(
        "NOWAYOS: pl011: enabled {}, transmit {}; counter at {} Hz",
        control.enabled(),
        control.transmit_enabled(),
        counter_frequency()
    );
    if !(control.enabled() && control.transmit_enabled()) {
        return Err(Failure::TransmitDisabled);
    }
    let timeout = counter_frequency()
        .checked_mul(SEND_TIMEOUT_MS)
        .map_or(u64::MAX, |ticks| ticks / 1000);
    for line in LINES {
        for byte in line.bytes().chain(*b"\r\n") {
            send(&mut uart, byte, timeout)?;
        }
    }
    Ok(())
}

/// Sends `byte`, waiting at most `timeout` counter ticks for room.
fn send(uart: &mut Pl011<'_>, byte: u8, timeout: u64) -> Result<(), Failure> {
    let start = counter();
    loop {
        match uart.try_send(byte) {
            Ok(()) => return Ok(()),
            Err(TxFull) if counter().wrapping_sub(start) < timeout => core::hint::spin_loop(),
            Err(TxFull) => return Err(Failure::Stuck),
        }
    }
}

/// Event handler for the PL011 PD. It has no channels or interrupts yet, so
/// the default `Handler` behaviour (nothing to do) is all it needs.
struct Pl011Pd;

impl Handler for Pl011Pd {
    type Error = Infallible;
}
