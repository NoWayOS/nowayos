// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-fan`: the fan PD. It runs the Raspberry Pi 5's active
//! cooler, which the firmware leaves off:
//!
//! 1. at start, the fan at full speed (`nowayos-hw`'s [`rp1_fan`]: PWM1's
//!    clock on, GPIO 45 handed to PWM1's channel 3);
//! 2. every second, the SoC's temperature from the firmware
//!    (`nowayos-hw`'s [`vc_mailbox`]);
//! 3. after [`KICK_SECS`], the fan's speed set by the device tree's cooling
//!    curve ([`Curve`]): off below 50 °C, up a level at 50, 60, 67.5 and
//!    75 °C, down one 5 °C below each;
//! 4. every [`REPORT_SECS`], and at each change of level, a line with the
//!    temperature, the level and the fan's speed in rpm (RP1's firmware
//!    measures it).
//!
//! Without a temperature the fan runs at full speed: never off when the PD
//! cannot tell. A reading the firmware does not answer in time - this PD
//! pre-empted while it waits, or the firmware busy - is asked for again the
//! next second, the late answer drained first; the report gives the
//! last reading's age, and says "no temperature" only once none has come for
//! [`GIVE_UP_SECS`], or the mailbox could not be had at all.
//!
//! Devices: 0 RP1's `clocks_main`, 1 RP1's `io_bank2`, 2 RP1's PWM1, 3 the
//! VideoCore mailbox's page; DMA region 0, the mailbox's buffer. Channel 0
//! the timer PD (`pp`), which times its seconds. The Pi 5 only.

#![no_std]

use nowayos_hw::rp1_fan::{self, Curve, Fan, RANGE};
use nowayos_hw::vc_mailbox::{self, Mailbox};
use nowayos_hw::{HwError, device, dma_region};
use nowayos_timer_api::Client;
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The timer PD, which times the seconds.
const TIMER: Client = Client::new(Channel::new(0));

/// The one timeout it sets.
const TICK: u64 = 1;

/// A second in nanoseconds.
const SECOND: u64 = 1_000_000_000;

/// Full speed for this long at start, before the curve: a fan started slow
/// may not start at all.
pub const KICK_SECS: u32 = 3;

/// A report this often, in seconds.
pub const REPORT_SECS: u32 = 10;

/// Seconds without a reading before a report says there is no temperature.
pub const GIVE_UP_SECS: u32 = 30;

/// Starts the fan PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> FanPd {
    let mut pd = FanPd {
        fan: None,
        mailbox: None,
        curve: Curve::new(),
        seconds: 0,
        temperature: None,
        missed: 0,
    };
    match start() {
        Ok((fan, started)) => {
            debug_println!(
                "NOWAYOS: fan: PWM1's channel 3 drives GPIO45 at 24 kHz: full speed for {KICK_SECS} s to start"
            );
            if started.clock_was_running {
                debug_println!("NOWAYOS: fan: PWM1's clock was already running");
            }
            if !started.pin_driven {
                debug_println!("NOWAYOS: fan: GPIO45's pad is not driven: the fan may not follow");
            }
            pd.fan = Some(fan);
        }
        Err(error) => {
            debug_println!("NOWAYOS: fan: gave up: {error}");
            return pd;
        }
    }
    match device(3).and_then(|regs| vc_mailbox::mailbox(regs, dma_region(0)?)) {
        Ok(mailbox) => pd.mailbox = Some(mailbox),
        Err(error) => no_temperature(error),
    }
    pd.read_temperature();
    if let Some(millidegrees) = pd.temperature {
        let (whole, tenth) = degrees(millidegrees);
        debug_println!("NOWAYOS: fan: the SoC at {whole}.{tenth} C");
    }
    pd.arm();
    pd
}

/// The fan started, from its three pages.
fn start() -> Result<(Fan, rp1_fan::Started), HwError> {
    rp1_fan::start(device(0)?, device(1)?, device(2)?)
}

fn no_temperature(error: HwError) {
    debug_println!("NOWAYOS: fan: no temperature: the fan stays at full speed");
    debug_println!("NOWAYOS: fan: the firmware's temperature: {error}");
}

/// A temperature in thousandths of a degree as whole degrees and tenths.
fn degrees(millidegrees: i32) -> (i32, u32) {
    (
        millidegrees / 1000,
        (millidegrees % 1000).unsigned_abs() / 100,
    )
}

