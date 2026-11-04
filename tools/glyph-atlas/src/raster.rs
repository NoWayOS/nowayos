// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rasterising one face with `swash`: every character of the set at every
//! size - unhinted, 8-bit coverage, each glyph's origin on a whole pixel -
//! with its metrics and the font's pair kerning, in 1/64 pixel.
//!
//! Metrics and kerning are read in font units, which are whole numbers, and
//! scaled to each size in integer arithmetic, so only the coverage depends
//! on floating point - `swash`'s rasteriser, which has no code specific to a
//! CPU.

use swash::scale::outline::Outline;
use swash::scale::{Render, ScaleContext, Source};
use swash::shape::ShapeContext;
use swash::text::Script;
use swash::zeno::{Format, Verb};
use swash::{FontRef, GlyphId, StringId};

use crate::Error;
use crate::spec::Spec;

/// The shaper's features for kerning pairs: GPOS kerning on; ligatures and
/// contextual forms off, so every character stays one glyph.
const FEATURES: [(&str, u16); 6] = [
    ("kern", 1),
    ("liga", 0),
    ("clig", 0),
    ("calt", 0),
    ("dlig", 0),
    ("rlig", 0),
];
/// Coverage comes from the glyph outlines only.
const SOURCES: [Source; 1] = [Source::Outline];

/// One glyph of an atlas at one size.
pub struct GlyphRow {
    /// The character it draws.
    pub ch: char,
    /// Its advance, in 1/64 pixel.
    pub advance: i32,
    /// From the pen to the outline's left edge, in 1/64 pixel.
    pub bearing_x: i32,
    /// From the baseline up to the outline's top edge, in 1/64 pixel.
    pub bearing_y: i32,
    /// From the pen to the ink box's left edge, in pixels.
    pub left: i16,
    /// From the baseline up to the ink box's top edge, in pixels.
    pub top: i16,
    /// The ink box's width, in pixels.
    pub width: u16,
    /// The ink box's height, in pixels. Its coverage is the next `width *
    /// height` bytes of the atlas's, glyph after glyph.
    pub height: u16,
}

/// A kerned pair of a face: its two characters, and the adjustment in
/// font units, which every size of the face shares: an atlas
/// scales it to 1/64 pixel at its size as the tool scales every metric.
pub struct KernRow {
    /// The first character.
    pub left: char,
    /// The second character.
    pub right: char,
    /// The adjustment to the pen between them, in font units; never 0.
    pub units: i16,
}

/// One character's outline, y up: its advance in font units, its points in
/// half font units, as a point TrueType implies between two curves' pulls
/// lies halfway.
pub struct OutlineRow {
    /// The character it draws.
    pub ch: char,
    /// Its advance.
    pub advance: i64,
    /// The least x and y of its points: the bottom-left of the box they lie
    /// in, in half font units.
    pub min: (i64, i64),
    /// The greatest x and y of its points: the top-right of that box, in
    /// half font units.
    pub max: (i64, i64),
    /// Its steps: `M` moves, `L` draws a line, `Q` a quadratic curve, `C`
    /// a cubic one and `Z` closes the contour.
    pub verbs: Vec<u8>,
    /// The points its steps take, x then y, in half font units: one for `M`
    /// and `L`, two for `Q`, three for `C`.
    pub points: Vec<i64>,
}

/// One face at every size, with its kerning in font units.
pub struct FaceAtlas {
    /// The font's units per em.
    pub upem: u16,
    /// Ascent, descent and line gap, in font units.
    pub line_units: [i64; 3],
    /// Every character's outline, in the set's order: what the device
    /// draws the face from at any size.
    pub outlines: Vec<OutlineRow>,
    /// The kerned pairs, in order of the first character, then the second;
    /// none of them 0.
    pub kerning: Vec<KernRow>,
    /// The face at each size.
    pub sizes: Vec<SizeAtlas>,
}

/// One face at one size.
pub struct SizeAtlas {
    /// Pixels per em.
    pub size: u16,
    /// Ascent, descent and line gap, in 1/64 pixel.
    pub line: [i32; 3],
    /// A glyph for every character of the set, in its order.
    pub glyphs: Vec<GlyphRow>,
    /// Every glyph's coverage, glyph after glyph.
    pub coverage: Vec<u8>,
}

