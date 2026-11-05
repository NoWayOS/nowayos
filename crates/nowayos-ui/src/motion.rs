// SPDX-License-Identifier: MIT OR Apache-2.0
//! Motion (#10086): transitions and animations the runtime runs on its
//! clock - [`Tree::advance`], which the UI PD calls at the display's rate
//! while anything moves. There is no script on the device.
//!
//! An [`Animation`] moves one [`Property`] of a node - its opacity, where
//! it is drawn, its scale, its fill's or its text's colour, how much of its
//! height shows, an icon's turn, a progress bar's sweep or a shimmer across
//! it - through keyframes, with a duration, a delay and an [`Easing`], once,
//! a number of times or until stopped, alternating if asked. What a node is
//! drawn as is its [`Visual`]; its layout never moves, so input acts on the
//! end state at once and is never delayed.
//!
//! Screens, dialogs and menus have default transitions; a product turns
//! them off, or asks for reduced motion (WCAG 2.3.3), with
//! [`Tree::set_motion`]: then movement is an instant change and a fade at
//! most [`REDUCED_FADE`] long.

use nowayos_bounded::BoundedVec;
use nowayos_display::Rect;

use crate::{Colour, NodeId, Tree, UiError};

/// Animations running at once, at most.
pub const MAX_RUNNING: usize = 16;

/// Keyframes an animation holds, at most.
pub const MAX_KEYS: usize = 6;

/// The longest fade under reduced motion, in milliseconds.
pub const REDUCED_FADE: u32 = 100;

/// How a transition's progress follows time: CSS's timing functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Easing {
    /// At an even pace.
    Linear,
    /// CSS's `ease`: quick, then slowing.
    #[default]
    Ease,
    /// Starting slowly.
    EaseIn,
    /// Ending slowly.
    EaseOut,
    /// Starting and ending slowly.
    EaseInOut,
    /// CSS's `cubic-bezier(x1, y1, x2, y2)`, in thousandths; the x's from
    /// 0 to 1000.
    Bezier([i16; 4]),
    /// CSS's `steps(count, jump-start | jump-end)`: `count` jumps.
    Steps {
        /// How many.
        count: u8,
        /// Whether the first jump is at the start.
        start: bool,
    },
}

impl Easing {
    /// Progress at time `t`, both in thousandths of the way: 0 at 0 and
    /// 1000 at 1000 for every curve but a Bézier whose y's leave 0 to 1.
    #[must_use]
    pub fn at(self, t: u16) -> i32 {
        let t = t.min(1000);
        let curve = match self {
            Self::Linear => return i32::from(t),
            Self::Steps { count, start } => {
                let count = u32::from(count.max(1));
                let jumps =
                    (u32::from(t).saturating_mul(count) / 1000).saturating_add(u32::from(start));
                let jumps = jumps.min(count);
                return i32::try_from(jumps.saturating_mul(1000).checked_div(count).unwrap_or(0))
                    .unwrap_or(1000);
            }
            Self::Ease => [250, 100, 250, 1000],
            Self::EaseIn => [420, 0, 1000, 1000],
            Self::EaseOut => [0, 0, 580, 1000],
            Self::EaseInOut => [420, 0, 580, 1000],
            Self::Bezier(points) => points,
        };
        bezier(curve, f32::from(t) / 1000.0)
    }
}

/// The cubic Bézier through (0, 0), the two control points and (1, 1), at
/// the `x` given: its parameter found by halving, to well under a
/// thousandth, then its y, in thousandths.
fn bezier([x1, y1, x2, y2]: [i16; 4], x: f32) -> i32 {
    let (x1, y1, x2, y2) = (
        f32::from(x1) / 1000.0,
        f32::from(y1) / 1000.0,
        f32::from(x2) / 1000.0,
        f32::from(y2) / 1000.0,
    );
    let along = |a: f32, b: f32, u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if along(x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0), mid) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    let y = along(y1, y2, (low + high) / 2.0) * 1000.0;
    // Exact enough: a curve's y stays within a few times 0 to 1.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "y is clamped to a few thousand first"
    )]
    let y = y.clamp(-4000.0, 5000.0);
    // Rounded to the nearest, half away from zero (no `round` without std).
    (if y >= 0.0 { y + 0.5 } else { y - 0.5 }) as i32
}

