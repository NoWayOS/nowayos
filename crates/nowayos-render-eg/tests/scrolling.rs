// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scrolling everywhere (builder contract v17): views one inside another,
//! down, across and both ways, with scrollbars, as the golden image shows;
//! and after any scrolls, moving what was drawn ([`Tree::moves`]) and
//! repainting only what the tree marks gives exactly the pixels of a full
//! repaint.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_dirty, to_eg};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, NodeId, ScrollAxes, Surface, Tree};
use proptest::prelude::*;

type Screen = Tree<96>;

/// A card holding a view that scrolls down through numbered buttons -
/// inside it, a row of buttons scrolling across - beside a view that
/// scrolls both ways over a grid of buttons; all in a 360 x 240 screen.
fn screen(theme: &Theme<Rgb888>) -> (Box<Screen>, [NodeId; 3]) {
    let mut tree: Box<Screen> = Box::default();
    let t = &mut *tree;
    const LABELS: [&str; 12] = [
        "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12",
    ];
    let across: Vec<NodeId> = LABELS[..8]
        .iter()
        .zip(1u16..)
        .map(|(label, at)| UiBuilder::button(t, label, ActionId(at)).unwrap())
        .collect();
    let row = UiBuilder::row(t, Layout::new().gap(8), &across).unwrap();
    let sideways =
        UiBuilder::scroll_view_axes(t, Layout::new(), ScrollAxes::Horizontal, row).unwrap();
    let mut down = vec![sideways];
    for (label, at) in LABELS.iter().zip(20u16..) {
        down.push(UiBuilder::button(t, label, ActionId(at)).unwrap());
    }
    let list = UiBuilder::column(t, Layout::new().gap(6), &down).unwrap();
    let tall = UiBuilder::scroll_view(t, Layout::new(), list).unwrap();
    let card = UiBuilder::column_on(
        t,
        Surface::Card,
        Layout::new().padding(8).width(180),
        &[tall],
    )
    .unwrap();
    let mut rows = Vec::new();
    for r in 0..6u16 {
        let cells: Vec<NodeId> = LABELS[..6]
            .iter()
            .zip(0u16..)
            .map(|(label, c)| UiBuilder::button(t, label, ActionId(100 + r * 10 + c)).unwrap())
            .collect();
        rows.push(UiBuilder::row(t, Layout::new().gap(6), &cells).unwrap());
    }
    let grid = UiBuilder::column(t, Layout::new().gap(6), &rows).unwrap();
    let both = UiBuilder::scroll_view_axes(t, Layout::new(), ScrollAxes::Both, grid).unwrap();
    let root = UiBuilder::row(t, Layout::new().gap(8).padding(8), &[card, both]).unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 360, 240), &theme.metrics())
        .unwrap();
    (tree, [tall, sideways, both])
}

fn full_frame(tree: &Screen, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

/// Moves what the tree says moved, then repaints what it marked, as a UI
/// PD does.
fn present(tree: &mut Screen, fb: &mut Framebuffer, theme: &Theme<Rgb888>) {
    for made in tree.moves().to_vec() {
        fb.move_area(&to_eg(made.area), made.dx, made.dy);
    }
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(tree, fb, theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

#[test]
fn views_inside_views_scroll_with_their_scrollbars_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let (mut tree, [tall, sideways, both]) = screen(&theme);
    for view in [tall, sideways, both] {
        let (x, y) = tree.scroll_range(view).unwrap();
        assert!(x > 0 || y > 0, "{view:?} cannot scroll");
    }
    tree.scroll_view_to(tall, 0, 40).unwrap();
    tree.scroll_view_to(sideways, 30, 0).unwrap();
    tree.scroll_view_to(both, 25, 35).unwrap();
    check_snapshot(&full_frame(&tree, &theme), &golden("scrolling.png")).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn any_scrolls_moved_and_repainted_equal_a_full_repaint(
        steps in prop::collection::vec((0usize..3, 0u32..400, 0u32..400), 1..6),
        each in any::<bool>(),
    ) {
        let theme = Theme::<Rgb888>::light();
        let (mut tree, views) = screen(&theme);
        let mut fb = full_frame(&tree, &theme);
        tree.clear_dirty();
        for (view, x, y) in steps {
            tree.scroll_view_to(views[view], x, y).unwrap();
            // Presented after each scroll, or once after them all.
            if each {
                present(&mut tree, &mut fb, &theme);
            }
        }
        present(&mut tree, &mut fb, &theme);
        prop_assert!(full_frame(&tree, &theme).diff(&fb).is_none());
    }
}
