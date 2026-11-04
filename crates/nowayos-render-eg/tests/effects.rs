// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shadows, gradients and opacity (#10079), as the golden image shows:
//! cards casting each of the theme's shadows over a picture, a linear and
//! a radial gradient between theme colours, and a card drawn half
//! see-through over a picture; a repaint of any part, composed in tiles
//! (ADR-0027), gives the full repaint's pixels; and no band shows in a
//! gradient at 32 bits.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, RgbColor};
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_area};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::pictures::{Fit, samples};
use nowayos_ui::{Colour, Elevation, Gradient, Look, Surface, Tree};

fn screen(theme: &Theme<Rgb888>) -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let t = &mut tree;
    let picture = UiBuilder::image(
        t,
        &samples::LANDSCAPE,
        Fit::Cover,
        "",
        Layout::new().width(560).height(140),
    )
    .unwrap();
    let mut cards = Vec::new();
    for (elevation, name) in [
        (Elevation::Low, "Low"),
        (Elevation::Medium, "Medium"),
        (Elevation::High, "High"),
    ] {
        let words = UiBuilder::text(t, name).unwrap();
        cards.push(
            UiBuilder::column_on(
                t,
                Surface::Card,
                Layout::new()
                    .padding(12)
                    .width(150)
                    .look(Look::NONE.shadow(elevation)),
                &[words],
            )
            .unwrap(),
        );
    }
    let row = UiBuilder::row(t, Layout::new().gap(40).padding(16), &cards).unwrap();
    let linear_words = UiBuilder::text(t, "A linear gradient").unwrap();
    let linear = UiBuilder::column(
        t,
        Layout::new().padding(16).width(260).height(70).look(
            Look::NONE
                .radius(12)
                .gradient(Gradient::linear(90, Colour::Input, Colour::Focus)),
        ),
        &[linear_words],
    )
    .unwrap();
    let radial_words = UiBuilder::text(t, "Radial").unwrap();
    let radial = UiBuilder::column(
        t,
        Layout::new().padding(16).width(260).height(70).look(
            Look::NONE
                .radius(12)
                .gradient(Gradient::radial(Colour::Input, Colour::Placeholder)),
        ),
        &[radial_words],
    )
    .unwrap();
    let fills = UiBuilder::row(t, Layout::new().gap(24), &[linear, radial]).unwrap();
    let under = UiBuilder::image(
        t,
        &samples::LANDSCAPE,
        Fit::Cover,
        "",
        Layout::new().width(260).height(80),
    )
    .unwrap();
    let ghost_words = UiBuilder::text(t, "Half see-through").unwrap();
    let ghost = UiBuilder::column_on(
        t,
        Surface::Card,
        Layout::new().padding(12).look(Look::NONE.opacity(128)),
        &[ghost_words],
    )
    .unwrap();
    let stack = t
        .add_with_children(
            nowayos_ui::ContainerKind::Stack,
            nowayos_ui::Style::new().width(nowayos_ui::Length::Fixed(260)),
            &[under, ghost],
        )
        .unwrap();
    let root = UiBuilder::column(
        t,
        Layout::new().gap(20).padding(20),
        &[picture, row, fills, stack],
    )
    .unwrap();
    t.set_root(root).unwrap();
    t.layout(Rect::new(0, 0, 600, 520), &theme.metrics())
        .unwrap();
    tree
}

fn frame<const N: usize>(tree: &Tree<N>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

#[test]
fn shadows_gradients_and_opacity_as_approved_and_repainted_whole() {
    let theme = Theme::<Rgb888>::light();
    let tree = screen(&theme);
    let full = frame(&tree, &theme);
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/effects.png");
    check_snapshot(&full, &golden).unwrap();
    let mut part = full.clone();
    part.clear(Rgb888::new(1, 2, 3)).unwrap();
    let size = tree.viewport().size;
    for y in (0..size.height).step_by(53) {
        for x in (0..size.width).step_by(71) {
            nowayos_testkit::glyph_store!(glyphs__);
            render_area(
                &tree,
                &mut part,
                &theme,
                Rect::new(x as i32, y as i32, 71, 53),
                &mut glyphs__,
            )
            .unwrap();
        }
    }
    assert_eq!(part.diff(&full), None);
}

#[test]
fn a_gradient_at_32_bits_shows_no_band() {
    // From a colour to one four levels away across 400 pixels: undithered,
    // each level would hold 100 pixels and step a whole level at its edge.
    // Dithered, each 4 x 4 block's mean rises a sixteenth of a level at a
    // time, and never falls.
    let mut tree: Tree<4> = Tree::new();
    let theme = Theme::<Rgb888> {
        input: Rgb888::new(100, 100, 100),
        focus: Rgb888::new(104, 104, 104),
        ..Theme::light()
    };
    let fill = UiBuilder::column(
        &mut tree,
        Layout::new()
            .width(400)
            .height(8)
            .look(Look::NONE.gradient(Gradient::linear(90, Colour::Input, Colour::Focus))),
        &[],
    )
    .unwrap();
    tree.set_root(fill).unwrap();
    tree.layout(Rect::new(0, 0, 400, 8), &theme.metrics())
        .unwrap();
    let fb = frame(&tree, &theme);
    let block = |left: u32| -> u32 {
        (0..4)
            .flat_map(|y| (0..4).map(move |x| (left + x, y)))
            .map(|(x, y)| u32::from(fb.pixel(x, y).unwrap().r()))
            .sum()
    };
    let sums: Vec<u32> = (0..100).map(|b| block(b * 4)).collect();
    for pair in sums.windows(2) {
        assert!(pair[1] >= pair[0], "{sums:?}");
        assert!(
            pair[1] - pair[0] <= 4,
            "a step of {} sixteenths: {sums:?}",
            pair[1] - pair[0]
        );
    }
    let row: Vec<u8> = (0..400).map(|x| fb.pixel(x, 0).unwrap().r()).collect();
    assert_eq!((row[0], row[399]), (100, 104));
}

#[test]
fn every_theme_s_shadows_stay_within_the_reach_repainted_round_a_node() {
    for (_, theme) in Theme::<Rgb888>::all() {
        for shadow in [theme.shadows.low, theme.shadows.medium, theme.shadows.high] {
            assert!(
                shadow.dy + shadow.blur + shadow.spread <= nowayos_ui::layer::SHADOW_REACH,
                "{shadow:?}"
            );
        }
    }
}

#[test]
fn text_over_a_gradient_is_checked_at_its_worse_end() {
    let theme = Theme::<Rgb888>::light();
    let tree = screen(&theme);
    // "A linear gradient", over input white to the focus orange: checked
    // against the orange, the worse.
    let checked = nowayos_render_eg::contrast::text_colours(&tree, &theme)
        .find(|&(_, _, ground)| ground == theme.focus);
    assert!(checked.is_some());
}
