// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-supervisor`: the supervisor PD of the PD fault policy, as a library a product runs with the
//! children it names, which enters the fail-safe state at start-up
//! when the fault policy's counts say so.
//!
//! The system description makes every other PD its child, so every fault
//! reaches it rather than Microkit's monitor, which prints nothing in a
//! release build. For each fault it:
//!
//! 1. reads the fault's **label first** and decodes the rest only for a
//!    label it knows - a memory fault (`nowayos-fault`) - since rust-sel4
//!    v5.0.0's parser panics on labels it does not know, such as a debug
//!    kernel's debug exception, which is how a panic arrives. Everything else in a fault message is chosen by the PD that
//!    faulted: it is only ever reported, never acted on;
//! 2. stops the child for good (suspends its thread and never replies);
//! 3. reports the child and the decoded fault - through a console PD when
//!    the system gives it one (a test system: the release kernel prints
//!    nothing), otherwise on the debug console;
//! 4. if the system counts faults, reports the counts the next boot would
//!    hold (below);
//! 5. resets the board with PSCI `SYSTEM_RESET`, made through its SMC
//!    capability - once the console has printed the report.
//!    If the reset ever returns, it stops every child, so nothing runs. In
//!    the fail-safe state it resets nothing.
//!
//! Capabilities: its children's thread capabilities and faults, the SMC
//! capability (`smc="true"`; role `supervisor`, `tools/grant-rules.toml`),
//! and, in a system with a watchdog PD, its one channel: the heartbeat
//! (channel 1), on which it answers each of the watchdog's pings.
//! In a test system, also a ring to a console PD and one channel to it, and
//! its stand-in counts, read-only (below). It reads the counter, to know how
//! long the system has run (`nowayos-hw`). No device, interrupt, call,
//! `unsafe` or heap. Microkit 2.3.1 gives the SMC capability unbadged: it
//! could make any call the firmware offers, so no product system may
//! hold it until it can be limited to `SYSTEM_RESET` (the grants check
//! refuses it).
//!
//! Written from the PSCI specification (Arm DEN 0022, `SYSTEM_RESET`,
//! function ID `0x8400_0009`) and Microkit 2.3.1's `microkit.h`, which puts
//! a PD's SMC capability in slot 7 and its children's thread capabilities
//! in the 64 slots from 202.
//!
//! # Counting faults, and the fail-safe state
//!
//! The fault policy's counting rules: two counts kept for the device - the
//! restart count and the sensitive count - decide at each start whether it
//! restarts or enters its **fail-safe state**, by `nowayos-fault-policy`'s
//! `decide`, with the default limits ([`init`]) or a product's tighter ones
//! ([`init_with_limits`]). The supervisor reads them in its start-up, which
//! runs before any child's. There is no store for them yet, so:
//!
//! * **a system without counts counts nothing**: every fault resets the
//!   board, as `supervisor-test` shows. Every system but NoWayOS's counting tests is one;
//! * **a test system can give the supervisor stand-in counts**: read-only
//!   region [`COUNTS_REGION`], prefilled at build time from
//!   `test-counts-*.bin` (`tools/board-system`), which the grants check
//!   refuses outside a test system (`tools/check-system` rule 12), in the
//!   layout `nowayos_fault_policy::stand_in` reads. Nothing in such a system
//!   tells how a start came about, so every start's cause "was not written
//!   down", and counts. The stand-in stores nothing: at a fault the
//!   supervisor reports the counts a store would hold for the next boot.
//!
//! **The fail-safe state.** When the counts say so - a count at its limit,
//! or counts that cannot be read or checked - the supervisor stops every
//! child but those its program marks to keep ([`ChildPd::kept`]: the
//! watchdog PD and, where the board can show an out-of-service image, the
//! display PD) before any of them runs, and reports it. It
//! stops every one of the 64 slots a child can have but the kept children's,
//! so a child its program leaves out is stopped too, and every child if its
//! program names none: it fails closed. A fault in a kept child then stops
//! that child and restarts nothing: only a cold start brings the device
//! back. What a device in its fail-safe state shows is its product's
//! choice.
//!
//! A debug kernel prints an error for every call on an empty slot, so in a
//! debug build the supervisor first asks the kernel which slots hold a
//! child, with its debug call `seL4_DebugCapIdentify`, which prints nothing.
//! A release kernel prints nothing: there it asks to stop every slot, and
//! takes `InvalidCapability`, seL4's answer for an empty slot, as no child.
//!
//! Each child's [`Class`] - its `role` in the system's grants file, which
//! nothing checks: it is the program's own declaration, as is its `id` -
//! says whether its faults are sensitive and whether it may be kept running
//! in the fail-safe state; a fault in a child the program does not name
//! counts as a `security` PD's, sensitive under any limits.
//!
//! # Running it in a system of your own
//!
//! The supervisor does not know its children: Microkit tells a PD only its
//! own name. So every system runs it from a program of its own that names
//! the system's children - each one's `id` in the system description and
//! its name, used in reports - and returns [`init`]. NoWayOS's own test
//! systems run `src/main.rs`; a product repo writes its own, in its own
//! `pds/` workspace, and builds it with NoWayOS's make fragments
//! (`tools/make/system.mk`, "Product repos"), as a product repo runs
//! NoWayOS's timer PD (`pds/README.md`, "Adding a PD").
//!
//! The program, `pds/supervisor/src/main.rs` in the product repo:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! use nowayos_supervisor::{ChildPd, Class, SupervisorPd};
//! use sel4_microkit::protection_domain;
//!
//! /// Every child of `supervisor` in systems/kiosk.system: its `id` there,
//! /// its name, its role in systems/kiosk.grants, and whether it keeps
//! /// running in the fail-safe state.
//! static CHILDREN: [ChildPd; 4] = [
//!     ChildPd::new(0, "watchdog", Class::Watchdog).kept(),
//!     ChildPd::new(1, "display", Class::Display).kept(),
//!     ChildPd::new(2, "input", Class::Input),
//!     ChildPd::new(3, "app", Class::App),
//! ];
//!
//! // Refuses to build if the list is empty, an id is out of range or named
//! // twice, or a child of a class the fail-safe state stops is kept.
//! const _: () = assert!(nowayos_supervisor::valid(&CHILDREN));
//!
//! #[protection_domain]
//! fn init() -> SupervisorPd {
//!     nowayos_supervisor::init(&CHILDREN)
//! }
//! ```
//!
//! A product whose own ADR tightens the limits returns
//! `nowayos_supervisor::init_with_limits(&CHILDREN, LIMITS)` instead, with
//! `LIMITS` built from [`Limits::DEFAULT`] - for example
//! `Limits::DEFAULT.with_restarts_at_most(3)`; [`Limits`] cannot loosen
//! them.
//!
//! Its manifest, `pds/supervisor/Cargo.toml`, names the program image and
//! takes NoWayOS's library by path from the product's `pds/Cargo.toml`
//! (`[workspace.dependencies]`):
//!
//! ```toml
//! [package]
//! name = "kiosk-supervisor"
//! # version, edition and publish from the workspace, as every PD crate
//!
//! [[bin]]
//! name = "kiosk-supervisor"
//! path = "src/main.rs"
//! test = false
//! bench = false
//!
//! [dependencies]
//! nowayos-supervisor.workspace = true   # { path = "../../nowayos/pds/supervisor" }
//! sel4-microkit.workspace = true        # the rust-sel4 commit NoWayOS pins
//!
//! [lints]
//! workspace = true
//! ```
//!
//! The system description puts the supervisor at the top, with the highest
//! priority and a full budget (budget equal to period: Microkit's default,
//! so leave both out), so it acts on a fault at once and its start-up runs
//! before any child's - under seL4's scheduler a budget below its period
//! could run out partway through, letting lower-priority children run
//! first - and every other PD inside it with the `id` the program gives:
//!
//! ```xml
//! <protection_domain name="supervisor" priority="254" stack_size="0x10000" smc="true">
//!     <program_image path="kiosk-supervisor.elf" />
//!     <protection_domain name="watchdog" id="0" priority="250" ...> ... </protection_domain>
//!     <protection_domain name="display" id="1" priority="200" ...> ... </protection_domain>
//!     <protection_domain name="input" id="2" ...> ... </protection_domain>
//!     <protection_domain name="app" id="3" ...> ... </protection_domain>
//! </protection_domain>
//! ```
//!
//! and its `.grants` file lists `role supervisor supervisor`, `child
//! supervisor PD` for each child and `smc supervisor` - which the grants
//! check refuses in a `product` system until the SMC capability can be
//! limited to `SYSTEM_RESET`, so for now in a `test` or `demo`
//! system. The product's Makefile sets `SYSTEM_REPO := $(CURDIR)` and
//! includes `../nowayos/tools/make/container.mk` and
//! `../nowayos/tools/make/system.mk`; `make qemu-test SYSTEM=kiosk` then
//! builds the program, checks the grants and boots the system, and the
//! system's expect file should check the supervisor's start-up line, which
//! names every child with its id: `NOWAYOS: supervisor: starting; children
//! watchdog (0), display (1), input (2) and app (3)`. A child the program
//! leaves out is reported as "an unknown child" when it faults, and its
//! faults count as sensitive; in the fail-safe state, and if the reset ever
//! returns, it is stopped like every child that is not kept.

