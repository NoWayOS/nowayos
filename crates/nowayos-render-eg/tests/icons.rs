// SPDX-License-Identifier: MIT OR Apache-2.0
//! Icons: each of Lucide's icons drawn from its alpha mask, tinted
//! with the colour of text where it stands - on the background, on a card
//! and on a header bar - in each theme and at device scale 2, as the golden
//! images show; a dirty repaint gives exactly the pixels of a full one.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::DrawTarget;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::{Style, Surface, Tree, icons};

/// Every icon in a row on the background, then on a card, then on a header
/// bar, laid out in `width` x `height` with `theme`'s metrics at `scale`.
fn screen(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let root = tree.column(Style::new().padding(8).gap(8)).unwrap();
    tree.set_root(root).unwrap();
    for surface in [None, Some(Surface::Card), Some(Surface::Header)] {
        let style = Style::new().padding(8).gap(8);
        let row = match surface {
            Some(surface) => tree.row(style.surface(surface)).unwrap(),
            None => tree.row(style).unwrap(),
        };
        tree.append(root, row).unwrap();
        for icon in icons::ALL {
            let node = tree.icon(icon).unwrap();
            tree.append(row, node).unwrap();
        }
    }
    tree.layout(
        Rect::new(0, 0, width * scale, height * scale),
        &theme.metrics_at(scale),
    )
    .unwrap();
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

#[test]
fn every_icon_matches_its_golden_images_in_each_theme() {
    for (theme, scale, image) in [
        (Theme::<Rgb888>::light(), 1, "icons.png"),
        (Theme::<Rgb888>::light(), 2, "icons-2x.png"),
        (
            Theme::<Rgb888>::high_contrast(),
            1,
            "icons-high-contrast.png",
        ),
    ] {
        let tree = screen(&theme, 240, 160, scale);
        let size = tree.viewport().size;
        let mut full = Framebuffer::new(size.width, size.height).unwrap();
        render(&tree, &mut full, &theme).unwrap();
        check_snapshot(&full, &golden(image)).unwrap();
        // Each icon is its mask's size at the scale, inked in the colour of
        // text where it stands, and a repaint of any part gives the same
        // pixels.
        let mut part = full.clone();
        part.clear(Rgb888::new(1, 2, 3)).unwrap();
        for y in (0..size.height).step_by(37) {
            for x in (0..size.width).step_by(41) {
                render_area(
                    &tree,
                    &mut part,
                    &theme,
                    Rect::new(x as i32, y as i32, 41, 37),
                )
                .unwrap();
            }
        }
        assert_eq!(part.diff(&full), None, "{image}");
        let icon = tree
            .paint_order()
            .find(|(_, node)| matches!(node.kind(), nowayos_ui::NodeKind::Icon(_)))
            .unwrap()
            .1;
        assert_eq!(icon.rect().size.width, 24 * scale, "{image}");
    }
}
