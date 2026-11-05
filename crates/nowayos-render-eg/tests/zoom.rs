// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zoom (#10083), as the golden images show: one form, in a scroll view,
//! drawn in the same 960 x 540 pixels at 100%, 150% and 200% - everything
//! larger in proportion, what does not fit scrolled to. And at 200% on a
//! 1920 x 1080 screen - scale 4, 480 x 270 units - nothing is lost: every
//! control is reached by Tab, scrolled fully into view, and no control
//! is wider than the screen.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::{DeviceScale, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, FieldId, Input, TextRole, TextStyle, Tree};

fn screen(viewport: Rect, scale: DeviceScale, theme: &Theme<Rgb888>) -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let t = &mut tree;
    let title = UiBuilder::text_styled(t, "Book a visit", TextStyle::new(TextRole::Title)).unwrap();
    let about = UiBuilder::text_styled(
        t,
        "Choose a day and tell us who is coming. We keep your answers only until the visit ends.",
        TextStyle::default().lines(4),
    )
    .unwrap();
    let label = UiBuilder::text(t, "Your name").unwrap();
    let name = UiBuilder::input(t, FieldId(1), Input::new().placeholder("First name")).unwrap();
    let day = UiBuilder::radio_group(t, FieldId(2), &["Monday", "Tuesday", "Wednesday"]).unwrap();
    let agree = UiBuilder::checkbox(t, FieldId(3), "I have read the site rules").unwrap();
    let back = UiBuilder::button_with(t, "Back", ActionId(1), nowayos_ui::ButtonVariant::Secondary)
        .unwrap();
    let spacer = UiBuilder::spacer(t, None).unwrap();
    let go = UiBuilder::button(t, "Continue", ActionId(2)).unwrap();
    let buttons = UiBuilder::row(t, Layout::new().gap(12), &[back, spacer, go]).unwrap();
    let form = UiBuilder::column(
        t,
        Layout::new().gap(12).padding(16),
        &[title, about, label, name, day, agree, buttons],
    )
    .unwrap();
    let view = UiBuilder::scroll_view(t, Layout::new(), form).unwrap();
    tree.set_root(view).unwrap();
    tree.layout(viewport, &theme.metrics_at(scale)).unwrap();
    tree
}

fn frame(tree: &Tree<32>, theme: &Theme<Rgb888>) -> Framebuffer {
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
fn one_form_at_100_150_and_200_percent_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let viewport = Rect::new(0, 0, 960, 540);
    for (quarters, image) in [
        (4, "zoom-100.png"),
        (6, "zoom-150.png"),
        (8, "zoom-200.png"),
    ] {
        let scale = DeviceScale::from_quarters(quarters).unwrap();
        let tree = screen(viewport, scale, &theme);
        let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(image);
        check_snapshot(&frame(&tree, &theme), &golden).unwrap();
    }
}

#[test]
fn at_200_percent_on_1920_by_1080_every_control_is_reached_and_shown_whole() {
    let theme = Theme::<Rgb888>::light();
    let viewport = Rect::new(0, 0, 1920, 1080);
    let mut tree = screen(viewport, DeviceScale::MAX, &theme);
    let mut reached = Vec::new();
    for _ in 0..12 {
        tree.handle(InputEvent::KeyDown(KeyCode::TAB));
        let focused = tree.focused().unwrap();
        if reached.contains(&focused) {
            break;
        }
        reached.push(focused);
        let rect = tree.get(focused).unwrap().rect();
        assert!(
            viewport.intersection(&rect) == Some(rect),
            "{focused:?} at {rect:?} is not shown whole"
        );
        assert!(rect.size.width <= viewport.size.width);
    }
    // The name, the day, the box and both buttons.
    assert_eq!(reached.len(), 5);
}