#![no_std]

use core::fmt::{self, Write};
use core::time::Duration;

use nowayos_fault::VmFault;
use nowayos_fault_policy::stand_in::{self, PROVEN_LEN, STAND_IN_LEN};
use nowayos_fault_policy::{Action, Counts, Start, decide};
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_ipc::Producer;
use nowayos_region::{
    PAGE_WORDS, ReadOnly, RegionError, RingTx, Words, read_only, read_write, ring_producer,
};
use sel4::sys::seL4_ARM_SMCContext;
use sel4::sys::seL4_Fault_tag::seL4_Fault_VMFault;
use sel4_microkit::{Channel, ChannelSet, Child, Handler, Infallible, MessageInfo, debug_println};

pub use nowayos_fault_policy::{Class, Limits};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: supervisor:";

/// The channel to the console PD, when the system gives it one.
const CONSOLE: Channel = Channel::new(0);

/// The heartbeat channel to the watchdog PD, when the system has one: the
/// supervisor answers each ping by notifying back.
const HEARTBEAT: Channel = Channel::new(1);

/// The ring to the console: its slots, each one line of text.
const CONSOLE_SLOTS: usize = 8;
const LINE: usize = 128;

/// PSCI `SYSTEM_RESET` (SMC32 calling convention).
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

/// The slot of a PD's SMC capability in its CSpace (Microkit's
/// `ARM_SMC_CAP`).
const ARM_SMC_CAP: u64 = 7;

