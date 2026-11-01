// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for ending a session: whatever a person typed,
//! ticked or chose, and whatever the application showed them, on whichever
//! screens, a reset leaves no input or text slot holding data - every byte
//! of every value zeroed, checked with `BoundedStr::is_zeroed` - and shows
//! the start screen afresh.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::{
    ActionId, FieldId, Navigator, NodeId, ScreenId, SlotId, Style, TextMetrics, Tree, UiError,
};
use proptest::prelude::*;

const CAP: usize = 16;
const VIEWPORT: Rect = Rect::new(0, 0, 200, 240);
const METRICS: TextMetrics = TextMetrics::new(6, 10);

const START: ScreenId = ScreenId(0);
const STEP: ScreenId = ScreenId(1);
const REVIEW: ScreenId = ScreenId(2);

/// Synthetic screens holding every kind of value: text inputs, a checkbox,
/// a radio group, slots and an error line.
fn build(screen: ScreenId, tree: &mut Tree<CAP>) -> Result<NodeId, UiError> {
    let column = tree.column(Style::new().padding(4).gap(2))?;
    let title = tree.text("Title")?;
    tree.append(column, title)?;
    let add = |tree: &mut Tree<CAP>, id: NodeId| tree.append(column, id);
    match screen {
        START => {
            let name = tree.text_input(FieldId(0), "Name")?;
            add(tree, name)?;
            let copy = tree.checkbox(FieldId(1), "Copy")?;
            add(tree, copy)?;
        }
        STEP => {
            let code = tree.text_input(FieldId(2), "Code")?;
            add(tree, code)?;
            let error = tree.field_error(FieldId(2))?;
            add(tree, error)?;
            let when = tree.radio_group(FieldId(3), &["Now", "Later", "Never"])?;
            add(tree, when)?;
            let echo = tree.slot(SlotId(0))?;
            add(tree, echo)?;
        }
        REVIEW => {
            for slot in 0..3 {
                let shown = tree.slot(SlotId(slot))?;
                add(tree, shown)?;
            }
        }
        _ => return Err(UiError::NoRoot),
    }
    let next = tree.button("Next", ActionId(1))?;
    add(tree, next)?;
    Ok(column)
}

#[derive(Debug, Clone)]
enum Op {
    Type(u16, String),
    Show(u16, String),
    Tick(bool),
    Choose(Option<u8>),
    Error,
    Go(u16),
    Replace(u16),
    Back,
    Key(u16),
    Tap(i32, i32),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u16..4, "[ -~]{0,20}").prop_map(|(f, t)| Op::Type(f, t)),
        2 => (0u16..3, "[ -~]{0,30}").prop_map(|(s, t)| Op::Show(s, t)),
        1 => any::<bool>().prop_map(Op::Tick),
        1 => proptest::option::of(0u8..3).prop_map(Op::Choose),
        1 => Just(Op::Error),
        2 => (0u16..3).prop_map(Op::Go),
        1 => (0u16..3).prop_map(Op::Replace),
        1 => Just(Op::Back),
        3 => (0x04u16..=0x52).prop_map(Op::Key),
        2 => (0i32..200, 0i32..240).prop_map(|(x, y)| Op::Tap(x, y)),
    ]
}

/// Runs `ops` on a fresh session, refusals and all.
fn session(ops: &[Op]) -> (Navigator<4>, Tree<CAP>) {
    let mut nav = Navigator::new(START, VIEWPORT, METRICS);
    let mut tree: Tree<CAP> = Tree::new();
    nav.show_start(&mut tree, build).unwrap();
    for op in ops {
        let _ = match op {
            Op::Type(field, text) => tree.set_value(FieldId(*field), text),
            Op::Show(slot, text) => tree.set_slot(SlotId(*slot), text),
            Op::Tick(to) => tree.set_checked(FieldId(1), *to),
            Op::Choose(option) => tree.set_selected(FieldId(3), *option),
            Op::Error => tree.set_field_error(FieldId(2), "Check this"),
            Op::Go(screen) => nav.go(ScreenId(*screen), &mut tree, build),
            Op::Replace(screen) => nav.replace(ScreenId(*screen), &mut tree, build),
            Op::Back => nav.back(&mut tree, build).map(|_| ()),
            Op::Key(code) => {
                if let Some(key) = KeyCode::new(*code) {
                    tree.handle(InputEvent::KeyDown(key));
                }
                Ok(())
            }
            Op::Tap(x, y) => {
                for phase in [TouchPhase::Start, TouchPhase::End] {
                    tree.handle(InputEvent::Touch {
                        id: 0,
                        x: *x,
                        y: *y,
                        phase,
                    });
                }
                Ok(())
            }
        };
    }
    (nav, tree)
}

proptest! {
    #[test]
    fn any_session_ending_in_a_reset_leaves_no_data(ops in proptest::collection::vec(op(), 0..40)) {
        // The wipe a reset starts with, on the session's own nodes: every
        // value and slot zeroed in place, every choice cleared.
        let (_, mut tree) = session(&ops);
        let nodes = tree.len();
        tree.wipe_session();
        prop_assert!(tree.is_wiped());
        prop_assert_eq!(tree.len(), nodes);
        for field in 0..4 {
            prop_assert!(tree.value(FieldId(field)).is_none_or(str::is_empty));
        }
        for slot in 0..3 {
            prop_assert!(tree.slot_text(SlotId(slot)).is_none_or(str::is_empty));
        }

        // The whole reset: the start screen afresh, nothing to go back to,
        // focus on its first control and the whole viewport to repaint.
        let (mut nav, mut tree) = session(&ops);
        tree.clear_dirty();
        nav.reset(&mut tree, build).unwrap();
        prop_assert!(tree.is_wiped());
        prop_assert_eq!(nav.current(), START);
        prop_assert!(nav.back_stack().is_empty());
        prop_assert_eq!(tree.value(FieldId(0)), Some(""));
        prop_assert_eq!(tree.checked(FieldId(1)), Some(false));
        prop_assert_eq!(tree.focused(), tree.find_field(FieldId(0)));
        prop_assert_eq!(tree.dirty_rects(), &[VIEWPORT]);
    }
}
