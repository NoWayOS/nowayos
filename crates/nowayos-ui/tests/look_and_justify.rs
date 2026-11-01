// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a screen's style declares, laid out: a container's children
//! placed along its main axis as `justify` says when they leave room over,
//! a node kept within its least and greatest width, and the builder's
//! `Layout` carrying sizes, alignment, justification and a look onto the
//! node's style.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{Align, Colour, Justify, Length, Look, NodeId, Style, TextMetrics, Tree};

/// A row 200 wide of three fixed children 20 wide, with a gap of 10.
fn row(justify: Justify) -> (Tree<8>, [NodeId; 3]) {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.row(Style::new().gap(10).justify(justify)).unwrap();
    tree.set_root(root).unwrap();
    let mut ids = [root; 3];
    for id in &mut ids {
        *id = tree.spacer().unwrap();
        tree.set_style(*id, Style::new().size(20, 20)).unwrap();
        tree.append(root, *id).unwrap();
    }
    tree.layout(Rect::new(0, 0, 200, 50), &TextMetrics::new(6, 10))
        .unwrap();
    (tree, ids)
}

fn lefts(tree: &Tree<8>, ids: [NodeId; 3]) -> [i64; 3] {
    ids.map(|id| tree.get(id).unwrap().rect().left())
}

#[test]
fn justify_puts_the_room_over_where_it_says() {
    // 3 x 20 + 2 x 10 = 80: 120 over.
    assert_eq!(lefts_of(Justify::Start), [0, 30, 60]);
    assert_eq!(lefts_of(Justify::Center), [60, 90, 120]);
    assert_eq!(lefts_of(Justify::End), [120, 150, 180]);
    // 120 shared between two gaps: 60 more each.
    assert_eq!(lefts_of(Justify::SpaceBetween), [0, 90, 180]);
}

fn lefts_of(justify: Justify) -> [i64; 3] {
    let (tree, ids) = row(justify);
    lefts(&tree, ids)
}

#[test]
fn a_child_that_fills_takes_the_room_whatever_justify_says() {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.row(Style::new().justify(Justify::End)).unwrap();
    tree.set_root(root).unwrap();
    let fixed = tree.spacer().unwrap();
    tree.set_style(fixed, Style::new().size(20, 20)).unwrap();
    let fill = tree.spacer().unwrap();
    tree.append(root, fixed).unwrap();
    tree.append(root, fill).unwrap();
    tree.layout(Rect::new(0, 0, 200, 50), &TextMetrics::new(6, 10))
        .unwrap();
    assert_eq!(tree.get(fixed).unwrap().rect().left(), 0);
}

#[test]
fn space_between_shares_an_odd_room_a_pixel_at_a_time() {
    let mut tree: Tree<8> = Tree::new();
    let root = tree
        .row(Style::new().justify(Justify::SpaceBetween))
        .unwrap();
    tree.set_root(root).unwrap();
    let mut ids = Vec::new();
    for _ in 0..4 {
        let id = tree.spacer().unwrap();
        tree.set_style(id, Style::new().size(10, 10)).unwrap();
        tree.append(root, id).unwrap();
        ids.push(id);
    }
    // 101 - 40 = 61 over, three gaps: 21, 20, 20.
    tree.layout(Rect::new(0, 0, 101, 20), &TextMetrics::new(6, 10))
        .unwrap();
    let lefts: Vec<i64> = ids
        .iter()
        .map(|&id| tree.get(id).unwrap().rect().left())
        .collect();
    assert_eq!(lefts, [0, 31, 61, 91]);
}

#[test]
fn a_width_stays_within_its_least_and_greatest() {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.column(Style::new()).unwrap();
    tree.set_root(root).unwrap();
    // Stretched across 300, but at most 120.
    let narrow = tree.column(Style::new().max_width(120)).unwrap();
    // Centred, and at least 80 though it holds nothing.
    let wide = tree.column(Style::new().min_width(80)).unwrap();
    let centred = tree.column(Style::new().align(Align::Center)).unwrap();
    tree.append(root, narrow).unwrap();
    tree.append(root, centred).unwrap();
    tree.append(centred, wide).unwrap();
    tree.layout(Rect::new(0, 0, 300, 200), &TextMetrics::new(6, 10))
        .unwrap();
    let narrow = tree.get(narrow).unwrap().rect();
    assert_eq!((narrow.left(), narrow.size.width), (0, 120));
    let wide = tree.get(wide).unwrap().rect();
    assert_eq!((wide.left(), wide.size.width), (110, 80));
    // The least wins over the greatest, as in CSS.
    let both = Style::new().min_width(50).max_width(30);
    let mut tree: Tree<8> = Tree::new();
    let root = tree.row(Style::new()).unwrap();
    tree.set_root(root).unwrap();
    let child = tree.column(both).unwrap();
    tree.append(root, child).unwrap();
    tree.layout(Rect::new(0, 0, 300, 100), &TextMetrics::new(6, 10))
        .unwrap();
    assert_eq!(tree.get(child).unwrap().rect().size.width, 50);
}

#[test]
fn widths_are_layout_units_at_the_device_scale() {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.column(Style::new()).unwrap();
    tree.set_root(root).unwrap();
    let child = tree.column(Style::new().max_width(100)).unwrap();
    tree.append(root, child).unwrap();
    tree.layout(
        Rect::new(0, 0, 600, 200),
        &TextMetrics::new(6, 10).with_scale(2),
    )
    .unwrap();
    assert_eq!(tree.get(child).unwrap().rect().size.width, 200);
}

#[test]
fn the_builder_s_layout_carries_sizes_alignment_justification_and_a_look() {
    let look = Look::NONE
        .fill(Colour::Input)
        .border(Colour::InputBorder)
        .border_width(2)
        .radius(8);
    let layout = Layout::new()
        .width(160)
        .height(40)
        .min_width(100)
        .max_width(200)
        .align(Align::Center)
        .justify(Justify::SpaceBetween)
        .look(look);
    let style = layout.style();
    assert_eq!(style.width, Length::Fixed(160));
    assert_eq!(style.height, Length::Fixed(40));
    assert_eq!((style.min_width, style.max_width), (100, Some(200)));
    assert_eq!(style.align, Align::Center);
    assert_eq!(style.justify, Justify::SpaceBetween);
    assert_eq!(style.look, look);
    assert!(look.draws() && look.has_border());
    assert!(!Look::NONE.draws());
    // A plain layout is what it was: stretched, from the start, no look.
    let plain = Layout::new().style();
    assert_eq!(plain, Style::new());
    // A button's look rides on its node.
    let mut tree: Tree<8> = Tree::new();
    let button = UiBuilder::button_look(
        &mut tree,
        "Pay",
        nowayos_ui::ActionId(1),
        nowayos_ui::ButtonVariant::Primary,
        Look::NONE.fill(Colour::Error).text(Colour::Background),
    )
    .unwrap();
    assert_eq!(
        tree.get(button).unwrap().style().look.fill,
        Some(Colour::Error)
    );
}
