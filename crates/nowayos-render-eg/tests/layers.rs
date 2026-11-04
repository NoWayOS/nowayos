// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layers over the screen (#10076), as the golden images show: a menu on
//! a card beside its button, and a dialog over the screen, which is veiled
//! beneath it - its picture too; and with layers open, a repaint of any
//! part, or of what opening and closing them marked, gives the pixels of a
//! full repaint.

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
use nowayos_input::{InputEvent, KeyCode};
use nowayos_render_eg::{Theme, render, render_area, render_dirty};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::pictures::{Fit, samples};
use nowayos_ui::{ActionId, ButtonVariant, LayerId, Surface, Tree};
use proptest::prelude::*;

const CONFIRM: LayerId = LayerId(1);
const MORE: LayerId = LayerId(2);

type Screen = Tree<40>;

/// A header bar with a More button and its menu, a card with a picture
/// and a line of text, a Delete button with a tooltip and a confirming
/// dialog: in 400 x 300.
fn screen(theme: &Theme<Rgb888>) -> Screen {
    let mut tree: Screen = Tree::new();
    let t = &mut tree;
    let title = UiBuilder::text(t, "Messages").unwrap();
    let more = UiBuilder::button(t, "More", ActionId(1)).unwrap();
    let rename = UiBuilder::button_with(t, "Rename", ActionId(2), ButtonVariant::Quiet).unwrap();
    let archive = UiBuilder::button_with(t, "Archive", ActionId(3), ButtonVariant::Quiet).unwrap();
    let items = UiBuilder::column(t, Layout::new().gap(4), &[rename, archive]).unwrap();
    let menu = UiBuilder::menu(t, MORE, more, items).unwrap();
    let bar = UiBuilder::row_on(
        t,
        Surface::Header,
        Layout::new().gap(8).padding(8),
        &[title, more, menu],
    )
    .unwrap();
    let picture = UiBuilder::image(
        t,
        &samples::LANDSCAPE,
        Fit::Cover,
        "A lake at sunset",
        Layout::new().width(120).height(72),
    )
    .unwrap();
    let words = UiBuilder::text(t, "A message with a picture").unwrap();
    let card = UiBuilder::row_on(
        t,
        Surface::Card,
        Layout::new().gap(8).padding(8),
        &[picture, words],
    )
    .unwrap();
    let delete = UiBuilder::button(t, "Delete", ActionId(4)).unwrap();
    let tip = UiBuilder::tooltip(t, delete, "Deletes it for good").unwrap();
    let question = UiBuilder::text(t, "Delete this message?").unwrap();
    let yes = UiBuilder::button(t, "Delete", ActionId(5)).unwrap();
    let no = UiBuilder::button(t, "Keep", ActionId(6)).unwrap();
    let answers = UiBuilder::row(t, Layout::new().gap(8), &[no, yes]).unwrap();
    let body = UiBuilder::column(t, Layout::new().gap(12), &[question, answers]).unwrap();
    let dialog = UiBuilder::dialog(t, CONFIRM, body).unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().gap(12).padding(8),
        &[bar, card, delete, tip, dialog],
    )
    .unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 400, 300), &theme.metrics())
        .unwrap();
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn frame(tree: &Screen, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

/// Repaints every part of `full`'s frame in tiles and checks the pixels
/// are the full repaint's.
fn tiles_match(tree: &Screen, theme: &Theme<Rgb888>, full: &Framebuffer) {
    let mut part = full.clone();
    part.clear(Rgb888::new(1, 2, 3)).unwrap();
    let size = tree.viewport().size;
    for y in (0..size.height).step_by(37) {
        for x in (0..size.width).step_by(41) {
            nowayos_testkit::glyph_store!(glyphs__);
            render_area(
                tree,
                &mut part,
                theme,
                Rect::new(x as i32, y as i32, 41, 37),
                &mut glyphs__,
            )
            .unwrap();
        }
    }
    assert_eq!(part.diff(full), None);
}

#[test]
fn a_menu_and_a_dialog_over_a_veiled_screen_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme);
    tree.open(MORE).unwrap();
    let menu = frame(&tree, &theme);
    check_snapshot(&menu, &golden("layers-menu.png")).unwrap();
    tiles_match(&tree, &theme, &menu);
    tree.close(MORE).unwrap();
    // The dialog, focus moved to its second button by Tab.
    tree.open(CONFIRM).unwrap();
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    let dialog = frame(&tree, &theme);
    check_snapshot(&dialog, &golden("layers-dialog.png")).unwrap();
    tiles_match(&tree, &theme, &dialog);
    // In the high-contrast theme too, each repaint whole.
    let large = Theme::<Rgb888>::high_contrast();
    let mut tree = screen(&large);
    tree.open(CONFIRM).unwrap();
    let full = frame(&tree, &large);
    tiles_match(&tree, &large, &full);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn opening_and_closing_repaint_exactly_what_changed(
        steps in prop::collection::vec((0usize..6, 0i32..400, 0i32..300), 1..10),
    ) {
        let theme = Theme::<Rgb888>::light();
        let mut tree = screen(&theme);
        let mut fb = frame(&tree, &theme);
        tree.clear_dirty();
        for (step, x, y) in steps {
            match step {
                0 => { let _ = tree.open(CONFIRM); }
                1 => { let _ = tree.open(MORE); }
                2 => { let _ = tree.close(CONFIRM); }
                3 => { tree.handle(InputEvent::KeyDown(KeyCode::ESCAPE)); }
                // The pointer moved: over Delete, a tooltip.
                _ => { tree.handle(InputEvent::PointerMove { x, y }); }
            }
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_dirty(&tree, &mut fb, &theme, &mut glyphs__).unwrap();
            }
            tree.clear_dirty();
            prop_assert!(frame(&tree, &theme).diff(&fb).is_none());
        }
    }
}
