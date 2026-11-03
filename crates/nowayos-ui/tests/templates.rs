// SPDX-License-Identifier: MIT OR Apache-2.0
//! A slot between literal text compiled in (builder contract v14), React's
//! `<Text>Hello, {props.name}!</Text>`: the literals show from the first
//! frame, the application sets only its part, within the slot's bound, and
//! the whole is measured, wrapped and checked as one text. A wipe empties
//! the application's part and leaves the literals.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{NodeId, SLOT_CAPACITY, SlotId, TextMetrics, TextRole, TextStyle, Tree, UiError};

/// Six wide, ten high: a line of N characters is 6N wide.
const METRICS: TextMetrics = TextMetrics::new(6, 10);
const NAME: SlotId = SlotId(1);
const HELLO: ui::Template = ui::Template {
    before: "Hello, ",
    after: "!",
};

fn shown(tree: &Tree<8>, id: NodeId) -> String {
    tree.get(id)
        .unwrap()
        .kind()
        .slot_shown()
        .unwrap()
        .as_str()
        .to_owned()
}

/// A column `width` wide holding the templated slot, in `style`.
fn screen(width: u32, style: TextStyle) -> (Tree<8>, NodeId) {
    let mut tree: Tree<8> = Tree::new();
    let b = &mut tree;
    let slot = UiBuilder::slot_template_styled(b, NAME, &HELLO, style).unwrap();
    let root = UiBuilder::column(b, ui::Layout::new(), &[slot]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, width, 100), &METRICS).unwrap();
    (tree, slot)
}

#[test]
fn the_literals_show_at_first_and_the_application_sets_only_its_part() {
    let (mut tree, slot) = screen(200, TextStyle::BODY);
    assert_eq!(
        shown(&tree, slot),
        "Hello, !",
        "as React shows an undefined prop"
    );
    assert_eq!(tree.slot_text(NAME), Some(""));
    tree.set_slot(NAME, "Ada").unwrap();
    assert_eq!(shown(&tree, slot), "Hello, Ada!");
    assert_eq!(tree.slot_text(NAME), Some("Ada"), "the application's part");
    // Its bound is the slot's, whatever the literals take.
    let longest = "a".repeat(SLOT_CAPACITY);
    tree.set_slot(NAME, &longest).unwrap();
    assert_eq!(shown(&tree, slot).len(), SLOT_CAPACITY + 8);
    assert!(matches!(
        tree.set_slot(NAME, &"a".repeat(SLOT_CAPACITY + 1)),
        Err(UiError::TextTooLong(_))
    ));
}

#[test]
fn the_whole_text_is_measured_and_wrapped_as_one() {
    // "Hello, Ada!" fits one line in 200.
    let (mut tree, slot) = screen(200, TextStyle::BODY.lines(3));
    tree.set_slot(NAME, "Ada").unwrap();
    tree.layout(Rect::new(0, 0, 200, 100), &METRICS).unwrap();
    assert_eq!(tree.get(slot).unwrap().rect().size.height, 10);
    // "Hello, Ada Lovelace!" is 20 characters, 120 wide: two lines in 60.
    let (mut tree, slot) = screen(60, TextStyle::BODY.lines(3));
    tree.set_slot(NAME, "Ada Lovelace").unwrap();
    tree.layout(Rect::new(0, 0, 60, 100), &METRICS).unwrap();
    assert_eq!(tree.get(slot).unwrap().rect().size.height, 20);
    // A text too wide for one line, cut: named by the application's part.
    let (mut tree, slot) = screen(60, TextStyle::BODY);
    tree.set_slot(NAME, "Ada Lovelace").unwrap();
    tree.layout(Rect::new(0, 0, 60, 100), &METRICS).unwrap();
    assert_eq!(
        tree.get(slot).unwrap().cut_text(&METRICS),
        Some("Ada Lovelace")
    );
}

#[test]
fn a_wipe_empties_the_application_s_part_and_leaves_the_literals() {
    let (mut tree, slot) = screen(200, TextStyle::BODY);
    tree.set_slot(NAME, "Ada").unwrap();
    assert!(!tree.is_wiped());
    tree.wipe_session();
    assert!(tree.is_wiped(), "the literals are not the application's");
    assert_eq!(shown(&tree, slot), "Hello, !");
}

#[test]
fn literals_are_checked_as_any_text_and_bounded() {
    let mut tree: Tree<8> = Tree::new();
    static CONTROL: ui::Template = ui::Template {
        before: "Line\nbreak ",
        after: "",
    };
    assert_eq!(
        UiBuilder::slot_template(&mut tree, NAME, &CONTROL),
        Err(UiError::ControlCharacter)
    );
    static LONG: ui::Template = ui::Template {
        before: "This sentence is rather longer than any template may be, ",
        after: "or so it is.",
    };
    assert!(matches!(
        UiBuilder::slot_template(&mut tree, NAME, &LONG),
        Err(UiError::TextTooLong(_))
    ));
    // In the display role, the literals in its atlases: digits,
    // capitals and a few signs.
    static GATE: ui::Template = ui::Template {
        before: "Gate ",
        after: "",
    };
    assert_eq!(
        UiBuilder::slot_template_styled(&mut tree, NAME, &GATE, TextStyle::new(TextRole::Display)),
        Err(UiError::NoGlyph)
    );
    static DISPLAY: ui::Template = ui::Template {
        before: "GATE ",
        after: "",
    };
    let gate = UiBuilder::slot_template_styled(
        &mut tree,
        NAME,
        &DISPLAY,
        TextStyle::new(TextRole::Display),
    )
    .unwrap();
    assert!(tree.get(gate).is_some());
    // A slot number is one slot's, templated or not.
    assert_eq!(
        UiBuilder::slot_template(&mut tree, NAME, &HELLO),
        Err(UiError::DuplicateSlot(NAME))
    );
    assert_eq!(
        UiBuilder::slot(&mut tree, NAME),
        Err(UiError::DuplicateSlot(NAME))
    );
}
