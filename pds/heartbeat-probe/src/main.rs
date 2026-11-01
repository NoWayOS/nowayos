// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-heartbeat-probe`: a test instrument for the watchdog PD.
//! Every PD that runs it answers the watchdog's heartbeat: each
//! notification on channel 0, its heartbeat channel, is answered by a
//! notification back. The PD called `looper` answers [`LOOP_AFTER`] rounds,
//! then stops answering and loops for ever - with a full budget, so it
//! starves every PD below it, as a looping PD in a product would. It holds
//! nothing but that channel.

#![no_std]
#![no_main]

use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// The probe's end of its heartbeat channel to the watchdog.
const HEARTBEAT: Channel = Channel::new(0);

/// The rounds `looper` answers before it loops.
const LOOP_AFTER: u32 = 10;

#[protection_domain]
fn init() -> ProbePd {
    let name = pd_name().unwrap_or("?");
    debug_println!("NOWAYOS: heartbeat-probe: {name} answering its heartbeat");
    ProbePd { name, answered: 0 }
}

struct ProbePd {
    name: &'static str,
    answered: u32,
}

impl Handler for ProbePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if !channels.contains(HEARTBEAT) {
            return Ok(());
        }
        if self.name == "looper" && self.answered >= LOOP_AFTER {
            debug_println!(
                "NOWAYOS: heartbeat-probe: looper: looping from now on, after {LOOP_AFTER} answers, starving every PD below it"
            );
            loop {
                core::hint::spin_loop();
            }
        }
        HEARTBEAT.notify();
        self.answered = self.answered.saturating_add(1);
        Ok(())
    }
}
