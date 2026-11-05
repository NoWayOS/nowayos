// SPDX-License-Identifier: MIT OR Apache-2.0
//! Drawing one line of text from a face's outlines, at any size:
//! proportional, kerned and anti-aliased, with nothing read back from the
//! framebuffer. Each glyph's coverage comes from the glyph store
//! ([`Glyphs`]), drawn from its outline the first time and kept.
//!
//! **Placing.** A line is placed exactly as layout measures it
//! (`nowayos_ui::TextMetrics`):
//! each character's pen is the sum of the advances before it and of the
//! kerning of each pair up to it, in 1/64 pixel from the line's start. A
//! character the face has no glyph for takes no room, is kerned with
//! neither neighbour and is drawn as nothing - text a tree holds never has
//! one, but the two still agree. Each glyph is drawn at its pen rounded to
//! the nearest whole pixel (a half rounds right), its ink box `left` pixels
//! right of that and `top` pixels above the baseline; the baseline lies the
//! face's ascent, rounded to the nearest whole pixel, below the line's top.
//!
//! **Blending.** Each pixel of a line is drawn once, from the coverage of
//! every glyph over it, summed and capped at full: two glyphs that touch
//! share a pixel's area, so its coverage is the sum of their parts. That
//! coverage `c`, from 0 to 255, blends the text's colour over the
//! background the caller names - the node's known background, never the
//! pixel already in the framebuffer - per channel, in the colour type's own
//! depth (8 bits a channel in `Rgb888`; 5, 6 and 5 in `Rgb565`), in integer
//! arithmetic: `(text × c + background × (255 − c) + 127) / 255`, the
//! nearest whole value (255 is odd, so there is never a tie). No coverage
//! gives exactly the background and full coverage exactly the text colour.
//! A pixel no glyph covers is not drawn: it already shows the background.
//!
//! **Tiles.** The visible part of a line's ink is worked through in tiles
//! of at most 64 × 16 pixels, each with its coverage summed in a 1 KiB
//! buffer on the stack, so nothing allocates however long the line. A
//! pixel's value depends only on the line - never on the tile or the clip
//! it falls in - so painting any part of a screen gives exactly the pixels
//! a full repaint gives there.

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::Point;
use nowayos_display::Rect;
use nowayos_glyphs::Face;

use crate::Glyphs;

/// Advances, bearings and kerning come in 64ths of a pixel.
const UNITS_PER_PIXEL: i64 = 64;

/// A tile's width and height in pixels: its coverage buffer is 1 KiB.
const TILE_WIDTH: usize = 64;
const TILE_HEIGHT: usize = 16;

/// A tile's coverage, row by row.
type Tile = [[u8; TILE_WIDTH]; TILE_HEIGHT];

/// One line of text: what it says, in which face at what size (1/64 pixel
/// per em), and where its pen starts, in pixels - at the line's left edge,
/// on its baseline.
#[derive(Clone, Copy)]
pub(crate) struct Line<'a> {
    pub(crate) face: &'static Face,
    pub(crate) size64: u32,
    pub(crate) text: &'a str,
    pub(crate) left: i64,
    pub(crate) baseline: i64,
}

/// How far below a line's top its baseline lies: the face's ascent at
/// `size64`, at the nearest whole pixel - 18, 24, 35 and 47 pixels at 18,
/// 24, 36 and 48 pixels per em, whose lines are 24, 32, 47 and 63 pixels
/// high.
pub(crate) fn baseline_offset(face: &Face, size64: u32) -> i64 {
    face.scale(size64).map_or(0, |scale| {
        nearest_pixel(i64::from(face.line_metrics(scale).ascent())).unwrap_or(0)
    })
}

/// Where the baseline lies below the top of a line `line_height` pixels
/// high set in `face` at `size64`: at its own line height, the ascent,
/// rounded ([`baseline_offset`]); at another, set by the type scale, the
/// font's ascent and descent centred in the line, as CSS sets a line
/// height, rounded to the nearest pixel.
pub(crate) fn baseline_in(face: &Face, size64: u32, line_height: u32) -> i64 {
    let Some(scale) = face.scale(size64) else {
        return 0;
    };
    let metrics = face.line_metrics(scale);
    if line_height == metrics.line_height() {
        return baseline_offset(face, size64);
    }
    let (ascent, descent) = (i64::from(metrics.ascent()), i64::from(metrics.descent()));
    let leading = i64::from(line_height)
        .saturating_mul(UNITS_PER_PIXEL)
        .saturating_sub(ascent.saturating_add(descent))
        .checked_div_euclid(2)
        .unwrap_or(0);
    nearest_pixel(leading.saturating_add(ascent)).unwrap_or(0)
}

