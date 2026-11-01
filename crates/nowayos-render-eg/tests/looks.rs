// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a screen's style declares, drawn: a container's fill and
//! border, a text's colour and a button's fill and label, each in the
//! theme's own colour, so the large-text, high-contrast theme recolours
//! them; text inside a filled container blended over the fill.

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
use nowayos_ui::builder::UiBuilder;
use nowayos_ui::{ActionId, ButtonVariant, Colour, Look, NodeId, NodeKind, Style, TextStyle, Tree};

/// A filled, bordered column holding a coloured text, and a button with a
/// look of its own.
fn screen(theme: &Theme<Rgb888>) -> (Tree<16>, NodeId, NodeId, NodeId) {
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new().padding(10).gap(10)).unwrap();
    tree.set_root(root).unwrap();
    let panel = tree
        .column(
            Style::new().padding(12).look(
                Look::NONE
                    .fill(Colour::Input)
                    .border(Colour::Error)
                    .border_width(3),
            ),
        )
        .unwrap();
    tree.append(root, panel).unwrap();
    let text = tree
        .add(
            NodeKind::Text("Paid"),
            Style::new().text(TextStyle::BODY.colour(Colour::Focus)),
        )
        .unwrap();
    tree.append(panel, text).unwrap();
    let button = UiBuilder::button_look(
        &mut tree,
        "Done",
        ActionId(1),
        ButtonVariant::Primary,
        Look::NONE.fill(Colour::Error).text(Colour::Background),
    )
    .unwrap();
    tree.append(root, button).unwrap();
    tree.layout(Rect::new(0, 0, 320, 200), &theme.metrics())
        .unwrap();
    (tree, panel, text, button)
}

fn frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    render(tree, &mut fb, theme).unwrap();
    fb
}

fn at(fb: &Framebuffer, x: i64, y: i64) -> Rgb888 {
    fb.pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
        .unwrap()
}

/// Every colour in `rect`.
fn colours(fb: &Framebuffer, rect: Rect) -> Vec<Rgb888> {
    let mut found = Vec::new();
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            let colour = at(fb, x, y);
            if !found.contains(&colour) {
                found.push(colour);
            }
        }
    }
    found
}

#[test]
fn every_colour_a_style_names_is_the_theme_s_own() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let (tree, panel, text, button) = screen(&theme);
        let fb = frame(&tree, &theme);
        let panel = tree.get(panel).unwrap().rect();
        // The fill inside the border, the border at the edge.
        assert_eq!(
            at(&fb, panel.left() + 6, panel.top() + 6),
            theme.colour(Colour::Input),
            "{name}: fill"
        );
        assert_eq!(
            at(
                &fb,
                panel.left() + 1,
                panel.top() + panel.size.height as i64 / 2
            ),
            theme.colour(Colour::Error),
            "{name}: border"
        );
        // The text in its colour, blended over the fill: nothing else.
        let ink = colours(&fb, tree.get(text).unwrap().rect());
        assert!(ink.contains(&theme.colour(Colour::Focus)), "{name}: text");
        assert!(ink.contains(&theme.colour(Colour::Input)), "{name}: ground");
        // Where the fill differs from the background (in the large-text
        // theme both are black).
        if theme.colour(Colour::Input) != theme.background {
            assert!(
                !ink.contains(&theme.background),
                "{name}: not over the background"
            );
        }
        // The button's fill and label.
        let button = tree.get(button).unwrap().rect();
        let inside = colours(&fb, button.inset(nowayos_display::Insets::uniform(4)));
        assert!(
            inside.contains(&theme.colour(Colour::Error)),
            "{name}: button fill"
        );
        assert!(
            inside.contains(&theme.colour(Colour::Background)),
            "{name}: label"
        );
        assert!(
            !inside.contains(&theme.button),
            "{name}: not the variant's fill"
        );
    }
}

#[test]
fn the_large_text_theme_recolours_a_styled_screen() {
    let light = Theme::<Rgb888>::light();
    let large = Theme::<Rgb888>::high_contrast();
    for colour in Colour::ALL {
        // Each token is a field of each theme; the two themes differ where
        // their own colours do.
        let _ = (light.colour(colour), large.colour(colour));
    }
    assert_eq!(light.colour(Colour::Text), light.text);
    assert_eq!(large.colour(Colour::Text), large.text);
    assert_ne!(
        light.colour(Colour::Background),
        large.colour(Colour::Background)
    );
}

#[test]
fn a_styled_screen_s_text_colours_are_checked_against_what_lies_beneath() {
    use nowayos_render_eg::contrast::{Need, as_shown, ratio, text_colours};
    for (name, theme) in Theme::<Rgb888>::all() {
        let (tree, _, text, button) = screen(&theme);
        let found: Vec<_> = text_colours(&tree, &theme).collect();
        let of = |id| found.iter().find(|(at, _, _)| *at == id).unwrap();
        let (_, ink, ground) = of(text);
        assert_eq!(
            (*ink, *ground),
            (theme.colour(Colour::Focus), theme.colour(Colour::Input)),
            "{name}"
        );
        let (_, label, fill) = of(button);
        assert_eq!(
            (*label, *fill),
            (
                theme.colour(Colour::Background),
                theme.colour(Colour::Error)
            ),
            "{name}"
        );
        // Muted text on the error colour: not text anyone should read.
        let mut tree: Tree<16> = Tree::new();
        let root = tree
            .column(Style::new().look(Look::NONE.fill(Colour::Error)))
            .unwrap();
        tree.set_root(root).unwrap();
        let faint = tree
            .add(
                NodeKind::Text("Faint"),
                Style::new().text(TextStyle::BODY.colour(Colour::Error)),
            )
            .unwrap();
        tree.append(root, faint).unwrap();
        tree.layout(Rect::new(0, 0, 200, 100), &theme.metrics())
            .unwrap();
        let (_, ink, ground) = text_colours(&tree, &theme).next().unwrap();
        assert!(
            ratio(as_shown(ink), as_shown(ground)) < Need::Text.minimum(),
            "{name}"
        );
    }
}
