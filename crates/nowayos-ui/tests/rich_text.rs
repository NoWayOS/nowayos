// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rich text (#10078): runs wrap as one paragraph; each link is a tab stop
//! of its own, Enter or Space reports its action, a press on it does too,
//! and a press on the rest of the text does nothing.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, Run, UiBuilder};
use nowayos_ui::{ActionId, NodeId, NodeKind, TextMetrics, TextStyle, Tree, UiEvent};

const GUIDE: ActionId = ActionId(1);
const HELP: ActionId = ActionId(2);
const AFTER: ActionId = ActionId(3);

const RUNS: &[Run] = &[
    Run::new("Press "),
    Run::new("Save").bold(),
    Run::new(", read "),
    Run::new("the guide").link(GUIDE),
    Run::new(" or ask for "),
    Run::new("help").link(HELP).italic(),
    Run::new(" with "),
    Run::new("F1").code(),
    Run::new("."),
];

fn screen() -> (Tree<8>, NodeId, NodeId) {
    let mut tree: Tree<8> = Tree::new();
    let text = UiBuilder::rich_text(&mut tree, RUNS, TextStyle::default()).unwrap();
    let after = UiBuilder::button(&mut tree, "Next", AFTER).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new().gap(8), &[text, after]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 200, 200), &TextMetrics::new(8, 16))
        .unwrap();
    (tree, text, after)
}

fn key(tree: &mut Tree<8>, code: KeyCode) -> Option<UiEvent> {
    let event = tree.handle(InputEvent::KeyDown(code));
    tree.handle(InputEvent::KeyUp(code));
    event
}

fn link(tree: &Tree<8>, id: NodeId) -> u8 {
    match tree.get(id).unwrap().kind() {
        NodeKind::Rich { link, .. } => *link,
        _ => panic!("not a rich text"),
    }
}

fn click(tree: &mut Tree<8>, at: Point) -> Option<UiEvent> {
    tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: false,
    })
}

#[test]
fn runs_wrap_as_one_paragraph() {
    let (tree, text, _) = screen();
    // 25 characters a line: the text takes three lines of 16.
    assert_eq!(tree.get(text).unwrap().rect().size.height, 48);
}

#[test]
fn each_link_is_a_tab_stop_and_reports_its_action() {
    let (mut tree, text, after) = screen();
    key(&mut tree, KeyCode::TAB);
    assert_eq!((tree.focused(), link(&tree, text)), (Some(text), 0));
    assert_eq!(key(&mut tree, KeyCode::ENTER), Some(UiEvent::Action(GUIDE)));
    key(&mut tree, KeyCode::TAB);
    assert_eq!((tree.focused(), link(&tree, text)), (Some(text), 1));
    assert_eq!(key(&mut tree, KeyCode::SPACE), Some(UiEvent::Action(HELP)));
    key(&mut tree, KeyCode::TAB);
    assert_eq!(tree.focused(), Some(after));
    // Back: the last link first.
    tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
    key(&mut tree, KeyCode::TAB);
    assert_eq!((tree.focused(), link(&tree, text)), (Some(text), 1));
    key(&mut tree, KeyCode::TAB);
    assert_eq!((tree.focused(), link(&tree, text)), (Some(text), 0));
}

#[test]
fn a_press_on_a_link_reports_it_and_on_the_rest_nothing() {
    let (mut tree, text, _) = screen();
    let content = tree.get(text).unwrap().content();
    let row = |line: i64| i32::try_from(content.top() + line * 16 + 8).unwrap();
    let column = |chars: i64| i32::try_from(content.left() + chars * 8 + 4).unwrap();
    // "Press Save, read the " then "guide or ask for help " then "with F1."
    assert_eq!(click(&mut tree, Point::new(column(1), row(0))), None);
    assert_eq!(
        click(&mut tree, Point::new(column(18), row(0))),
        Some(UiEvent::Action(GUIDE))
    );
    assert_eq!(
        click(&mut tree, Point::new(column(2), row(1))),
        Some(UiEvent::Action(GUIDE))
    );
    assert_eq!(
        click(&mut tree, Point::new(column(18), row(1))),
        Some(UiEvent::Action(HELP))
    );
}
