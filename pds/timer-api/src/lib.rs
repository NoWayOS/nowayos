// SPDX-License-Identifier: MIT OR Apache-2.0
//! The timer PD's interface: monotonic time and bounded timeouts for other
//! protection domains.
//!
//! A client asks the timer PD by a **protected procedure call** on its
//! channel - `pp="true"` on the client's end, and the timer PD at the higher
//! priority, as Microkit requires. A call carries a message label and up to
//! [`MAX_WORDS`] message registers (machine words), and so does the reply:
//!
//! | Request | Label | Words | Reply when served (label 0) |
//! | --- | --- | --- | --- |
//! | [`Request::Now`] | [`NOW`] | none | word 0: the time, an [`Instant`] in nanoseconds |
//! | [`Request::SetTimeout`] | [`SET_TIMEOUT`] | an id the client chooses; nanoseconds from now | none |
//! | [`Request::Cancel`] | [`CANCEL`] | the id | word 0: 1 if the timeout was still waiting, else 0 |
//! | [`Request::Expired`] | [`EXPIRED`] | none | word 0: the id of the earliest expired timeout not yet collected; no words if there is none |
//!
//! When a timeout's deadline passes, the timer PD **notifies** the client on
//! the same channel. A notification carries no data, and several deadlines
//! can pass before the client runs, so the client then asks
//! [`Request::Expired`] until it answers with no id: it learns its expired
//! timeouts in deadline order. A timeout is never early: its deadline is
//! rounded up to the next counter tick. A timeout lasts from 1 nanosecond to
//! [`MAX_TIMEOUT_NANOS`], and a client has at most [`MAX_PENDING`] timeouts
//! at a time, waiting or expired but not yet collected; a cancelled timeout
//! is gone, and is never reported.
//!
//! A request the timer PD cannot serve gets a reply whose label is an error
//! code, with the details in its words; each `*_reply` method of [`Message`]
//! decodes a reply into a typed result. Nothing here names a board: every
//! board's timer PD answers the same calls, so a client never changes when
//! the timer behind it does.
//!
//! The protocol and the timer PD's bookkeeping ([`schedule`]) are plain data
//! and need no seL4, so they are tested on the host. The `client` feature
//! adds [`Client`], which makes the calls from inside a PD.
//!
//! Provisional.

#![cfg_attr(not(test), no_std)]

use core::fmt;

#[cfg(feature = "client")]
mod client;
pub mod schedule;

#[cfg(feature = "client")]
pub use client::Client;

/// The most message registers a request or reply uses.
pub const MAX_WORDS: usize = 4;

/// The most timeouts a client may have at a time.
pub const MAX_PENDING: usize = 4;

/// The longest timeout: 24 hours, in nanoseconds - enough for session limits
/// and idle resets, and a bound on how long a timeout can hold one of a
/// client's [`MAX_PENDING`] slots. Provisional.
pub const MAX_TIMEOUT_NANOS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The label of [`Request::Now`].
pub const NOW: u64 = 1;
/// The label of [`Request::SetTimeout`].
pub const SET_TIMEOUT: u64 = 2;
/// The label of [`Request::Cancel`].
pub const CANCEL: u64 = 3;
/// The label of [`Request::Expired`].
pub const EXPIRED: u64 = 4;

/// Reply label: the request was served.
const OK: u64 = 0;
/// Error reply labels, one per [`TimerError`] the timer PD sends.
const UNKNOWN_REQUEST: u64 = 1;
const BAD_LENGTH: u64 = 2;
const ZERO_TIMEOUT: u64 = 3;
const TIMEOUT_TOO_LONG: u64 = 4;
const TOO_MANY_TIMEOUTS: u64 = 5;
const ALREADY_PENDING: u64 = 6;
const UNKNOWN_CLIENT: u64 = 7;

/// Monotonic time: nanoseconds since the timer started counting, at boot,
/// never decreasing. Defined in `nowayos-time` beside wall-clock time, which
/// it never mixes with, and re-exported here, so
/// `nowayos_timer_api::Instant` keeps working wherever it is used.
pub use nowayos_time::Instant;

/// What a client can ask the timer PD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Request {
    /// The time now.
    Now,
    /// Notify me `nanos` nanoseconds from now, under `id`.
    SetTimeout {
        /// The client's name for this timeout, reported back when it
        /// expires.
        id: u64,
        /// How long from now, in nanoseconds: at least 1.
        nanos: u64,
    },
    /// Forget timeout `id`, whether waiting or expired.
    Cancel {
        /// The timeout.
        id: u64,
    },
    /// The earliest of my expired timeouts not yet collected.
    Expired,
}

/// What the timer PD answers a request it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The time, for [`Request::Now`].
    Time(Instant),
    /// The timeout is set.
    Set,
    /// Whether the cancelled timeout was still there.
    Cancelled(bool),
    /// The earliest expired timeout's id, or `None`.
    Expired(Option<u64>),
}

