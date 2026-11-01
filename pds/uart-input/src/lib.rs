// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-uart-input`: keys typed on a terminal attached to the board's
//! debug UART, as canonical key events for the input broker.
//!
//! **A development route only.** On the Raspberry Pi 5 the debug UART is
//! wired to the Debug Probe, and a terminal on the developer's Mac sends
//! what is typed in it to the UART's receive side; this PD reads those bytes,
//! turns them into key presses (`nowayos_input::terminal`) and sends each
//! press's events to the input broker through its ring, so the Product
//! Owner can drive a UI on the board before it has an input device of its
//! own. A product's keys never come this way: `tools/grant-rules.toml`
//! names this program development-only (`[development_only]`, by the
//! pattern `*-uart-input.elf`, which a product's own program wrapping this
//! library keeps), refused in every product system, and `make image` says so
//! of every image that runs it; and, whatever a program is called, no input
//! PD may hold the debug UART in a product (`debug-uart-no-input-in-product`).
//!
//! # Who owns the debug UART
//!
//! The UART is shared with the seL4 kernel, which in Microkit's debug
//! configuration writes its debug console - every PD's `debug_println!` - to
//! its transmitter. So the ownership is split:
//!
//! * **the kernel owns transmission**, and everything else stays as the
//!   boot firmware and U-Boot left it - the baud rate, line settings,
//!   enables and interrupt mask. The kernel alone writes UARTDR to send;
//! * **this PD owns reception, and only that**: it reads UARTCR once at
//!   start-up, to check the receiver is enabled (and, on a PrimeCell such as
//!   QEMU's, its identification registers, to check it is a PL011), then
//!   UARTFR, to see whether a byte is waiting, and UARTDR, taking bytes off
//!   the receive FIFO - which the kernel never reads. It writes nothing to
//!   the UART at all: its system maps the registers read-only, and the
//!   grants check holds an input PD to that (`input-reads-the-debug-uart-
//!   only`). There is no echo. Nothing else in a system may map the UART
//!   (grants-check rule 7: a device's window in one PD only).
//!
//! # Polled, on the timer
//!
//! It polls: every [`POLL_MS`] the timer PD (`nowayos-timer`, a protected
//! procedure call away) wakes it, and it drains the receive FIFO. The
//! UART's own receive interrupt does not reach a PD on the Pi 5 (seen on
//! the board, 2026-10-08: with its receive interrupts unmasked the UART
//! raised one - UARTMIS 0x40 - and no notification came, while the system
//! timer's interrupt does), and the board's facts give
//! no other way to be woken. A poll every 10 ms keeps ahead of a person
//! typing; at 115,200 baud the FIFO's 32 bytes take 2.8 ms to arrive, so a
//! longer burst - a paste - can overrun it, which the UART reports and this
//! PD counts.
//!
//! ESC alone is Escape, but ESC also begins the arrows' sequences, so a
//! decoder left part-way through a sequence when the FIFO runs dry first
//! waits up to [`SEQUENCE_WAIT_US`] for the next byte - a terminal writes a
//! key's whole sequence at once, so its bytes arrive a character's time
//! apart (87 µs at 115,200 baud) - and, if none comes, waits for the next
//! poll: a poll that finds the FIFO empty means the line has gone quiet, so
//! a lone ESC becomes Escape and any other sequence is cut short and
//! counted. Escape therefore arrives 10 to 20 ms after the key. A poll waits
//! [`SEQUENCE_WAIT_US`] at most in all, however many sequences it meets, so
//! a stream of bytes that keeps opening sequences cannot keep it spinning
//! and hold the UI back: what it has not finished, the next poll does.
//!
//! # Nothing untrusted reaches the UI
//!
//! The bytes are not trusted: the decoder drops and counts every byte or
//! sequence of no key it sends, a byte the UART received with a framing,
//! parity or break error is dropped and counted, and an overrun (bytes lost
//! before this one) is counted. A press is sent whole or not at all - its
//! Shift with it: if the ring has no room for a press's events, the PD
//! notifies the broker, which runs above it and drains its sources at once,
//! and if there is still no room the press is dropped and counted. The
//! broker then checks every event against the screen and holds this source
//! to its rate - 64 events at once, then 250 a second - dropping single
//! events over it, which could leave Shift held. So this PD meters whole
//! presses below that rate ([`BURST`] events at once, then [`RATE`] a
//! second): a press its meter cannot yet afford waits, with the bytes after
//! it, in the UART's FIFO, until a later poll. The broker's limiter,
//! refilling faster and holding more, never has fewer events in hand than
//! the meter, so its rate limit drops no event of this PD's (a paste faster
//! than that overruns the FIFO instead - counted). The broker still drops
//! events when the UI's ring is full (counted): then part of a press can be
//! lost, and Shift left held. It reports its counts whenever they change.
//!
//! Its regions: the UART's register window (device 0, uncached, read-only), the
//! board's facts about it (read-only region 5: which PL011, how many bytes
//! of registers), and its ring to the broker - messages read-write
//! (`nowayos_rw_region_0`), the broker's index read-only
//! (`nowayos_ro_region_0`). Channel 0 to the broker, on which only this PD
//! notifies; channel 1 to the timer PD, which it calls and which notifies it.
//! No interrupt. Role `input`. No `unsafe` code.
//!
//! A library and a program, as the display PD is: NoWayOS's systems run the
//! program; a product's development system runs the PD from a program of
//! its own whose `init` returns [`init`]'s - named `PRODUCT-uart-input`, so
//! that the development-only rule binds it.

