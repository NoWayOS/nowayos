// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mouse (#10082): the control the pointer hovers, marked only when it
//! changes and never while a finger touches; the pointer's shape over each
//! kind of thing, a node's own and beneath a dialog; and a context action
//! on a right click or a long press, a menu then opened for its control
//! opening at the point pressed.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, TouchPhase};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::mouse::LONG_PRESS;
use nowayos_ui::{
    ActionId, Cursor, FieldId, Input, LayerId, NodeId, TextMetrics, TextStyle, Tree, UiEvent,
};

const OPEN: ActionId = ActionId(1);
const ROW_MENU: ActionId = ActionId(2);
const MENU: LayerId = LayerId(1);
const CONFIRM: LayerId = LayerId(2);

struct Screen {
    tree: Tree<32>,
    open: NodeId,
    plain: NodeId,
    name: NodeId,
    note: NodeId,
    choice: NodeId,
    panel: NodeId,
    inside: NodeId,
}

fn screen() -> Screen {
    let mut tree: Tree<32> = Tree::new();
    let t = &mut tree;
    let open = UiBuilder::button(t, "Open", OPEN).unwrap();
    let rename = UiBuilder::button(t, "Rename", ActionId(3)).unwrap();
    let items = UiBuilder::column(t, Layout::new(), &[rename]).unwrap();
    let menu = UiBuilder::menu(t, MENU, open, items).unwrap();
    let plain = UiBuilder::text(t, "Plain words").unwrap();
    let note = UiBuilder::text_styled(t, "Ref 4471", TextStyle::default().selectable()).unwrap();
    let name = UiBuilder::input(t, FieldId(1), Input::new()).unwrap();
    let choice = UiBuilder::radio_group(t, FieldId(2), &["One", "Two", "Three"]).unwrap();
    let inside = UiBuilder::text(t, "Inside").unwrap();
    let panel = UiBuilder::column(t, Layout::new().padding(8), &[inside]).unwrap();
    let ok = UiBuilder::button(t, "OK", ActionId(4)).unwrap();
    let dialog = UiBuilder::dialog(t, CONFIRM, ok).unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().gap(8).padding(8),
        &[open, menu, plain, note, name, choice, panel, dialog],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 640, 480), &TextMetrics::new(8, 16))
        .unwrap();
    tree.set_context_action(open, ROW_MENU).unwrap();
    tree.set_cursor(panel, Cursor::NotAllowed).unwrap();
    Screen {
        tree,
        open,
        plain,
        name,
        note,
        choice,
        panel,
        inside,
    }
}

fn centre(tree: &Tree<32>, id: NodeId) -> Point {
    let rect = tree.get(id).unwrap().rect();
    Point::new(
        i32::try_from(rect.left() + i64::from(rect.size.width / 2)).unwrap(),
        i32::try_from(rect.top() + i64::from(rect.size.height / 2)).unwrap(),
    )
}

fn plus(at: Point, x: i32, y: i32) -> Point {
    Point::new(at.x + x, at.y + y)
}

fn move_to(tree: &mut Tree<32>, at: Point) -> Option<UiEvent> {
    tree.handle(InputEvent::PointerMove { x: at.x, y: at.y })
}

fn touch(tree: &mut Tree<32>, at: Point, phase: TouchPhase) -> Option<UiEvent> {
    tree.handle(InputEvent::Touch {
        id: 0,
        x: at.x,
        y: at.y,
        phase,
    })
}

#[test]
fn the_hovered_control_is_marked_only_when_it_changes() {
    let mut s = screen();
    let open = centre(&s.tree, s.open);
    move_to(&mut s.tree, open);
    assert_eq!(s.tree.hover().node, Some(s.open));
    s.tree.clear_dirty();
    // Within the same button: nothing to repaint.
    move_to(&mut s.tree, plus(open, 2, 1));
    assert!(s.tree.dirty_rects().is_empty());
    // Onto the input: both repainted.
    let name = centre(&s.tree, s.name);
    move_to(&mut s.tree, name);
    assert_eq!(s.tree.hover().node, Some(s.name));
    let button = s.tree.get(s.open).unwrap().rect();
    let input = s.tree.get(s.name).unwrap().rect();
    let dirty = s.tree.dirty_rects();
    assert!(dirty.iter().any(|rect| rect.intersects(&button)));
    assert!(dirty.iter().any(|rect| rect.intersects(&input)));
    // Onto plain text: nothing hovered.
    let plain = centre(&s.tree, s.plain);
    move_to(&mut s.tree, plain);
    assert_eq!(s.tree.hover().node, None);
    // A radio group's option is hovered by its part.
    let group = s.tree.get(s.choice).unwrap();
    let second = group.option_rect(1).unwrap();
    let at = Point::new(
        i32::try_from(second.left() + 2).unwrap(),
        i32::try_from(second.top() + 2).unwrap(),
    );
    move_to(&mut s.tree, at);
    assert_eq!(
        (s.tree.hover().node, s.tree.hover().part),
        (Some(s.choice), Some(1))
    );
    // A finger hovers nothing.
    touch(&mut s.tree, open, TouchPhase::Start);
    touch(&mut s.tree, open, TouchPhase::Cancel);
    assert_eq!(s.tree.hover().node, None);
}