/// `units` of a font with `upem` units per em, at `size` pixels per em, in
/// 1/64 pixel, rounded half away from zero; `None` if it does not fit.
#[must_use]
pub fn to_64(units: i64, size: u16, upem: u16) -> Option<i32> {
    let upem = i64::from(upem);
    let twice = units.checked_mul(i64::from(size))?.checked_mul(128)?;
    let rounded = if twice >= 0 {
        twice.checked_add(upem)?
    } else {
        twice.checked_sub(upem)?
    };
    i32::try_from(rounded.checked_div(upem.checked_mul(2)?)?).ok()
}

/// A whole number of font units from `swash`'s `f32`, which holds them
/// exactly: anything else is refused, never rounded.
fn whole_units(value: f32, file: &str, what: &str) -> Result<i64, Error> {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= 1_000_000.0 {
        // A whole number within 10^6: the conversion is exact.
        Ok(value as i64)
    } else {
        Err(Error::Format {
            file: file.into(),
            message: format!("{what} is {value} font units, not a whole number"),
        })
    }
}

/// `ch`'s outline in half font units, from `swash`'s unscaled outline:
/// every point a whole number of half units, refused otherwise, never
/// rounded.
fn outline_row(ch: char, advance: i64, outline: &Outline, file: &str) -> Result<OutlineRow, Error> {
    let mut verbs = Vec::with_capacity(outline.verbs().len());
    for verb in outline.verbs() {
        verbs.push(match verb {
            Verb::MoveTo => b'M',
            Verb::LineTo => b'L',
            Verb::QuadTo => b'Q',
            Verb::CurveTo => b'C',
            Verb::Close => b'Z',
        });
    }
    let mut points = Vec::with_capacity(outline.points().len().saturating_mul(2));
    let mut min = (i64::MAX, i64::MAX);
    let mut max = (i64::MIN, i64::MIN);
    for point in outline.points() {
        let x = whole_units(point.x * 2.0, file, "twice an outline's point")?;
        let y = whole_units(point.y * 2.0, file, "twice an outline's point")?;
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
        points.push(x);
        points.push(y);
    }
    if points.is_empty() {
        (min, max) = ((0, 0), (0, 0));
    }
    let taken: usize = verbs
        .iter()
        .map(|verb| match verb {
            b'M' | b'L' => 2,
            b'Q' => 4,
            b'C' => 6,
            _ => 0,
        })
        .sum();
    if taken != points.len() {
        return Err(Error::Format {
            file: file.into(),
            message: format!(
                "{}'s outline has steps that do not match its points",
                show(ch)
            ),
        });
    }
    Ok(OutlineRow {
        ch,
        advance,
        min,
        max,
        verbs,
        points,
    })
}

/// A character as the messages show it: its code point and the character.
pub fn show(ch: char) -> String {
    format!("U+{:04X} {ch:?}", u32::from(ch))
}