/// `units` of 1/64 pixel at the nearest whole pixel, a half rounding up;
/// `None` only beyond the ends of the `i64` range.
fn nearest_pixel(units: i64) -> Option<i64> {
    units
        .checked_add(UNITS_PER_PIXEL / 2)?
        .checked_div_euclid(UNITS_PER_PIXEL)
}

/// A rectangle by its edges, in pixels: `right` and `bottom` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edges {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Edges {
    fn of(rect: Rect) -> Self {
        Self {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        }
    }

    /// The overlap of the two, or `None` if they share no pixel.
    fn intersection(self, other: Self) -> Option<Self> {
        let overlap = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (overlap.left < overlap.right && overlap.top < overlap.bottom).then_some(overlap)
    }

    /// The smallest rectangle holding both.
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// One glyph placed on a line: its character, and its ink box's top-left
/// corner and size, in pixels.
#[derive(Clone, Copy)]
struct Placed {
    x: i64,
    y: i64,
    ch: char,
    width: u32,
    height: u32,
}

impl Placed {
    /// The glyph's ink box; `None` for a glyph with no ink, a space.
    fn ink(&self) -> Option<Edges> {
        let ink = Edges {
            left: self.x,
            top: self.y,
            right: self.x.saturating_add(i64::from(self.width)),
            bottom: self.y.saturating_add(i64::from(self.height)),
        };
        (ink.left < ink.right && ink.top < ink.bottom).then_some(ink)
    }
}

impl<'a> Line<'a> {
    /// The line's glyphs, each where it is drawn, in the order of the text.
    /// The walk is layout's: advances and kerning summed in 1/64 pixel with
    /// checked arithmetic (the line ends, drawn no further, should a sum
    /// ever leave the `i64` range, which no text a tree holds comes near).
    fn glyphs(self) -> impl Iterator<Item = Placed> + 'a {
        let Line {
            face,
            size64,
            text,
            left,
            baseline,
        } = self;
        let scales = face.scale(size64).zip(face.outline_scale(size64));
        text.chars()
            .scan((0_i64, None::<char>), move |(pen, previous), ch| {
                let (metrics, outlines) = scales?;
                let Ok(outline) = face.glyph(ch) else {
                    // No glyph: no room, and no kerning across it.
                    *previous = None;
                    return Some(None);
                };
                let kerning = previous.map_or(0, |before| {
                    i64::from(metrics.to_64(face.kerning_units(before, ch)))
                });
                let at = pen.checked_add(kerning)?;
                *pen = at.checked_add(i64::from(metrics.to_64(outline.advance())))?;
                *previous = Some(ch);
                let placement = outline.placement(outlines);
                let x = left
                    .checked_add(nearest_pixel(at)?)?
                    .checked_add(i64::from(placement.left()))?;
                let y = baseline.checked_sub(i64::from(placement.top()))?;
                Some(Some(Placed {
                    x,
                    y,
                    ch,
                    width: placement.width(),
                    height: placement.height(),
                }))
            })
            .flatten()
    }

    /// The smallest rectangle holding every glyph's ink; `None` for a line
    /// with no ink.
    fn ink(self) -> Option<Edges> {
        self.glyphs()
            .filter_map(|placed| placed.ink())
            .reduce(Edges::union)
    }

    /// Where the pen ends, in 1/64 pixel from the line's start: the width
    /// layout measures the line as, before it is rounded up.
    #[cfg(test)]
    fn end(self) -> Option<i64> {
        let Line {
            face, size64, text, ..
        } = self;
        let metrics = face.scale(size64)?;
        text.chars()
            .try_fold((0_i64, None::<char>), |(pen, previous), ch| {
                let Ok(outline) = face.glyph(ch) else {
                    return Some((pen, None));
                };
                let kerning = previous.map_or(0, |before| {
                    i64::from(metrics.to_64(face.kerning_units(before, ch)))
                });
                let pen = pen
                    .checked_add(kerning)?
                    .checked_add(i64::from(metrics.to_64(outline.advance())))?;
                Some((pen, Some(ch)))
            })
            .map(|(pen, _)| pen)
    }
}

