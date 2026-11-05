// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keyboard layouts (#10081): a tree types on the layout a product names -
//! UK, with AltGr and its dead keys, or US International, with its dead
//! keys - into text inputs and areas; Caps Lock; an input's mode still
//! refuses what it does not take; and an accent waiting for its letter is
//! dropped when focus moves.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use nowayos_display::Rect;
use nowayos_input::keymap::Layout;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout as Spacing, UiBuilder};
use nowayos_ui::{FieldId, Input, InputMode, NodeId, TextMetrics, Tree};

const NAME: FieldId = FieldId(1);
const CODE: FieldId = FieldId(2);
const NOTE: FieldId = FieldId(3);

fn screen(layout: Layout) -> (Tree<8, 256>, [NodeId; 3]) {
    let mut tree: Tree<8, 256> = Tree::new();
    let name = UiBuilder::input(&mut tree, NAME, Input::new().max_len(32)).unwrap();
    let code = UiBuilder::input(
        &mut tree,
        CODE,
        Input::new().max_len(8).mode(InputMode::Digits),
    )
    .unwrap();
    let note = UiBuilder::text_area(&mut tree, NOTE, 3, 200, "").unwrap();
    let root = UiBuilder::column(&mut tree, Spacing::new(), &[name, code, note]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &TextMetrics::new(8, 16))
        .unwrap();
    tree.set_keyboard_layout(layout);
    tree.set_focus(Some(name)).unwrap();
    (tree, [name, code, note])
}

/// Presses the key with HID usage `usage`, with Shift and AltGr held as
/// asked.
fn press(tree: &mut Tree<8, 256>, usage: u16, shift: bool, altgr: bool) {
    let key = KeyCode::new(usage).unwrap();
    if shift {
        tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    }
    if altgr {
        tree.handle(InputEvent::KeyDown(KeyCode::RIGHT_ALT));
    }
    tree.handle(InputEvent::KeyDown(key));
    tree.handle(InputEvent::KeyUp(key));
    if altgr {
        tree.handle(InputEvent::KeyUp(KeyCode::RIGHT_ALT));
    }
    if shift {
        tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    }
}

fn letter(ch: char) -> u16 {
    KeyCode::letter(ch).unwrap().usage()
}

#[test]
fn uk_types_its_symbols_and_accents_with_altgr() {
    let (mut tree, _) = screen(Layout::Uk);
    assert_eq!(tree.keyboard_layout(), Layout::Uk);
    // Zoë: AltGr+2 is a dead diaeresis.
    press(&mut tree, letter('z'), true, false);
    press(&mut tree, letter('o'), false, false);
    press(&mut tree, 0x1F, false, true);
    press(&mut tree, letter('e'), false, false);
    press(&mut tree, 0x2C, false, false);
    // Siobhán: AltGr+a is á.
    for ch in "siobh".chars() {
        press(&mut tree, letter(ch), ch == 's', false);
    }
    press(&mut tree, letter('a'), false, true);
    press(&mut tree, letter('n'), false, false);
    press(&mut tree, 0x2C, false, false);
    // £5 "x" @ #
    press(&mut tree, 0x20, true, false);
    press(&mut tree, 0x22, false, false);
    press(&mut tree, 0x1F, true, false);
    press(&mut tree, 0x34, true, false);
    press(&mut tree, 0x31, false, false);
    assert_eq!(tree.value(NAME), Some("Zoë Siobhán £5\"@#"));
}

#[test]
fn us_international_dead_keys_type_into_inputs_and_areas() {
    let (mut tree, [_, _, note]) = screen(Layout::UsInternational);
    // crème: ` is a dead grave.
    for ch in "cr".chars() {
        press(&mut tree, letter(ch), false, false);
    }
    press(&mut tree, 0x35, false, false);
    for ch in "eme".chars() {
        press(&mut tree, letter(ch), false, false);
    }
    assert_eq!(tree.value(NAME), Some("crème"));
    // In a text area, ~ then n: ñ; ' then t: both.
    tree.set_focus(Some(note)).unwrap();
    press(&mut tree, 0x35, true, false);
    press(&mut tree, letter('n'), false, false);
    press(&mut tree, 0x34, false, false);
    press(&mut tree, letter('t'), false, false);
    assert_eq!(tree.value(NOTE), Some("ñ't"));
}

#[test]
fn caps_lock_capitals_letters_and_a_mode_refuses_what_it_does_not_take() {
    let (mut tree, [_, code, _]) = screen(Layout::Uk);
    press(&mut tree, 0x39, false, false);
    press(&mut tree, letter('a'), false, false);
    press(&mut tree, letter('b'), true, false);
    press(&mut tree, letter('e'), false, true);
    press(&mut tree, 0x39, false, false);
    press(&mut tree, letter('c'), false, false);
    assert_eq!(tree.value(NAME), Some("AbÉc"));
    // A digits input takes no accented letter, and no £.
    tree.set_focus(Some(code)).unwrap();
    press(&mut tree, letter('a'), false, true);
    press(&mut tree, 0x20, true, false);
    press(&mut tree, 0x20, false, false);
    assert_eq!(tree.value(CODE), Some("3"));
}

#[test]
fn an_accent_waiting_is_dropped_when_focus_moves_or_the_session_is_wiped() {
    let (mut tree, [name, code, _]) = screen(Layout::UsInternational);
    press(&mut tree, 0x34, false, false);
    tree.set_focus(Some(code)).unwrap();
    tree.set_focus(Some(name)).unwrap();
    press(&mut tree, letter('e'), false, false);
    assert_eq!(tree.value(NAME), Some("e"));
    press(&mut tree, 0x34, false, false);
    tree.wipe_session();
    tree.set_focus(Some(name)).unwrap();
    press(&mut tree, letter('e'), false, false);
    assert_eq!(tree.value(NAME), Some("e"));
    // US, the default, has no dead keys: ' types itself.
    tree.set_keyboard_layout(Layout::Us);
    press(&mut tree, 0x34, false, false);
    assert_eq!(tree.value(NAME), Some("e'"));
}
