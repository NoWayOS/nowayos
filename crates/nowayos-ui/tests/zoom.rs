// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zoom (#10083): Ctrl and plus, minus or zero ask for the whole interface
//! larger, smaller or as the display has it, a quarter at a time within 1
//! to 4; laid out again at another scale, the tree keeps its focus and each
//! scroll view its place, in proportion.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use nowayos_display::{DeviceScale, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{FieldId, Input, TextMetrics, Tree, UiEvent, Zoom};

fn chord(tree: &mut Tree<32>, usage: u16) -> Option<UiEvent> {
    tree.handle(InputEvent::KeyDown(KeyCode::LEFT_CTRL));
    let key = KeyCode::new(usage).unwrap();
    let event = tree.handle(InputEvent::KeyDown(key));
    tree.handle(InputEvent::KeyUp(key));
    tree.handle(InputEvent::KeyUp(KeyCode::LEFT_CTRL));
    event
}

/// A text input over forty lines in a scroll view, in 400 x 300 pixels.
fn screen() -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let t = &mut tree;
    let input = UiBuilder::input(t, FieldId(1), Input::new()).unwrap();
    let mut lines = Vec::new();
    for _ in 0..20 {
        lines.push(UiBuilder::text(t, "A line of the page").unwrap());
    }
    let page = UiBuilder::column(t, Layout::new().gap(8), &lines).unwrap();
    let view = UiBuilder::scroll_view(t, Layout::new(), page).unwrap();
    let root = UiBuilder::column(t, Layout::new(), &[input, view]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &TextMetrics::new(8, 16))
        .unwrap();
    tree.set_focus(Some(input)).unwrap();
    tree
}

#[test]
fn ctrl_and_plus_minus_or_zero_ask_for_zoom_and_type_nothing() {
    let mut tree = screen();
    for (usage, zoom) in [
        (0x2E, Zoom::In),
        (0x57, Zoom::In),
        (0x2D, Zoom::Out),
        (0x56, Zoom::Out),
        (0x27, Zoom::Reset),
        (0x62, Zoom::Reset),
    ] {
        assert_eq!(
            chord(&mut tree, usage),
            Some(UiEvent::Zoom(zoom)),
            "{usage:#x}"
        );
    }
    assert_eq!(tree.value(FieldId(1)), Some(""));
    // Without Ctrl, = and - type.
    tree.handle(InputEvent::KeyDown(KeyCode::new(0x2E).unwrap()));
    tree.handle(InputEvent::KeyDown(KeyCode::new(0x2D).unwrap()));
    assert_eq!(tree.value(FieldId(1)), Some("=-"));
}

#[test]
fn zoom_steps_a_quarter_within_one_to_four_or_returns_to_the_display() {
    let two = DeviceScale::TWO;
    let quarter = |q| DeviceScale::from_quarters(q).unwrap();
    assert_eq!(Zoom::In.apply(two, two), quarter(9));
    assert_eq!(Zoom::Out.apply(two, two), quarter(7));
    assert_eq!(Zoom::Reset.apply(quarter(12), two), two);
    assert_eq!(Zoom::In.apply(DeviceScale::MAX, two), DeviceScale::MAX);
    assert_eq!(Zoom::Out.apply(DeviceScale::ONE, two), DeviceScale::ONE);
}

#[test]
fn laid_out_at_another_scale_focus_and_each_scroll_place_are_kept() {
    let mut tree = screen();
    let focused = tree.focused();
    tree.scroll_to(100).unwrap();
    let view = tree.scroll_node().unwrap();
    let height = |tree: &Tree<32>| tree.get(view).unwrap().rect().size.height;
    let before = height(&tree);
    // At 1.5, the same place: half as far again, in pixels.
    let metrics = TextMetrics::new(12, 24).with_scale(DeviceScale::from_quarters(6).unwrap());
    tree.layout(Rect::new(0, 0, 400, 300), &metrics).unwrap();
    assert_eq!(tree.scale(), DeviceScale::from_quarters(6).unwrap());
    assert_eq!(tree.scroll_offset(), Some(150));
    assert_eq!(tree.focused(), focused);
    assert!(height(&tree) < before, "the input above it is taller");
    // Back at 1: where it was.
    tree.layout(Rect::new(0, 0, 400, 300), &TextMetrics::new(8, 16))
        .unwrap();
    assert_eq!(tree.scroll_offset(), Some(100));
}