/// Draws `line` in `color` over `background` - the colour every pixel the
/// line covers is known to have beneath it - only inside `clip`.
///
/// # Errors
///
/// Whatever the draw target reports.
pub(crate) fn draw<D>(
    target: &mut D,
    line: Line<'_>,
    clip: Rect,
    color: D::Color,
    background: D::Color,
    glyphs: &mut Glyphs<'_>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    let Some(visible) = line.ink().and_then(|ink| ink.intersection(Edges::of(clip))) else {
        return Ok(());
    };
    // Both are below i64's range by far: 64 and 16.
    let (tile_width, tile_height) = (TILE_WIDTH as i64, TILE_HEIGHT as i64);
    for top in (visible.top..visible.bottom).step_by(TILE_HEIGHT) {
        for left in (visible.left..visible.right).step_by(TILE_WIDTH) {
            let tile = Edges {
                left,
                top,
                right: left.saturating_add(tile_width).min(visible.right),
                bottom: top.saturating_add(tile_height).min(visible.bottom),
            };
            let mut cover: Tile = [[0; TILE_WIDTH]; TILE_HEIGHT];
            for placed in line.glyphs() {
                if placed
                    .ink()
                    .and_then(|ink| ink.intersection(tile))
                    .is_none()
                {
                    continue;
                }
                if let Some(glyph) = glyphs.glyph(line.face, line.size64, placed.ch) {
                    add_coverage(&mut cover, tile, placed, glyph.coverage());
                }
            }
            target.draw_iter(pixels(&cover, tile, color, background))?;
        }
    }
    Ok(())
}

/// Adds `placed`'s `coverage` to `cover`, the coverage of `tile`, wherever
/// the two overlap, capping each pixel at full.
fn add_coverage(cover: &mut Tile, tile: Edges, placed: Placed, coverage: &[u8]) {
    let Some(overlap) = placed.ink().and_then(|ink| ink.intersection(tile)) else {
        return;
    };
    let Ok(width) = usize::try_from(placed.width) else {
        return;
    };
    // Columns of the overlap, in the glyph and in the tile.
    let (Some(glyph_column), Some(tile_column), Some(len)) = (
        offset(overlap.left, placed.x),
        offset(overlap.left, tile.left),
        offset(overlap.right, overlap.left),
    ) else {
        return;
    };
    for y in overlap.top..overlap.bottom {
        let (Some(glyph_row), Some(tile_row)) = (offset(y, placed.y), offset(y, tile.top)) else {
            continue;
        };
        let source = glyph_row
            .checked_mul(width)
            .and_then(|start| start.checked_add(glyph_column))
            .and_then(|start| Some(start..start.checked_add(len)?))
            .and_then(|span| coverage.get(span));
        let target = cover
            .get_mut(tile_row)
            .and_then(|row| row.get_mut(tile_column..tile_column.checked_add(len)?));
        if let (Some(source), Some(target)) = (source, target) {
            for (sum, &coverage) in target.iter_mut().zip(source) {
                *sum = sum.saturating_add(coverage);
            }
        }
    }
}

/// `from - to` as an index: `None` if negative or too large.
fn offset(from: i64, to: i64) -> Option<usize> {
    usize::try_from(from.checked_sub(to)?).ok()
}

/// The pixels of `tile` some glyph covers, each blended from its coverage.
fn pixels<'a, C>(
    cover: &'a Tile,
    tile: Edges,
    color: C,
    background: C,
) -> impl Iterator<Item = Pixel<C>> + 'a
where
    C: RgbColor + From<Rgb888> + 'a,
{
    cover
        .iter()
        .zip(tile.top..tile.bottom)
        .flat_map(move |(row, y)| {
            row.iter()
                .zip(tile.left..tile.right)
                .filter_map(move |(&coverage, x)| {
                    if coverage == 0 {
                        return None;
                    }
                    let point = Point::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?);
                    Some(Pixel(point, blend(color, background, coverage)))
                })
        })
}

/// `color` over `background` at `coverage` out of 255, per channel in the
/// colour type's own depth: [`mix`]. Exactly `background` at 0 and exactly
/// `color` at 255.
pub(crate) fn blend<C>(color: C, background: C, coverage: u8) -> C
where
    C: RgbColor + From<Rgb888>,
{
    match coverage {
        0 => background,
        u8::MAX => color,
        _ => C::from(Rgb888::new(
            widen(mix(color.r(), background.r(), coverage), C::MAX_R),
            widen(mix(color.g(), background.g(), coverage), C::MAX_G),
            widen(mix(color.b(), background.b(), coverage), C::MAX_B),
        )),
    }
}