/// How many children a Microkit PD can have: its children's thread
/// capabilities fill the 64 slots from `BASE_TCB_CAP` (202) to
/// `BASE_VM_TCB_CAP` (266), so a child's `id` is below this.
pub const MAX_CHILDREN: usize = 64;

/// The read-only region (`nowayos_ro_region_1`) a test system prefills with
/// the supervisor's stand-in counts (see the crate documentation).
pub const COUNTS_REGION: usize = 1;

/// The words of [`COUNTS_REGION`] the supervisor copies out before it reads
/// them, and their bytes: enough for a stand-in's length word and one
/// record, and no more than Kani's proof of the reader covers
/// (`nowayos_fault_policy::stand_in::PROVEN_LEN`), since the supervisor
/// passes it all of them.
const COUNTS_WORDS: usize = 3;
const COUNTS_BYTES: usize = 12;
const _: () = assert!(
    COUNTS_WORDS.saturating_mul(4) == COUNTS_BYTES
        && COUNTS_BYTES >= STAND_IN_LEN
        && COUNTS_BYTES <= PROVEN_LEN
        && COUNTS_WORDS <= PAGE_WORDS
);

/// One child of the supervisor, as its system description names it: its
/// `id` (the `id` attribute of its `<protection_domain>`, inside the
/// supervisor's), its name, which the supervisor's reports use, its class -
/// its `role` in the system's grants file - and whether it keeps running in
/// the fail-safe state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildPd {
    id: usize,
    name: &'static str,
    class: Class,
    keep: bool,
}

