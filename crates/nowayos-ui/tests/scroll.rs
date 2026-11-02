// SPDX-License-Identifier: MIT OR Apache-2.0
//! A scroll view: one child laid out at its natural height and
//! shown through the view, scrolled by Page Up and Page Down, by a mouse's
//! wheel, by a touch dragged on it and by focus moving onto a control it
//! hides; at most one
//! in a screen and never nested; back at the top after a session's wipe.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, NodeId, TextMetrics, Tree, UiError, UiEvent};

/// A title, then a scroll view filling the rest of a 200 x 200 screen,
/// holding ten buttons 30 high each (with a gap of 10: 390 high in all).
fn screen() -> (Tree<32>, NodeId, Vec<NodeId>) {
    let mut tree: Tree<32> = Tree::new();
    let title = tree.text("Title").unwrap();
    let mut buttons = Vec::new();
    for at in 0..10u16 {
        let button = UiBuilder::button(&mut tree, "Go", ActionId(at + 1)).unwrap();
        buttons.push(button);
    }
    let list = UiBuilder::column(&mut tree, Layout::new().gap(10), &buttons).unwrap();
    let view = UiBuilder::scroll_view(&mut tree, Layout::new(), list).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[title, view]).unwrap();
    tree.set_root(root).unwrap();
    // Buttons at least 30 high: the minimum target.
    let metrics = TextMetrics::new(6, 10).with_min_target(30);
    tree.layout(Rect::new(0, 0, 200, 200), &metrics).unwrap();
    (tree, view, buttons)
}

fn top(tree: &Tree<32>, id: NodeId) -> i64 {
    tree.get(id).unwrap().rect().top()
}

#[test]
fn the_view_fills_the_room_left_and_its_child_keeps_its_height() {
    let (tree, view, buttons) = screen();
    let view_rect = tree.get(view).unwrap().rect();
    // The title is 30 high (the minimum target applies only to controls:
    // a text is its line, 10); the view takes the rest.
    assert_eq!(view_rect.bottom(), 200);
    assert_eq!(tree.scroll_offset(), Some(0));
    assert_eq!(top(&tree, buttons[0]), view_rect.top());
    // The last button lies below the view, unscrolled.
    assert!(tree.get(buttons[9]).unwrap().rect().top() > 200);
}

#[test]
fn page_down_and_up_scroll_a_page_less_a_line_within_the_range() {
    let (mut tree, view, buttons) = screen();
    let view_height = tree.get(view).unwrap().rect().size.height;
    let page = view_height - 10;
    assert_eq!(tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN)), None);
    assert_eq!(tree.scroll_offset(), Some(page));
    assert_eq!(
        top(&tree, buttons[0]),
        i64::from(200 - view_height) - i64::from(page)
    );
    // No further than the child is taller than the view.
    for _ in 0..5 {
        tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
    }
    let most = 390 - view_height;
    assert_eq!(tree.scroll_offset(), Some(most));
    assert_eq!(tree.get(buttons[9]).unwrap().rect().bottom(), 200);
    for _ in 0..5 {
        tree.handle(InputEvent::KeyDown(KeyCode::PAGE_UP));
    }
    assert_eq!(tree.scroll_offset(), Some(0));
    // Only the view is marked for repainting by a scroll.
    tree.clear_dirty();
    tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
    let view_rect = tree.get(view).unwrap().rect();
    for area in tree.dirty_rects() {
        assert!(area.top() >= view_rect.top(), "{area:?}");
    }
}

#[test]
fn the_wheel_scrolls_three_lines_a_notch_within_the_range() {
    let (mut tree, view, _) = screen();
    let view_height = tree.get(view).unwrap().rect().size.height;
    // Wherever the pointer is: here over the title, outside the view.
    tree.handle(InputEvent::PointerMove { x: 5, y: 5 });
    // Towards the user scrolls down: three lines of 10 a notch.
    assert_eq!(tree.handle(InputEvent::Wheel { notches: -1 }), None);
    assert_eq!(tree.scroll_offset(), Some(30));
    tree.handle(InputEvent::Wheel { notches: -2 });
    assert_eq!(tree.scroll_offset(), Some(90));
    tree.handle(InputEvent::Wheel { notches: 1 });
    assert_eq!(tree.scroll_offset(), Some(60));
    // Never past either end.
    tree.handle(InputEvent::Wheel { notches: -127 });
    assert_eq!(tree.scroll_offset(), Some(390 - view_height));
    tree.handle(InputEvent::Wheel { notches: i8::MAX });
    assert_eq!(tree.scroll_offset(), Some(0));
}