/// One channel of `fore` over `back` at `coverage` out of 255:
/// `(fore × coverage + back × (255 − coverage) + 127) / 255` in integers,
/// the nearest whole value to the exact blend. The two values are in the
/// same depth, and so is the result, which lies between them.
pub(crate) fn mix(fore: u8, back: u8, coverage: u8) -> u8 {
    let coverage = u32::from(coverage);
    let sum = u32::from(fore)
        .saturating_mul(coverage)
        .saturating_add(u32::from(back).saturating_mul(255_u32.saturating_sub(coverage)))
        .saturating_add(127);
    u8::try_from(sum / 255).unwrap_or(u8::MAX)
}

/// A channel `value` of a depth whose largest value is `max`, as 8 bits:
/// `value × 255 / max` at the nearest whole value. `embedded-graphics`
/// converts an `Rgb888` to a smaller depth at the nearest level too, which
/// gives back exactly `value` (tested for every value of every depth its
/// RGB colours have), so a colour is built in its own depth through
/// `From<Rgb888>`.
pub(crate) fn widen(value: u8, max: u8) -> u8 {
    if max == u8::MAX || max == 0 {
        return value;
    }
    let max = u32::from(max);
    let wide = u32::from(value)
        .saturating_mul(2 * 255)
        .saturating_add(max)
        .checked_div(max.saturating_mul(2))
        .unwrap_or(0);
    u8::try_from(wide).unwrap_or(u8::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use embedded_graphics::pixelcolor::{
        Bgr555, Bgr565, Bgr666, Bgr888, Rgb555, Rgb565, Rgb666, Rgb888,
    };
    use nowayos_glyphs::{ATLASES, DISPLAY, REGULAR};
    use nowayos_ui::TextMetrics;
    use proptest::prelude::*;

    /// The exact blend, rounded to the nearest whole value, in floating
    /// point: what the integer `mix` must give.
    fn exact(fore: u8, back: u8, coverage: u8) -> u8 {
        let c = f64::from(coverage) / 255.0;
        (f64::from(fore) * c + f64::from(back) * (1.0 - c)).round() as u8
    }

    #[test]
    fn mixing_is_the_nearest_value_to_the_exact_blend() {
        // Every coverage, for channel values at the ends and the middle of
        // every depth (5, 6 and 8 bits).
        let values = [0_u8, 1, 15, 16, 30, 31, 32, 62, 63, 127, 128, 200, 254, 255];
        for fore in values {
            for back in values {
                for coverage in 0..=255 {
                    assert_eq!(
                        mix(fore, back, coverage),
                        exact(fore, back, coverage),
                        "{fore} over {back} at {coverage}"
                    );
                }
            }
        }
        // Worked by hand: 255 over 0 at a half (128) is 128.0, and at 127
        // it is 127.0; 200 over 100 at 64 is 125.1, so 125.
        assert_eq!(mix(255, 0, 128), 128);
        assert_eq!(mix(255, 0, 127), 127);
        assert_eq!(mix(200, 100, 64), 125);
        // No coverage is the background, full coverage the text, in any
        // depth; and the result lies between the two.
        for value in 0..=255 {
            assert_eq!(mix(value, 255 - value, 0), 255 - value);
            assert_eq!(mix(value, 255 - value, 255), value);
        }
    }

    /// Every value of every depth `embedded-graphics`' RGB colours have
    /// comes back from 8 bits exactly: so building a colour of any depth
    /// through `From<Rgb888>` keeps the value `mix` gave.
    #[test]
    fn widening_a_channel_to_8_bits_comes_back_exactly() {
        for value in 0..=31 {
            let wide = Rgb888::new(widen(value, 31), widen(value, 63), widen(value, 31));
            assert_eq!(Rgb565::from(wide), Rgb565::new(value, value, value));
            assert_eq!(Bgr565::from(wide), Bgr565::new(value, value, value));
            let five = Rgb888::new(widen(value, 31), widen(value, 31), widen(value, 31));
            assert_eq!(Rgb555::from(five), Rgb555::new(value, value, value));
            assert_eq!(Bgr555::from(five), Bgr555::new(value, value, value));
        }
        for value in 0..=63 {
            let wide = Rgb888::new(widen(value, 63), widen(value, 63), widen(value, 63));
            assert_eq!(Rgb666::from(wide), Rgb666::new(value, value, value));
            assert_eq!(Bgr666::from(wide), Bgr666::new(value, value, value));
            let green = Rgb888::new(0, widen(value, 63), 0);
            assert_eq!(Rgb565::from(green), Rgb565::new(0, value, 0));
        }
        for value in 0..=255 {
            assert_eq!(widen(value, 255), value);
        }
    }

    #[test]
    fn blending_works_in_each_colour_types_own_depth() {
        // Rgb888: each channel blended in 8 bits.
        let (text, back) = (Rgb888::new(0x1F, 0x23, 0x28), Rgb888::new(0xF4, 0xF5, 0xF7));
        assert_eq!(blend(text, back, 0), back);
        assert_eq!(blend(text, back, 255), text);
        // 0x1F over 0xF4 at 128: (31 × 128 + 244 × 127 + 127) / 255 = 137.6,
        // so 137; 0x23 over 0xF5: 140.1, so 140; 0x28 over 0xF7: 143.6, so
        // 143.
        assert_eq!(blend(text, back, 128), Rgb888::new(137, 140, 143));
        // Rgb565: red and blue in 5 bits, green in 6.
        let (text, back) = (Rgb565::new(3, 4, 5), Rgb565::new(30, 61, 31));
        assert_eq!(blend(text, back, 0), back);
        assert_eq!(blend(text, back, 255), text);
        // 3 over 30 at 128: (384 + 3,810 + 127) / 255 = 16.9, so 16; 4 over
        // 61: (512 + 7,747 + 127) / 255 = 32.9, so 32; 5 over 31: 18.
        assert_eq!(blend(text, back, 128), Rgb565::new(16, 32, 18));
        for coverage in 0..=255 {
            let mixed = blend(text, back, coverage);
            assert_eq!(
                (mixed.r(), mixed.g(), mixed.b()),
                (
                    mix(3, 30, coverage),
                    mix(4, 61, coverage),
                    mix(5, 31, coverage)
                ),
                "coverage {coverage}"
            );
        }
        // Every RGB type keeps its ends exactly.
        assert_eq!(blend(Bgr888::WHITE, Bgr888::BLACK, 255), Bgr888::WHITE);
        assert_eq!(blend(Rgb666::WHITE, Rgb666::RED, 0), Rgb666::RED);
    }

    /// Each atlas's face and size: the sizes the type scales draw at
    /// device scales 1 and 2.
    fn faces() -> impl Iterator<Item = (&'static Face, u32)> {
        ATLASES
            .iter()
            .map(|atlas| (atlas.face(), u32::from(atlas.size()) * 64))
    }

    /// The ink box of `ch` in `face` at `size64`.
    fn placement(face: &Face, size64: u32, ch: char) -> nowayos_raster::Placement {
        face.glyph(ch)
            .unwrap()
            .placement(face.outline_scale(size64).unwrap())
    }

    #[test]
    fn the_baseline_is_the_ascent_at_the_nearest_pixel() {
        // 1,134, 1,511, 2,267, 3,023 and 6,046 sixty-fourths: 17.7, 23.6,
        // 35.4, 47.2 and 94.5 pixels.
        let offsets: Vec<i64> = [
            (&REGULAR, 18),
            (&REGULAR, 24),
            (&REGULAR, 36),
            (&REGULAR, 48),
            (&DISPLAY, 96),
        ]
        .iter()
        .map(|&(face, size)| baseline_offset(face, size * 64))
        .collect();
        assert_eq!(offsets, [18, 24, 35, 47, 94]);
        assert_eq!(
            (nearest_pixel(31), nearest_pixel(32), nearest_pixel(-33)),
            (Some(0), Some(1), Some(-1))
        );
        assert_eq!(nearest_pixel(i64::MAX), None);
        // Every glyph's ink stays inside its line, so a line's ink stays
        // in the line box layout gives it.
        for (face, size64) in faces() {
            let height = i64::from(face.line_metrics(face.scale(size64).unwrap()).line_height());
            for outline in face.glyphs() {
                let ink = placement(face, size64, outline.ch());
                let top = baseline_offset(face, size64) - i64::from(ink.top());
                assert!(top >= 0, "{:?} at {size64}/64 px", outline.ch());
                assert!(
                    top + i64::from(ink.height()) <= height,
                    "{:?} at {size64}/64 px",
                    outline.ch()
                );
            }
        }
    }

    /// A role the type scale sets tighter than the font's own line,
    /// display solid and titles at 120%, has the font's ascent and descent
    /// centred in its line, and every glyph of its face still inks only
    /// inside that line, in every theme at every device scale: so a node
    /// clipped to its line cuts no character off.
    #[test]
    fn a_line_set_tighter_centres_the_font_and_keeps_every_glyph_inside() {
        use crate::Theme;
        use nowayos_display::DeviceScale;
        use nowayos_ui::{TextRole, TextStyle};
        // 96 px set solid: (96 x 64 - 6,046 - 1,942) / 2 = -922 of
        // leading; plus the ascent, 6,046, is 5,124 sixty-fourths, 80.1
        // pixels.
        assert_eq!(baseline_in(&DISPLAY, 96 * 64, 96), 80);
        // At its own line, the ascent as before.
        let own = DISPLAY
            .line_metrics(DISPLAY.scale(96 * 64).unwrap())
            .line_height();
        assert_eq!(
            baseline_in(&DISPLAY, 96 * 64, own),
            baseline_offset(&DISPLAY, 96 * 64)
        );
        let mut tighter = 0;
        for (name, theme) in Theme::<Rgb888>::all() {
            for scale in (4..=16).filter_map(DeviceScale::from_quarters) {
                let metrics = theme.metrics_at(scale);
                for role in TextRole::ALL {
                    let style = TextStyle::new(role);
                    let set = metrics.for_style(style);
                    let (face, size64) = set.face().unwrap();
                    let height = set.line_height();
                    if height == face.line_metrics(face.scale(size64).unwrap()).line_height() {
                        continue;
                    }
                    tighter += 1;
                    let percent = theme.type_scale.line_heights.of(role).unwrap();
                    assert_eq!(
                        height,
                        (size64 * percent + 3200) / 6400,
                        "{name}: {role:?} at {scale}"
                    );
                    let baseline = baseline_in(face, size64, height);
                    for outline in face.glyphs() {
                        let ink = placement(face, size64, outline.ch());
                        let top = baseline - i64::from(ink.top());
                        let bottom = top + i64::from(ink.height());
                        assert!(
                            top >= 0 && bottom <= i64::from(height),
                            "{name}: {role:?} at {scale}: {:?} inks rows {top} to {bottom} of {height}",
                            outline.ch()
                        );
                    }
                }
            }
        }
        // Display and title, in both themes, at all 13 scales.
        assert_eq!(tighter, 52);
    }

    /// A line's glyphs, by hand: "To" at 24 pixels per em.
    #[test]
    fn glyphs_sit_at_the_pen_rounded_with_kerning() {
        let line = Line {
            face: &REGULAR,
            size64: 24 * 64,
            text: "To",
            left: 10,
            baseline: 30,
        };
        let placed: Vec<Placed> = line.glyphs().collect();
        assert_eq!(placed.len(), 2);
        let (t, o) = (
            placement(&REGULAR, 24 * 64, 'T'),
            placement(&REGULAR, 24 * 64, 'o'),
        );
        // T at the pen, 0; o at T's advance, 857, kerned by -123: 734/64 =
        // 11.47, so pixel 11, then its own left.
        assert_eq!(
            (placed[0].x, placed[0].y),
            (10 + i64::from(t.left()), 30 - i64::from(t.top()))
        );
        assert_eq!(
            (placed[1].x, placed[1].y),
            (10 + 11 + i64::from(o.left()), 30 - i64::from(o.top()))
        );
        assert_eq!(line.end(), Some(857 - 123 + 842));
        // A character with no glyph takes no room and breaks the kerning:
        // o then sits at T's full advance, 857/64 = 13.4, so 13.
        let broken = Line {
            text: "T\u{2014}o",
            ..line
        };
        let placed: Vec<Placed> = broken.glyphs().collect();
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[1].x, 10 + 13 + i64::from(o.left()));
        assert_eq!(broken.end(), Some(857 + 842));
    }

    /// Where two glyphs share pixels, the pixel's coverage is the sum of
    /// theirs, capped at full. Two i's with diaereses: at 24 px their dots
    /// share pixels, some covered more than fully by the two together.
    #[test]
    fn touching_glyphs_add_their_coverage_capped_at_full() {
        nowayos_testkit::glyph_store!(glyphs);
        let i = placement(&REGULAR, 24 * 64, '\u{EF}');
        let line = Line {
            face: &REGULAR,
            size64: 24 * 64,
            text: "\u{EF}\u{EF}",
            left: 0,
            baseline: i64::from(i.top()),
        };
        let coverage: Vec<u8> = glyphs
            .glyph(&REGULAR, 24 * 64, '\u{EF}')
            .unwrap()
            .coverage()
            .to_vec();
        let placed: Vec<Placed> = line.glyphs().collect();
        let (first, second) = (placed[0].ink().unwrap(), placed[1].ink().unwrap());
        assert!(first.intersection(second).is_some(), "the boxes overlap");
        let tile = Edges {
            left: 0,
            top: 0,
            right: 64,
            bottom: 16,
        };
        let mut cover: Tile = [[0; TILE_WIDTH]; TILE_HEIGHT];
        for glyph in &placed {
            add_coverage(&mut cover, tile, *glyph, &coverage);
        }
        let at = |placed: &Placed, x: i64, y: i64| -> u8 {
            let ink = placed.ink().unwrap();
            if x < ink.left || x >= ink.right || y < ink.top || y >= ink.bottom {
                return 0;
            }
            let width = i64::from(placed.width);
            coverage[((y - ink.top) * width + (x - ink.left)) as usize]
        };
        let (mut shared, mut capped) = (0, 0);
        for y in tile.top..tile.bottom {
            for x in tile.left..tile.right {
                let (one, two) = (at(&placed[0], x, y), at(&placed[1], x, y));
                let sum = u32::from(one) + u32::from(two);
                assert_eq!(
                    u32::from(cover[y as usize][x as usize]),
                    sum.min(255),
                    "({x}, {y})"
                );
                shared += u32::from(one > 0 && two > 0);
                capped += u32::from(sum > 255);
            }
        }
        assert!(shared > 0, "inked pixels the two share");
        assert!(capped > 0, "a pixel covered more than fully is full");
    }

    proptest! {
        /// Drawing places every glyph where layout measured it: the pen
        /// ends at the width layout gives the line, before rounding up,
        /// for any text of the set (and characters outside it), in every
        /// atlas.
        #[test]
        fn drawing_and_layout_measure_alike(
            text in proptest::collection::vec(
                prop_oneof![
                    8 => (0x20_u32..=0x7E).prop_map(|c| char::from_u32(c).unwrap()),
                    4 => (0xA0_u32..=0xFF).prop_map(|c| char::from_u32(c).unwrap()),
                    1 => Just('\u{2014}'),
                ],
                0..40,
            ).prop_map(|chars| chars.into_iter().collect::<std::string::String>()),
        ) {
            for atlas in ATLASES {
                let metrics = TextMetrics::from_atlas(atlas);
                let (face, size64) = (atlas.face(), u32::from(atlas.size()) * 64);
                let line = Line { face, size64, text: &text, left: 0, baseline: 0 };
                let end = line.end().unwrap();
                let width = if end <= 0 { 0 } else { u32::try_from((end + 63) / 64).unwrap() };
                prop_assert_eq!(width, metrics.text_width(&text), "{} px", atlas.size());
                // Each glyph's pen is the end of the text before it.
                let scale = face.scale(size64).unwrap();
                let mut before = std::string::String::new();
                let mut placed = line.glyphs();
                for ch in text.chars() {
                    if let Ok(outline) = face.glyph(ch) {
                        let next = placed.next().unwrap();
                        prop_assert_eq!(next.ch, ch);
                        let mut through = before.clone();
                        through.push(ch);
                        let pen = Line { text: &through, ..line }.end().unwrap()
                            - i64::from(scale.to_64(outline.advance()));
                        let rounded = (pen + 32).div_euclid(64);
                        let ink = placement(face, size64, ch);
                        prop_assert_eq!(next.x, rounded + i64::from(ink.left()));
                    }
                    before.push(ch);
                }
                prop_assert!(placed.next().is_none());
            }
        }

        /// The ends of the blend are exact, and every blend lies between
        /// its two values and is the nearest to the exact one.
        #[test]
        fn mixing_any_values(fore in any::<u8>(), back in any::<u8>(), coverage in any::<u8>()) {
            let mixed = mix(fore, back, coverage);
            prop_assert_eq!(mixed, exact(fore, back, coverage));
            prop_assert!(mixed >= fore.min(back) && mixed <= fore.max(back));
        }
    }
}
