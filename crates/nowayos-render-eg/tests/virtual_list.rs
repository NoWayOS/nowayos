// SPDX-License-Identifier: MIT OR Apache-2.0
//! A virtual list (builder contract v18) of 10,000 items through a pool of
//! sixteen rows, as the golden image shows; and after any scroll, the
//! application binding the rows that come to show another item, moving
//! what was drawn and repainting what was marked gives exactly the pixels of
//! a full repaint.

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
use nowayos_ui::{ListId, NodeId, SlotId, Surface, Tree};
use proptest::prelude::*;

const LIST: ListId = ListId(1);
const POOL: u16 = 16;

/// Each row a card with an item's name: in a 320 x 240 screen.
fn screen(theme: &Theme<Rgb888>) -> Box<Tree<96>> {
    let mut tree: Box<Tree<96>> = Box::default();
    let t = &mut *tree;
    let rows: Vec<NodeId> = (0..POOL)
        .map(|r| {
            let name = UiBuilder::slot(t, SlotId(100 + r)).unwrap();
            UiBuilder::column_on(t, Surface::Card, Layout::new().padding(4), &[name]).unwrap()
        })
        .collect();
    let list = UiBuilder::virtual_list(t, Layout::new().padding(4), LIST, 40, &rows).unwrap();
    t.set_root(list).unwrap();
    t.layout(Rect::new(0, 0, 320, 240), &theme.metrics())
        .unwrap();
    t.set_list_count(LIST, 10_000).unwrap();
    tree
}

/// Binds each row whose item changed, as an application does.
fn bind(tree: &mut Tree<96>) {
    let window = tree.list_window(LIST).unwrap();
    for item in window.first..(window.first + window.pool).min(window.count) {
        let row = tree.list_row(LIST, item).unwrap();
        let text = format!("Item {item}");
        if tree.slot_text(SlotId(100 + row)) != Some(text.as_str()) {
            tree.set_slot(SlotId(100 + row), &text).unwrap();
        }
    }
}

fn full_frame(tree: &Tree<96>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

fn present(tree: &mut Tree<96>, fb: &mut Framebuffer, theme: &Theme<Rgb888>) {
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

#[test]
fn ten_thousand_items_scrolled_far_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme);
    bind(&mut tree);
    let view = tree.find_list(LIST).unwrap();
    tree.scroll_view_to(view, 0, 4_321 * 40 + 13).unwrap();
    bind(&mut tree);
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/virtual-list.png");
    check_snapshot(&full_frame(&tree, &theme), &golden).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn any_scroll_rebound_moved_and_repainted_equals_a_full_repaint(
        steps in prop::collection::vec(0u32..2_000, 1..6),
    ) {
        let theme = Theme::<Rgb888>::light();
        let mut tree = screen(&theme);
        bind(&mut tree);
        let mut fb = full_frame(&tree, &theme);
        tree.clear_dirty();
        let view = tree.find_list(LIST).unwrap();
        for y in steps {
            tree.scroll_view_to(view, 0, y).unwrap();
            bind(&mut tree);
            present(&mut tree, &mut fb, &theme);
        }
        prop_assert!(full_frame(&tree, &theme).diff(&fb).is_none());
    }
}
