// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layers over the screen (#10076): a dialog opens in the middle, holding
//! focus, nothing beneath it reachable, until the application closes it; a
//! menu or popover opens beside its control on whichever side has room and
//! closes on Escape or a press outside, a menu also on a choice; focus
//! returns to the control that opened it; a tooltip shows while the
//! pointer rests on its control; one inside what does not show cannot
//! open; and a session wipe closes every layer.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::layer::MAX_OPEN;
use nowayos_ui::{ActionId, GroupId, LayerId, NodeId, TextMetrics, Tree, UiError, UiEvent};
use proptest::prelude::*;

const CONFIRM: LayerId = LayerId(1);
const MORE: LayerId = LayerId(2);
const HELP: LayerId = LayerId(3);

const OPEN_MENU: ActionId = ActionId(1);
const DELETE: ActionId = ActionId(2);
const YES: ActionId = ActionId(3);
const NO: ActionId = ActionId(4);
const RENAME: ActionId = ActionId(5);
const COPY: ActionId = ActionId(6);

fn metrics() -> TextMetrics {
    TextMetrics::new(8, 16)
}

/// The handles a test needs.
struct Screen {
    tree: Tree<40>,
    more: NodeId,
    delete: NodeId,
    yes: NodeId,
    no: NodeId,
    rename: NodeId,
    bottom: NodeId,
}

/// A row of buttons at the top - More, which a menu belongs to, and
/// Delete, which has a tooltip - a confirming dialog of two buttons, and a
/// button at the bottom with a popover; in 400 x 300.
fn screen() -> Screen {
    let mut tree: Tree<40> = Tree::new();
    let t = &mut tree;
    let more = UiBuilder::button(t, "More", OPEN_MENU).unwrap();
    let delete = UiBuilder::button(t, "Delete", DELETE).unwrap();
    let rename = UiBuilder::button(t, "Rename", RENAME).unwrap();
    let copy = UiBuilder::button(t, "Copy", COPY).unwrap();
    let items = UiBuilder::column(t, Layout::new(), &[rename, copy]).unwrap();
    let menu = UiBuilder::menu(t, MORE, more, items).unwrap();
    let tip = UiBuilder::tooltip(t, delete, "Deletes it for good").unwrap();
    let top = UiBuilder::row(t, Layout::new().gap(8), &[more, delete, menu, tip]).unwrap();
    let question = UiBuilder::text(t, "Delete it?").unwrap();
    let yes = UiBuilder::button(t, "Yes", YES).unwrap();
    let no = UiBuilder::button(t, "No", NO).unwrap();
    let answers = UiBuilder::row(t, Layout::new().gap(8), &[yes, no]).unwrap();
    let body = UiBuilder::column(t, Layout::new().gap(8), &[question, answers]).unwrap();
    let dialog = UiBuilder::dialog(t, CONFIRM, body).unwrap();
    let spacer = UiBuilder::spacer(t, None).unwrap();
    let bottom = UiBuilder::button(t, "Help", ActionId(9)).unwrap();
    let words = UiBuilder::text(t, "Help is here").unwrap();
    let popover = UiBuilder::popover(t, HELP, bottom, words).unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().padding(8),
        &[top, dialog, spacer, bottom, popover],
    )
    .unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 400, 300), &metrics()).unwrap();
    Screen {
        tree,
        more,
        delete,
        yes,
        no,
        rename,
        bottom,
    }
}

fn key(tree: &mut Tree<40>, code: KeyCode) -> Option<UiEvent> {
    let event = tree.handle(InputEvent::KeyDown(code));
    tree.handle(InputEvent::KeyUp(code));
    event
}

fn click(tree: &mut Tree<40>, at: Point) -> Option<UiEvent> {
    tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    let down = tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    let up = tree.handle(InputEvent::PointerButton {
        button: 0,
        down: false,
    });
    up.or(down)
}

fn centre(tree: &Tree<40>, id: NodeId) -> Point {
    tree.get(id).unwrap().rect().center()
}

#[test]
fn a_layer_takes_no_place_where_it_is_written() {
    let s = screen();
    let more = s.tree.get(s.more).unwrap().rect();
    let delete = s.tree.get(s.delete).unwrap().rect();
    // Delete stands one gap after More: the menu and tooltip in the row
    // take no place, and add no gap.
    assert_eq!(delete.left(), more.right() + 8);
    assert_eq!(s.tree.shown_layers().count(), 0);
    assert!(!s.tree.is_open(CONFIRM));
}

