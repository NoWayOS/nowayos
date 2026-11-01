// SPDX-License-Identifier: MIT OR Apache-2.0
//! A list picker: one choice of up to 64 options in a list of 3 to
//! 8 rows that scrolls inside itself, with one tab stop; the arrows move a
//! highlight, Enter or Space chooses it, a tap on a row chooses that one, a
//! touch dragged on the list scrolls it, and the application sets a choice
//! back; a session's wipe clears it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::builder::{Layout, Picker, UiBuilder};
use nowayos_ui::{FieldId, NodeId, NodeKind, TextMetrics, Tree, UiError, UiEvent};

const KIND: FieldId = FieldId(1);

const OPTIONS: &[&str] = &[
    "Bag", "Phone", "Keys", "Wallet", "Glasses", "Umbrella", "Coat", "Watch", "Book", "Toy",
    "Other",
];

fn screen() -> (Tree<16>, NodeId) {
    let mut tree: Tree<16> = Tree::new();
    let picker = UiBuilder::list_picker(
        &mut tree,
        KIND,
        OPTIONS,
        Picker::new().placeholder("Choose one"),
    )
    .unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[picker]).unwrap();
    tree.set_root(root).unwrap();
    let metrics = TextMetrics::new(6, 10).with_min_target(30);
    tree.layout(Rect::new(0, 0, 300, 400), &metrics).unwrap();
    (tree, picker)
}

/// The picker's first option showing, highlighted option and choice.
fn state(tree: &Tree<16>, id: NodeId) -> (u8, u8, Option<u8>) {
    match tree.get(id).unwrap().kind() {
        NodeKind::ListPicker {
            top,
            cursor,
            selected,
            ..
        } => (*top, *cursor, *selected),
        other => panic!("{other:?}"),
    }
}

fn key(tree: &mut Tree<16>, key: KeyCode) -> Option<UiEvent> {
    tree.handle(InputEvent::KeyDown(key))
}

#[test]
fn it_is_a_header_and_its_rows_and_refuses_what_is_out_of_range() {
    let (tree, picker) = screen();
    let node = tree.get(picker).unwrap();
    // A header and five rows, 30 each, inside the choice padding.
    let rows: Vec<Rect> = (0..6).map(|row| node.list_row(row).unwrap()).collect();
    assert!(rows.iter().all(|row| row.size.height == 30));
    assert!(node.list_row(6).is_none());
    let mut tree: Tree<8> = Tree::new();
    let many: &'static [&'static str] = Box::leak(vec!["x"; 65].into_boxed_slice());
    assert_eq!(
        UiBuilder::list_picker(&mut tree, KIND, many, Picker::new()),
        Err(UiError::TooManyOptions { max: 64 })
    );
    assert_eq!(
        UiBuilder::list_picker(&mut tree, KIND, &[], Picker::new()),
        Err(UiError::NoOptions)
    );
    for rows in [2, 9] {
        assert_eq!(
            UiBuilder::list_picker(&mut tree, KIND, OPTIONS, Picker::new().rows(rows)),
            Err(UiError::RowsOutOfRange)
        );
    }
    UiBuilder::list_picker(&mut tree, KIND, OPTIONS, Picker::new().rows(3)).unwrap();
    assert_eq!(
        UiBuilder::list_picker(&mut tree, KIND, OPTIONS, Picker::new()),
        Err(UiError::DuplicateField(KIND))
    );
}

#[test]
fn the_arrows_move_the_highlight_and_enter_or_space_chooses_it() {
    let (mut tree, picker) = screen();
    tree.set_focus(Some(picker)).unwrap();
    // Up at the top stays; Down moves; past the fifth row the list scrolls.
    assert_eq!(key(&mut tree, KeyCode::UP), None);
    assert_eq!(state(&tree, picker), (0, 0, None));
    for _ in 0..6 {
        assert_eq!(key(&mut tree, KeyCode::DOWN), None);
    }
    assert_eq!(state(&tree, picker), (2, 6, None));
    // Focus stayed: the picker is one tab stop.
    assert_eq!(tree.focused(), Some(picker));
    assert_eq!(
        key(&mut tree, KeyCode::ENTER),
        Some(UiEvent::Selected {
            field: KIND,
            option: 6
        })
    );
    // The same again changes nothing.
    assert_eq!(key(&mut tree, KeyCode::SPACE), None);
    // To the end, and no further.
    for _ in 0..20 {
        key(&mut tree, KeyCode::DOWN);
    }
    assert_eq!(state(&tree, picker), (6, 10, Some(6)));
    assert_eq!(
        key(&mut tree, KeyCode::SPACE),
        Some(UiEvent::Selected {
            field: KIND,
            option: 10
        })
    );
    assert_eq!(tree.selected(KIND), Some(10));
}

fn centre(rect: Rect) -> (i32, i32) {
    (
        i32::try_from(rect.left()).unwrap() + i32::try_from(rect.size.width / 2).unwrap(),
        i32::try_from(rect.top()).unwrap() + i32::try_from(rect.size.height / 2).unwrap(),
    )
}

#[test]
fn a_tap_on_a_row_chooses_it_and_a_drag_scrolls_the_list() {
    let (mut tree, picker) = screen();
    let (x, y) = centre(tree.get(picker).unwrap().list_row(3).unwrap());
    let touch = |y, phase| InputEvent::Touch { id: 0, x, y, phase };
    tree.handle(touch(y, TouchPhase::Start));
    assert_eq!(
        tree.handle(touch(y, TouchPhase::End)),
        Some(UiEvent::Selected {
            field: KIND,
            option: 2
        })
    );
    // The header chooses nothing.
    let (hx, hy) = centre(tree.get(picker).unwrap().list_row(0).unwrap());
    assert_eq!(tree.control_at(Point::new(hx, hy)), Some(picker));
    tree.handle(InputEvent::Touch {
        id: 0,
        x: hx,
        y: hy,
        phase: TouchPhase::Start,
    });
    assert_eq!(
        tree.handle(InputEvent::Touch {
            id: 0,
            x: hx,
            y: hy,
            phase: TouchPhase::End
        }),
        None
    );
    // Dragged up three rows: the list shows from the fourth option, and
    // lifting the finger chooses nothing.
    tree.handle(touch(y, TouchPhase::Start));
    tree.handle(touch(y - 90, TouchPhase::Move));
    assert_eq!(state(&tree, picker).0, 3);
    assert_eq!(tree.pressed(), None);
    assert_eq!(tree.handle(touch(y - 90, TouchPhase::End)), None);
    // Never past the last page.
    tree.handle(touch(y, TouchPhase::Start));
    tree.handle(touch(y - 900, TouchPhase::Move));
    assert_eq!(state(&tree, picker).0, 6);
    tree.handle(touch(y - 900, TouchPhase::End));
}

#[test]
fn the_application_sets_a_choice_back_and_a_wipe_clears_it() {
    let (mut tree, picker) = screen();
    tree.set_selected(KIND, Some(9)).unwrap();
    // Chosen, highlighted and in view.
    assert_eq!(state(&tree, picker), (5, 9, Some(9)));
    assert_eq!(tree.selected(KIND), Some(9));
    assert_eq!(
        tree.set_selected(KIND, Some(11)),
        Err(UiError::NoSuchOption)
    );
    assert!(!tree.is_wiped());
    tree.wipe_session();
    assert_eq!(state(&tree, picker), (0, 0, None));
    assert!(tree.is_wiped());
}
