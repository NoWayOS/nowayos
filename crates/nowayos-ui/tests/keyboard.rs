// SPDX-License-Identifier: MIT OR Apache-2.0
//! The on-screen keyboard: keys drawn as buttons that send canonical
//! key events to the focused text input without taking focus from it, in a
//! layout that follows that input's mode - letters with Shift, digits, or
//! letters with `@` and `.` - offering no suggestions and holding no copy
//! of what is typed.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::keyboard::{KEY_ROWS, layout};
use nowayos_ui::{
    ActionId, FieldId, Input, InputMode, NodeId, NodeKind, TextMetrics, Tree, UiError, UiEvent,
};

const NAME: FieldId = FieldId(1);
const PHONE: FieldId = FieldId(2);
const EMAIL: FieldId = FieldId(3);
const NEXT: ActionId = ActionId(1);

struct Screen {
    tree: Tree<16>,
    name: NodeId,
    phone: NodeId,
    email: NodeId,
    next: NodeId,
    keyboard: NodeId,
}

/// A name, a phone number and an address, a button and the keyboard, at
/// 800 by 480; the name focused, as a screen starts.
fn screen() -> Screen {
    let mut tree: Tree<16> = Tree::new();
    let name = UiBuilder::input(&mut tree, NAME, Input::new().max_len(8)).unwrap();
    let phone = UiBuilder::input(
        &mut tree,
        PHONE,
        Input::new().max_len(15).mode(InputMode::Digits),
    )
    .unwrap();
    let email = UiBuilder::input(&mut tree, EMAIL, Input::new().mode(InputMode::Email)).unwrap();
    let next = UiBuilder::button(&mut tree, "Next", NEXT).unwrap();
    let keyboard = UiBuilder::keyboard(&mut tree).unwrap();
    let root = UiBuilder::column(
        &mut tree,
        Layout::new().padding(16).gap(8),
        &[name, phone, email, next, keyboard],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    let metrics = TextMetrics::new(10, 24);
    tree.layout(Rect::new(0, 0, 800, 480), &metrics).unwrap();
    tree.set_focus(Some(name)).unwrap();
    Screen {
        tree,
        name,
        phone,
        email,
        next,
        keyboard,
    }
}

/// The index of the key labelled `label` in the keyboard's layout now.
fn key_index(tree: &Tree<16>, keyboard: NodeId, label: &str) -> usize {
    let NodeKind::Keyboard { mode, .. } = *tree.get(keyboard).unwrap().kind() else {
        panic!("not a keyboard");
    };
    layout(mode)
        .keys()
        .position(|key| key.label == label)
        .unwrap_or_else(|| panic!("no key {label:?} in {mode:?}"))
}

fn centre(rect: Rect) -> Point {
    Point::new(
        i32::try_from(rect.left()).unwrap() + i32::try_from(rect.size.width / 2).unwrap(),
        i32::try_from(rect.top()).unwrap() + i32::try_from(rect.size.height / 2).unwrap(),
    )
}

fn touch(tree: &mut Tree<16>, at: Point, phase: TouchPhase) -> Option<UiEvent> {
    tree.handle(InputEvent::Touch {
        id: 0,
        x: at.x,
        y: at.y,
        phase,
    })
}

/// Taps the key labelled `label`, returning what its release reported.
fn tap(s: &mut Screen, label: &str) -> Option<UiEvent> {
    let index = key_index(&s.tree, s.keyboard, label);
    let at = centre(s.tree.get(s.keyboard).unwrap().key_rect(index).unwrap());
    assert_eq!(touch(&mut s.tree, at, TouchPhase::Start), None, "{label}");
    touch(&mut s.tree, at, TouchPhase::End)
}

fn mode(tree: &Tree<16>, keyboard: NodeId) -> InputMode {
    match *tree.get(keyboard).unwrap().kind() {
        NodeKind::Keyboard { mode, .. } => mode,
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn its_layout_follows_the_focused_inputs_mode() {
    let mut s = screen();
    assert_eq!(mode(&s.tree, s.keyboard), InputMode::Text);
    assert_eq!(s.tree.typing_target(), Some(s.name));
    s.tree.set_focus(Some(s.phone)).unwrap();
    assert_eq!(mode(&s.tree, s.keyboard), InputMode::Digits);
    s.tree.set_focus(Some(s.email)).unwrap();
    assert_eq!(mode(&s.tree, s.keyboard), InputMode::Email);
    // On a button the keyboard keeps its layout, but types nothing.
    s.tree.set_focus(Some(s.next)).unwrap();
    assert_eq!(mode(&s.tree, s.keyboard), InputMode::Email);
    assert_eq!(s.tree.typing_target(), None);
    // Tab passes it by: it is not a tab stop.
    assert_eq!(
        s.tree.handle(InputEvent::KeyDown(KeyCode::TAB)),
        Some(UiEvent::FocusChanged(Some(s.name)))
    );
    assert_eq!(mode(&s.tree, s.keyboard), InputMode::Text);
}

#[test]
fn a_key_types_into_the_focused_input_without_taking_focus() {
    let mut s = screen();
    assert_eq!(tap(&mut s, "z"), Some(UiEvent::ValueChanged(NAME)));
    assert_eq!(s.tree.focused(), Some(s.name));
    // Shift makes the next letter a capital, then goes off.
    assert_eq!(tap(&mut s, "Shift"), None);
    assert_eq!(tap(&mut s, "q"), Some(UiEvent::ValueChanged(NAME)));
    tap(&mut s, "v");
    tap(&mut s, "Space");
    tap(&mut s, "'");
    tap(&mut s, "-");
    assert_eq!(s.tree.value(NAME), Some("zQv '-"));
    // Shift pressed twice is off again.
    tap(&mut s, "Shift");
    tap(&mut s, "Shift");
    tap(&mut s, "j");
    assert_eq!(s.tree.value(NAME), Some("zQv '-j"));
    assert_eq!(tap(&mut s, "Delete"), Some(UiEvent::ValueChanged(NAME)));
    assert_eq!(s.tree.value(NAME), Some("zQv '-"));
    // Full at eight bytes, as from a keyboard.
    tap(&mut s, ",");
    tap(&mut s, ".");
    assert_eq!(tap(&mut s, "z"), Some(UiEvent::InputFull(NAME)));
    assert_eq!(s.tree.focused(), Some(s.name));
}

#[test]
fn the_digits_and_address_layouts_type_what_their_modes_take() {
    let mut s = screen();
    s.tree.set_focus(Some(s.phone)).unwrap();
    for label in ["9", "0", "8", "1", "7", "2", "6", "3", "5", "4"] {
        assert_eq!(tap(&mut s, label), Some(UiEvent::ValueChanged(PHONE)));
    }
    tap(&mut s, "Delete");
    assert_eq!(s.tree.value(PHONE), Some("908172635"));
    s.tree.set_focus(Some(s.email)).unwrap();
    for label in ["z", "q", "_", "v", "@", "j", ".", "-"] {
        assert_eq!(tap(&mut s, label), Some(UiEvent::ValueChanged(EMAIL)));
    }
    assert_eq!(s.tree.value(EMAIL), Some("zq_v@j.-"));
    // The address layout has no space bar: an address has no space.
    assert!(
        !layout(InputMode::Email)
            .keys()
            .any(|key| key.label == "Space")
    );
}

#[test]
fn with_no_text_input_focused_its_keys_type_nothing() {
    let mut s = screen();
    s.tree.set_focus(Some(s.next)).unwrap();
    let index = key_index(&s.tree, s.keyboard, "z");
    let at = centre(s.tree.get(s.keyboard).unwrap().key_rect(index).unwrap());
    assert_eq!(touch(&mut s.tree, at, TouchPhase::Start), None);
    assert_eq!(s.tree.pressed(), None);
    assert_eq!(touch(&mut s.tree, at, TouchPhase::End), None);
    assert_eq!(s.tree.focused(), Some(s.next));
    assert!(s.tree.is_wiped());
}

#[test]
fn a_press_moved_off_its_key_types_nothing() {
    let mut s = screen();
    let node = s.tree.get(s.keyboard).unwrap();
    let z = centre(node.key_rect(key_index(&s.tree, s.keyboard, "z")).unwrap());
    let x = centre(node.key_rect(key_index(&s.tree, s.keyboard, "x")).unwrap());
    touch(&mut s.tree, z, TouchPhase::Start);
    assert_eq!(s.tree.pressed(), Some(s.keyboard));
    assert_eq!(
        s.tree.pressed_key(),
        Some(key_index(&s.tree, s.keyboard, "z"))
    );
    touch(&mut s.tree, x, TouchPhase::Move);
    assert_eq!(touch(&mut s.tree, x, TouchPhase::End), None);
    assert_eq!(s.tree.pressed_key(), None);
    assert_eq!(s.tree.value(NAME), Some(""));
    // With the pointer too: press, release over the same key.
    s.tree.handle(InputEvent::PointerMove { x: z.x, y: z.y });
    s.tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    assert_eq!(
        s.tree.handle(InputEvent::PointerButton {
            button: 0,
            down: false,
        }),
        Some(UiEvent::ValueChanged(NAME))
    );
}

#[test]
fn it_holds_no_copy_of_what_is_typed_and_a_wipe_leaves_nothing() {
    let mut s = screen();
    for label in ["Shift", "z", "q", "v", "j"] {
        tap(&mut s, label);
    }
    tap(&mut s, "Shift");
    assert_eq!(s.tree.value(NAME), Some("Zqvj"));
    // Its whole storage: the layout it shows and its Shift - no text.
    assert_eq!(
        *s.tree.get(s.keyboard).unwrap().kind(),
        NodeKind::Keyboard {
            mode: InputMode::Text,
            shift: true,
        }
    );
    assert!(!s.tree.is_wiped());
    s.tree.wipe_session();
    assert!(s.tree.is_wiped());
    assert_eq!(
        *s.tree.get(s.keyboard).unwrap().kind(),
        NodeKind::Keyboard {
            mode: InputMode::Text,
            shift: false,
        }
    );
}

#[test]
fn its_keys_are_targets_and_a_press_repaints_only_its_key() {
    let mut s = screen();
    for input in [s.name, s.phone, s.email] {
        s.tree.set_focus(Some(input)).unwrap();
        let node = s.tree.get(s.keyboard).unwrap();
        let NodeKind::Keyboard { mode, .. } = *node.kind() else {
            panic!("not a keyboard");
        };
        let keys = layout(mode).keys().count();
        for index in 0..keys {
            let rect = node.key_rect(index).unwrap();
            assert!(
                rect.size.width >= 24 && rect.size.height >= 24,
                "{mode:?} {index}"
            );
            assert!(node.rect().contains(centre(rect)));
            assert_eq!(node.key_at(centre(rect)), Some(index));
        }
        assert!(node.key_rect(keys).is_none());
        // Four rows of the key height and its padding: 24 + 12, and 6.
        assert_eq!(node.rect().size.height, u32::from(KEY_ROWS) * 42 + 6);
        assert_eq!(node.cut_text(&TextMetrics::new(10, 24)), None);
    }
    s.tree.set_focus(Some(s.name)).unwrap();
    s.tree.clear_dirty();
    let index = key_index(&s.tree, s.keyboard, "z");
    let key = s.tree.get(s.keyboard).unwrap().key_rect(index).unwrap();
    touch(&mut s.tree, centre(key), TouchPhase::Start);
    assert_eq!(s.tree.dirty_rects(), &[key]);
}

#[test]
fn a_screen_has_at_most_one() {
    let mut tree: Tree<4> = Tree::new();
    UiBuilder::keyboard(&mut tree).unwrap();
    assert_eq!(UiBuilder::keyboard(&mut tree), Err(UiError::SecondKeyboard));
}
