// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scrolling everywhere (builder contract v17): several scroll views, one
//! inside another, down, across or both ways; a wheel or touch goes to the
//! innermost under the pointer that can still move, keys to the one
//! holding focus, and focus scrolls a control into view through every view
//! around it; a lifted touch flings on and slows on the runtime's clock; a
//! scrollbar shows where a view is and can be dragged; and a scroll moves
//! what is drawn, marking only what that exposes.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::scroll::Move;
use nowayos_ui::{ActionId, NodeId, ScrollAxes, TextMetrics, Tree};

/// Ten pixels a line; controls 30 high.
fn metrics() -> TextMetrics {
    TextMetrics::new(6, 10).with_min_target(30)
}

/// An outer view, 200 x 200, down: a 100-high inner view of twenty
/// buttons, then ten more buttons below it.
fn nested() -> (Tree<64>, NodeId, NodeId, Vec<NodeId>, Vec<NodeId>) {
    let mut tree: Tree<64> = Tree::new();
    let inner_buttons: Vec<NodeId> = (0..20u16)
        .map(|at| UiBuilder::button(&mut tree, "In", ActionId(at + 1)).unwrap())
        .collect();
    let inner_list = UiBuilder::column(&mut tree, Layout::new(), &inner_buttons).unwrap();
    let inner = UiBuilder::scroll_view(&mut tree, Layout::new().height(100), inner_list).unwrap();
    let outer_buttons: Vec<NodeId> = (0..10u16)
        .map(|at| UiBuilder::button(&mut tree, "Out", ActionId(at + 100)).unwrap())
        .collect();
    let mut children = vec![inner];
    children.extend(&outer_buttons);
    let list = UiBuilder::column(&mut tree, Layout::new(), &children).unwrap();
    let outer = UiBuilder::scroll_view(&mut tree, Layout::new(), list).unwrap();
    tree.set_root(outer).unwrap();
    tree.layout(Rect::new(0, 0, 200, 200), &metrics()).unwrap();
    (tree, outer, inner, inner_buttons, outer_buttons)
}

fn wheel(tree: &mut Tree<64>, at: Point, notches: i8) {
    tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    tree.handle(InputEvent::Wheel { notches });
}

#[test]
fn views_stand_one_inside_another_each_clipping_its_child() {
    let (tree, outer, inner, inner_buttons, _) = nested();
    assert_eq!(tree.scroll_views().collect::<Vec<_>>(), [outer, inner]);
    // Twenty 30-high buttons in a 100-high view: 500 to scroll.
    assert_eq!(tree.scroll_range(inner), Some((0, 500)));
    // The inner view's last button is seen through both views: nothing.
    let last = tree.get(inner_buttons[19]).unwrap();
    let clip = tree.scroll_clip(last).unwrap();
    assert!(clip.intersection(&last.rect()).is_none());
    // Its first, through the inner view.
    let first = tree.get(inner_buttons[0]).unwrap();
    assert_eq!(
        tree.scroll_clip(first),
        Some(tree.get(inner).unwrap().content())
    );
}

#[test]
fn a_wheel_moves_the_innermost_view_under_the_pointer_that_can_still_move() {
    let (mut tree, outer, inner, _, _) = nested();
    let on_inner = Point::new(50, 50);
    // Three lines a notch: 30 pixels.
    wheel(&mut tree, on_inner, -1);
    assert_eq!(tree.scroll_position(inner), Some((0, 30)));
    assert_eq!(tree.scroll_position(outer), Some((0, 0)));
    // At the inner view's end, the outer one moves.
    tree.scroll_view_to(inner, 0, 500).unwrap();
    wheel(&mut tree, on_inner, -1);
    assert_eq!(tree.scroll_position(inner), Some((0, 500)));
    assert_eq!(tree.scroll_position(outer), Some((0, 30)));
    // Below the inner view, only the outer.
    wheel(&mut tree, Point::new(50, 150), 1);
    assert_eq!(tree.scroll_position(outer), Some((0, 0)));
}

