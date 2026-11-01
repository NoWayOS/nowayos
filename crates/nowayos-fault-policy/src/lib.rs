// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-fault-policy`: what a device does at each start - restart,
//! restart uncounted, or enter its fail-safe state - from how the start came
//! about and the counts it keeps for the next boot. NoWayOS's fault policy's
//! counting rules, written once as one function,
//! [`decide`], and proven with Kani.
//!
//! # The rule
//!
//! Two counts are kept for the device, where the next boot can read them
//! ([`Counts`]):
//!
//! * the **restart count**: every start that is not a cold start adds 1 - a
//!   fault, a watchdog reset, or a reset whose cause was never written
//!   down. A fault after the system has run 10 minutes since its last start
//!   ([`Limits::clearing_run`]) clears it instead, and that start is not
//!   counted;
//! * the **sensitive count**: faults in a PD of a sensitive class -
//!   `security`, `storage`, `update`, `supervisor` and `entropy`
//!   ([`Limits::sensitive`]) - which only a cold start clears.
//!
//! The device enters its **fail-safe state** instead of restarting when the
//! restart count reaches 4 ([`Limits::restart_limit`]) or the sensitive count
//! reaches 2 ([`Limits::sensitive_limit`]), and only a **cold start** - power
//! off and on - brings it back, clearing both counts. Counts that cannot be
//! read or checked at boot count as the limit reached.
//!
//! [`decide`] takes the counts read at boot (or `None`: they could not be
//! read or checked), how this start came about ([`Start`]) and how long the
//! system had run before it (or `None`: not known), and returns the counts
//! to store and one [`Action`]. The limits are parameters ([`Limits`]),
//! the default ones unless a product tightens them - fewer
//! restarts, fewer sensitive faults, a longer run before a fault clears the
//! restart count, more sensitive classes - without changing this logic, and
//! [`Limits`] offers no way to loosen them.
//!
//! The `entropy` class is sensitive too: a PD that seeds every generator
//! is one whose fault matters.
//!
//! # Readings
//!
//! Where the fault policy leaves a point open, [`decide`] takes the simplest reading
//! that keeps every rule:
//!
//! 1. **A watchdog reset always counts**, however long the system had run.
//!    Rule 5 clears the restart count after a *fault*, and a hang is not a
//!    fault (a hung PD never faults); rule 5 names a
//!    watchdog reset among the starts that add 1, and rule 7 says a starved
//!    watchdog's reset "counts". A reset whose cause was never written down
//!    counts the same way. Only a fault, with the time the system had run
//!    known and at least [`Limits::clearing_run`], clears the count.
//! 2. **A cold start clears both counts, even counts that cannot be read.**
//!    Rule 6: only a cold start brings the device back, and it clears both
//!    counts - and a new device's first start finds no counts at all. At
//!    every other start, counts that cannot be read or checked give the
//!    fail-safe state, and the counts to store are at both limits (rule 5).
//! 3. **Once a count has reached its limit, only a cold start clears it.** A
//!    fault after the clearing run clears the restart count only while both
//!    counts are below their limits, so no start answers restart once either
//!    limit is reached (rule 6).
//!
//! # What the caller decides
//!
//! Where the counts live and how they are checked, how a cold start is told
//! apart from a reset, and how a planned restart (an update's) is told apart
//! so that it is not counted, are the storage, boot and update designs still to come:
//! a planned restart is not a [`Start`] here. So is
//! telling the supervisor's own fault apart: it reaches the next boot only
//! through the watchdog PD's record, and a caller that
//! finds the supervisor among the PDs that missed their heartbeat passes
//! [`Start::Fault`] with [`Class::Supervisor`], so that it counts as
//! sensitive.
//!
//! [`stand_in`] reads the counts a test system gives its supervisor until
//! A store still to come keeps them.
//!
//! `no_std`, no allocation, no `unsafe`, and no panic or overflow for any
//! input (Kani's proofs, `src/proofs.rs`, and `tests/`).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use core::fmt;
use core::time::Duration;