/// What an animation moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Property {
    /// How opaque it and its children are: 0 to 255.
    Opacity,
    /// How far right it is drawn, in layout units.
    TranslateX,
    /// How far down it is drawn, in layout units.
    TranslateY,
    /// Its scale about its centre, in thousandths: 500 to 2000. Drawn for
    /// a node on a surface or a fill of its own.
    Scale,
    /// Its fill's colour, from one of the theme's to another: 0 to 1000 of
    /// the way.
    Fill(Colour, Colour),
    /// Its text's colour, likewise.
    TextColour(Colour, Colour),
    /// How much of its height shows, from its top: 0 to 1000.
    Reveal,
    /// An icon's turn, in tenths of a degree.
    Turn,
    /// A progress bar's moving segment, indeterminate: 0 to 1000 along.
    Sweep,
    /// A shimmer's band across it: 0 to 1000 along.
    Shimmer,
}

impl Property {
    /// Whether it moves something: what reduced motion makes instant.
    #[must_use]
    pub const fn moves(self) -> bool {
        !matches!(
            self,
            Self::Opacity | Self::Fill(..) | Self::TextColour(..) | Self::Reveal
        )
    }
}

/// How often an animation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Repeat {
    /// This many times.
    Times(u16),
    /// Until stopped.
    Forever,
}

/// One animation of one property: keyframes - offsets in thousandths of
/// the way, each with its value - a duration and delay in milliseconds,
/// an easing between keys, how often, whether it alternates, and whether
/// its last value stays once it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Animation {
    /// What it moves.
    pub property: Property,
    keys: [(u16, i32); MAX_KEYS],
    count: u8,
    /// Its length, in milliseconds.
    pub duration: u32,
    /// How long it waits first, in milliseconds.
    pub delay: u32,
    /// How each key's way to the next is paced.
    pub easing: Easing,
    /// How often it runs.
    pub repeat: Repeat,
    /// Whether every second run goes backwards.
    pub alternate: bool,
    /// Whether its last value stays once it ends; else the node returns to
    /// how it is drawn at rest.
    pub keep: bool,
}

impl Animation {
    /// From `from` to `to` over `duration` milliseconds, easing as CSS's
    /// `ease`, once, its last value kept.
    #[must_use]
    pub const fn new(property: Property, from: i32, to: i32, duration: u32) -> Self {
        Self {
            property,
            keys: [
                (0, from),
                (1000, to),
                (1000, to),
                (1000, to),
                (1000, to),
                (1000, to),
            ],
            count: 2,
            duration,
            delay: 0,
            easing: Easing::Ease,
            repeat: Repeat::Times(1),
            alternate: false,
            keep: true,
        }
    }

    /// Through `keys` - each an offset from 0 to 1000 and a value, in
    /// order, at most [`MAX_KEYS`] - over `duration` milliseconds.
    #[must_use]
    pub fn keyframes(property: Property, keys: &[(u16, i32)], duration: u32) -> Self {
        let mut animation = Self::new(property, 0, 0, duration);
        let mut count = 0_u8;
        for (slot, key) in animation.keys.iter_mut().zip(keys) {
            *slot = (key.0.min(1000), key.1);
            count = count.saturating_add(1);
        }
        animation.count = count.max(1);
        animation
    }

    /// After `delay` milliseconds.
    #[must_use]
    pub const fn delay(self, delay: u32) -> Self {
        Self { delay, ..self }
    }

    /// Paced by `easing`.
    #[must_use]
    pub const fn easing(self, easing: Easing) -> Self {
        Self { easing, ..self }
    }

    /// As often as `repeat` says.
    #[must_use]
    pub const fn repeat(self, repeat: Repeat) -> Self {
        Self { repeat, ..self }
    }

    /// Every second run backwards.
    #[must_use]
    pub const fn alternate(self) -> Self {
        Self {
            alternate: true,
            ..self
        }
    }

    /// Back to rest when it ends.
    #[must_use]
    pub const fn release(self) -> Self {
        Self {
            keep: false,
            ..self
        }
    }

    /// A turn, over and over: an icon spinning.
    #[must_use]
    pub const fn spin() -> Self {
        Self::new(Property::Turn, 0, 3600, 1000)
            .easing(Easing::Linear)
            .repeat(Repeat::Forever)
    }

