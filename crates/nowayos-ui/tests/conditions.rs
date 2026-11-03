// SPDX-License-Identifier: MIT OR Apache-2.0
//! Conditions beyond one prop's truth (builder contract v13): a child shown
//! while a group is hidden - React's `!cond && <X/>` - and switches, a
//! child shown while a switch the application sets is on one case -
//! `step === "review" && <X/>`, or `? <X/> : <Y/>` - every switch of one
//! number moving together. What is not shown takes no space and no gap,
//! and is neither drawn, focusable nor hit, keeping its values; a wipe
//! puts every switch back on no case.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{ActionId, GroupId, NodeId, SwitchId, TextMetrics, Tree, UiError};

const METRICS: TextMetrics = TextMetrics::new(6, 10);
const VIEWPORT: Rect = Rect::new(0, 0, 200, 120);
const READY: GroupId = GroupId(1);
const STEP: SwitchId = SwitchId(1);
const CHOOSE: u8 = 0;
const REVIEW: u8 = 1;
const PAY: u8 = 2;

/// A column, gap 4, each line 10 high: "Waiting" unless ready; "Choose" on
/// the step's case 0; a button "Review" on case 1; "Pay" on case 2, else
/// "Not paying yet"; and a last line.
struct Screen {
    tree: Tree<24>,
    waiting: NodeId,
    choose: NodeId,
    review: NodeId,
    pay: NodeId,
    not_paying: NodeId,
    last: NodeId,
}

fn screen() -> Screen {
    let mut tree: Tree<24> = Tree::new();
    let b = &mut tree;
    let waiting = UiBuilder::text(b, "Waiting").unwrap();
    let unless_ready = UiBuilder::unless(b, READY, waiting).unwrap();
    let choose = UiBuilder::text(b, "Choose").unwrap();
    let on_choose = UiBuilder::case(b, STEP, CHOOSE, choose).unwrap();
    let review = UiBuilder::button(b, "Review", ActionId(1)).unwrap();
    let on_review = UiBuilder::case(b, STEP, REVIEW, review).unwrap();
    let pay = UiBuilder::text(b, "Pay").unwrap();
    let not_paying = UiBuilder::text(b, "Not paying yet").unwrap();
    let on_pay = UiBuilder::case_or(b, STEP, PAY, pay, not_paying).unwrap();
    let last = UiBuilder::text(b, "Last").unwrap();
    let root = UiBuilder::column(
        b,
        ui::Layout::new().gap(4),
        &[unless_ready, on_choose, on_review, on_pay, last],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(VIEWPORT, &METRICS).unwrap();
    Screen {
        tree,
        waiting,
        choose,
        review,
        pay,
        not_paying,
        last,
    }
}

impl Screen {
    fn shown(&self) -> Vec<NodeId> {
        [
            self.waiting,
            self.choose,
            self.review,
            self.pay,
            self.not_paying,
            self.last,
        ]
        .into_iter()
        .filter(|id| self.tree.is_shown(*id))
        .collect()
    }

    fn relayout(&mut self) {
        self.tree.layout(VIEWPORT, &METRICS).unwrap();
    }

    fn y(&self, id: NodeId) -> i32 {
        self.tree.get(id).unwrap().rect().origin.y
    }
}

#[test]
fn unless_shows_its_child_while_its_group_is_hidden() {
    let mut s = screen();
    // At first: not ready, so "Waiting"; no case, so "Not paying yet".
    assert_eq!(s.shown(), [s.waiting, s.not_paying, s.last]);
    assert_eq!(
        (s.y(s.waiting), s.y(s.not_paying), s.y(s.last)),
        (0, 14, 28)
    );
    s.tree.set_visible(READY, true).unwrap();
    s.relayout();
    assert_eq!(s.shown(), [s.not_paying, s.last]);
    // Showing nothing, it takes no space and no gap.
    assert_eq!((s.y(s.not_paying), s.y(s.last)), (0, 14));
    assert_eq!(s.tree.visible(READY), Some(true));
}

#[test]
fn every_switch_of_one_number_shows_its_own_case() {
    let mut s = screen();
    s.tree.set_visible(READY, true).unwrap();
    for (case, shown) in [
        (Some(CHOOSE), vec![s.choose, s.not_paying, s.last]),
        (Some(REVIEW), vec![s.review, s.not_paying, s.last]),
        (Some(PAY), vec![s.pay, s.last]),
        (Some(9), vec![s.not_paying, s.last]),
        (None, vec![s.not_paying, s.last]),
    ] {
        s.tree.set_case(STEP, case).unwrap();
        s.relayout();
        assert_eq!(s.shown(), shown, "{case:?}");
        assert_eq!(s.tree.case_of(STEP), Some(case));
        assert_eq!(
            s.y(shown[0]),
            0,
            "the case shown takes the first place: {case:?}"
        );
    }
}

#[test]
fn a_control_a_switch_stops_showing_loses_focus_and_cannot_be_hit() {
    let mut s = screen();
    s.tree.set_case(STEP, Some(REVIEW)).unwrap();
    s.relayout();
    s.tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    assert_eq!(s.tree.focused(), Some(s.review));
    let centre = s.tree.get(s.review).unwrap().rect().center();
    s.tree.set_case(STEP, Some(PAY)).unwrap();
    s.relayout();
    assert_eq!(
        s.tree.focused(),
        None,
        "focus never rests on what does not show"
    );
    assert_eq!(s.tree.control_at(Point::new(centre.x, centre.y)), None);
}

#[test]
fn a_wipe_puts_every_switch_back_on_no_case_and_hides_every_group() {
    let mut s = screen();
    s.tree.set_visible(READY, true).unwrap();
    s.tree.set_case(STEP, Some(PAY)).unwrap();
    assert!(!s.tree.is_wiped());
    s.tree.wipe_session();
    assert!(s.tree.is_wiped());
    assert_eq!(s.tree.case_of(STEP), Some(None));
    s.relayout();
    assert_eq!(s.shown(), [s.waiting, s.not_paying, s.last]);
}

#[test]
fn an_unknown_switch_is_an_error_and_changes_nothing() {
    let mut s = screen();
    assert_eq!(
        s.tree.set_case(SwitchId(9), Some(0)),
        Err(UiError::UnknownSwitch(SwitchId(9)))
    );
    assert_eq!(s.tree.case_of(SwitchId(9)), None);
    assert_eq!(s.tree.case_of(STEP), Some(None));
}
