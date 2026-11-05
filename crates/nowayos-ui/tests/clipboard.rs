// SPDX-License-Identifier: MIT OR Apache-2.0
//! Copy and paste inside the application (#10080): Ctrl+C, Ctrl+X and
//! Ctrl+V in text inputs and areas, a paste refused where it does not fit
//! or the input does not take its characters, selectable text selected by a
//! press and copied, and the clipboard wiped at a session wipe or once past
//! its life - and never shown by `Debug`.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{FieldId, Input, InputMode, NodeId, TextMetrics, TextStyle, Tree, UiEvent};

const NAME: FieldId = FieldId(1);
const CODE: FieldId = FieldId(2);
const NOTE: FieldId = FieldId(3);

struct Screen {
    tree: Tree<8, 512>,
    name: NodeId,
    code: NodeId,
    note: NodeId,
    reference: NodeId,
    title: NodeId,
}

/// A title, a selectable reference, a text input, a digits input and a
/// three-row text area, laid out in 400 x 300 at eight pixels a character.
fn screen() -> Screen {
    let mut tree: Tree<8, 512> = Tree::new();
    let title = UiBuilder::text_styled(&mut tree, "Your visit", TextStyle::default()).unwrap();
    let reference =
        UiBuilder::text_styled(&mut tree, "Ref 4471", TextStyle::default().selectable()).unwrap();
    let name = UiBuilder::input(&mut tree, NAME, Input::new().max_len(12)).unwrap();
    let code = UiBuilder::input(
        &mut tree,
        CODE,
        Input::new().max_len(8).mode(InputMode::Digits),
    )
    .unwrap();
    let note = UiBuilder::text_area(&mut tree, NOTE, 3, 400, "").unwrap();
    let root = UiBuilder::column(
        &mut tree,
        Layout::new().gap(8),
        &[title, reference, name, code, note],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &TextMetrics::new(8, 16))
        .unwrap();
    tree.set_focus(Some(name)).unwrap();
    Screen {
        tree,
        name,
        code,
        note,
        reference,
        title,
    }
}

fn key(tree: &mut Tree<8, 512>, code: KeyCode) -> Option<UiEvent> {
    let event = tree.handle(InputEvent::KeyDown(code));
    tree.handle(InputEvent::KeyUp(code));
    event
}

fn chord(tree: &mut Tree<8, 512>, held: KeyCode, code: KeyCode) -> Option<UiEvent> {
    tree.handle(InputEvent::KeyDown(held));
    let event = key(tree, code);
    tree.handle(InputEvent::KeyUp(held));
    event
}

fn ctrl(tree: &mut Tree<8, 512>, letter: char) -> Option<UiEvent> {
    chord(tree, KeyCode::LEFT_CTRL, KeyCode::letter(letter).unwrap())
}

fn type_text(tree: &mut Tree<8, 512>, text: &str) {
    for ch in text.chars() {
        let code = match ch {
            ' ' => KeyCode::SPACE,
            c if c.is_ascii_digit() => {
                KeyCode::digit(u8::try_from(c.to_digit(10).unwrap()).unwrap()).unwrap()
            }
            c => KeyCode::letter(c).unwrap(),
        };
        key(tree, code);
    }
}

/// A press and release at the middle of `id`.
fn tap(tree: &mut Tree<8, 512>, id: NodeId) {
    let rect = tree.get(id).unwrap().rect();
    let x = i32::try_from(rect.left() + i64::from(rect.size.width / 2)).unwrap();
    let y = i32::try_from(rect.top() + i64::from(rect.size.height / 2)).unwrap();
    tree.handle(InputEvent::PointerMove { x, y });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: false,
    });
}

#[test]
fn ctrl_c_x_and_v_copy_cut_and_paste_in_an_input() {
    let mut s = screen();
    type_text(&mut s.tree, "hello world");
    // "world" selected and copied: the value stays.
    s.tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    chord(&mut s.tree, KeyCode::LEFT_CTRL, KeyCode::LEFT);
    s.tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    assert_eq!(ctrl(&mut s.tree, 'c'), None);
    assert!(s.tree.has_clipboard());
    assert_eq!(s.tree.value(NAME), Some("hello world"));
    // Pasted over the start: "world" replaces "hello".
    key(&mut s.tree, KeyCode::HOME);
    s.tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    chord(&mut s.tree, KeyCode::LEFT_CTRL, KeyCode::RIGHT);
    s.tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    assert_eq!(ctrl(&mut s.tree, 'v'), Some(UiEvent::ValueChanged(NAME)));
    assert_eq!(s.tree.value(NAME), Some("world world"));
    // Everything cut, then pasted back.
    ctrl(&mut s.tree, 'a');
    assert_eq!(ctrl(&mut s.tree, 'x'), Some(UiEvent::ValueChanged(NAME)));
    assert_eq!(s.tree.value(NAME), Some(""));
    ctrl(&mut s.tree, 'v');
    assert_eq!(s.tree.value(NAME), Some("world world"));
    // Nothing selected: Ctrl+C keeps what was copied.
    ctrl(&mut s.tree, 'c');
    ctrl(&mut s.tree, 'v');
    assert_eq!(
        s.tree.value(NAME),
        Some("world world"),
        "a paste longer than the room left is refused"
    );
}

