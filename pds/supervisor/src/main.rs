// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-supervisor`: the supervisor PD's program for NoWayOS's own test
//! systems. The supervisor itself is this crate's library; this program
//! names each test system's children and runs it, as a product's own
//! program does for a product system (the library's documentation).
//!
//! One program serves every test system that has a supervisor, so it picks
//! the children by its own PD name, which the system description gives it.

#![no_std]
#![no_main]

use nowayos_supervisor::{ChildPd, Class, SupervisorPd};
use sel4_microkit::{pd_name, protection_domain};

/// `systems/supervisor-test.system`: `faulter` faults when `trigger`
/// notifies it.
static FAULT_TEST: [ChildPd; 2] = [
    ChildPd::new(0, "faulter", Class::Driver),
    ChildPd::new(1, "trigger", Class::Test),
];

/// `systems/supervisor-panic.system`: `panicker` panics; `console` prints
/// the reports.
static PANIC_TEST: [ChildPd; 2] = [
    ChildPd::new(0, "console", Class::Test),
    ChildPd::new(1, "panicker", Class::Test),
];

/// `systems/supervisor-counts.system`: as `supervisor-test`, with
/// stand-in counts below their limits; `faulter` stands in for a storage
/// PD, so its fault is sensitive.
static COUNTS_TEST: [ChildPd; 2] = [
    ChildPd::new(0, "faulter", Class::Storage),
    ChildPd::new(1, "trigger", Class::Test),
];

/// The six `systems/failsafe-*.system`: stand-in counts that put
/// the device in its fail-safe state; `kept_child` stands in for the
/// watchdog PD a product keeps running - so this program classes it as the
/// watchdog, the grants file as a test instrument - and faults. Their third
/// child, `unnamed_child` (id 2), is left out on purpose: the fail-safe
/// state stops it too.
static FAIL_SAFE_TEST: [ChildPd; 2] = [
    ChildPd::new(0, "kept_child", Class::Watchdog).kept(),
    ChildPd::new(1, "stopped_child", Class::Test),
];

/// `systems/supervisor-bad-list.system`: a list `valid` refuses -
/// a child of class `app` kept, an `id` named twice (the second entry
/// marked to keep) and one out of range - which no program that asserts
/// `valid` builds with, run to show that `init` reports each and fails
/// closed: in the fail-safe state it keeps none of them.
static BAD_LIST_TEST: [ChildPd; 4] = [
    ChildPd::new(0, "kept_app", Class::App).kept(),
    ChildPd::new(1, "stopped_child", Class::Test),
    ChildPd::new(1, "named_twice", Class::Test).kept(),
    ChildPd::new(64, "out_of_range", Class::Test),
];

/// `systems/supervisor-failsafe-release.system`: the fail-safe
/// state in the debug and the release configuration, reported through
/// `console`, which is kept running: it stands in for the display PD that
/// shows a product's out-of-service image, so this program classes it as
/// the display, the grants file as a test instrument. Its other child,
/// `unnamed_faulter` (id 1), is left out on purpose, and would fault if it
/// ever ran.
static FAIL_SAFE_RELEASE_TEST: [ChildPd; 1] = [ChildPd::new(0, "console", Class::Display).kept()];

/// `systems/watchdog-test.system`: the watchdog PD and what it
/// watches - the hardware watchdog's stand-in and the timer that times it,
/// and three heartbeat probes, one of which loops. The supervisor answers
/// the watchdog's heartbeat itself.
static WATCHDOG_TEST: [ChildPd; 6] = [
    ChildPd::new(0, "watchdog", Class::Watchdog),
    ChildPd::new(1, "timer", Class::Driver),
    ChildPd::new(2, "stand_in", Class::Test),
    ChildPd::new(3, "upper", Class::Test),
    ChildPd::new(4, "looper", Class::Test),
    ChildPd::new(5, "lower", Class::Test),
];

/// `systems/entropy-stuck-test.system`: the entropy PD, fed a
/// stuck source, faults on purpose; its consumer is never seeded. The
/// entropy role joins the sensitive classes with the stored counts still to
/// come: until then this program classes it as a driver.
static ENTROPY_STUCK_TEST: [ChildPd; 2] = [
    ChildPd::new(0, "entropy", Class::Driver),
    ChildPd::new(1, "seed_consumer", Class::Test),
];

/// `systems/update-trial-r*.system`: the update PD, its storage
/// stand-in and the bundle server, which stands in for the health check
/// and, in one run, faults on purpose during a trial boot - so the board
/// resets, and the harness, playing the bootloader, falls back.
static UPDATE_TRIAL: [ChildPd; 3] = [
    ChildPd::new(0, "update_storage", Class::Test),
    ChildPd::new(1, "update", Class::Update),
    ChildPd::new(2, "update_bundles", Class::Test),
];

const _: () = assert!(nowayos_supervisor::valid(&FAULT_TEST));
const _: () = assert!(nowayos_supervisor::valid(&PANIC_TEST));
const _: () = assert!(nowayos_supervisor::valid(&COUNTS_TEST));
const _: () = assert!(nowayos_supervisor::valid(&FAIL_SAFE_TEST));
const _: () = assert!(!nowayos_supervisor::valid(&BAD_LIST_TEST));
const _: () = assert!(nowayos_supervisor::valid(&FAIL_SAFE_RELEASE_TEST));
const _: () = assert!(nowayos_supervisor::valid(&WATCHDOG_TEST));
const _: () = assert!(nowayos_supervisor::valid(&ENTROPY_STUCK_TEST));
const _: () = assert!(nowayos_supervisor::valid(&UPDATE_TRIAL));

/// The supervisors this program can be, by PD name, and their children. A
/// supervisor with another name runs with no children named, and fails
/// closed: every child is an unknown child, and the fail-safe state stops
/// them all (`systems/supervisor-empty-list.system`).
static SYSTEMS: [(&str, &[ChildPd]); 9] = [
    ("fault_supervisor", &FAULT_TEST),
    ("panic_supervisor", &PANIC_TEST),
    ("counts_supervisor", &COUNTS_TEST),
    ("failsafe_supervisor", &FAIL_SAFE_TEST),
    ("bad_list_supervisor", &BAD_LIST_TEST),
    ("failsafe_release_supervisor", &FAIL_SAFE_RELEASE_TEST),
    ("watchdog_supervisor", &WATCHDOG_TEST),
    ("entropy_supervisor", &ENTROPY_STUCK_TEST),
    ("update_supervisor", &UPDATE_TRIAL),
];

#[protection_domain]
fn init() -> SupervisorPd {
    let children = match pd_name() {
        Ok(name) => SYSTEMS
            .iter()
            .find(|(supervisor, _)| *supervisor == name)
            .map_or(&[][..], |(_, children)| *children),
        Err(_) => &[],
    };
    nowayos_supervisor::init(children)
}
