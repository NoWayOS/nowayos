// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-heartbeat`: the watchdog PD's heartbeat rounds, with no seL4, so the host tests them.
//!
//! The watchdog PD keeps the board's hardware watchdog from firing only
//! while every PD it watches - the supervisor included - answers its
//! heartbeat in time. It works in **rounds**, each ended by the hardware's
//! fixed-period tick (in QEMU, a test-only stand-in's notification):
//!
//! * a round begins when the watchdog pings every watched PD, each on a
//!   notification-only channel of its own, and each PD answers on it from
//!   its event loop;
//! * [`Rounds::answer`] records an answer for the round in progress only:
//!   a PD answers once a round however often it notifies, and nothing it
//!   sent before the round began counts, since the record is cleared when
//!   the round begins;
//! * [`Rounds::tick`] ends the round. If every PD answered, the watchdog
//!   services the hardware and the next round begins
//!   ([`Verdict::Serviced`]); if not, it names **every** PD that missed,
//!   highest priority first, and **never services again**
//!   ([`Verdict::Missed`]), so the hardware resets the board. A looping PD
//!   starves every PD below it, so the highest-priority PD that missed is
//!   the likely cause.
//!
//! The watched PDs are numbered from 0, highest priority first; the
//! supervisor, which runs above every other PD, is 0 in a product. A caller
//! that takes its notifications in batches applies a batch's answers before
//! its tick: they arrived before the watchdog ran, so within the round.
//!
//! A heartbeat shows that a PD's event loop runs, not that it serves its
//! peers.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// The most PDs one watchdog watches: every PD of a product, the
/// supervisor included.
pub const MAX_WATCHED: usize = 32;

/// The watchdog's rounds; see the [crate documentation](crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rounds {
    /// How many PDs are watched.
    count: usize,
    /// Which have answered in the round in progress, a bit each.
    answered: u32,
    /// The round in progress, from 1.
    round: u32,
    /// The round some PD missed, after which servicing stopped for good.
    missed_at: Option<u32>,
}

/// What ending a round asks of the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every watched PD answered in `round`: service the hardware watchdog
    /// and ping every PD, beginning the next round.
    Serviced {
        /// The round that ended.
        round: u32,
    },
    /// The PDs in `missed` did not answer in `round`: stop servicing the
    /// hardware watchdog, for good.
    Missed {
        /// The round that ended.
        round: u32,
        /// Every PD that missed it.
        missed: Missed,
    },
    /// Servicing stopped at an earlier round: nothing to do.
    Stopped,
}

/// The PDs that missed a round, highest priority first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missed {
    bits: u32,
}

impl Missed {
    /// The PDs that missed, by number, highest priority first.
    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..MAX_WATCHED).filter(move |&pd| bit(pd).is_some_and(|bit| self.bits & bit != 0))
    }

    /// How many PDs missed.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.bits.count_ones()
    }

    /// Whether none did (never so in a [`Verdict::Missed`]).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

/// PD `pd`'s bit, if it is a number a watchdog can watch.
fn bit(pd: usize) -> Option<u32> {
    if pd < MAX_WATCHED {
        1_u32.checked_shl(u32::try_from(pd).ok()?)
    } else {
        None
    }
}

impl Rounds {
    /// Rounds over `count` watched PDs, numbered from 0 highest priority
    /// first, with round 1 begun: the watchdog pings every PD as it starts.
    /// `None` for no PDs, or more than [`MAX_WATCHED`].
    #[must_use]
    pub const fn new(count: usize) -> Option<Self> {
        if count == 0 || count > MAX_WATCHED {
            return None;
        }
        Some(Self {
            count,
            answered: 0,
            round: 1,
            missed_at: None,
        })
    }

    /// How many PDs are watched.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The round in progress, from 1.
    #[must_use]
    pub const fn round(&self) -> u32 {
        self.round
    }

    /// Whether servicing has stopped for good: some PD missed a round.
    #[must_use]
    pub const fn stopped(&self) -> bool {
        self.missed_at.is_some()
    }

    /// PD `pd` answered: counted once in the round in progress. A number
    /// that is not watched, or an answer after servicing stopped, changes
    /// nothing.
    pub fn answer(&mut self, pd: usize) {
        if self.missed_at.is_some() || pd >= self.count {
            return;
        }
        if let Some(bit) = bit(pd) {
            self.answered |= bit;
        }
    }

