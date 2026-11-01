// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-watchdog`: the watchdog PD of the PD fault policy, as a library a system runs with the PDs it
//! watches, from a program of its own.
//!
//! It keeps the board's hardware watchdog from firing only while every PD
//! it watches - the supervisor included - answers its heartbeat in time,
//! in rounds (`nowayos-heartbeat`):
//!
//! 1. A round begins when it notifies - **pings** - every watched PD, each
//!    on a notification-only channel of its own. Each answers by notifying
//!    back, from its event loop.
//! 2. A fixed-period **tick** ends the round: the watchdog PD makes **no
//!    call**, so no PD can hold it while heartbeats go unjudged, and takes
//!    its time from the tick alone. In QEMU, whose `virt` board has no
//!    watchdog, a test-only stand-in (`nowayos-watchdog-stand-in`) sends
//!    the tick and plays the hardware ([`init`]). The Pi 5's PM watchdog
//!    has no tick of its own, so there the tick comes from the stand-in's
//!    ticker (a test system's) and the watchdog services the hardware
//!    itself, through the PM watchdog's registers ([`init_pm`]).
//! 3. If every PD answered, it **services** the hardware - notifies the
//!    stand-in, or arms the PM watchdog for [`PM_TIMEOUT_SECS`] more - and
//!    pings every PD, beginning the next round. If any did not, it names
//!    **every PD that missed, highest priority first** - a looping PD
//!    starves every PD below it, so the first is the likely cause - and
//!    **never services again**, so the hardware resets the board.
//!
//! When the ticks stop - a test system's run length reached, or the PD
//! that sends them starved - the rounds stop too and nothing is serviced
//! any more: the board resets. That is how a test system on the Pi asks for
//! a run of a set length ([`init_pm_ticks_only`] keeps the board up for
//! exactly as long as the ticks come, watching no PD).
//!
//! It runs above every PD it watches but the supervisor, so no looping PD
//! it watches can starve it (`tools/grant-rules.toml`,
//! `watchdog-above-every-pd-it-watches`); if it is starved anyway, the
//! hardware still resets the board, with nothing named.
//!
//! Capabilities: its channels, and on the Pi 5 the PM watchdog's page,
//! which only a watchdog PD may hold (`pm-watchdog-only-watchdog`) -
//! no call, child, map, DMA or SMC capability (role `watchdog`). Until
//! The store still to come, the PDs that missed are only printed, in a debug
//! build.

#![no_std]

use nowayos_heartbeat::{Rounds, Verdict};
use nowayos_hw::pm_watchdog::PmWatchdog;
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// How long each servicing arms the Pi 5's PM watchdog for: three of the
/// ticker's two-second periods, so one late tick does not reset the board,
/// and a missed round does within about four seconds.
pub const PM_TIMEOUT_SECS: u32 = 6;

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: watchdog:";

/// One watched PD: its name, which the watchdog's reports use, and the
/// watchdog's end of its heartbeat channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watched {
    channel: Channel,
    name: &'static str,
}

impl Watched {
    /// The PD called `name`, whose heartbeat channel is the watchdog's
    /// channel `channel`.
    #[must_use]
    pub const fn new(channel: usize, name: &'static str) -> Self {
        Self {
            channel: Channel::new(channel),
            name,
        }
    }
}

/// What the watchdog services, and where its ticks come from.
enum Hardware {
    /// QEMU's stand-in: its ticks in and the servicing out, on one channel.
    StandIn(Channel),
    /// The Pi 5's PM watchdog, armed through its registers; the ticks on
    /// their own channel.
    Pm {
        /// The ticker's channel.
        ticks: Channel,
        /// The PM watchdog.
        pm: PmWatchdog,
        /// Whether a failed servicing was reported.
        said: bool,
    },
}

impl Hardware {
    /// The channel the ticks come on.
    const fn ticks(&self) -> Channel {
        match self {
            Self::StandIn(channel) | Self::Pm { ticks: channel, .. } => *channel,
        }
    }

    /// Services the hardware: `true` if it was.
    fn service(&mut self) -> bool {
        match self {
            Self::StandIn(channel) => {
                channel.notify();
                true
            }
            Self::Pm { pm, said, .. } => match pm.service(PM_TIMEOUT_SECS) {
                Ok(()) => true,
                Err(error) => {
                    if !*said {
                        *said = true;
                        debug_println!("{TAG} could not service the PM watchdog: {error}");
                    }
                    false
                }
            },
        }
    }
}

/// How the watchdog judges a round.
enum Judging {
    /// Every watched PD must answer.
    Rounds(Rounds),
    /// It watches no PD: every tick is serviced (a test system's run length).
    TicksOnly,
    /// It cannot watch the list it was given: it never services.
    Nothing,
}

/// The watchdog PD's state; see the [crate documentation](crate).
pub struct WatchdogPd {
    watched: &'static [Watched],
    hardware: Option<Hardware>,
    judging: Judging,
    /// Ticks serviced, for [`Judging::TicksOnly`]'s reports.
    serviced: u32,
}

