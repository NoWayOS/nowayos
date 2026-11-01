// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-timer-client`: a client of the timer PD. Two PDs
//! run this program and pick their script by PD name:
//!
//! * `client_alpha` reads the time five times and checks it never goes back;
//!   checks that malformed requests and bad timeouts - unknown, a word too
//!   many, zero, too long, an id already set - are each refused with their
//!   typed error; and sets three timeouts, cancelling one at once;
//! * `client_beta` sets three timeouts, fills its fourth slot, and checks
//!   that a fifth is refused - while the other client keeps being served.
//!
//! On each notification from the timer PD a client collects its expired
//! timeouts and checks none was early, against the time it read just before
//! setting it. Between them the two clients' timeouts interleave, so the
//! serial output shows they expire in deadline order across clients. A
//! client holds nothing but its channel to the timer PD.
//!
//! The interleaving holds only while `client_beta` sets its timeouts less
//! than 200 ms (the gap between interleaved deadlines) after `client_alpha`
//! sets its own. Between the two, about 440 bytes are printed, and the
//! kernel's debug console writes each byte by waiting for the UART: about
//! 40 ms at 115200 baud on the Pi 5, next to nothing in QEMU.

#![no_std]
#![no_main]

use nowayos_timer_api::{Client, Instant, MAX_PENDING, Message, NOW, TimerError};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// The client's end of its channel to the timer PD.
const TIMER: Client = Client::new(Channel::new(0));

/// A millisecond in nanoseconds.
const MS: u64 = 1_000_000;

/// `client_alpha`'s timeouts: (id, milliseconds). Id 3 is cancelled at once;
/// its deadline passes before `client_beta`'s last, and must never fire.
const ALPHA: [(u64, u64); 3] = [(1, 700), (2, 300), (3, 900)];

/// `client_beta`'s timeouts. Its fourth slot then holds a long one (id 4),
/// so a fifth is refused; id 4 is cancelled after that.
const BETA: [(u64, u64); 3] = [(1, 500), (2, 1100), (3, 100)];

#[protection_domain]
fn init() -> TimerClientPd {
    let name = pd_name().unwrap_or("?");
    let mut pd = TimerClientPd {
        name,
        set: [None; MAX_PENDING],
    };
    debug_println!("NOWAYOS: timer-client {name}: starting");
    let result = match name {
        "client_alpha" => pd.alpha(),
        "client_beta" => pd.beta(),
        _ => Err("this PD has no script"),
    };
    if let Err(why) = result {
        debug_println!("NOWAYOS: timer-client {name}: gave up: {why}");
    }
    pd
}

/// A client's state: its name, and when and for how long each of its
/// timeouts was set.
struct TimerClientPd {
    name: &'static str,
    set: [Option<Set>; MAX_PENDING],
}

/// One timeout this client set.
#[derive(Clone, Copy)]
struct Set {
    id: u64,
    nanos: u64,
    at: Instant,
}

impl TimerClientPd {
    fn alpha(&mut self) -> Result<(), &'static str> {
        let name = self.name;
        let first = TIMER.now().map_err(|_| "the time request was refused")?;
        let mut last = first;
        for _ in 1..5 {
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
            let now = TIMER.now().map_err(|_| "the time request was refused")?;
            if now < last {
                return Err("the time went back");
            }
            last = now;
        }
        debug_println!(
            "NOWAYOS: timer-client {name}: 5 readings of the time, in nanoseconds, never decreasing"
        );
        debug_println!(
            "NOWAYOS: timer-client {name}: {} ns from the first reading to the last",
            last.saturating_since(first)
        );

        let unknown = Message::new(99, &[]).ok_or("could not build a request")?;
        match TIMER.call(&unknown).now_reply() {
            Err(error @ TimerError::UnknownRequest { label: 99 }) => {
                debug_println!("NOWAYOS: timer-client {name}: request 99 refused: {error}");
            }
            _ => return Err("an unknown request was not refused as one"),
        }
        let too_long = Message::new(NOW, &[1]).ok_or("could not build a request")?;
        match TIMER.call(&too_long).now_reply() {
            Err(
                error @ TimerError::BadLength {
                    label: NOW,
                    words: 1,
                },
            ) => {
                debug_println!(
                    "NOWAYOS: timer-client {name}: a time request with a word too many refused: {error}"
                );
            }
            _ => return Err("a request with a word too many was not refused as one"),
        }