    /// The hardware's tick: the round in progress ends. Every PD answered:
    /// [`Verdict::Serviced`], and the next round begins with no answer
    /// counted. Some did not: [`Verdict::Missed`] names each, and servicing
    /// stops for good - every later tick is [`Verdict::Stopped`].
    pub fn tick(&mut self) -> Verdict {
        if self.missed_at.is_some() {
            return Verdict::Stopped;
        }
        let round = self.round;
        let all = all(self.count);
        let missed = all & !self.answered;
        if missed == 0 {
            self.answered = 0;
            self.round = round.saturating_add(1);
            Verdict::Serviced { round }
        } else {
            self.missed_at = Some(round);
            Verdict::Missed {
                round,
                missed: Missed { bits: missed },
            }
        }
    }
}

/// The bits of PDs 0 to `count - 1`.
fn all(count: usize) -> u32 {
    (0..count).filter_map(bit).fold(0, |all, bit| all | bit)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_round_completes_when_every_pd_answered() {
        let mut rounds = Rounds::new(3).unwrap();
        rounds.answer(2);
        rounds.answer(0);
        rounds.answer(1);
        assert_eq!(rounds.tick(), Verdict::Serviced { round: 1 });
        assert_eq!(rounds.round(), 2);
        // Round 2 begins with nothing counted: last round's answers do not
        // carry over.
        rounds.answer(0);
        rounds.answer(0);
        let Verdict::Missed { round, missed } = rounds.tick() else {
            panic!("round 2 should be missed");
        };
        assert_eq!(round, 2);
        assert_eq!(missed.iter().collect::<Vec<_>>(), [1, 2]);
        assert!(rounds.stopped());
        // Never again: later answers and ticks change nothing.
        for pd in 0..3 {
            rounds.answer(pd);
        }
        assert_eq!(rounds.tick(), Verdict::Stopped);
    }

    #[test]
    fn a_number_not_watched_counts_for_nothing() {
        assert_eq!(Rounds::new(0), None);
        assert_eq!(Rounds::new(MAX_WATCHED + 1), None);
        let mut rounds = Rounds::new(MAX_WATCHED).unwrap();
        for pd in 0..MAX_WATCHED {
            rounds.answer(pd);
        }
        rounds.answer(MAX_WATCHED);
        rounds.answer(usize::MAX);
        assert_eq!(rounds.tick(), Verdict::Serviced { round: 1 });
        let mut two = Rounds::new(2).unwrap();
        two.answer(0);
        two.answer(5);
        let Verdict::Missed { missed, .. } = two.tick() else {
            panic!("PD 1 never answered");
        };
        assert_eq!(missed.iter().collect::<Vec<_>>(), [1]);
    }

    /// One notification the watchdog takes: a PD's answer, or the tick.
    #[derive(Debug, Clone, Copy)]
    enum Event {
        Answer(usize),
        Tick,
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            4 => (0..40_usize).prop_map(Event::Answer),
            1 => Just(Event::Tick),
        ]
    }

    proptest! {
        // Against a plain model: a round completes only when every watched
        // PD answered after it began; an answer never counts for another
        // round; the PDs that missed are exactly those that did not
        // answer, highest priority first; after a miss, nothing is ever
        // serviced again; and no order of notifications panics.
        #[test]
        fn rounds_agree_with_the_model(
            count in 1..=MAX_WATCHED,
            events in proptest::collection::vec(event(), 0..400),
        ) {
            let mut rounds = Rounds::new(count).unwrap();
            let mut answered = vec![false; count];
            let mut round = 1_u32;
            let mut stopped = false;
            for event in events {
                match event {
                    Event::Answer(pd) => {
                        rounds.answer(pd);
                        if !stopped && pd < count {
                            answered[pd] = true;
                        }
                    }
                    Event::Tick => {
                        let verdict = rounds.tick();
                        if stopped {
                            prop_assert_eq!(verdict, Verdict::Stopped);
                            continue;
                        }
                        let expected: Vec<usize> =
                            (0..count).filter(|&pd| !answered[pd]).collect();
                        if expected.is_empty() {
                            prop_assert_eq!(verdict, Verdict::Serviced { round });
                            round += 1;
                            answered = vec![false; count];
                        } else {
                            let Verdict::Missed { round: at, missed } = verdict else {
                                return Err(TestCaseError::fail("a PD missed, unnamed"));
                            };
                            prop_assert_eq!(at, round);
                            prop_assert_eq!(missed.iter().collect::<Vec<_>>(), expected.clone());
                            prop_assert_eq!(usize::try_from(missed.len()).unwrap(), expected.len());
                            stopped = true;
                        }
                    }
                }
                prop_assert_eq!(rounds.round(), round);
                prop_assert_eq!(rounds.stopped(), stopped);
            }
        }
    }
}