impl ChildPd {
    /// The child with `id` in the system description, called `name`, of
    /// `class`; stopped in the fail-safe state, unless marked [`kept`](Self::kept).
    #[must_use]
    pub const fn new(id: usize, name: &'static str, class: Class) -> Self {
        Self {
            id,
            name,
            class,
            keep: false,
        }
    }

    /// The same child, kept running in the fail-safe state: only the watchdog PD and, where the board can show an
    /// out-of-service image without the other PDs, the display PD
    /// ([`Class::may_keep_running`]). [`valid`] refuses a list that keeps a
    /// child of any other class, and [`init`] reports such a child and stops
    /// it in the fail-safe state with the rest. The class is the program's
    /// own declaration: nothing checks it against the child's role in the
    /// grants file.
    #[must_use]
    pub const fn kept(self) -> Self {
        Self { keep: true, ..self }
    }

    /// Its `id` in the system description.
    #[must_use]
    pub const fn id(&self) -> usize {
        self.id
    }

    /// Its name, as the supervisor reports it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Its class: its `role` in the system's grants file.
    #[must_use]
    pub const fn class(&self) -> Class {
        self.class
    }

    /// Whether it keeps running in the fail-safe state.
    #[must_use]
    pub const fn is_kept(&self) -> bool {
        self.keep
    }

    /// Whether its `id` can be a Microkit child's.
    const fn in_range(&self) -> bool {
        self.id < MAX_CHILDREN
    }

    /// Whether it is marked to keep running in the fail-safe state, but its
    /// class is stopped there.
    const fn kept_against_its_class(&self) -> bool {
        self.keep && !self.class.may_keep_running()
    }
}

/// Whether `children` can be a supervisor's: at least one child, every `id`
/// below [`MAX_CHILDREN`] and none twice, and no child kept running in the
/// fail-safe state whose class is stopped there ([`ChildPd::kept`]). A
/// `const fn`, so a program can refuse to build with a list that is not:
/// `const _: () = assert!(nowayos_supervisor::valid(&CHILDREN));`. [`init`]
/// checks it again, reports what is wrong, and fails closed: a supervisor
/// whose program names no children, or leaves one out, still stops every
/// child but the kept ones in the fail-safe state.
#[must_use]
pub const fn valid(children: &[ChildPd]) -> bool {
    if children.is_empty() {
        return false;
    }
    let mut rest = children;
    while let [child, others @ ..] = rest {
        if !child.in_range() || has_id(others, child.id) || child.kept_against_its_class() {
            return false;
        }
        rest = others;
    }
    true
}

// `valid`'s rules, checked whenever this crate builds: it is built only for
// seL4, so it has no host tests.
const _: () = {
    let watchdog = ChildPd::new(0, "watchdog", Class::Watchdog);
    let display = ChildPd::new(1, "display", Class::Display);
    let app = ChildPd::new(2, "app", Class::App);
    let past = ChildPd::new(MAX_CHILDREN, "past", Class::App);
    let security = ChildPd::new(3, "security", Class::Security);
    assert!(valid(&[watchdog.kept(), display.kept(), app]));
    assert!(valid(&[ChildPd::new(63, "last", Class::Test)]));
    assert!(!valid(&[ChildPd::new(63, "test", Class::Test).kept()]));
    assert!(!valid(&[]));
    assert!(!valid(&[watchdog, past]));
    assert!(!valid(&[watchdog, ChildPd::new(0, "again", Class::App)]));
    assert!(!valid(&[watchdog.kept(), app.kept()]));
    assert!(!valid(&[security.kept()]));
};

