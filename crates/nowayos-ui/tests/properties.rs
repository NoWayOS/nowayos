// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests: random trees, layouts and input streams never panic and
//! always keep the tree's invariants.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::HashSet;

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::{
    ActionId, Align, FieldId, GroupId, INPUT_CAPACITY, InputMode, Length, MAX_DEPTH, NodeId,
    NodeKind, SLOT_CAPACITY, SlotId, Style, TextMetrics, TextStyle, Tree, UiError,
};
use proptest::prelude::*;

const CAP: usize = 24;
const METRICS: TextMetrics = TextMetrics::new(6, 10);

#[derive(Debug, Clone)]
enum Build {
    Add(u8, Style),
    Append(usize, usize),
    SetRoot(usize),
    /// A group around one node, or (with a second) choosing between two.
    Group(u8, usize, Option<usize>),
    /// Shows or hides a group.
    Show(u8, bool),
}

fn length() -> impl Strategy<Value = Length> {
    prop_oneof![
        Just(Length::Auto),
        Just(Length::Fill),
        (0u32..300).prop_map(Length::Fixed)
    ]
}

fn align() -> impl Strategy<Value = Align> {
    prop_oneof![
        Just(Align::Start),
        Just(Align::Center),
        Just(Align::End),
        Just(Align::Stretch)
    ]
}

fn style() -> impl Strategy<Value = Style> {
    (length(), length(), 0u32..20, 0u32..12, align(), 0u8..=9).prop_map(
        |(w, h, pad, gap, align, lines)| {
            Style::new()
                .width(w)
                .height(h)
                .padding(pad)
                .gap(gap)
                .align(align)
                // Text on up to 8 lines, and outside the range.
                .text(TextStyle::BODY.lines(lines))
        },
    )
}

fn build() -> impl Strategy<Value = Build> {
    prop_oneof![
        3 => (0u8..11, style()).prop_map(|(k, s)| Build::Add(k, s)),
        4 => (0usize..CAP, 0usize..CAP).prop_map(|(p, c)| Build::Append(p, c)),
        1 => (0usize..CAP).prop_map(Build::SetRoot),
        1 => (0u8..4, 0usize..CAP, proptest::option::of(0usize..CAP))
            .prop_map(|(g, a, b)| Build::Group(g, a, b)),
        1 => (0u8..4, any::<bool>()).prop_map(|(g, v)| Build::Show(g, v)),
    ]
}

fn kind(k: u8, n: usize) -> NodeKind {
    match k {
        0 => NodeKind::Column,
        1 => NodeKind::Row,
        2 => NodeKind::Stack,
        3 => NodeKind::Text("label"),
        4 => NodeKind::Button {
            label: "Go",
            action: ActionId(n as u16),
        },
        5 => NodeKind::TextInput {
            field: FieldId(n as u16),
            placeholder: "type here",
            value: Default::default(),
            // Every length from 1 up, in each mode.
            max_len: (n % INPUT_CAPACITY + 1) as u8,
            mode: [InputMode::Text, InputMode::Digits, InputMode::Email][n % 3],
            caret: 0,
            anchor: 0,
        },
        6 => NodeKind::Slot {
            slot: SlotId(n as u16),
            text: Default::default(),
            template: None,
        },
        7 => NodeKind::FieldError {
            field: FieldId(n as u16),
            message: Some("Check this"),
        },
        8 => NodeKind::Checkbox {
            field: FieldId(n as u16),
            label: "Tick",
            checked: n.is_multiple_of(2),
        },
        9 => NodeKind::RadioGroup {
            field: FieldId(n as u16),
            options: &["One", "Two", "Three"],
            selected: None,
        },
        _ => NodeKind::Spacer,
    }
}

fn event() -> impl Strategy<Value = InputEvent> {
    let key = prop_oneof![
        Just(KeyCode::TAB),
        Just(KeyCode::ENTER),
        Just(KeyCode::SPACE),
        Just(KeyCode::BACKSPACE),
        Just(KeyCode::UP),
        Just(KeyCode::DOWN),
        Just(KeyCode::LEFT),
        Just(KeyCode::RIGHT),
        Just(KeyCode::LEFT_SHIFT),
        (0x04u16..=0x38).prop_map(|u| KeyCode::new(u).unwrap()),
    ];
    let phase = prop_oneof![
        Just(TouchPhase::Start),
        Just(TouchPhase::Move),
        Just(TouchPhase::End),
        Just(TouchPhase::Cancel)
    ];
    prop_oneof![
        key.clone().prop_map(InputEvent::KeyDown),
        key.prop_map(InputEvent::KeyUp),
        (-20i32..340, -20i32..260).prop_map(|(x, y)| InputEvent::PointerMove { x, y }),
        (0u8..3, any::<bool>())
            .prop_map(|(button, down)| InputEvent::PointerButton { button, down }),
        (0u8..2, -20i32..340, -20i32..260, phase).prop_map(|(id, x, y, phase)| InputEvent::Touch {
            id,
            x,
            y,
            phase
        }),
    ]
}

