// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Navigator`]: moving between an application's screens within one
//! bounded [`Tree`], with a bounded back stack.
//!
//! A kiosk's form is several screens, but the UI holds one tree of at most
//! `N` nodes. Switching screens therefore rebuilds the tree: the old screen
//! is cleared - its typed values and slot texts wiped in place - and the
//! application's build function builds the new one into the same storage.
//! The navigator then lays the screen out, so the next frame repaints the
//! whole viewport, and moves focus to its first control.
//!
//! A screen that cannot be built - more nodes than the tree holds, nesting
//! deeper than [`MAX_DEPTH`](crate::MAX_DEPTH), or any other error from the
//! build function - is never shown half-built: the tree is cleared, the
//! start screen is built afresh and the error is returned. The back stack
//! holds at most `DEPTH` screens; going further is refused before anything
//! changes.
//!
//! What a screen shows is rebuilt each time it is shown: the navigator
//! keeps no values. An application that wants answers back on a screen it
//! returns to keeps them itself and sets them again
//! ([`Tree::set_value`], [`Tree::set_slot`]) - in its build function or
//! after the switch.
//!
//! ```
//! use nowayos_display::Rect;
//! use nowayos_ui::{ActionId, Navigator, NodeId, ScreenId, Style, TextMetrics, Tree, UiError};
//!
//! const START: ScreenId = ScreenId(0);
//! const DETAILS: ScreenId = ScreenId(1);
//!
//! fn build(screen: ScreenId, tree: &mut Tree<16>) -> Result<NodeId, UiError> {
//!     let column = tree.column(Style::new().padding(8).gap(6))?;
//!     let title = tree.text(if screen == START { "Welcome" } else { "Your details" })?;
//!     tree.append(column, title)?;
//!     let next = tree.button("Next", ActionId(1))?;
//!     tree.append(column, next)?;
//!     Ok(column)
//! }
//!
//! let mut tree: Tree<16> = Tree::new();
//! let mut nav: Navigator<4> =
//!     Navigator::new(START, Rect::new(0, 0, 160, 120), TextMetrics::new(6, 10));
//! nav.show_start(&mut tree, build)?;
//! nav.go(DETAILS, &mut tree, build)?;
//! assert_eq!(nav.current(), DETAILS);
//! assert!(tree.focused().is_some(), "the first control has focus");
//! assert_eq!(tree.dirty_rects(), &[Rect::new(0, 0, 160, 120)], "a full repaint");
//! assert!(nav.back(&mut tree, build)?);
//! assert_eq!(nav.current(), START);
//! # Ok::<(), UiError>(())
//! ```
//!
//! Provisional: this API may change.

use nowayos_bounded::BoundedVec;
use nowayos_display::Rect;

use crate::{NodeId, TextMetrics, Tree, UiError};

/// Which screen, as a number the application defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ScreenId(pub u16);

impl ScreenId {
    /// The screen numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The screen's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Moves between screens built into one bounded [`Tree`], remembering at
/// most `DEPTH` screens to go back to. See the [module](self) for how.
///
/// Every method that shows a screen takes the tree and the application's
/// build function, `build(screen, tree)`, which builds `screen` into the
/// empty tree and returns its root. The navigator holds no values and no
/// nodes of its own.
#[derive(Debug, Clone)]
pub struct Navigator<const DEPTH: usize> {
    start: ScreenId,
    current: ScreenId,
    back: BoundedVec<ScreenId, DEPTH>,
    viewport: Rect,
    metrics: TextMetrics,
}

impl<const DEPTH: usize> Navigator<DEPTH> {
    /// A navigator whose start screen is `start`, laying screens out in
    /// `viewport` with `metrics` (the theme's font). It shows nothing until
    /// [`show_start`](Self::show_start) is called.
    #[must_use]
    pub fn new(start: ScreenId, viewport: Rect, metrics: TextMetrics) -> Self {
        Self {
            start,
            current: start,
            back: BoundedVec::new(),
            viewport,
            metrics,
        }
    }

    /// The screen showing (the start screen before anything is shown).
    #[must_use]
    pub const fn current(&self) -> ScreenId {
        self.current
    }

    /// The start screen.
    #[must_use]
    pub const fn start(&self) -> ScreenId {
        self.start
    }

    /// The screens [`back`](Self::back) would return to, oldest first.
    #[must_use]
    pub fn back_stack(&self) -> &[ScreenId] {
        self.back.as_slice()
    }