#[test]
fn a_paste_is_refused_where_the_input_does_not_take_its_characters() {
    let mut s = screen();
    type_text(&mut s.tree, "ab12");
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'c');
    s.tree.set_focus(Some(s.code)).unwrap();
    assert_eq!(ctrl(&mut s.tree, 'v'), None, "letters into a digits input");
    assert_eq!(s.tree.value(CODE), Some(""));
    // Digits go in.
    s.tree.set_focus(Some(s.name)).unwrap();
    s.tree.set_value(NAME, "4471").unwrap();
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'c');
    s.tree.set_focus(Some(s.code)).unwrap();
    assert_eq!(ctrl(&mut s.tree, 'v'), Some(UiEvent::ValueChanged(CODE)));
    assert_eq!(s.tree.value(CODE), Some("4471"));
}

#[test]
fn a_text_area_copies_cuts_and_pastes_lines() {
    let mut s = screen();
    s.tree.set_value(NOTE, "first line\nsecond").unwrap();
    s.tree.set_focus(Some(s.note)).unwrap();
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'c');
    key(&mut s.tree, KeyCode::END);
    key(&mut s.tree, KeyCode::ENTER);
    assert_eq!(ctrl(&mut s.tree, 'v'), Some(UiEvent::ValueChanged(NOTE)));
    assert_eq!(
        s.tree.value(NOTE),
        Some("first line\nsecond\nfirst line\nsecond")
    );
    // A line break cannot go into a one-line input.
    s.tree.set_focus(Some(s.name)).unwrap();
    assert_eq!(ctrl(&mut s.tree, 'v'), None);
    assert_eq!(s.tree.value(NAME), Some(""));
    // Cut empties the area.
    s.tree.set_focus(Some(s.note)).unwrap();
    ctrl(&mut s.tree, 'a');
    assert_eq!(ctrl(&mut s.tree, 'x'), Some(UiEvent::ValueChanged(NOTE)));
    assert_eq!(s.tree.value(NOTE), Some(""));
}

#[test]
fn a_press_selects_selectable_text_and_ctrl_c_copies_it() {
    let mut s = screen();
    // Text not marked selectable is not selected.
    tap(&mut s.tree, s.title);
    assert_eq!(s.tree.selected_text(), None);
    assert_eq!(s.tree.focused(), Some(s.name));
    // Pressed, the reference is selected and the input loses focus.
    tap(&mut s.tree, s.reference);
    assert_eq!(s.tree.selected_text(), Some(s.reference));
    assert_eq!(s.tree.focused(), None);
    ctrl(&mut s.tree, 'c');
    assert!(s.tree.has_clipboard());
    // A press elsewhere clears the selection; the copy pastes.
    tap(&mut s.tree, s.name);
    assert_eq!(s.tree.selected_text(), None);
    ctrl(&mut s.tree, 'v');
    assert_eq!(s.tree.value(NAME), Some("Ref 4471"));
}

#[test]
fn the_clipboard_is_wiped_at_a_session_wipe_and_past_its_life() {
    let mut s = screen();
    type_text(&mut s.tree, "private");
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'c');
    assert!(!s.tree.clipboard_is_wiped());
    s.tree.wipe_session();
    assert!(!s.tree.has_clipboard());
    assert!(s.tree.clipboard_is_wiped(), "zeros, not just emptied");
    // A product's life: kept until it has passed.
    s.tree.set_clipboard_life(Some(1000));
    s.tree.set_focus(Some(s.name)).unwrap();
    type_text(&mut s.tree, "again");
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'c');
    s.tree.pass_time(999);
    assert!(s.tree.has_clipboard());
    s.tree.pass_time(1);
    assert!(!s.tree.has_clipboard());
    assert!(s.tree.clipboard_is_wiped());
}

#[test]
fn debug_never_shows_what_is_copied() {
    let mut s = screen();
    type_text(&mut s.tree, "secret");
    ctrl(&mut s.tree, 'a');
    ctrl(&mut s.tree, 'x');
    assert_eq!(s.tree.value(NAME), Some(""));
    assert!(s.tree.has_clipboard());
    let shown = format!("{:?}", s.tree);
    assert!(shown.contains("Clipboard"));
    assert!(!shown.contains("secret"));
}
