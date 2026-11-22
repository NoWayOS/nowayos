// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tree drawn whole into a framebuffer of its viewport's size.

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::Tree;

/// `tree` drawn whole in `theme`, with a fresh glyph store.
pub fn frame<const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    theme: &Theme<Rgb888>,
) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}
