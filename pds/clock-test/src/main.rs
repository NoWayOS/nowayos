// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-clock-test`: the test instrument of `systems/clock-test.system`.
//! Two PDs run this program and pick their script
//! by PD name:
//!
//! * `tls_stand_in` stands in for the TLS PD: the clock PD's one source. It
//!   reads the image's build time from the same read-only region as the
//!   clock, and asks the clock the time: not known, its earliest no earlier
//!   than the build time, so a certificate valid from a day before the build
//!   is refused. It sends a sample of the floor plus a day - the date check
//!   now passes, and every reading is exactly that sample carried on at the
//!   drift bound - then three bad samples (from before the floor, narrower
//!   than its round trip, measured a second from now), checking after each
//!   that the time is as it was. 300 ms of monotonic time later it checks
//!   the interval moved on and widened exactly as the drift bound says, and
//!   that no reading of monotonic time - its own or the clock's - went back.
//! * `reader` holds only a channel it may call the clock on: it is refused
//!   an unknown request and a read with a word too many, offers a sample by
//!   call and is refused, and checks the time is unchanged.
//!
//! Every line it prints is fixed text, relative to the floor: no time,
//! Instant or date is printed, so `clock-test.expect` holds on any day. A
//! check that fails prints `gave up` and the reason, which the expect file
//! forbids. A test instrument: it sends bad samples on purpose, so it runs
//! only in test systems (`tools/grant-rules.toml`, `[test_only]`).

#![no_std]
#![no_main]

use nowayos_clock_api::{Client as Clock, ClockError, Message, READ};
use nowayos_ipc::Producer;
use nowayos_region::{RingTx, build_time, read_only, read_write, ring_producer};
use nowayos_time::record::{SAMPLE_RING_SLOT, SAMPLE_RING_SLOTS};
use nowayos_time::{
    Anchor, DRIFT_PPM, Instant, NANOS_PER_SECOND, Reading, Sample, Source, Utc, UtcInterval,
    ValidityError, drift,
};
use nowayos_timer_api::Client as Timer;
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// A millisecond in nanoseconds.
const MS: u64 = 1_000_000;

/// A day in nanoseconds.
const DAY: u64 = 86_400 * NANOS_PER_SECOND;

/// Two days in nanoseconds.
const TWO_DAYS: u64 = 2 * DAY;

/// A year of 365 days in nanoseconds.
const YEAR: u64 = 365 * DAY;

/// How long `tls_stand_in` waits before it asks again: a few hundred
/// milliseconds of monotonic time.
const WAIT_MS: u64 = 300;

/// The id of that timeout.
const WAIT: u64 = 1;

/// `tls_stand_in`'s channels: the timer PD, and the clock PD, which it calls
/// and notifies when a sample waits on its ring.
const STAND_IN_TIMER: Timer = Timer::new(Channel::new(0));
const STAND_IN_CLOCK: Clock = Clock::new(Channel::new(1));

/// `reader`'s one channel: the clock PD, which it may only call.
const READER_CLOCK: Clock = Clock::new(Channel::new(0));

/// `tls_stand_in`'s end of its ring into the clock PD: the messages region
/// read-write, the control region read-only.
type Ring = RingTx<SAMPLE_RING_SLOTS, SAMPLE_RING_SLOT>;

#[protection_domain]
fn init() -> ClockTestPd {
    let name = pd_name().unwrap_or("?");
    debug_println!("NOWAYOS: clock-test {name}: starting");
    let mut pd = ClockTestPd {
        name,
        stand_in: None,
    };
    let result = match name {
        "tls_stand_in" => StandIn::start().map(|stand_in| pd.stand_in = Some(stand_in)),
        "reader" => reader(),
        _ => Err("this PD has no script"),
    };
    if let Err(why) = result {
        debug_println!("NOWAYOS: clock-test {name}: gave up: {why}");
    }
    pd
}

/// A test PD's state: its name, and `tls_stand_in`'s once it waits.
struct ClockTestPd {
    name: &'static str,
    stand_in: Option<StandIn>,
}

/// `tls_stand_in`, waiting for its timeout.
struct StandIn {
    /// The latest Instant it has seen, from the timer or in a reading.
    last: Instant,
    /// The sample the clock took, as an anchor: what every reading must be,
    /// carried on.
    anchor: Anchor,
    /// The last reading, before the wait.
    before: Reading,
}

/// Prints a line of `tls_stand_in`'s.
macro_rules! stand_in {
    ($($arg:tt)*) => {
        debug_println!("NOWAYOS: clock-test tls_stand_in: {}", format_args!($($arg)*))
    };
}

/// Prints a line of `reader`'s.
macro_rules! reader {
    ($($arg:tt)*) => {
        debug_println!("NOWAYOS: clock-test reader: {}", format_args!($($arg)*))
    };
}

/// `when` moved on by `nanos`, or a reason.
fn plus(when: Utc, nanos: u64) -> Result<Utc, &'static str> {
    when.checked_add_nanos(nanos).ok_or("a time past 2554")
}

