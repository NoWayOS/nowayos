// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text inputs with a capacity and a mode of their own, and the value
//! controls of builder contract v1: a key for a character the mode
//! refuses, or one more than `max_len` holds, is refused; a value set by the
//! application is held to the same rules; each field number is bound once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_bounded::BoundedStr;
use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{
    FieldId, INPUT_CAPACITY, Input, InputMode, NodeKind, Style, TextMetrics, Tree, UiError, UiEvent,
};

const METRICS: TextMetrics = TextMetrics::new(6, 10);
const REF: FieldId = FieldId(1);

/// A tree holding one focused input made as `input` says.
fn focused(input: Input) -> Tree<4> {
    let mut tree: Tree<4> = Tree::new();
    let node = UiBuilder::input(&mut tree, REF, input).unwrap();
    let root = UiBuilder::column(&mut tree, ui::Layout::new(), &[node]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 200, 40), &METRICS).unwrap();
    tree.set_focus(Some(node)).unwrap();
    tree
}

fn key(tree: &mut Tree<4>, key: KeyCode) -> Option<UiEvent> {
    tree.handle(InputEvent::KeyDown(key))
}

#[test]
fn a_digits_input_takes_digits_only_up_to_its_length() {
    let mut t = focused(Input::new().max_len(3).mode(InputMode::Digits));
    assert_eq!(key(&mut t, KeyCode::letter('a').unwrap()), None);
    assert_eq!(key(&mut t, KeyCode::SPACE), None);
    for digit in [4, 2, 7] {
        assert_eq!(
            key(&mut t, KeyCode::digit(digit).unwrap()),
            Some(UiEvent::ValueChanged(REF))
        );
    }
    assert_eq!(
        key(&mut t, KeyCode::digit(1).unwrap()),
        Some(UiEvent::InputFull(REF)),
        "a fourth digit is one more than it takes"
    );
    assert_eq!(t.value(REF), Some("427"));
    assert_eq!(
        key(&mut t, KeyCode::BACKSPACE),
        Some(UiEvent::ValueChanged(REF))
    );
    assert_eq!(t.value(REF), Some("42"));
}

#[test]
fn an_email_input_refuses_the_space_and_takes_the_rest_of_ascii() {
    let mut t = focused(Input::new().mode(InputMode::Email));
    key(&mut t, KeyCode::letter('a').unwrap());
    assert_eq!(key(&mut t, KeyCode::SPACE), None);
    t.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    key(&mut t, KeyCode::digit(2).unwrap()); // Shift+2: '@'
    t.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
    key(&mut t, KeyCode::letter('b').unwrap());
    assert_eq!(t.value(REF), Some("a@b"));
    assert!(InputMode::Email.accepts('.') && !InputMode::Email.accepts('\u{e9}'));
    assert!(InputMode::Text.accepts('\u{e9}') && InputMode::Text.accepts(' '));
    assert!(
        !InputMode::Text.accepts('\u{200B}'),
        "text mode is printable only"
    );
}

#[test]
fn a_value_the_application_sets_is_held_to_the_same_rules() {
    let mut t = focused(Input::new().max_len(4).mode(InputMode::Digits));
    t.set_value(REF, "1234").unwrap();
    // Beyond the input's length - even beyond what any input holds - the
    // error names the input's own capacity.
    for long in ["12345", "1234567890".repeat(7).as_str()] {
        assert!(
            matches!(
                t.set_value(REF, long),
                Err(UiError::ValueTooLong(err)) if err.capacity == 4 && err.required == long.len()
            ),
            "{} bytes",
            long.len()
        );
    }
    assert_eq!(t.set_value(REF, "12a"), Err(UiError::NotInInputMode));
    assert_eq!(t.value(REF), Some("1234"), "the previous value stays");
    // Bytes, not characters: 'é' is two.
    let mut text = focused(Input::new().max_len(2));
    text.set_value(REF, "\u{e9}").unwrap();
    assert!(matches!(
        text.set_value(REF, "a\u{e9}"),
        Err(UiError::ValueTooLong(_))
    ));
    // Wiped like any value.
    t.wipe_session();
    assert!(t.is_wiped());
}

