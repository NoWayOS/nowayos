// SPDX-License-Identifier: OFL-1.1
//! NoWayOS's glyph outlines: every character the UI draws, as the outlines
//! of the signed-off font's faces ([`Face`], [`Outline`]), which
//! `nowayos-raster` draws on the device at any size, so the device draws
//! proportional text without ever parsing a font file.
//!
//! **Provisional**: `nowayos-ui` lays text out with these metrics, and
//! `nowayos-render-eg` draws it from these outlines; either may change this
//! API.
//!
//! Each [`Face`] holds an [`Outline`] for every character of its set - its
//! advance and its path, in font units - the face's line metrics and the
//! font's pair kerning, in font units, which [`Face::scale`] scales to 1/64
//! pixel at any size. A character outside the set has no glyph
//! ([`NoGlyph`]): nothing is ever drawn as a box. Lookups search sorted
//! tables; nothing allocates and nothing panics.
//!
//! ```
//! use nowayos_glyphs::{NoGlyph, REGULAR};
//!
//! let at_24 = REGULAR.scale(24 * 64).ok_or(NoGlyph)?;
//! let (t, o) = (REGULAR.glyph('T')?, REGULAR.glyph('o')?);
//! let advances = at_24.to_64(t.advance()) + at_24.to_64(o.advance());
//! // "To" is narrower than its two advances: the font kerns the pair.
//! let kerned = at_24.to_64(REGULAR.kerning_units('T', 'o'));
//! assert!(advances + kerned < advances);
//! assert!(REGULAR.glyph('\u{3BC}').is_err()); // outside the set
//! # Ok::<(), NoGlyph>(())
//! ```
//!
//! # Where the outlines come from
//!
//! `tools/glyph-atlas`, a host tool in a Cargo workspace of its own, reads
//! them with `swash` from the font files and settings in `glyphs.toml` -
//! every file pinned by its sha256 - and writes each face's outline and
//! kerning modules and `manifest.toml` into `src/atlas/` (`make atlases`).
//! They are checked in; `make atlases-check`, in `make ci`, writes them
//! again and fails on any difference, so it is what checks their values:
//! every point, advance, metric and kerned pair. The tool's tests rasterise
//! every face with `swash` at every size `glyphs.toml` lists, in memory,
//! and check that these outlines, drawn by `nowayos-raster`, measure exactly
//! the same and draw the same to within a level or two of coverage.
//!
//! Their structure is checked when they compile: each face's outlines are
//! exactly its characters, in order, every outline's steps take exactly its
//! points, and its kerned pairs are in order, each naming two of its
//! characters, none adjusting by nothing.
//!
//! The character set is printable Latin-1, the 190 characters
//! `nowayos_ui::text::is_printable` accepts. The font has no micro sign, so
//! U+00B5 is drawn with the glyph of U+03BC, the Greek mu: the one alias,
//! signed off with the font. Outlines are unhinted, each glyph meant to be
//! drawn at a whole pixel; ligatures are off, so every character is one
//! glyph.
//!
//! # Faces
//!
//! [`REGULAR`] and [`BOLD`] set every role of the type scale, and
//! [`DISPLAY`] its display role. Rich text's italics are [`ITALIC`] and
//! [`BOLDITALIC`], and its code [`CODE`] and [`CODEBOLD`], the monospace
//! sibling: [`styled`] picks the one a run is drawn in.
//!
//! # Licence
//!
//! The outlines are a Modified Version of Atkinson Hyperlegible Next 2.001
//! and Atkinson Hyperlegible Mono 2.001 under the SIL Open Font License
//! 1.1, and so is this whole crate: `OFL-1.1`, whatever licence NoWayOS's
//! own crates take ([`NOTICE`]; the licence is `OFL.txt` beside this
//! crate's `Cargo.toml`, with both families' copyright lines, and each
//! family's licence file as released is in `fonts/`). `make image` puts
//! that notice beside every image, and its SBOM names the fonts.

