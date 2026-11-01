// SPDX-License-Identifier: MIT OR Apache-2.0
//! Groups (builder contract v1): content the application shows or
//! hides - React's `cond && <X/>` and `cond ? <X/> : <Y/>`. A hidden group
//! takes no space and no gap, and what it hides is neither drawn, focusable
//! nor hit, keeping its values; a shown child is laid out in the group's
//! place with its own style.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{
    ActionId, FieldId, GroupId, Length, NodeId, SlotId, Style, TextMetrics, Tree, UiError, UiEvent,
};

const METRICS: TextMetrics = TextMetrics::new(6, 10);
const VIEWPORT: Rect = Rect::new(0, 0, 200, 120);
const HELP: GroupId = GroupId(1);
const READY: GroupId = GroupId(2);
const NOTE: SlotId = SlotId(1);
const NAME: FieldId = FieldId(1);
const GO: ActionId = ActionId(1);

/// A column: a title, a group holding a text input, a group choosing
/// between two texts, a row whose button is in a group beside a slot, and
/// a last line - each 10 high, with a gap of 4.
struct Screen {
    tree: Tree<24>,
    help: NodeId,
    input: NodeId,
    ready: NodeId,
    on_way: NodeId,
    note: NodeId,
    go: NodeId,
    last: NodeId,
}

