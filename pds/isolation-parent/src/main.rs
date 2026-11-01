// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-isolation-parent`: the fault-handling side of NoWayOS's isolation
//! tests (`systems/isolation-test.system`, `systems/ui-isolation.system`,
//! `systems/input-isolation.system`, `systems/net-isolation.system`).
//!
//! The system description makes this PD the parent of the probe PDs, each of
//! which attempts one memory access its capabilities do not grant. seL4
//! stops the probe and sends its fault to this PD.
//! For each fault this PD:
//!
//! 1. stops the probe for good (suspends its thread, as libmicrokit's
//!    `microkit_pd_stop` does, and never replies to the fault) — what the
//!    fault policy keeps for test systems;
//! 2. decodes and reports the fault on the debug console (`nowayos-fault`);
//! 3. notifies the bystander PD, which answers only if it is still running.
//!
//! It reads each fault's label before parsing it, and decodes only a memory
//! fault: rust-sel4 v5.0.0's parser panics on labels it does not know, such
//! as a debug kernel's debug exception - a panic in the child (the fault
//! policy's first rule).
//!
//! When every probe has faulted and the bystander has answered each time, it
//! prints the test's summary line. Which probes it expects - the test - is
//! chosen by this PD's name ([`TESTS`]).
//!
//! Capabilities: its children's faults and thread capabilities (which
//! Microkit gives every parent), and one channel to the bystander. No memory
//! regions, no `unsafe`, no heap.
//!
//! `Child::tcb` is public but hidden from rust-sel4 v5.0.0's documentation;
//! it is the only way there to reach a child's thread, and it is exactly what
//! libmicrokit's public `microkit_pd_stop` uses. Recheck when rust-sel4 is
//! bumped.

#![no_std]
#![no_main]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use core::fmt;

use nowayos_fault::VmFault;
use sel4_microkit::{
    Channel, ChannelSet, Child, Handler, Infallible, MessageInfo, debug_println, pd_name,
    protection_domain,
};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: isolation-parent:";

/// The channel to the bystander.
const BYSTANDER: Channel = Channel::new(0);

/// One isolation test: the system it belongs to, and its probes.
struct Test {
    /// The system's name, which starts the summary line.
    system: &'static str,
    /// The child PDs, by child `id` in the system description.
    children: &'static [&'static str],
}

/// The tests this program runs, by the parent's PD name.
static TESTS: [(&str, Test); 4] = [
    (
        "isolation_parent",
        Test {
            system: "isolation-test",
            children: &["probe_unmapped", "probe_readonly"],
        },
    ),
    (
        "ui_isolation_parent",
        Test {
            system: "ui-isolation",
            children: &[
                "display_writes_framebuffer",
                "ui_writes_display_index",
                "ui_reads_display_private",
                "ui_reads_device",
            ],
        },
    ),
    (
        "input_isolation_parent",
        Test {
            system: "input-isolation",
            children: &["ui_writes_driver_ring", "ui_writes_broker_ring"],
        },
    ),
    (
        "net_isolation_parent",
        Test {
            system: "net-isolation",
            children: &["net_writes_driver_dma", "net_writes_rx_buffers"],
        },
    ),
];

#[protection_domain]
fn init() -> ParentPd {
    let test = match pd_name() {
        Ok(name) => TESTS
            .iter()
            .find(|(parent, _)| *parent == name)
            .map(|(_, test)| test),
        Err(_) => None,
    };
    match test {
        Some(test) => debug_println!("{TAG} starting; children {}", Children(test.children)),
        None => debug_println!("{TAG} no test is defined for this PD's name"),
    }
    ParentPd {
        test,
        faults: 0,
        answers: 0,
    }
}

/// A list of children as the start-up line shows it: `a (0), b (1) and c
/// (2)`.
struct Children(&'static [&'static str]);

impl fmt::Display for Children {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last = self.0.len().saturating_sub(1);
        for (id, name) in self.0.iter().enumerate() {
            if id > 0 {
                f.write_str(if id == last { " and " } else { ", " })?;
            }
            write!(f, "{name} ({id})")?;
        }
        Ok(())
    }
}

/// The test being run, and counts of what has happened so far.
struct ParentPd {
    /// The test, if this PD's name names one.
    test: Option<&'static Test>,
    /// Child faults received.
    faults: usize,
    /// Answers from the bystander.
    answers: usize,
}

impl ParentPd {
    /// The probes this parent expects.
    fn children(&self) -> &'static [&'static str] {
        self.test.map_or(&[], |test| test.children)
    }
}

impl Handler for ParentPd {
    type Error = Infallible;

    fn fault(
        &mut self,
        child: Child,
        msg_info: MessageInfo,
    ) -> Result<Option<MessageInfo>, Self::Error> {
        let name = self
            .children()
            .get(child.index())
            .copied()
            .unwrap_or("unknown child");
        // Copy the fault out of the message registers first: the suspend
        // below is a kernel call that reuses them.
        // The label first: only a memory fault is parsed.
        let fault = (msg_info.label() == sel4::sys::seL4_Fault_tag::seL4_Fault_VMFault)
            .then(|| msg_info.fault());
        // Stop the child explicitly, as libmicrokit's microkit_pd_stop does
        // (seL4_TCB_Suspend on the child's TCB, which Microkit gives the
        // parent). It never runs again, and seL4 cancels its pending fault
        // reply cleanly instead of when this PD next waits.
        let stopped = child.tcb().tcb_suspend();
        let state = if stopped.is_ok() {
            "stopped"
        } else {
            "faulted (NOT stopped)"
        };
        match fault {
            Some(sel4::Fault::VmFault(fault)) => {
                let fault =
                    VmFault::new(fault.ip(), fault.addr(), fault.fsr(), fault.is_prefetch());
                debug_println!("{TAG} {name} {state}: {}", fault.summary());
                debug_println!("{TAG} {name} fault detail: {}", fault.detail());
            }
            Some(other) => {
                debug_println!("{TAG} {name} {state} by an unexpected fault: {other:?}");
            }
            None => debug_println!(
                "{TAG} {name} {state} by an unexpected fault (label {})",
                msg_info.label()
            ),
        }
        if let Err(err) = stopped {
            debug_println!("{TAG} cannot stop {name}: {err:?}");
        }
        self.faults = self.faults.saturating_add(1);
        // Is the rest of the system still running? Ask the bystander.
        BYSTANDER.notify();
        // No reply either way: a reply would resume the child.
        Ok(None)
    }

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(BYSTANDER) {
            self.answers = self.answers.saturating_add(1);
            debug_println!("{TAG} bystander answered after fault {}", self.faults);
            let expected = self.children().len();
            if let Some(test) = self.test
                && self.answers == expected
                && self.faults == expected
            {
                debug_println!(
                    "NOWAYOS: {}: {} of {} forbidden accesses faulted and were contained; bystander still running",
                    test.system,
                    self.faults,
                    expected
                );
            }
        } else {
            debug_println!(
                "{TAG} ignoring notification on unexpected channels {}",
                channels.display()
            );
        }
        Ok(())
    }
}
