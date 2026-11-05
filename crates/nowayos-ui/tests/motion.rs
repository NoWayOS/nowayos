// SPDX-License-Identifier: MIT OR Apache-2.0
//! Motion (#10086) in the tree: off until the loop that drives the clock
//! turns it on; a dialog's default transition and a new screen's; input
//! during a transition acts on the end state at once; animations end,
//! repeat and stop; and a session wipe leaves nothing moving.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use nowayos_display::Rect;
use nowayos_input::InputEvent;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::motion::{Animation, MotionSetting, Property, Visual};
use nowayos_ui::{
    ActionId, LayerId, Navigator, NodeId, ScreenId, TextMetrics, Tree, UiError, UiEvent,
};

const ASK: LayerId = LayerId(1);
const KEEP: ActionId = ActionId(1);

fn screen() -> (Tree<16>, NodeId) {
    let mut tree: Tree<16> = Tree::new();
    let title = UiBuilder::text(&mut tree, "Beneath").unwrap();
    let keep = UiBuilder::button(&mut tree, "Keep", KEEP).unwrap();
    let dialog = UiBuilder::dialog(&mut tree, ASK, keep).unwrap();
    let root = UiBuilder::column(&mut tree, Layout::new(), &[title, dialog]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 300, 200), &TextMetrics::new(8, 16))
        .unwrap();
    (tree, keep)
}

#[test]
fn input_during_a_transition_acts_on_the_end_state_at_once() {
    let (mut tree, keep) = screen();
    tree.set_motion(MotionSetting::Full);
    tree.open(ASK).unwrap();
    let dialog = tree.find_layer(ASK).unwrap();
    assert!(tree.is_moving());
    assert_eq!(tree.visual(dialog).opacity, 0, "starting unseen");
    // Pressed where Keep is laid out, before a single frame: chosen.
    let at = tree.get(keep).unwrap().rect().center();
    tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    let chosen = tree.handle(InputEvent::PointerButton {
        button: 0,
        down: false,
    });
    assert_eq!(chosen, Some(UiEvent::Action(KEEP)));
    assert!(tree.advance(80), "still moving half way");
    assert!(!tree.advance(80));
    assert_eq!(tree.visual(dialog), Visual::REST);
}

#[test]
fn motion_is_off_until_turned_on() {
    let (mut tree, _) = screen();
    tree.open(ASK).unwrap();
    assert!(!tree.is_moving());
}

#[test]
fn a_new_screen_slides_and_fades_in() {
    let mut tree: Tree<16> = Tree::new();
    tree.set_motion(MotionSetting::Full);
    let mut nav: Navigator<4> = Navigator::new(
        ScreenId(0),
        Rect::new(0, 0, 300, 200),
        TextMetrics::new(8, 16),
    );
    let build = |_: ScreenId, tree: &mut Tree<16>| -> Result<NodeId, UiError> {
        UiBuilder::text(tree, "A screen")
    };
    nav.show_start(&mut tree, build).unwrap();
    assert!(!tree.is_moving(), "the start screen just shows");
    nav.go(ScreenId(1), &mut tree, build).unwrap();
    let root = tree.root().unwrap();
    assert!(tree.visual(root).offset.0 > 0, "from the right");
    tree.advance(200);
    assert_eq!(tree.visual(root), Visual::REST);
    nav.back(&mut tree, build).unwrap();
    assert!(
        tree.visual(tree.root().unwrap()).offset.0 < 0,
        "from the left"
    );
}

#[test]
fn animations_repeat_stop_and_a_wipe_leaves_nothing_moving() {
    let (mut tree, keep) = screen();
    tree.set_motion(MotionSetting::Full);
    tree.animate(keep, Animation::pulse()).unwrap();
    tree.advance(5_000);
    assert!(tree.is_moving(), "a pulse goes on");
    tree.stop(keep, Property::Scale);
    assert!(!tree.is_moving());
    tree.animate(keep, Animation::spin()).unwrap();
    tree.wipe_session();
    assert!(!tree.is_moving());
    assert_eq!(tree.visual(keep), Visual::REST);
}