#![no_std]

#[rustfmt::skip]
mod atlas;
mod outline;

pub use atlas::*;
pub use outline::{Face, Outline, Path};
pub(crate) use outline::{lay_out_outlines, outlines_fit};

/// The face a run of text in `face` is drawn in when it is in italics or
/// code, and whether it is slanted as an oblique because no face draws it:
/// italics in [`ITALIC`] or [`BOLDITALIC`], code in [`CODE`] or
/// [`CODEBOLD`] - Bold's for a Bold face's runs - and code in italics, or
/// the display face's italics, slanted.
///
/// ```
/// use nowayos_glyphs::{BOLD, BOLDITALIC, CODE, DISPLAY, REGULAR, styled};
///
/// assert!(core::ptr::eq(styled(&BOLD, true, false).0, &BOLDITALIC));
/// assert!(core::ptr::eq(styled(&REGULAR, true, true).0, &CODE));
/// assert!(styled(&REGULAR, true, true).1);
/// let (face, slanted) = styled(&DISPLAY, true, false);
/// assert!(core::ptr::eq(face, &DISPLAY) && slanted);
/// ```
#[must_use]
pub fn styled(face: &'static Face, italic: bool, code: bool) -> (&'static Face, bool) {
    let bold = face.id() == BOLD.id() || face.id() == BOLDITALIC.id() || face.id() == CODEBOLD.id();
    let display = face.id() == DISPLAY.id();
    match (italic, code) {
        (_, true) if !display => (if bold { &CODEBOLD } else { &CODE }, italic),
        (true, false) if !display => (if bold { &BOLDITALIC } else { &ITALIC }, false),
        (true, _) => (face, true),
        (false, _) => (face, false),
    }
}

/// No glyph: the character is outside the face's set. It never says which
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
    /// use nowayos_glyphs::REGULAR;
    ///
    /// // 984, 316 and 0 font units of 1,000 a line, at 24 pixels per em:
    /// // 31.2 pixels, so 32.
    /// let at_24 = REGULAR.scale(24 * 64).unwrap();
    /// assert_eq!(REGULAR.line_metrics(at_24).line_height(), 32);
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

/// A kerned pair: indexes into its face's characters, and the adjustment
/// to the pen between them, in font units, which every size of the face
/// shares: [`Face::scale`] scales it when it is looked up.
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
/// 1/64 pixel, rounded half away from zero - exactly as the glyph tool
/// scales every metric (`tools/glyph-atlas`'s `to_64`); 0 if it would not
/// fit.
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

/// The index of `ch` in `chars`, sorted, or `u16::MAX` if it is not
/// there - which [`kerning_is_whole`] then refuses, when the crate
/// compiles. Used only to build the generated kerning tables.
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

