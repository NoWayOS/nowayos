// SPDX-License-Identifier: MIT OR Apache-2.0
//! Long text (builder contract v16): paragraphs the application sets,
//! kept in the tree's text store - nothing allocates - wrapped onto as many
//! lines as they need, a line break starting a paragraph; refused when too
//! long, when the store is full or when a character cannot be shown; laid
//! out again when its number of lines changes; wiped like a slot.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::long_text::{MAX_LONG_TEXT, check_paragraphs, lines};
use nowayos_ui::{SlotId, TextMetrics, TextStyle, Tree, UiError};

const BODY: SlotId = SlotId::new(1);
/// Eight pixels a character, sixteen a line.
const METRICS: TextMetrics = TextMetrics::new(8, 16);

fn screen() -> Tree<8, 256> {
    let mut tree: Tree<8, 256> = Tree::new();
    let body = UiBuilder::long_text(&mut tree, BODY, 200, TextStyle::default()).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[body]).unwrap();
    tree.set_root(root).unwrap();
    // Ten characters a line.
    tree.layout(Rect::new(0, 0, 80, 400), &METRICS).unwrap();
    tree
}

fn height(tree: &Tree<8, 256>) -> u32 {
    tree.get(tree.find_slot(BODY).unwrap())
        .unwrap()
        .rect()
        .size
        .height
}

#[test]
fn paragraphs_wrap_and_a_blank_line_separates_them() {
    let wrapped: Vec<&str> = lines("one two three\n\nfour", 80, &METRICS).collect();
    assert_eq!(wrapped, ["one two", "three", "", "four"]);
    assert_eq!(lines("", 80, &METRICS).count(), 0);
    assert_eq!(lines("\n", 80, &METRICS).collect::<Vec<_>>(), ["", ""]);
}

#[test]
fn the_node_is_as_high_as_its_lines_and_laid_out_again_when_they_change() {
    let mut tree = screen();
    // Empty: one line high.
    assert_eq!(height(&tree), 16);
    tree.clear_dirty();
    tree.set_slot(BODY, "one two three\n\nfour").unwrap();
    assert_eq!(height(&tree), 64);
    assert_eq!(tree.slot_text(BODY), Some("one two three\n\nfour"));
    // Laid out again: everything repaints.
    assert_eq!(tree.dirty_rects(), &[Rect::new(0, 0, 80, 400)]);
    // The same number of lines: only the line that differs.
    tree.clear_dirty();
    tree.set_slot(BODY, "one two three\n\nfive").unwrap();
    assert_eq!(height(&tree), 64);
    assert_eq!(tree.dirty_rects(), &[Rect::new(0, 48, 80, 16)]);
}

#[test]
fn too_long_too_many_and_unshowable_texts_are_refused() {
    let mut tree = screen();
    let long = "a".repeat(201);
    assert!(matches!(
        tree.set_slot(BODY, &long),
        Err(UiError::TextTooLong(_))
    ));
    assert_eq!(
        tree.set_slot(BODY, "tab\there"),
        Err(UiError::ControlCharacter)
    );
    assert_eq!(
        tree.set_slot(BODY, "a\r\nb"),
        Err(UiError::ControlCharacter)
    );
    assert_eq!(check_paragraphs("one\ntwo"), Ok(()));
    // The store holds 256 bytes; 200 are taken.
    let mut more: Tree<8, 256> = Tree::new();
    UiBuilder::long_text(&mut more, BODY, 200, TextStyle::default()).unwrap();
    assert_eq!(
        UiBuilder::long_text(&mut more, SlotId::new(2), 100, TextStyle::default()),
        Err(UiError::TextStoreFull { capacity: 256 })
    );
    assert_eq!(
        UiBuilder::long_text(&mut more, BODY, 10, TextStyle::default()),
        Err(UiError::DuplicateSlot(BODY))
    );
    assert_eq!(
        more.long_text(SlotId::new(3), 0),
        Err(UiError::LongTextOutOfRange)
    );
    assert_eq!(
        more.long_text(SlotId::new(3), MAX_LONG_TEXT + 1),
        Err(UiError::LongTextOutOfRange)
    );
    // A tree without a store has room for none.
    let mut none: Tree<8> = Tree::new();
    assert_eq!(
        none.long_text(BODY, 1),
        Err(UiError::TextStoreFull { capacity: 0 })
    );
}

#[test]
fn clearing_the_slots_wipes_a_long_text_and_clearing_the_tree_empties_the_store() {
    let mut tree = screen();
    tree.set_slot(BODY, "A private note").unwrap();
    assert!(!tree.is_wiped());
    tree.clear_slots();
    assert!(tree.is_wiped());
    assert_eq!(tree.slot_text(BODY), Some(""));
    tree.clear();
    // The whole store is free again.
    assert!(tree.long_text(BODY, 256).is_ok());
}

/// A long text set as its screen is built - the tree cleared, its root not
/// yet set, the last screen's metrics kept - is laid out with the rest.
#[test]
fn a_long_text_set_while_its_screen_is_built_is_laid_out_with_it() {
    let mut tree = screen();
    tree.clear();
    let body = UiBuilder::long_text(&mut tree, BODY, 200, TextStyle::default()).unwrap();
    tree.set_slot(BODY, "one two three\n\nfour").unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[body]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 80, 400), &METRICS).unwrap();
    assert_eq!(height(&tree), 64);
}