/// Rasterises the face in `data` (the file `file`, pinned at `version`) for
/// the character set `chars` - the face's own, from `spec` - at each of
/// `sizes`, with its kerning in font units.
pub fn rasterise(
    file: &str,
    data: &[u8],
    version: &str,
    spec: &Spec,
    chars: &[char],
    sizes: &[u16],
) -> Result<FaceAtlas, Error> {
    let fail = |message: String| Error::Font {
        file: file.into(),
        message,
    };
    let font = FontRef::from_index(data, 0)
        .ok_or_else(|| fail("is not a font the rasteriser can read".into()))?;
    check_version(&font, file, version)?;

    // Every character's glyph, through its alias if it has one. An alias is
    // only for a character the font lacks.
    let charmap = font.charmap();
    for (&ch, &of) in &spec.aliases {
        if charmap.map(ch) != 0 {
            return Err(fail(format!(
                "has a glyph for {} itself: {} need not draw it with the glyph of {}; remove the alias",
                show(ch),
                crate::SPEC_FILE,
                show(of)
            )));
        }
    }
    let mut sources = Vec::with_capacity(chars.len());
    let mut ids: Vec<GlyphId> = Vec::with_capacity(chars.len());
    let mut missing = Vec::new();
    for &ch in chars {
        let source = spec.aliases.get(&ch).copied().unwrap_or(ch);
        let id = charmap.map(source);
        if id == 0 {
            missing.push(if source == ch {
                show(ch)
            } else {
                format!(
                    "{}, which {} draws with the glyph of {}",
                    show(ch),
                    crate::SPEC_FILE,
                    show(source)
                )
            });
        }
        sources.push(source);
        ids.push(id);
    }
    if !missing.is_empty() {
        return Err(Error::MissingGlyphs {
            file: file.into(),
            missing,
        });
    }

    // Font units: the line metrics, and each glyph's advance and outline.
    let metrics = font.metrics(&[]);
    let upem = metrics.units_per_em;
    if upem == 0 {
        return Err(fail("has no units per em".into()));
    }
    let line = [
        whole_units(metrics.ascent, file, "the ascent")?,
        whole_units(metrics.descent, file, "the descent")?,
        whole_units(metrics.leading, file, "the line gap")?,
    ];
    let glyph_metrics = font.glyph_metrics(&[]);
    let mut context = ScaleContext::new();
    let mut units = Vec::with_capacity(ids.len());
    let mut outlines = Vec::with_capacity(ids.len());
    {
        let mut unscaled = context.builder(font).size(0.0).hint(false).build();
        for (&ch, &id) in chars.iter().zip(&ids) {
            let advance = whole_units(glyph_metrics.advance_width(id), file, "an advance")?;
            let outline = unscaled
                .scale_outline(id)
                .ok_or_else(|| fail(format!("has no outline for {}", show(ch))))?;
            let (x_min, y_max) = if outline.points().is_empty() {
                (0, 0)
            } else {
                let bounds = outline.bounds();
                (
                    whole_units(bounds.min.x, file, "an outline's left edge")?,
                    whole_units(bounds.max.y, file, "an outline's top edge")?,
                )
            };
            units.push((advance, x_min, y_max));
            outlines.push(outline_row(ch, advance, &outline, file)?);
        }
    }
    let pairs = kerning(font, file, chars, &sources, &ids, &units)?;
    let mut kerning_rows = Vec::with_capacity(pairs.len());
    for &(left, right, adjust) in &pairs {
        let units = i16::try_from(adjust).map_err(|_| Error::Format {
            file: file.into(),
            message: format!(
                "{} {}'s kerning, {adjust} font units, does not fit 16 bits",
                show(left),
                show(right)
            ),
        })?;
        kerning_rows.push(KernRow { left, right, units });
    }

    let mut atlases = Vec::with_capacity(sizes.len());
    for &size in sizes {
        let scale = |value: i64, what: &str| {
            to_64(value, size, upem).ok_or_else(|| Error::Format {
                file: file.into(),
                message: format!("{what} does not fit at {size} px"),
            })
        };
        let mut scaler = context
            .builder(font)
            .size(f32::from(size))
            .hint(false)
            .build();
        let mut render = Render::new(&SOURCES);
        render.format(Format::Alpha);
        let mut glyphs = Vec::with_capacity(ids.len());
        let mut coverage = Vec::new();
        for ((&ch, &id), &(advance, x_min, y_max)) in chars.iter().zip(&ids).zip(&units) {
            let image = render
                .render(&mut scaler, id)
                .ok_or_else(|| fail(format!("cannot rasterise {} at {size} px", show(ch))))?;
            let placement = image.placement;
            let too_big = || {
                fail(format!(
                    "{}'s ink box at {size} px does not fit the atlas format",
                    show(ch)
                ))
            };
            let (width, height) = (
                u16::try_from(placement.width).map_err(|_| too_big())?,
                u16::try_from(placement.height).map_err(|_| too_big())?,
            );
            let (left, top) = (
                i16::try_from(placement.left).map_err(|_| too_big())?,
                i16::try_from(placement.top).map_err(|_| too_big())?,
            );
            if image.data.len() != usize::from(width).saturating_mul(usize::from(height)) {
                return Err(fail(format!(
                    "{}'s coverage at {size} px is not its ink box",
                    show(ch)
                )));
            }
            glyphs.push(GlyphRow {
                ch,
                advance: scale(advance, "an advance")?,
                bearing_x: scale(x_min, "a bearing")?,
                bearing_y: scale(y_max, "a bearing")?,
                left,
                top,
                width,
                height,
            });
            coverage.extend_from_slice(&image.data);
        }
        // Every pair's adjustment must scale to this size, as every
        // metric must: the atlas scales it when it is looked up.
        for &(left, right, adjust) in &pairs {
            scale(adjust, "a kerning adjustment").map_err(|_| Error::Format {
                file: file.into(),
                message: format!(
                    "{} {}'s kerning does not fit at {size} px",
                    show(left),
                    show(right)
                ),
            })?;
        }
        atlases.push(SizeAtlas {
            size,
            line: [
                scale(line[0], "the ascent")?,
                scale(line[1], "the descent")?,
                scale(line[2], "the line gap")?,
            ],
            glyphs,
            coverage,
        });
    }
    Ok(FaceAtlas {
        upem,
        line_units: line,
        outlines,
        kerning: kerning_rows,
        sizes: atlases,
    })
}

