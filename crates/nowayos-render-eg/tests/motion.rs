// SPDX-License-Identifier: MIT OR Apache-2.0
//! Motion (#10086), as the golden images show: a dialog's default
//! transition - fading and growing - at fixed steps of the runtime's
//! clock; a spinning icon, a pulsing card, an indeterminate progress bar
//! and a shimmering placeholder at one moment; repaints of only what moved
//! giving a full repaint's pixels at every step; and under reduced motion,
//! no movement.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Point, RgbColor};
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_dirty};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::motion::{Animation, MotionSetting, Property};
use nowayos_ui::{ActionId, Colour, LayerId, SlotId, Surface, Tree, icons};

const ASK: LayerId = LayerId(1);

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn frame(tree: &Tree<24>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

fn dialog_screen(theme: &Theme<Rgb888>) -> Tree<24> {
    let mut tree: Tree<24> = Tree::new();
    let t = &mut tree;
    let title = UiBuilder::text(t, "A screen beneath").unwrap();
    let card_text = UiBuilder::text(t, "Something on a card").unwrap();
    let card =
        UiBuilder::column_on(t, Surface::Card, Layout::new().padding(12), &[card_text]).unwrap();
    let question = UiBuilder::text(t, "Keep the changes?").unwrap();
    let keep = UiBuilder::button(t, "Keep", ActionId(1)).unwrap();
    let body = UiBuilder::column(t, Layout::new().gap(12), &[question, keep]).unwrap();
    let dialog = UiBuilder::dialog(t, ASK, body).unwrap();
    let root =
        UiBuilder::column(t, Layout::new().gap(12).padding(16), &[title, card, dialog]).unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 320, 200), &theme.metrics())
        .unwrap();
    tree
}

/// Frames side by side, for one image of a timeline.
fn montage(frames: &[Framebuffer]) -> Framebuffer {
    let (w, h) = (frames[0].size().width, frames[0].size().height);
    let mut out = Framebuffer::new(w * frames.len() as u32, h).unwrap();
    for (index, frame) in frames.iter().enumerate() {
        for y in 0..h {
            for x in 0..w {
                let colour = frame.pixel(x, y).unwrap();
                out.draw_iter([embedded_graphics::Pixel(
                    Point::new((index as u32 * w + x) as i32, y as i32),
                    colour,
                )])
                .unwrap();
            }
        }
    }
    out
}

#[test]
fn a_dialog_fades_and_grows_in_as_the_timeline_shows_each_step_repainted_exactly() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = dialog_screen(&theme);
    tree.set_motion(MotionSetting::Full);
    let mut fb = frame(&tree, &theme);
    tree.clear_dirty();
    tree.open(ASK).unwrap();
    let mut frames = Vec::new();
    for step in [0, 40, 40, 80] {
        tree.advance(step);
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut fb, &theme, &mut glyphs__).unwrap();
        }
        tree.clear_dirty();
        let full = frame(&tree, &theme);
        assert_eq!(fb.diff(&full), None, "the step after {step} ms");
        frames.push(full);
    }
    assert!(!tree.is_moving(), "160 ms on, the transition has ended");
    check_snapshot(&montage(&frames), &golden("motion-dialog.png")).unwrap();
}

#[test]
fn named_animations_at_one_moment_as_approved() {
    let theme = Theme::<Rgb888>::light();
    let mut tree: Tree<24> = Tree::new();
    let t = &mut tree;
    let spinner = UiBuilder::icon(t, &icons::SHIELD_CHECK).unwrap();
    let pulse_text = UiBuilder::text(t, "Pulsing").unwrap();
    let pulse =
        UiBuilder::column_on(t, Surface::Card, Layout::new().padding(12), &[pulse_text]).unwrap();
    let bar = UiBuilder::progress(t, SlotId(1)).unwrap();
    let shimmer_text = UiBuilder::text(t, "Loading").unwrap();
    let shimmer = UiBuilder::column_on(
        t,
        Surface::Card,
        Layout::new().padding(12).width(240),
        &[shimmer_text],
    )
    .unwrap();
    let colour_text = UiBuilder::text(t, "Changing colour").unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().gap(16).padding(16),
        &[spinner, pulse, bar, shimmer, colour_text],
    )
    .unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 320, 300), &theme.metrics())
        .unwrap();
    t.set_motion(MotionSetting::Full);
    t.animate(spinner, Animation::spin()).unwrap();
    t.animate(pulse, Animation::pulse()).unwrap();
    t.animate(bar, Animation::indeterminate()).unwrap();
    t.animate(shimmer, Animation::shimmer()).unwrap();
    t.animate(
        colour_text,
        Animation::new(
            Property::TextColour(Colour::Text, Colour::Error),
            0,
            1000,
            400,
        ),
    )
    .unwrap();
    t.advance(300);
    assert!(t.is_moving());
    check_snapshot(&frame(&tree, &theme), &golden("motion-named.png")).unwrap();
}

#[test]
fn under_reduced_motion_nothing_moves_and_a_fade_is_short() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = dialog_screen(&theme);
    tree.set_motion(MotionSetting::Reduced);
    tree.open(ASK).unwrap();
    let dialog = tree.find_layer(ASK).unwrap();
    assert_eq!(tree.visual(dialog).scale, 1000, "no growing");
    tree.advance(100);
    assert!(!tree.is_moving(), "a fade 100 ms at most");
    assert_eq!(tree.visual(dialog).opacity, 255);
    // Off: instant.
    let mut off = dialog_screen(&theme);
    off.open(ASK).unwrap();
    assert!(!off.is_moving());
    let _ = Rgb888::WHITE;
}
