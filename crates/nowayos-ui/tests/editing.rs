// SPDX-License-Identifier: MIT OR Apache-2.0
//! Editing text anywhere (#10075): a caret moved by the arrows, Home and
//! End, a word at a time with Ctrl, and placed by the pointer; a selection
//! made with Shift, a drag or a double click, which typing replaces and
//! Backspace and Delete remove; text areas of several lines, Enter starting
//! one, scrolled to keep the caret in view; and every value - a text area's
//! bytes in the text store, and a long text's - wiped to zeros.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{FieldId, Input, NodeKind, SlotId, TextMetrics, TextStyle, Tree, UiEvent};
use proptest::prelude::*;

const NAME: FieldId = FieldId(1);
const NOTE: FieldId = FieldId(2);

/// Eight pixels a character (the built-in fixed font's metrics), sixteen a
/// line.
fn metrics() -> TextMetrics {
    TextMetrics::new(8, 16)
}

/// A text input and a three-row text area, laid out in 400 x 300.
fn screen() -> Tree<8, 512> {
    let mut tree: Tree<8, 512> = Tree::new();
    let input = UiBuilder::input(&mut tree, NAME, Input::new().max_len(32)).unwrap();
    let area = UiBuilder::text_area(&mut tree, NOTE, 3, 400, "Write here").unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new().gap(8), &[input, area]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &metrics()).unwrap();
    tree.set_focus(Some(input)).unwrap();
    tree
}

fn key(tree: &mut Tree<8, 512>, code: KeyCode) -> Option<UiEvent> {
    let event = tree.handle(InputEvent::KeyDown(code));
    tree.handle(InputEvent::KeyUp(code));
    event
}

fn chord(tree: &mut Tree<8, 512>, held: KeyCode, code: KeyCode) {
    tree.handle(InputEvent::KeyDown(held));
    key(tree, code);
    tree.handle(InputEvent::KeyUp(held));
}

fn type_text(tree: &mut Tree<8, 512>, text: &str) {
    for ch in text.chars() {
        let (code, shift) = match ch {
            ' ' => (KeyCode::SPACE, false),
            '\n' => (KeyCode::ENTER, false),
            c if c.is_ascii_uppercase() => (KeyCode::letter(c).unwrap(), true),
            c => (KeyCode::letter(c).unwrap(), false),
        };
        if shift {
            chord(tree, KeyCode::LEFT_SHIFT, code);
        } else {
            key(tree, code);
        }
    }
}

/// The focused input's caret and anchor.
fn caret(tree: &Tree<8, 512>) -> (u32, u32) {
    match *tree.get(tree.focused().unwrap()).unwrap().kind() {
        NodeKind::TextInput { caret, anchor, .. } => (u32::from(caret), u32::from(anchor)),
        NodeKind::TextArea { caret, anchor, .. } => (caret, anchor),
        _ => panic!("no text input focused"),
    }
}

#[test]
fn the_caret_moves_and_typing_goes_in_where_it_is() {
    let mut tree = screen();
    type_text(&mut tree, "hello world");
    assert_eq!(caret(&tree), (11, 11));
    key(&mut tree, KeyCode::HOME);
    assert_eq!(caret(&tree), (0, 0));
    chord(&mut tree, KeyCode::LEFT_CTRL, KeyCode::RIGHT);
    assert_eq!(caret(&tree), (5, 5));
    type_text(&mut tree, " there");
    assert_eq!(tree.value(NAME), Some("hello there world"));
    key(&mut tree, KeyCode::END);
    chord(&mut tree, KeyCode::LEFT_CTRL, KeyCode::LEFT);
    assert_eq!(caret(&tree), (12, 12));
    // Backspace takes the character before the caret, Delete the one after.
    key(&mut tree, KeyCode::BACKSPACE);
    key(&mut tree, KeyCode::DELETE);
    assert_eq!(tree.value(NAME), Some("hello thereorld"));
}

#[test]
fn shift_selects_and_typing_backspace_or_delete_replace_the_selection() {
    let mut tree = screen();
    type_text(&mut tree, "hello world");
    // Shift and Ctrl-Left: "world" selected.
    tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    chord(&mut tree, KeyCode::LEFT_CTRL, KeyCode::LEFT);
    tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    assert_eq!(caret(&tree), (6, 11));
    type_text(&mut tree, "there");
    assert_eq!(tree.value(NAME), Some("hello there"));
    // Ctrl-A and Delete: empty.
    chord(&mut tree, KeyCode::LEFT_CTRL, KeyCode::letter('a').unwrap());
    assert_eq!(caret(&tree), (11, 0));
    key(&mut tree, KeyCode::DELETE);
    assert_eq!(tree.value(NAME), Some(""));
    // A value set by the application puts the caret after it.
    tree.set_value(NAME, "café").unwrap();
    assert_eq!(caret(&tree), (5, 5));
    key(&mut tree, KeyCode::LEFT);
    assert_eq!(
        caret(&tree),
        (3, 3),
        "a character back is two bytes back over é"
    );
}

