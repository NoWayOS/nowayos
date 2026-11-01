// SPDX-License-Identifier: MIT OR Apache-2.0
//! A scroll view, drawn: nothing inside it is drawn outside its
//! rectangle, however far it is scrolled.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, Tree};

#[test]
fn nothing_inside_the_view_is_drawn_outside_it() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let mut tree: Tree<32> = Tree::new();
        let mut buttons = Vec::new();
        for at in 0..8u16 {
            buttons.push(UiBuilder::button(&mut tree, "Pay", ActionId(at + 1)).unwrap());
        }
        let list = UiBuilder::column(&mut tree, Layout::new().gap(12), &buttons).unwrap();
        // A view 160 high, 40 below the top of a 320 x 240 screen, with
        // nothing else drawn above or below it.
        let view = UiBuilder::scroll_view(&mut tree, Layout::new().height(160), list).unwrap();
        let root = UiBuilder::column(&mut tree, Layout::new().padding(40), &[view]).unwrap();
        tree.set_root(root).unwrap();
        tree.layout(Rect::new(0, 0, 320, 240), &theme.metrics())
            .unwrap();
        // Scrolled so that a button straddles each of the view's edges.
        tree.handle(InputEvent::KeyDown(KeyCode::PAGE_DOWN));
        tree.scroll_to(tree.scroll_offset().unwrap() - 20).unwrap();
        let mut fb = Framebuffer::new(320, 240).unwrap();
        render(&tree, &mut fb, &theme).unwrap();
        let view_rect = tree.get(view).unwrap().rect();
        let straddles = |edge: i64| {
            buttons.iter().any(|&id| {
                let rect = tree.get(id).unwrap().rect();
                rect.top() < edge && rect.bottom() > edge
            })
        };
        assert!(
            straddles(view_rect.top()) || straddles(view_rect.bottom()),
            "{name}"
        );
        // Every pixel above and below the view is the background's.
        for y in (0..240).filter(|&y| y < view_rect.top() || y >= view_rect.bottom()) {
            for x in 0..320 {
                assert_eq!(
                    fb.pixel(x, u32::try_from(y).unwrap()).unwrap(),
                    theme.background,
                    "{name}: ({x}, {y}) outside the view"
                );
            }
        }
    }
}