#[cfg(kani)]
mod proofs;
pub mod stand_in;

/// A PD's class for the fault policy: its role in the system's grants file
/// (`tools/grant-rules.toml`), and `entropy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Draws the screen and turns canonical input into actions.
    Ui,
    /// Turns device input into canonical events.
    Input,
    /// Merges, checks and rate-limits the input PDs' events for the UI.
    Broker,
    /// Owns a display controller and scans out frames.
    Display,
    /// Owns another device for other PDs.
    Driver,
    /// A network stack or protocol PD.
    Network,
    /// Holds keys and credentials. Sensitive.
    Security,
    /// Persistent state. Sensitive.
    Storage,
    /// Verifies and applies updates. Sensitive.
    Update,
    /// Application logic, holding no device.
    App,
    /// The parent of every other PD. Sensitive: its input is
    /// chosen by the PD that faulted.
    Supervisor,
    /// Keeps the hardware watchdog serviced while every PD answers its
    /// heartbeat.
    Watchdog,
    /// Owns the hardware random source. Sensitive.
    Entropy,
    /// A test instrument, in a test system only.
    Test,
}

impl Class {
    /// Every class, in a fixed order.
    pub const ALL: [Self; 14] = [
        Self::Ui,
        Self::Input,
        Self::Broker,
        Self::Display,
        Self::Driver,
        Self::Network,
        Self::Security,
        Self::Storage,
        Self::Update,
        Self::App,
        Self::Supervisor,
        Self::Watchdog,
        Self::Entropy,
        Self::Test,
    ];

    /// Its name, as a grants file's `role` line spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Input => "input",
            Self::Broker => "broker",
            Self::Display => "display",
            Self::Driver => "driver",
            Self::Network => "network",
            Self::Security => "security",
            Self::Storage => "storage",
            Self::Update => "update",
            Self::App => "app",
            Self::Supervisor => "supervisor",
            Self::Watchdog => "watchdog",
            Self::Entropy => "entropy",
            Self::Test => "test",
        }
    }

    /// The class a grants file's `role` line names, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.name() == name)
    }

    /// Whether a PD of this class may keep running in the fail-safe state
    /// beside the supervisor: the watchdog PD and, where
    /// the board can show an out-of-service image without the others, the
    /// display PD. The fail-safe state stops a PD of every other class, a
    /// test instrument's included.
    #[must_use]
    pub const fn may_keep_running(self) -> bool {
        matches!(self, Self::Watchdog | Self::Display)
    }

    /// Its bit in a [`ClassSet`].
    const fn bit(self) -> u16 {
        // Every class's number is below 16 (`ALL` has 14), so the shift
        // never fails.
        match 1u16.checked_shl(self as u32) {
            Some(bit) => bit,
            None => 0,
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of [`Class`]es.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassSet(u16);

impl ClassSet {
    /// No class.
    pub const EMPTY: Self = Self(0);

    /// This set and `class`.
    #[must_use]
    pub const fn with(self, class: Class) -> Self {
        Self(self.0 | class.bit())
    }

    /// Whether `class` is in the set.
    #[must_use]
    pub const fn contains(self, class: Class) -> bool {
        self.0 & class.bit() != 0
    }

    /// Whether every class in `other` is in this set too.
    #[must_use]
    pub const fn contains_all(self, other: Self) -> bool {
        other.0 & !self.0 == 0
    }

    /// The classes in the set, in [`Class::ALL`]'s order.
    pub fn iter(self) -> impl Iterator<Item = Class> {
        Class::ALL
            .into_iter()
            .filter(move |class| self.contains(*class))
    }
}

impl fmt::Display for ClassSet {
    /// `security, storage and update`; `none` for the empty set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.iter().count();
        if count == 0 {
            return f.write_str("none");
        }
        for (index, class) in self.iter().enumerate() {
            if index > 0 {
                let last = index.saturating_add(1) == count;
                f.write_str(if last { " and " } else { ", " })?;
            }
            f.write_str(class.name())?;
        }
        Ok(())
    }
}

/// The limits [`decide`] applies: the default ones ([`Limits::DEFAULT`]),
/// or tighter. A product may tighten them, never loosen
/// them without a change to the fault policy itself - so there is
/// no way to make limits looser than the ones they start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    restarts: u8,
    sensitive_faults: u8,
    clearing_run: Duration,
    sensitive: ClassSet,
}

