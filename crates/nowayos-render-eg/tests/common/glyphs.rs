// SPDX-License-Identifier: MIT OR Apache-2.0
//! A fresh glyph store for one draw.

use nowayos_render_eg::Glyphs;

/// Calls `draw` with a glyph store made for it alone.
pub fn with_glyphs<R>(draw: impl FnOnce(&mut Glyphs<'_>) -> R) -> R {
    nowayos_testkit::glyph_store!(glyphs);
    draw(&mut glyphs)
}
