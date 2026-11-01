// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text roles and weights: a node's text is measured in its own
//! style's atlas, every style has its own place among the twelve, and text
//! in the display role holds only what its atlases draw - refused, when a
//! node is added or text is set, with an error that never names the
//! character.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_glyphs::{BOLD_18, BOLD_28, DISPLAY_96, REGULAR_14, REGULAR_18};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{
    ActionId, Align, FieldId, NodeKind, SlotId, Style, TextMetrics, TextRole, TextStyle, Tree,
    UiError, Weight,
};

/// A type scale of the light theme's sizes, as `nowayos-render-eg`'s theme
/// gives it.
fn metrics() -> TextMetrics {
    TextMetrics::from_atlas(&REGULAR_18)
        .with_face(TextStyle::BODY, &REGULAR_18)
        .with_face(TextStyle::BODY.bold(), &BOLD_18)
        .with_face(TextStyle::new(TextRole::Title), &BOLD_28)
        .with_face(TextStyle::new(TextRole::Caption), &REGULAR_14)
        .with_face(TextStyle::new(TextRole::Display), &DISPLAY_96)
}

#[test]
fn every_style_has_a_place_of_its_own() {
    let mut seen = [false; TextStyle::COUNT];
    for role in TextRole::ALL {
        for style in [TextStyle::new(role).regular(), TextStyle::new(role).bold()] {
            assert!(!seen[style.index()], "{style:?}");
            seen[style.index()] = true;
        }
    }
    assert!(seen.iter().all(|&s| s));
    assert_eq!(TextStyle::new(TextRole::Display).weight, Weight::Bold);
    assert_eq!(TextStyle::new(TextRole::Caption).weight, Weight::Regular);
    assert_eq!(Style::new().text, TextStyle::BODY);
}

/// Each node's text is measured in its own role and weight: as wide and as
/// high as that atlas makes it, and a style given no atlas as body text.
#[test]
fn each_node_is_measured_in_its_own_style() {
    let mut t: Tree<16> = Tree::new();
    let root = t.column(Style::new().align(Align::Start)).unwrap();
    t.set_root(root).unwrap();
    let styles = [
        TextStyle::BODY,
        TextStyle::BODY.bold(),
        TextStyle::new(TextRole::Title),
        TextStyle::new(TextRole::Caption),
        TextStyle::new(TextRole::Label),
    ];
    let mut nodes = Vec::new();
    for style in styles {
        let id = t
            .add_child(root, NodeKind::Text("Room 4"), Style::new().text(style))
            .unwrap();
        nodes.push(id);
    }
    let m = metrics();
    t.layout(Rect::new(0, 0, 800, 480), &m).unwrap();
    let size = |id| t.get(id).unwrap().rect().size;
    for (atlas, id) in [&REGULAR_18, &BOLD_18, &BOLD_28, &REGULAR_14, &REGULAR_18]
        .iter()
        .zip(&nodes)
    {
        let own = TextMetrics::from_atlas(atlas);
        assert_eq!(
            size(*id).width,
            own.text_width("Room 4"),
            "{} px",
            atlas.size()
        );
        assert_eq!(size(*id).height, own.line_height(), "{} px", atlas.size());
    }
    assert!(size(nodes[1]).width > size(nodes[0]).width, "Bold is wider");
    assert!(
        size(nodes[2]).height > size(nodes[0]).height,
        "a title is taller"
    );
    // The metrics of a style keep the scale and target, and the faces.
    let title = m.for_style(TextStyle::new(TextRole::Title));
    assert_eq!(
        (title.scale(), title.min_target()),
        (m.scale(), m.min_target())
    );
    assert!(core::ptr::eq(title.atlas().unwrap(), &BOLD_28));
    assert!(core::ptr::eq(
        title.for_style(TextStyle::BODY).atlas().unwrap(),
        &REGULAR_18
    ));
}

