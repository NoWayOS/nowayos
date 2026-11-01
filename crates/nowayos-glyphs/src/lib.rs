// SPDX-License-Identifier: OFL-1.1
//! NoWayOS's glyph atlases: every character the UI draws, rasterised in
//! advance from the signed-off font at every size the themes use, so the
//! device draws proportional text without ever parsing a font file.
//!
//! **Provisional**: `nowayos-ui` lays text out with these metrics,
//! and `nowayos-render-eg` draws text from these glyphs, anti-aliased;
//! either may change this API.
//!
//! Each [`Atlas`] is one face at one size: a [`Glyph`] for every character
//! of the set - its advance and bearings in 1/64 pixel, its ink box in whole
//! pixels and its 8-bit coverage - the face's [`LineMetrics`], and the
//! font's pair kerning in 1/64 pixel. A character outside the set has no
//! glyph ([`NoGlyph`]): nothing is ever drawn as a box. Lookups search sorted
//! tables; nothing allocates and nothing panics.
//!
//! ```
//! use nowayos_glyphs::{NoGlyph, REGULAR_24};
//!
//! let t = REGULAR_24.glyph('T')?;
//! let o = REGULAR_24.glyph('o')?;
//! // "To" is narrower than its two advances: the font kerns the pair.
//! let width = t.advance() + REGULAR_24.kerning('T', 'o') + o.advance();
//! assert!(width < t.advance() + o.advance());
//! assert_eq!(t.coverage().len(), usize::from(t.width()) * usize::from(t.height()));
//! assert_eq!(REGULAR_24.glyph('\u{3BC}'), Err(NoGlyph)); // outside the set
//! # Ok::<(), NoGlyph>(())
//! ```
//!
//! # Where the atlases come from
//!
//! `tools/glyph-atlas`, a host tool in a Cargo workspace of its own, makes
//! them with `swash` from the font files and settings in
//! `glyphs.toml` - every file pinned by its sha256 - and writes the
//! generated modules, their coverage files and `manifest.toml` into
//! `src/atlas/` (`make atlases`). They are checked in; `make atlases-check`,
//! in `make ci`, writes them again and fails on any difference, so it is
//! what checks their values: every pixel, metric and kerned pair.
//!
//! Their structure is checked when they compile. A generated table holds no
//! offsets: each glyph's pixels are the next `width * height` bytes of its
//! coverage file, in table order, so no glyph's pixels can be another's. A
//! constant assertion in each module then refuses to build an atlas whose
//! glyphs are out of order or name a character twice, whose glyphs do not
//! take exactly the whole coverage file, or whose kerned pairs are out of
//! order, name a character outside the set or adjust by nothing.
//!
//! The character set is printable Latin-1, the 190 characters
//! `nowayos_ui::text::is_printable` accepts. The font has no micro sign, so
//! U+00B5 is drawn with the glyph of U+03BC, the Greek mu: the one alias,
//! signed off with the font. Glyphs are unhinted, each meant to be drawn at
//! a whole pixel; ligatures are off, so every character is one glyph.
//!
//! # Licence
//!
//! The atlases are a Modified Version of Atkinson Hyperlegible Next 2.001
//! under the SIL Open Font License 1.1, and so is this whole crate:
//! `OFL-1.1`, whatever licence NoWayOS's own crates take ([`NOTICE`]; the
//! licence is `OFL.txt` beside this crate's `Cargo.toml`). `make image` puts
//! that notice beside every image, and its SBOM names the font.

#![no_std]

#[rustfmt::skip]
mod atlas;

pub use atlas::*;

/// No glyph: the character is outside the atlas's set. It never says which
/// character, so text a person typed cannot leak through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoGlyph;

/// A face's vertical metrics at one size, in 1/64 pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineMetrics {
    ascent: i32,
    descent: i32,
    line_gap: i32,
}

impl LineMetrics {
    pub(crate) const fn new(ascent: i32, descent: i32, line_gap: i32) -> Self {
        Self {
            ascent,
            descent,
            line_gap,
        }
    }

    /// From the baseline up to the top of a line, in 1/64 pixel.
    #[must_use]
    pub const fn ascent(&self) -> i32 {
        self.ascent
    }

