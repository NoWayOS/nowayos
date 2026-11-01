// SPDX-License-Identifier: MIT OR Apache-2.0
//! A stream's capture, as its client asks: the USB host PD streams a
//! microphone or a camera only while its client last
//! asked it to, so nothing is taken in while no call is listening or
//! watching.
//!
//! [`Gate`] says, for one stream, which control request to make next and
//! whether the PD may queue its transfers ([`Gate::feeding`]):
//!
//! * enumeration leaves the stream's interface at its streaming alternate
//!   setting, so the gate first puts it back at alternate setting 0, the
//!   one with no bandwidth (USB 2.0, section 9.6.5);
//! * asked to capture, it makes a camera's commit again (UVC 1.5, section
//!   4.3.1.1: the commit comes before the setting is selected), selects the
//!   streaming setting, sets a microphone's rate where its stream takes one
//!   (Audio 1.0, section 5.2.3.2.3.1), and then lets the PD queue
//!   transfers;
//! * asked to stop, it stops the PD queuing; once the last transfer has
//!   come back ([`Gate::drained`]), it puts the interface back at 0.
//!
//! A request in flight is never followed by another before it completes:
//! what the client asks meanwhile is kept and acted on then. A request that
//! fails leaves the gate [`State::Failed`], capturing nothing - except a rate
//! the stream's format takes alone, which it runs at whatever the request
//! did, as enumeration does.

use crate::audio::Stream;
use crate::request::{OutData, Setup};
use crate::video::camera::Choice;

/// Where a gate is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not capturing: the interface at alternate setting 0, nothing queued.
    Off,
    /// A camera's commit made.
    Committing,
    /// SET_INTERFACE to the streaming setting made.
    Selecting,
    /// A microphone's rate's SET_CUR made.
    SettingRate,
    /// Capturing: the PD queues the stream's transfers.
    On,
    /// Asked to stop: no more transfers queued; the queued ones coming back.
    Draining,
    /// SET_INTERFACE to alternate setting 0 made.
    Deselecting,
    /// A request failed: nothing is captured.
    Failed,
}

/// The control request a gate asks the PD to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A request with no Data Stage.
    Control(Setup),
    /// A request with its Data Stage.
    ControlOut(Setup, OutData),
}

/// What a gate selects, and the requests on either side of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    interface: u8,
    alternate: u8,
    /// Made before the setting is selected: a camera's commit.
    before: Option<Ask>,
    /// Made after: a microphone's rate, and whether its stream runs
    /// whatever the request did (a format with one rate).
    after: Option<(Ask, bool)>,
}

/// One stream's capture gate (see the module's documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    plan: Plan,
    state: State,
    wanted: bool,
}

impl Gate {
    /// The gate for `stream`, a microphone enumeration selected and set up,
    /// and its first request: the interface back at alternate setting 0.
    #[must_use]
    pub fn microphone(stream: Stream) -> (Self, Ask) {
        let after = stream.sets_rate().then(|| {
            let (setup, data) = Setup::set_sampling_frequency(stream.endpoint, stream.rate);
            let alone = stream.format.is_some_and(|f| !f.rates.several());
            (Ask::ControlOut(setup, data), alone)
        });
        Self::begin(Plan {
            interface: stream.interface,
            alternate: stream.alternate,
            before: None,
            after,
        })
    }

    /// The gate for a camera's video, `choice`, enumeration committed with
    /// `commit` and selected, and its first request: the interface back at
    /// alternate setting 0.
    #[must_use]
    pub const fn camera(choice: &Choice, commit: (Setup, OutData)) -> (Self, Ask) {
        Self::begin(Plan {
            interface: choice.interface,
            alternate: choice.alternate.alternate,
            before: Some(Ask::ControlOut(commit.0, commit.1)),
            after: None,
        })
    }

    const fn begin(plan: Plan) -> (Self, Ask) {
        let gate = Self {
            plan,
            state: State::Deselecting,
            wanted: false,
        };
        (gate, Ask::Control(Setup::set_interface(plan.interface, 0)))
    }

    /// Where it is.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Whether the client last asked to capture.
    #[must_use]
    pub const fn wanted(&self) -> bool {
        self.wanted
    }

    /// Whether the PD may queue the stream's transfers.
    #[must_use]
    pub fn feeding(&self) -> bool {
        self.state == State::On
    }

    /// The client asks to capture (`on`) or to stop: the request to make
    /// now, if any.
    pub fn want(&mut self, on: bool) -> Option<Ask> {
        self.wanted = on;
        match (self.state, on) {
            (State::Off, true) => Some(self.start()),
            (State::On, false) => {
                self.state = State::Draining;
                None
            }
            (State::Draining, true) => {
                self.state = State::On;
                None
            }
            // A request in flight is followed when it completes; the rest
            // are where the client asks.
            _ => None,
        }
    }

