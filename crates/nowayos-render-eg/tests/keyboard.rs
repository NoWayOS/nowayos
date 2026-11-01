// SPDX-License-Identifier: MIT OR Apache-2.0
//! The on-screen keyboard, drawn: each key a keycap on a text
//! input's fill, its label whole in every theme; the key held down in a
//! pressed button's colours, Shift while it is on in a button's, and the
//! labels muted while no text input has focus for it to type into.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::{Insets, Point, Rect};
use nowayos_input::{InputEvent, TouchPhase};
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::keyboard::layout;
use nowayos_ui::{ActionId, FieldId, Input, InputMode, NodeId, NodeKind, Tree};

fn colours(fb: &Framebuffer, rect: Rect) -> Vec<Rgb888> {
    let mut found = Vec::new();
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            let c = fb
                .pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
                .unwrap();
            if !found.contains(&c) {
                found.push(c);
            }
        }
    }
    found
}

fn centre(rect: Rect) -> Point {
    Point::new(
        i32::try_from(rect.left()).unwrap() + i32::try_from(rect.size.width / 2).unwrap(),
        i32::try_from(rect.top()).unwrap() + i32::try_from(rect.size.height / 2).unwrap(),
    )
}

/// An input in each mode, a button and the keyboard, at 800 by 480.
fn screen(theme: &Theme<Rgb888>) -> (Tree<16>, [NodeId; 4], NodeId) {
    let mut tree: Tree<16> = Tree::new();
    let name = UiBuilder::input(&mut tree, FieldId(1), Input::new()).unwrap();
    let phone =
        UiBuilder::input(&mut tree, FieldId(2), Input::new().mode(InputMode::Digits)).unwrap();
    let email =
        UiBuilder::input(&mut tree, FieldId(3), Input::new().mode(InputMode::Email)).unwrap();
    let next = UiBuilder::button(&mut tree, "Next", ActionId(1)).unwrap();
    let keyboard = UiBuilder::keyboard(&mut tree).unwrap();
    let root = UiBuilder::column(
        &mut tree,
        Layout::new().padding(16).gap(8),
        &[name, phone, email, next, keyboard],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 800, 480), &theme.metrics())
        .unwrap();
    tree.set_focus(Some(name)).unwrap();
    (tree, [name, phone, email, next], keyboard)
}

fn draw(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let mut fb = Framebuffer::new(800, 480).unwrap();
    render(tree, &mut fb, theme).unwrap();
    fb
}

fn key_rect(tree: &Tree<16>, keyboard: NodeId, label: &str) -> Rect {
    let node = tree.get(keyboard).unwrap();
    let NodeKind::Keyboard { mode, .. } = *node.kind() else {
        panic!("not a keyboard");
    };
    let index = layout(mode)
        .keys()
        .position(|key| key.label == label)
        .unwrap();
    node.key_rect(index).unwrap()
}

#[test]
fn every_layout_is_drawn_whole_in_every_theme() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let (mut tree, inputs, keyboard) = screen(&theme);
        assert!(
            tree.get(keyboard).unwrap().rect().bottom() <= 480,
            "{name}: the keyboard fits"
        );
        for input in &inputs[..3] {
            tree.set_focus(Some(*input)).unwrap();
            let node = tree.get(keyboard).unwrap();
            assert_eq!(node.cut_text(&theme.metrics()), None, "{name}");
            let fb = draw(&tree, &theme);
            let NodeKind::Keyboard { mode, .. } = *node.kind() else {
                panic!("not a keyboard");
            };
            for (index, key) in layout(mode).keys().enumerate() {
                let ink = colours(&fb, node.key_rect(index).unwrap());
                assert!(ink.contains(&theme.input), "{name} {mode:?} {index}: fill");
                // A stroke as thin as a comma's never reaches full ink.
                if key.label.chars().all(char::is_alphanumeric) {
                    assert!(ink.contains(&theme.text), "{name} {mode:?} {index}: label");
                } else {
                    assert!(ink.len() > 3, "{name} {mode:?} {index}: label");
                }
                assert!(
                    ink.contains(&theme.input_border),
                    "{name} {mode:?} {index}: edge"
                );
            }
        }
    }
}

#[test]
fn a_held_key_and_shift_on_are_drawn_as_buttons_and_idle_keys_muted() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let (mut tree, inputs, keyboard) = screen(&theme);
        let z = key_rect(&tree, keyboard, "z");
        let shift = key_rect(&tree, keyboard, "Shift");
        let touch = |tree: &mut Tree<16>, at: Point, phase| {
            tree.handle(InputEvent::Touch {
                id: 0,
                x: at.x,
                y: at.y,
                phase,
            })
        };
        touch(&mut tree, centre(shift), TouchPhase::Start);
        touch(&mut tree, centre(shift), TouchPhase::End);
        touch(&mut tree, centre(z), TouchPhase::Start);
        let fb = draw(&tree, &theme);
        let held = colours(&fb, z);
        assert!(held.contains(&theme.button_pressed), "{name}: held");
        assert!(held.contains(&theme.button_text), "{name}: held label");
        let on = colours(&fb, shift);
        assert!(on.contains(&theme.button), "{name}: Shift on");
        assert!(on.contains(&theme.button_text), "{name}: Shift label");
        touch(&mut tree, centre(z), TouchPhase::End);
        assert_eq!(tree.value(FieldId(1)), Some("Z"));
        // On the button, nothing to type into: labels muted.
        tree.set_focus(Some(inputs[3])).unwrap();
        let fb = draw(&tree, &theme);
        // Inside its edge, which is the text's colour in high contrast.
        let idle = colours(&fb, z.inset(Insets::uniform(7)));
        assert!(idle.contains(&theme.text_muted), "{name}: muted");
        assert!(!idle.contains(&theme.text), "{name}: not live");
    }
}