#[test]
fn a_dialog_holds_focus_and_nothing_beneath_it_is_reachable_until_closed() {
    let mut s = screen();
    let t = &mut s.tree;
    t.set_focus(Some(s.delete)).unwrap();
    t.open(CONFIRM).unwrap();
    assert!(t.is_open(CONFIRM));
    // In the middle of the viewport, focus on its first control.
    let dialog = t.find_layer(CONFIRM).unwrap();
    let rect = t.get(dialog).unwrap().rect();
    assert_eq!(rect.center(), Point::new(200, 150));
    assert_eq!(t.focused(), Some(s.yes));
    // Tab stays inside: Yes, No, Yes.
    key(t, KeyCode::TAB);
    assert_eq!(t.focused(), Some(s.no));
    key(t, KeyCode::TAB);
    assert_eq!(t.focused(), Some(s.yes));
    // Nothing beneath can be pressed or hit.
    let under = centre(t, s.more);
    assert_eq!(click(t, under), None);
    assert_eq!(t.hit_test(under), None);
    assert!(t.is_open(CONFIRM));
    // Escape asks the application; the dialog stays until it closes it.
    assert_eq!(key(t, KeyCode::ESCAPE), Some(UiEvent::Dismissed(CONFIRM)));
    assert!(t.is_open(CONFIRM));
    let no = centre(t, s.no);
    assert_eq!(click(t, no), Some(UiEvent::Action(NO)));
    t.close(CONFIRM).unwrap();
    assert!(!t.is_open(CONFIRM));
    // Focus back where it was.
    assert_eq!(t.focused(), Some(s.delete));
    // The screen beneath reachable again.
    assert_eq!(t.hit_test(under), Some(s.more));
}

#[test]
fn a_menu_opens_beside_its_control_and_closes_on_escape_outside_or_a_choice() {
    let mut s = screen();
    let t = &mut s.tree;
    t.set_focus(Some(s.more)).unwrap();
    t.open(MORE).unwrap();
    let menu = t.get(t.find_layer(MORE).unwrap()).unwrap().rect();
    let more = t.get(s.more).unwrap().rect();
    // Below its control, lined up with it but kept the margin from the
    // viewport's edge.
    assert_eq!(more.left(), 8);
    assert_eq!((menu.left(), menu.top()), (16, more.bottom() + 4));
    assert_eq!(t.focused(), Some(s.rename));
    // Down moves between its choices.
    key(t, KeyCode::DOWN);
    key(t, KeyCode::UP);
    assert_eq!(t.focused(), Some(s.rename));
    // Escape closes it; focus returns to More.
    assert_eq!(key(t, KeyCode::ESCAPE), Some(UiEvent::Closed(MORE)));
    assert!(!t.is_open(MORE));
    assert_eq!(t.focused(), Some(s.more));
    // A press outside closes it, and presses nothing beneath.
    t.open(MORE).unwrap();
    let outside = centre(t, s.bottom);
    assert_eq!(click(t, outside), Some(UiEvent::Closed(MORE)));
    assert!(!t.is_open(MORE));
    // A choice closes it, reporting the choice.
    t.open(MORE).unwrap();
    assert_eq!(key(t, KeyCode::ENTER), Some(UiEvent::Action(RENAME)));
    assert!(!t.is_open(MORE));
    assert_eq!(t.focused(), Some(s.more));
}

#[test]
fn a_popover_with_no_room_below_opens_above() {
    let mut s = screen();
    let t = &mut s.tree;
    t.open(HELP).unwrap();
    let popover = t.get(t.find_layer(HELP).unwrap()).unwrap().rect();
    let help = t.get(s.bottom).unwrap().rect();
    assert_eq!(popover.bottom(), help.top() - 4);
    assert!(popover.top() >= 16, "inside the viewport's margin");
}

#[test]
fn a_tooltip_shows_while_the_pointer_rests_on_its_control_or_it_has_focus_after_a_key() {
    let mut s = screen();
    let t = &mut s.tree;
    let delete = centre(t, s.delete);
    t.handle(InputEvent::PointerMove {
        x: delete.x,
        y: delete.y,
    });
    assert_eq!(t.shown_layers().count(), 1);
    let tip = t.shown_layers().next().unwrap();
    assert!(t.get(tip).unwrap().rect().top() > t.get(s.delete).unwrap().rect().bottom());
    // Never hit: what lies beneath it is.
    let on_tip = t.get(tip).unwrap().rect().center();
    assert_ne!(t.hit_test(on_tip), Some(tip));
    t.handle(InputEvent::PointerMove { x: 390, y: 150 });
    assert_eq!(t.shown_layers().count(), 0);
    // Focus after a key.
    t.set_focus(Some(s.more)).unwrap();
    key(t, KeyCode::TAB);
    assert_eq!(t.focused(), Some(s.delete));
    assert_eq!(t.shown_layers().count(), 1);
}