    /// The request made completed, `ok` or not: the next to make, if any.
    pub fn done(&mut self, ok: bool) -> Option<Ask> {
        match (self.state, ok) {
            (State::Committing, true) => {
                if self.wanted {
                    Some(self.select())
                } else {
                    // Nothing selected yet: still at alternate setting 0.
                    self.state = State::Off;
                    None
                }
            }
            (State::Selecting, true) => match self.plan.after {
                // No longer wanted: no rate to set for nothing.
                Some((ask, _)) if self.wanted => {
                    self.state = State::SettingRate;
                    Some(ask)
                }
                _ => self.started(),
            },
            (State::SettingRate, true) => self.started(),
            // A format with one rate runs at it whatever the request did.
            (State::SettingRate, false) if self.plan.after.is_some_and(|(_, alone)| alone) => {
                self.started()
            }
            (State::Deselecting, true) => {
                self.state = State::Off;
                if self.wanted {
                    Some(self.start())
                } else {
                    None
                }
            }
            (
                State::Committing | State::Selecting | State::SettingRate | State::Deselecting,
                false,
            ) => {
                self.state = State::Failed;
                None
            }
            // No request in flight: nothing to follow.
            _ => None,
        }
    }

    /// While draining, the stream's last queued transfer came back: the
    /// interface goes back to alternate setting 0.
    pub fn drained(&mut self) -> Option<Ask> {
        if self.state != State::Draining {
            return None;
        }
        self.state = State::Deselecting;
        Some(Ask::Control(Setup::set_interface(self.plan.interface, 0)))
    }

    /// The first request of a start: a camera's commit, or the selection.
    fn start(&mut self) -> Ask {
        match self.plan.before {
            Some(ask) => {
                self.state = State::Committing;
                ask
            }
            None => self.select(),
        }
    }

    /// SET_INTERFACE to the streaming setting.
    fn select(&mut self) -> Ask {
        self.state = State::Selecting;
        Ask::Control(Setup::set_interface(
            self.plan.interface,
            self.plan.alternate,
        ))
    }

    /// The setting selected and its rate set: capture, or, if the client
    /// has asked to stop meanwhile, drain at once.
    fn started(&mut self) -> Option<Ask> {
        self.state = if self.wanted {
            State::On
        } else {
            State::Draining
        };
        None
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code: a failed check panics"
)]
mod tests {
    use super::*;
    use crate::audio::{Pcm, Rates};
    use crate::video::camera::Alternate;
    use crate::video::probe::{self, COMMIT};
    use proptest::prelude::*;

    /// A webcam's microphone: interface 3, streaming at alternate setting 2,
    /// endpoint 0x84, 48 kHz from a range - so a rate is set - or 48 kHz
    /// alone.
    fn microphone(several: bool) -> Stream {
        let rates = if several {
            Rates::Range {
                low: 16_000,
                high: 48_000,
            }
        } else {
            Rates::Range {
                low: 48_000,
                high: 48_000,
            }
        };
        Stream {
            interface: 3,
            alternate: 2,
            endpoint: 0x84,
            max_packet: 96,
            transactions: 1,
            interval: 1,
            sync: 1,
            format: Some(Pcm {
                channels: 1,
                subframe: 2,
                bits: 16,
                rates,
            }),
            max_packets_only: false,
            rate: 48_000,
            rate_control: false,
            feedback: None,
            refusal: None,
        }
    }

    /// A webcam's camera: interface 1, streaming 1280 x 720 at alternate
    /// setting 6, committed with a UVC 1.1 commit of 34 bytes.
    fn camera() -> (Choice, (Setup, OutData)) {
        let choice = Choice {
            interface: 1,
            endpoint: 0x81,
            format: 2,
            frame: 1,
            width: 1280,
            height: 720,
            interval: 333_333,
            alternate: Alternate {
                alternate: 6,
                max_packet: 1024,
                transactions: 3,
                interval: 1,
            },
            probe_len: 34,
        };
        let setup = probe::set(COMMIT, 1, 34);
        let data = OutData::new(&[0x5A; 34]).unwrap();
        (choice, (setup, data))
    }

    fn select(alternate: u8) -> Option<Ask> {
        Some(Ask::Control(Setup::set_interface(3, alternate)))
    }

    fn select_video(alternate: u8) -> Option<Ask> {
        Some(Ask::Control(Setup::set_interface(1, alternate)))
    }