fn screen() -> Screen {
    let mut tree: Tree<24> = Tree::new();
    let b = &mut tree;
    let title = UiBuilder::text(b, "Title").unwrap();
    let input = Tree::text_input(b, NAME, "Name").unwrap();
    let help = UiBuilder::group(b, HELP, input).unwrap();
    let ready = UiBuilder::text(b, "Ready").unwrap();
    let on_way = UiBuilder::text(b, "On its way").unwrap();
    let choice = UiBuilder::either(b, READY, ready, on_way).unwrap();
    let note = UiBuilder::slot(b, NOTE).unwrap();
    let go = UiBuilder::button(b, "Go", GO).unwrap();
    let go_group = UiBuilder::group(b, GroupId(3), go).unwrap();
    let row = UiBuilder::row(b, ui::Layout::new().gap(4), &[note, go_group]).unwrap();
    let last = UiBuilder::text(b, "Last").unwrap();
    let root = UiBuilder::column(
        b,
        ui::Layout::new().gap(4),
        &[title, help, choice, row, last],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(VIEWPORT, &METRICS).unwrap();
    Screen {
        tree,
        help,
        input,
        ready,
        on_way,
        note,
        go,
        last,
    }
}

fn rect(tree: &Tree<24>, id: NodeId) -> Rect {
    tree.get(id).unwrap().rect()
}

fn shown(tree: &Tree<24>) -> Vec<NodeId> {
    tree.paint_order().map(|(id, _)| id).collect()
}

#[test]
fn a_hidden_group_takes_no_space_and_no_gap() {
    let s = screen();
    let t = &s.tree;
    // Title at 0; the hidden input group takes nothing, not even a gap; the
    // choice shows "On its way" at 14, the row at 28 - one line high, as its
    // button is hidden - and the last line at 42.
    assert_eq!(rect(t, s.on_way), Rect::new(0, 14, 200, 10));
    assert_eq!(rect(t, s.last).origin.y, 42);
    assert!(rect(t, s.help).is_empty() && rect(t, s.input).is_empty());
    assert!(
        rect(t, s.ready).is_empty(),
        "the branch not shown has no place"
    );
    assert!(!shown(t).contains(&s.input) && !shown(t).contains(&s.ready));
    assert!(shown(t).contains(&s.on_way));
    // The slot fills the row, as nothing else is in it.
    assert_eq!(rect(t, s.note), Rect::new(0, 28, 200, 10));
    assert!(t.is_shown(s.on_way) && !t.is_shown(s.ready) && !t.is_shown(s.go));
}

#[test]
fn a_shown_child_is_laid_out_in_the_groups_place_with_its_own_style() {
    let mut s = screen();
    for group in [HELP, READY, GroupId(3)] {
        s.tree.set_visible(group, true).unwrap();
    }
    assert_eq!(s.tree.visible(READY), Some(true));
    s.tree.layout(VIEWPORT, &METRICS).unwrap();
    let t = &s.tree;
    // The input: 10 + 2 x 5 padding = 20, raised to 24, stretched across.
    assert_eq!(rect(t, s.input), Rect::new(0, 14, 200, 24));
    assert_eq!(rect(t, s.help), rect(t, s.input), "the group is its child");
    assert_eq!(rect(t, s.ready), Rect::new(0, 42, 200, 10));
    assert!(rect(t, s.on_way).is_empty());
    // In the row the button keeps its natural width, 12 + 48 = 60, and its
    // height, 10 + 16 = 26; the slot - Fill - takes the rest of
    // the width: 200 - 60 - 4.
    assert_eq!(rect(t, s.go), Rect::new(140, 56, 60, 26));
    assert_eq!(rect(t, s.note), Rect::new(0, 56, 136, 26));
    assert_eq!(rect(t, s.last).origin.y, 86);
    assert_eq!(
        shown(t)
            .iter()
            .filter(|&&id| id == s.input || id == s.go)
            .count(),
        2
    );
}

#[test]
fn hidden_content_is_neither_focusable_nor_hit_and_keeps_its_value() {
    let mut s = screen();
    s.tree.set_visible(HELP, true).unwrap();
    s.tree.layout(VIEWPORT, &METRICS).unwrap();
    // Tab reaches the input (the only control showing), and typing fills it.
    assert_eq!(
        s.tree.handle(InputEvent::KeyDown(KeyCode::TAB)),
        Some(UiEvent::FocusChanged(Some(s.input)))
    );
    s.tree
        .handle(InputEvent::KeyDown(KeyCode::letter('a').unwrap()));
    assert_eq!(s.tree.value(NAME), Some("a"));
    let at = rect(&s.tree, s.input).center();
    // Hiding the group takes focus away at once, before any new layout, and
    // a tap where the input was hits nothing.
    s.tree.set_visible(HELP, false).unwrap();
    assert_eq!(s.tree.focused(), None);
    assert_eq!(s.tree.hit_test(at), Some(s.tree.root().unwrap()));
    assert_eq!(s.tree.control_at(at), None);
    for phase in [TouchPhase::Start, TouchPhase::End] {
        let touch = InputEvent::Touch {
            id: 0,
            x: at.x,
            y: at.y,
            phase,
        };
        assert_eq!(s.tree.handle(touch), None);
    }
    assert_eq!(
        s.tree.handle(InputEvent::KeyDown(KeyCode::TAB)),
        None,
        "nothing showing can take focus"
    );
    assert_eq!(
        s.tree.set_focus(Some(s.input)),
        Err(UiError::NotFocusable(s.input))
    );
    // The value stays, and shows again with the group.
    assert_eq!(s.tree.value(NAME), Some("a"));
    s.tree.set_visible(HELP, true).unwrap();
    s.tree.layout(VIEWPORT, &METRICS).unwrap();
    assert_eq!(s.tree.set_focus(Some(s.input)), Ok(()));
    assert_eq!(
        s.tree.control_at(rect(&s.tree, s.input).center()),
        Some(s.input)
    );
}

#[test]
fn a_press_on_a_control_that_stops_showing_is_cancelled() {
    let mut s = screen();
    s.tree.set_visible(GroupId(3), true).unwrap();
    s.tree.layout(VIEWPORT, &METRICS).unwrap();
    let at = rect(&s.tree, s.go).center();
    s.tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    s.tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    assert_eq!(s.tree.pressed(), Some(s.go));
    s.tree.set_visible(GroupId(3), false).unwrap();
    assert_eq!(s.tree.pressed(), None);
    assert_eq!(
        s.tree.handle(InputEvent::PointerButton {
            button: 0,
            down: false,
        }),
        None,
        "no action from a hidden button"
    );
}

#[test]
fn showing_or_hiding_marks_the_viewport_and_unknown_groups_are_refused() {
    let mut s = screen();
    s.tree.clear_dirty();
    s.tree.set_visible(READY, false).unwrap();
    assert!(
        s.tree.dirty_rects().is_empty(),
        "no change, nothing to repaint"
    );
    s.tree.set_visible(READY, true).unwrap();
    assert_eq!(s.tree.dirty_rects(), &[VIEWPORT]);
    s.tree.clear_dirty();
    assert_eq!(
        s.tree.set_visible(GroupId(9), true),
        Err(UiError::UnknownGroup(GroupId(9)))
    );
    assert!(s.tree.dirty_rects().is_empty());
    assert_eq!(s.tree.visible(GroupId(9)), None);
    assert_eq!(s.tree.find_group(HELP), Some(s.help));
}

#[test]
fn the_builder_refuses_a_slot_number_used_twice() {
    let mut t: Tree<8> = Tree::new();
    UiBuilder::slot(&mut t, NOTE).unwrap();
    assert_eq!(
        UiBuilder::slot(&mut t, NOTE),
        Err(UiError::DuplicateSlot(NOTE))
    );
    assert_eq!(t.len(), 1, "nothing was created on a refusal");
    // A child already in a group cannot go in another.
    let a = UiBuilder::text(&mut t, "a").unwrap();
    let group = UiBuilder::group(&mut t, HELP, a).unwrap();
    assert_eq!(
        UiBuilder::group(&mut t, GroupId(2), a),
        Err(UiError::AlreadyAttached(a))
    );
    // Nothing can be appended to a group.
    let c = UiBuilder::text(&mut t, "c").unwrap();
    assert_eq!(t.append(group, c), Err(UiError::NotAContainer(group)));
}

#[test]
fn groups_sharing_a_number_show_and_hide_together() {
    // One prop may show several elements: `{props.ready && <A/>}` and
    // `{props.ready ? <B/> : <C/>}` share the group number of `ready`.
    let mut t: Tree<8> = Tree::new();
    let a = UiBuilder::text(&mut t, "A").unwrap();
    let b = UiBuilder::text(&mut t, "B").unwrap();
    let c = UiBuilder::text(&mut t, "C").unwrap();
    let first = UiBuilder::group(&mut t, READY, a).unwrap();
    let second = UiBuilder::either(&mut t, READY, b, c).unwrap();
    let root = UiBuilder::column(&mut t, ui::Layout::new(), &[first, second]).unwrap();
    t.set_root(root).unwrap();
    let shown = |t: &Tree<8>| -> Vec<NodeId> { t.paint_order().map(|(id, _)| id).collect() };
    assert_eq!(shown(&t), [root, first, second, c]);
    t.set_visible(READY, true).unwrap();
    assert_eq!(shown(&t), [root, first, a, second, b]);
    assert_eq!(t.find_group(READY), Some(first), "the first created");
}

#[test]
fn a_session_wipe_hides_every_group_again() {
    let mut s = screen();
    assert!(s.tree.is_wiped());
    s.tree.set_visible(READY, true).unwrap();
    assert!(!s.tree.is_wiped(), "a group the application showed counts");
    s.tree.set_visible(HELP, true).unwrap();
    s.tree.layout(VIEWPORT, &METRICS).unwrap();
    s.tree.set_focus(Some(s.input)).unwrap();
    s.tree.wipe_session();
    assert_eq!(s.tree.visible(READY), Some(false));
    assert_eq!(s.tree.visible(HELP), Some(false));
    assert_eq!(
        s.tree.focused(),
        None,
        "focus does not stay on hidden input"
    );
    assert!(s.tree.is_wiped());
}

#[test]
fn groups_nest_and_may_be_the_root() {
    let mut t: Tree<8> = Tree::new();
    let text = UiBuilder::text(&mut t, "Deep").unwrap();
    let inner = UiBuilder::group(&mut t, GroupId(1), text).unwrap();
    let outer = UiBuilder::group(&mut t, GroupId(2), inner).unwrap();
    let after = UiBuilder::text(&mut t, "After").unwrap();
    let root = UiBuilder::column(&mut t, ui::Layout::new().gap(4), &[outer, after]).unwrap();
    t.set_root(root).unwrap();
    t.set_visible(GroupId(2), true).unwrap();
    t.layout(VIEWPORT, &METRICS).unwrap();
    // The outer group shows the inner, which shows nothing: no space.
    assert_eq!(t.get(after).unwrap().rect().origin.y, 0);
    t.set_visible(GroupId(1), true).unwrap();
    t.layout(VIEWPORT, &METRICS).unwrap();
    assert_eq!(t.get(text).unwrap().rect(), Rect::new(0, 0, 200, 10));
    assert_eq!(t.get(after).unwrap().rect().origin.y, 14);

    // A group at the root: what it shows fills the viewport.
    let mut u: Tree<8> = Tree::new();
    let a = UiBuilder::column(&mut u, ui::Layout::new(), &[]).unwrap();
    let b = UiBuilder::column(&mut u, ui::Layout::new(), &[]).unwrap();
    let root = UiBuilder::either(&mut u, READY, a, b).unwrap();
    u.set_root(root).unwrap();
    u.layout(VIEWPORT, &METRICS).unwrap();
    assert_eq!(u.get(b).unwrap().rect(), VIEWPORT);
    assert!(u.get(a).unwrap().rect().is_empty());
    assert_eq!(u.hit_test(Point::new(5, 5)), Some(b));
}

#[test]
fn a_fill_child_in_a_group_shares_the_row_like_any_fill_child() {
    let mut t: Tree<8> = Tree::new();
    let left = Tree::spacer(&mut t).unwrap();
    let right = Tree::spacer(&mut t).unwrap();
    let group = UiBuilder::group(&mut t, HELP, right).unwrap();
    let row = t.row(Style::new()).unwrap();
    t.append(row, left).unwrap();
    t.append(row, group).unwrap();
    t.set_root(row).unwrap();
    t.layout(Rect::new(0, 0, 100, 10), &METRICS).unwrap();
    assert_eq!(t.get(left).unwrap().rect().size.width, 100);
    t.set_visible(HELP, true).unwrap();
    t.layout(Rect::new(0, 0, 100, 10), &METRICS).unwrap();
    assert_eq!(t.get(left).unwrap().rect().size.width, 50);
    assert_eq!(t.get(right).unwrap().rect(), Rect::new(50, 0, 50, 10));
    assert_eq!(
        t.get(right).unwrap().style().width,
        Length::Fill,
        "the child keeps its own style"
    );
}

#[test]
fn focus_never_rests_on_a_control_a_new_group_hides() {
    // `group`: a focused input placed in a group, hidden at first. Focus
    // goes, and a key - pressed before any root is set, which would clear
    // focus itself - reaches no input.
    let mut t: Tree<8> = Tree::new();
    let input = Tree::text_input(&mut t, NAME, "Name").unwrap();
    t.set_focus(Some(input)).unwrap();
    UiBuilder::group(&mut t, HELP, input).unwrap();
    assert_eq!(t.focused(), None);
    assert_eq!(
        t.handle(InputEvent::KeyDown(KeyCode::letter('a').unwrap())),
        None
    );
    assert_eq!(t.value(NAME), Some(""));

    // `either`: a focused input as the child shown while visible - hidden
    // at first - loses focus; as the child shown otherwise, it keeps it.
    let mut u: Tree<8> = Tree::new();
    let hidden = Tree::text_input(&mut u, NAME, "Name").unwrap();
    let other = UiBuilder::text(&mut u, "Other").unwrap();
    u.set_focus(Some(hidden)).unwrap();
    UiBuilder::either(&mut u, READY, hidden, other).unwrap();
    assert_eq!(u.focused(), None);
    let shown = Tree::text_input(&mut u, FieldId(2), "Shown").unwrap();
    let another = UiBuilder::text(&mut u, "Another").unwrap();
    u.set_focus(Some(shown)).unwrap();
    UiBuilder::either(&mut u, GroupId(3), another, shown).unwrap();
    assert_eq!(u.focused(), Some(shown), "the child showing keeps focus");

    // `append`: focus still set at the append, the input appended into a
    // column a hidden group holds.
    let mut w: Tree<8> = Tree::new();
    let inner = w.column(Style::new()).unwrap();
    UiBuilder::group(&mut w, HELP, inner).unwrap();
    let field = Tree::text_input(&mut w, NAME, "Name").unwrap();
    w.set_focus(Some(field)).unwrap();
    assert_eq!(w.focused(), Some(field));
    w.append(inner, field).unwrap();
    assert_eq!(w.focused(), None);
    assert_eq!(
        w.handle(InputEvent::KeyDown(KeyCode::letter('a').unwrap())),
        None
    );
    assert_eq!(w.value(NAME), Some(""));
}