#[test]
fn a_length_outside_one_to_the_capacity_is_refused_when_made() {
    let mut t: Tree<4> = Tree::new();
    for max_len in [0, INPUT_CAPACITY as u16 + 1, u16::MAX] {
        assert_eq!(
            t.input(REF, Input::new().max_len(max_len)),
            Err(UiError::MaxLenOutOfRange {
                max: INPUT_CAPACITY
            })
        );
    }
    assert!(t.is_empty());
    t.input(REF, Input::new().max_len(1)).unwrap();
    t.input(FieldId(2), Input::new().max_len(INPUT_CAPACITY as u16))
        .unwrap();
    // A node made directly is checked the same way, value and all.
    let node = |max_len, mode, text: &str| NodeKind::TextInput {
        field: FieldId(3),
        placeholder: "",
        value: BoundedStr::try_from_str(text).unwrap(),
        max_len,
        mode,
        caret: 0,
        anchor: 0,
    };
    let mut u: Tree<4> = Tree::new();
    assert!(matches!(
        u.add(node(0, InputMode::Text, ""), Style::new()),
        Err(UiError::MaxLenOutOfRange { .. })
    ));
    assert!(matches!(
        u.add(node(2, InputMode::Text, "abc"), Style::new()),
        Err(UiError::ValueTooLong(_))
    ));
    assert_eq!(
        u.add(node(8, InputMode::Digits, "12x"), Style::new()),
        Err(UiError::NotInInputMode)
    );
    assert!(u.is_empty());
}

#[test]
fn a_v0_text_input_is_a_v1_input_with_the_defaults() {
    let mut t: Tree<4> = Tree::new();
    let v0 = t.text_input(REF, "Name").unwrap();
    let v1 = t
        .input(FieldId(2), Input::new().placeholder("Name"))
        .unwrap();
    let kind = |id| t.get(id).unwrap().kind().clone();
    match (kind(v0), kind(v1)) {
        (
            NodeKind::TextInput {
                max_len: a,
                mode: m,
                placeholder: p,
                ..
            },
            NodeKind::TextInput {
                max_len: b,
                mode: n,
                placeholder: q,
                ..
            },
        ) => {
            assert_eq!((a, m, p), (b, n, q));
            assert_eq!(usize::from(a), INPUT_CAPACITY);
            assert_eq!(m, InputMode::Text);
        }
        other => panic!("not two text inputs: {other:?}"),
    }
    assert_eq!(Input::default(), Input::new());
}

#[test]
fn the_builder_binds_each_field_number_once() {
    let mut t: Tree<8> = Tree::new();
    UiBuilder::input(&mut t, REF, Input::new()).unwrap();
    assert_eq!(
        UiBuilder::checkbox(&mut t, REF, "Agree"),
        Err(UiError::DuplicateField(REF))
    );
    assert_eq!(
        UiBuilder::radio_group(&mut t, REF, &["One", "Two"]),
        Err(UiError::DuplicateField(REF))
    );
    UiBuilder::checkbox(&mut t, FieldId(2), "Agree").unwrap();
    assert_eq!(
        UiBuilder::input(&mut t, FieldId(2), Input::new()),
        Err(UiError::DuplicateField(FieldId(2)))
    );
    UiBuilder::radio_group(&mut t, FieldId(3), &["One", "Two"]).unwrap();
    assert_eq!(t.len(), 3, "nothing was created on a refusal");
    // A field's error line shares its number: it is not a value.
    t.field_error(REF).unwrap();
    // The builder's controls are the tree's own.
    assert!(t.has_field(FieldId(3)) && !t.has_field(FieldId(4)));
    assert_eq!(t.checked(FieldId(2)), Some(false));
    assert_eq!(t.selected(FieldId(3)), None);
    assert_eq!(
        UiBuilder::radio_group(&mut t, FieldId(4), &[]),
        Err(UiError::NoOptions)
    );
}

#[test]
fn a_v0_text_input_never_takes_a_field_a_v1_control_holds() {
    // v0's text inputs number themselves 1, 2, … by count: the first would
    // be field 1, which the checkbox holds.
    let mut t: Tree<8> = Tree::new();
    UiBuilder::checkbox(&mut t, REF, "Agree").unwrap();
    assert_eq!(
        UiBuilder::text_input(&mut t, None),
        Err(UiError::DuplicateField(REF))
    );
    assert_eq!(t.len(), 1, "nothing was created");
    // Without a clash it numbers as before.
    let mut u: Tree<8> = Tree::new();
    let first = UiBuilder::text_input(&mut u, None).unwrap();
    UiBuilder::checkbox(&mut u, FieldId(9), "Agree").unwrap();
    let second = UiBuilder::text_input(&mut u, Some("Two")).unwrap();
    assert_eq!(u.field_of(first), Some(REF));
    assert_eq!(u.field_of(second), Some(FieldId(2)));
}
