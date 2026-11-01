// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-input-broker`: the input broker PD between the input drivers and
//! the UI (DP-08). It merges the canonical events of every input
//! source into one stream, checks each against the screen, and holds each
//! source to its own rate, so that a hostile or faulty device cannot flood
//! or confuse the UI, whichever driver it comes through.
//!
//! Each source is a shared-memory ring from an input PD - a driver's
//! keyboard or pointer ring - whose messages region is read-only here. On
//! any notification the broker drains its sources in turn, one record from
//! each: every record is copied out and decoded by `nowayos-ipc` (a
//! malformed one is refused, consumed and counted), then
//! `nowayos_input::broker` normalises the event against the screen (an
//! impossible button or contact is refused and counted) and applies the
//! source's own token bucket (events over the rate are dropped and counted).
//! What passes goes to the UI through one ring; what does not fit it is
//! dropped and counted too. Nothing blocks and nothing is queued: a source's
//! excess costs the broker a counter increment, and the other sources keep
//! their own buckets.
//!
//! It reports each kind of trouble at most once a second per source, and a
//! source's counts when its drops have changed, at most once a second, so a
//! flood cannot become a flood of log lines.
//!
//! It holds no device, interrupt or DMA memory - its role, `broker`, forbids
//! the first two (`tools/grant-rules.toml`) - and reads only the counter,
//! through `nowayos-hw`, as the rates' clock. No `unsafe`, no heap.
//!
//! A library and a program, as the display and timer PDs are:
//! NoWayOS's systems run the program; a product's system runs the broker
//! from a program of its own whose `init` returns [`init`]'s, with the same
//! regions and channels.

#![no_std]

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_input::broker::{Broker, SourceCounts, UI_RING_SLOT, UI_RING_SLOTS, Verdict};
use nowayos_input::{InputError, InputEvent, InputLimits};
use nowayos_ipc::{Consumer, DecodeError, Producer, RecvError};
use nowayos_region::{
    RegionError, RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer, screen,
};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: input-broker:";

/// The channel to the UI, on which only the broker notifies. The sources
/// notify on channels of their own; any notification drains every source.
const UI: Channel = Channel::new(0);

/// The most sources: the rings in regions 1 to 3 (`nowayos-region` binds
/// four regions of each kind, and region 0 is the UI's ring).
const SOURCES: usize = 3;

/// Each source's ring: the drivers' slots and slot size. The ring to the UI
/// is the UI's input ring (`nowayos_input::broker::UI_RING_SLOTS`, 64 slots).
const RING_SLOTS: usize = 4;
const RING_SLOT: usize = 32;

/// Each source's rate: up to `BURST` events at once, then `RATE` a second. A
/// key press is two events and a tablet reports at most a few hundred times
/// a second, so a person stays within it; a flood does not.
const BURST: u32 = 64;
const RATE: u64 = 250;

/// What went wrong with one source in one drain: the first malformed record
/// and the first invalid event, and whether any event was over the rate or
/// did not fit the UI's ring.
#[derive(Clone, Copy, Default)]
struct Trouble {
    malformed: Option<DecodeError>,
    invalid: Option<InputError>,
    over_rate: bool,
    undelivered: bool,
}

/// When each kind of trouble was last reported for one source.
#[derive(Clone, Copy, Default)]
struct Warned {
    malformed: Option<u64>,
    invalid: Option<u64>,
    over_rate: Option<u64>,
    undelivered: Option<u64>,
}

/// Starts the broker: binds the ring to the UI and every source's ring the
/// system grants, and reads the screen's size.
#[must_use]
pub fn init() -> BrokerPd {
    let mut pd = BrokerPd {
        ui: None,
        sources: [None, None, None],
        broker: None,
        second: 0,
        warned: [Warned::default(); SOURCES],
        reported: [(SourceCounts::default(), None); SOURCES],
    };
    if let Err(why) = pd.start() {
        debug_println!("{TAG} gave up: {why}");
    }
    pd
}

/// The broker: its rings, its decisions, and what it has reported.
pub struct BrokerPd {
    /// The ring to the UI: messages read-write (`nowayos_rw_region_0`),
    /// control read-only (`nowayos_ro_region_0`).
    ui: Option<RingTx<UI_RING_SLOTS, UI_RING_SLOT>>,
    /// Source N's ring at index N - 1: its control region read-write
    /// (`nowayos_rw_region_N`), its messages read-only
    /// (`nowayos_ro_region_N`); `None` if not granted or given up.
    sources: [Option<RingRx<RING_SLOTS, RING_SLOT>>; SOURCES],
    /// What becomes of each event (`None` until started).
    broker: Option<Broker<SOURCES>>,
    /// Counter ticks in a second.
    second: u64,
    /// When each source's troubles were last reported.
    warned: [Warned; SOURCES],
    /// Each source's counts when last reported, and when.
    reported: [(SourceCounts, Option<u64>); SOURCES],
}