impl Limits {
    /// The default limits: fail safe when the restart count reaches
    /// 4 or the sensitive count reaches 2; a fault after the system has run
    /// 10 minutes since its last start clears the restart count; faults in
    /// `security`, `storage`, `update`, `supervisor` and `entropy`
    /// PDs are sensitive.
    pub const DEFAULT: Self = Self {
        restarts: 4,
        sensitive_faults: 2,
        clearing_run: Duration::from_secs(600),
        sensitive: ClassSet::EMPTY
            .with(Class::Security)
            .with(Class::Storage)
            .with(Class::Update)
            .with(Class::Supervisor)
            .with(Class::Entropy),
    };

    /// The restart count at which the device fails safe instead of
    /// restarting.
    #[must_use]
    pub const fn restart_limit(&self) -> u8 {
        self.restarts
    }

    /// The sensitive count at which the device fails safe instead of
    /// restarting.
    #[must_use]
    pub const fn sensitive_limit(&self) -> u8 {
        self.sensitive_faults
    }

    /// How long the system must have run since its last start for a fault
    /// to clear the restart count, and restart the device uncounted.
    #[must_use]
    pub const fn clearing_run(&self) -> Duration {
        self.clearing_run
    }

    /// The classes whose faults add to the sensitive count.
    #[must_use]
    pub const fn sensitive(&self) -> ClassSet {
        self.sensitive
    }

    /// Whether a fault in a PD of `class` adds to the sensitive count.
    #[must_use]
    pub const fn is_sensitive(&self, class: Class) -> bool {
        self.sensitive.contains(class)
    }

    /// These limits, failing safe at `limit` restarts if that is lower; a
    /// higher `limit` changes nothing.
    #[must_use]
    pub const fn with_restarts_at_most(self, limit: u8) -> Self {
        Self {
            restarts: if limit < self.restarts {
                limit
            } else {
                self.restarts
            },
            ..self
        }
    }

    /// These limits, failing safe at `limit` sensitive faults if that is
    /// lower; a higher `limit` changes nothing.
    #[must_use]
    pub const fn with_sensitive_faults_at_most(self, limit: u8) -> Self {
        Self {
            sensitive_faults: if limit < self.sensitive_faults {
                limit
            } else {
                self.sensitive_faults
            },
            ..self
        }
    }

    /// These limits, with a fault clearing the restart count only after
    /// `run` if that is longer; a shorter `run` changes nothing.
    #[must_use]
    pub const fn with_clearing_run_at_least(self, run: Duration) -> Self {
        Self {
            clearing_run: if run.as_nanos() > self.clearing_run.as_nanos() {
                run
            } else {
                self.clearing_run
            },
            ..self
        }
    }

    /// These limits, with faults in `class` sensitive too.
    #[must_use]
    pub const fn with_sensitive(self, class: Class) -> Self {
        Self {
            sensitive: self.sensitive.with(class),
            ..self
        }
    }

    /// Whether these limits are at least as strict as `other` in every
    /// respect: no higher limit, no shorter clearing run, no class that
    /// `other` counts as sensitive left out.
    #[must_use]
    pub const fn at_least_as_strict_as(&self, other: &Self) -> bool {
        self.restarts <= other.restarts
            && self.sensitive_faults <= other.sensitive_faults
            && self.clearing_run.as_nanos() >= other.clearing_run.as_nanos()
            && self.sensitive.contains_all(other.sensitive)
    }