#![no_std]

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::pl011::{Pl011, Received};
use nowayos_hw::{HwError, device};
use nowayos_input::terminal::{Decoder, DropCounts, Press, Presses};
use nowayos_input::{InputEvent, RateLimiter};
use nowayos_ipc::Producer;
use nowayos_region::{RegionError, RingTx, device_facts, read_only, read_write, ring_producer};
use nowayos_timer_api::{Client, TimerError};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: uart-input:";

/// The device number the system description binds the UART to.
const UART: usize = 0;

/// The channel to the broker, on which only this PD notifies.
const BROKER: Channel = Channel::new(0);

/// This PD's end of its channel to the timer PD: it calls the timer, and
/// the timer notifies it when the poll is due.
const TIMER: Client = Client::new(Channel::new(1));

/// The id of the poll's timeout.
const POLL: u64 = 1;

/// The ring to the broker: the broker's slots and slot size for a source.
pub const RING_SLOTS: usize = 4;
/// The size of one slot of the ring to the broker.
pub const RING_SLOT: usize = 32;

/// How often the receive FIFO is drained, in milliseconds.
pub const POLL_MS: u64 = 10;

/// How long a sequence's next byte is waited for when the FIFO runs dry
/// part-way through it, in microseconds.
pub const SEQUENCE_WAIT_US: u64 = 2_000;

/// How often the flags are read while waiting, in microseconds.
const WAIT_STEP_US: u64 = 100;

/// The meter's burst: below the broker's 64 events at once.
pub const BURST: u64 = 60;
/// The meter's rate, events a second: below the broker's 250.
pub const RATE: u64 = 240;

/// The most bytes taken off the FIFO in one poll: a poll stays bounded,
/// and whatever is left is taken at the next.
const MAX_BYTES: usize = 256;

/// UARTDR's error bits as `nowayos_hw::pl011::Received` gives them: a
/// framing, parity or break error (the byte itself is bad), and an overrun
/// (bytes were lost before it).
const BYTE_ERRORS: u8 = 0b0111;
const OVERRUN: u8 = 0b1000;

/// Starts the PD: binds the UART and its facts and the ring to the broker,
/// and asks the timer for the first poll.
#[must_use]
pub fn init() -> UartInputPd {
    debug_println!("{TAG} starting");
    let state = match start() {
        Ok(state) => Some(state),
        Err(why) => {
            debug_println!("{TAG} gave up: {why}");
            None
        }
    };
    UartInputPd { state }
}

/// Why the PD could not start.
enum Failure {
    Facts(RegionError),
    Device(HwError),
    Ring(RegionError),
    ReceiveDisabled,
    NoCounter,
    Timer(TimerError),
}

