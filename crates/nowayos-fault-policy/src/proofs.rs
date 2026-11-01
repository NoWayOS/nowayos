// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs: for **every** input - any counts read at boot, or
//! none; any start, a fault in any class; any run, or none; and any limits,
//! with any sensitive classes - [`decide`] never panics or overflows, never
//! answers restart once either limit is reached, and fails safe exactly when
//! a count reaches its limit at a start that is not cold. And the
//! supervisor's stand-in counts: their reader and their record's
//! decoder never panic, for every input up to one byte past what they read.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use core::time::Duration;

use crate::stand_in::{self, PROVEN_LEN, RECORD_LEN, STAND_IN_LEN};
use crate::{Action, Class, ClassSet, Counts, Decision, Limits, Start, decide};

/// Any class.
fn any_class() -> Class {
    let index: usize = kani::any_where(|index: &usize| *index < Class::ALL.len());
    Class::ALL[index]
}

/// Any start: cold, the watchdog, unknown, or a fault in any class.
fn any_start() -> Start {
    match kani::any::<u8>() {
        0 => Start::Cold,
        1 => Start::Watchdog,
        2 => Start::Unknown,
        _ => Start::Fault(any_class()),
    }
}

/// Any counts.
fn any_counts() -> Counts {
    Counts {
        restarts: kani::any(),
        sensitive: kani::any(),
    }
}

/// Any duration that can exist: whole seconds, and nanoseconds below one
/// second.
fn any_duration() -> Duration {
    let nanos: u32 = kani::any_where(|nanos: &u32| *nanos < 1_000_000_000);
    Duration::new(kani::any(), nanos)
}

/// Any limits at all - not only the default ones or tighter ones - with any set
/// of sensitive classes, bits for no class included.
fn any_limits() -> Limits {
    Limits {
        restarts: kani::any(),
        sensitive_faults: kani::any(),
        clearing_run: any_duration(),
        sensitive: ClassSet(kani::any()),
    }
}

/// One decision on any input, with that input.
fn any_decision() -> (Option<Counts>, Start, Option<Duration>, Limits, Decision) {
    let stored = if kani::any() {
        Some(any_counts())
    } else {
        None
    };
    let start = any_start();
    let ran_for = if kani::any() {
        Some(any_duration())
    } else {
        None
    };
    let limits = any_limits();
    let decision = decide(stored, start, ran_for, &limits);
    (stored, start, ran_for, limits, decision)
}

/// `decide` returns for every input: no panic, and no arithmetic overflow
/// (Kani checks every operation); the counts it stores are never below
/// zero or above 255 by construction of `u8`, and a count stops at 255.
#[kani::proof]
fn decide_never_panics_or_overflows() {
    let (_, _, _, _, decision) = any_decision();
    assert!(matches!(
        decision.action,
        Action::RestartCounted | Action::RestartUncounted | Action::FailSafe
    ));
}

/// Once either count has reached its limit, or the counts cannot be read,
/// no start but a cold one answers restart; and what it stores still
/// reaches a limit, so the next start fails safe too.
#[kani::proof]
fn never_restarts_once_a_limit_is_reached() {
    let (stored, start, _, limits, decision) = any_decision();
    let reached = stored.is_none_or(|counts| counts.reach_a_limit(&limits));
    if start != Start::Cold && reached {
        assert!(decision.action == Action::FailSafe);
        assert!(decision.counts.reach_a_limit(&limits));
    }
}

/// The device fails safe exactly when, at a start that is not cold, the
/// counts could not be read or the counts it stores reach a limit.
#[kani::proof]
fn fails_safe_exactly_when_a_count_reaches_its_limit() {
    let (stored, start, _, limits, decision) = any_decision();
    let fails_safe =
        start != Start::Cold && (stored.is_none() || decision.counts.reach_a_limit(&limits));
    assert!((decision.action == Action::FailSafe) == fails_safe);
}

/// Only a cold start clears the sensitive count, and it clears both counts
/// whatever was read; at any other start the sensitive count grows by
/// exactly the start's own sensitive fault, if the counts were read.
#[kani::proof]
fn only_a_cold_start_clears_the_sensitive_count() {
    let (stored, start, _, limits, decision) = any_decision();
    match (start, stored) {
        (Start::Cold, _) => {
            assert!(decision.counts == Counts::ZERO);
            assert!(decision.action == Action::RestartUncounted);
        }
        (Start::Fault(class), Some(counts)) if limits.is_sensitive(class) => {
            assert!(decision.counts.sensitive == counts.sensitive.saturating_add(1));
        }
        (_, Some(counts)) => {
            assert!(decision.counts.sensitive == counts.sensitive);
        }
        (_, None) => {
            assert!(decision.counts == limits.reached());
        }
    }
}

/// The restart count falls only to zero, and only after a fault that came
/// once the system had run at least the clearing run, with both counts
/// below their limits; that start is uncounted unless a limit of zero makes
/// it fail safe. Every other start that is not cold adds exactly 1.
#[kani::proof]
fn only_a_late_fault_clears_the_restart_count() {
    let (stored, start, ran_for, limits, decision) = any_decision();
    let Some(counts) = stored else {
        return;
    };
    if start == Start::Cold {
        return;
    }
    let clears = !counts.reach_a_limit(&limits)
        && matches!(start, Start::Fault(_))
        && ran_for.is_some_and(|run| run >= limits.clearing_run());
    if clears {
        assert!(decision.counts.restarts == 0);
        assert!(decision.action != Action::RestartCounted);
    } else {
        assert!(decision.counts.restarts == counts.restarts.saturating_add(1));
        assert!(decision.action != Action::RestartUncounted);
    }
}

/// The stand-in's reader, for every input up to one byte past a
/// stand-in (`stand_in::PROVEN_LEN`, 12 bytes: all the supervisor copies
/// out of its region and passes it), never panics, and what it reads is
/// exactly a record's counts behind a length word of 7, as
/// `stand_in::stand_in` writes them; a byte past them never matters.
#[kani::proof]
#[kani::unwind(13)]
fn stand_in_read_any_input() {
    let bytes: [u8; PROVEN_LEN] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= PROVEN_LEN);
    let input = &bytes[..len];
    if let Ok(counts) = stand_in::read(input) {
        let expected = stand_in::stand_in(counts);
        assert!(input.get(..STAND_IN_LEN) == Some(&expected[..]));
    }
}

/// The record's decoder, for every input up to one byte past a record,
/// never panics, and accepts exactly the records `encode` writes.
#[kani::proof]
#[kani::unwind(9)]
fn stand_in_decode_any_record() {
    let bytes: [u8; RECORD_LEN + 1] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= RECORD_LEN + 1);
    let input = &bytes[..len];
    if let Ok(counts) = stand_in::decode(input) {
        let expected = stand_in::encode(counts);
        assert!(input == &expected[..]);
    }
}
