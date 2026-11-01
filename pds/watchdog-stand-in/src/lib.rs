// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-watchdog-stand-in`: a test instrument that stands in for the
//! hardware watchdog's tick. It never
//! reaches a demo or product system (`tools/grant-rules.toml`,
//! `[test_only]`). It picks its part by its PD name:
//!
//! * **`stand_in`** (QEMU, whose `virt` board has no watchdog): every
//!   [`PERIOD_MS`] it notifies the watchdog PD - the **tick** that ends a
//!   heartbeat round, the watchdog's only clock - and the watchdog
//!   **services** it by notifying back when every PD it watches answered.
//!   Unserviced for [`RESET_AFTER`] ticks in a row, it reports that the
//!   hardware would now reset the board, and stops ticking. It resets
//!   nothing.
//! * **`ticker`** (the Pi 5, whose PM watchdog the watchdog PD services
//!   itself, but which has no tick): every [`TICKER_PERIOD_MS`] it notifies
//!   the watchdog PD, and nothing more - until the system's **run length**,
//!   if the system gives one: read-only region 0, prefilled from
//!   `test-run-length-N.bin` (N seconds, one 32-bit little-endian word,
//!   which `tools/board-system` writes). Then it stops ticking; the watchdog
//!   services no more, and the PM watchdog resets the board - into U-Boot's
//!   prompt in lab mode, where `tools/pi-run` waits for it.
//!
//! Channels: 0 the watchdog PD; 1 the timer PD (`pp`), which times its
//! ticks. It holds nothing else but the run length.

#![no_std]

use nowayos_region::{RegionError, read_only};
use nowayos_timer_api::Client;
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, pd_name};

/// The watchdog PD: its ticks out, its servicing in.
const WATCHDOG: Channel = Channel::new(0);

/// The timer PD, which times the ticks.
const TIMER: Client = Client::new(Channel::new(1));

/// The tick's period, in milliseconds.
const PERIOD_MS: u64 = 200;

/// Ticks in a row with no servicing after which the hardware would reset
/// the board.
const RESET_AFTER: u32 = 3;

/// The ticker's period, in milliseconds: the Pi 5's watchdog PD arms the PM
/// watchdog for three of these at each tick.
const TICKER_PERIOD_MS: u64 = 2000;

/// The longest run length the ticker takes, in seconds: a day.
const MAX_RUN_SECS: u64 = 86_400;

/// The run length's region: read-only region 0.
const RUN_LENGTH: usize = 0;

/// The one timeout it sets.
const TICK: u64 = 1;

/// A millisecond in nanoseconds.
const MS: u64 = 1_000_000;

/// Starts the watchdog stand-in: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> StandInPd {
    let mut pd = StandInPd {
        ticker: None,
        serviced: true,
        unserviced: 0,
        serviced_times: 0,
        done: false,
    };
    if pd_name() == Ok("ticker") {
        let ticks = run_length();
        match ticks {
            Some(ticks) => debug_println!(
                "NOWAYOS: watchdog-stand-in: ticker: a tick to the watchdog every {TICKER_PERIOD_MS} ms for the run length, {} s",
                ticks.saturating_mul(TICKER_PERIOD_MS) / 1000
            ),
            None => debug_println!(
                "NOWAYOS: watchdog-stand-in: ticker: a tick to the watchdog every {TICKER_PERIOD_MS} ms, with no run length"
            ),
        }
        pd.ticker = Some(Ticker { left: ticks });
    } else {
        debug_println!(
            "NOWAYOS: watchdog-stand-in: a tick to the watchdog every {PERIOD_MS} ms; unserviced for {RESET_AFTER} ticks, the hardware would reset the board"
        );
    }
    pd.arm();
    pd
}

/// The ticks the run length allows, rounded up to whole periods; `None`
/// with no run length. A run length out of range is reported and taken as
/// none: the ticker stops at once, so the board resets.
fn run_length() -> Option<u64> {
    let seconds = match read_only(RUN_LENGTH).and_then(|region| region.into_words()) {
        Ok(words) => u64::from(words.load(0).unwrap_or(0)),
        Err(RegionError::NotMapped { .. }) => return None,
        Err(error) => {
            debug_println!("NOWAYOS: watchdog-stand-in: ticker: its run length: {error}");
            0
        }
    };
    if !(1..=MAX_RUN_SECS).contains(&seconds) {
        debug_println!(
            "NOWAYOS: watchdog-stand-in: ticker: a run length of {seconds} s, not 1 to {MAX_RUN_SECS}: no ticks"
        );
        return Some(0);
    }
    Some(seconds.saturating_mul(1000).div_ceil(TICKER_PERIOD_MS))
}

/// The ticker's state.
struct Ticker {
    /// Ticks left before the run length; `None` for no run length.
    left: Option<u64>,
}

/// The watchdog stand-in, as Microkit runs it.
pub struct StandInPd {
    /// The ticker's state, if this PD is the ticker.
    ticker: Option<Ticker>,
    /// Serviced since the last tick (the first tick has none before it).
    serviced: bool,
    /// Ticks in a row with no servicing.
    unserviced: u32,
    /// How often the watchdog has serviced it.
    serviced_times: u32,
    /// It has reported the reset, and ticks no more.
    done: bool,
}

impl StandInPd {
    /// Sets the next tick's timeout; a timer that refuses ends the test.
    fn arm(&mut self) {
        let period = if self.ticker.is_some() {
            TICKER_PERIOD_MS
        } else {
            PERIOD_MS
        };
        if TIMER.set_timeout(TICK, period.saturating_mul(MS)).is_err() {
            debug_println!("NOWAYOS: watchdog-stand-in: gave up: the timer refused a timeout");
            self.done = true;
        }
    }

    /// The period has passed: a tick to the watchdog, or the reset.
    fn tick(&mut self) {
        if self.done {
            return;
        }
        if let Some(ticker) = self.ticker.as_mut() {
            match ticker.left {
                Some(0) => {
                    debug_println!(
                        "NOWAYOS: watchdog-stand-in: ticker: the run length is reached: no more ticks, so the watchdog services no more and the board resets"
                    );
                    self.done = true;
                    return;
                }
                Some(left) => ticker.left = Some(left.saturating_sub(1)),
                None => {}
            }
            WATCHDOG.notify();
            self.arm();
            return;
        }
        if core::mem::replace(&mut self.serviced, false) {
            self.unserviced = 0;
        } else {
            self.unserviced = self.unserviced.saturating_add(1);
        }
        if self.unserviced >= RESET_AFTER {
            debug_println!(
                "NOWAYOS: watchdog-stand-in: serviced {} times, then not for {RESET_AFTER} ticks ({} ms): the hardware would now reset the board",
                self.serviced_times,
                u64::from(RESET_AFTER).saturating_mul(PERIOD_MS)
            );
            self.done = true;
            return;
        }
        WATCHDOG.notify();
        self.arm();
    }
}

impl Handler for StandInPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(WATCHDOG) {
            self.serviced = true;
            self.serviced_times = self.serviced_times.saturating_add(1);
        }
        if channels.contains(TIMER.channel()) {
            loop {
                match TIMER.expired() {
                    Ok(Some(TICK)) => self.tick(),
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        debug_println!(
                            "NOWAYOS: watchdog-stand-in: gave up: the timer refused a request"
                        );
                        self.done = true;
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