    #[test]
    fn it_starts_at_alternate_setting_0_and_captures_nothing_until_asked() {
        let (mut gate, first) = Gate::microphone(microphone(true));
        assert_eq!(Some(first), select(0));
        assert!(!gate.feeding());
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Off);
        assert!(!gate.feeding());
    }

    #[test]
    fn asked_it_selects_the_setting_sets_the_rate_then_captures() {
        let (mut gate, _) = Gate::microphone(microphone(true));
        gate.done(true);
        assert_eq!(gate.want(true), select(2));
        assert!(!gate.feeding(), "nothing queued before the setting is in");
        let (setup, data) = Setup::set_sampling_frequency(0x84, 48_000);
        assert_eq!(gate.done(true), Some(Ask::ControlOut(setup, data)));
        assert!(!gate.feeding());
        assert_eq!(gate.done(true), None);
        assert!(gate.feeding());
    }

    #[test]
    fn stopped_it_drains_then_goes_back_to_alternate_setting_0() {
        let (mut gate, _) = Gate::microphone(microphone(false));
        gate.done(true);
        gate.want(true);
        assert_eq!(gate.done(true), None, "one rate alone: none set");
        assert!(gate.feeding());
        assert_eq!(gate.want(false), None);
        assert!(!gate.feeding(), "no more transfers queued");
        assert_eq!(gate.drained(), select(0));
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Off);
    }

    #[test]
    fn what_the_client_asks_meanwhile_waits_for_the_request_in_flight() {
        let (mut gate, _) = Gate::microphone(microphone(false));
        // Asked while the first request is in flight: made once it is done.
        assert_eq!(gate.want(true), None);
        assert_eq!(gate.done(true), select(2));
        // Stopped while selecting: drained at once, never fed.
        assert_eq!(gate.want(false), None);
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Draining);
        assert!(!gate.feeding());
    }

    #[test]
    fn a_failed_request_captures_nothing_but_a_rate_taken_alone_runs() {
        let (mut gate, _) = Gate::microphone(microphone(true));
        gate.done(true);
        gate.want(true);
        gate.done(true);
        assert_eq!(gate.done(false), None);
        assert_eq!(gate.state(), State::Failed);
        assert!(!gate.feeding());
        assert_eq!(gate.want(true), None, "a failed gate asks nothing");

        let mut one_rate = microphone(false);
        one_rate.rate_control = true;
        let (mut gate, _) = Gate::microphone(one_rate);
        gate.done(true);
        gate.want(true);
        gate.done(true);
        assert_eq!(gate.done(false), None);
        assert!(gate.feeding());
    }

    #[test]
    fn a_camera_commits_again_then_selects_its_setting_and_stops_as_a_microphone() {
        let (choice, commit) = camera();
        let (mut gate, first) = Gate::camera(&choice, commit);
        assert_eq!(Some(first), select_video(0));
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Off);
        assert_eq!(
            gate.want(true),
            Some(Ask::ControlOut(commit.0, commit.1)),
            "the commit first"
        );
        assert!(!gate.feeding());
        assert_eq!(gate.done(true), select_video(6));
        assert!(!gate.feeding());
        assert_eq!(gate.done(true), None, "no rate for a camera");
        assert!(gate.feeding());
        assert_eq!(gate.want(false), None);
        assert_eq!(gate.drained(), select_video(0));
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Off);
        // Started again: the commit again.
        assert_eq!(gate.want(true), Some(Ask::ControlOut(commit.0, commit.1)));
    }

    #[test]
    fn a_camera_stopped_while_committing_selects_nothing() {
        let (choice, commit) = camera();
        let (mut gate, _) = Gate::camera(&choice, commit);
        gate.done(true);
        gate.want(true);
        assert_eq!(gate.want(false), None);
        assert_eq!(gate.done(true), None);
        assert_eq!(gate.state(), State::Off, "still at alternate setting 0");
        // A commit refused captures nothing.
        gate.want(true);
        assert_eq!(gate.done(false), None);
        assert_eq!(gate.state(), State::Failed);
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Want(bool),
        Done(bool),
        Drained,
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            any::<bool>().prop_map(Event::Want),
            any::<bool>().prop_map(Event::Done),
            Just(Event::Drained),
        ]
    }

    proptest! {
        /// For a microphone or a camera, whatever the client asks and
        /// however requests end: a request is asked only when none is in
        /// flight; it feeds only while the client last asked to capture;
        /// and a stop the client asks, followed by the drain and the
        /// request coming back, leaves the interface at alternate setting
        /// 0 with nothing fed.
        #[test]
        fn any_sequence_keeps_the_gate_honest(
            kind in 0u8..3,
            events in prop::collection::vec(event(), 0..40),
        ) {
            let (choice, commit) = camera();
            let (mut gate, first) = match kind {
                0 => Gate::microphone(microphone(false)),
                1 => Gate::microphone(microphone(true)),
                _ => Gate::camera(&choice, commit),
            };
            let back = Some(first);
            let mut in_flight = true;
            for event in events {
                let ask = match event {
                    Event::Want(on) => gate.want(on),
                    Event::Done(ok) => {
                        if !in_flight {
                            continue;
                        }
                        in_flight = false;
                        gate.done(ok)
                    }
                    Event::Drained => gate.drained(),
                };
                if ask.is_some() {
                    prop_assert!(!in_flight, "a request asked while one was in flight");
                    in_flight = true;
                }
                prop_assert!(!gate.feeding() || gate.wanted());
                prop_assert_eq!(
                    in_flight,
                    matches!(
                        gate.state(),
                        State::Committing | State::Selecting | State::SettingRate | State::Deselecting
                    )
                );
            }
            gate.want(false);
            if in_flight {
                let ask = gate.done(true);
                prop_assert!(ask.is_none());
            }
            if let Some(ask) = gate.drained() {
                prop_assert_eq!(Some(ask), back);
                prop_assert_eq!(gate.done(true), None);
            }
            prop_assert!(matches!(gate.state(), State::Off | State::Failed));
            prop_assert!(!gate.feeding());
        }
    }
}
