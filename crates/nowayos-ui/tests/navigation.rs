// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for switching screens: whatever the application
//! asks for, the tree always holds exactly one whole screen - the one the
//! navigator says is showing - with its first control focused, and the back
//! stack never grows past its bound.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_ui::{
    ActionId, FieldId, Navigator, NodeId, ScreenId, Style, TextMetrics, Tree, UiError,
};
use proptest::prelude::*;

const CAP: usize = 16;
const DEPTH: usize = 3;
const VIEWPORT: Rect = Rect::new(0, 0, 160, 120);
const METRICS: TextMetrics = TextMetrics::new(6, 10);

/// Screen `n` has `n` text inputs after its title; screens with more nodes
/// than the tree holds fail with `TreeFull`, and screen 13 fails part-way
/// through with another error.
fn build(screen: ScreenId, tree: &mut Tree<CAP>) -> Result<NodeId, UiError> {
    let column = tree.column(Style::new().gap(1))?;
    let title = tree.text("Step")?;
    tree.append(column, title)?;
    for field in 0..screen.get() {
        let input = tree.text_input(FieldId(field), "")?;
        tree.append(column, input)?;
    }
    if screen.get() == 13 {
        return Err(UiError::NoRoot);
    }
    let next = tree.button("Next", ActionId(1))?;
    tree.append(column, next)?;
    Ok(column)
}

/// How many nodes screen `n` has when it can be built.
fn nodes_of(screen: ScreenId) -> usize {
    usize::from(screen.get()) + 3
}

#[derive(Debug, Clone)]
enum Op {
    Go(u16),
    Replace(u16),
    Back,
    Start,
    Type,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0u16..18).prop_map(Op::Go),
        2 => (0u16..18).prop_map(Op::Replace),
        3 => Just(Op::Back),
        1 => Just(Op::Start),
        2 => Just(Op::Type),
    ]
}

proptest! {
    #[test]
    fn the_tree_always_holds_one_whole_screen(
        start in 0u16..6,
        ops in proptest::collection::vec(op(), 0..40),
    ) {
        let start = ScreenId(start);
        let mut nav: Navigator<DEPTH> = Navigator::new(start, VIEWPORT, METRICS);
        let mut tree: Tree<CAP> = Tree::new();
        nav.show_start(&mut tree, build).unwrap();
        for op in ops {
            let before = (nav.current(), nav.back_stack().to_vec(), tree.root());
            // The result, and whether a screen was shown afresh.
            let (result, shown) = match op {
                Op::Go(n) => (nav.go(ScreenId(n), &mut tree, build), true),
                Op::Replace(n) => (nav.replace(ScreenId(n), &mut tree, build), true),
                Op::Back => match nav.back(&mut tree, build) {
                    Ok(went) => (Ok(()), went),
                    Err(error) => (Err(error), true),
                },
                Op::Start => (nav.show_start(&mut tree, build), true),
                Op::Type => {
                    if let Some(field) = (0..20).map(FieldId).find(|&f| tree.value(f).is_some()) {
                        tree.set_value(field, "typed").unwrap();
                    }
                    (Ok(()), false)
                }
            };
            let refused = matches!(result, Err(UiError::BackStackFull { .. }));
            match result {
                Ok(()) => {}
                Err(UiError::BackStackFull { capacity }) => {
                    // Refused before anything changed.
                    prop_assert_eq!(capacity, DEPTH);
                    prop_assert_eq!(&before.1, &nav.back_stack().to_vec());
                    prop_assert_eq!(before.0, nav.current());
                    prop_assert_eq!(before.2, tree.root());
                }
                Err(_) => {
                    // A screen that cannot be built: the start screen shows.
                    prop_assert_eq!(nav.current(), start);
                    prop_assert!(nav.back_stack().is_empty());
                }
            }
            prop_assert!(nav.back_stack().len() <= DEPTH);
            // Exactly the current screen's nodes: never a partial screen.
            prop_assert!(tree.root().is_some());
            prop_assert_eq!(tree.len(), nodes_of(nav.current()));
            prop_assert_eq!(tree.paint_order().count(), tree.len());
            if shown && !refused {
                // Freshly shown: its first control has focus, nothing typed
                // is left, and the whole viewport repaints.
                let first = tree
                    .paint_order()
                    .find(|(_, node)| node.is_focusable())
                    .map(|(id, _)| id);
                prop_assert_eq!(tree.focused(), first);
                prop_assert!((0..20).all(|f| tree.value(FieldId(f)).is_none_or(str::is_empty)));
                prop_assert_eq!(tree.dirty_rects(), &[VIEWPORT]);
            } else {
                prop_assert!(!refused || tree.dirty_rects().is_empty());
            }
            tree.clear_dirty();
        }
    }
}