/// The font's own version, from its name table, must be the pinned one:
/// "Version 2.001", alone or followed by `;` and more.
fn check_version(font: &FontRef<'_>, file: &str, version: &str) -> Result<(), Error> {
    let wanted = format!("Version {version}");
    let found: Vec<String> = font
        .localized_strings()
        .filter(|string| string.id() == StringId::Version)
        .map(|string| string.chars().collect())
        .collect();
    let matches = |text: &String| {
        text.strip_prefix(&wanted)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(';'))
    };
    if !found.is_empty() && found.iter().all(matches) {
        Ok(())
    } else {
        Err(Error::Font {
            file: file.into(),
            message: format!(
                "its name table gives the version as {found:?}, but {} pins {version}",
                crate::SPEC_FILE
            ),
        })
    }
}

/// Every ordered pair of the set the font kerns, in font units, from its
/// GPOS table through `swash`'s shaper: (first, second, adjustment), in
/// order. A pair the shaper turns into anything but its two glyphs, or
/// moves other than by the pen between them, is refused: the atlas format
/// holds pair kerning only.
fn kerning(
    font: FontRef<'_>,
    file: &str,
    chars: &[char],
    sources: &[char],
    ids: &[GlyphId],
    units: &[(i64, i64, i64)],
) -> Result<Vec<(char, char, i64)>, Error> {
    let mut context = ShapeContext::new();
    let mut pairs = Vec::new();
    let glyphs = || chars.iter().zip(sources).zip(ids).zip(units);
    for (((&left, &left_source), &left_id), &(left_advance, _, _)) in glyphs() {
        for (((&right, &right_source), &right_id), &(right_advance, _, _)) in glyphs() {
            let refuse = |what: &str| Error::Format {
                file: file.into(),
                message: format!(
                    "shaping {} {} {what}: the atlas format holds pair kerning only",
                    show(left),
                    show(right)
                ),
            };
            let mut shaper = context
                .builder(font)
                .script(Script::Latin)
                .size(0.0)
                .features(FEATURES)
                .build();
            shaper.add_str(&String::from_iter([left_source, right_source]));
            let mut shaped = Vec::with_capacity(2);
            shaper.shape_with(|cluster| shaped.extend_from_slice(cluster.glyphs));
            let [first, second] = shaped.as_slice() else {
                return Err(refuse("does not give two glyphs"));
            };
            if first.id != left_id || second.id != right_id {
                return Err(refuse("substitutes a glyph"));
            }
            let still = [first.x, first.y, second.x, second.y]
                .iter()
                .all(|offset| *offset == 0.0);
            if !still || whole_units(second.advance, file, "an advance")? != right_advance {
                return Err(refuse("moves a glyph other than by the pen between them"));
            }
            let adjust = whole_units(first.advance, file, "an advance")?
                .checked_sub(left_advance)
                .ok_or_else(|| refuse("overflows"))?;
            if adjust != 0 {
                pairs.push((left, right, adjust));
            }
        }
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_rounds_half_away_from_zero() {
        // "To" in a font of 1,000 units: -80 units at 24 px is -122.88/64.
        assert_eq!(to_64(-80, 24, 1000), Some(-123));
        assert_eq!(to_64(984, 18, 1000), Some(1134)); // 1133.568
        assert_eq!(to_64(316, 48, 1000), Some(971)); // 970.752
        assert_eq!(to_64(0, 24, 1000), Some(0));
        // Exactly half a 64th: away from zero, either side.
        assert_eq!(to_64(1, 1, 128), Some(1));
        assert_eq!(to_64(-1, 1, 128), Some(-1));
        assert_eq!(to_64(1, 1, 129), Some(0));
        // Too big, or no units per em.
        assert_eq!(to_64(i64::MAX, 2, 1000), None);
        assert_eq!(to_64(1 << 40, 256, 1), None);
        assert_eq!(to_64(1, 1, 0), None);
    }

    #[test]
    fn font_units_must_be_whole() {
        assert_eq!(whole_units(-61.0, "f", "x").ok(), Some(-61));
        for bad in [0.5, f32::NAN, f32::INFINITY, 2.0e6] {
            assert!(whole_units(bad, "f", "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn characters_are_shown_by_code_point() {
        assert_eq!(show('\u{B5}'), "U+00B5 '\u{B5}'");
        assert_eq!(show('\u{A0}'), "U+00A0 '\\u{a0}'");
        assert_eq!(show('\u{416}'), "U+0416 '\u{416}'");
    }
}