    /// The viewport screens are laid out in.
    #[must_use]
    pub const fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The metrics screens are laid out with.
    #[must_use]
    pub const fn metrics(&self) -> TextMetrics {
        self.metrics
    }

    /// Lays screens out in `viewport` with `metrics` from now on, and lays
    /// `tree` - the screen showing - out again in them, keeping its values,
    /// focus and back stack: what a UI calls once the display has
    /// announced its layout and device scale, so screens built before then
    /// fill the screen at its own pixels. The whole viewport is dirty after.
    ///
    /// # Errors
    ///
    /// A layout error ([`Tree::layout`]); the navigator then keeps the new
    /// viewport and metrics, and the tree its last layout.
    pub fn resize<const N: usize, const T: usize>(
        &mut self,
        tree: &mut Tree<N, T>,
        viewport: Rect,
        metrics: TextMetrics,
    ) -> Result<(), UiError> {
        self.viewport = viewport;
        self.metrics = metrics;
        tree.layout(viewport, &metrics)
    }

    /// Shows the start screen and forgets every screen to go back to.
    ///
    /// # Errors
    ///
    /// The build function's error, or a layout error. The tree is then left
    /// cleared, with no root, and the next frame paints only the
    /// background.
    pub fn show_start<const N: usize, const T: usize, F>(
        &mut self,
        tree: &mut Tree<N, T>,
        mut build: F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        self.back.clear();
        self.current = self.start;
        let shown = self.build_into(self.start, tree, &mut build);
        if shown.is_err() {
            tree.clear();
        }
        shown
    }

    /// Ends the session without trace: wipes everything the tree holds in
    /// place ([`Tree::wipe_session`]: every typed value and slot text
    /// zeroed, every choice cleared), then shows the start screen, freshly
    /// built, with nothing to go back to. The start screen is laid out
    /// again, so the next frame repaints the whole viewport and no pixel of
    /// the previous screen survives it.
    ///
    /// # Errors
    ///
    /// As [`show_start`](Self::show_start); the wipe happens first either
    /// way.
    pub fn reset<const N: usize, const T: usize, F>(
        &mut self,
        tree: &mut Tree<N, T>,
        build: F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        tree.wipe_session();
        self.show_start(tree, build)
    }

    /// Shows `screen`, remembering the current one to go back to.
    ///
    /// # Errors
    ///
    /// [`UiError::BackStackFull`] if `DEPTH` screens are already remembered:
    /// nothing changes, and the current screen stays. Otherwise, if `screen`
    /// cannot be built, the build function's error (for example
    /// [`UiError::TreeFull`] or [`UiError::TooDeep`]) or a layout error,
    /// after the start screen has been shown in its place, freshly built,
    /// with nothing to go back to.
    pub fn go<const N: usize, const T: usize, F>(
        &mut self,
        screen: ScreenId,
        tree: &mut Tree<N, T>,
        build: F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        self.back
            .push(self.current)
            .map_err(|_| UiError::BackStackFull { capacity: DEPTH })?;
        self.switch(screen, tree, build)?;
        // Its default transition, as the motion setting allows (#10086).
        tree.transition_screen(true);
        Ok(())
    }

    /// Shows `screen` in place of the current one, which is not remembered.
    ///
    /// # Errors
    ///
    /// As [`go`](Self::go) when `screen` cannot be built.
    pub fn replace<const N: usize, const T: usize, F>(
        &mut self,
        screen: ScreenId,
        tree: &mut Tree<N, T>,
        build: F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        self.switch(screen, tree, build)
    }

    /// Shows the screen before this one: `Ok(false)`, changing nothing, if
    /// there is none.
    ///
    /// # Errors
    ///
    /// As [`go`](Self::go) when that screen cannot be built.
    pub fn back<const N: usize, const T: usize, F>(
        &mut self,
        tree: &mut Tree<N, T>,
        build: F,
    ) -> Result<bool, UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        let Some(previous) = self.back.pop() else {
            return Ok(false);
        };
        self.switch(previous, tree, build)?;
        tree.transition_screen(false);
        Ok(true)
    }