/// Why a request was refused, or a reply could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimerError {
    /// The timer PD has no request with this label.
    UnknownRequest {
        /// The label sent.
        label: u64,
    },
    /// The request had the wrong number of words.
    BadLength {
        /// The request's label.
        label: u64,
        /// The number of words sent.
        words: usize,
    },
    /// A timeout of zero nanoseconds: it would already have passed.
    ZeroTimeout,
    /// A timeout longer than [`MAX_TIMEOUT_NANOS`], or whose deadline does
    /// not fit in the counter.
    TimeoutTooLong {
        /// The nanoseconds asked for.
        nanos: u64,
    },
    /// The client already has [`MAX_PENDING`] timeouts.
    TooManyTimeouts {
        /// The limit.
        limit: usize,
    },
    /// The client already has a timeout with this id.
    AlreadyPending {
        /// The id.
        id: u64,
    },
    /// The call came on a channel that is not a client's.
    UnknownClient,
    /// A reply did not have the shape its request's reply must have.
    MalformedReply {
        /// The reply's label.
        label: u64,
        /// The number of words in it.
        words: usize,
    },
}

impl fmt::Display for TimerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRequest { label } => write!(f, "unknown request {label}"),
            Self::BadLength { label, words } => {
                write!(f, "request {label} sent {words} word(s), the wrong number")
            }
            Self::ZeroTimeout => write!(f, "a timeout of zero"),
            Self::TimeoutTooLong { nanos } => write!(f, "a timeout of {nanos} ns is too long"),
            Self::TooManyTimeouts { limit } => {
                write!(f, "too many timeouts: a client may have {limit}")
            }
            Self::AlreadyPending { id } => write!(f, "timeout {id} is already set"),
            Self::UnknownClient => write!(f, "the call did not come from a client"),
            Self::MalformedReply { label, words } => {
                write!(f, "malformed reply: label {label}, {words} word(s)")
            }
        }
    }
}

/// A request or reply as it crosses the channel: a label and up to
/// [`MAX_WORDS`] words. A message that arrived with more words than that
/// keeps only the count, so it is refused rather than cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    label: u64,
    words: [u64; MAX_WORDS],
    count: usize,
}

impl Message {
    /// A message with `label` and `words`; `None` if there are more than
    /// [`MAX_WORDS`] words.
    #[must_use]
    pub fn new(label: u64, words: &[u64]) -> Option<Self> {
        let mut message = Self {
            label,
            words: [0; MAX_WORDS],
            count: words.len(),
        };
        message.words.get_mut(..words.len())?.copy_from_slice(words);
        Some(message)
    }

    /// A message with at most [`MAX_WORDS`] words, which this crate builds
    /// itself (if there were more, it would have none).
    fn fixed(label: u64, words: &[u64]) -> Self {
        Self::new(label, words).unwrap_or(Self {
            label,
            words: [0; MAX_WORDS],
            count: 0,
        })
    }

    /// A message as received: `label`, `count` words, and the first
    /// [`MAX_WORDS`] of them read by `word(index)`. A count above
    /// `MAX_WORDS` is kept, so the message is refused, not truncated.
    #[must_use]
    pub fn received(label: u64, count: usize, mut word: impl FnMut(usize) -> u64) -> Self {
        let mut words = [0; MAX_WORDS];
        for (index, slot) in words.iter_mut().enumerate().take(count) {
            *slot = word(index);
        }
        Self {
            label,
            words,
            count,
        }
    }

    /// The message that asks `request`.
    #[must_use]
    pub fn request_for(request: Request) -> Self {
        match request {
            Request::Now => Self::fixed(NOW, &[]),
            Request::SetTimeout { id, nanos } => Self::fixed(SET_TIMEOUT, &[id, nanos]),
            Request::Cancel { id } => Self::fixed(CANCEL, &[id]),
            Request::Expired => Self::fixed(EXPIRED, &[]),
        }
    }

    /// The label.
    #[must_use]
    pub const fn label(&self) -> u64 {
        self.label
    }

    /// The words, or `None` if the message arrived with more than
    /// [`MAX_WORDS`].
    #[must_use]
    pub fn words(&self) -> Option<&[u64]> {
        self.words.get(..self.count)
    }

