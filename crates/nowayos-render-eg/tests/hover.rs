// SPDX-License-Identifier: MIT OR Apache-2.0
//! Hover states (#10082): a hovered button on its hover fill, a hovered
//! radio option on the hover tint, a hovered input's border twice as
//! thick; and after the pointer moves, a repaint of what it marked gives a
//! full repaint's pixels.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_input::InputEvent;
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::Framebuffer;
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, ButtonVariant, FieldId, Input, NodeId, Tree};

fn screen(theme: &Theme<Rgb888>) -> (Tree<16>, [NodeId; 3]) {
    let mut tree: Tree<16> = Tree::new();
    let t = &mut tree;
    let button = UiBuilder::button(t, "Go", ActionId(1)).unwrap();
    let input = UiBuilder::input(t, FieldId(1), Input::new()).unwrap();
    let choice = UiBuilder::radio_group(t, FieldId(2), &["One", "Two"]).unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().gap(12).padding(12),
        &[button, input, choice],
    )
    .unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 320, 320), &theme.metrics())
        .unwrap();
    (tree, [button, input, choice])
}

fn frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

fn to(tree: &mut Tree<16>, x: i64, y: i64) {
    tree.handle(InputEvent::PointerMove {
        x: i32::try_from(x).unwrap(),
        y: i32::try_from(y).unwrap(),
    });
}

#[test]
fn what_the_pointer_hovers_is_drawn_so_and_repainted_alone() {
    let theme = Theme::<Rgb888>::light();
    let (mut tree, [button, input, choice]) = screen(&theme);
    let at_rest = frame(&tree, &theme);
    // Just inside the button's corner, clear of its label and its curve.
    let rect = tree.get(button).unwrap().rect();
    let (x, y) = (rect.left() + 8, rect.top() + rect.size.height as i64 / 2);
    let colours = theme.button_on(None, ButtonVariant::Primary);
    let pixel = |fb: &Framebuffer| fb.pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
    assert_eq!(pixel(&at_rest), colours.fill);
    let mut previous = at_rest;
    for (target, check) in [(button, 0), (input, 1), (choice, 2)] {
        let rect = tree.get(target).unwrap().rect();
        // A radio group's first option, or the control's corner.
        let spot = tree.get(target).unwrap().option_rect(0).unwrap_or(rect);
        to(&mut tree, spot.left() + 4, spot.top() + 4);
        assert_eq!(tree.hover().node, Some(target));
        let full = frame(&tree, &theme);
        // What the move marked, repainted over the frame before, is the
        // whole frame.
        let mut part = previous.clone();
        for &dirty in tree.dirty_rects() {
            nowayos_testkit::glyph_store!(glyphs__);
            render_area(&tree, &mut part, &theme, dirty, &mut glyphs__).unwrap();
        }
        assert_eq!(part.diff(&full), None, "hovering {target:?}");
        tree.clear_dirty();
        match check {
            0 => assert_eq!(pixel(&full), Some(colours.hover)),
            1 => {
                // Its border's second pixel row: the border colour, not
                // the fill.
                let (bx, by) = (
                    u32::try_from(rect.left() + rect.size.width as i64 / 2).unwrap(),
                    u32::try_from(rect.top() + 1).unwrap(),
                );
                assert_eq!(full.pixel(bx, by), Some(theme.input_border));
                assert_eq!(previous.pixel(bx, by), Some(theme.input));
            }
            _ => {
                let row = tree.get(choice).unwrap().option_rect(0).unwrap();
                let (rx, ry) = (
                    u32::try_from(row.right() - 2).unwrap(),
                    u32::try_from(row.top() + 2).unwrap(),
                );
                assert_eq!(full.pixel(rx, ry), Some(theme.hover_over(theme.background)));
            }
        }
        previous = full;
    }
    // Away from everything: as at rest.
    to(&mut tree, 310, 310);
    assert_eq!(tree.hover().node, None);
}