        for (id, ms) in ALPHA {
            self.set_timeout(id, ms)?;
        }
        if TIMER.cancel(3) != Ok(true) {
            return Err("timeout 3 could not be cancelled");
        }
        self.forget(3);
        debug_println!(
            "NOWAYOS: timer-client {name}: timeouts set: 1 (700 ms), 2 (300 ms); 3 (900 ms) cancelled"
        );
        for (what, result, expected) in [
            (
                "a zero timeout",
                TIMER.set_timeout(9, 0),
                TimerError::ZeroTimeout,
            ),
            (
                "a timeout too long for the counter",
                TIMER.set_timeout(9, u64::MAX),
                TimerError::TimeoutTooLong { nanos: u64::MAX },
            ),
            (
                "a second timeout 1",
                TIMER.set_timeout(1, MS),
                TimerError::AlreadyPending { id: 1 },
            ),
        ] {
            match result {
                Err(error) if error == expected => {
                    debug_println!("NOWAYOS: timer-client {name}: {what} refused: {error}");
                }
                _ => return Err("a bad timeout was not refused as it should be"),
            }
        }
        Ok(())
    }

    fn beta(&mut self) -> Result<(), &'static str> {
        let name = self.name;
        for (id, ms) in BETA {
            self.set_timeout(id, ms)?;
        }
        self.set_timeout(4, 10_000)?;
        match TIMER.set_timeout(5, MS) {
            Err(error @ TimerError::TooManyTimeouts { limit: MAX_PENDING }) => {
                debug_println!("NOWAYOS: timer-client {name}: a fifth timeout refused: {error}");
            }
            _ => return Err("a fifth timeout was not refused"),
        }
        if TIMER.cancel(4) != Ok(true) {
            return Err("timeout 4 could not be cancelled");
        }
        self.forget(4);
        debug_println!(
            "NOWAYOS: timer-client {name}: timeouts set: 1 (500 ms), 2 (1100 ms), 3 (100 ms)"
        );
        Ok(())
    }

    /// Sets timeout `id` for `ms` milliseconds, noting when.
    fn set_timeout(&mut self, id: u64, ms: u64) -> Result<(), &'static str> {
        let nanos = ms.checked_mul(MS).ok_or("timeout too long")?;
        let at = TIMER.now().map_err(|_| "the time request was refused")?;
        TIMER
            .set_timeout(id, nanos)
            .map_err(|_| "a timeout was refused")?;
        let slot = self
            .set
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or("no room to note a timeout")?;
        *slot = Some(Set { id, nanos, at });
        Ok(())
    }

    /// Forgets the note of timeout `id`, returning it.
    fn forget(&mut self, id: u64) -> Option<Set> {
        self.set
            .iter_mut()
            .find(|slot| slot.is_some_and(|set| set.id == id))
            .and_then(Option::take)
    }

    /// Collects every expired timeout and reports each.
    fn collect(&mut self) -> Result<(), &'static str> {
        let name = self.name;
        while let Some(id) = TIMER
            .expired()
            .map_err(|_| "the expired request was refused")?
        {
            let now = TIMER.now().map_err(|_| "the time request was refused")?;
            let set = self
                .forget(id)
                .ok_or("a timeout this client did not set expired")?;
            let ms = set.nanos.checked_div(MS).unwrap_or_default();
            if now.saturating_since(set.at) < set.nanos {
                return Err("a timeout fired early");
            }
            debug_println!("NOWAYOS: timer-client {name}: timeout {id} ({ms} ms) fired, not early");
        }
        Ok(())
    }
}

impl Handler for TimerClientPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(TIMER.channel())
            && let Err(why) = self.collect()
        {
            debug_println!("NOWAYOS: timer-client {}: gave up: {why}", self.name);
        }
        Ok(())
    }
}