    /// The number of words the message has (or arrived with).
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The request this message carries.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownRequest`] for an unknown label;
    /// [`TimerError::BadLength`] for the wrong number of words.
    pub fn request(&self) -> Result<Request, TimerError> {
        let bad_length = TimerError::BadLength {
            label: self.label,
            words: self.count,
        };
        match (self.label, self.words()) {
            (NOW, Some([])) => Ok(Request::Now),
            (SET_TIMEOUT, Some(&[id, nanos])) => Ok(Request::SetTimeout { id, nanos }),
            (CANCEL, Some(&[id])) => Ok(Request::Cancel { id }),
            (EXPIRED, Some([])) => Ok(Request::Expired),
            (NOW | SET_TIMEOUT | CANCEL | EXPIRED, _) => Err(bad_length),
            (label, _) => Err(TimerError::UnknownRequest { label }),
        }
    }

    /// The reply that carries `result`.
    #[must_use]
    pub fn reply_for(result: Result<Reply, TimerError>) -> Self {
        let wide = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        match result {
            Ok(Reply::Time(now)) => Self::fixed(OK, &[now.as_nanos()]),
            Ok(Reply::Set | Reply::Expired(None)) => Self::fixed(OK, &[]),
            Ok(Reply::Cancelled(was)) => Self::fixed(OK, &[u64::from(was)]),
            Ok(Reply::Expired(Some(id))) => Self::fixed(OK, &[id]),
            Err(TimerError::UnknownRequest { label }) => Self::fixed(UNKNOWN_REQUEST, &[label]),
            Err(TimerError::BadLength { label, words }) => {
                Self::fixed(BAD_LENGTH, &[label, wide(words)])
            }
            Err(TimerError::ZeroTimeout) => Self::fixed(ZERO_TIMEOUT, &[]),
            Err(TimerError::TimeoutTooLong { nanos }) => Self::fixed(TIMEOUT_TOO_LONG, &[nanos]),
            Err(TimerError::TooManyTimeouts { limit }) => {
                Self::fixed(TOO_MANY_TIMEOUTS, &[wide(limit)])
            }
            Err(TimerError::AlreadyPending { id }) => Self::fixed(ALREADY_PENDING, &[id]),
            Err(TimerError::UnknownClient | TimerError::MalformedReply { .. }) => {
                Self::fixed(UNKNOWN_CLIENT, &[])
            }
        }
    }

    /// Reads this message as the reply to [`Request::Now`].
    ///
    /// # Errors
    ///
    /// The error the timer PD refused the request with, or
    /// [`TimerError::MalformedReply`] if the reply has the wrong shape.
    pub fn now_reply(&self) -> Result<Instant, TimerError> {
        match (self.label, self.words()) {
            (OK, Some(&[nanos])) => Ok(Instant::from_nanos(nanos)),
            _ => Err(self.refusal()),
        }
    }

    /// Reads this message as the reply to [`Request::SetTimeout`].
    ///
    /// # Errors
    ///
    /// As [`now_reply`](Self::now_reply).
    pub fn set_reply(&self) -> Result<(), TimerError> {
        match (self.label, self.words()) {
            (OK, Some([])) => Ok(()),
            _ => Err(self.refusal()),
        }
    }

    /// Reads this message as the reply to [`Request::Cancel`]: whether the
    /// timeout was still there.
    ///
    /// # Errors
    ///
    /// As [`now_reply`](Self::now_reply).
    pub fn cancel_reply(&self) -> Result<bool, TimerError> {
        match (self.label, self.words()) {
            (OK, Some(&[0])) => Ok(false),
            (OK, Some(&[1])) => Ok(true),
            _ => Err(self.refusal()),
        }
    }

    /// Reads this message as the reply to [`Request::Expired`].
    ///
    /// # Errors
    ///
    /// As [`now_reply`](Self::now_reply).
    pub fn expired_reply(&self) -> Result<Option<u64>, TimerError> {
        match (self.label, self.words()) {
            (OK, Some([])) => Ok(None),
            (OK, Some(&[id])) => Ok(Some(id)),
            _ => Err(self.refusal()),
        }
    }

    /// The error a refusal carries, or [`TimerError::MalformedReply`].
    fn refusal(&self) -> TimerError {
        let malformed = TimerError::MalformedReply {
            label: self.label,
            words: self.count,
        };
        let narrow = |value: u64| usize::try_from(value).ok();
        match (self.label, self.words()) {
            (UNKNOWN_REQUEST, Some(&[label])) => TimerError::UnknownRequest { label },
            (BAD_LENGTH, Some(&[label, words])) => {
                narrow(words).map_or(malformed, |words| TimerError::BadLength { label, words })
            }
            (ZERO_TIMEOUT, Some([])) => TimerError::ZeroTimeout,
            (TIMEOUT_TOO_LONG, Some(&[nanos])) => TimerError::TimeoutTooLong { nanos },
            (TOO_MANY_TIMEOUTS, Some(&[limit])) => {
                narrow(limit).map_or(malformed, |limit| TimerError::TooManyTimeouts { limit })
            }
            (ALREADY_PENDING, Some(&[id])) => TimerError::AlreadyPending { id },
            (UNKNOWN_CLIENT, Some([])) => TimerError::UnknownClient,
            _ => malformed,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn instant_is_nowayos_time_s_monotonic_time() {
        // The re-export is the same type, so an Instant from either path is
        // the other's: the timer's clients and the clock PD share it.
        let from_time: nowayos_time::Instant = Instant::from_nanos(1_500);
        let from_timer: Instant = nowayos_time::Instant::from_ticks(3, 3);
        assert_eq!(from_time.as_nanos(), 1_500);
        assert_eq!(from_timer.as_nanos(), 1_000_000_000);
        assert_eq!(from_timer.saturating_since(from_time), 999_998_500);
    }

    #[test]
    fn every_request_round_trips() {
        for request in [
            Request::Now,
            Request::SetTimeout { id: 7, nanos: 1 },
            Request::Cancel { id: u64::MAX },
            Request::Expired,
        ] {
            assert_eq!(Message::request_for(request).request(), Ok(request));
        }
    }

    #[test]
    fn every_reply_round_trips() {
        let now = Instant::from_nanos(1_500);
        let reply = |result| Message::reply_for(result);
        assert_eq!(reply(Ok(Reply::Time(now))).now_reply(), Ok(now));
        assert_eq!(reply(Ok(Reply::Set)).set_reply(), Ok(()));
        assert_eq!(reply(Ok(Reply::Cancelled(true))).cancel_reply(), Ok(true));
        assert_eq!(reply(Ok(Reply::Cancelled(false))).cancel_reply(), Ok(false));
        assert_eq!(
            reply(Ok(Reply::Expired(Some(9)))).expired_reply(),
            Ok(Some(9))
        );
        assert_eq!(reply(Ok(Reply::Expired(None))).expired_reply(), Ok(None));
        for error in [
            TimerError::UnknownRequest { label: 99 },
            TimerError::BadLength {
                label: NOW,
                words: 2,
            },
            TimerError::ZeroTimeout,
            TimerError::TimeoutTooLong { nanos: u64::MAX },
            TimerError::TooManyTimeouts { limit: MAX_PENDING },
            TimerError::AlreadyPending { id: 3 },
            TimerError::UnknownClient,
        ] {
            let message = reply(Err(error));
            assert_eq!(message.now_reply(), Err(error));
            assert_eq!(message.set_reply(), Err(error));
            assert_eq!(message.cancel_reply(), Err(error));
            assert_eq!(message.expired_reply(), Err(error));
        }
    }

    #[test]
    fn malformed_requests_are_refused_with_a_typed_error() {
        let cases = [
            (
                Message::new(99, &[]).unwrap(),
                TimerError::UnknownRequest { label: 99 },
            ),
            (
                Message::new(0, &[7]).unwrap(),
                TimerError::UnknownRequest { label: 0 },
            ),
            (
                Message::new(NOW, &[1, 2]).unwrap(),
                TimerError::BadLength {
                    label: NOW,
                    words: 2,
                },
            ),
            (
                Message::new(SET_TIMEOUT, &[1]).unwrap(),
                TimerError::BadLength {
                    label: SET_TIMEOUT,
                    words: 1,
                },
            ),
            (
                Message::new(CANCEL, &[]).unwrap(),
                TimerError::BadLength {
                    label: CANCEL,
                    words: 0,
                },
            ),
            (
                Message::received(EXPIRED, 200, |_| 0),
                TimerError::BadLength {
                    label: EXPIRED,
                    words: 200,
                },
            ),
        ];
        for (request, error) in cases {
            assert_eq!(request.request(), Err(error));
        }
    }

    #[test]
    fn a_reply_of_the_wrong_shape_is_malformed() {
        for (label, words) in [
            (OK, &[][..]),
            (OK, &[1, 2][..]),
            (7, &[1][..]),
            (BAD_LENGTH, &[1][..]),
            (99, &[][..]),
        ] {
            let reply = Message::new(label, words).unwrap();
            assert_eq!(
                reply.now_reply(),
                Err(TimerError::MalformedReply {
                    label,
                    words: words.len()
                })
            );
        }
        assert!(matches!(
            Message::new(OK, &[2]).unwrap().cancel_reply(),
            Err(TimerError::MalformedReply { .. })
        ));
        let oversized = Message::received(OK, MAX_WORDS + 1, |_| 3);
        assert_eq!(oversized.words(), None);
        assert!(matches!(
            oversized.now_reply(),
            Err(TimerError::MalformedReply { .. })
        ));
    }

    #[test]
    fn messages_hold_at_most_max_words() {
        assert!(Message::new(NOW, &[0; MAX_WORDS]).is_some());
        assert!(Message::new(NOW, &[0; MAX_WORDS + 1]).is_none());
        let received = Message::received(5, 2, |index| [10, 20][index]);
        assert_eq!(received.words(), Some(&[10, 20][..]));
        assert_eq!((received.label(), received.count()), (5, 2));
    }
}
