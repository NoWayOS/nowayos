// SPDX-License-Identifier: MIT OR Apache-2.0
//! A list picker, drawn: its header showing the placeholder until a
//! choice is made, then the choice; its options on a text input's fill; and
//! a bar down its edge when the list holds more than it shows.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::builder::{Layout, Picker, UiBuilder};
use nowayos_ui::{FieldId, Tree};

const OPTIONS: &[&str] = &[
    "Bag", "Phone", "Keys", "Wallet", "Glasses", "Umbrella", "Coat",
];

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

#[test]
fn the_header_shows_the_placeholder_then_the_choice() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let mut tree: Tree<8> = Tree::new();
        let picker = UiBuilder::list_picker(
            &mut tree,
            FieldId(1),
            OPTIONS,
            Picker::new().placeholder("Choose one").rows(3),
        )
        .unwrap();
        let root = UiBuilder::column(&mut tree, Layout::new().padding(10), &[picker]).unwrap();
        tree.set_root(root).unwrap();
        tree.layout(Rect::new(0, 0, 360, 300), &theme.metrics())
            .unwrap();
        let draw = |tree: &Tree<8>| {
            let mut fb = Framebuffer::new(360, 300).unwrap();
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render(tree, &mut fb, &theme, &mut glyphs__)
            }
            .unwrap();
            fb
        };
        let header = tree.get(picker).unwrap().list_row(0).unwrap();
        let before = draw(&tree);
        let ink = colours(&before, header);
        assert!(ink.contains(&theme.placeholder), "{name}: placeholder");
        assert!(ink.contains(&theme.input), "{name}: on the input fill");
        // A bar down the edge: seven options, three showing.
        let first = tree.get(picker).unwrap().list_row(1).unwrap();
        let edge = Rect::new(
            i32::try_from(first.right()).unwrap() - 6,
            i32::try_from(first.top()).unwrap(),
            6,
            first.size.height,
        );
        assert!(
            colours(&before, edge).contains(&theme.text_muted),
            "{name}: bar"
        );
        tree.set_selected(FieldId(1), Some(1)).unwrap();
        let after = draw(&tree);
        let ink = colours(&after, header);
        assert!(ink.contains(&theme.text), "{name}: the choice");
        // The header was redrawn: the placeholder gone, the choice in its
        // place (anti-aliased greys can match the placeholder's colour, so
        // the pixels are compared rather than the colours).
        let changed = (header.top()..header.bottom()).any(|y| {
            (header.left()..header.right()).any(|x| {
                let (x, y) = (u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
                before.pixel(x, y) != after.pixel(x, y)
            })
        });
        assert!(changed, "{name}: header redrawn");
    }
}
