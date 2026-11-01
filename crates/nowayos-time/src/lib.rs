// SPDX-License-Identifier: MIT OR Apache-2.0
//! Monotonic time and wall-clock time, as two types that never mix.
//!
//! * **Monotonic time** is [`Instant`]: nanoseconds since the counter started,
//!   at boot, from the timer PD. It is never set and never goes back.
//!   Timeouts, retransmission, idle resets, session limits, leases and rate
//!   limits use it alone, so a step in the wall clock can never fire them
//!   early or hold them back. `nowayos-timer-api` re-exports it.
//! * **Wall-clock time** is [`Utc`], and what a device knows of it is a
//!   [`UtcInterval`]: never earlier than its `earliest`, never later than its
//!   `latest` - which may be unknown - together with the [`Source`] it came
//!   from. One clock PD holds it ([`Clock`] is its bookkeeping); only the
//!   clock relates the two kinds of time.
//!
//! # The floor
//!
//! An image records the time it was built - the commit time of its source -
//! and the device runs it only after that, so from the moment the clock
//! starts, UTC is no earlier than the build time: the **floor**
//! ([`Anchor::floor`]). Until an authenticated source answers, the interval
//! runs from the floor, moved on by the monotonic time since the clock
//! started, to "not known".
//!
//! # Anchors and the drift bound
//!
//! An [`Anchor`] is an interval measured at an [`Instant`]. Asked for the
//! interval at a later Instant ([`Anchor::interval_at`]), it moves
//! `earliest` on by the elapsed time **less** the drift the counter may have
//! made, and `latest` by the elapsed time **plus** it: [`DRIFT_PPM`], 100
//! parts per million of true time, until a board's datasheet gives a
//! tighter figure - `ceil(elapsed / 9,999)` of the elapsed time as measured
//! ([`drift`]). The drift is rounded up, so the interval only ever widens,
//! and every sum is
//! checked: `earliest` stops at [`Utc::MAX`], which is still no later than
//! the true time, and a `latest` past [`Utc::MAX`] becomes unknown.
//!
//! # Samples
//!
//! A source - the TLS PD with the pinned backend's `Date`, or a test PD in
//! its place - sends the clock a [`Sample`]: the interval as measured, and
//! the Instants its request was sent and its answer arrived.
//! [`Sample::from_date`] makes one from a `Date` and its round trip. The
//! clock refuses a sample ([`Sample::check`], [`Clock::take`]) whose
//! earliest is after its latest, whose answer arrived before its request
//! was sent or at an Instant still to come, that is narrower than its round
//! trip, or whose earliest is before the floor at the Instant it was
//! measured - each with its own [`TimeError`] - and a valid one replaces the
//! anchor only if it was measured later than the one held.
//!
//! # Date checks
//!
//! [`UtcInterval::check_validity`] passes a certificate only if it is valid
//! for the **whole** interval: `notBefore <= earliest` and
//! `latest <= notAfter`. While `latest` is not known, it always refuses.
//!
//! # Records
//!
//! The sample a source sends on its ring and the clock's answer to "what is
//! the time?" ([`Reading`]) are fixed-size `nowayos-ipc` records, validated
//! field by field when decoded: whatever decodes has its earliest no later
//! than its latest. Their layouts are in [`record`]; a reading crosses a
//! protected procedure call as five message registers
//! ([`Reading::to_words`], [`Reading::from_words`]), the clock PD's answer
//! (`nowayos-clock-api`).
//!
//! ```
//! use nowayos_time::{Clock, Instant, Sample, Source, Taken, Utc, ValidityError};
//!
//! const DAY: u64 = 86_400;
//! let ms = |ms: u64| Instant::from_nanos(ms * 1_000_000);
//! let build_time = Utc::from_unix_seconds(1_800_000_000).unwrap();
//! let seconds = |s: u64| Utc::from_unix_seconds(s).unwrap();
//! // A certificate valid from a day before the build to a year after it.
//! let (not_before, not_after) = (seconds(1_800_000_000 - DAY), seconds(1_800_000_000 + 365 * DAY));
//!
//! // At boot only the floor is known: every date check fails closed.
//! let mut clock = Clock::new(build_time, ms(100));
//! let floor = clock.read(ms(150)).unwrap();
//! assert_eq!(floor.source(), Source::BuildTime);
//! assert_eq!(floor.check_validity(not_before, not_after), Err(ValidityError::TimeNotKnown));
//!
//! // The backend's Date, a day after the build, over a 40 ms round trip.
//! let sample = Sample::from_date(seconds(1_800_000_000 + DAY), ms(200), ms(240)).unwrap();
//! assert_eq!(clock.take(&sample, Source::Backend, ms(250)), Ok(Taken::Newer));
//! let known = clock.read(ms(300)).unwrap();
//! assert_eq!(known.check_validity(not_before, not_after), Ok(()));
//! ```
//!
//! Plain data: no `unsafe`, no allocation, `no_std`. Provisional.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod anchor;
mod clock;
mod error;
mod instant;
#[cfg(kani)]
mod proofs;
pub mod record;
mod sample;
mod utc;

pub use anchor::{Anchor, DRIFT_PPM, drift};
pub use clock::{Clock, Taken};
pub use error::{TimeError, ValidityError};
pub use instant::Instant;
pub use record::Reading;
pub use sample::Sample;
pub use utc::{NANOS_PER_SECOND, Source, Utc, UtcInterval};