impl BrokerPd {
    fn start(&mut self) -> Result<(), &'static str> {
        self.ui = Some(
            read_write(0)
                .and_then(|messages| ring_producer(messages, read_only(0)?))
                .map_err(|_| "the ring to the UI")?,
        );
        let mut bound = 0usize;
        for (index, slot) in (1..).zip(self.sources.iter_mut()) {
            match read_write(index).and_then(|control| ring_consumer(control, read_only(index)?)) {
                Ok(ring) => {
                    *slot = Some(ring);
                    bound = bound.saturating_add(1);
                }
                // The first region not granted ends the list of sources.
                Err(RegionError::NotMapped { .. }) => break,
                Err(error) => {
                    debug_println!("{TAG} source {index}'s ring: {error}");
                    return Err("a source's ring");
                }
            }
        }
        if bound == 0 {
            return Err("no source is granted");
        }
        self.second = counter_frequency();
        let ticks_per_token = self
            .second
            .checked_div(RATE)
            .ok_or("the counter has no frequency")?;
        // The screen events are checked against: the UI's, from the system
        // description (read-only region 7).
        let (width, height) = screen().map_err(|_| "the screen's size is not granted")?;
        let limits = u16::try_from(width)
            .ok()
            .zip(u16::try_from(height).ok())
            .and_then(|(width, height)| InputLimits::new(width, height))
            .ok_or("the screen's size is impossible")?;
        self.broker =
            Some(Broker::new(limits, BURST, ticks_per_token).ok_or("the counter is too slow")?);
        debug_println!(
            "{TAG} starting; {bound} source{}, screen {width}x{height}, each source {BURST} events at once and {RATE} a second",
            if bound == 1 { "" } else { "s" }
        );
        Ok(())
    }

    /// Drains every source, one record from each in turn, until all are
    /// empty. Returns what went wrong, by source, and whether anything was
    /// sent to the UI.
    fn drain(&mut self, now: u64) -> ([Trouble; SOURCES], bool) {
        let mut trouble = [Trouble::default(); SOURCES];
        let Self {
            ui,
            sources,
            broker,
            ..
        } = self;
        let (Some(memory), Some(broker)) = (ui.as_ref(), broker.as_mut()) else {
            return (trouble, false);
        };
        let Ok(mut tx) = Producer::new(memory) else {
            debug_println!("{TAG} gave up: the ring to the UI is broken");
            *ui = None;
            return (trouble, false);
        };
        let mut sent = false;
        loop {
            let mut any = false;
            for ((source, slot), trouble) in sources.iter_mut().enumerate().zip(&mut trouble) {
                let Some(memory) = slot.as_ref() else {
                    continue;
                };
                let received = match Consumer::new(memory) {
                    Ok(mut rx) => rx.try_recv::<InputEvent>(),
                    Err(error) => Err(RecvError::Ring(error)),
                };
                let event = match received {
                    Ok(Some(event)) => event,
                    Ok(None) => continue,
                    Err(RecvError::Decode(error)) => {
                        any = true;
                        broker.record_malformed(source);
                        trouble.malformed.get_or_insert(error);
                        continue;
                    }
                    Err(error) => {
                        debug_println!(
                            "{TAG} gave up on source {}: {error}",
                            source.saturating_add(1)
                        );
                        *slot = None;
                        continue;
                    }
                };
                any = true;
                match broker.admit(source, event, now) {
                    Verdict::Forward(event) => {
                        if tx.try_send(&event).is_ok() {
                            sent = true;
                        } else {
                            broker.record_undelivered(source);
                            trouble.undelivered = true;
                        }
                    }
                    Verdict::RateLimited => trouble.over_rate = true,
                    Verdict::Invalid(error) => {
                        trouble.invalid.get_or_insert(error);
                    }
                    Verdict::UnknownSource => {}
                }
            }
            if !any {
                break;
            }
        }
        (trouble, sent)
    }

    /// Reports each source's troubles from this drain, each kind at most
    /// once a second, then its counts if its drops have changed, at most
    /// once a second.
    fn report(&mut self, troubles: &[Trouble; SOURCES], now: u64) {
        let second = self.second;
        // Whether a report last made at `last` may be made again now; if
        // so, it is recorded as made.
        let due = |last: &mut Option<u64>| {
            let due = last.is_none_or(|at| now.saturating_sub(at) >= second);
            if due {
                *last = Some(now);
            }
            due
        };
        for ((source, trouble), warned) in troubles.iter().enumerate().zip(&mut self.warned) {
            let n = source.saturating_add(1);
            if let Some(error) = trouble.malformed
                && due(&mut warned.malformed)
            {
                debug_println!("{TAG} refused a record from source {n}: {error}");
            }
            if let Some(error) = trouble.invalid
                && due(&mut warned.invalid)
            {
                debug_println!("{TAG} refused an event from source {n}: {error}");
            }
            if trouble.over_rate && due(&mut warned.over_rate) {
                debug_println!("{TAG} source {n} is over its rate: dropping and counting");
            }
            if trouble.undelivered && due(&mut warned.undelivered) {
                debug_println!(
                    "{TAG} the UI's ring is full: dropping and counting events from source {n}"
                );
            }
        }
        let Some(broker) = self.broker.as_ref() else {
            return;
        };
        for (source, (reported, at)) in self.reported.iter_mut().enumerate() {
            let Some(counts) = broker.counts(source) else {
                continue;
            };
            if counts.dropped() == reported.dropped() || !due(at) {
                continue;
            }
            *reported = counts;
            debug_println!(
                "{TAG} source {} so far: {} forwarded, {} over the rate, {} invalid, {} malformed, {} did not fit the UI's ring",
                source.saturating_add(1),
                counts.forwarded,
                counts.rate_limited,
                counts.invalid,
                counts.malformed,
                counts.undelivered
            );
        }
    }
}

impl Handler for BrokerPd {
    type Error = Infallible;

    fn notified(&mut self, _channels: ChannelSet) -> Result<(), Self::Error> {
        let now = counter();
        let (troubles, sent) = self.drain(now);
        if sent {
            UI.notify();
        }
        self.report(&troubles, now);
        Ok(())
    }
}