    /// From the baseline down to the bottom of a line, in 1/64 pixel
    /// (positive below the baseline).
    #[must_use]
    pub const fn descent(&self) -> i32 {
        self.descent
    }

    /// The space the font asks for between lines, in 1/64 pixel.
    #[must_use]
    pub const fn line_gap(&self) -> i32 {
        self.line_gap
    }

    /// A line's height in whole pixels: ascent, descent and line gap
    /// together, rounded up; 0 if they come to nothing.
    ///
    /// ```
    /// // 984, 316 and 0 font units of 1,000 a line, at 24 pixels per em:
    /// // 31.2 pixels, so 32.
    /// assert_eq!(nowayos_glyphs::REGULAR_24.line_metrics().line_height(), 32);
    /// ```
    #[must_use]
    pub const fn line_height(&self) -> u32 {
        let total = self
            .ascent
            .saturating_add(self.descent)
            .saturating_add(self.line_gap);
        if total <= 0 {
            0
        } else {
            total.unsigned_abs().div_ceil(64)
        }
    }
}

/// One character's glyph at one size.
///
/// Its pen position is on the baseline, at a whole pixel. Its ink box is
/// [`width`](Self::width) by [`height`](Self::height) pixels, its top-left
/// corner [`left`](Self::left) pixels right of the pen and
/// [`top`](Self::top) pixels above the baseline; [`coverage`](Self::coverage)
/// fills it row by row from the top, one byte a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub(crate) ch: char,
    pub(crate) advance: i32,
    pub(crate) bearing_x: i32,
    pub(crate) bearing_y: i32,
    pub(crate) left: i16,
    pub(crate) top: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) coverage: &'static [u8],
}

impl Glyph {
    /// The character this glyph draws.
    #[must_use]
    pub const fn ch(&self) -> char {
        self.ch
    }

    /// How far the pen moves after this glyph, in 1/64 pixel.
    #[must_use]
    pub const fn advance(&self) -> i32 {
        self.advance
    }

    /// From the pen to the left edge of the outline, in 1/64 pixel
    /// (negative when the glyph reaches back over the one before).
    #[must_use]
    pub const fn bearing_x(&self) -> i32 {
        self.bearing_x
    }

    /// From the baseline up to the top of the outline, in 1/64 pixel.
    #[must_use]
    pub const fn bearing_y(&self) -> i32 {
        self.bearing_y
    }

    /// From the pen to the ink box's left edge, in pixels.
    #[must_use]
    pub const fn left(&self) -> i16 {
        self.left
    }

    /// From the baseline up to the ink box's top edge, in pixels.
    #[must_use]
    pub const fn top(&self) -> i16 {
        self.top
    }

    /// The ink box's width in pixels: 0 for a glyph with no ink, a space.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// The ink box's height in pixels.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// The ink box's coverage, row by row from the top: one byte a pixel,
    /// 0 for none and 255 for full; `width * height` bytes.
    #[must_use]
    pub const fn coverage(&self) -> &'static [u8] {
        self.coverage
    }
}

/// A kerned pair: indexes into its face's characters - every size of a
/// face has the same glyphs, in the same order - and the adjustment to the
/// pen between them, in font units, which every size of the face shares:
/// an atlas scales it to its size when it is looked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KernPair {
    left: u16,
    right: u16,
    units: i16,
}

impl KernPair {
    pub(crate) const fn new(left: u16, right: u16, units: i16) -> Self {
        Self { left, right, units }
    }
}

/// `units` of a font with `upem` units per em, at `size` pixels per em, in
/// 1/64 pixel, rounded half away from zero - exactly as the atlas tool
/// scales every metric (`tools/glyph-atlas`'s `to_64`); 0 if it would not
/// fit, which the tool refuses for every pair of every atlas.
#[must_use]
pub const fn scale_units(units: i32, size: u16, upem: u16) -> i32 {
    let upem = upem as i64;
    let Some(twice) = (units as i64).checked_mul(size as i64) else {
        return 0;
    };
    let Some(twice) = twice.checked_mul(128) else {
        return 0;
    };
    let rounded = if twice >= 0 {
        twice.saturating_add(upem)
    } else {
        twice.saturating_sub(upem)
    };
    let Some(divisor) = upem.checked_mul(2) else {
        return 0;
    };
    match rounded.checked_div(divisor) {
        Some(scaled) if scaled <= i32::MAX as i64 && scaled >= i32::MIN as i64 => scaled as i32,
        _ => 0,
    }
}

