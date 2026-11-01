// SPDX-License-Identifier: MIT OR Apache-2.0
//! The error types. Every way a sample, a reading or a date check can be
//! refused has a variant here; nothing in this crate panics instead.
//!
//! The messages name no time or Instant, so a PD can print them as fixed
//! text: a test's expected output never depends on when it ran.

use core::fmt;

use crate::{Instant, Utc};

/// Why the clock refused a sample or a reading. Nothing changes when one is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeError {
    /// The sample's earliest is after its latest.
    EarliestAfterLatest {
        /// Its earliest.
        earliest: Utc,
        /// Its latest.
        latest: Utc,
    },
    /// The sample's answer arrived before its request was sent.
    ReceivedBeforeSent {
        /// When its request was sent.
        sent: Instant,
        /// When its answer arrived.
        received: Instant,
    },
    /// The sample was measured at an Instant still to come.
    FromTheFuture {
        /// When its answer arrived, by its own account.
        received: Instant,
        /// The clock's now.
        now: Instant,
    },
    /// The sample is narrower than its round trip, so it claims more than
    /// its source can know.
    NarrowerThanRoundTrip {
        /// Nanoseconds from its earliest to its latest.
        width: u64,
        /// Nanoseconds from its request to its answer.
        round_trip: u64,
    },
    /// The sample's earliest is before the floor at the Instant it was
    /// measured: an attack or a fault.
    BeforeFloor {
        /// Its earliest.
        earliest: Utc,
        /// The floor's earliest at the Instant it was measured.
        floor: Utc,
    },
    /// A sample was offered as the build time's: the floor is the image's
    /// own, never a sample.
    NotASampleSource,
    /// A time beyond [`Utc::MAX`] (2554).
    OutOfRange,
    /// Monotonic time went back: an Instant earlier than one the clock was
    /// already given.
    WentBack {
        /// The Instant given now.
        now: Instant,
        /// The latest Instant given before it.
        seen: Instant,
    },
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EarliestAfterLatest { .. } => "its earliest is after its latest",
            Self::ReceivedBeforeSent { .. } => "its answer arrived before its request was sent",
            Self::FromTheFuture { .. } => "it was measured at an Instant still to come",
            Self::NarrowerThanRoundTrip { .. } => "it is narrower than its round trip",
            Self::BeforeFloor { .. } => "its earliest is before the floor",
            Self::NotASampleSource => "the build time gives no samples",
            Self::OutOfRange => "the time is beyond what a Utc holds",
            Self::WentBack { .. } => "monotonic time went back",
        })
    }
}

impl core::error::Error for TimeError {}

/// Why a certificate's validity period was refused against a
/// [`UtcInterval`](crate::UtcInterval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidityError {
    /// The time is not known (no `latest`): every date check fails closed.
    TimeNotKnown,
    /// The certificate's `notBefore` is after the interval's earliest: it
    /// may not be valid yet.
    NotYetValid,
    /// The certificate's `notAfter` is before the interval's latest: it may
    /// have expired.
    Expired,
}

impl fmt::Display for ValidityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TimeNotKnown => "the time is not known",
            Self::NotYetValid => "it may not be valid yet",
            Self::Expired => "it may have expired",
        })
    }
}

impl core::error::Error for ValidityError {}
