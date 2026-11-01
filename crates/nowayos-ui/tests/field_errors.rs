// SPDX-License-Identifier: MIT OR Apache-2.0
//! Field errors through the builder (contract v12): one error line
//! per field, empty until the application sets an error; the application,
//! having checked a step, focuses the first field in error.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use nowayos_display::Rect;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, FieldId, Input, InputMode, TextMetrics, Tree, UiError, UiEvent};

const NAME: FieldId = FieldId(1);
const PHONE: FieldId = FieldId(2);
const NEXT: ActionId = ActionId(1);

#[test]
fn the_application_sets_errors_and_focuses_the_first_field_in_error() {
    let mut tree: Tree<16> = Tree::new();
    let name = UiBuilder::input(&mut tree, NAME, Input::new()).unwrap();
    let name_error = UiBuilder::field_error(&mut tree, NAME).unwrap();
    let phone = UiBuilder::input(&mut tree, PHONE, Input::new().mode(InputMode::Digits)).unwrap();
    let phone_error = UiBuilder::field_error(&mut tree, PHONE).unwrap();
    let next = UiBuilder::button(&mut tree, "Next", NEXT).unwrap();
    let root = UiBuilder::column(
        &mut tree,
        Layout::new().padding(8).gap(4),
        &[name, name_error, phone, phone_error, next],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 400, 300), &TextMetrics::new(6, 10))
        .unwrap();
    // Each error line takes a line while empty, so an error can show and go
    // without a new layout.
    let line = tree.get(phone_error).unwrap().rect();
    assert_eq!(line.size.height, 10);
    assert_eq!(tree.field_error_message(PHONE), None);
    // The person presses Next with nothing typed; the application checks
    // the step, sets its errors and focuses the first of them.
    tree.set_focus(Some(next)).unwrap();
    tree.set_field_error(PHONE, "Enter a phone number").unwrap();
    tree.set_field_error(NAME, "Enter your name").unwrap();
    assert_eq!(tree.focus_first_error(), Some(name));
    assert_eq!(tree.get(phone_error).unwrap().rect(), line);
    // Once the name is right, the phone is first.
    tree.clear_field_error(NAME).unwrap();
    assert_eq!(tree.focus_first_error(), Some(phone));
    assert_eq!(
        tree.handle(nowayos_ui::InputEvent::KeyDown(
            nowayos_input::KeyCode::DIGIT_1
        )),
        Some(UiEvent::ValueChanged(PHONE))
    );
}

#[test]
fn a_field_has_at_most_one_error_line() {
    let mut tree: Tree<4> = Tree::new();
    UiBuilder::field_error(&mut tree, NAME).unwrap();
    assert_eq!(
        UiBuilder::field_error(&mut tree, NAME),
        Err(UiError::SecondFieldError(NAME))
    );
    UiBuilder::field_error(&mut tree, PHONE).unwrap();
}