/// Watches every Instant `tls_stand_in` sees, from the timer PD and in the
/// clock PD's readings: none may be earlier than the one before.
struct Watch {
    last: Instant,
}

impl Watch {
    fn saw(&mut self, at: Instant) -> Result<Instant, &'static str> {
        if at < self.last {
            return Err("a reading of monotonic time went back");
        }
        self.last = at;
        Ok(at)
    }

    fn now(&mut self) -> Result<Instant, &'static str> {
        let now = STAND_IN_TIMER
            .now()
            .map_err(|_| "the timer PD refused a time request")?;
        self.saw(now)
    }

    fn read(&mut self) -> Result<Reading, &'static str> {
        let reading = STAND_IN_CLOCK
            .read()
            .map_err(|_| "the clock PD refused a read")?;
        self.saw(reading.at)?;
        Ok(reading)
    }
}

impl StandIn {
    /// Steps 1 and 2: the floor alone, then one sample taken and three
    /// refused; then the wait for step 3.
    fn start() -> Result<Self, &'static str> {
        let build = build_time().map_err(|_| "could not read the build time")?;
        let ring: Ring = ring_producer(
            read_write(0).map_err(|_| "no ring into the clock")?,
            read_only(0).map_err(|_| "no ring into the clock")?,
        )
        .map_err(|_| "could not bind the ring into the clock")?;
        let mut watch = Watch {
            last: Instant::from_nanos(0),
        };
        let first = watch.now()?;
        let not_before = build
            .checked_sub_nanos(DAY)
            .ok_or("a build time before 1970")?;
        let not_after = plus(build, YEAR)?;

        // At boot: the floor, and the time not known.
        let floor = watch.read()?.interval;
        if floor.source() != Source::BuildTime || floor.is_known() {
            return Err("the clock knew more than the floor before any sample");
        }
        if floor.earliest() < build {
            return Err("the clock's earliest was before the build time");
        }
        stand_in!("the time is not known: its earliest is no earlier than the build time");
        if floor.check_validity(not_before, not_after) != Err(ValidityError::TimeNotKnown) {
            return Err("a date check was not refused while the time was not known");
        }
        stand_in!(
            "a certificate valid from a day before the build refused: {}",
            ValidityError::TimeNotKnown
        );

        // A sample of the floor plus a day: taken, and the date check passes.
        let send = |sample: &Sample| -> Result<(), &'static str> {
            let mut tx = Producer::new(&ring).map_err(|_| "the ring into the clock is broken")?;
            tx.try_send(sample).map_err(|_| "could not send a sample")?;
            STAND_IN_CLOCK.channel().notify();
            Ok(())
        };
        let (sent, received) = (watch.now()?, watch.now()?);
        let sample = Sample::from_date(plus(build, DAY)?, sent, received)
            .map_err(|_| "could not make a sample")?;
        send(&sample)?;
        let interval = UtcInterval::new(sample.earliest, Some(sample.latest), Source::Backend)
            .ok_or("could not make the sample's interval")?;
        let anchor = Anchor::new(sample.received, interval);
        let taken = watch.read()?;
        if taken.interval.source() != Source::Backend || !taken.interval.is_known() {
            return Err("the clock did not take the sample");
        }
        if Ok(taken.interval) != anchor.interval_at(taken.at) {
            return Err("the reading is not the sample carried on at the drift bound");
        }
        if taken.interval.check_validity(not_before, not_after) != Ok(()) {
            return Err("the date check failed with the time known");
        }
        stand_in!(
            "a sample of the floor plus a day taken: the same certificate's date check passes"
        );

        // Three bad samples, each refused by the clock with its own error
        // (its lines), and the time as it was after each.
        let (sent, received) = (watch.now()?, watch.now()?);
        let before_floor =
            Sample::from_date(not_before, sent, received).map_err(|_| "could not make a sample")?;
        let mut received = watch.now()?;
        for _ in 0..1_000 {
            if received > first {
                break;
            }
            received = watch.now()?;
        }
        if received <= first {
            return Err("monotonic time did not move");
        }
        let two_days = plus(build, TWO_DAYS)?;
        let narrow = Sample {
            earliest: two_days,
            latest: two_days,
            sent: first,
            received,
        };
        let sent = watch.now()?;
        let future = Sample::from_date(
            two_days,
            sent,
            sent.checked_add_nanos(NANOS_PER_SECOND)
                .ok_or("an Instant past 584 years")?,
        )
        .map_err(|_| "could not make a sample")?;
        let mut before = taken;
        for (what, bad) in [
            (
                "a sample from a day before the build, earlier than the floor,",
                before_floor,
            ),
            ("a sample narrower than its round trip", narrow),
            ("a sample measured a second from now", future),
        ] {
            send(&bad)?;
            let reading = watch.read()?;
            if Ok(reading.interval) != anchor.interval_at(reading.at) {
                return Err("a refused sample changed the time");
            }
            stand_in!("{what} refused: the time is as it was");
            before = reading;
        }

        STAND_IN_TIMER
            .set_timeout(WAIT, WAIT_MS.saturating_mul(MS))
            .map_err(|_| "the timer PD refused a timeout")?;
        stand_in!("waiting {WAIT_MS} ms of monotonic time");
        Ok(Self {
            last: watch.last,
            anchor,
            before,
        })
    }

    /// Step 3, when the timeout has expired: the interval moved on and
    /// widened exactly as the drift bound says.
    fn after_the_wait(&mut self) -> Result<(), &'static str> {
        let mut watch = Watch { last: self.last };
        let reading = watch.read()?;
        self.last = watch.last;
        if reading.at.saturating_since(self.before.at) < WAIT_MS.saturating_mul(MS) {
            return Err("the timeout fired early");
        }
        let anchor = self.anchor.interval();
        let elapsed = reading.at.saturating_since(self.anchor.at());
        let d = drift(elapsed);
        let earliest = plus(anchor.earliest(), elapsed.saturating_sub(d))?;
        let latest = anchor
            .latest()
            .ok_or("the sample's latest is not known")
            .and_then(|latest| plus(latest, elapsed.saturating_add(d)))?;
        let interval = reading.interval;
        if interval.earliest() != earliest || interval.latest() != Some(latest) {
            return Err("the interval did not move on by the elapsed time and the drift bound");
        }
        let (Some(was), Some(now)) = (self.before.interval.width(), interval.width()) else {
            return Err("the time stopped being known");
        };
        if now <= was
            || anchor
                .width()
                .map(|w| w.saturating_add(d.saturating_mul(2)))
                != Some(now)
        {
            return Err("the interval did not widen by twice the drift");
        }
        stand_in!(
            "{WAIT_MS} ms on, the interval has moved on and widened as the drift bound says, {DRIFT_PPM} ppm"
        );
        stand_in!("no reading of monotonic time went back");
        stand_in!("done");
        Ok(())
    }

    /// Collects the expired timeouts; `true` once the wait is over.
    fn collect(&mut self) -> Result<bool, &'static str> {
        let mut over = false;
        while let Some(id) = STAND_IN_TIMER
            .expired()
            .map_err(|_| "the timer PD refused an expired request")?
        {
            if id != WAIT {
                return Err("a timeout this PD did not set expired");
            }
            over = true;
        }
        Ok(over)
    }
}