/// Whether the child at `index` in `children` has an `id` a child before it
/// has.
fn named_before(children: &[ChildPd], index: usize) -> bool {
    match (children.get(..index), children.get(index)) {
        (Some(before), Some(child)) => has_id(before, child.id),
        _ => false,
    }
}

/// The children the supervisor acts on: each one whose `id` can be a
/// Microkit child's, under its first entry.
fn supervised(children: &[ChildPd]) -> impl Iterator<Item = &ChildPd> + Clone {
    children
        .iter()
        .enumerate()
        .filter(|(index, child)| child.in_range() && !named_before(children, *index))
        .map(|(_, child)| child)
}

/// The children kept running in the fail-safe state: each one the
/// supervisor acts on that is marked to keep and whose class may keep
/// running there.
fn kept(children: &[ChildPd]) -> impl Iterator<Item = &ChildPd> + Clone {
    supervised(children).filter(|child| child.keep && child.class.may_keep_running())
}

/// Whether a child in `children` has `id`.
const fn has_id(children: &[ChildPd], id: usize) -> bool {
    let mut rest = children;
    while let [child, others @ ..] = rest {
        if child.id == id {
            return true;
        }
        rest = others;
    }
    false
}

/// Starts the supervisor PD with `children`, the children its system
/// description gives it, and the default limits, and reports them; a child
/// whose `id` cannot be a Microkit child's, or is named twice, is reported,
/// and never acted on under that entry, and a child kept against its class
/// is reported and not kept. If the system gives it counts, it reads them
/// and, if they say so, enters the fail-safe state, all before any child
/// runs - even with no children named, when every child is stopped. Return
/// it from the system's own program (see the crate documentation).
#[must_use]
pub fn init(children: &'static [ChildPd]) -> SupervisorPd {
    init_with_limits(children, Limits::DEFAULT)
}

/// [`init`], with `limits` in place of the default ones: a product may
/// tighten them, and [`Limits`] can only be tightened.
#[must_use]
pub fn init_with_limits(children: &'static [ChildPd], limits: Limits) -> SupervisorPd {
    let started = counter();
    // The ring to the console: its messages read-write
    // (`nowayos_rw_region_0`), its control read-only (`nowayos_ro_region_0`).
    let console = match read_write(0).and_then(|messages| ring_producer(messages, read_only(0)?)) {
        Ok(ring) => Some(ring),
        Err(RegionError::NotMapped { .. }) => None,
        Err(error) => {
            debug_println!("{TAG} no console: {error}");
            None
        }
    };
    let mut pd = SupervisorPd {
        children,
        console,
        reset_pending: false,
        limits,
        counts: None,
        fail_safe: false,
        started,
        frequency: counter_frequency(),
    };
    // With no children named, every child is an unknown child: reported as
    // such, and stopped in the fail-safe state like any child not kept.
    if children.is_empty() {
        pd.report(format_args!(
            "{TAG} no children are defined for this supervisor"
        ));
    } else {
        pd.report(format_args!("{TAG} starting; children {}", Names(children)));
    }
    for (index, child) in children.iter().enumerate() {
        if !child.in_range() {
            pd.report(format_args!(
                "{TAG} cannot supervise {}: id {} is not a Microkit child's (0 to {})",
                child.name,
                child.id,
                MAX_CHILDREN.saturating_sub(1)
            ));
        } else if named_before(children, index) {
            pd.report(format_args!(
                "{TAG} cannot supervise {}: id {} is already {}'s",
                child.name,
                child.id,
                pd.name_of(child.id)
            ));
        } else if child.kept_against_its_class() {
            pd.report(format_args!(
                "{TAG} cannot keep {} ({}) running in the fail-safe state: class {} is stopped there",
                child.name, child.id, child.class
            ));
        }
    }
    pd.start_counting();
    pd
}