    /// Clears the tree and shows `screen`; if it cannot be built, shows the
    /// start screen instead and returns the error.
    fn switch<const N: usize, const T: usize, F>(
        &mut self,
        screen: ScreenId,
        tree: &mut Tree<N, T>,
        mut build: F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        match self.build_into(screen, tree, &mut build) {
            Ok(()) => {
                self.current = screen;
                Ok(())
            }
            Err(error) => {
                // Never a partial screen: start again from the start screen.
                // If even that fails, the tree stays cleared.
                let _ = self.show_start(tree, build);
                Err(error)
            }
        }
    }

    /// Clears the tree, builds `screen` into it, lays it out (so the whole
    /// viewport is dirty) and focuses its first control.
    fn build_into<const N: usize, const T: usize, F>(
        &self,
        screen: ScreenId,
        tree: &mut Tree<N, T>,
        build: &mut F,
    ) -> Result<(), UiError>
    where
        F: FnMut(ScreenId, &mut Tree<N, T>) -> Result<NodeId, UiError>,
    {
        tree.clear();
        let root = build(screen, tree)?;
        tree.set_root(root)?;
        tree.layout(self.viewport, &self.metrics)?;
        tree.focus_next();
        Ok(())
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
    use crate::{ActionId, FieldId, SlotId, Style};

    /// A screen built before the display's layout is known is laid
    /// out again in its viewport, at its scale, keeping what was typed;
    /// the next screen is built there too.
    #[test]
    fn a_resize_lays_the_screen_out_again_and_keeps_its_values() {
        let small = TextMetrics::new(6, 10);
        let mut nav: Navigator<4> = Navigator::new(ScreenId(0), Rect::new(0, 0, 80, 40), small);
        let mut tree: Tree<16> = Tree::new();
        let build = |_: ScreenId, tree: &mut Tree<16>| {
            let column = tree.column(Style::new().padding(4))?;
            let name = tree.text_input(FieldId(1), "")?;
            tree.append(column, name)?;
            Ok(column)
        };
        nav.show_start(&mut tree, build).unwrap();
        tree.set_value(FieldId(1), "Ada").unwrap();
        let input = tree.find_field(FieldId(1)).unwrap();
        assert_eq!(tree.get(input).unwrap().rect().origin.x, 4);
        let large = TextMetrics::new(12, 20).with_scale(nowayos_display::DeviceScale::TWO);
        nav.resize(&mut tree, Rect::new(0, 0, 160, 80), large)
            .unwrap();
        assert_eq!(
            (nav.viewport(), nav.metrics()),
            (Rect::new(0, 0, 160, 80), large)
        );
        assert_eq!(tree.scale(), nowayos_display::DeviceScale::TWO);
        assert_eq!(tree.get(input).unwrap().rect().origin.x, 8, "padding at 2x");
        assert_eq!(tree.value(FieldId(1)), Some("Ada"));
        assert_eq!(tree.dirty_rects(), &[Rect::new(0, 0, 160, 80)]);
        nav.go(ScreenId(1), &mut tree, build).unwrap();
        assert_eq!(tree.viewport(), Rect::new(0, 0, 160, 80));
    }

    const START: ScreenId = ScreenId(0);
    const DETAILS: ScreenId = ScreenId(1);
    const REVIEW: ScreenId = ScreenId(2);
    /// More nodes than the tree holds.
    const HUGE: ScreenId = ScreenId(3);
    /// Nested deeper than MAX_DEPTH.
    const DEEP: ScreenId = ScreenId(4);
    /// Fails part-way through, after creating nodes.
    const BROKEN: ScreenId = ScreenId(5);
    /// No controls at all.
    const NOTICE: ScreenId = ScreenId(6);

    const NAME: FieldId = FieldId(1);
    const CODE: FieldId = FieldId(2);
    const ANSWER: SlotId = SlotId(1);
    const NEXT: ActionId = ActionId(1);
    const VIEWPORT: Rect = Rect::new(0, 0, 160, 120);
    const METRICS: TextMetrics = TextMetrics::new(6, 10);

    type Small = Tree<24>;

    /// Synthetic screens. Each starts with a title, so its first control is
    /// never its first node.
    fn build(screen: ScreenId, tree: &mut Small) -> Result<NodeId, UiError> {
        let column = tree.column(Style::new().padding(4).gap(2))?;
        let title = tree.text("Title")?;
        tree.append(column, title)?;
        match screen {
            START => {
                let next = tree.button("Start", NEXT)?;
                tree.append(column, next)?;
            }
            DETAILS => {
                for field in [NAME, CODE] {
                    let input = tree.text_input(field, "")?;
                    tree.append(column, input)?;
                }
            }
            REVIEW => {
                let answer = tree.slot(ANSWER)?;
                tree.append(column, answer)?;
                let next = tree.button("Send", NEXT)?;
                tree.append(column, next)?;
            }
            HUGE => loop {
                let line = tree.text("more")?;
                tree.append(column, line)?;
            },
            DEEP => {
                let mut parent = column;
                loop {
                    let inner = tree.column(Style::new())?;
                    tree.append(parent, inner)?;
                    parent = inner;
                }
            }
            BROKEN => {
                let input = tree.text_input(NAME, "")?;
                tree.append(column, input)?;
                tree.set_value(NAME, "half-built")?;
                return Err(UiError::NoRoot);
            }
            _ => {}
        }
        Ok(column)
    }

    /// The navigator showing its start screen in a fresh tree.
    fn started<const DEPTH: usize>() -> (Navigator<DEPTH>, Small) {
        let mut nav = Navigator::new(START, VIEWPORT, METRICS);
        let mut tree = Small::new();
        nav.show_start(&mut tree, build).unwrap();
        (nav, tree)
    }

    /// The first focusable node in paint order, if any.
    fn first_control(tree: &Small) -> Option<NodeId> {
        tree.paint_order()
            .find(|(_, node)| node.is_focusable())
            .map(|(id, _)| id)
    }

    /// The tree holds exactly the start screen, freshly built.
    fn shows_fresh_start(nav: &Navigator<2>, tree: &Small) {
        assert_eq!(nav.current(), START);
        assert!(nav.back_stack().is_empty());
        let mut fresh = Small::new();
        let root = build(START, &mut fresh).unwrap();
        assert_eq!(tree.root(), Some(root));
        assert_eq!(tree.len(), fresh.len(), "nothing left of the failed screen");
        let kinds: std::vec::Vec<_> = tree.paint_order().map(|(_, n)| n.kind().clone()).collect();
        let expected: std::vec::Vec<_> = {
            fresh.set_root(root).unwrap();
            fresh.paint_order().map(|(_, n)| n.kind().clone()).collect()
        };
        assert_eq!(kinds, expected);
        assert_eq!(tree.focused(), first_control(tree));
        assert_eq!(tree.dirty_rects(), &[VIEWPORT]);
    }

    #[test]
    fn switching_replaces_the_screen_focuses_its_first_control_and_repaints_all() {
        let (mut nav, mut tree) = started::<2>();
        assert_eq!(tree.focused(), first_control(&tree));
        tree.clear_dirty();
        nav.go(DETAILS, &mut tree, build).unwrap();
        assert_eq!(nav.current(), DETAILS);
        assert_eq!(nav.back_stack(), &[START]);
        // Only the new screen is in the tree: title, two inputs, column.
        assert_eq!(tree.len(), 4);
        let first_input = tree.find_field(NAME).unwrap();
        assert_eq!(tree.focused(), Some(first_input));
        assert_eq!(
            tree.dirty_rects(),
            &[VIEWPORT],
            "the next frame is a full repaint"
        );
        tree.set_value(NAME, "Sam").unwrap();
        // Away and back: the values typed are wiped, not carried over.
        nav.go(REVIEW, &mut tree, build).unwrap();
        assert_eq!(tree.value(NAME), None);
        assert!(tree.slot_text(ANSWER).is_some());
        assert!(nav.back(&mut tree, build).unwrap());
        assert_eq!(nav.current(), DETAILS);
        assert_eq!(tree.value(NAME), Some(""));
        assert!(nav.back(&mut tree, build).unwrap());
        assert_eq!(nav.current(), START);
        // Nothing further back: nothing changes.
        tree.clear_dirty();
        assert!(!nav.back(&mut tree, build).unwrap());
        assert_eq!(nav.current(), START);
        assert!(tree.dirty_rects().is_empty());
    }

    #[test]
    fn a_screen_with_no_controls_has_nothing_focused() {
        let (mut nav, mut tree) = started::<2>();
        nav.replace(NOTICE, &mut tree, build).unwrap();
        assert_eq!(nav.current(), NOTICE);
        assert!(nav.back_stack().is_empty(), "replace remembers nothing");
        assert_eq!(tree.focused(), None);
    }

    #[test]
    fn a_screen_too_big_or_too_deep_shows_the_cleared_start_screen() {
        for (screen, error) in [
            (HUGE, UiError::TreeFull { capacity: 24 }),
            (DEEP, UiError::TooDeep),
            (BROKEN, UiError::NoRoot),
        ] {
            let (mut nav, mut tree) = started::<2>();
            nav.go(DETAILS, &mut tree, build).unwrap();
            tree.set_value(NAME, "Sam").unwrap();
            assert_eq!(nav.go(screen, &mut tree, build), Err(error), "{screen:?}");
            shows_fresh_start(&nav, &tree);
            assert_eq!(tree.value(NAME), None, "{screen:?}: nothing typed survives");
            // The same through replace and back.
            nav.go(DETAILS, &mut tree, build).unwrap();
            assert_eq!(nav.replace(screen, &mut tree, build), Err(error));
            shows_fresh_start(&nav, &tree);
        }
    }

    #[test]
    fn going_beyond_the_back_stack_is_refused_and_the_screen_stays() {
        let (mut nav, mut tree) = started::<2>();
        nav.go(DETAILS, &mut tree, build).unwrap();
        nav.go(REVIEW, &mut tree, build).unwrap();
        tree.set_slot(ANSWER, "Sam").unwrap();
        let focused = tree.focused();
        let root = tree.root();
        tree.clear_dirty();
        assert_eq!(
            nav.go(DETAILS, &mut tree, build),
            Err(UiError::BackStackFull { capacity: 2 })
        );
        assert_eq!(nav.current(), REVIEW);
        assert_eq!(nav.back_stack(), &[START, DETAILS]);
        assert_eq!(tree.root(), root);
        assert_eq!(tree.focused(), focused);
        assert_eq!(tree.slot_text(ANSWER), Some("Sam"));
        assert!(tree.dirty_rects().is_empty(), "nothing repaints");
        // Going back makes room again.
        assert!(nav.back(&mut tree, build).unwrap());
        nav.go(REVIEW, &mut tree, build).unwrap();
        // A navigator with no back stack refuses every go.
        let (mut flat, mut other) = started::<0>();
        assert_eq!(
            flat.go(DETAILS, &mut other, build),
            Err(UiError::BackStackFull { capacity: 0 })
        );
        flat.replace(DETAILS, &mut other, build).unwrap();
        assert_eq!(flat.current(), DETAILS);
    }

    #[test]
    fn a_reset_wipes_every_value_in_place_then_shows_the_start_screen() {
        let (mut nav, mut tree) = started::<2>();
        nav.go(DETAILS, &mut tree, build).unwrap();
        tree.set_value(NAME, "Sam Sample").unwrap();
        tree.set_value(CODE, "4207").unwrap();
        assert!(!tree.is_wiped());
        // The wipe a reset starts with, seen on the session's own nodes:
        // every byte of every value zeroed in place.
        let nodes = tree.len();
        tree.clear_dirty();
        tree.wipe_session();
        assert!(tree.is_wiped());
        assert_eq!(tree.len(), nodes, "the nodes stay");
        assert_eq!(tree.value(NAME), Some(""));
        assert_eq!(tree.dirty_rects(), &[VIEWPORT]);
        // A whole reset, from a later screen.
        tree.set_value(NAME, "Sam Sample").unwrap();
        nav.go(REVIEW, &mut tree, build).unwrap();
        tree.set_slot(ANSWER, "Sam Sample").unwrap();
        assert!(!tree.is_wiped());
        tree.clear_dirty();
        nav.reset(&mut tree, build).unwrap();
        shows_fresh_start(&nav, &tree);
        assert!(tree.is_wiped());
        assert_eq!(tree.slot_text(ANSWER), None);
    }

    #[test]
    fn if_even_the_start_screen_fails_the_tree_is_left_cleared() {
        let mut nav: Navigator<2> = Navigator::new(HUGE, VIEWPORT, METRICS);
        let mut tree = Small::new();
        assert_eq!(
            nav.show_start(&mut tree, build),
            Err(UiError::TreeFull { capacity: 24 })
        );
        assert!(tree.is_empty());
        assert_eq!(tree.root(), None);
        // A failed switch falls back to that start screen, which fails too.
        nav.replace(DETAILS, &mut tree, build).unwrap();
        assert_eq!(nav.replace(BROKEN, &mut tree, build), Err(UiError::NoRoot));
        assert!(tree.is_empty());
        assert_eq!(nav.current(), HUGE);
        assert_eq!(
            tree.value(NAME),
            None,
            "nothing of the broken screen is left"
        );
    }
}
