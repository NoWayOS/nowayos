// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-clock`: the clock PD.
//!
//! It owns wall-clock time. At start-up it reads the image's build time -
//! the floor - from the read-only configuration the system gives it
//! (`nowayos_region::build_time`, read-only region 6) and the time from the
//! timer PD, and from then on holds UTC as `nowayos-time`'s [`Clock`] does:
//! never earlier than the floor moved on by the monotonic time since, and
//! with no latest until a source answers. It keeps the newest valid sample
//! as its anchor and carries it on at the drift bound. It holds no key, no
//! device and no interrupt, and parses no network data: a source parses its
//! `Date` and sends a typed sample.
//!
//! Channels, by this PD's convention:
//!
//! * **0, the timer PD**, which it calls for monotonic time (`pp="true"` on
//!   this end; the timer runs above it). Neither end notifies.
//! * **1 to [`SOURCES`], a source's** - when, and only when, the system
//!   grants its ring: source `k`'s samples come on a ring whose messages
//!   region is read-only region `k - 1` here and whose control region is
//!   read-write region `k - 1`, and it notifies on channel `k` when samples
//!   wait. A channel whose ring is not granted carries no samples, whatever
//!   its number: the grants decide which PDs move the time. A source's
//!   samples are the backend's ([`Source::Backend`]): the TLS PD's, or in a
//!   test system a test PD's in its place.
//! * **Any channel, a reader's**: a PD that calls (`pp="true"` on its end,
//!   at a lower priority) is answered as `nowayos-clock-api` describes - the
//!   time as a reading, or a typed refusal. A sample offered by call is
//!   refused, and so is a notification on a channel with no ring.
//!
//! It fails closed: if it cannot read the build time or the time at
//! start-up, it answers every read with no time.
//!
//! No `unsafe` code (the workspace lints forbid it). A library and a
//! program: NoWayOS's systems run the program, and a product system runs the
//! clock from a program of its own that returns [`init`].

#![no_std]

use nowayos_clock_api::{ClockError, Message, serve, take_samples};
use nowayos_region::{RegionError, RingRx, read_only, read_write, ring_consumer};
use nowayos_time::record::{SAMPLE_RING_SLOT, SAMPLE_RING_SLOTS};
use nowayos_time::{Clock, Source};
use nowayos_timer_api::Client as Timer;
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, MessageInfo, debug_println, with_msg_regs,
    with_msg_regs_mut,
};

/// The channel to the timer PD.
const TIMER: Timer = Timer::new(Channel::new(0));

/// The most sources: on channels 1 to `SOURCES`, their rings on regions 0
/// to `SOURCES - 1` - the TLS PD, and later a Roughtime client.
pub const SOURCES: usize = 2;

/// Microkit's channel numbers: 0 to 61 (rust-sel4's `Channel` refuses any
/// other).
const CHANNELS: usize = 62;

/// The clock PD's end of a source's ring.
type Ring = RingRx<SAMPLE_RING_SLOTS, SAMPLE_RING_SLOT>;

/// Starts the clock PD: the floor from the image's build time at the
/// Instant the timer gives now, and the rings the system grants. Reports on
/// serial what it found, in fixed text that names no time.
#[must_use]
pub fn init() -> ClockPd {
    debug_println!("NOWAYOS: clock: starting");
    let mut pd = ClockPd {
        clock: None,
        sources: [const { None }; SOURCES],
    };
    let build_time = match nowayos_region::build_time() {
        Ok(build_time) => build_time,
        Err(err) => {
            debug_println!(
                "NOWAYOS: clock: could not read the image's build time: {err}; every reading is refused"
            );
            return pd;
        }
    };
    let start = match TIMER.now() {
        Ok(start) => start,
        Err(err) => {
            debug_println!(
                "NOWAYOS: clock: could not read monotonic time from the timer PD: {err}; every reading is refused"
            );
            return pd;
        }
    };
    pd.clock = Some(Clock::new(build_time, start));
    debug_println!(
        "NOWAYOS: clock: the floor is the image's build time, from its read-only configuration; the time is not known"
    );
    for (index, slot) in pd.sources.iter_mut().enumerate() {
        let channel = index.saturating_add(1);
        match bind(index) {
            Ok(Some(ring)) => {
                *slot = Some(ring);
                debug_println!("NOWAYOS: clock: samples on channel {channel}'s ring");
            }
            Ok(None) => {}
            Err(err) => debug_println!(
                "NOWAYOS: clock: could not bind channel {channel}'s ring: {err}; it carries no samples"
            ),
        }
    }
    if pd.sources.iter().all(Option::is_none) {
        debug_println!("NOWAYOS: clock: no source's ring is granted: the time stays not known");
    }
    pd
}