/// A list of children as the start-up line shows it: `a (0) and b (1)`;
/// `none` if there are none.
fn write_names<'a>(
    f: &mut fmt::Formatter<'_>,
    children: impl Iterator<Item = &'a ChildPd> + Clone,
) -> fmt::Result {
    let count = children.clone().count();
    if count == 0 {
        return f.write_str("none");
    }
    for (index, child) in children.enumerate() {
        if index > 0 {
            let last = index.saturating_add(1) == count;
            f.write_str(if last { " and " } else { ", " })?;
        }
        write!(f, "{} ({})", child.name, child.id)?;
    }
    Ok(())
}

/// Every child, as the start-up line names them.
struct Names(&'static [ChildPd]);

impl fmt::Display for Names {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_names(f, self.0.iter())
    }
}

/// The children kept running in the fail-safe state.
struct Kept(&'static [ChildPd]);

impl fmt::Display for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_names(f, kept(self.0))
    }
}

/// Counts against their limits: `restart count 2 of 4, sensitive count 0 of
/// 2`.
struct Held(Counts, Limits);

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(counts, limits) = self;
        write!(
            f,
            "restart count {} of {}, sensitive count {} of {}",
            counts.restarts,
            limits.restart_limit(),
            counts.sensitive,
            limits.sensitive_limit()
        )
    }
}

/// One line of text in a fixed buffer; what does not fit is cut off.
struct Line {
    bytes: [u8; LINE],
    len: usize,
}

impl Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if let Some(slot) = self.bytes.get_mut(self.len) {
                *slot = byte;
                self.len = self.len.saturating_add(1);
            }
        }
        Ok(())
    }
}

/// The supervisor PD's state. Make it with [`init`].
pub struct SupervisorPd {
    /// Its children, as its program names them.
    children: &'static [ChildPd],
    /// The ring to its console PD, if the system gives it one.
    console: Option<RingTx<CONSOLE_SLOTS, LINE>>,
    /// Whether a reset waits for the console to print.
    reset_pending: bool,
    /// The limits the counts are held to.
    limits: Limits,
    /// The counts this boot holds, if the system gives it counts.
    counts: Option<Counts>,
    /// Whether it is in the fail-safe state.
    fail_safe: bool,
    /// The counter when it started, and the counter's frequency.
    started: u64,
    frequency: u64,
}