/// Display text holds digits, capitals and a few signs: anything else is
/// refused - when the node is added, through the builder, when its text,
/// a slot's text or its style is set - and the previous text stays.
#[test]
fn display_text_holds_only_what_its_atlases_draw() {
    let display = TextStyle::new(TextRole::Display);
    let mut t: Tree<16> = Tree::new();
    for good in ["A047", "\u{A3}27.40", "12:30", "#4 - 50%", "G/7+"] {
        assert!(
            t.add(NodeKind::Text(good), Style::new().text(display))
                .is_ok(),
            "{good}"
        );
    }
    for bad in ["a047", "Room 4", "\u{A3}27,40p", "\u{E9}", "\u{20AC}5"] {
        assert_eq!(
            t.add(NodeKind::Text(bad), Style::new().text(display)),
            Err(UiError::NoGlyph),
            "{bad}"
        );
        // Any role but display takes it, if printable.
        if bad.chars().all(nowayos_ui::text::is_printable) {
            assert!(t.add(NodeKind::Text(bad), Style::new()).is_ok(), "{bad}");
        }
    }
    let shown = t
        .add(NodeKind::Text("A047"), Style::new().text(display))
        .unwrap();
    assert_eq!(t.set_text(shown, "b12"), Err(UiError::NoGlyph));
    assert_eq!(t.get(shown).unwrap().kind(), &NodeKind::Text("A047"));
    let plain = t.add(NodeKind::Text("ticket"), Style::new()).unwrap();
    assert_eq!(
        t.set_style(plain, Style::new().text(display)),
        Err(UiError::NoGlyph)
    );
    assert_eq!(t.get(plain).unwrap().style().text, TextStyle::BODY);

    // Through the builder: text and a slot.
    let mut b: Tree<16> = Tree::new();
    assert_eq!(
        UiBuilder::text_styled(&mut b, "abc", display),
        Err(UiError::NoGlyph)
    );
    let title =
        UiBuilder::text_styled(&mut b, "About you", TextStyle::new(TextRole::Title)).unwrap();
    assert_eq!(b.get(title).unwrap().style().text.role, TextRole::Title);
    let number = SlotId::new(1);
    let slot = UiBuilder::slot_styled(&mut b, number, display).unwrap();
    assert_eq!(b.get(slot).unwrap().style().text, display);
    assert_eq!(b.set_slot(number, "A048"), Ok(()));
    assert_eq!(b.set_slot(number, "ab"), Err(UiError::NoGlyph));
    assert_eq!(b.slot_text(number), Some("A048"));
    assert_eq!(
        UiBuilder::slot_styled(&mut b, number, display),
        Err(UiError::DuplicateSlot(number))
    );
    let _ = ui::TextRole::Caption;
}

/// Only a text, a slot and a button may be in the display role: a text
/// input, a field's error, a checkbox and a radio group take text through
/// calls that do not check the role - typing, `set_value`,
/// `set_field_error` - so each is refused the role, when it is added and
/// when its style is set, and keeps its style.
#[test]
fn only_text_slots_and_buttons_take_the_display_role() {
    let display = Style::new().text(TextStyle::new(TextRole::Display));
    let mut t: Tree<16> = Tree::new();
    let input = t.text_input(FieldId(1), "").unwrap();
    let checkbox = t.checkbox(FieldId(2), "OK").unwrap();
    let radio = t.radio_group(FieldId(3), &["A", "B"]).unwrap();
    let error = t.field_error(FieldId(1)).unwrap();
    for id in [input, checkbox, radio, error] {
        let before = *t.get(id).unwrap().style();
        assert_eq!(t.set_style(id, display), Err(UiError::RoleNotAllowed));
        assert_eq!(*t.get(id).unwrap().style(), before);
        let kind = t.get(id).unwrap().kind().clone();
        assert_eq!(t.add(kind, display), Err(UiError::RoleNotAllowed));
    }
    assert!(t.add(NodeKind::Text("A047"), display).is_ok());
    let button = t.button("GO", ActionId(1)).unwrap();
    assert_eq!(t.set_style(button, display), Ok(()));
    assert_eq!(
        UiError::RoleNotAllowed.to_string(),
        "only a text, a slot or a button may be in the display role"
    );
}
