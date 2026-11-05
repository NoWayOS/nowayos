// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where text's glyphs come from: drawn from the faces' outlines by
//! `nowayos-raster`, at the size and device scale the text is drawn at,
//! and kept in a bounded cache ([`Glyphs`]) so each is drawn once
//! (ADR-0026, proposed).
//!
//! A [`Glyphs`] holds the rasteriser itself, about 192 KiB, and a cache
//! over storage its owner gives it: a buffer of coverage and a table of
//! [`Slot`]s. A UI PD keeps both in its own frame, as a PD has no heap; a
//! host test, on its heap:
//!
//! ```
//! use nowayos_render_eg::{DEFAULT_BYTES, DEFAULT_SLOTS, Glyphs, Slot};
//!
//! let (mut pixels, mut slots) = (vec![0; DEFAULT_BYTES], vec![Slot::EMPTY; DEFAULT_SLOTS]);
//! let mut glyphs = Glyphs::new(&mut pixels, &mut slots);
//! let a = glyphs.glyph(&nowayos_glyphs::REGULAR, 18 * 64, 'a').unwrap();
//! assert!(a.coverage().iter().any(|&c| c > 0));
//! ```

use nowayos_glyphs::{Face, Outline};
pub use nowayos_raster::Slot;
use nowayos_raster::{CachedGlyph, GlyphCache, GlyphKey, Placement, Rasteriser};

/// The rasteriser every UI draws text with: 4,096 lines an outline, more
/// than any glyph of the signed-off font is cut into at any size, and
/// 16,384 cells, a band of rows of a glyph at a time.
pub type GlyphRasteriser = Rasteriser<4096, 16_384>;

/// How many glyphs a UI's cache holds by default, and its bytes of
/// coverage: enough for every character of the type scale's sizes at
/// device scale 2 at once, and for a screen of text at scale 4.
pub const DEFAULT_SLOTS: usize = 4096;
/// The default cache's coverage, in bytes: 4 MiB.
pub const DEFAULT_BYTES: usize = 4 << 20;

/// Glyphs drawn from outlines, and kept.
pub struct Glyphs<'a> {
    rasteriser: GlyphRasteriser,
    cache: GlyphCache<'a>,
}

impl<'a> Glyphs<'a> {
    /// Glyphs kept in `pixels` and `slots`; whatever they held is
    /// forgotten.
    #[must_use]
    pub fn new(pixels: &'a mut [u8], slots: &'a mut [Slot]) -> Self {
        Self {
            rasteriser: GlyphRasteriser::new(),
            cache: GlyphCache::new(pixels, slots),
        }
    }

    /// `ch`'s glyph in `face` at `size64` 1/64 pixel per em: from the
    /// cache, or drawn into it. `None` for a character the face lacks, a
    /// size the rasteriser refuses or a glyph larger than the whole cache.
    pub fn glyph(&mut self, face: &'static Face, size64: u32, ch: char) -> Option<CachedGlyph<'_>> {
        let outline: &Outline = face.glyph(ch).ok()?;
        let scale = face.outline_scale(size64)?;
        let placement: Placement = outline.placement(scale);
        let key = GlyphKey {
            face: face.id(),
            size64,
            ch,
        };
        let rasteriser = &mut self.rasteriser;
        self.cache
            .get_or_draw(key, placement, |pixels| {
                rasteriser.render(outline.path(), scale, placement, pixels)
            })
            .ok()
    }

    /// How many times its cache has been emptied because it was full.
    #[must_use]
    pub const fn clears(&self) -> u32 {
        self.cache.clears()
    }
}