impl SupervisorPd {
    /// The child with `id`, as the program names it.
    fn child(&self, id: usize) -> Option<&'static ChildPd> {
        let children = self.children;
        children.iter().find(|child| child.id == id)
    }

    /// The name of the child with `id`, as the program names it.
    fn name_of(&self, id: usize) -> &'static str {
        self.child(id)
            .map_or("an unknown child", |child| child.name)
    }

    /// How long the system has run since this start, in whole seconds, if
    /// the counter says.
    fn ran_for(&self) -> Option<Duration> {
        let ticks = counter().checked_sub(self.started)?;
        Some(Duration::from_secs(ticks.checked_div(self.frequency)?))
    }

    /// Reads the counts, if the system gives it any, and decides this
    /// start: restart, or enter the fail-safe state. Runs in its start-up, before any child runs.
    fn start_counting(&mut self) {
        let words = match read_only(COUNTS_REGION) {
            // No counts: nothing is counted (no store is built yet).
            Err(RegionError::NotMapped { .. }) => return,
            region => region.and_then(|region| region.into_words()),
        };
        let stored = match words.map(|words| stand_in::read(&copy_out(&words))) {
            Ok(Ok(counts)) => {
                self.report(format_args!(
                    "{TAG} counts read from the test stand-in, which stores nothing: {counts}"
                ));
                Some(counts)
            }
            Ok(Err(why)) => {
                self.report(format_args!("{TAG} the counts cannot be read: {why}"));
                None
            }
            Err(error) => {
                self.report(format_args!("{TAG} the counts cannot be read: {error}"));
                None
            }
        };
        // Nothing in a system without a store tells how this start came
        // about, so it counts as a reset whose cause was never written down.
        let start = Start::Unknown;
        self.report(format_args!("{TAG} this start: {start}"));
        let decision = decide(stored, start, None, &self.limits);
        self.report(format_args!(
            "{TAG} {}: {}",
            Held(decision.counts, self.limits),
            decision.action
        ));
        self.counts = Some(decision.counts);
        if decision.action == Action::FailSafe {
            self.enter_fail_safe();
        }
    }

    /// Enters the fail-safe state: stops every child but
    /// those its program keeps - every one of the 64 slots a child can have,
    /// so a child the program does not name is stopped too. Runs before any
    /// child runs.
    fn enter_fail_safe(&mut self) {
        self.fail_safe = true;
        let children = self.children;
        for id in 0..MAX_CHILDREN {
            if kept(children).any(|child| child.id == id) {
                continue;
            }
            let name = self.name_of(id);
            match stop_slot(id) {
                Stopped::Yes => self.report(format_args!("{TAG} {name} ({id}) stopped")),
                Stopped::NoChild => {}
                Stopped::Failed => self.report(format_args!("{TAG} cannot stop {name} ({id})")),
            }
        }
        self.report(format_args!(
            "{TAG} in the fail-safe state; kept running: {}",
            Kept(children)
        ));
    }

    /// Prints one line: to the console PD if the system gives it one,
    /// otherwise on the debug console.
    fn report(&mut self, text: fmt::Arguments<'_>) {
        let Some(memory) = self.console.as_ref() else {
            debug_println!("{text}");
            return;
        };
        let mut line = Line {
            bytes: [0; LINE],
            len: 0,
        };
        // Formatting into a Line never fails; the length is bounded.
        let _ = line.write_fmt(text);
        let sent = Producer::new(memory)
            .ok()
            .and_then(|mut tx| tx.try_send_bytes(line.bytes.get(..line.len)?).ok());
        if sent.is_none() {
            debug_println!("{TAG} the console's ring is full or broken");
        }
        CONSOLE.notify();
    }

    /// Resets the board through PSCI. If that ever returns, stops every
    /// child - every one of the 64 slots a child can have, kept or not,
    /// named or not - so nothing runs.
    fn reset(&mut self) {
        let arguments = seL4_ARM_SMCContext {
            x0: PSCI_SYSTEM_RESET,
            x1: 0,
            x2: 0,
            x3: 0,
            x4: 0,
            x5: 0,
            x6: 0,
            x7: 0,
        };
        let mut response = arguments;
        let error = sel4::with_ipc_buffer_mut(|ipc_buffer| {
            ipc_buffer
                .inner_mut()
                .seL4_ARM_SMC_Call(ARM_SMC_CAP, &arguments, &mut response)
        });
        self.report(format_args!(
            "{TAG} the reset returned (error {error}, x0 {:#x}): stopping every child",
            response.x0
        ));
        for id in 0..MAX_CHILDREN {
            if stop_slot(id) == Stopped::Failed {
                let name = self.name_of(id);
                self.report(format_args!("{TAG} cannot stop {name} ({id})"));
            }
        }
    }
}

/// What asking to stop the child in one of the 64 slots came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stopped {
    /// The child in the slot is stopped.
    Yes,
    /// The slot holds no child: no PD in the system has that `id`.
    NoChild,
    /// The slot holds a child, and it could not be stopped.
    Failed,
}

/// Stops the child with `id` - suspends its thread for good - if its slot
/// holds one.
fn stop_slot(id: usize) -> Stopped {
    let tcb = Child::new(id).tcb();
    if !holds_a_capability(tcb) {
        return Stopped::NoChild;
    }
    match tcb.tcb_suspend() {
        Ok(()) => Stopped::Yes,
        // seL4's answer for an empty slot: in Microkit these 64 slots hold
        // a child's thread capability or nothing.
        Err(sel4::Error::InvalidCapability) => Stopped::NoChild,
        Err(_) => Stopped::Failed,
    }
}

/// Whether the slot of `tcb` holds a capability. A debug kernel prints an
/// error for every call on an empty slot, so the supervisor asks with the
/// debug call `seL4_DebugCapIdentify`, which prints nothing; seL4's null
/// capability, an empty slot's, is type 0.
#[sel4::sel4_cfg(DEBUG_BUILD)]
fn holds_a_capability(tcb: sel4::cap::Tcb) -> bool {
    tcb.debug_identify() != 0
}