#[test]
fn the_pointer_shows_what_a_press_will_do() {
    let mut s = screen();
    for (node, cursor) in [
        (s.open, Cursor::Hand),
        (s.plain, Cursor::Arrow),
        (s.note, Cursor::Text),
        (s.name, Cursor::Text),
        (s.choice, Cursor::Hand),
        // A node's own, over what it holds.
        (s.inside, Cursor::NotAllowed),
        (s.panel, Cursor::NotAllowed),
    ] {
        let at = centre(&s.tree, node);
        move_to(&mut s.tree, at);
        assert_eq!(s.tree.cursor(), cursor, "{node:?}");
    }
    move_to(&mut s.tree, Point::new(630, 470));
    assert_eq!(s.tree.cursor(), Cursor::Arrow);
    // Beneath a dialog nothing can be pressed.
    s.tree.open(CONFIRM).unwrap();
    let at = centre(&s.tree, s.plain);
    move_to(&mut s.tree, at);
    assert_eq!(s.tree.cursor(), Cursor::NotAllowed);
}

#[test]
fn a_right_click_reports_the_context_action_and_its_menu_opens_there() {
    let mut s = screen();
    let at = plus(centre(&s.tree, s.open), 10, 3);
    move_to(&mut s.tree, at);
    assert_eq!(
        s.tree.handle(InputEvent::PointerButton {
            button: 1,
            down: true
        }),
        Some(UiEvent::Action(ROW_MENU))
    );
    s.tree.handle(InputEvent::PointerButton {
        button: 1,
        down: false,
    });
    // Nothing else answers a right click.
    let plain = centre(&s.tree, s.plain);
    move_to(&mut s.tree, plain);
    assert_eq!(
        s.tree.handle(InputEvent::PointerButton {
            button: 1,
            down: true
        }),
        None
    );
    // The application opens the control's menu: beside the point pressed.
    s.tree.open(MENU).unwrap();
    let menu = s.tree.get(s.tree.find_layer(MENU).unwrap()).unwrap().rect();
    let open = s.tree.get(s.open).unwrap().rect();
    assert!(
        menu.top() > open.top() && menu.top() < open.bottom() + 16,
        "{menu:?}"
    );
    assert!(menu.left() >= i64::from(at.x) - 1, "{menu:?} at {at:?}");
    // Closed and opened again without a context action: beside the button.
    s.tree.close(MENU).unwrap();
    s.tree.open(MENU).unwrap();
    let menu = s.tree.get(s.tree.find_layer(MENU).unwrap()).unwrap().rect();
    assert!(menu.top() >= open.bottom(), "{menu:?}");
}

#[test]
fn a_finger_resting_reports_the_context_action_instead_of_a_press() {
    let mut s = screen();
    let at = centre(&s.tree, s.open);
    // A short touch presses.
    touch(&mut s.tree, at, TouchPhase::Start);
    assert!(s.tree.is_moving(), "the clock runs to time a rest");
    s.tree.advance(100);
    assert_eq!(
        touch(&mut s.tree, at, TouchPhase::End),
        Some(UiEvent::Action(OPEN))
    );
    // A long one, a little unsteady: the context action, and no press.
    touch(&mut s.tree, at, TouchPhase::Start);
    touch(&mut s.tree, plus(at, 3, -2), TouchPhase::Move);
    s.tree.advance(LONG_PRESS);
    assert!(!s.tree.is_moving());
    assert_eq!(
        touch(&mut s.tree, at, TouchPhase::End),
        Some(UiEvent::Action(ROW_MENU))
    );
    assert_eq!(s.tree.pressed(), None);
    // Moved away, it is no rest.
    touch(&mut s.tree, at, TouchPhase::Start);
    touch(&mut s.tree, plus(at, 40, 0), TouchPhase::Move);
    s.tree.advance(LONG_PRESS);
    assert_ne!(
        touch(&mut s.tree, plus(at, 40, 0), TouchPhase::End),
        Some(UiEvent::Action(ROW_MENU))
    );
}
