// SPDX-License-Identifier: MIT OR Apache-2.0
//! Large screens (builder contract v18): a 2,000-node screen builds and
//! lays out; a virtual list shows 10,000 items through a pool of rows
//! built once, rebinding only the rows that come to show another item as
//! it scrolls; and a change inside a scroll view lays out only that view -
//! the times measured.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::print_stderr,
    reason = "test code: a failed check panics, and tests index, compute and report times"
)]

use std::time::Instant;

use nowayos_display::{Point, Rect};
use nowayos_input::InputEvent;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, ListId, NodeId, SlotId, TextMetrics, TextStyle, Tree, UiEvent};

fn metrics() -> TextMetrics {
    TextMetrics::new(8, 16).with_min_target(32)
}

type Big = Tree<2200, 1024>;

/// 1,900 buttons in rows of ten inside a scroll view, under a long text.
fn big() -> (Box<Big>, NodeId) {
    let mut tree: Box<Big> = Box::default();
    let t = &mut *tree;
    let mut rows = Vec::new();
    for r in 0..190u16 {
        let cells: Vec<NodeId> = (0..10u16)
            .map(|c| UiBuilder::button(t, "Go", ActionId(r * 10 + c + 1)).unwrap())
            .collect();
        rows.push(UiBuilder::row(t, Layout::new().gap(4), &cells).unwrap());
    }
    let note = UiBuilder::long_text(t, SlotId(1), 1000, TextStyle::default()).unwrap();
    let mut children = vec![note];
    children.extend(rows);
    let column = UiBuilder::column(t, Layout::new().gap(4), &children).unwrap();
    let view = UiBuilder::scroll_view(t, Layout::new(), column).unwrap();
    let root = UiBuilder::column(t, Layout::new().padding(8), &[view]).unwrap();
    t.set_root(root).unwrap();
    (tree, view)
}

#[test]
fn a_two_thousand_node_screen_builds_lays_out_and_relays_out_only_its_view() {
    // A tree is built where it is declared: a thread with room for one.
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(two_thousand_nodes)
        .unwrap()
        .join()
        .unwrap();
}

fn two_thousand_nodes() {
    eprintln!(
        "a node is {} bytes; a Tree<2200, 1024> {} bytes",
        std::mem::size_of::<nowayos_ui::Node>(),
        std::mem::size_of::<Big>()
    );
    let (mut tree, view) = big();
    assert!(tree.len() > 2000, "{} nodes", tree.len());
    let started = Instant::now();
    tree.layout(Rect::new(0, 0, 1920, 1080), &metrics())
        .unwrap();
    let whole = started.elapsed();
    assert!(tree.scroll_range(view).unwrap().1 > 1080);
    // A note of a few lines more: only the view is laid out again.
    tree.clear_dirty();
    let started = Instant::now();
    tree.set_slot(SlotId(1), "One line,\nthen another,\nand a third.")
        .unwrap();
    let part = started.elapsed();
    let seen = tree.get(view).unwrap().content();
    assert!(
        tree.dirty_rects()
            .iter()
            .all(|rect| seen.contains_rect(rect))
    );
    eprintln!("2,000 nodes: whole layout {whole:?}; the view alone, for a longer note, {part:?}");
}

const LIST: ListId = ListId(1);
/// Row r's text is slot 100 + r.
const POOL: u16 = 16;

/// A virtual list of 32-high rows - each a text bound to its own slot - in
/// a 400 x 320 screen: ten rows show.
fn listed(count: u32) -> Tree<64> {
    let mut tree: Tree<64> = Tree::new();
    let rows: Vec<NodeId> = (0..POOL)
        .map(|r| UiBuilder::slot(&mut tree, SlotId(100 + r)).unwrap())
        .collect();
    let list = UiBuilder::virtual_list(&mut tree, Layout::new(), LIST, 32, &rows).unwrap();
    tree.set_root(list).unwrap();
    tree.layout(Rect::new(0, 0, 400, 320), &metrics()).unwrap();
    tree.set_list_count(LIST, count).unwrap();
    tree
}

/// Binds every row the window shows, as an application does: item `i`'s
/// text is "Item i".
fn bind(tree: &mut Tree<64>) -> usize {
    let window = tree.list_window(LIST).unwrap();
    let mut bound = 0;
    for item in window.first..(window.first + window.pool).min(window.count) {
        let row = tree.list_row(LIST, item).unwrap();
        let text = format!("Item {item}");
        if tree.slot_text(SlotId(100 + row)) != Some(text.as_str()) {
            tree.set_slot(SlotId(100 + row), &text).unwrap();
            bound += 1;
        }
    }
    bound
}