#[test]
fn keys_scroll_the_view_holding_focus_and_focus_is_revealed_through_every_view() {
    let (mut tree, outer, inner, inner_buttons, outer_buttons) = nested();
    // Focus on a button the inner view hides, itself below the outer
    // view's edge once the inner one is scrolled: both move.
    tree.set_focus(Some(inner_buttons[15])).unwrap();
    let shown = tree.get(inner_buttons[15]).unwrap().rect();
    let inner_seen = tree.get(inner).unwrap().content();
    assert!(
        inner_seen.contains_rect(&shown),
        "{shown:?} in {inner_seen:?}"
    );
    // Page Down scrolls the inner view, holding focus.
    let before = tree.scroll_position(inner).unwrap();
    tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
    assert!(tree.scroll_position(inner).unwrap().1 > before.1);
    // Focus on the outer view's last button: the outer view moves.
    tree.set_focus(Some(outer_buttons[9])).unwrap();
    let last = tree.get(outer_buttons[9]).unwrap().rect();
    assert!(last.bottom() <= 200);
    assert!(tree.scroll_position(outer).unwrap().1 > 0);
}

/// A row of twelve 40-wide buttons in a view 100 wide that scrolls
/// across, and a scroll view both ways below it.
fn across() -> (Tree<64>, NodeId, NodeId) {
    let mut tree: Tree<64> = Tree::new();
    let buttons: Vec<NodeId> = (0..12u16)
        .map(|at| {
            let button = UiBuilder::button(&mut tree, "Go", ActionId(at + 1)).unwrap();
            tree.set_style(
                button,
                nowayos_ui::Style::new().width(nowayos_ui::Length::Fixed(40)),
            )
            .unwrap();
            button
        })
        .collect();
    let row = UiBuilder::row(&mut tree, Layout::new(), &buttons).unwrap();
    let view = UiBuilder::scroll_view_axes(
        &mut tree,
        Layout::new().width(100),
        ScrollAxes::Horizontal,
        row,
    )
    .unwrap();
    let big = UiBuilder::spacer(&mut tree, Some(400)).unwrap();
    let wide = UiBuilder::row(&mut tree, Layout::new().width(400).height(300), &[big]).unwrap();
    let both = UiBuilder::scroll_view_axes(
        &mut tree,
        Layout::new().width(100).height(80),
        ScrollAxes::Both,
        wide,
    )
    .unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[view, both]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 300, 300), &metrics()).unwrap();
    (tree, view, both)
}

#[test]
fn a_view_across_keeps_its_child_s_width_and_shift_turns_the_wheel_across() {
    let (mut tree, view, both) = across();
    assert_eq!(tree.scroll_range(view), Some((380, 0)));
    let (wide, tall) = tree.scroll_range(both).unwrap();
    assert_eq!(wide, 300);
    assert!(tall > 0, "{tall}");
    let on = Point::new(20, 10);
    tree.handle(InputEvent::PointerMove { x: on.x, y: on.y });
    tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    tree.handle(InputEvent::Wheel { notches: -1 });
    tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    assert_eq!(tree.scroll_position(view), Some((30, 0)));
}

#[test]
fn a_touch_drags_its_view_both_ways_and_a_lifted_one_flings_on_and_stops() {
    let (mut tree, _, both) = across();
    let seen = tree.get(both).unwrap().content();
    let start = Point::new(
        i32::try_from(seen.left()).unwrap() + 50,
        i32::try_from(seen.top()).unwrap() + 40,
    );
    let touch = |tree: &mut Tree<64>, at: Point, phase| {
        tree.handle(InputEvent::Touch {
            id: 0,
            x: at.x,
            y: at.y,
            phase,
        });
    };
    touch(&mut tree, start, TouchPhase::Start);
    // Past the slop, the view follows the finger exactly.
    touch(
        &mut tree,
        Point::new(start.x - 30, start.y - 20),
        TouchPhase::Move,
    );
    assert_eq!(tree.scroll_position(both), Some((30, 20)));
    // Moving quickly across: 30 pixels in 16 ms, twice.
    tree.advance(16);
    touch(
        &mut tree,
        Point::new(start.x - 60, start.y - 20),
        TouchPhase::Move,
    );
    tree.advance(16);
    touch(
        &mut tree,
        Point::new(start.x - 60, start.y - 20),
        TouchPhase::End,
    );
    let lifted = tree.scroll_position(both).unwrap();
    // It goes on, slower each step, and stops.
    let mut steps = 0;
    let mut last = lifted;
    let mut gaps = Vec::new();
    while tree.advance(16) && steps < 200 {
        let now = tree.scroll_position(both).unwrap();
        gaps.push(now.0 - last.0);
        last = now;
        steps += 1;
    }
    assert!(steps > 2 && steps < 200, "{steps} steps");
    assert!(last.0 > lifted.0, "{last:?} after {lifted:?}");
    assert!(gaps.windows(2).all(|pair| pair[1] <= pair[0]), "{gaps:?}");
    // A new touch stops a fling at once.
    touch(&mut tree, start, TouchPhase::Start);
    let held = tree.scroll_position(both).unwrap();
    tree.advance(16);
    assert_eq!(tree.scroll_position(both).unwrap(), held);
}