/// The presses the decoder may finish with one byte (or the end of input):
/// an Escape, then the byte's own.
const HELD: usize = 2;

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Facts(error) => write!(f, "the board's facts about the UART: {error}"),
            Self::Device(error) => write!(f, "the UART: {error}"),
            Self::Ring(error) => write!(f, "the ring to the broker: {error}"),
            Self::ReceiveDisabled => f.write_str("the UART's receiver is disabled"),
            Self::NoCounter => f.write_str("the counter has no frequency"),
            Self::Timer(error) => write!(f, "the timer refused the poll: {error}"),
        }
    }
}

fn start() -> Result<State, Failure> {
    let facts = device_facts().map_err(Failure::Facts)?;
    let device = device(UART).map_err(Failure::Device)?;
    let uart =
        Pl011::new(device, facts.compatible(), facts.registers()).map_err(Failure::Device)?;
    let control = uart.control();
    if !(control.enabled() && control.receive_enabled()) {
        return Err(Failure::ReceiveDisabled);
    }
    let ring = read_write(0)
        .and_then(|messages| ring_producer(messages, read_only(0)?))
        .map_err(Failure::Ring)?;
    let frequency = counter_frequency();
    let wait = frequency
        .checked_mul(SEQUENCE_WAIT_US)
        .map(|ticks| ticks / 1_000_000)
        .filter(|&ticks| ticks > 0)
        .ok_or(Failure::NoCounter)?;
    let step = frequency.saturating_mul(WAIT_STEP_US) / 1_000_000;
    let meter = frequency
        .checked_div(RATE)
        .filter(|&ticks| ticks > 0)
        .and_then(|ticks| RateLimiter::new(u32::try_from(BURST).ok()?, ticks))
        .ok_or(Failure::NoCounter)?;
    TIMER
        .set_timeout(POLL, POLL_MS.saturating_mul(1_000_000))
        .map_err(Failure::Timer)?;
    // Which PL011 differs from board to board: on a line of its own, which
    // no expect file checks.
    debug_println!("{TAG} the UART is an {}", uart.kind().compatible());
    debug_println!(
        "{TAG} reading keys from the debug UART every {POLL_MS} ms; the kernel keeps its transmitter"
    );
    Ok(State {
        uart,
        ring,
        decoder: Decoder::new(),
        meter,
        held: [None; HELD],
        wait,
        step,
        counts: Counts::default(),
        reported: Counts::default(),
    })
}

/// What the PD has dropped or seen go wrong, besides the decoder's own
/// counts.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    decoder: DropCounts,
    /// Bytes received with a framing, parity or break error: dropped.
    bad_bytes: u32,
    /// Bytes received after an overrun: bytes were lost before them.
    overruns: u32,
    /// Presses with no room in the ring, even after the broker drained it.
    no_room: u32,
}

/// Everything the PD holds once started.
struct State {
    uart: Pl011<'static>,
    ring: RingTx<RING_SLOTS, RING_SLOT>,
    decoder: Decoder,
    /// Whole presses, metered below the broker's rate.
    meter: RateLimiter,
    /// Presses decoded but not yet affordable, in order: sent first at the
    /// next poll, and no byte is taken off the FIFO until they are.
    held: [Option<Press>; HELD],
    /// [`SEQUENCE_WAIT_US`] and the step between reads of the flags while
    /// waiting, in counter ticks.
    wait: u64,
    step: u64,
    counts: Counts,
    reported: Counts,
}

impl State {
    /// Takes what the UART received, decodes it and sends each press to the
    /// broker, as the meter affords. Returns whether anything was sent.
    fn poll(&mut self) -> bool {
        let mut sent = self.send_held();
        if !self.nothing_held() {
            // Over the meter: the bytes wait in the FIFO for a later poll.
            return sent;
        }
        // However many sequences this poll meets, it waits for their bytes
        // no longer than one SEQUENCE_WAIT_US in all.
        let wait_until = counter().saturating_add(self.wait);
        let mut taken = 0usize;
        while taken < MAX_BYTES {
            let Some(received) = self.next_byte(wait_until) else {
                break;
            };
            taken = taken.saturating_add(1);
            if received.errors & OVERRUN != 0 {
                self.counts.overruns = self.counts.overruns.saturating_add(1);
            }
            if received.errors & BYTE_ERRORS != 0 {
                self.counts.bad_bytes = self.counts.bad_bytes.saturating_add(1);
                continue;
            }
            let presses = self.decoder.push(received.byte);
            self.hold(presses);
            sent |= self.send_held();
            if !self.nothing_held() {
                break;
            }
        }
        // A poll that finds the FIFO empty and took nothing: the line has
        // gone quiet, so a sequence still open ends here - a lone ESC is
        // Escape.
        if taken == 0 && self.decoder.pending() && self.uart.flags().rx_empty() {
            let presses = self.decoder.end_of_input();
            self.hold(presses);
            sent |= self.send_held();
        }
        self.counts.decoder = self.decoder.dropped();
        sent
    }