/// Binds source `index`'s ring, if the system grants it: its control region
/// read-write and its messages region read-only. `None` if it grants
/// neither.
fn bind(index: usize) -> Result<Option<Ring>, RegionError> {
    match (read_write(index), read_only(index)) {
        (Err(RegionError::NotMapped { .. }), Err(RegionError::NotMapped { .. })) => Ok(None),
        (control, messages) => ring_consumer(control?, messages?).map(Some),
    }
}

/// The clock PD's state. Make it with [`init`].
pub struct ClockPd {
    /// Wall-clock time: `None` if the PD could not start, when every
    /// reading is refused.
    clock: Option<Clock>,
    /// Each source's ring, where the system grants one; a ring whose source
    /// broke it is dropped.
    sources: [Option<Ring>; SOURCES],
}

impl Handler for ClockPd {
    type Error = Infallible;

    fn protected(
        &mut self,
        channel: Channel,
        msg_info: MessageInfo,
    ) -> Result<MessageInfo, Self::Error> {
        // Copied out first: calling the timer reuses the message registers.
        let request = with_msg_regs(|regs| {
            Message::received(msg_info.label(), msg_info.count(), |index| {
                regs.get(index).copied().unwrap_or_default()
            })
        });
        let result = match self.clock.as_mut() {
            Some(clock) => serve(clock, &request, || TIMER.now().ok()),
            None => request.request().and(Err(ClockError::NoTime)),
        };
        if result == Err(ClockError::NotASource) {
            debug_println!(
                "NOWAYOS: clock: a sample offered by call on channel {} refused: {}; the time is as it was",
                channel.index(),
                ClockError::NotASource
            );
        }
        let reply = Message::reply_for(result);
        let words = reply.words().unwrap_or_default();
        with_msg_regs_mut(|regs| {
            for (reg, word) in regs.iter_mut().zip(words) {
                *reg = *word;
            }
        });
        Ok(MessageInfo::new(reply.label(), words.len()))
    }

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        // Monotonic time, read once after the notification and only if a
        // ring is to be read: no sample's answer arrived after it.
        let mut now = None;
        for index in 0..CHANNELS {
            if !channels.contains(Channel::new(index)) {
                continue;
            }
            let source = index.checked_sub(1);
            let ring = source
                .and_then(|source| self.sources.get(source))
                .and_then(Option::as_ref);
            let (Some(ring), Some(clock)) = (ring, self.clock.as_mut()) else {
                debug_println!(
                    "NOWAYOS: clock: a notification on channel {index} refused: it has no ring of samples"
                );
                continue;
            };
            let at = match now {
                Some(at) => at,
                None => match TIMER.now() {
                    Ok(at) => *now.insert(at),
                    Err(err) => {
                        debug_println!(
                            "NOWAYOS: clock: could not read monotonic time from the timer PD: {err}; no sample taken"
                        );
                        return Ok(());
                    }
                },
            };
            let taken = take_samples(clock, ring, Source::Backend, at, |outcome| {
                debug_println!("NOWAYOS: clock: a sample on channel {index} {outcome}");
            });
            if let Err(err) = taken {
                debug_println!(
                    "NOWAYOS: clock: could not read channel {index}'s ring: {err}; it carries no more samples"
                );
                if let Some(slot) = source.and_then(|source| self.sources.get_mut(source)) {
                    *slot = None;
                }
            }
        }
        Ok(())
    }
}