/// Applies random build steps, ignoring the ones the tree refuses.
fn random_tree(steps: &[Build]) -> Tree<CAP> {
    let mut tree: Tree<CAP> = Tree::new();
    let mut ids: Vec<NodeId> = Vec::new();
    for step in steps {
        match step {
            Build::Add(k, s) => {
                if let Ok(id) = tree.add(kind(*k, ids.len()), *s) {
                    ids.push(id);
                }
            }
            Build::Append(p, c) => {
                if let (Some(&p), Some(&c)) = (ids.get(*p), ids.get(*c)) {
                    let _ = tree.append(p, c);
                }
            }
            Build::SetRoot(r) => {
                if let Some(&r) = ids.get(*r) {
                    let _ = tree.set_root(r);
                }
            }
            Build::Group(g, a, b) => {
                let group = GroupId(u16::from(*g));
                let made = match (ids.get(*a), b.and_then(|b| ids.get(b))) {
                    (Some(&a), None) => tree.group(group, a),
                    (Some(&a), Some(&b)) => tree.either(group, a, b),
                    (None, _) => continue,
                };
                if let Ok(id) = made {
                    ids.push(id);
                }
            }
            Build::Show(g, visible) => {
                let _ = tree.set_visible(GroupId(u16::from(*g)), *visible);
            }
        }
    }
    tree
}

fn depth(tree: &Tree<CAP>, id: NodeId) -> usize {
    let mut depth = 0;
    let mut current = tree.get(id).unwrap().parent();
    while let Some(parent) = current {
        depth += 1;
        assert!(depth <= MAX_DEPTH + 1, "cycle or excessive depth");
        current = tree.get(parent).unwrap().parent();
    }
    depth
}

