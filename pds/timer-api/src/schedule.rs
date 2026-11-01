// SPDX-License-Identifier: MIT OR Apache-2.0
//! The timer PD's bookkeeping: each client's timeouts, which deadline the
//! hardware timer must fire at next, and which clients to notify when it
//! does. Plain data, no seL4, so it is tested on the host; the timer PD
//! feeds it the counter and programs the timer from it.
//!
//! Deadlines are counter ticks. A timeout of `n` nanoseconds expires at the
//! first tick at least `n` nanoseconds after the request - rounded up, so
//! never early - and is reported only once the counter has passed it.
//!
//! The hardware timer that wakes the timer PD may run from a clock of its
//! own - the Pi 5's system timer, at 1 MHz beside a 54 MHz counter -
//! armed some ticks of its own ahead ([`alarm_ticks`]). Whichever fires, a
//! timeout expires only by the counter, so an alarm a little early only
//! makes the timer PD arm it again.

use crate::{MAX_PENDING, MAX_TIMEOUT_NANOS, Reply, Request, TimerError};

/// One timeout: the client's id for it, its deadline in counter ticks, and
/// whether the deadline has passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timeout {
    id: u64,
    deadline: u64,
    expired: bool,
}

/// The timeouts of `CLIENTS` clients (at most 64), numbered from 0.
#[derive(Debug, Clone)]
pub struct Timeouts<const CLIENTS: usize> {
    clients: [[Option<Timeout>; MAX_PENDING]; CLIENTS],
}

impl<const CLIENTS: usize> Default for Timeouts<CLIENTS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CLIENTS: usize> Timeouts<CLIENTS> {
    /// No timeouts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            clients: [[None; MAX_PENDING]; CLIENTS],
        }
    }

    /// Serves `request` from `client`, at counter reading `now`, with the
    /// counter at `frequency` ticks per second.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownClient`] if `client` is not below `CLIENTS`; for
    /// [`Request::SetTimeout`], [`TimerError::ZeroTimeout`],
    /// [`TimerError::TimeoutTooLong`] if it is longer than
    /// [`MAX_TIMEOUT_NANOS`] or the deadline does not fit,
    /// [`TimerError::AlreadyPending`] if the client has that id already, and
    /// [`TimerError::TooManyTimeouts`] if it has [`MAX_PENDING`]. Nothing
    /// changes when a request is refused.
    pub fn serve(
        &mut self,
        client: usize,
        request: Request,
        now: u64,
        frequency: u64,
    ) -> Result<Reply, TimerError> {
        let slots = self
            .clients
            .get_mut(client)
            .ok_or(TimerError::UnknownClient)?;
        match request {
            Request::Now => Ok(Reply::Time(crate::Instant::from_ticks(now, frequency))),
            Request::SetTimeout { id, nanos } => {
                if nanos == 0 {
                    return Err(TimerError::ZeroTimeout);
                }
                if nanos > MAX_TIMEOUT_NANOS {
                    return Err(TimerError::TimeoutTooLong { nanos });
                }
                let deadline = ticks_for(nanos, frequency)
                    .and_then(|ticks| now.checked_add(ticks))
                    .ok_or(TimerError::TimeoutTooLong { nanos })?;
                if slots.iter().flatten().any(|timeout| timeout.id == id) {
                    return Err(TimerError::AlreadyPending { id });
                }
                let free = slots
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(TimerError::TooManyTimeouts { limit: MAX_PENDING })?;
                *free = Some(Timeout {
                    id,
                    deadline,
                    expired: false,
                });
                Ok(Reply::Set)
            }
            Request::Cancel { id } => {
                let found = slots
                    .iter_mut()
                    .find(|slot| slot.is_some_and(|timeout| timeout.id == id));
                Ok(Reply::Cancelled(found.map(Option::take).is_some()))
            }
            Request::Expired => {
                let earliest = slots
                    .iter_mut()
                    .filter(|slot| slot.is_some_and(|timeout| timeout.expired))
                    .min_by_key(|slot| slot.map(|timeout| (timeout.deadline, timeout.id)));
                Ok(Reply::Expired(
                    earliest.and_then(Option::take).map(|timeout| timeout.id),
                ))
            }
        }
    }

    /// Marks every waiting timeout whose deadline is at or before `now` as
    /// expired, and returns the clients that have newly expired timeouts, as
    /// a bit mask (bit `n` for client `n`): each is notified once, however
    /// many of its deadlines passed.
    pub fn expire(&mut self, now: u64) -> u64 {
        let mut notify = 0;
        for (client, slots) in self.clients.iter_mut().enumerate() {
            for timeout in slots.iter_mut().flatten() {
                if !timeout.expired && timeout.deadline <= now {
                    timeout.expired = true;
                    notify |= 1_u64
                        .checked_shl(u32::try_from(client).unwrap_or(u32::MAX))
                        .unwrap_or(0);
                }
            }
        }
        notify
    }

    /// The earliest deadline still waiting, for the hardware timer; `None`
    /// if nothing is waiting.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.clients
            .iter()
            .flatten()
            .flatten()
            .filter(|timeout| !timeout.expired)
            .map(|timeout| timeout.deadline)
            .min()
    }
}

