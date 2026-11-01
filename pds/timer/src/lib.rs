// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-timer`: the timer PD.
//!
//! Other PDs ask it the time, and for timeouts, by protected procedure call,
//! as `nowayos-timer-api` describes. It answers the time from the CPU's
//! counter, read through `nowayos-hw`, as nanoseconds since the counter
//! started - a time that never goes back. It keeps each client's timeouts in
//! `nowayos_timer_api::schedule::Timeouts`, arms a hardware timer for the
//! earliest deadline, and when the timer's interrupt arrives notifies each
//! client with a newly expired timeout - once, however many of its deadlines
//! passed. It is the only PD granted the timer's interrupt.
//!
//! The hardware timer is whichever the system grants it ([`Alarm`]), so the
//! program is the same on every board and never tests for one:
//!
//! * no device: the CPU's EL1 physical timer, through system registers -
//!   on QEMU, whose kernel exports it to user level (`phys_timer`);
//! * the system timer's registers, bound as device 0: Broadcom's system
//!   timer, channel 1 - on the Pi 5, whose kernel does not export the CPU's
//!   timer (`system_timer`, `systems/rpi5b_2gb/timer-demo.system`).
//!
//! Either way the time and every deadline are the counter's, and a timeout
//! expires only once the counter has passed it: the hardware timer only
//! wakes the PD, and one that fires early is armed again.
//!
//! Channels: the interrupt on channel 0; clients on channels 1 to
//! [`CLIENTS`]. A call on any other channel is refused.
//!
//! No `unsafe` code (the workspace lints forbid it). The client interface
//! names no board: every board's timer PD answers the same calls.
//!
//! A library and a program: NoWayOS's systems run the program, and a
//! product system runs the timer from a program of its own that returns
//! [`init`].

#![no_std]

use nowayos_hw::cpu::{counter, counter_frequency, set_timer, stop_timer};
use nowayos_hw::system_timer::{self, Channel as SystemTimerChannel, SystemTimer};
use nowayos_hw::{HwError, device};
use nowayos_timer_api::schedule::{Timeouts, alarm_ticks};
use nowayos_timer_api::{Message, Reply};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, MessageInfo, debug_println, with_msg_regs,
    with_msg_regs_mut,
};

/// The channel the timer's interrupt arrives on (`<irq id="0">`).
const TIMER_IRQ: Channel = Channel::new(0);

/// The most clients: on channels 1 to `CLIENTS` - twelve, so a product's
/// system can give the timer to its UART input, two USB host PDs, an
/// application PD for each of several examples and the fan PD.
const CLIENTS: usize = 12;

/// The device the system timer's registers are bound as, when the system
/// grants them: `nowayos_device_0_vaddr` and `_size`.
const SYSTEM_TIMER_DEVICE: usize = 0;

/// The system timer's channel for deadlines: channel 1, the first of the
/// ARM's two (channels 0 and 2 are the VideoCore's). The system takes its
/// interrupt, `irq="@system_timer[1]"`.
const CHANNEL: SystemTimerChannel = SystemTimerChannel::One;

/// The shortest delay the system timer is armed for, in its ticks
/// (microseconds): a deadline already due fires this soon.
const MIN_DELAY: u32 = 2;

/// The longest: 2^30 ticks, about 17.9 minutes - under half the low word's
/// wrap of about 71.6 minutes, so a compare value is never ambiguous. A
/// later deadline is reached by arming again when this one fires.
const MAX_DELAY: u32 = 1 << 30;

/// How often arming the system timer is tried, the delay doubling each
/// time, before it is reported as failed.
const ARM_ATTEMPTS: u32 = 8;

/// The time the start-up check watches the system timer count for: 1 ms of
/// the counter. Bounded also by [`CHECK_SPINS`] reads, in case the counter
/// does not move.
const CHECK_DIVISOR: u64 = 1_000;

/// The most counter reads the start-up check waits for.
const CHECK_SPINS: u32 = 10_000_000;