/// Whether the slot of `tcb` may hold a capability: a release kernel has no
/// debug calls and prints nothing, so the supervisor asks to stop every
/// slot, and an empty one answers `InvalidCapability`.
#[sel4::sel4_cfg(not(DEBUG_BUILD))]
fn holds_a_capability(_tcb: sel4::cap::Tcb) -> bool {
    true
}

impl Handler for SupervisorPd {
    type Error = Infallible;

    fn fault(
        &mut self,
        child: Child,
        msg_info: MessageInfo,
    ) -> Result<Option<MessageInfo>, Self::Error> {
        let named = self.child(child.index());
        let name = named.map_or("an unknown child", |named| named.name);
        // The label first, and the fault decoded only for a label known to
        // parse; both copied out before the suspend below, a kernel call
        // that reuses the message registers.
        let label = msg_info.label();
        let decoded = if label == seL4_Fault_VMFault {
            match msg_info.fault() {
                sel4::Fault::VmFault(fault) => Some(VmFault::new(
                    fault.ip(),
                    fault.addr(),
                    fault.fsr(),
                    fault.is_prefetch(),
                )),
                _ => None,
            }
        } else {
            None
        };
        // Stop the child for good: it never runs again in this boot, and no
        // reply is sent.
        let state = if child.tcb().tcb_suspend().is_ok() {
            "stopped"
        } else {
            "faulted (NOT stopped)"
        };
        match decoded {
            Some(fault) => {
                self.report(format_args!("{TAG} {name} {state}: {}", fault.summary()));
                self.report(format_args!(
                    "{TAG} {name} fault detail: {}",
                    fault.detail()
                ));
            }
            None => {
                self.report(format_args!(
                    "{TAG} {name} {state}: a fault the supervisor does not decode"
                ));
                self.report(format_args!("{TAG} {name} fault detail: label {label}"));
            }
        }
        if self.fail_safe {
            // Only a cold start brings the device back.
            self.report(format_args!(
                "{TAG} in the fail-safe state: nothing is restarted"
            ));
            return Ok(None);
        }
        if let Some(counts) = self.counts {
            // A child the program does not name: its class is not known, so
            // its fault counts as a security PD's, sensitive under any limits.
            let class = named.map_or(Class::Security, |named| named.class);
            let ran_for = self.ran_for();
            match ran_for {
                Some(run) => self.report(format_args!(
                    "{TAG} the system had run {} s since this start",
                    run.as_secs()
                )),
                None => self.report(format_args!(
                    "{TAG} how long the system had run is not known"
                )),
            }
            let next = decide(Some(counts), Start::Fault(class), ran_for, &self.limits);
            self.report(format_args!(
                "{TAG} the next boot would hold {}: {}",
                Held(next.counts, self.limits),
                next.action
            ));
        }
        self.report(format_args!("{TAG} resetting the board"));
        // With a console, reset once it has printed the report (it
        // notifies back); without one, the report is already printed.
        if self.console.is_some() {
            self.reset_pending = true;
        } else {
            self.reset();
        }
        Ok(None)
    }

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        // The watchdog's ping: answered at once - the supervisor runs above
        // every other PD, the watchdog included.
        if channels.contains(HEARTBEAT) {
            HEARTBEAT.notify();
        }
        if channels.contains(CONSOLE) && self.reset_pending {
            self.reset_pending = false;
            self.reset();
        }
        Ok(())
    }
}

/// The first words of the stand-in counts, copied out of the region before
/// they are read, as bytes in memory order.
fn copy_out(words: &Words<ReadOnly>) -> [u8; COUNTS_BYTES] {
    let mut bytes = [0; COUNTS_BYTES];
    for (index, chunk) in bytes.chunks_exact_mut(4).enumerate() {
        let word = words.load(index).unwrap_or(0).to_le_bytes();
        for (slot, byte) in chunk.iter_mut().zip(word) {
            *slot = byte;
        }
    }
    bytes
}
