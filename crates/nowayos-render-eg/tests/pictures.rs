// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pictures: NoWayOS's samples - a landscape and a badge with a
//! transparent ground - at their own size and fitted to boxes of another
//! shape (contained, covering, filled), on the background, a card and a
//! header bar, at device scales 1, 1.5 and 2, as the golden images show; a
//! dirty repaint gives exactly the pixels of a full one.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, RgbColor};
use nowayos_display::{DeviceScale, Rect};
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::pictures::{Fit, samples};
use nowayos_ui::{Align, Length, NodeKind, Style, Surface, Tree};

/// A row's surface, and each picture's fit and style, if any.
type Row = (Option<Surface>, [(Fit, Option<Style>); 4]);

/// The samples in two rows, laid out in 600 x 280 units at `scale`.
fn screen(theme: &Theme<Rgb888>, scale: DeviceScale) -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let root = tree.column(Style::new().padding(8).gap(8)).unwrap();
    tree.set_root(root).unwrap();
    let square = Style::new()
        .width(Length::Fixed(100))
        .height(Length::Fixed(100));
    let rows: [Row; 2] = [
        (
            Some(Surface::Card),
            [
                (Fit::Fill, None),
                (Fit::Contain, Some(square)),
                (Fit::Cover, Some(square)),
                (Fit::Fill, Some(square)),
            ],
        ),
        (
            Some(Surface::Header),
            [
                (Fit::Fill, None),
                (Fit::Contain, Some(square)),
                (Fit::Cover, Some(square.width(Length::Fixed(60)))),
                (Fit::Fill, Some(square.height(Length::Fixed(40)))),
            ],
        ),
    ];
    for (index, (surface, pictures)) in rows.into_iter().enumerate() {
        // The badges at the start across, not stretched to the row's height
        // as flexbox stretches a picture by default.
        let mut style = Style::new().padding(8).gap(8);
        if index == 1 {
            style = style.align(Align::Start);
        }
        if let Some(surface) = surface {
            style = style.surface(surface);
        }
        let row = tree.row(style).unwrap();
        tree.append(root, row).unwrap();
        let picture = if index == 0 {
            &samples::LANDSCAPE
        } else {
            &samples::BADGE
        };
        for (fit, style) in pictures {
            let alt = if index == 0 { "A lake at sunset" } else { "" };
            let node = tree.image(picture, fit, alt).unwrap();
            if let Some(style) = style {
                tree.set_style(node, style).unwrap();
            }
            tree.append(row, node).unwrap();
        }
    }
    let size = |units: u32| scale.px(units);
    tree.layout(
        Rect::new(0, 0, size(600), size(280)),
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
fn the_samples_match_their_golden_images_at_each_scale() {
    let theme = Theme::<Rgb888>::light();
    for (quarters, image) in [
        (4, "pictures.png"),
        (6, "pictures-1.5x.png"),
        (8, "pictures-2x.png"),
    ] {
        let scale = DeviceScale::from_quarters(quarters).unwrap();
        let tree = screen(&theme, scale);
        let size = tree.viewport().size;
        let mut full = Framebuffer::new(size.width, size.height).unwrap();
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render(&tree, &mut full, &theme, &mut glyphs__)
        }
        .unwrap();
        check_snapshot(&full, &golden(image)).unwrap();
        // A repaint of any part gives the same pixels.
        let mut part = full.clone();
        part.clear(Rgb888::new(1, 2, 3)).unwrap();
        for y in (0..size.height).step_by(37) {
            for x in (0..size.width).step_by(41) {
                {
                    nowayos_testkit::glyph_store!(glyphs__);
                    render_area(
                        &tree,
                        &mut part,
                        &theme,
                        Rect::new(x as i32, y as i32, 41, 37),
                        &mut glyphs__,
                    )
                }
                .unwrap();
            }
        }
        assert_eq!(part.diff(&full), None, "{image}");
    }
}

#[test]
fn at_its_own_scale_a_picture_is_copied_and_its_alpha_shows_the_ground() {
    let theme = Theme::<Rgb888>::light();
    let tree = screen(&theme, DeviceScale::ONE);
    let mut frame =
        Framebuffer::new(tree.viewport().size.width, tree.viewport().size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(&tree, &mut frame, &theme, &mut glyphs__)
    }
    .unwrap();
    let images: Vec<_> = tree
        .paint_order()
        .filter(|(_, node)| matches!(node.kind(), NodeKind::Image { .. }))
        .map(|(_, node)| node.rect())
        .collect();
    // The landscape at its own size: every pixel its own.
    let first = images[0];
    assert_eq!((first.size.width, first.size.height), (160, 100));
    let pixels = samples::LANDSCAPE.best(160, 100);
    for (x, y) in [(0, 0), (80, 50), (159, 99), (17, 83)] {
        let [r, g, b, _] = pixels.at(x, y).unwrap();
        let at = (first.origin.x as u32 + x, first.origin.y as u32 + y);
        assert_eq!(
            frame.pixel(at.0, at.1),
            Some(Rgb888::new(r, g, b)),
            "({x}, {y})"
        );
    }
    // The badge's transparent corner shows the header bar beneath it.
    let badge = images[4];
    let (left, top) = (badge.origin.x as u32, badge.origin.y as u32);
    assert_eq!(frame.pixel(left, top).unwrap(), theme.surfaces.header.fill);
    // Its middle is opaque white, the tick.
    assert!(frame.pixel(left + 18, top + 28).unwrap().r() > 200);
}