/// One face of the font at one size: a glyph for every character of its
/// set, the face's line metrics and its pair kerning.
pub struct Atlas {
    size: u16,
    line: LineMetrics,
    glyphs: &'static [Glyph],
    kerning: &'static [KernPair],
    upem: u16,
}

impl Atlas {
    pub(crate) const fn new(
        size: u16,
        line: LineMetrics,
        glyphs: &'static [Glyph],
        kerning: &'static [KernPair],
        upem: u16,
    ) -> Self {
        Self {
            size,
            line,
            glyphs,
            kerning,
            upem,
        }
    }

    /// The size, in pixels per em.
    #[must_use]
    pub const fn size(&self) -> u16 {
        self.size
    }

    /// The face's vertical metrics at this size.
    #[must_use]
    pub const fn line_metrics(&self) -> LineMetrics {
        self.line
    }

    /// Every glyph, in the order of their characters: the atlas's whole
    /// character set.
    #[must_use]
    pub const fn glyphs(&self) -> &'static [Glyph] {
        self.glyphs
    }

    /// The glyph that draws `ch`.
    ///
    /// # Errors
    ///
    /// [`NoGlyph`] if `ch` is outside the atlas's set.
    pub fn glyph(&self, ch: char) -> Result<&'static Glyph, NoGlyph> {
        let index = self.index(ch)?;
        self.glyphs.get(index).ok_or(NoGlyph)
    }

    /// The adjustment to the pen between `left` and `right` when one
    /// follows the other, in 1/64 pixel: negative brings them closer. 0 for
    /// a pair the font does not kern, or a character outside the set.
    #[must_use]
    pub fn kerning(&self, left: char, right: char) -> i32 {
        let (Ok(left), Ok(right)) = (self.index(left), self.index(right)) else {
            return 0;
        };
        let (Ok(left), Ok(right)) = (u16::try_from(left), u16::try_from(right)) else {
            return 0;
        };
        self.kerning
            .binary_search_by(|pair| (pair.left, pair.right).cmp(&(left, right)))
            .ok()
            .and_then(|found| self.kerning.get(found))
            .map_or(0, |pair| {
                scale_units(i32::from(pair.units), self.size, self.upem)
            })
    }

    fn index(&self, ch: char) -> Result<usize, NoGlyph> {
        self.glyphs
            .binary_search_by(|glyph| glyph.ch.cmp(&ch))
            .map_err(|_| NoGlyph)
    }
}

/// The index of `ch` in `chars`, sorted, or `u16::MAX` if it is not
/// there - which [`is_whole`] then refuses, when the atlas compiles. Used
/// only to build the generated kerning tables.
pub(crate) const fn index_of_char(chars: &[char], ch: char) -> u16 {
    let (mut low, mut high) = (0_usize, chars.len());
    while low < high {
        let middle = low.midpoint(high);
        let Some(&found) = (match chars.split_at_checked(middle) {
            Some((_, rest)) => rest.first(),
            None => None,
        }) else {
            return u16::MAX;
        };
        if found == ch {
            return if middle < u16::MAX as usize {
                middle as u16
            } else {
                u16::MAX
            };
        }
        if found < ch {
            low = middle.saturating_add(1);
        } else {
            high = middle;
        }
    }
    u16::MAX
}

/// Whether `glyphs` draw exactly `chars`, in order: so a face's kerning,
/// which names its characters by index, means the same in every atlas of
/// the face. Checked by a constant assertion in each generated module.
pub(crate) const fn same_chars(glyphs: &[Glyph], chars: &[char]) -> bool {
    if glyphs.len() != chars.len() {
        return false;
    }
    let (mut glyphs, mut chars) = (glyphs, chars);
    while let (Some((glyph, glyph_rest)), Some((&ch, char_rest))) =
        (glyphs.split_first(), chars.split_first())
    {
        if glyph.ch != ch {
            return false;
        }
        glyphs = glyph_rest;
        chars = char_rest;
    }
    true
}

