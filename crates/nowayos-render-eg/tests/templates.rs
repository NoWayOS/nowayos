// SPDX-License-Identifier: MIT OR Apache-2.0
//! A slot between literal text (builder contract v14), drawn: exactly as
//! the same sentence written as one text, in each theme, set against the
//! start, centred and wrapped.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::{SlotId, TextAlign, TextStyle, Tree};

const NAME: SlotId = SlotId(1);
const PLACE: ui::Template = ui::Template {
    before: "Your table is ready, ",
    after: ". Please follow the green line.",
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 160;

/// One text in `style`, in a padded column, drawn: the templated slot set
/// to `name`, or the whole sentence as one text.
fn drawn(
    theme: &Theme<embedded_graphics::pixelcolor::Rgb888>,
    style: TextStyle,
    templated: bool,
) -> Framebuffer {
    let mut tree: Tree<4> = Tree::new();
    let b = &mut tree;
    let text = if templated {
        UiBuilder::slot_template_styled(b, NAME, &PLACE, style).unwrap()
    } else {
        UiBuilder::text_styled(
            b,
            "Your table is ready, Ada. Please follow the green line.",
            style,
        )
        .unwrap()
    };
    let root = UiBuilder::column(b, ui::Layout::new().padding(12), &[text]).unwrap();
    tree.set_root(root).unwrap();
    if templated {
        tree.set_slot(NAME, "Ada").unwrap();
    }
    tree.layout(Rect::new(0, 0, WIDTH, HEIGHT), &theme.metrics())
        .unwrap();
    let mut frame = Framebuffer::new(WIDTH, HEIGHT).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(&tree, &mut frame, theme, &mut glyphs__)
    }
    .unwrap();
    frame
}

#[test]
fn a_templated_slot_draws_as_the_sentence_written_out() {
    for (name, theme) in Theme::all() {
        for style in [
            TextStyle::BODY,
            TextStyle::BODY.align(TextAlign::Center),
            TextStyle::BODY.lines(3).align(TextAlign::End),
        ] {
            assert!(
                drawn(&theme, style, true) == drawn(&theme, style, false),
                "{name} theme, {style:?}"
            );
        }
    }
}
