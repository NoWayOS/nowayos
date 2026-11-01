// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text set on several lines: `text::wrap` breaks after spaces,
//! first fit, never losing a character and never making a line wider than
//! its width; layout gives a wrapping node its width first and then the
//! height of its lines, in a column and sharing a row; and `Node::cut_text`
//! finds text a node does not show whole, which every fit test fails on.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_glyphs::REGULAR_18;
use nowayos_ui::text::wrap;
use nowayos_ui::{
    ActionId, Align, Length, MAX_LINES, NodeId, SlotId, Style, TextMetrics, TextStyle, Tree,
};
use proptest::prelude::*;

/// Every character 10 pixels wide, a line 20 high.
const MONO: TextMetrics = TextMetrics::new(10, 20);

const NOTICE: &str = "This screen forgets you when you finish, and nobody after you sees it.";

#[test]
fn breaks_fall_after_spaces_first_fit() {
    let lines: Vec<&str> = wrap(NOTICE, 200, &MONO).collect();
    assert_eq!(
        lines,
        [
            "This screen forgets",
            "you when you finish,",
            "and nobody after you",
            "sees it."
        ]
    );
    // Several spaces at a break belong to neither line; a no-break space
    // never breaks.
    let lines: Vec<&str> = wrap("two   spaces", 60, &MONO).collect();
    assert_eq!(lines, ["two", "spaces"]);
    let lines: Vec<&str> = wrap("no\u{A0}break here", 80, &MONO).collect();
    assert_eq!(lines, ["no\u{A0}break", "here"]);
    // Nothing but spaces is no line at all; a short text one.
    assert_eq!(wrap("   ", 60, &MONO).count(), 0);
    assert_eq!(wrap("", 60, &MONO).count(), 0);
    assert_eq!(wrap("Fits", 60, &MONO).collect::<Vec<_>>(), ["Fits"]);
    // Not even one character fits: one a line, so every line moves on.
    assert_eq!(wrap("abc", 5, &MONO).collect::<Vec<_>>(), ["a", "b", "c"]);
}

#[test]
fn an_atlas_line_is_measured_as_a_line_of_its_own() {
    let metrics = TextMetrics::from_atlas(&REGULAR_18);
    for width in [80, 120, 200, 320] {
        for line in wrap(NOTICE, width, &metrics) {
            assert!(metrics.text_width(line) <= width, "{line:?} at {width}");
        }
    }
}

/// A screen: a column with a title and, if `lines` > 1, a notice wrapped
/// onto at most `lines` lines; and a row of a button and a wrapped slot.
fn screen(lines: u8, width: u32) -> (Tree<16>, NodeId, NodeId, NodeId) {
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new().padding(10).gap(4)).unwrap();
    tree.set_root(root).unwrap();
    let title = tree.text("Welcome").unwrap();
    tree.append(root, title).unwrap();
    let notice = tree
        .add(
            nowayos_ui::NodeKind::Text(NOTICE),
            Style::new().text(TextStyle::BODY.lines(lines)),
        )
        .unwrap();
    tree.append(root, notice).unwrap();
    let row = tree.row(Style::new().gap(10).align(Align::Start)).unwrap();
    tree.append(root, row).unwrap();
    let button = tree.button("Back", ActionId(1)).unwrap();
    tree.append(row, button).unwrap();
    let slot = tree.slot(SlotId::new(1)).unwrap();
    tree.set_style(slot, Style::new().text(TextStyle::BODY.lines(3)))
        .unwrap();
    tree.append(row, slot).unwrap();
    tree.set_slot(SlotId::new(1), "Your pass shows a number")
        .unwrap();
    tree.layout(Rect::new(0, 0, width, 400), &MONO).unwrap();
    (tree, notice, button, slot)
}

#[test]
fn layout_gives_a_wrapping_text_its_width_then_its_lines_height() {
    let (tree, notice, button, slot) = screen(MAX_LINES, 220);
    // The column's content is 200 wide: four lines of 20.
    let rect = tree.get(notice).unwrap().rect();
    assert_eq!((rect.size.width, rect.size.height), (200, 80));
    // In the row, the slot takes what the button and the gap leave.
    let button_width = tree.get(button).unwrap().rect().size.width;
    let slot_rect = tree.get(slot).unwrap().rect();
    assert!(slot_rect.size.width <= 200 - button_width - 10);
    assert_eq!(slot_rect.size.height, 60, "three lines");
    for (_, node) in tree.paint_order() {
        assert_eq!(node.cut_text(&MONO), None);
    }
}

#[test]
fn text_needing_more_lines_than_it_may_take_is_cut() {
    let (tree, notice, ..) = screen(3, 220);
    assert_eq!(tree.get(notice).unwrap().rect().size.height, 60);
    assert_eq!(tree.get(notice).unwrap().cut_text(&MONO), Some(NOTICE));
    // On one line, as without wrapping: cut where it is wider than its node.
    let (tree, notice, ..) = screen(1, 220);
    assert_eq!(tree.get(notice).unwrap().rect().size.height, 20);
    assert_eq!(tree.get(notice).unwrap().cut_text(&MONO), Some(NOTICE));
    // A fixed height too short for its lines cuts it too.
    let mut tree: Tree<4> = Tree::new();
    let text = tree
        .add(
            nowayos_ui::NodeKind::Text(NOTICE),
            Style::new()
                .text(TextStyle::BODY.lines(8))
                .height(Length::Fixed(40)),
        )
        .unwrap();
    tree.set_root(text).unwrap();
    tree.layout(Rect::new(0, 0, 200, 400), &MONO).unwrap();
    assert_eq!(tree.get(text).unwrap().cut_text(&MONO), Some(NOTICE));
}

#[test]
fn a_fixed_width_wraps_at_that_width() {
    let mut tree: Tree<4> = Tree::new();
    let root = tree.column(Style::new().align(Align::Start)).unwrap();
    tree.set_root(root).unwrap();
    let text = tree
        .add(
            nowayos_ui::NodeKind::Text(NOTICE),
            Style::new()
                .text(TextStyle::BODY.lines(8))
                .width(Length::Fixed(120)),
        )
        .unwrap();
    tree.append(root, text).unwrap();
    tree.layout(Rect::new(0, 0, 800, 400), &MONO).unwrap();
    let rect = tree.get(text).unwrap().rect();
    let lines = wrap(NOTICE, 120, &MONO).count() as u32;
    assert_eq!((rect.size.width, rect.size.height), (120, 20 * lines));
    assert_eq!(tree.get(text).unwrap().cut_text(&MONO), None);
}

proptest! {
    /// Whatever the text and width, no character is lost or added but the
    /// spaces at a break, every line is as wide as the width at most when
    /// one character fits, and no line starts with a space or is empty.
    #[test]
    fn wrapping_keeps_every_character_and_fits_every_line(
        text in "[a-z \u{A0}.,]{0,120}",
        width in 0u32..300,
    ) {
        let lines: Vec<&str> = wrap(&text, width, &MONO).collect();
        let kept: String = lines.iter().flat_map(|line| line.chars()).filter(|&c| c != ' ').collect();
        let given: String = text.chars().filter(|&c| c != ' ').collect();
        prop_assert_eq!(kept, given);
        for line in &lines {
            prop_assert!(!line.is_empty());
            prop_assert!(!line.starts_with(' '));
            if width >= 10 {
                prop_assert!(MONO.text_width(line) <= width, "{:?} at {}", line, width);
            }
        }
        prop_assert!(lines.len() <= text.chars().count());
    }
}