/// Starts the timer PD: the hardware timer the system grants, stopped, and
/// any match left from before boot cleared; no timeouts yet. Reports on
/// serial what it found: the counter's frequency and which hardware timer
/// wakes it.
#[must_use]
pub fn init() -> TimerPd {
    debug_println!("NOWAYOS: timer: starting");
    let frequency = counter_frequency();
    debug_println!("NOWAYOS: timer: the counter runs at {frequency} Hz");
    if frequency == 0 {
        debug_println!(
            "NOWAYOS: timer: could not use the counter: CNTFRQ_EL0 reads 0, so the firmware did not set its frequency"
        );
    }
    let mut alarm = Alarm::bind(frequency);
    alarm.clear();
    alarm.stop();
    TimerPd {
        frequency,
        alarm,
        timeouts: Timeouts::new(),
    }
}

/// The hardware timer that wakes the timer PD at the earliest deadline: the
/// one the system grants.
enum Alarm {
    /// The CPU's EL1 physical timer, through system registers: its compare
    /// value is a counter reading, so a deadline is armed as it is.
    Cpu,
    /// The system timer's [`CHANNEL`]: armed some of its own ticks ahead
    /// (see [`Alarm::set`]).
    System(SystemTimer<'static>),
}

impl Alarm {
    /// The hardware timer the system grants: the system timer if its
    /// registers are bound as [`SYSTEM_TIMER_DEVICE`], otherwise the CPU's.
    /// Reports which on serial, and for the system timer that its counter
    /// moves and at about what rate.
    fn bind(frequency: u64) -> Self {
        match device(SYSTEM_TIMER_DEVICE).and_then(SystemTimer::new) {
            Ok(timer) => {
                debug_println!(
                    "NOWAYOS: timer: deadlines from the system timer, channel {}",
                    CHANNEL.number()
                );
                check(&timer, frequency);
                Self::System(timer)
            }
            Err(HwError::NotMapped { .. }) => {
                debug_println!("NOWAYOS: timer: deadlines from the CPU's EL1 physical timer");
                Self::Cpu
            }
            // A description tools/check-system refuses when the image is
            // built; if one ever ran, the CPU's timer faults on a board that
            // does not export it, and the monitor reports the fault.
            Err(err) => {
                debug_println!("NOWAYOS: timer: could not use the system timer: {err}");
                Self::Cpu
            }
        }
    }

    /// Clears a passed deadline's interrupt before the timeouts are
    /// expired: the system timer's match, which stays set until cleared,
    /// so a match that comes while the PD runs on is not lost. (The CPU's
    /// timer clears when it is armed again or stopped.)
    fn clear(&mut self) {
        if let Self::System(timer) = self {
            timer.clear_match(CHANNEL);
        }
    }

    /// Arms the timer for `deadline`, a counter reading; `now` is the
    /// counter, at `frequency` ticks per second.
    ///
    /// The system timer is armed for the deadline's distance in its own
    /// ticks, rounded up, from [`MIN_DELAY`] to [`MAX_DELAY`]. Its match
    /// comes only when its counter's low word equals the compare value, so
    /// if that may have passed before the value took effect it is armed
    /// again, further ahead, up to [`ARM_ATTEMPTS`] times.
    fn set(&mut self, deadline: u64, now: u64, frequency: u64) {
        match self {
            Self::Cpu => set_timer(deadline),
            Self::System(timer) => {
                let ticks = alarm_ticks(deadline, now, frequency, system_timer::FREQUENCY);
                let mut delay = u32::try_from(ticks)
                    .unwrap_or(MAX_DELAY)
                    .clamp(MIN_DELAY, MAX_DELAY);
                for _ in 0..ARM_ATTEMPTS {
                    match timer.arm(CHANNEL, delay) {
                        Ok(_) => return,
                        Err(_) => delay = delay.saturating_mul(2).min(MAX_DELAY),
                    }
                }
                debug_println!(
                    "NOWAYOS: timer: could not arm the system timer in {ARM_ATTEMPTS} attempts"
                );
            }
        }
    }