/// `glyphs`, each given the next `width * height` bytes of `coverage` as its
/// pixels, in order: so the glyphs take the coverage file's bytes in table
/// order, each byte at most once, and a generated table holds no offsets. A
/// glyph whose pixels would run past the end gets none, which [`is_whole`]
/// then refuses when the atlas compiles, as it refuses bytes left over. Used
/// only to build the generated tables.
pub(crate) const fn lay_out<const N: usize>(
    coverage: &'static [u8],
    mut glyphs: [Glyph; N],
) -> [Glyph; N] {
    let mut rest = coverage;
    let mut todo: &mut [Glyph] = &mut glyphs;
    while let Some((glyph, tail)) = todo.split_first_mut() {
        let len = (glyph.width as usize).saturating_mul(glyph.height as usize);
        let (pixels, after): (&'static [u8], &'static [u8]) = match rest.split_at_checked(len) {
            Some(split) => split,
            None => (&[], &[]),
        };
        glyph.coverage = pixels;
        rest = after;
        todo = tail;
    }
    glyphs
}

/// Whether a generated atlas's structure is whole, checked by a constant
/// assertion in each generated module, so one that is not does not compile:
/// the glyphs in strictly increasing order of their characters, each
/// glyph's coverage exactly its `width * height` bytes, and all of them
/// together exactly the `coverage_len` bytes of the atlas's coverage file -
/// which, as [`lay_out`] gives each glyph the next bytes, means they take
/// every byte of it once; the kerned pairs in strictly increasing order, each
/// naming two of the glyphs, and none of them 0. The values themselves - the
/// pixels, metrics and adjustments - are `make atlases-check`'s to check.
pub(crate) const fn is_whole(glyphs: &[Glyph], kerning: &[KernPair], coverage_len: usize) -> bool {
    let mut total: usize = 0;
    let mut previous: Option<char> = None;
    let mut rest = glyphs;
    while let Some((glyph, tail)) = rest.split_first() {
        if let Some(before) = previous
            && before >= glyph.ch
        {
            return false;
        }
        let Some(len) = (glyph.width as usize).checked_mul(glyph.height as usize) else {
            return false;
        };
        if glyph.coverage.len() != len {
            return false;
        }
        total = match total.checked_add(len) {
            Some(sum) => sum,
            None => return false,
        };
        previous = Some(glyph.ch);
        rest = tail;
    }
    if total != coverage_len {
        return false;
    }
    let count = glyphs.len();
    let mut previous: Option<(u16, u16)> = None;
    let mut rest = kerning;
    while let Some((pair, tail)) = rest.split_first() {
        if pair.left as usize >= count || pair.right as usize >= count || pair.units == 0 {
            return false;
        }
        if let Some((left, right)) = previous
            && (left > pair.left || (left == pair.left && right >= pair.right))
        {
            return false;
        }
        previous = Some((pair.left, pair.right));
        rest = tail;
    }
    true
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The set, written out independently of glyphs.toml.
    fn in_set(ch: char) -> bool {
        matches!(ch, ' '..='~' | '\u{A0}'..='\u{FF}') && ch != '\u{AD}'
    }

    /// The display face's set: digits, capitals and a few signs.
    fn in_display_set(ch: char) -> bool {
        matches!(ch, ' ' | '#' | '%' | '+'..=':' | 'A'..='Z' | '\u{A3}')
    }

    /// Each atlas's set: the display face's or the whole set.
    fn in_atlas_set(atlas: &Atlas, ch: char) -> bool {
        if is_display(atlas) {
            in_display_set(ch)
        } else {
            in_set(ch)
        }
    }

    fn is_display(atlas: &Atlas) -> bool {
        core::ptr::eq(atlas, &DISPLAY_96) || core::ptr::eq(atlas, &DISPLAY_192)
    }

    #[test]
    fn every_atlas_has_its_whole_set_and_nothing_else() {
        assert_eq!(ATLASES.len(), 22);
        for atlas in ATLASES {
            let count = if is_display(atlas) { 46 } else { 190 };
            assert_eq!(atlas.glyphs().len(), count, "{} px", atlas.size());
            for code in 0..=0x2FF_u32 {
                let ch = char::from_u32(code).unwrap();
                assert_eq!(
                    atlas.glyph(ch).is_ok(),
                    in_atlas_set(atlas, ch),
                    "{ch:?} at {} px",
                    atlas.size()
                );
            }
            for glyph in atlas.glyphs() {
                assert_eq!(atlas.glyph(glyph.ch()), Ok(glyph));
            }
        }
    }

    #[test]
    fn the_sizes_are_every_themes_type_scale() {
        let sizes: [u16; 22] = core::array::from_fn(|i| ATLASES[i].size());
        assert_eq!(
            sizes,
            [
                14, 16, 18, 24, 28, 32, 36, 48, // Regular
                14, 16, 18, 20, 24, 28, 32, 36, 40, 48, 56, 72, // Bold
                96, 192, // display
            ]
        );
        assert_eq!(REGULAR_18.size(), 18);
        assert_eq!(BOLD_56.size(), 56);
        assert_eq!(DISPLAY_192.size(), 192);
    }

    #[test]
    fn glyphs_have_ink_where_they_should() {
        for atlas in ATLASES {
            let spaces: &[char] = if is_display(atlas) {
                &[' ']
            } else {
                &[' ', '\u{A0}']
            };
            for &ch in spaces {
                let space = atlas.glyph(ch).unwrap();
                assert_eq!(
                    (space.width(), space.height(), space.coverage()),
                    (0, 0, &[][..])
                );
                assert!(space.advance() > 0);
            }
            for glyph in atlas.glyphs() {
                assert_eq!(
                    glyph.coverage().len(),
                    usize::from(glyph.width()) * usize::from(glyph.height())
                );
                if !matches!(glyph.ch(), ' ' | '\u{A0}') {
                    assert!(glyph.coverage().iter().any(|&c| c > 0), "{:?}", glyph.ch());
                }
            }
            if is_display(atlas) {
                continue;
            }
            // Below the baseline: g and j, and j reaches back over the pen.
            let j = atlas.glyph('j').unwrap();
            assert!(i32::from(j.top()) < i32::from(j.height()));
            assert!(j.bearing_x() < 0);
            // The micro sign is drawn with the Greek mu: a descender, ink.
            let micro = atlas.glyph('\u{B5}').unwrap();
            assert!(i32::from(micro.top()) < i32::from(micro.height()));
        }
    }

    #[test]
    fn metrics_scale_with_the_size() {
        // 984 and 316 font units of 1,000, no line gap.
        for (atlas, ascent, descent, height) in [
            (&REGULAR_18, 1134, 364, 24),
            (&REGULAR_24, 1511, 485, 32),
            (&REGULAR_36, 2267, 728, 47),
            (&REGULAR_48, 3023, 971, 63),
            (&BOLD_18, 1134, 364, 24),
            (&BOLD_56, 3527, 1133, 73),
            (&DISPLAY_96, 6046, 1942, 125),
        ] {
            let line = atlas.line_metrics();
            assert_eq!(
                (line.ascent(), line.descent(), line.line_gap()),
                (ascent, descent, 0),
                "{} px",
                atlas.size()
            );
            assert_eq!(line.line_height(), height);
        }
        // Twice the size, twice the advance, to within rounding: what a
        // screen drawn at device scale 2 draws from.
        for (small, large) in [
            (&REGULAR_18, &REGULAR_36),
            (&REGULAR_24, &REGULAR_48),
            (&BOLD_28, &BOLD_56),
            (&BOLD_36, &BOLD_72),
            (&DISPLAY_96, &DISPLAY_192),
        ] {
            for glyph in small.glyphs() {
                let big = large.glyph(glyph.ch()).unwrap();
                assert!(
                    (big.advance() - 2 * glyph.advance()).abs() <= 1,
                    "{:?} at {} px",
                    glyph.ch(),
                    small.size()
                );
            }
        }
        // Bold is wider than Regular.
        let width = |atlas: &Atlas| atlas.glyph('m').unwrap().advance();
        assert!(width(&BOLD_18) > width(&REGULAR_18));
    }

    #[test]
    fn line_height_rounds_up_and_never_goes_below_nothing() {
        assert_eq!(LineMetrics::new(64, 0, 0).line_height(), 1);
        assert_eq!(LineMetrics::new(64, 1, 0).line_height(), 2);
        assert_eq!(LineMetrics::new(0, 0, 0).line_height(), 0);
        assert_eq!(LineMetrics::new(-500, 10, 0).line_height(), 0);
        assert_eq!(
            LineMetrics::new(i32::MAX, i32::MAX, i32::MAX).line_height(),
            33_554_432
        );
    }

    #[test]
    fn the_font_kerns_its_pairs() {
        for atlas in ATLASES {
            if !is_display(atlas) {
                assert!(atlas.kerning('T', 'o') < 0, "{} px", atlas.size());
                assert!(atlas.kerning.len() > 1000);
            }
            assert!(atlas.kerning('A', 'V') < 0, "{} px", atlas.size());
            assert_eq!(atlas.kerning('o', '\u{3BC}'), 0);
            assert_eq!(atlas.kerning('\u{2014}', 'T'), 0);
        }
        // "To" closes up by 80 font units of 1,000: 1.92 pixels at 24 px,
        // scaled from the units every size of the face shares.
        assert_eq!(REGULAR_24.kerning('T', 'o'), -123);
        assert_eq!(REGULAR_48.kerning('T', 'o'), -246);
        assert!(is_whole(REGULAR_24.glyphs(), REGULAR_24.kerning, 37_259));
    }

    #[test]
    fn kerning_scales_as_the_tool_scales_every_metric() {
        // Rounded half away from zero, either side; 0 for what cannot fit.
        assert_eq!(scale_units(-80, 24, 1000), -123); // -122.88
        assert_eq!(scale_units(984, 18, 1000), 1134); // 1133.568
        assert_eq!(scale_units(316, 48, 1000), 971); // 970.752
        assert_eq!(scale_units(1, 1, 128), 1);
        assert_eq!(scale_units(-1, 1, 128), -1);
        assert_eq!(scale_units(1, 1, 129), 0);
        assert_eq!(scale_units(1, 1, 0), 0);
        assert_eq!(scale_units(i32::MAX, u16::MAX, 1), 0);
    }

    #[test]
    fn every_atlas_takes_its_coverage_file_glyph_by_glyph() {
        let files: [(&Atlas, &[u8]); 22] = [
            (&REGULAR_14, include_bytes!("atlas/regular_14.coverage")),
            (&REGULAR_16, include_bytes!("atlas/regular_16.coverage")),
            (&REGULAR_18, include_bytes!("atlas/regular_18.coverage")),
            (&REGULAR_24, include_bytes!("atlas/regular_24.coverage")),
            (&REGULAR_28, include_bytes!("atlas/regular_28.coverage")),
            (&REGULAR_32, include_bytes!("atlas/regular_32.coverage")),
            (&REGULAR_36, include_bytes!("atlas/regular_36.coverage")),
            (&REGULAR_48, include_bytes!("atlas/regular_48.coverage")),
            (&BOLD_14, include_bytes!("atlas/bold_14.coverage")),
            (&BOLD_16, include_bytes!("atlas/bold_16.coverage")),
            (&BOLD_18, include_bytes!("atlas/bold_18.coverage")),
            (&BOLD_20, include_bytes!("atlas/bold_20.coverage")),
            (&BOLD_24, include_bytes!("atlas/bold_24.coverage")),
            (&BOLD_28, include_bytes!("atlas/bold_28.coverage")),
            (&BOLD_32, include_bytes!("atlas/bold_32.coverage")),
            (&BOLD_36, include_bytes!("atlas/bold_36.coverage")),
            (&BOLD_40, include_bytes!("atlas/bold_40.coverage")),
            (&BOLD_48, include_bytes!("atlas/bold_48.coverage")),
            (&BOLD_56, include_bytes!("atlas/bold_56.coverage")),
            (&BOLD_72, include_bytes!("atlas/bold_72.coverage")),
            (&DISPLAY_96, include_bytes!("atlas/display_96.coverage")),
            (&DISPLAY_192, include_bytes!("atlas/display_192.coverage")),
        ];
        for (atlas, file) in files {
            let mut at = 0;
            for glyph in atlas.glyphs() {
                let end = at + glyph.coverage().len();
                assert_eq!(
                    glyph.coverage(),
                    &file[at..end],
                    "{:?} at {} px",
                    glyph.ch(),
                    atlas.size()
                );
                at = end;
            }
            assert_eq!(at, file.len(), "{} px", atlas.size());
        }
    }

    #[test]
    fn a_damaged_table_is_not_whole() {
        static PIXELS: [u8; 6] = [1, 2, 3, 4, 5, 6];
        let glyph = |ch, width, height| Glyph {
            ch,
            advance: 0,
            bearing_x: 0,
            bearing_y: 0,
            left: 0,
            top: 0,
            width,
            height,
            coverage: &[],
        };
        // Each glyph takes the next width * height bytes, in order: their
        // pixels follow one another, so no glyph's can be another's.
        let two = lay_out(&PIXELS, [glyph('a', 2, 2), glyph('b', 1, 2)]);
        assert_eq!(
            (two[0].coverage(), two[1].coverage()),
            (&[1, 2, 3, 4][..], &[5, 6][..])
        );
        let pair = KernPair::new(0, 1, -3);
        assert!(is_whole(&two, &[pair], 6));
        let chars = ['a', 'b'];
        assert!(same_chars(&two, &chars));
        assert!(!same_chars(&two, &['a', 'c']));
        assert!(!same_chars(&two, &['a']));
        assert_eq!(
            (index_of_char(&chars, 'a'), index_of_char(&chars, 'b')),
            (0, 1)
        );
        assert_eq!(
            (index_of_char(&chars, 'c'), index_of_char(&[], 'a')),
            (u16::MAX, u16::MAX)
        );
        // Pixels past the end of the coverage, or bytes left over.
        let long = lay_out(&PIXELS, [glyph('a', 2, 2), glyph('b', 1, 3)]);
        assert_eq!(long[1].coverage(), &[] as &[u8]);
        assert!(!is_whole(&long, &[], 6));
        assert!(!is_whole(&lay_out(&PIXELS, [glyph('a', 2, 2)]), &[], 6));
        // A glyph's pixels not its width times its height.
        let mut changed = two;
        changed[1].height = 1;
        assert!(!is_whole(&changed, &[], 6));
        // Out of order, or twice.
        assert!(!is_whole(
            &lay_out(&PIXELS, [glyph('b', 1, 2), glyph('a', 2, 2)]),
            &[],
            6
        ));
        assert!(!is_whole(
            &lay_out(&PIXELS, [glyph('a', 1, 2), glyph('a', 2, 2)]),
            &[],
            6
        ));
        // Pairs: outside the glyphs, out of order, twice or 0.
        assert!(!is_whole(&two, &[KernPair::new(0, 2, -3)], 6));
        assert!(!is_whole(
            &two,
            &[KernPair::new(index_of_char(&chars, 'c'), 0, -3)],
            6
        ));
        assert!(!is_whole(
            &two,
            &[KernPair::new(1, 0, 1), KernPair::new(0, 1, 1)],
            6
        ));
        assert!(!is_whole(&two, &[pair, pair], 6));
        assert!(!is_whole(&two, &[KernPair::new(0, 1, 0)], 6));
    }

    #[test]
    fn the_notice_names_the_font_and_its_licence() {
        assert!(
            NOTICE
                .starts_with("Copyright 2020-2024 The Atkinson Hyperlegible Next Project Authors")
        );
        assert!(NOTICE.contains("SIL Open Font License, Version 1.1"));
    }

    proptest! {
        #[test]
        fn lookups_never_panic_and_agree_with_the_set(left in any::<char>(), right in any::<char>()) {
            for atlas in ATLASES {
                prop_assert_eq!(atlas.glyph(left).is_ok(), in_atlas_set(atlas, left));
                if !in_atlas_set(atlas, left) || !in_atlas_set(atlas, right) {
                    prop_assert_eq!(atlas.kerning(left, right), 0);
                }
            }
        }
    }
}