proptest! {
    #[test]
    fn random_trees_keep_their_invariants(steps in proptest::collection::vec(build(), 0..80)) {
        let tree = random_tree(&steps);
        prop_assert!(tree.len() <= CAP);
        // Every node's ancestry terminates within the depth limit.
        for index in 0..tree.len() {
            let id = tree.paint_order().map(|(id, _)| id).find(|id| id.index() == index);
            if let Some(id) = id {
                prop_assert!(depth(&tree, id) <= MAX_DEPTH);
            }
        }
        // Paint order visits each reachable node once, starting at the root.
        let order: Vec<NodeId> = tree.paint_order().map(|(id, _)| id).collect();
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        prop_assert_eq!(unique.len(), order.len());
        prop_assert_eq!(order.first().copied(), tree.root());
        // Children point back at their parent; a group has one or two.
        for &id in &order {
            for child in tree.children(id) {
                prop_assert_eq!(tree.get(child).unwrap().parent(), Some(id));
            }
            if matches!(tree.get(id).unwrap().kind(), NodeKind::Group { .. }) {
                prop_assert!((1..=2).contains(&tree.children(id).count()));
            }
            // Paint order holds only what shows.
            prop_assert!(tree.is_shown(id));
        }
    }

    #[test]
    fn layout_and_input_never_panic(
        steps in proptest::collection::vec(build(), 0..80),
        events in proptest::collection::vec(event(), 0..120),
        w in 0u32..400,
        h in 0u32..300,
    ) {
        let mut tree = random_tree(&steps);
        let viewport = Rect::new(0, 0, w, h);
        if tree.layout(viewport, &METRICS).is_ok() {
            prop_assert!(!tree.dirty_rects().is_empty() || viewport.is_empty());
        }
        for e in events {
            let _ = tree.handle(e);
            if let Some(focused) = tree.focused() {
                prop_assert!(tree.get(focused).unwrap().is_focusable());
                prop_assert!(tree.is_shown(focused), "focus on a hidden control");
            }
            for rect in tree.dirty_rects() {
                prop_assert!(viewport.contains_rect(rect));
            }
        }
        for (_, node) in tree.paint_order() {
            match node.kind() {
                NodeKind::TextInput { value, max_len, mode, .. } => {
                    prop_assert!(value.len() <= usize::from(*max_len));
                    prop_assert!(value.chars().all(|ch| mode.accepts(ch)));
                }
                NodeKind::RadioGroup { options, selected, .. } => {
                    prop_assert!(selected.is_none_or(|option| usize::from(option) < options.len()));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn hit_test_results_contain_the_point(
        steps in proptest::collection::vec(build(), 0..80),
        x in -10i32..330,
        y in -10i32..250,
    ) {
        let mut tree = random_tree(&steps);
        if tree.layout(Rect::new(0, 0, 320, 240), &METRICS).is_ok() {
            let p = Point::new(x, y);
            if let Some(hit) = tree.hit_test(p) {
                prop_assert!(tree.get(hit).unwrap().rect().contains(p));
                prop_assert!(tree.is_shown(hit), "a hidden node was hit");
            }
        }
    }

    /// Fill children with no gaps or padding tile their parent exactly,
    /// whatever the sizes.
    #[test]
    fn fill_children_tile_the_parent(count in 1usize..10, main in 0u32..1000, vertical in any::<bool>()) {
        let mut tree: Tree<16> = Tree::new();
        let root = if vertical { tree.column(Style::new()) } else { tree.row(Style::new()) }.unwrap();
        tree.set_root(root).unwrap();
        let mut children = Vec::new();
        for _ in 0..count {
            children.push(tree.spacer().unwrap());
        }
        for &c in &children {
            tree.append(root, c).unwrap();
        }
        let viewport = if vertical { Rect::new(0, 0, 10, main) } else { Rect::new(0, 0, main, 10) };
        tree.layout(viewport, &METRICS).unwrap();
        let mut expected_start = 0i64;
        let mut sizes = Vec::new();
        for &c in &children {
            let r = tree.get(c).unwrap().rect();
            let (start, size) = if vertical { (r.top(), r.size.height) } else { (r.left(), r.size.width) };
            prop_assert_eq!(start, expected_start);
            expected_start += i64::from(size);
            sizes.push(size);
        }
        prop_assert_eq!(expected_start, i64::from(main));
        let (min, max) = (sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
        prop_assert!(max - min <= 1, "fills differ by more than one pixel: {:?}", sizes);
    }

    /// Setting any text in a slot never panics: printable text that fits is
    /// shown exactly, anything else is refused with a typed error and the
    /// previous text stays. Storage beyond the text is always zero, and
    /// clearing zeroes all of it.
    #[test]
    fn setting_random_slot_text_never_panics(
        texts in proptest::collection::vec(
            prop_oneof![
                3 => "[ -~\u{A0}-\u{FF}]{0,70}",
                1 => any::<String>(),
                1 => "[a-z\u{0}-\u{1F}\u{200B}\u{2014}\u{1F600}]{0,8}",
            ],
            1..24,
        ),
    ) {
        const SLOT: SlotId = SlotId(3);
        let mut tree: Tree<4> = Tree::new();
        let root = tree.column(Style::new()).unwrap();
        tree.set_root(root).unwrap();
        let slot = tree.slot(SLOT).unwrap();
        tree.append(root, slot).unwrap();
        tree.layout(Rect::new(0, 0, 200, 40), &METRICS).unwrap();
        let mut shown = String::new();
        for text in &texts {
            tree.clear_dirty();
            let showable = text.len() <= SLOT_CAPACITY
                && text.chars().all(|c| matches!(c, ' '..='~' | '\u{A0}'..='\u{FF}') && c != '\u{AD}');
            match tree.set_slot(SLOT, text) {
                Ok(()) => {
                    prop_assert!(showable, "accepted {:?}", text);
                    // Only what changed is marked, within the slot:
                    // nothing if the text is the same.
                    let rect = tree.get(slot).unwrap().rect();
                    prop_assert!(tree.dirty_rects().iter().all(|dirty| rect.contains_rect(dirty)));
                    prop_assert_eq!(tree.dirty_rects().is_empty(), shown == *text);
                    shown.clone_from(text);
                }
                Err(error) => {
                    prop_assert!(!showable, "refused {:?}: {:?}", text, error);
                    prop_assert!(matches!(
                        error,
                        UiError::TextTooLong(_)
                            | UiError::ControlCharacter
                            | UiError::InvisibleCharacter
                            | UiError::NoGlyph
                    ));
                    prop_assert!(tree.dirty_rects().is_empty());
                }
            }
            prop_assert_eq!(tree.slot_text(SLOT), Some(shown.as_str()));
            let NodeKind::Slot { text: stored, .. } = tree.get(slot).unwrap().kind() else {
                panic!("the slot changed kind");
            };
            prop_assert!(stored.unused_is_zeroed());
        }
        tree.clear_slots();
        let NodeKind::Slot { text: stored, .. } = tree.get(slot).unwrap().kind() else {
            panic!("the slot changed kind");
        };
        prop_assert!(stored.is_zeroed());
    }
}