/// `reader`'s script: it may only read the time, and moves nothing.
fn reader() -> Result<(), &'static str> {
    let first = READER_CLOCK
        .read()
        .map_err(|_| "the clock PD refused a read")?;
    let unknown = Message::new(99, &[]).ok_or("could not build a request")?;
    match READER_CLOCK.call(&unknown).read_reply() {
        Err(error @ ClockError::UnknownRequest { label: 99 }) => {
            reader!("request 99 refused: {error}");
        }
        _ => return Err("an unknown request was not refused as one"),
    }
    let too_long = Message::new(READ, &[1]).ok_or("could not build a request")?;
    match READER_CLOCK.call(&too_long).read_reply() {
        Err(
            error @ ClockError::BadLength {
                label: READ,
                words: 1,
            },
        ) => reader!("a read with a word too many refused: {error}"),
        _ => return Err("a read with a word too many was not refused as one"),
    }
    // A year on from what the clock says: it must not take it.
    let year_on = plus(first.interval.earliest(), YEAR)?;
    let offered = Sample {
        earliest: year_on,
        latest: plus(year_on, NANOS_PER_SECOND)?,
        sent: first.at,
        received: first.at,
    };
    match READER_CLOCK.call(&Message::offer(&offered)).read_reply() {
        Err(error @ ClockError::NotASource) => {
            reader!("a sample sent by call refused: {error}");
        }
        _ => return Err("a sample sent by call was not refused"),
    }
    let then = READER_CLOCK
        .read()
        .map_err(|_| "the clock PD refused a read")?;
    let moved = Anchor::new(first.at, first.interval)
        .interval_at(then.at)
        .map_err(|_| "a reading of monotonic time went back")?;
    let within = then.interval.source() == first.interval.source()
        && then.interval.earliest() >= moved.earliest()
        && then.interval.earliest() < year_on
        && match (then.interval.latest(), moved.latest()) {
            (Some(latest), Some(bound)) => latest <= bound,
            (None, None) => true,
            _ => false,
        };
    if !within {
        return Err("the time changed");
    }
    reader!("the time is unchanged: within what it read before, moved on");
    reader!("done");
    Ok(())
}

impl Handler for ClockTestPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let name = self.name;
        if let Some(stand_in) = self.stand_in.as_mut()
            && channels.contains(STAND_IN_TIMER.channel())
        {
            let result = stand_in.collect().and_then(|over| {
                if over {
                    stand_in.after_the_wait()
                } else {
                    Ok(())
                }
            });
            if let Err(why) = result {
                debug_println!("NOWAYOS: clock-test {name}: gave up: {why}");
            }
        }
        Ok(())
    }
}
