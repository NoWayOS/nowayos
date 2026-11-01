// SPDX-License-Identifier: MIT OR Apache-2.0
//! The theme's spacing tokens: a gap, padding or spacer named by
//! size is measured against the spacing the metrics carry - the light
//! theme's 4 to 32, the large-text theme's half again - each time the tree
//! is laid out, at the device scale like every length; a plain number
//! stays fixed.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{NodeId, NodeKind, Space, Spacing, Style, TextMetrics, Tree};

/// A column padded by `padding` with a gap of `gap` between two texts.
fn column(style: Style) -> (Tree<8>, NodeId, NodeId) {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.column(style).unwrap();
    tree.set_root(root).unwrap();
    let first = tree.add(NodeKind::Text("one"), Style::new()).unwrap();
    let second = tree.add(NodeKind::Text("two"), Style::new()).unwrap();
    tree.append(root, first).unwrap();
    tree.append(root, second).unwrap();
    (tree, first, second)
}

/// Where the first text starts, and the space between the two.
fn placed(tree: &mut Tree<8>, first: NodeId, second: NodeId, metrics: &TextMetrics) -> (i64, i64) {
    tree.layout(Rect::new(0, 0, 200, 100), metrics).unwrap();
    let one = tree.get(first).unwrap().rect();
    let two = tree.get(second).unwrap().rect();
    (one.top(), two.top() - one.bottom())
}

#[test]
fn every_token_measures_its_theme_s_spacing() {
    for (space, standard, large) in [
        (Space::Xs, 4, 6),
        (Space::Sm, 8, 12),
        (Space::Md, 16, 24),
        (Space::Lg, 24, 36),
        (Space::Xl, 32, 48),
    ] {
        assert_eq!(Spacing::STANDARD.of(space), standard);
        assert_eq!(Spacing::LARGE.of(space), large);
    }
    assert_eq!(TextMetrics::new(6, 10).spacing(), Spacing::STANDARD);
}

#[test]
fn a_named_gap_and_padding_follow_the_theme_and_the_scale() {
    let style = Style::new().padding_space(Space::Sm).gap_space(Space::Md);
    let (mut tree, first, second) = column(style);
    let metrics = TextMetrics::new(6, 10);
    assert_eq!(placed(&mut tree, first, second, &metrics), (8, 16));
    // The same tree laid out again in a theme that spaces more.
    let large = metrics.with_spacing(Spacing::LARGE);
    assert_eq!(placed(&mut tree, first, second, &large), (12, 24));
    // At device scale 2, every layout unit two pixels.
    let at_two = metrics.with_scale(2);
    assert_eq!(placed(&mut tree, first, second, &at_two), (16, 32));
}

#[test]
fn a_number_stays_fixed_whatever_the_theme() {
    let (mut tree, first, second) = column(Style::new().padding(8).gap(16));
    let large = TextMetrics::new(6, 10).with_spacing(Spacing::LARGE);
    assert_eq!(placed(&mut tree, first, second, &large), (8, 16));
}

#[test]
fn a_token_stands_in_place_of_its_number() {
    let style = Style::new()
        .padding(2)
        .gap(3)
        .padding_space(Space::Lg)
        .gap_space(Space::Xs);
    let (mut tree, first, second) = column(style);
    assert_eq!(
        placed(&mut tree, first, second, &TextMetrics::new(6, 10)),
        (24, 4)
    );
}

#[test]
fn a_spacer_s_token_is_its_size_along_the_column() {
    let mut tree: Tree<8> = Tree::new();
    let one = tree.text("one").unwrap();
    let gap = UiBuilder::spacer_space(&mut tree, Space::Xl).unwrap();
    let two = tree.text("two").unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[one, gap, two]).unwrap();
    tree.set_root(root).unwrap();
    for (metrics, expected) in [
        (TextMetrics::new(6, 10), 32),
        (TextMetrics::new(6, 10).with_spacing(Spacing::LARGE), 48),
    ] {
        tree.layout(Rect::new(0, 0, 200, 200), &metrics).unwrap();
        let first = tree.get(one).unwrap().rect();
        let second = tree.get(two).unwrap().rect();
        assert_eq!(second.top() - first.bottom(), expected);
    }
}

#[test]
fn the_builder_s_layout_names_the_tokens() {
    let layout = Layout::new().gap_space(Space::Md).padding_space(Space::Sm);
    assert_eq!(layout.gap_token(), Some(Space::Md));
    assert_eq!(layout.padding_token(), Some(Space::Sm));
    assert_eq!(Layout::new().gap(4).gap_token(), None);
    let mut tree: Tree<8> = Tree::new();
    let one = tree.text("one").unwrap();
    let two = tree.text("two").unwrap();
    let root = UiBuilder::column(&mut tree, layout, &[one, two]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 200, 100), &TextMetrics::new(6, 10))
        .unwrap();
    let first = tree.get(one).unwrap().rect();
    let second = tree.get(two).unwrap().rect();
    assert_eq!((first.top(), second.top() - first.bottom()), (8, 16));
}