    /// Growing a little and back, over and over.
    #[must_use]
    pub fn pulse() -> Self {
        Self::keyframes(
            Property::Scale,
            &[(0, 1000), (500, 1060), (1000, 1000)],
            1200,
        )
        .easing(Easing::EaseInOut)
        .repeat(Repeat::Forever)
    }

    /// Fading in.
    #[must_use]
    pub const fn fade_in(duration: u32) -> Self {
        Self::new(Property::Opacity, 0, 255, duration).easing(Easing::EaseOut)
    }

    /// A progress bar's segment sweeping along, over and over.
    #[must_use]
    pub const fn indeterminate() -> Self {
        Self::new(Property::Sweep, 0, 1000, 1400)
            .easing(Easing::EaseInOut)
            .repeat(Repeat::Forever)
    }

    /// A light band crossing a placeholder, over and over: loading.
    #[must_use]
    pub const fn shimmer() -> Self {
        Self::new(Property::Shimmer, 0, 1000, 1500)
            .easing(Easing::Linear)
            .repeat(Repeat::Forever)
    }

    /// Its keyframes.
    #[must_use]
    pub fn keys(&self) -> &[(u16, i32)] {
        self.keys.get(..usize::from(self.count)).unwrap_or(&[])
    }

    /// Its value `elapsed` milliseconds after it started, and whether it
    /// has ended.
    #[must_use]
    pub fn value_at(&self, elapsed: u32) -> (i32, bool) {
        let keys = self.keys();
        let first = keys.first().map_or(0, |key| key.1);
        let Some(running) = elapsed.checked_sub(self.delay) else {
            return (first, false);
        };
        let duration = self.duration.max(1);
        let run = running.checked_div(duration).unwrap_or(0);
        let ended = match self.repeat {
            Repeat::Times(times) => run >= u32::from(times.max(1)),
            Repeat::Forever => false,
        };
        let (run, within) = if ended {
            let last = match self.repeat {
                Repeat::Times(times) => u32::from(times.max(1)).saturating_sub(1),
                Repeat::Forever => 0,
            };
            (last, duration)
        } else {
            (run, running.checked_rem(duration).unwrap_or(0))
        };
        let mut t = u16::try_from(
            u64::from(within)
                .saturating_mul(1000)
                .checked_div(u64::from(duration))
                .unwrap_or(1000),
        )
        .unwrap_or(1000);
        if self.alternate && run % 2 == 1 {
            t = 1000_u16.saturating_sub(t);
        }
        (self.between(t), ended)
    }

    /// Its value at `t` thousandths of the way through a run.
    fn between(&self, t: u16) -> i32 {
        let keys = self.keys();
        let Some(&(_, mut value)) = keys.first() else {
            return 0;
        };
        for pair in keys.windows(2) {
            let (Some(&(a, from)), Some(&(b, to))) = (pair.first(), pair.get(1)) else {
                continue;
            };
            if t < a {
                break;
            }
            if t <= b {
                let span = b.saturating_sub(a).max(1);
                let local = u16::try_from(
                    u32::from(t.saturating_sub(a))
                        .saturating_mul(1000)
                        .checked_div(u32::from(span))
                        .unwrap_or(1000),
                )
                .unwrap_or(1000);
                let eased = i64::from(self.easing.at(local));
                let delta = i64::from(to).saturating_sub(i64::from(from));
                return i32::try_from(
                    i64::from(from).saturating_add(delta.saturating_mul(eased) / 1000),
                )
                .unwrap_or(to);
            }
            value = to;
        }
        value
    }
}

/// How a node comes into view when what it is in starts to show (builder
/// contract v23; CSS's `transition` on a property a condition changes):
/// from `property`'s unshown value - unseen, a little to the side or
/// below, a little small - to rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    /// What moves: opacity, a translation or the scale.
    pub property: Property,
    /// How long, in milliseconds.
    pub duration: u32,
    /// After how long, in milliseconds.
    pub delay: u32,
    /// How it is paced.
    pub easing: Easing,
}

impl Transition {
    /// `property` over `duration` milliseconds, as CSS's `ease`.
    #[must_use]
    pub const fn new(property: Property, duration: u32) -> Self {
        Self {
            property,
            duration,
            delay: 0,
            easing: Easing::Ease,
        }
    }