#[test]
fn a_scrollbar_shows_where_the_view_is_and_its_thumb_can_be_dragged() {
    let (mut tree, _, inner, _, _) = nested();
    let bars = tree.scrollbars(inner);
    assert!(bars.horizontal.is_none());
    let bar = bars.vertical.unwrap();
    // A 100-high view of 600: a thumb a sixth of its track - 16, so the
    // least, 24 - at the top.
    assert_eq!(bar.track.size.height, 100);
    assert_eq!(bar.thumb.size.height, 24);
    assert_eq!(bar.thumb.top(), bar.track.top());
    // Dragged to the bottom of its track with the mouse: the view's end.
    let grab = Point::new(
        i32::try_from(bar.thumb.left()).unwrap() + 1,
        i32::try_from(bar.thumb.top()).unwrap() + 2,
    );
    tree.handle(InputEvent::PointerMove {
        x: grab.x,
        y: grab.y,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    tree.handle(InputEvent::PointerMove {
        x: grab.x,
        y: grab.y + 100,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: false,
    });
    assert_eq!(tree.scroll_position(inner), Some((0, 500)));
    let moved = tree.scrollbars(inner).vertical.unwrap();
    assert_eq!(moved.thumb.bottom(), moved.track.bottom());
    // Nothing was pressed.
    assert_eq!(tree.pressed(), None);
}

#[test]
fn a_scroll_moves_what_is_drawn_and_marks_only_the_strip_it_exposes() {
    let (mut tree, _, inner, _, _) = nested();
    tree.clear_dirty();
    tree.scroll_view_to(inner, 0, 25).unwrap();
    let seen = tree.get(inner).unwrap().content();
    // All but the strip its scrollbar runs in moves up 25 rows.
    let track = tree.scrollbars(inner).vertical.unwrap().track;
    let moving = Rect::from_edges(seen.left(), seen.top(), track.left(), seen.bottom());
    assert_eq!(
        tree.moves(),
        &[Move {
            area: moving,
            dx: 0,
            dy: -25
        }]
    );
    // The 25 rows exposed at the bottom, and its scrollbar's strip.
    let exposed = Rect::from_edges(
        moving.left(),
        moving.bottom() - 25,
        moving.right(),
        moving.bottom(),
    );
    assert!(
        tree.dirty_rects().contains(&track),
        "{:?}",
        tree.dirty_rects()
    );
    let marked = tree.dirty_rects();
    assert!(
        marked.iter().any(|rect| rect.contains_rect(&exposed)),
        "{marked:?}"
    );
    let area: u64 = marked
        .iter()
        .map(|rect| u64::from(rect.size.width) * u64::from(rect.size.height))
        .sum();
    assert!(
        area < u64::from(seen.size.width) * u64::from(seen.size.height),
        "{marked:?}"
    );
    tree.clear_dirty();
    assert!(tree.moves().is_empty());
}

#[test]
fn a_view_with_something_drawn_over_it_is_repainted_rather_than_moved() {
    let mut tree: Tree<32> = Tree::new();
    let buttons: Vec<NodeId> = (0..10u16)
        .map(|at| UiBuilder::button(&mut tree, "Go", ActionId(at + 1)).unwrap())
        .collect();
    let list = UiBuilder::column(&mut tree, Layout::new(), &buttons).unwrap();
    let view = UiBuilder::scroll_view(&mut tree, Layout::new(), list).unwrap();
    // A note standing over the view.
    let note = tree.text("Over").unwrap();
    let root = tree.stack(nowayos_ui::Style::new()).unwrap();
    tree.append(root, view).unwrap();
    tree.append(root, note).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 200, 100), &metrics()).unwrap();
    tree.clear_dirty();
    tree.scroll_view_to(view, 0, 10).unwrap();
    assert!(tree.moves().is_empty());
    assert_eq!(tree.dirty_rects(), &[tree.get(view).unwrap().content()]);
}