    /// The counts that stand for "the limit reached": both at their limits.
    #[must_use]
    pub const fn reached(&self) -> Counts {
        Counts {
            restarts: self.restarts,
            sensitive: self.sensitive_faults,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The two counts kept for the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Starts since the last cold start, or since the last fault that came
    /// after the clearing run, not counting that one.
    pub restarts: u8,
    /// Faults in a PD of a sensitive class since the last cold start.
    pub sensitive: u8,
}

impl Counts {
    /// Both counts zero: as a cold start leaves them.
    pub const ZERO: Self = Self {
        restarts: 0,
        sensitive: 0,
    };

    /// Whether either count has reached its limit.
    #[must_use]
    pub const fn reach_a_limit(&self, limits: &Limits) -> bool {
        self.restarts >= limits.restarts || self.sensitive >= limits.sensitive_faults
    }
}

impl fmt::Display for Counts {
    /// `restart count 2, sensitive count 0`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "restart count {}, sensitive count {}",
            self.restarts, self.sensitive
        )
    }
}

/// How a start came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Power off and on: a person, or a power cut. Telling
    /// it apart from a reset is the boot design still to come.
    Cold,
    /// The supervisor reset the board after a fault in a PD of this class.
    Fault(Class),
    /// The hardware watchdog reset the board.
    Watchdog,
    /// A reset whose cause was never written down.
    Unknown,
}

impl fmt::Display for Start {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cold => f.write_str("a cold start"),
            Self::Fault(class) => write!(f, "a fault in a PD of class {class}"),
            Self::Watchdog => f.write_str("the watchdog"),
            Self::Unknown => f.write_str("a reset whose cause was not written down"),
        }
    }
}

/// What the device does at a start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start the system; this start added 1 to the restart count.
    RestartCounted,
    /// Start the system; this start added nothing to the restart count - a
    /// cold start, or a fault after the clearing run.
    RestartUncounted,
    /// Enter the fail-safe state: stop every PD but the
    /// supervisor, the watchdog and, where the board can show an
    /// out-of-service image, the display ([`Class::may_keep_running`]).
    FailSafe,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RestartCounted => "restart, counted",
            Self::RestartUncounted => "restart, uncounted",
            Self::FailSafe => "enter the fail-safe state",
        })
    }
}

/// What [`decide`] answers: the counts to store, and the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The counts to store for the next boot.
    pub counts: Counts,
    /// What the device does now.
    pub action: Action,
}