#[test]
fn the_pointer_places_the_caret_drags_a_selection_and_double_clicks_a_word() {
    let mut tree = screen();
    type_text(&mut tree, "one two three");
    let input = tree.focused().unwrap();
    let content = tree.get(input).unwrap().content();
    let at = |chars: i64| content.left() + chars * 8;
    let y = i32::try_from(content.top() + 4).unwrap();
    let press = |tree: &mut Tree<8, 512>, x: i64, down: bool| {
        tree.handle(InputEvent::PointerMove {
            x: i32::try_from(x).unwrap(),
            y,
        });
        tree.handle(InputEvent::PointerButton { button: 0, down });
    };
    // Pressed between "t" and "w" of "two".
    press(&mut tree, at(5) - 2, true);
    press(&mut tree, at(5) - 2, false);
    assert_eq!(caret(&tree), (5, 5));
    // Dragged to the end of "two" with the button down.
    press(&mut tree, at(4), true);
    tree.handle(InputEvent::PointerMove {
        x: i32::try_from(at(7)).unwrap(),
        y,
    });
    press(&mut tree, at(7), false);
    assert_eq!(caret(&tree), (7, 4));
    // A second press where the first fell selects the word.
    press(&mut tree, at(9), true);
    press(&mut tree, at(9), false);
    press(&mut tree, at(9), true);
    press(&mut tree, at(9), false);
    assert_eq!(caret(&tree), (13, 8));
}

#[test]
fn a_text_area_wraps_starts_lines_and_scrolls_to_its_caret() {
    let mut tree = screen();
    key(&mut tree, KeyCode::TAB);
    let area = tree.focused().unwrap();
    assert!(matches!(
        tree.get(area).unwrap().kind(),
        NodeKind::TextArea { .. }
    ));
    type_text(&mut tree, "first\nsecond\nthird\nfourth");
    assert_eq!(tree.value(NOTE), Some("first\nsecond\nthird\nfourth"));
    // Four lines in three rows: scrolled down one.
    let top = |tree: &Tree<8, 512>| match *tree.get(area).unwrap().kind() {
        NodeKind::TextArea { top, .. } => top,
        _ => unreachable!(),
    };
    assert_eq!(top(&tree), 1);
    // Up three lines, keeping the column: the first line, scrolled back.
    for _ in 0..3 {
        key(&mut tree, KeyCode::UP);
    }
    assert_eq!(
        caret(&tree),
        (5, 5),
        "the end of \"first\", nearest column 6"
    );
    assert_eq!(top(&tree), 0);
    // Down moves a line; Tab still leaves.
    key(&mut tree, KeyCode::DOWN);
    assert_eq!(caret(&tree), (11, 11), "\"secon|d\": column 5 kept");
    key(&mut tree, KeyCode::TAB);
    assert_ne!(tree.focused(), Some(area));
}

#[test]
fn every_value_is_wiped_to_zeros_text_areas_and_long_texts_too() {
    let mut tree: Tree<8, 512> = Tree::new();
    let input = UiBuilder::input(&mut tree, NAME, Input::new().max_len(32)).unwrap();
    let area = UiBuilder::text_area(&mut tree, NOTE, 3, 200, "").unwrap();
    let long = UiBuilder::long_text(&mut tree, SlotId(9), 200, TextStyle::default()).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[input, area, long]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &metrics()).unwrap();
    tree.set_value(NAME, "Ada").unwrap();
    tree.set_value(NOTE, "A private note\nover two lines")
        .unwrap();
    tree.set_slot(SlotId(9), "Shown to this person only")
        .unwrap();
    assert!(!tree.is_wiped());
    tree.wipe_session();
    assert!(tree.is_wiped(), "a session wipe zeroes every byte");
    tree.set_value(NOTE, "Again").unwrap();
    tree.clear_values();
    assert_eq!(tree.value(NOTE), Some(""));
}

proptest! {
    #[test]
    fn any_keys_keep_values_whole_and_carets_on_character_boundaries(
        keys in prop::collection::vec(0usize..14, 0..80),
        area in any::<bool>(),
    ) {
        let codes = [
            KeyCode::letter('a').unwrap(),
            KeyCode::letter('b').unwrap(),
            KeyCode::SPACE,
            KeyCode::ENTER,
            KeyCode::BACKSPACE,
            KeyCode::DELETE,
            KeyCode::LEFT,
            KeyCode::RIGHT,
            KeyCode::HOME,
            KeyCode::END,
            KeyCode::UP,
            KeyCode::DOWN,
            KeyCode::LEFT_SHIFT,
            KeyCode::LEFT_CTRL,
        ];
        let mut tree = screen();
        if area {
            key(&mut tree, KeyCode::TAB);
        }
        let held = std::cell::Cell::new(None);
        for index in keys {
            let code = codes[index];
            if matches!(code, KeyCode::LEFT_SHIFT | KeyCode::LEFT_CTRL) {
                // Held for the next key.
                tree.handle(InputEvent::KeyDown(code));
                held.set(Some(code));
                continue;
            }
            if !area && matches!(code, KeyCode::ENTER | KeyCode::UP | KeyCode::DOWN) {
                continue;
            }
            key(&mut tree, code);
            if let Some(code) = held.take() {
                tree.handle(InputEvent::KeyUp(code));
            }
            let field = if area { NOTE } else { NAME };
            let value = tree.value(field).unwrap().to_owned();
            let (at, from) = caret(&tree);
            prop_assert!(value.is_char_boundary(at as usize) && value.is_char_boundary(from as usize));
            prop_assert!(at as usize <= value.len() && from as usize <= value.len());
            let most = if area { 400 } else { 32 };
            prop_assert!(value.len() <= most);
        }
    }
}
