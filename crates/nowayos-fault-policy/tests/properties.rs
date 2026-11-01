// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the fault policy's rule: one start at a time,
//! and any sequence of starts on a device that stores what [`decide`]
//! answers, checked against a model of the fault policy's counting rules written as
//! the rules read rather than as `decide` computes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use core::time::Duration;

use nowayos_fault_policy::{Action, Class, Counts, Limits, Start, decide};
use proptest::prelude::*;

/// The default clearing run, in milliseconds.
const TEN_MINUTES_MS: u64 = 600_000;

fn class() -> impl Strategy<Value = Class> {
    prop::sample::select(Class::ALL.to_vec())
}

fn start() -> impl Strategy<Value = Start> {
    prop_oneof![
        1 => Just(Start::Cold),
        4 => class().prop_map(Start::Fault),
        1 => Just(Start::Watchdog),
        1 => Just(Start::Unknown),
    ]
}

/// How long the system had run: often near ten minutes, sometimes not
/// known.
fn ran_for() -> impl Strategy<Value = Option<Duration>> {
    prop::option::weighted(
        0.9,
        prop_oneof![
            Just(TEN_MINUTES_MS - 1),
            Just(TEN_MINUTES_MS),
            Just(TEN_MINUTES_MS + 1),
            0..3 * TEN_MINUTES_MS,
            any::<u64>(),
        ]
        .prop_map(Duration::from_millis),
    )
}

/// Counts as they might be read: mostly small, sometimes anything.
fn counts() -> impl Strategy<Value = Counts> {
    let count = prop_oneof![4 => 0u8..6, 1 => any::<u8>()];
    (count.clone(), count).prop_map(|(restarts, sensitive)| Counts {
        restarts,
        sensitive,
    })
}

/// The default limits, or tighter ones, as a product could set them.
fn limits() -> impl Strategy<Value = Limits> {
    prop_oneof![
        Just(Limits::DEFAULT),
        (
            0u8..=4,
            0u8..=2,
            TEN_MINUTES_MS..4 * TEN_MINUTES_MS,
            prop::option::of(class()),
        )
            .prop_map(|(restarts, sensitive, clearing_ms, extra)| {
                let limits = Limits::DEFAULT
                    .with_restarts_at_most(restarts)
                    .with_sensitive_faults_at_most(sensitive)
                    .with_clearing_run_at_least(Duration::from_millis(clearing_ms));
                match extra {
                    Some(class) => limits.with_sensitive(class),
                    None => limits,
                }
            }),
    ]
}

/// One start of a device: how it came about, how long the system had run
/// before it, and whether reading the counts fails at this start.
#[derive(Debug, Clone, Copy)]
struct Boot {
    start: Start,
    ran_for: Option<Duration>,
    unreadable: bool,
}

fn boots() -> impl Strategy<Value = Vec<Boot>> {
    prop::collection::vec(
        (start(), ran_for(), prop::bool::weighted(0.05)).prop_map(
            |(start, ran_for, unreadable)| Boot {
                start,
                ran_for,
                unreadable,
            },
        ),
        1..80,
    )
}

/// The counting rules as they read, kept apart from `decide`: a
/// device's counts since its last cold start, and whether it is in its
/// fail-safe state. A new device has stored nothing, so its counts cannot
/// be read until its first cold start.
#[derive(Debug)]
struct Model {
    stored_anything: bool,
    restarts: u32,
    sensitive: u32,
    failed_safe: bool,
}

impl Model {
    fn new() -> Self {
        Self {
            stored_anything: false,
            restarts: 0,
            sensitive: 0,
            failed_safe: false,
        }
    }

