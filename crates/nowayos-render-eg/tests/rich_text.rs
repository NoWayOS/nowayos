// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rich text (#10078), as the golden image shows: runs in Bold, italics
//! (the face slanted), a theme colour, code on its tint and links,
//! underlined, the focused one ringed - on the background, on a card and
//! on a header bar; and a repaint of any part gives the full repaint's
//! pixels.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::DrawTarget;
use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, Run, UiBuilder};
use nowayos_ui::{ActionId, Colour, Surface, TextStyle, Tree};

const RUNS: &[Run] = &[
    Run::new("Press "),
    Run::new("Save").bold(),
    Run::new(" to keep your work, or read "),
    Run::new("the guide").link(ActionId(1)),
    Run::new(" first. "),
    Run::new("Unsaved changes are lost").italic(),
    Run::new(" when the session ends; "),
    Run::new("errors show in red").colour(Colour::Error),
    Run::new(". Type "),
    Run::new("help --all").code(),
    Run::new(" for "),
    Run::new("every command").link(ActionId(2)).bold(),
    Run::new("."),
];

const BAR: &[Run] = &[
    Run::new("Signed in as "),
    Run::new("a visitor").italic(),
    Run::new(" - "),
    Run::new("sign out").link(ActionId(3)),
];

fn screen(theme: &Theme<Rgb888>) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let t = &mut tree;
    let bar_text = UiBuilder::rich_text(t, BAR, TextStyle::default()).unwrap();
    let bar = UiBuilder::row_on(t, Surface::Header, Layout::new().padding(8), &[bar_text]).unwrap();
    let plain = UiBuilder::rich_text(t, RUNS, TextStyle::default()).unwrap();
    let on_card = UiBuilder::rich_text(t, RUNS, TextStyle::default()).unwrap();
    let card =
        UiBuilder::column_on(t, Surface::Card, Layout::new().padding(12), &[on_card]).unwrap();
    let root =
        UiBuilder::column(t, Layout::new().gap(12).padding(12), &[bar, plain, card]).unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 560, 420), &theme.metrics())
        .unwrap();
    tree
}

fn frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
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
fn runs_of_every_kind_as_approved_and_repainted_whole() {
    for (theme, image) in [
        (Theme::<Rgb888>::light(), "rich-text.png"),
        (Theme::<Rgb888>::high_contrast(), "rich-text-large.png"),
    ] {
        let mut tree = screen(&theme);
        // The second text's second link focused, after keys.
        for _ in 0..4 {
            tree.handle(InputEvent::KeyDown(KeyCode::TAB));
        }
        let full = frame(&tree, &theme);
        let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(image);
        check_snapshot(&full, &golden).unwrap();
        let mut part = full.clone();
        part.clear(Rgb888::new(1, 2, 3)).unwrap();
        let size = tree.viewport().size;
        for y in (0..size.height).step_by(23) {
            for x in (0..size.width).step_by(37) {
                nowayos_testkit::glyph_store!(glyphs__);
                render_area(
                    &tree,
                    &mut part,
                    &theme,
                    Rect::new(x as i32, y as i32, 37, 23),
                    &mut glyphs__,
                )
                .unwrap();
            }
        }
        assert_eq!(part.diff(&full), None, "{image}");
        // Every run readable where it stands.
        let worst = nowayos_render_eg::contrast::text_colours(&tree, &theme)
            .map(|(_, text, ground)| nowayos_render_eg::contrast::ratio(text, ground))
            .fold(f64::MAX, f64::min);
        assert!(worst >= 4.5, "{image}: {worst}");
    }
}