#[test]
fn ten_thousand_items_scroll_through_a_pool_of_sixteen_rows() {
    let mut tree = listed(10_000);
    assert_eq!(bind(&mut tree), 16);
    let view = tree.find_list(LIST).unwrap();
    // As tall as all the items.
    assert_eq!(tree.scroll_range(view), Some((0, 10_000 * 32 - 320)));
    // Item 3 at three rows down, shown by row 3.
    let row = tree.find_slot(SlotId(103)).unwrap();
    assert_eq!(tree.get(row).unwrap().rect().top(), 3 * 32);
    // A notch of the wheel: 48 pixels down, still in the margin - no
    // window moves.
    tree.handle(InputEvent::PointerMove { x: 50, y: 50 });
    assert_eq!(tree.handle(InputEvent::Wheel { notches: -1 }), None);
    // Further down: the window moves, the event says so, and only the rows
    // that came to show another item are bound again.
    let mut moved = None;
    for _ in 0..10 {
        if let Some(event) = tree.handle(InputEvent::Wheel { notches: -1 }) {
            moved = Some(event);
        }
    }
    assert_eq!(moved, Some(UiEvent::ListMoved(LIST)));
    let window = tree.list_window(LIST).unwrap();
    assert!(window.first > 0);
    let rebound = bind(&mut tree);
    assert!(rebound > 0 && rebound < 16, "{rebound} rows bound again");
    // Every item in the window is where its index puts it, by its row.
    for item in window.first..window.first + window.pool {
        let row = tree.list_row(LIST, item).unwrap();
        let node = tree.find_slot(SlotId(100 + row)).unwrap();
        let rect = tree.get(node).unwrap().rect();
        let (_, y) = tree.scroll_position(view).unwrap();
        assert_eq!(
            rect.top(),
            i64::from(item) * 32 - i64::from(y),
            "item {item}"
        );
        assert_eq!(
            tree.slot_text(SlotId(100 + row)),
            Some(format!("Item {item}").as_str())
        );
    }
    // To the end: the last item shows, at the view's bottom.
    tree.scroll_view_to(view, 0, u32::MAX).unwrap();
    assert_eq!(tree.take_list_moved(), Some(LIST));
    bind(&mut tree);
    let last = tree.list_row(LIST, 9_999).unwrap();
    let node = tree.find_slot(SlotId(100 + last)).unwrap();
    assert_eq!(tree.get(node).unwrap().rect().bottom(), 320);
    assert_eq!(tree.slot_text(SlotId(100 + last)), Some("Item 9999"));
    // The tree is the same size, whatever the count.
    assert_eq!(
        std::mem::size_of_val(&tree),
        std::mem::size_of::<Tree<64>>()
    );
}

#[test]
fn a_short_list_hides_the_rows_it_does_not_need_and_grows_in_place() {
    let mut tree = listed(3);
    bind(&mut tree);
    let shown: Vec<bool> = (0..POOL)
        .map(|r| {
            let node = tree.find_slot(SlotId(100 + r)).unwrap();
            !tree.get(node).unwrap().rect().is_empty()
        })
        .collect();
    assert_eq!(shown.iter().filter(|&&s| s).count(), 3);
    assert_eq!(tree.list_row(LIST, 3), None);
    // More items: laid out again where it stands.
    tree.clear_dirty();
    tree.set_list_count(LIST, 5).unwrap();
    assert_eq!(tree.list_row(LIST, 4), Some(4));
    let view = tree.find_list(LIST).unwrap();
    assert!(!tree.dirty_rects().is_empty());
    assert!(
        tree.dirty_rects()
            .iter()
            .all(|rect| tree.get(view).unwrap().rect().contains_rect(rect))
    );
    assert_eq!(
        tree.set_list_count(ListId(9), 1),
        Err(nowayos_ui::UiError::UnknownList(ListId(9)))
    );
    let _ = Point::ORIGIN;
}

/// A screen being built sets its list's count and binds its rows before
/// it has a root, as an application does.
#[test]
fn a_list_is_counted_and_bound_before_its_screen_has_a_root() {
    let mut tree: Tree<64> = Tree::new();
    let rows: Vec<NodeId> = (0..POOL)
        .map(|r| UiBuilder::slot(&mut tree, SlotId(100 + r)).unwrap())
        .collect();
    let list = UiBuilder::virtual_list(&mut tree, Layout::new(), LIST, 32, &rows).unwrap();
    tree.set_list_count(LIST, 40).unwrap();
    assert_eq!(tree.list_window(LIST).unwrap().count, 40);
    tree.set_root(list).unwrap();
    tree.layout(Rect::new(0, 0, 400, 320), &metrics()).unwrap();
    assert_eq!(bind(&mut tree), 16);
}
