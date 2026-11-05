// SPDX-License-Identifier: MIT OR Apache-2.0
//! Editing text (#10075), as the golden images show: a text input with a
//! word selected on the selection colour and the caret after it, a long
//! value scrolled to keep its caret in view, and a text area of several
//! lines with a selection across two of them; and a repaint of any part
//! gives the pixels of a full one. Selectable text, pressed, on the
//! selection colour (#10080).

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
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{FieldId, Input, TextStyle, Tree};

fn key(tree: &mut Tree<16, 512>, code: KeyCode) {
    tree.handle(InputEvent::KeyDown(code));
    tree.handle(InputEvent::KeyUp(code));
}

fn shifted(tree: &mut Tree<16, 512>, code: KeyCode, times: usize) {
    tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    for _ in 0..times {
        key(tree, code);
    }
    tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
}

fn screen(theme: &Theme<Rgb888>) -> Tree<16, 512> {
    let mut tree: Tree<16, 512> = Tree::new();
    let name = UiBuilder::input(&mut tree, FieldId(1), Input::new().max_len(64)).unwrap();
    let long = UiBuilder::input(&mut tree, FieldId(2), Input::new().max_len(64)).unwrap();
    let note = UiBuilder::text_area(&mut tree, FieldId(3), 3, 400, "").unwrap();
    let root = UiBuilder::column(
        &mut tree,
        Layout::new().gap(8).padding(8),
        &[name, long, note],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 360, 240), &theme.metrics())
        .unwrap();
    tree.set_value(
        FieldId(2),
        "A value far too long for its field to show whole",
    )
    .unwrap();
    tree.set_value(
        FieldId(3),
        "First line of a note\nand a second line\nthen a third",
    )
    .unwrap();
    tree.set_value(FieldId(1), "Ada Lovelace").unwrap();
    // "Lovelace" selected, the caret before it: Shift and Left eight times.
    tree.set_focus(Some(name)).unwrap();
    shifted(&mut tree, KeyCode::LEFT, 8);
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn frame(tree: &Tree<16, 512>, theme: &Theme<Rgb888>) -> Framebuffer {
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
fn a_selection_a_scrolled_value_and_a_text_area_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme);
    check_snapshot(&frame(&tree, &theme), &golden("editing.png")).unwrap();
    // The text area focused, a selection from inside its first line into
    // its second.
    key(&mut tree, KeyCode::TAB);
    key(&mut tree, KeyCode::TAB);
    key(&mut tree, KeyCode::HOME);
    for _ in 0..2 {
        key(&mut tree, KeyCode::UP);
    }
    shifted(&mut tree, KeyCode::DOWN, 1);
    let full = frame(&tree, &theme);
    check_snapshot(&full, &golden("editing-area.png")).unwrap();
    // A repaint of any part gives the same pixels.
    let mut part = full.clone();
    part.clear(Rgb888::new(1, 2, 3)).unwrap();
    let size = tree.viewport().size;
    for y in (0..size.height).step_by(29) {
        for x in (0..size.width).step_by(31) {
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_area(
                    &tree,
                    &mut part,
                    &theme,
                    Rect::new(x as i32, y as i32, 31, 29),
                    &mut glyphs__,
                )
            }
            .unwrap();
        }
    }
    assert_eq!(part.diff(&full), None);
}

#[test]
fn selectable_text_pressed_is_drawn_on_the_selection_colour() {
    let theme = Theme::<Rgb888>::light();
    let mut tree: Tree<16, 512> = Tree::new();
    let text =
        UiBuilder::text_styled(&mut tree, "Ref 4471", TextStyle::default().selectable()).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new().padding(12), &[text]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 200, 60), &theme.metrics())
        .unwrap();
    let content = tree.get(text).unwrap().content();
    // A corner of the text's box: beside its glyphs, never on one.
    let (x, y) = (
        u32::try_from(content.left()).unwrap(),
        u32::try_from(content.top()).unwrap(),
    );
    let before = frame(&tree, &theme);
    assert_eq!(before.pixel(x, y), Some(theme.background));
    let press = |tree: &mut Tree<16, 512>, down: bool| {
        tree.handle(InputEvent::PointerMove {
            x: i32::try_from(x).unwrap() + 2,
            y: i32::try_from(y).unwrap() + 2,
        });
        tree.handle(InputEvent::PointerButton { button: 0, down });
    };
    press(&mut tree, true);
    press(&mut tree, false);
    assert_eq!(tree.selected_text(), Some(text));
    let full = frame(&tree, &theme);
    assert_eq!(full.pixel(x, y), Some(theme.selection));
    // A repaint of the text alone, over the frame before, gives the full
    // one's pixels.
    let mut part = before;
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_area(
            &tree,
            &mut part,
            &theme,
            tree.get(text).unwrap().rect(),
            &mut glyphs__,
        )
    }
    .unwrap();
    assert_eq!(part.diff(&full), None);
}