    /// What the device must do at `boot`.
    fn boot(&mut self, boot: Boot, limits: &Limits) -> Action {
        let readable = self.stored_anything && !boot.unreadable;
        self.stored_anything = true;
        if boot.start == Start::Cold {
            // Only a cold start brings the device back, clearing both counts.
            self.restarts = 0;
            self.sensitive = 0;
            self.failed_safe = false;
            return Action::RestartUncounted;
        }
        if self.failed_safe {
            return Action::FailSafe;
        }
        if !readable {
            // A count that cannot be read counts as the limit reached.
            self.failed_safe = true;
            return Action::FailSafe;
        }
        if let Start::Fault(class) = boot.start
            && limits.is_sensitive(class)
        {
            self.sensitive += 1;
        }
        let late_fault = matches!(boot.start, Start::Fault(_))
            && boot.ran_for.is_some_and(|run| run >= limits.clearing_run());
        if late_fault {
            self.restarts = 0;
        } else {
            self.restarts += 1;
        }
        if self.restarts >= u32::from(limits.restart_limit())
            || self.sensitive >= u32::from(limits.sensitive_limit())
        {
            self.failed_safe = true;
            return Action::FailSafe;
        }
        if late_fault {
            Action::RestartUncounted
        } else {
            Action::RestartCounted
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// One start: the device fails safe exactly when, at a start that is not
    /// cold, the counts could not be read or the counts it stores reach a
    /// limit; and once either limit is reached only a cold start restarts it.
    #[test]
    fn fails_safe_exactly_when_a_count_reaches_its_limit(
        stored in prop::option::weighted(0.9, counts()),
        start in start(),
        ran_for in ran_for(),
        limits in limits(),
    ) {
        let decision = decide(stored, start, ran_for, &limits);
        let reaches = decision.counts.restarts >= limits.restart_limit()
            || decision.counts.sensitive >= limits.sensitive_limit();
        let fails_safe = start != Start::Cold && (stored.is_none() || reaches);
        prop_assert_eq!(decision.action == Action::FailSafe, fails_safe);
        if let Some(counts) = stored {
            let reached = counts.restarts >= limits.restart_limit()
                || counts.sensitive >= limits.sensitive_limit();
            if reached && start != Start::Cold {
                prop_assert_eq!(decision.action, Action::FailSafe);
            }
        }
    }

    /// Only a cold start clears both counts, and it does so even when they
    /// cannot be read.
    #[test]
    fn only_a_cold_start_clears_both(
        stored in prop::option::of(counts()),
        ran_for in ran_for(),
        limits in limits(),
    ) {
        let decision = decide(stored, Start::Cold, ran_for, &limits);
        prop_assert_eq!(decision.counts, Counts::ZERO);
        prop_assert_eq!(decision.action, Action::RestartUncounted);
    }

    /// Counts that cannot be read give the fail-safe state at every start
    /// but a cold one, and store counts at both limits.
    #[test]
    fn unreadable_counts_fail_safe(
        start in start().prop_filter("not cold", |start| *start != Start::Cold),
        ran_for in ran_for(),
        limits in limits(),
    ) {
        let decision = decide(None, start, ran_for, &limits);
        prop_assert_eq!(decision.action, Action::FailSafe);
        prop_assert_eq!(decision.counts, limits.reached());
    }

    /// Below both limits, a fault after the clearing run clears the restart
    /// count and restarts uncounted, while a sensitive fault still adds to
    /// the sensitive count; any other start that is not cold adds 1 to the
    /// restart count.
    #[test]
    fn a_late_fault_clears_the_restart_count_but_not_the_sensitive_count(
        restarts in 0u8..4,
        sensitive in 0u8..2,
        class in class(),
        ran_for in ran_for(),
    ) {
        let limits = Limits::DEFAULT;
        let stored = Counts { restarts, sensitive };
        let decision = decide(Some(stored), Start::Fault(class), ran_for, &limits);
        let late = ran_for.is_some_and(|run| run >= Duration::from_millis(TEN_MINUTES_MS));
        let expected_sensitive = sensitive + u8::from(limits.is_sensitive(class));
        prop_assert_eq!(decision.counts.sensitive, expected_sensitive);
        if late {
            prop_assert_eq!(decision.counts.restarts, 0);
            let expected = if expected_sensitive >= 2 {
                Action::FailSafe
            } else {
                Action::RestartUncounted
            };
            prop_assert_eq!(decision.action, expected);
        } else {
            prop_assert_eq!(decision.counts.restarts, restarts + 1);
            prop_assert_ne!(decision.action, Action::RestartUncounted);
        }
    }

    /// Any sequence of starts on a device that stores what `decide` answers
    /// and reads it back at the next start - unless reading fails - does
    /// what the model of the counting rules says, start by start: the
    /// same action, and below the limits the same counts.
    #[test]
    fn any_sequence_of_starts_follows_the_rules(boots in boots(), limits in limits()) {
        let mut stored: Option<Counts> = None;
        let mut model = Model::new();
        let mut failed_safe_since_cold = false;
        let mut counted_since_clear = 0u32;
        for (index, boot) in boots.iter().enumerate() {
            let read = if boot.unreadable { None } else { stored };
            let decision = decide(read, boot.start, boot.ran_for, &limits);
            let expected = model.boot(*boot, &limits);
            prop_assert_eq!(decision.action, expected, "start {} of {:?}", index, boots);
            if decision.action != Action::FailSafe {
                prop_assert_eq!(u32::from(decision.counts.restarts), model.restarts);
                prop_assert_eq!(u32::from(decision.counts.sensitive), model.sensitive);
            }
            // Once the device fails safe, only a cold start restarts it.
            if boot.start == Start::Cold {
                failed_safe_since_cold = false;
                counted_since_clear = 0;
            } else if failed_safe_since_cold {
                prop_assert_eq!(decision.action, Action::FailSafe);
            }
            match decision.action {
                Action::FailSafe => failed_safe_since_cold = true,
                Action::RestartCounted => counted_since_clear += 1,
                Action::RestartUncounted => counted_since_clear = 0,
            }
            // No run of counted restarts reaches the restart limit.
            prop_assert!(
                counted_since_clear == 0
                    || counted_since_clear < u32::from(limits.restart_limit())
            );
            stored = Some(decision.counts);
        }
    }

    /// Over any sequence, the sensitive count falls only at a cold start.
    #[test]
    fn the_sensitive_count_falls_only_at_a_cold_start(boots in boots(), limits in limits()) {
        let mut stored: Option<Counts> = None;
        for boot in &boots {
            let read = if boot.unreadable { None } else { stored };
            let decision = decide(read, boot.start, boot.ran_for, &limits);
            if let (Some(before), false) = (read, boot.start == Start::Cold) {
                prop_assert!(decision.counts.sensitive >= before.sensitive);
            }
            stored = Some(decision.counts);
        }
    }

    /// Tightened limits are never looser than the default ones, whatever is asked.
    #[test]
    fn limits_can_only_tighten(
        restarts in any::<u8>(),
        sensitive in any::<u8>(),
        clearing_ms in any::<u64>(),
        extra in prop::option::of(class()),
    ) {
        let mut limits = Limits::DEFAULT
            .with_restarts_at_most(restarts)
            .with_sensitive_faults_at_most(sensitive)
            .with_clearing_run_at_least(Duration::from_millis(clearing_ms));
        if let Some(class) = extra {
            limits = limits.with_sensitive(class);
        }
        prop_assert!(limits.at_least_as_strict_as(&Limits::DEFAULT));
    }
}