/// Starts the watchdog over the PDs in `watched`, **listed highest
/// priority first**, with QEMU's stand-in for the hardware watchdog on
/// channel `hardware` - its ticks in, the servicing out: says what it
/// watches, and pings each, beginning round 1. A list it cannot watch -
/// empty or too long - is reported, and the watchdog never services the
/// hardware, which then resets the board.
#[must_use]
pub fn init(watched: &'static [Watched], hardware: usize) -> WatchdogPd {
    start(watched, Some(Hardware::StandIn(Channel::new(hardware))))
}

/// Starts the watchdog as [`init`] does, on the Pi 5: the ticks on channel
/// `ticks`, and the PM watchdog bound as `nowayos-hw`'s device `device`,
/// serviced for [`PM_TIMEOUT_SECS`] at a time. A device it cannot
/// bind is reported, and the watchdog never services: whatever armed the
/// PM watchdog then resets the board.
#[must_use]
pub fn init_pm(watched: &'static [Watched], ticks: usize, device: usize) -> WatchdogPd {
    start(watched, bind_pm(ticks, device))
}

/// Starts a watchdog that **watches no PD** and services the PM watchdog
/// on every tick, as [`init_pm`] binds it: the board stays up for exactly
/// as long as the ticks come. For a test system's run length on the Pi -
/// never a product's, whose watchdog watches every PD.
#[must_use]
pub fn init_pm_ticks_only(ticks: usize, device: usize) -> WatchdogPd {
    let hardware = bind_pm(ticks, device);
    debug_println!(
        "{TAG} watching no PD: the PM watchdog is serviced on every tick, for {PM_TIMEOUT_SECS} s, for as long as the ticks come"
    );
    WatchdogPd {
        watched: &[],
        hardware,
        judging: Judging::TicksOnly,
        serviced: 0,
    }
}

/// Binds the PM watchdog, reporting why not.
fn bind_pm(ticks: usize, device: usize) -> Option<Hardware> {
    match nowayos_hw::device(device).and_then(PmWatchdog::new) {
        Ok(pm) => {
            debug_println!(
                "{TAG} the PM watchdog, armed for {PM_TIMEOUT_SECS} s at each servicing"
            );
            Some(Hardware::Pm {
                ticks: Channel::new(ticks),
                pm,
                said: false,
            })
        }
        Err(error) => {
            debug_println!(
                "{TAG} gave up: the PM watchdog: {error}: the hardware watchdog is never serviced"
            );
            None
        }
    }
}

/// Starts the watchdog over `watched` and `hardware`.
fn start(watched: &'static [Watched], hardware: Option<Hardware>) -> WatchdogPd {
    let judging = Rounds::new(watched.len()).map_or(Judging::Nothing, Judging::Rounds);
    let pd = WatchdogPd {
        watched,
        hardware,
        judging,
        serviced: 0,
    };
    if matches!(pd.judging, Judging::Nothing) {
        debug_println!(
            "{TAG} cannot watch {} PDs: the hardware watchdog is never serviced",
            watched.len()
        );
        return pd;
    }
    debug_println!(
        "{TAG} watching {} PDs, highest priority first: {}",
        watched.len(),
        Names(watched, None)
    );
    pd.ping();
    pd
}

impl WatchdogPd {
    /// Begins a round: pings every watched PD.
    fn ping(&self) {
        for watched in self.watched {
            watched.channel.notify();
        }
    }
}

impl Handler for WatchdogPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let Some(hardware) = self.hardware.as_mut() else {
            return Ok(());
        };
        let tick = channels.contains(hardware.ticks());
        match &mut self.judging {
            Judging::Nothing => {}
            Judging::TicksOnly => {
                if tick && hardware.service() {
                    self.serviced = self.serviced.saturating_add(1);
                    if self.serviced == 1 {
                        debug_println!("{TAG} first tick: hardware watchdog serviced");
                    }
                }
            }
            Judging::Rounds(rounds) => {
                // A batch's answers first: they arrived before this PD ran,
                // so within the round its tick ends.
                for (pd, watched) in self.watched.iter().enumerate() {
                    if channels.contains(watched.channel) {
                        rounds.answer(pd);
                    }
                }
                if !tick {
                    return Ok(());
                }
                match rounds.tick() {
                    Verdict::Serviced { round } => {
                        if hardware.service() {
                            debug_println!(
                                "{TAG} round {round}: every PD answered; hardware watchdog serviced"
                            );
                        }
                        self.ping();
                    }
                    Verdict::Missed { round, missed } => {
                        let names = Names(self.watched, Some(missed));
                        debug_println!(
                            "{TAG} round {round}: no answer from {names} (highest priority first); the hardware watchdog is no longer serviced and will reset the board"
                        );
                    }
                    Verdict::Stopped => {}
                }
            }
        }
        Ok(())
    }
}

/// The names of the watched PDs - all of them, or those that missed -
/// comma-separated, in priority order.
struct Names(&'static [Watched], Option<nowayos_heartbeat::Missed>);

impl core::fmt::Display for Names {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        for (pd, watched) in self.0.iter().enumerate() {
            if self.1.is_some_and(|missed| !missed.iter().any(|m| m == pd)) {
                continue;
            }
            if !first {
                f.write_str(", ")?;
            }
            f.write_str(watched.name)?;
            first = false;
        }
        Ok(())
    }
}