/// Whether a face's kerned pairs are whole: in strictly increasing order,
/// each naming two of its `count` characters, and none of them 0 - as
/// [`Face::kerning_units`]' search needs. Checked for every face when the
/// crate compiles, so a table that is not does not build; the adjustments
/// themselves are `make atlases-check`'s to check.
pub(crate) const fn kerning_is_whole(kerning: &[KernPair], count: usize) -> bool {
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

// Checked when the crate compiles: every face's kerning is whole.
const _: () = assert!(
    REGULAR.kerning_is_whole()
        && BOLD.kerning_is_whole()
        && DISPLAY.kerning_is_whole()
        && ITALIC.kerning_is_whole()
        && BOLDITALIC.kerning_is_whole()
        && CODE.kerning_is_whole()
        && CODEBOLD.kerning_is_whole()
);

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

    /// Every face.
    fn faces() -> [&'static Face; 7] {
        [
            &REGULAR,
            &BOLD,
            &DISPLAY,
            &ITALIC,
            &BOLDITALIC,
            &CODE,
            &CODEBOLD,
        ]
    }

    fn is_display(face: &Face) -> bool {
        face.id() == DISPLAY.id()
    }

    /// The monospace face: no kerned pairs, its letters set in cells of
    /// one width.
    fn is_code(face: &Face) -> bool {
        face.id() == CODE.id() || face.id() == CODEBOLD.id()
    }

    /// Each face's set: the display face's or the whole set.
    fn in_face_set(face: &Face, ch: char) -> bool {
        if is_display(face) {
            in_display_set(ch)
        } else {
            in_set(ch)
        }
    }

    /// `units` of `face` at `px` pixels per em, in 1/64 pixel.
    fn at(face: &Face, px: u32, units: i32) -> i32 {
        face.scale(px * 64).unwrap().to_64(units)
    }

    #[test]
    fn every_face_has_its_whole_set_and_nothing_else() {
        for face in faces() {
            let count = if is_display(face) { 46 } else { 190 };
            assert_eq!(face.glyphs().len(), count, "face {}", face.id());
            for outline in face.glyphs() {
                assert!(in_face_set(face, outline.ch()), "{:?}", outline.ch());
            }
        }
    }

    #[test]
    fn code_is_monospace_and_styled_picks_each_face() {
        for face in [&CODE, &CODEBOLD] {
            let em = face.glyph('m').unwrap().advance();
            for ch in (' '..='~')
                .chain('\u{A0}'..='\u{FF}')
                .filter(|&c| c != '\u{AD}')
            {
                assert_eq!(face.glyph(ch).unwrap().advance(), em, "{ch:?}");
            }
        }
        let picked = |face, italic, code| {
            let (face, slanted): (&Face, bool) = styled(face, italic, code);
            (face.id(), slanted)
        };
        assert_eq!(picked(&REGULAR, false, false), (REGULAR.id(), false));
        assert_eq!(picked(&REGULAR, true, false), (ITALIC.id(), false));
        assert_eq!(picked(&BOLD, true, false), (BOLDITALIC.id(), false));
        assert_eq!(picked(&REGULAR, false, true), (CODE.id(), false));
        assert_eq!(picked(&BOLD, false, true), (CODEBOLD.id(), false));
        assert_eq!(picked(&BOLD, true, true), (CODEBOLD.id(), true));
        assert_eq!(picked(&DISPLAY, true, false), (DISPLAY.id(), true));
        assert_eq!(picked(&DISPLAY, false, true), (DISPLAY.id(), false));
    }

    #[test]
    fn glyphs_have_ink_where_they_should() {
        for face in faces() {
            let spaces: &[char] = if is_display(face) {
                &[' ']
            } else {
                &[' ', '\u{A0}']
            };
            for &ch in spaces {
                let space = face.glyph(ch).unwrap();
                assert!(!space.has_ink(), "{ch:?}");
                assert!(space.advance() > 0);
            }
            for outline in face.glyphs() {
                if !matches!(outline.ch(), ' ' | '\u{A0}') {
                    assert!(outline.has_ink(), "{:?}", outline.ch());
                }
            }
            if is_display(face) {
                continue;
            }
            // Below the baseline: j, which reaches back over the pen - but
            // in the monospace face, inside its cell.
            let j = face.glyph('j').unwrap();
            assert!(j.min().y < 0);
            assert_eq!(j.min().x < 0, !is_code(face), "face {}", face.id());
            // The micro sign is drawn with the Greek mu: a descender, ink.
            let micro = face.glyph('\u{B5}').unwrap();
            assert!(micro.has_ink() && micro.min().y < 0);
        }
    }

    #[test]
    fn metrics_scale_with_the_size() {
        // 984 and 316 font units of 1,000, no line gap.
        for (face, px, ascent, descent, height) in [
            (&REGULAR, 18, 1134, 364, 24),
            (&REGULAR, 24, 1511, 485, 32),
            (&REGULAR, 36, 2267, 728, 47),
            (&REGULAR, 48, 3023, 971, 63),
            (&BOLD, 18, 1134, 364, 24),
            (&BOLD, 56, 3527, 1133, 73),
            (&DISPLAY, 96, 6046, 1942, 125),
        ] {
            let line = face.line_metrics(face.scale(px * 64).unwrap());
            assert_eq!(
                (line.ascent(), line.descent(), line.line_gap()),
                (ascent, descent, 0),
                "{px} px"
            );
            assert_eq!(line.line_height(), height);
        }
        // Twice the size, twice the advance, to within rounding: what a
        // screen drawn at device scale 2 draws from.
        for (face, small) in [
            (&REGULAR, 18),
            (&REGULAR, 24),
            (&BOLD, 28),
            (&BOLD, 36),
            (&DISPLAY, 96),
        ] {
            for outline in face.glyphs() {
                let (one, two) = (
                    at(face, small, outline.advance()),
                    at(face, small * 2, outline.advance()),
                );
                assert!(
                    (two - 2 * one).abs() <= 1,
                    "{:?} at {small} px",
                    outline.ch()
                );
            }
        }
        // Bold is wider than Regular.
        assert!(BOLD.glyph('m').unwrap().advance() > REGULAR.glyph('m').unwrap().advance());
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
        for face in faces() {
            if is_code(face) {
                for left in face.glyphs() {
                    for right in face.glyphs() {
                        assert_eq!(
                            face.kerning_units(left.ch(), right.ch()),
                            0,
                            "the monospace face kerns nothing"
                        );
                    }
                }
                continue;
            }
            if !is_display(face) {
                assert!(face.kerning_units('T', 'o') < 0, "face {}", face.id());
            }
            assert!(face.kerning_units('A', 'V') < 0, "face {}", face.id());
            assert_eq!(face.kerning_units('o', '\u{3BC}'), 0);
            assert_eq!(face.kerning_units('\u{2014}', 'T'), 0);
            assert!(face.kerning_is_whole());
        }
        // "To" closes up by 80 font units of 1,000: 1.92 pixels at 24 px,
        // scaled from the units every size of the face shares.
        assert_eq!(REGULAR.kerning_units('T', 'o'), -80);
        assert_eq!(at(&REGULAR, 24, -80), -123);
        assert_eq!(at(&REGULAR, 48, -80), -246);
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
        // And as a face scales its kerning.
        assert_eq!(scale_units(-80, 24, 1000), at(&REGULAR, 24, -80));
    }

    #[test]
    fn a_damaged_kerning_table_is_not_whole() {
        let chars = ['a', 'b'];
        assert_eq!(
            (index_of_char(&chars, 'a'), index_of_char(&chars, 'b')),
            (0, 1)
        );
        assert_eq!(
            (index_of_char(&chars, 'c'), index_of_char(&[], 'a')),
            (u16::MAX, u16::MAX)
        );
        let pair = KernPair::new(0, 1, -3);
        assert!(kerning_is_whole(&[pair], 2));
        assert!(kerning_is_whole(&[], 2));
        // Pairs: outside the characters, out of order, twice or 0.
        assert!(!kerning_is_whole(&[KernPair::new(0, 2, -3)], 2));
        assert!(!kerning_is_whole(
            &[KernPair::new(index_of_char(&chars, 'c'), 0, -3)],
            2
        ));
        assert!(!kerning_is_whole(
            &[KernPair::new(1, 0, 1), KernPair::new(0, 1, 1)],
            2
        ));
        assert!(!kerning_is_whole(&[pair, pair], 2));
        assert!(!kerning_is_whole(&[KernPair::new(0, 1, 0)], 2));
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
            for face in faces() {
                prop_assert_eq!(face.glyph(left).is_ok(), in_face_set(face, left));
                if !in_face_set(face, left) || !in_face_set(face, right) {
                    prop_assert_eq!(face.kerning_units(left, right), 0);
                }
            }
        }
    }
}