/// Counter ticks for `nanos` nanoseconds at `frequency` ticks per second,
/// rounded up so a timeout is never early. `None` if the frequency is 0 or
/// the result does not fit.
fn ticks_for(nanos: u64, frequency: u64) -> Option<u64> {
    if frequency == 0 {
        return None;
    }
    let ticks = u128::from(nanos)
        .checked_mul(u128::from(frequency))?
        .div_ceil(1_000_000_000);
    u64::try_from(ticks).ok()
}

/// Ticks of an alarm clock at `alarm_frequency` ticks per second from
/// counter reading `now` until `deadline`, counter ticks at `frequency`:
/// rounded up, so an alarm armed that many of its ticks ahead is not early
/// by the arithmetic. 0 if the deadline has passed; `u64::MAX` if the
/// frequency is 0 or the result does not fit.
#[must_use]
pub fn alarm_ticks(deadline: u64, now: u64, frequency: u64, alarm_frequency: u64) -> u64 {
    if frequency == 0 {
        return u64::MAX;
    }
    let ticks = u128::from(deadline.saturating_sub(now))
        .checked_mul(u128::from(alarm_frequency))
        .map(|scaled| scaled.div_ceil(u128::from(frequency)));
    ticks.map_or(u64::MAX, |ticks| u64::try_from(ticks).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]
mod tests {
    use super::*;
    use crate::Instant;

    /// 62.5 MHz, QEMU's counter: 16 ns a tick.
    const HZ: u64 = 62_500_000;
    const MS: u64 = 1_000_000;

    fn set(
        timeouts: &mut Timeouts<2>,
        client: usize,
        id: u64,
        nanos: u64,
        now: u64,
    ) -> Result<Reply, TimerError> {
        timeouts.serve(client, Request::SetTimeout { id, nanos }, now, HZ)
    }

    fn collect(timeouts: &mut Timeouts<2>, client: usize) -> Vec<u64> {
        let mut ids = Vec::new();
        while let Ok(Reply::Expired(Some(id))) = timeouts.serve(client, Request::Expired, 0, HZ) {
            ids.push(id);
        }
        ids
    }

    #[test]
    fn deadlines_are_rounded_up_never_early() {
        assert_eq!(ticks_for(16, HZ), Some(1));
        assert_eq!(ticks_for(17, HZ), Some(2));
        assert_eq!(ticks_for(1, HZ), Some(1));
        assert_eq!(ticks_for(MS, HZ), Some(62_500));
        assert_eq!(ticks_for(u64::MAX, u64::MAX), None);
        assert_eq!(ticks_for(5, 0), None);
    }

    #[test]
    fn an_alarm_on_another_clock_is_rounded_up_never_early() {
        /// The Pi 5's counter, 54 MHz, and its system timer, 1 MHz.
        const PI: u64 = 54_000_000;
        const SYSTEM_TIMER: u64 = 1_000_000;
        assert_eq!(alarm_ticks(54, 0, PI, SYSTEM_TIMER), 1);
        assert_eq!(alarm_ticks(55, 0, PI, SYSTEM_TIMER), 2);
        assert_eq!(alarm_ticks(1_054, 1_000, PI, SYSTEM_TIMER), 1);
        assert_eq!(alarm_ticks(10 * 54_000, 0, PI, SYSTEM_TIMER), 10_000);
        // Passed, or due now: no ticks to wait.
        assert_eq!(alarm_ticks(5, 9, PI, SYSTEM_TIMER), 0);
        assert_eq!(alarm_ticks(9, 9, PI, SYSTEM_TIMER), 0);
        // The same clock: the same ticks.
        assert_eq!(alarm_ticks(u64::MAX, 0, HZ, HZ), u64::MAX);
        assert_eq!(alarm_ticks(u64::MAX, 0, 1, u64::MAX), u64::MAX);
        assert_eq!(alarm_ticks(5, 0, 0, SYSTEM_TIMER), u64::MAX);
    }

    #[test]
    fn two_clients_expire_in_deadline_order_and_none_early() {
        let mut timeouts = Timeouts::<2>::new();
        let start = 1_000;
        for (client, id, ms) in [
            (0, 1, 30),
            (0, 2, 10),
            (0, 3, 50),
            (1, 1, 20),
            (1, 2, 40),
            (1, 3, 5),
        ] {
            assert_eq!(
                set(&mut timeouts, client, id, ms * MS, start),
                Ok(Reply::Set)
            );
        }
        let mut fired = Vec::new();
        while let Some(deadline) = timeouts.next_deadline() {
            assert_eq!(timeouts.expire(deadline - 1), 0, "nothing is early");
            let notify = timeouts.expire(deadline);
            for client in 0..2 {
                if notify & (1 << client) != 0 {
                    for id in collect(&mut timeouts, client) {
                        fired.push((
                            client,
                            id,
                            Instant::from_ticks(deadline - start, HZ).as_nanos() / MS,
                        ));
                    }
                }
            }
        }
        assert_eq!(
            fired,
            [
                (1, 3, 5),
                (0, 2, 10),
                (1, 1, 20),
                (0, 1, 30),
                (1, 2, 40),
                (0, 3, 50)
            ]
        );
    }

    #[test]
    fn deadlines_that_pass_together_are_reported_in_order_with_one_notification() {
        let mut timeouts = Timeouts::<2>::new();
        for (id, ms) in [(1, 30), (2, 10), (3, 20)] {
            set(&mut timeouts, 0, id, ms * MS, 0).unwrap();
        }
        assert_eq!(timeouts.expire(u64::MAX), 0b01);
        assert_eq!(timeouts.expire(u64::MAX), 0, "notified once");
        assert_eq!(collect(&mut timeouts, 0), [2, 3, 1]);
        assert_eq!(timeouts.next_deadline(), None);
    }

    #[test]
    fn a_cancelled_timeout_never_expires() {
        let mut timeouts = Timeouts::<2>::new();
        set(&mut timeouts, 0, 1, 10 * MS, 0).unwrap();
        set(&mut timeouts, 0, 2, 20 * MS, 0).unwrap();
        assert_eq!(
            timeouts.serve(0, Request::Cancel { id: 1 }, 0, HZ),
            Ok(Reply::Cancelled(true))
        );
        assert_eq!(
            timeouts.serve(0, Request::Cancel { id: 1 }, 0, HZ),
            Ok(Reply::Cancelled(false))
        );
        assert_eq!(timeouts.next_deadline(), Some(20 * 62_500));
        timeouts.expire(u64::MAX);
        assert_eq!(collect(&mut timeouts, 0), [2]);
        set(&mut timeouts, 0, 3, MS, 0).unwrap();
        timeouts.expire(u64::MAX);
        assert_eq!(
            timeouts.serve(0, Request::Cancel { id: 3 }, 0, HZ),
            Ok(Reply::Cancelled(true))
        );
        assert_eq!(
            collect(&mut timeouts, 0),
            Vec::<u64>::new(),
            "expired but cancelled"
        );
    }

    #[test]
    fn bad_timeouts_are_refused_and_change_nothing() {
        let mut timeouts = Timeouts::<2>::new();
        assert_eq!(set(&mut timeouts, 0, 1, 0, 0), Err(TimerError::ZeroTimeout));
        assert_eq!(
            set(&mut timeouts, 0, 1, u64::MAX, 0),
            Err(TimerError::TimeoutTooLong { nanos: u64::MAX })
        );
        assert_eq!(
            set(&mut timeouts, 0, 1, MAX_TIMEOUT_NANOS + 1, 0),
            Err(TimerError::TimeoutTooLong {
                nanos: MAX_TIMEOUT_NANOS + 1
            })
        );
        assert_eq!(
            set(&mut timeouts, 0, 1, MS, u64::MAX - 10),
            Err(TimerError::TimeoutTooLong { nanos: MS })
        );
        assert_eq!(
            set(&mut timeouts, 1, 7, MAX_TIMEOUT_NANOS, 0),
            Ok(Reply::Set)
        );
        assert_eq!(
            timeouts.serve(1, Request::Cancel { id: 7 }, 0, HZ),
            Ok(Reply::Cancelled(true))
        );
        for id in 0..MAX_PENDING as u64 {
            set(&mut timeouts, 0, id, MS, 0).unwrap();
        }
        assert_eq!(
            set(&mut timeouts, 0, 99, MS, 0),
            Err(TimerError::TooManyTimeouts { limit: MAX_PENDING })
        );
        assert_eq!(
            set(&mut timeouts, 0, 0, MS, 0),
            Err(TimerError::AlreadyPending { id: 0 })
        );
        assert_eq!(
            timeouts.serve(2, Request::Now, 0, HZ),
            Err(TimerError::UnknownClient)
        );
        assert_eq!(
            set(&mut timeouts, 1, 99, MS, 0),
            Ok(Reply::Set),
            "the other client is still served"
        );
        timeouts.expire(u64::MAX);
        assert_eq!(collect(&mut timeouts, 0), [0, 1, 2, 3]);
    }
}