/// The fault policy's rule: from the counts read at
/// boot (`None`: they could not be read or checked), how this start came
/// about, and how long the system had run since the start before it
/// (`None`: not known), the counts to store and what the device does.
///
/// * A cold start clears both counts, readable or not, and restarts
///   uncounted.
/// * At any other start, counts that cannot be read give the fail-safe
///   state, storing counts at both limits; so do counts of which either has
///   already reached its limit (they keep growing, never clearing).
/// * Otherwise a fault in a sensitive class adds 1 to the sensitive count; a
///   fault after the clearing run (the run known) clears the restart count
///   and is uncounted, and every other start adds 1 to it; and the device
///   fails safe if either count reaches its limit.
///
/// So the device fails safe exactly when, at a start that is not cold, the
/// counts could not be read or the counts it stores reach a limit. Never
/// panics, and no count overflows: a count stops at 255.
#[must_use]
pub fn decide(
    stored: Option<Counts>,
    start: Start,
    ran_for: Option<Duration>,
    limits: &Limits,
) -> Decision {
    if start == Start::Cold {
        return Decision {
            counts: Counts::ZERO,
            action: Action::RestartUncounted,
        };
    }
    let Some(stored) = stored else {
        return Decision {
            counts: limits.reached(),
            action: Action::FailSafe,
        };
    };
    let sensitive = match start {
        Start::Fault(class) if limits.is_sensitive(class) => stored.sensitive.saturating_add(1),
        _ => stored.sensitive,
    };
    let clears = !stored.reach_a_limit(limits)
        && matches!(start, Start::Fault(_))
        && ran_for.is_some_and(|run| run >= limits.clearing_run);
    let restarts = if clears {
        0
    } else {
        stored.restarts.saturating_add(1)
    };
    let counts = Counts {
        restarts,
        sensitive,
    };
    let action = if counts.reach_a_limit(limits) {
        Action::FailSafe
    } else if clears {
        Action::RestartUncounted
    } else {
        Action::RestartCounted
    };
    Decision { counts, action }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn fault(class: Class, ran_for: Duration, stored: Counts) -> Decision {
        decide(
            Some(stored),
            Start::Fault(class),
            Some(ran_for),
            &Limits::DEFAULT,
        )
    }

    #[test]
    fn the_defaults_are_adr_0014s() {
        let limits = Limits::default();
        assert_eq!(limits, Limits::DEFAULT);
        assert_eq!(limits.restart_limit(), 4);
        assert_eq!(limits.sensitive_limit(), 2);
        assert_eq!(limits.clearing_run(), 10 * MINUTE);
        assert_eq!(
            limits.sensitive().to_string(),
            "security, storage, update, supervisor and entropy"
        );
        for class in Class::ALL {
            let sensitive = matches!(
                class,
                Class::Security
                    | Class::Storage
                    | Class::Update
                    | Class::Supervisor
                    | Class::Entropy
            );
            assert_eq!(limits.is_sensitive(class), sensitive, "{class}");
        }
    }

    #[test]
    fn four_quick_faults_restart_three_times_then_fail_safe() {
        // A cold start, then faults a minute after each start.
        let mut stored = decide(None, Start::Cold, None, &Limits::DEFAULT).counts;
        let mut actions = std::vec::Vec::new();
        for _ in 0..5 {
            let decision = fault(Class::Driver, MINUTE, stored);
            actions.push(decision.action);
            stored = decision.counts;
        }
        assert_eq!(
            actions,
            [
                Action::RestartCounted,
                Action::RestartCounted,
                Action::RestartCounted,
                Action::FailSafe,
                Action::FailSafe,
            ]
        );
        assert_eq!(
            stored,
            Counts {
                restarts: 5,
                sensitive: 0
            }
        );
    }

    #[test]
    fn the_second_sensitive_fault_fails_safe_even_after_ten_minutes() {
        let first = fault(Class::Storage, 11 * MINUTE, Counts::ZERO);
        assert_eq!(first.action, Action::RestartUncounted);
        assert_eq!(
            first.counts,
            Counts {
                restarts: 0,
                sensitive: 1
            }
        );
        let second = fault(Class::Security, 11 * MINUTE, first.counts);
        assert_eq!(second.action, Action::FailSafe);
        assert_eq!(second.counts.sensitive, 2);
    }

    #[test]
    fn a_fault_after_exactly_the_clearing_run_clears_and_one_just_before_counts() {
        let stored = Counts {
            restarts: 3,
            sensitive: 0,
        };
        let at = fault(Class::App, 10 * MINUTE, stored);
        assert_eq!(at.action, Action::RestartUncounted);
        assert_eq!(at.counts.restarts, 0);
        let before = fault(Class::App, 10 * MINUTE - Duration::from_nanos(1), stored);
        assert_eq!(before.action, Action::FailSafe);
        assert_eq!(before.counts.restarts, 4);
    }

    #[test]
    fn a_watchdog_or_unknown_reset_counts_whatever_the_run() {
        for start in [Start::Watchdog, Start::Unknown] {
            let decision = decide(
                Some(Counts::ZERO),
                start,
                Some(Duration::MAX),
                &Limits::DEFAULT,
            );
            assert_eq!(decision.action, Action::RestartCounted, "{start}");
            assert_eq!(decision.counts.restarts, 1, "{start}");
        }
    }

    #[test]
    fn a_fault_whose_run_is_not_known_counts() {
        let decision = decide(
            Some(Counts::ZERO),
            Start::Fault(Class::Ui),
            None,
            &Limits::DEFAULT,
        );
        assert_eq!(decision.action, Action::RestartCounted);
    }

    #[test]
    fn unreadable_counts_fail_safe_except_at_a_cold_start() {
        let limits = Limits::DEFAULT;
        for start in [Start::Fault(Class::Ui), Start::Watchdog, Start::Unknown] {
            let decision = decide(None, start, Some(Duration::MAX), &limits);
            assert_eq!(decision.action, Action::FailSafe, "{start}");
            assert_eq!(decision.counts, limits.reached(), "{start}");
        }
        let cold = decide(None, Start::Cold, None, &limits);
        assert_eq!(cold.action, Action::RestartUncounted);
        assert_eq!(cold.counts, Counts::ZERO);
    }

    #[test]
    fn once_a_limit_is_reached_a_late_fault_does_not_clear_it() {
        let stored = Counts {
            restarts: 4,
            sensitive: 0,
        };
        let decision = fault(Class::Ui, 60 * MINUTE, stored);
        assert_eq!(decision.action, Action::FailSafe);
        assert_eq!(decision.counts.restarts, 5);
    }

    #[test]
    fn counts_stop_at_255() {
        let stored = Counts {
            restarts: 255,
            sensitive: 255,
        };
        let decision = fault(Class::Security, MINUTE, stored);
        assert_eq!(decision.counts, stored);
        assert_eq!(decision.action, Action::FailSafe);
    }

    #[test]
    fn limits_only_tighten() {
        let adr = Limits::DEFAULT;
        let tighter = adr
            .with_restarts_at_most(2)
            .with_sensitive_faults_at_most(1)
            .with_clearing_run_at_least(30 * MINUTE)
            .with_sensitive(Class::Network);
        assert_eq!(tighter.restart_limit(), 2);
        assert_eq!(tighter.sensitive_limit(), 1);
        assert_eq!(tighter.clearing_run(), 30 * MINUTE);
        assert!(tighter.is_sensitive(Class::Network));
        assert!(tighter.at_least_as_strict_as(&adr));
        assert!(!adr.at_least_as_strict_as(&tighter));
        // Asking for looser limits changes nothing.
        let same = adr
            .with_restarts_at_most(9)
            .with_sensitive_faults_at_most(200)
            .with_clearing_run_at_least(MINUTE);
        assert_eq!(same, adr);
    }

    #[test]
    fn a_tighter_limit_fails_safe_sooner() {
        let limits = Limits::DEFAULT.with_restarts_at_most(2);
        let first = decide(Some(Counts::ZERO), Start::Unknown, None, &limits);
        assert_eq!(first.action, Action::RestartCounted);
        let second = decide(Some(first.counts), Start::Unknown, None, &limits);
        assert_eq!(second.action, Action::FailSafe);
    }

    #[test]
    fn only_the_watchdog_and_the_display_may_keep_running() {
        let kept: std::vec::Vec<Class> = Class::ALL
            .into_iter()
            .filter(|class| class.may_keep_running())
            .collect();
        assert_eq!(kept, [Class::Display, Class::Watchdog]);
        // None of them is sensitive under the default limits.
        assert!(
            kept.iter()
                .all(|class| !Limits::DEFAULT.is_sensitive(*class))
        );
    }

    #[test]
    fn classes_are_named_as_the_grant_rules_name_roles() {
        for class in Class::ALL {
            assert_eq!(Class::from_name(class.name()), Some(class));
        }
        assert_eq!(Class::from_name("kiosk"), None);
        let mut bits = 0u16;
        for class in Class::ALL {
            assert_eq!(bits & class.bit(), 0, "{class} shares a bit");
            assert_ne!(class.bit(), 0, "{class} has no bit");
            bits |= class.bit();
        }
    }

    #[test]
    fn reports_read_as_sentences() {
        assert_eq!(
            Start::Fault(Class::Storage).to_string(),
            "a fault in a PD of class storage"
        );
        assert_eq!(
            Counts {
                restarts: 2,
                sensitive: 1
            }
            .to_string(),
            "restart count 2, sensitive count 1"
        );
        assert_eq!(ClassSet::EMPTY.to_string(), "none");
        assert_eq!(
            ClassSet::EMPTY.with(Class::Ui).with(Class::App).to_string(),
            "ui and app"
        );
        assert_eq!(Action::FailSafe.to_string(), "enter the fail-safe state");
    }
}