    /// After `delay` milliseconds.
    #[must_use]
    pub const fn delay(self, delay: u32) -> Self {
        Self { delay, ..self }
    }

    /// Paced by `easing`.
    #[must_use]
    pub const fn easing(self, easing: Easing) -> Self {
        Self { easing, ..self }
    }

    /// The animation it runs as its node comes into view.
    #[must_use]
    pub const fn entering(&self) -> Animation {
        let from = match self.property {
            Property::Opacity => 0,
            Property::TranslateX | Property::TranslateY => 8,
            Property::Scale => 950,
            Property::Reveal => 0,
            _ => 0,
        };
        let to = match self.property {
            Property::Opacity => 255,
            Property::Scale | Property::Reveal => 1000,
            _ => 0,
        };
        Animation::new(self.property, from, to, self.duration)
            .delay(self.delay)
            .easing(self.easing)
    }
}

/// How a node is drawn while it moves: at rest, all at their identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Visual {
    /// Opacity, 0 to 255.
    pub opacity: u8,
    /// How far right and down it is drawn, in layout units.
    pub offset: (i32, i32),
    /// Its scale about its centre, in thousandths.
    pub scale: u16,
    /// Its fill: from, to and how far, in thousandths.
    pub fill: Option<(Colour, Colour, u16)>,
    /// Its text's colour likewise.
    pub text: Option<(Colour, Colour, u16)>,
    /// How much of its height shows, in thousandths.
    pub reveal: u16,
    /// An icon's turn, in tenths of a degree.
    pub turn: i32,
    /// A progress bar's moving segment.
    pub sweep: Option<u16>,
    /// A shimmer's band.
    pub shimmer: Option<u16>,
}

impl Default for Visual {
    fn default() -> Self {
        Self::REST
    }
}

impl Default for Animation {
    /// An opacity held at 0, for no time: a placeholder.
    fn default() -> Self {
        Self::new(Property::Opacity, 0, 0, 0)
    }
}

impl Visual {
    /// At rest.
    pub const REST: Self = Self {
        opacity: u8::MAX,
        offset: (0, 0),
        scale: 1000,
        fill: None,
        text: None,
        reveal: 1000,
        turn: 0,
        sweep: None,
        shimmer: None,
    };

    /// Whether drawing it needs composing (ADR-0027): see-through, scaled
    /// or shimmering.
    #[must_use]
    pub const fn composes(&self) -> bool {
        self.opacity < u8::MAX || self.scale != 1000 || self.shimmer.is_some()
    }

    /// `value` of `property`, set.
    fn set(&mut self, property: Property, value: i32) {
        let thousandths = |v: i32| u16::try_from(v.clamp(0, 1000)).unwrap_or(0);
        match property {
            Property::Opacity => {
                self.opacity = u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
            }
            Property::TranslateX => self.offset.0 = value,
            Property::TranslateY => self.offset.1 = value,
            Property::Scale => self.scale = u16::try_from(value.clamp(500, 2000)).unwrap_or(1000),
            Property::Fill(from, to) => self.fill = Some((from, to, thousandths(value))),
            Property::TextColour(from, to) => self.text = Some((from, to, thousandths(value))),
            Property::Reveal => self.reveal = thousandths(value),
            Property::Turn => self.turn = value,
            Property::Sweep => self.sweep = Some(thousandths(value)),
            Property::Shimmer => self.shimmer = Some(thousandths(value)),
        }
    }

    /// `property` back at rest.
    fn rest(&mut self, property: Property) {
        match property {
            Property::Opacity => self.opacity = u8::MAX,
            Property::TranslateX => self.offset.0 = 0,
            Property::TranslateY => self.offset.1 = 0,
            Property::Scale => self.scale = 1000,
            Property::Fill(..) => self.fill = None,
            Property::TextColour(..) => self.text = None,
            Property::Reveal => self.reveal = 1000,
            Property::Turn => self.turn = 0,
            Property::Sweep => self.sweep = None,
            Property::Shimmer => self.shimmer = None,
        }
    }
}

