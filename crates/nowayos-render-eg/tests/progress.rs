// SPDX-License-Identifier: MIT OR Apache-2.0
//! A progress bar: a rounded track and a fill in the theme's
//! colours, drawn as the golden images show in each theme, on the
//! background and on a card; moving its fill repaints only between the old
//! and the new end, with exactly the pixels of a full repaint.

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
use nowayos_ui::{SlotId, Style, Surface, Tree};

const FUEL: SlotId = SlotId::new(1);
const PUMP: SlotId = SlotId::new(2);

/// Two bars: one on the background, one on a card, laid out in `width` x
/// `height` with `theme`'s metrics at `scale`.
fn screen(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> Tree<8> {
    let mut tree: Tree<8> = Tree::new();
    let root = tree.column(Style::new().padding(16).gap(16)).unwrap();
    tree.set_root(root).unwrap();
    let title = tree.text("Fuelling").unwrap();
    tree.append(root, title).unwrap();
    let bar = tree.progress(FUEL).unwrap();
    tree.append(root, bar).unwrap();
    let card = tree
        .column(Style::new().padding(12).surface(Surface::Card))
        .unwrap();
    tree.append(root, card).unwrap();
    let on_card = tree.progress(PUMP).unwrap();
    tree.append(card, on_card).unwrap();
    tree.layout(
        Rect::new(0, 0, width * scale, height * scale),
        &theme.metrics_at(nowayos_display::DeviceScale::whole(scale).unwrap()),
    )
    .unwrap();
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn full_frame(tree: &Tree<8>, theme: &Theme<Rgb888>) -> Framebuffer {
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
fn a_progress_bar_matches_its_golden_images_in_each_theme() {
    for (theme, scale, image) in [
        (Theme::<Rgb888>::light(), 1, "progress.png"),
        (Theme::<Rgb888>::light(), 2, "progress-2x.png"),
        (
            Theme::<Rgb888>::high_contrast(),
            1,
            "progress-high-contrast.png",
        ),
    ] {
        let mut tree = screen(&theme, 320, 160, scale);
        let bar = tree.get(tree.find_progress(FUEL).unwrap()).unwrap().rect();
        assert_eq!(bar.size.height, 12 * scale, "{image}: 12 units high");
        tree.set_progress(FUEL, 2, 5).unwrap();
        tree.set_progress(PUMP, 9, 10).unwrap();
        assert_eq!(tree.progress_of(FUEL), Some(400));
        check_snapshot(&full_frame(&tree, &theme), &golden(image)).unwrap();
    }
}

#[test]
fn moving_the_fill_repaints_only_between_its_ends() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme, 320, 160, 2);
    let bar = tree.get(tree.find_progress(FUEL).unwrap()).unwrap().rect();
    let mut incremental = full_frame(&tree, &theme);
    tree.clear_dirty();
    for litres in (0..=40).chain((0..40).rev()).chain([1, 1]) {
        tree.set_progress(FUEL, litres, 40).unwrap();
        for rect in tree.dirty_rects() {
            assert!(bar.contains_rect(rect), "{litres}: {rect:?}");
            // A litre a step: a fortieth of the bar and its height each
            // side, not the whole bar.
            assert!(rect.size.width <= bar.size.width / 40 + 2 * bar.size.height + 1);
        }
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
        }
        .unwrap();
        tree.clear_dirty();
        assert_eq!(
            incremental.diff(&full_frame(&tree, &theme)),
            None,
            "{litres}"
        );
    }
    // The same value again marks nothing; past the whole is all of it; a
    // whole of 0 is none; and the session's wipe empties it.
    tree.set_progress(FUEL, 1, 40).unwrap();
    assert!(tree.dirty_rects().is_empty());
    tree.set_progress(FUEL, 99, 40).unwrap();
    assert_eq!(tree.progress_of(FUEL), Some(1000));
    tree.set_progress(FUEL, 3, 0).unwrap();
    assert_eq!(tree.progress_of(FUEL), Some(0));
    tree.set_progress(FUEL, 1, 3).unwrap();
    assert_eq!(tree.progress_of(FUEL), Some(333));
    tree.wipe_session();
    assert_eq!(tree.progress_of(FUEL), Some(0));
    assert!(tree.is_wiped());
    assert!(tree.set_progress(SlotId::new(9), 1, 2).is_err());
}