#[test]
fn focus_moving_onto_a_hidden_control_scrolls_it_into_view() {
    let (mut tree, view, buttons) = screen();
    let view_rect = tree.get(view).unwrap().rect();
    tree.set_focus(Some(buttons[9])).unwrap();
    let last = tree.get(buttons[9]).unwrap().rect();
    assert_eq!(last.bottom(), view_rect.bottom());
    // Back up to the first with Tab's partner: it shows at the top.
    tree.set_focus(Some(buttons[0])).unwrap();
    assert_eq!(top(&tree, buttons[0]), view_rect.top());
    assert_eq!(tree.scroll_offset(), Some(0));
}

#[test]
fn a_touch_dragged_on_the_view_scrolls_it_and_presses_nothing() {
    let (mut tree, _, buttons) = screen();
    let start = tree.get(buttons[1]).unwrap().rect();
    let (x, y) = (
        i32::try_from(start.left()).unwrap() + 5,
        i32::try_from(start.top()).unwrap() + 5,
    );
    let touch = |y, phase| InputEvent::Touch { id: 0, x, y, phase };
    tree.handle(touch(y, TouchPhase::Start));
    assert_eq!(tree.pressed(), Some(buttons[1]));
    // Within the slop: still a press.
    tree.handle(touch(y - 5, TouchPhase::Move));
    assert_eq!(tree.scroll_offset(), Some(0));
    assert_eq!(tree.pressed(), Some(buttons[1]));
    // Past it: the press is cancelled and the view follows the finger.
    tree.handle(touch(y - 40, TouchPhase::Move));
    assert_eq!(tree.pressed(), None);
    assert_eq!(tree.scroll_offset(), Some(40));
    assert_eq!(tree.handle(touch(y - 40, TouchPhase::End)), None);
    // A tap still presses.
    let (mut tree, _, buttons) = screen();
    let rect = tree.get(buttons[0]).unwrap().rect();
    let (x, y) = (
        i32::try_from(rect.left()).unwrap() + 5,
        i32::try_from(rect.top()).unwrap() + 5,
    );
    let tap = |phase| InputEvent::Touch { id: 0, x, y, phase };
    tree.handle(tap(TouchPhase::Start));
    assert_eq!(
        tree.handle(tap(TouchPhase::End)),
        Some(UiEvent::Action(ActionId(1)))
    );
}

#[test]
fn a_control_scrolled_out_of_the_view_cannot_be_hit() {
    let (mut tree, view, buttons) = screen();
    tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
    // The first button now lies above the view, under the title.
    let first = tree.get(buttons[0]).unwrap().rect();
    assert!(first.bottom() <= tree.get(view).unwrap().rect().top());
    let inside_first = Point::new(
        i32::try_from(first.left()).unwrap() + 5,
        i32::try_from(first.top()).unwrap() + 5,
    );
    assert_ne!(tree.control_at(inside_first), Some(buttons[0]));
}

#[test]
fn a_screen_has_one_scroll_view_never_nested() {
    let mut tree: Tree<16> = Tree::new();
    let a = tree.text("A").unwrap();
    let inner = UiBuilder::column(&mut tree, Layout::new(), &[a]).unwrap();
    let first = UiBuilder::scroll_view(&mut tree, Layout::new(), inner).unwrap();
    let b = tree.text("B").unwrap();
    let other = UiBuilder::column(&mut tree, Layout::new(), &[b]).unwrap();
    assert_eq!(
        UiBuilder::scroll_view(&mut tree, Layout::new(), other),
        Err(UiError::SecondScrollView)
    );
    let outer = UiBuilder::column(&mut tree, Layout::new(), &[first]).unwrap();
    assert_eq!(
        UiBuilder::scroll_view(&mut tree, Layout::new(), outer),
        Err(UiError::NestedScrollView)
    );
}

#[test]
fn a_session_s_wipe_returns_the_view_to_the_top() {
    let (mut tree, _, _) = screen();
    tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
    assert_ne!(tree.scroll_offset(), Some(0));
    tree.wipe_session();
    assert_eq!(tree.scroll_offset(), Some(0));
}