    /// Stops the timer, so it does not fire. A system timer channel cannot
    /// be stopped - its compare register always compares - so it is left:
    /// if its counter's low word comes round to the compare value again,
    /// about 71.6 minutes on, the interrupt is cleared, nothing expires and
    /// nothing is armed.
    fn stop(&mut self) {
        match self {
            Self::Cpu => stop_timer(),
            Self::System(_) => {}
        }
    }
}

/// Reports, at start-up, that the system timer's counter moves, and at
/// about what rate against the CPU's counter (at `frequency`): it should
/// be [`system_timer::FREQUENCY`]. Watches it for 1 ms of the counter.
fn check(timer: &SystemTimer<'_>, frequency: u64) {
    let status = timer.status();
    let (start, counter_start) = (timer.counter(), counter());
    let wait = frequency.checked_div(CHECK_DIVISOR).unwrap_or(0);
    let mut counter_end = counter_start;
    for _ in 0..CHECK_SPINS {
        counter_end = counter();
        if counter_end.wrapping_sub(counter_start) >= wait {
            break;
        }
    }
    let ticks = timer.counter().wrapping_sub(start);
    let rate = u128::from(ticks)
        .checked_mul(u128::from(frequency))
        .and_then(|scaled| scaled.checked_div(u128::from(counter_end.wrapping_sub(counter_start))))
        .map_or(0, |rate| u64::try_from(rate).unwrap_or(u64::MAX));
    debug_println!(
        "NOWAYOS: timer: the system timer reads {start} with match bits {status:#x}, and counts at about {rate} Hz (nominally {} Hz)",
        system_timer::FREQUENCY
    );
    if ticks == 0 {
        debug_println!(
            "NOWAYOS: timer: could not see the system timer count: it read {start} before and after 1 ms of the counter"
        );
    }
}

/// The timer PD's state. Make it with [`init`].
pub struct TimerPd {
    /// The counter's frequency, read once at start.
    frequency: u64,
    /// The hardware timer that wakes the PD.
    alarm: Alarm,
    /// Every client's timeouts.
    timeouts: Timeouts<CLIENTS>,
}

impl TimerPd {
    /// Arms the hardware timer for the earliest waiting deadline, or stops
    /// it if nothing is waiting.
    fn rearm(&mut self) {
        match self.timeouts.next_deadline() {
            Some(deadline) => self.alarm.set(deadline, counter(), self.frequency),
            None => self.alarm.stop(),
        }
    }
}

impl Handler for TimerPd {
    type Error = Infallible;

    fn protected(
        &mut self,
        channel: Channel,
        msg_info: MessageInfo,
    ) -> Result<MessageInfo, Self::Error> {
        let request = with_msg_regs(|regs| {
            Message::received(msg_info.label(), msg_info.count(), |index| {
                regs.get(index).copied().unwrap_or_default()
            })
        });
        // Client n calls on channel n + 1; channel 0, the interrupt, is not a
        // client's, and wraps to an index no client has.
        let client = channel.index().wrapping_sub(1);
        let result = request.request().and_then(|request| {
            self.timeouts
                .serve(client, request, counter(), self.frequency)
        });
        if matches!(result, Ok(Reply::Set | Reply::Cancelled(true))) {
            self.rearm();
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
        if !channels.contains(TIMER_IRQ) {
            return Ok(());
        }
        self.alarm.clear();
        let notify = self.timeouts.expire(counter());
        for client in 0..CLIENTS {
            let bit = u32::try_from(client)
                .ok()
                .and_then(|client| 1_u64.checked_shl(client))
                .unwrap_or(0);
            if notify & bit != 0 {
                Channel::new(client.saturating_add(1)).notify();
            }
        }
        // Re-arm (or stop) before acknowledging: the interrupt is
        // level-triggered, and stays asserted while the old deadline has
        // passed (the CPU's timer) or its match is set (the system timer's,
        // cleared above; set again only by the new deadline).
        self.rearm();
        if TIMER_IRQ.irq_ack().is_err() {
            debug_println!("NOWAYOS: timer: could not acknowledge the timer interrupt");
        }
        Ok(())
    }
}