#[test]
fn a_layer_is_whole_before_it_shows_and_bounded() {
    let mut tree: Tree<16> = Tree::new();
    let t = &mut tree;
    let inner = UiBuilder::text(t, "Hidden").unwrap();
    let hidden = UiBuilder::dialog(t, LayerId(1), inner).unwrap();
    let group = UiBuilder::group(t, GroupId(1), hidden).unwrap();
    let words = UiBuilder::text(t, "Shown").unwrap();
    let shown = UiBuilder::dialog(t, LayerId(2), words).unwrap();
    // A second layer of one number is refused when built.
    let again = UiBuilder::text(t, "Again").unwrap();
    assert_eq!(
        UiBuilder::dialog(t, LayerId(2), again),
        Err(UiError::DuplicateLayer(LayerId(2)))
    );
    let root = UiBuilder::column(t, Layout::new(), &[group, shown]).unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 200, 100), &metrics()).unwrap();
    assert_eq!(t.open(LayerId(9)), Err(UiError::UnknownLayer(LayerId(9))));
    assert_eq!(t.open(LayerId(1)), Err(UiError::LayerHidden(LayerId(1))));
    t.set_visible(GroupId(1), true).unwrap();
    t.open(LayerId(1)).unwrap();
    t.open(LayerId(2)).unwrap();
    assert_eq!(t.shown_layers().count(), 2);
    // Closing the lower closes the one over it.
    t.close(LayerId(1)).unwrap();
    assert_eq!(t.shown_layers().count(), 0);
}

#[test]
fn a_session_wipe_closes_every_layer() {
    let mut s = screen();
    s.tree.open(CONFIRM).unwrap();
    s.tree.wipe_session();
    assert_eq!(s.tree.shown_layers().count(), 0);
    assert!(!s.tree.is_open(CONFIRM));
}

#[test]
fn no_more_than_the_most_layers_open() {
    let mut tree: Tree<24> = Tree::new();
    let t = &mut tree;
    let mut layers = Vec::new();
    for n in 0..=MAX_OPEN {
        let words = UiBuilder::text(t, "Layer").unwrap();
        layers.push(UiBuilder::dialog(t, LayerId(n as u16), words).unwrap());
    }
    let root = UiBuilder::column(t, Layout::new(), &layers).unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 200, 100), &metrics()).unwrap();
    for n in 0..MAX_OPEN {
        t.open(LayerId(n as u16)).unwrap();
    }
    assert_eq!(
        t.open(LayerId(MAX_OPEN as u16)),
        Err(UiError::TooManyLayers)
    );
}

proptest! {
    #[test]
    fn whatever_is_done_focus_stays_in_the_layer_holding_it(
        steps in prop::collection::vec((0usize..10, 0i32..400, 0i32..300), 0..40),
    ) {
        let mut s = screen();
        let t = &mut s.tree;
        for (step, x, y) in steps {
            match step {
                0 => { let _ = t.open(CONFIRM); }
                1 => { let _ = t.open(MORE); }
                2 => { let _ = t.open(HELP); }
                3 => { let _ = t.close(CONFIRM); }
                4 => { let _ = key(t, KeyCode::ESCAPE); }
                5 => { let _ = key(t, KeyCode::TAB); }
                6 => { let _ = key(t, KeyCode::ENTER); }
                7 => { let _ = key(t, KeyCode::DOWN); }
                _ => { let _ = click(t, Point::new(x, y)); }
            }
            prop_assert!(t.shown_layers().count() <= MAX_OPEN + 1);
            // The top layer holding focus has it, if it has a control.
            let top = t.shown_layers().filter(|&id| {
                t.layer_kind(id).is_some_and(nowayos_ui::LayerKind::traps)
            }).last();
            if let (Some(top), Some(focused)) = (top, t.focused()) {
                prop_assert_eq!(t.layer_of(focused), Some(top));
            }
            let _ = (s.yes, s.no);
        }
    }
}