/// How much a screen moves: what a product's theme, or the person's
/// setting, asks for. Off until the UI loop, which drives the runtime's
/// clock ([`Tree::advance`]), turns it on: a screen never waits on a clock
/// that does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MotionSetting {
    /// Transitions and animations as asked.
    Full,
    /// Movement an instant change, fades at most [`REDUCED_FADE`] long,
    /// and nothing repeating forever.
    Reduced,
    /// No transitions: every change instant.
    #[default]
    Off,
}

/// An animation running on a node.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Running {
    node: NodeId,
    animation: Animation,
    elapsed: u32,
}

/// The tree's motion: what runs, how each moving node is drawn, and the
/// setting.
#[derive(Debug, Clone)]
pub(crate) struct Motion {
    running: BoundedVec<Running, MAX_RUNNING>,
    visuals: BoundedVec<(NodeId, Visual), MAX_RUNNING>,
    pub(crate) setting: MotionSetting,
}

impl Motion {
    pub(crate) fn new() -> Self {
        Self {
            running: BoundedVec::new(),
            visuals: BoundedVec::new(),
            setting: MotionSetting::Off,
        }
    }

    /// Nothing running, everything at rest.
    pub(crate) fn clear(&mut self) {
        self.running.clear();
        self.visuals.clear();
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// Sets how much the screen moves, from now; turned on, every node's
    /// own animation starts ([`Look::animation`](crate::Look)).
    pub fn set_motion(&mut self, setting: MotionSetting) {
        let was = self.motion.setting;
        self.motion.setting = setting;
        if was == MotionSetting::Off && setting != MotionSetting::Off {
            self.start_declared();
        }
    }

    /// Starts every node's own animation (builder contract v23), as the
    /// motion setting allows.
    pub(crate) fn start_declared(&mut self) {
        if self.motion.setting == MotionSetting::Off {
            return;
        }
        for index in 0..self.nodes.len() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let id = NodeId(index);
            if let Some(animation) = self.get(id).and_then(|node| node.style.look.animation) {
                let _ = self.animate(id, animation);
            }
        }
    }