    /// Whether no press waits for the meter.
    fn nothing_held(&self) -> bool {
        self.held.iter().all(Option::is_none)
    }

    /// Queues `presses` behind any already held (there are none when a byte
    /// is taken, and a byte or the end of input finishes at most two).
    fn hold(&mut self, presses: Presses) {
        for press in presses {
            if let Some(slot) = self.held.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(press);
            }
        }
    }

    /// Sends the held presses, in order, while the meter affords each whole.
    /// Returns whether anything was sent.
    fn send_held(&mut self) -> bool {
        let mut sent = false;
        // The held presses are always at the front: the first goes first.
        while let Some(press) = self.held.first().copied().flatten() {
            let events = u32::try_from(press.event_count()).unwrap_or(u32::MAX);
            if !self.meter.try_spend(counter(), events) {
                break;
            }
            if let Some(first) = self.held.first_mut() {
                *first = None;
            }
            self.held.rotate_left(1);
            sent |= self.send(press);
        }
        sent
    }

    /// The next byte received: at once, or - when the decoder is part-way
    /// through a sequence - before `wait_until`; `None` when there is none.
    fn next_byte(&mut self, wait_until: u64) -> Option<Received> {
        if let Some(received) = self.uart.try_receive() {
            return Some(received);
        }
        if !self.decoder.pending() {
            return None;
        }
        while counter() < wait_until {
            let pause = counter();
            while counter().wrapping_sub(pause) < self.step {
                core::hint::spin_loop();
            }
            if let Some(received) = self.uart.try_receive() {
                return Some(received);
            }
        }
        None
    }

    /// Sends a press's events to the broker, whole or not at all. Returns
    /// whether it was sent.
    fn send(&mut self, press: Press) -> bool {
        let Ok(mut tx) = Producer::new(&self.ring) else {
            self.counts.no_room = self.counts.no_room.saturating_add(1);
            return false;
        };
        let needed = u32::try_from(press.event_count()).unwrap_or(u32::MAX);
        if tx.free_slots().unwrap_or(0) < needed {
            // The broker runs above this PD: notified, it drains its
            // sources before this PD runs again.
            BROKER.notify();
        }
        if tx.free_slots().unwrap_or(0) < needed {
            self.counts.no_room = self.counts.no_room.saturating_add(1);
            return false;
        }
        for event in press.events() {
            if tx.try_send::<InputEvent>(&event).is_err() {
                self.counts.no_room = self.counts.no_room.saturating_add(1);
                break;
            }
        }
        true
    }

    /// Reports the counts if they changed.
    fn report(&mut self) {
        let counts = self.counts;
        if counts == self.reported {
            return;
        }
        self.reported = counts;
        debug_println!(
            "{TAG} dropped so far - bytes of no key: {}, sequences of no key: {}, cut short: {}, bad bytes: {}, overruns: {}, presses with no room: {}",
            counts.decoder.unknown_bytes,
            counts.decoder.unknown_sequences,
            counts.decoder.cut_short,
            counts.bad_bytes,
            counts.overruns,
            counts.no_room
        );
    }
}

/// The PD: its state once started (`None` if it gave up).
pub struct UartInputPd {
    state: Option<State>,
}

impl Handler for UartInputPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if !channels.contains(TIMER.channel()) {
            return Ok(());
        }
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        // Collect the poll's timeout, then ask for the next before polling,
        // so the polls keep their rate however long one takes.
        while let Ok(Some(_)) = TIMER.expired() {}
        if let Err(error) = TIMER.set_timeout(POLL, POLL_MS.saturating_mul(1_000_000)) {
            debug_println!("{TAG} gave up: the timer refused the next poll: {error}");
            self.state = None;
            return Ok(());
        }
        if state.poll() {
            BROKER.notify();
        }
        state.report();
        Ok(())
    }
}
