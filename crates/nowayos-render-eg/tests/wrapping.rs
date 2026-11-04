// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text set on several lines, drawn: a notice wrapped onto its
//! lines on its surface, and a slot beside a button wrapped in what the
//! button leaves - in each theme, matching its golden image, every line
//! shown whole, and a dirty repaint after the slot changes giving exactly
//! the pixels of a full one.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_dirty};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::{
    ActionId, Align, NodeKind, SlotId, Style, Surface, TextRole, TextStyle, Tone, Tree,
};

const NOTE: SlotId = SlotId::new(1);

/// A synthetic screen: a title, a caution notice whose text wraps onto up
/// to four lines, and a row of a button and a slot wrapping onto up to
/// three, laid out in `width` x `height` with `theme`'s metrics.
fn screen(theme: &Theme<Rgb888>, width: u32, height: u32) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new().padding(12).gap(8)).unwrap();
    tree.set_root(root).unwrap();
    let title = tree
        .add(
            NodeKind::Text("Before you start"),
            Style::new().text(TextStyle::new(TextRole::Title)),
        )
        .unwrap();
    tree.append(root, title).unwrap();
    let notice = tree
        .column(
            Style::new()
                .padding(8)
                .surface(Surface::Notice(Tone::Caution)),
        )
        .unwrap();
    tree.append(root, notice).unwrap();
    let text = tree
        .add(
            NodeKind::Text(
                "This screen forgets you when you finish, and nobody after you sees what you typed.",
            ),
            Style::new().text(TextStyle::BODY.lines(4)),
        )
        .unwrap();
    tree.append(notice, text).unwrap();
    let row = tree.row(Style::new().gap(8).align(Align::Start)).unwrap();
    tree.append(root, row).unwrap();
    let button = tree.button("Back", ActionId(1)).unwrap();
    tree.append(row, button).unwrap();
    let slot = tree.slot(NOTE).unwrap();
    tree.set_style(slot, Style::new().text(TextStyle::BODY.lines(3)))
        .unwrap();
    tree.append(row, slot).unwrap();
    tree.set_slot(NOTE, "Your pass shows a number, never your name")
        .unwrap();
    tree.layout(Rect::new(0, 0, width, height), &theme.metrics())
        .unwrap();
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn full_frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

#[test]
fn wrapped_text_draws_on_its_lines_in_each_theme() {
    for (theme, width, height, image) in [
        (Theme::<Rgb888>::light(), 320, 240, "wrap.png"),
        (
            Theme::<Rgb888>::high_contrast(),
            480,
            360,
            "wrap-high-contrast.png",
        ),
    ] {
        let mut tree = screen(&theme, width, height);
        let metrics = theme.metrics();
        for (_, node) in tree.paint_order() {
            assert_eq!(node.cut_text(&metrics), None, "{image}: {node:?}");
        }
        let mut fb = full_frame(&tree, &theme);
        check_snapshot(&fb, &golden(image)).unwrap();
        // The slot's text changes to fewer lines: a dirty repaint gives the
        // pixels of a full one.
        tree.clear_dirty();
        tree.set_slot(NOTE, "Wiped").unwrap();
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut fb, &theme, &mut glyphs__)
        }
        .unwrap();
        assert!(full_frame(&tree, &theme).diff(&fb).is_none(), "{image}");
    }
}