    /// After a condition changed what shows: each node with a transition
    /// now showing inside a group or switch numbered `which` comes into
    /// view (builder contract v23).
    pub(crate) fn enter_shown(&mut self, which: impl Fn(&crate::NodeKind) -> bool) {
        if self.motion.setting == MotionSetting::Off {
            return;
        }
        for index in 0..self.nodes.len() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let id = NodeId(index);
            let Some(transition) = self.get(id).and_then(|node| node.style.look.transition) else {
                continue;
            };
            if !self.is_shown(id) || !self.inside_where(id, &which) {
                continue;
            }
            let _ = self.animate(id, transition.entering());
        }
    }

    /// Whether `id` is inside a node `which` picks.
    fn inside_where(&self, id: NodeId, which: &impl Fn(&crate::NodeKind) -> bool) -> bool {
        let mut current = self.get(id).and_then(|node| node.parent);
        for _ in 0..=crate::MAX_DEPTH {
            let Some(node) = current.and_then(|at| self.get(at)) else {
                return false;
            };
            if which(&node.kind) {
                return true;
            }
            current = node.parent;
        }
        false
    }

    /// How much the screen moves.
    #[must_use]
    pub const fn motion(&self) -> MotionSetting {
        self.motion.setting
    }

    /// How node `id` is drawn now.
    #[must_use]
    pub fn visual(&self, id: NodeId) -> Visual {
        self.motion
            .visuals
            .iter()
            .find(|(node, _)| *node == id)
            .map_or(Visual::REST, |&(_, visual)| visual)
    }

    /// Whether anything is moving: the loop calls [`advance`](Self::advance)
    /// at its next frame.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        !self.motion.running.is_empty() || self.fling.is_some()
    }

    /// Runs `animation` on node `id`, in place of any running there on the
    /// same property, as the motion setting allows: under reduced motion,
    /// movement goes straight to its end, a fade is shortened and nothing
    /// repeats forever; with motion off, everything goes to its end.
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`]; [`UiError::TooManyAnimations`] with
    /// [`MAX_RUNNING`] running.
    pub fn animate(&mut self, id: NodeId, animation: Animation) -> Result<(), UiError> {
        self.node(id)?;
        self.stop(id, animation.property);
        let mut animation = animation;
        let instant = match self.motion.setting {
            MotionSetting::Full => false,
            MotionSetting::Off => true,
            MotionSetting::Reduced => {
                if animation.property.moves() || animation.repeat == Repeat::Forever {
                    true
                } else {
                    animation.duration = animation.duration.min(REDUCED_FADE);
                    animation.delay = 0;
                    false
                }
            }
        };
        if instant {
            if animation.keep && animation.repeat != Repeat::Forever {
                let last = animation.keys().last().map_or(0, |key| key.1);
                self.set_visual(id, |visual| visual.set(animation.property, last))?;
            }
            return Ok(());
        }
        let (start, _) = animation.value_at(0);
        self.set_visual(id, |visual| visual.set(animation.property, start))?;
        self.motion
            .running
            .push(Running {
                node: id,
                animation,
                elapsed: 0,
            })
            .map_err(|_| UiError::TooManyAnimations)
    }

    /// Stops the animation of `property` running on node `id`, if any;
    /// the node is drawn at rest on that property.
    pub fn stop(&mut self, id: NodeId, property: Property) {
        let before = self.motion.running.len();
        self.motion
            .running
            .retain(|running| !(running.node == id && running.animation.property == property));
        if self.motion.running.len() != before {
            let _ = self.set_visual(id, |visual| visual.rest(property));
        }
    }

    /// Moves every animation on by `elapsed` milliseconds and marks what
    /// each moved; whether any still runs.
    pub(crate) fn step_motion(&mut self, elapsed: u32) -> bool {
        let mut index = 0;
        while let Some(running) = self.motion.running.as_mut_slice().get_mut(index) {
            running.elapsed = running.elapsed.saturating_add(elapsed);
            let (node, animation, at) = (running.node, running.animation, running.elapsed);
            let (value, ended) = animation.value_at(at);
            let _ = self.set_visual(node, |visual| {
                if ended && !animation.keep {
                    visual.rest(animation.property);
                } else {
                    visual.set(animation.property, value);
                }
            });
            if ended {
                let _ = self.motion.running.remove(index);
            } else {
                index = index.saturating_add(1);
            }
        }
        // Visuals back at rest are let go.
        self.motion
            .visuals
            .retain(|(_, visual)| *visual != Visual::REST);
        !self.motion.running.is_empty()
    }

    /// Changes node `id`'s visual with `change`, marking where it was drawn
    /// and where it is drawn now.
    fn set_visual(&mut self, id: NodeId, change: impl FnOnce(&mut Visual)) -> Result<(), UiError> {
        let before = self.visual(id);
        let mut after = before;
        change(&mut after);
        if after == before {
            return Ok(());
        }
        let rect = self.node(id)?.rect;
        let reach = self.scale.px(crate::layer::SHADOW_REACH);
        for visual in [before, after] {
            self.mark_area_dirty(drawn_area(rect, visual, self.scale, reach));
        }
        match self
            .motion
            .visuals
            .as_mut_slice()
            .iter_mut()
            .find(|(node, _)| *node == id)
        {
            Some(entry) => entry.1 = after,
            None => self
                .motion
                .visuals
                .push((id, after))
                .map_err(|_| UiError::TooManyAnimations)?,
        }
        Ok(())
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// A layer's default transition as it opens: a dialog fades and grows,
    /// a menu or popover fades and drops in, a tooltip fades.
    pub(crate) fn transition_in(&mut self, id: NodeId, kind: crate::LayerKind) {
        let _ = match kind {
            crate::LayerKind::Dialog => self.animate(id, Animation::fade_in(160)).and_then(|()| {
                self.animate(
                    id,
                    Animation::new(Property::Scale, 940, 1000, 160).easing(Easing::EaseOut),
                )
            }),
            crate::LayerKind::Menu | crate::LayerKind::Popover => {
                self.animate(id, Animation::fade_in(120)).and_then(|()| {
                    self.animate(
                        id,
                        Animation::new(Property::TranslateY, -6, 0, 120).easing(Easing::EaseOut),
                    )
                })
            }
            crate::LayerKind::Tooltip => self.animate(id, Animation::fade_in(100)),
        };
    }

    /// A new screen's default transition: it slides in a little from the
    /// side it comes from - the right going on, the left going back - and
    /// fades in.
    pub(crate) fn transition_screen(&mut self, forward: bool) {
        let Some(root) = self.root else {
            return;
        };
        let from = if forward { 24 } else { -24 };
        let _ = self.animate(root, Animation::fade_in(200)).and_then(|()| {
            self.animate(
                root,
                Animation::new(Property::TranslateX, from, 0, 200).easing(Easing::EaseOut),
            )
        });
    }
}

/// Where a node laid out at `rect` is drawn as `visual`, at `scale`, with
/// `reach` pixels round it for a shadow: the area a change marks.
#[must_use]
pub fn drawn_area(
    rect: Rect,
    visual: Visual,
    scale: nowayos_display::DeviceScale,
    reach: u32,
) -> Rect {
    let (dx, dy) = (
        scale.px_signed(i64::from(visual.offset.0)),
        scale.px_signed(i64::from(visual.offset.1)),
    );
    let grow = |length: u32| {
        let scaled = i64::from(length).saturating_mul(i64::from(visual.scale)) / 1000;
        (scaled.saturating_sub(i64::from(length)).max(0) / 2).saturating_add(1)
    };
    let (gx, gy) = (grow(rect.size.width), grow(rect.size.height));
    let reach = i64::from(reach);
    Rect::from_edges(
        rect.left()
            .saturating_add(dx)
            .saturating_sub(gx)
            .saturating_sub(reach),
        rect.top()
            .saturating_add(dy)
            .saturating_sub(gy)
            .saturating_sub(reach),
        rect.right()
            .saturating_add(dx)
            .saturating_add(gx)
            .saturating_add(reach),
        rect.bottom()
            .saturating_add(dy)
            .saturating_add(gy)
            .saturating_add(reach),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure fails the test"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn named_curves_start_at_0_end_at_1000_and_steps_jump() {
        for easing in [
            Easing::Linear,
            Easing::Ease,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            assert_eq!(easing.at(0), 0, "{easing:?}");
            assert_eq!(easing.at(1000), 1000, "{easing:?}");
        }
        assert!(Easing::EaseIn.at(250) < 250 && Easing::EaseOut.at(250) > 250);
        let steps = Easing::Steps {
            count: 4,
            start: false,
        };
        assert_eq!(
            [steps.at(0), steps.at(249), steps.at(250), steps.at(999)],
            [0, 0, 250, 750]
        );
        let start = Easing::Steps {
            count: 4,
            start: true,
        };
        assert_eq!(start.at(0), 250);
    }

    #[test]
    fn an_animation_runs_its_keys_repeats_alternates_and_ends() {
        let fade = Animation::new(Property::Opacity, 0, 200, 100).easing(Easing::Linear);
        assert_eq!(fade.value_at(50), (100, false));
        assert_eq!(fade.value_at(100), (200, true));
        let late = fade.delay(40);
        assert_eq!(late.value_at(20), (0, false));
        assert_eq!(late.value_at(90), (100, false));
        let pulse = Animation::keyframes(
            Property::Scale,
            &[(0, 1000), (500, 1100), (1000, 1000)],
            100,
        )
        .easing(Easing::Linear)
        .repeat(Repeat::Times(2))
        .alternate();
        assert_eq!(pulse.value_at(25).0, 1050);
        assert_eq!(pulse.value_at(50).0, 1100);
        assert_eq!(pulse.value_at(125).0, 1050, "the second run backwards");
        assert!(pulse.value_at(200).1);
        assert!(!Animation::spin().value_at(1_000_000).1, "forever");
    }

    proptest! {
        #[test]
        fn standard_curves_never_go_back(a in 0u16..=1000, b in 0u16..=1000) {
            let (low, high) = (a.min(b), a.max(b));
            for easing in [Easing::Linear, Easing::Ease, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut,
                Easing::Steps { count: 5, start: false }, Easing::Bezier([100, 700, 300, 1000])] {
                prop_assert!(easing.at(low) <= easing.at(high) + 1, "{easing:?} at {low} and {high}");
                prop_assert!((0..=1000).contains(&easing.at(high)));
            }
        }

        #[test]
        fn any_animation_stays_between_its_keys(
            from in -500i32..500, to in -500i32..500, duration in 1u32..2000, at in 0u32..5000,
        ) {
            let (value, _) = Animation::new(Property::TranslateX, from, to, duration).value_at(at);
            prop_assert!(value >= from.min(to) - 1 && value <= from.max(to) + 1);
        }
    }
}