/// The fan PD, as Microkit runs it.
pub struct FanPd {
    /// The fan, once started; `None` if it could not be.
    fan: Option<Fan>,
    /// The mailbox; `None` if it could not be had, and the fan stays at full
    /// speed.
    mailbox: Option<Mailbox>,
    /// The fan's level on the cooling curve.
    curve: Curve,
    /// Seconds since start.
    seconds: u32,
    /// The last temperature read, in thousandths of a degree.
    temperature: Option<i32>,
    /// Readings missed in a row since it.
    missed: u32,
}

impl FanPd {
    /// Sets the next second's timeout; a timer that refuses leaves the fan
    /// as it is.
    fn arm(&self) {
        if TIMER.set_timeout(TICK, SECOND).is_err() {
            debug_println!("NOWAYOS: fan: the timer refused a timeout: the fan stays as it is");
        }
    }

    /// Asks the firmware for the temperature; a reading missed runs the fan
    /// at full speed until the next one comes, asked for each second.
    fn read_temperature(&mut self) {
        let Some(mailbox) = self.mailbox.as_mut() else {
            self.temperature = None;
            return;
        };
        match mailbox.temperature() {
            Ok(millidegrees) => {
                if self.missed > 0 {
                    debug_println!(
                        "NOWAYOS: fan: the firmware answered again, after {} missed",
                        self.missed
                    );
                }
                self.temperature = Some(millidegrees);
                self.missed = 0;
            }
            Err(error) => {
                if self.missed == 0 {
                    debug_println!(
                        "NOWAYOS: fan: the firmware's temperature: {error}: full speed, asking again each second"
                    );
                }
                self.missed = self.missed.saturating_add(1);
                self.set_duty(RANGE);
            }
        }
    }

    fn set_duty(&mut self, duty: u32) {
        if let Some(fan) = self.fan.as_mut()
            && fan.set_duty(duty).is_err()
        {
            debug_println!("NOWAYOS: fan: could not set the fan's speed");
        }
    }

    /// A second has passed.
    fn second(&mut self) {
        self.seconds = self.seconds.saturating_add(1);
        if self.seconds == 1
            && let Some(fan) = self.fan.as_mut()
        {
            match fan.counting() {
                Ok(true) => debug_println!("NOWAYOS: fan: PWM1 is counting"),
                _ => debug_println!("NOWAYOS: fan: PWM1 is not counting: its clock is off"),
            }
        }
        self.read_temperature();
        if self.seconds < KICK_SECS {
            return;
        }
        let was = self.curve.level();
        if let Some(millidegrees) = self.temperature.filter(|_| self.missed == 0) {
            let level = self.curve.update(millidegrees);
            if self.seconds == KICK_SECS {
                debug_println!("NOWAYOS: fan: started: following the SoC's temperature");
            }
            // Its level's duty, when the level changes - or when the fan
            // runs at another, as after readings were missed (full speed).
            let duty = Curve::duty(level);
            let elsewhere = self.fan.as_ref().is_some_and(|fan| fan.duty() != duty);
            if level != was || self.seconds == KICK_SECS || elsewhere {
                self.set_duty(duty);
                self.report();
            }
        }
        if self.seconds.is_multiple_of(REPORT_SECS) {
            self.report();
        }
    }

    /// The temperature, the level and the fan's speed.
    fn report(&mut self) {
        let Some(fan) = self.fan.as_mut() else {
            return;
        };
        let rpm = fan.rpm().unwrap_or(0);
        let percent = fan.duty().saturating_mul(100) / RANGE;
        match self.temperature {
            Some(millidegrees) if self.missed == 0 => {
                let (whole, tenth) = degrees(millidegrees);
                debug_println!(
                    "NOWAYOS: fan: {whole}.{tenth} C, level {} of 4, {percent}%, {rpm} rpm",
                    self.curve.level()
                );
            }
            Some(millidegrees) if self.missed < GIVE_UP_SECS => {
                let (whole, tenth) = degrees(millidegrees);
                debug_println!(
                    "NOWAYOS: fan: {whole}.{tenth} C {} s ago, {percent}%, {rpm} rpm",
                    self.missed
                );
            }
            _ => debug_println!("NOWAYOS: fan: no temperature, {percent}%, {rpm} rpm"),
        }
    }
}

impl Handler for FanPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(TIMER.channel()) {
            loop {
                match TIMER.expired() {
                    Ok(Some(TICK)) => {
                        self.second();
                        self.arm();
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        debug_println!(
                            "NOWAYOS: fan: the timer refused a request: the fan stays as it is"
                        );
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
