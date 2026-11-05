// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shapes, drawn smooth at any device scale: rectangles with rounded
//! corners - a radius per corner, a circle where the corners meet - filled,
//! with a border of their own (a button's body, a card, a field, a radio
//! option's ring and dot, a checkbox), and polygons such as a tick.
//!
//! **Coverage is exact area.** A shape's outline - lines, and each corner a
//! cubic quarter circle - is drawn by the glyph store's rasteriser
//! (`nowayos-raster`, the one that draws text) in 1/256 pixel, a tile of at
//! most 64 x 16 pixels at a time, into a buffer on the stack: each pixel's
//! coverage of the outline, and of the border's inside, from 0 to 255. Its
//! colour is the fill, the border and what lies beneath weighted by those,
//! in the colour type's own depth, in integers. A tile wholly inside the
//! plain middle of a shape is filled without the rasteriser.
//!
//! Every pixel's colour depends only on the shape and the pixel's place,
//! never on what is already in the framebuffer or on the area being
//! painted, so a dirty repaint of any part of a shape gives exactly the
//! pixels of a full repaint. A pixel the shape does not touch is left as
//! it is.

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use nowayos_display::Rect;
use nowayos_raster::{Command, Placement, Scale};

use crate::GlyphRasteriser;
use crate::text::{blend, widen};

/// Full coverage.
const FULL: u32 = 255;

/// A tile's width and height in pixels.
const TILE_WIDTH: usize = 64;
const TILE_HEIGHT: usize = 16;

/// One pixel in 1/256 pixel, the rasteriser's unit here.
const ONE: i64 = 256;

/// A quarter circle drawn as a cubic curve: each control point this far
/// along the tangent, in 10,000ths of the radius (4/3 x (sqrt 2 - 1)),
/// which keeps the curve within 0.03% of the circle.
const KAPPA: i64 = 5523;

/// A shape's three colours: inside its border, its border, and what lies
/// beneath it, which shows outside its rounded corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Paint<C> {
    /// Inside the border.
    pub fill: C,
    /// The border.
    pub border: C,
    /// Beneath the shape.
    pub ground: C,
}

/// The radius of each corner, in pixels: top left, top right, bottom
/// right, bottom left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Radii(pub [u32; 4]);

impl Radii {
    /// The same radius at every corner.
    pub(crate) const fn all(radius: u32) -> Self {
        Self([radius; 4])
    }
}

/// A rectangle with rounded corners and a border inside its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rounded {
    /// The shape's bounds.
    pub rect: Rect,
    /// The corners' radii, in pixels; each at most half the shorter side.
    pub radius: Radii,
    /// The border's width, in pixels, inside `rect`; 0 for none.
    pub border: u32,
}

/// What a draw leaves as it was: nothing, or the pixels wholly inside the
/// border - a focus ring drawn over a field keeps what the field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inside {
    /// Fill the inside.
    Fill,
    /// Leave the pixels wholly inside the border as they are; those the
    /// border's inner edge crosses blend over the fill colour.
    Keep,
}

impl Rounded {
    /// The radii and border used: each at most half the shorter side.
    fn clamped(self) -> ([i64; 4], i64) {
        let half = i64::from(self.rect.size.width.min(self.rect.size.height) / 2);
        let radii = self.radius.0.map(|radius| i64::from(radius).min(half));
        (radii, i64::from(self.border).min(half))
    }

    /// Draws the shape into `target`, only inside `clip`, its edges' coverage
    /// from `raster`.
    pub(crate) fn draw<D>(
        self,
        target: &mut D,
        clip: Rect,
        paint: Paint<D::Color>,
        raster: &mut GlyphRasteriser,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget,
        D::Color: RgbColor + From<Rgb888>,
    {
        self.draw_inside(target, clip, paint, Inside::Fill, raster)
    }

    /// As [`draw`](Self::draw), leaving the inside as `inside` says.
    pub(crate) fn draw_inside<D>(
        self,
        target: &mut D,
        clip: Rect,
        paint: Paint<D::Color>,
        inside: Inside,
        raster: &mut GlyphRasteriser,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget,
        D::Color: RgbColor + From<Rgb888>,
    {
        let Some(visible) = self.rect.intersection(&clip) else {
            return Ok(());
        };
        let (radii, border) = self.clamped();
        let outer = Edges::of(self.rect);
        let inner = outer.inset(border);
        let inner_radii = radii.map(|radius| radius.saturating_sub(border).max(0));
        // Where every pixel is wholly inside the border.
        let plain = outer.inset(radii.iter().copied().fold(border, i64::max));
        let shown = Edges::of(visible);
        for tile in tiles(shown, outer) {
            if plain.holds(tile) {
                if inside == Inside::Fill
                    && let Some(part) = tile.intersection(shown)
                {
                    fill(target, part, paint.fill)?;
                }
                continue;
            }
            let mut outside_cover = [[0_u8; TILE_WIDTH]; TILE_HEIGHT];
            let mut inside_cover = [[0_u8; TILE_WIDTH]; TILE_HEIGHT];
            cover(
                raster,
                &rounded_path(outer, radii, tile),
                tile,
                &mut outside_cover,
            );
            if border > 0 {
                if inner.left < inner.right && inner.top < inner.bottom {
                    cover(
                        raster,
                        &rounded_path(inner, inner_radii, tile),
                        tile,
                        &mut inside_cover,
                    );
                }
            } else {
                inside_cover = outside_cover;
            }
            let rows = outside_cover
                .iter()
                .zip(&inside_cover)
                .zip(tile.top..tile.bottom);
            for ((out_row, in_row), y) in rows {
                for ((&out, &inn), x) in out_row.iter().zip(in_row).zip(tile.left..tile.right) {
                    let (out, inn) = (u32::from(out), u32::from(inn).min(u32::from(out)));
                    if out == 0 || (inside == Inside::Keep && inn == FULL) || !shown.has(x, y) {
                        continue;
                    }
                    let colour = mix3(
                        [paint.fill, paint.border, paint.ground],
                        [inn, out.saturating_sub(inn), FULL.saturating_sub(out)],
                    );
                    pixel(target, x, y, colour)?;
                }
            }
        }
        Ok(())
    }
}

/// Fills the polygon through `points` (in 1/256 pixel, y down; any number
/// of contours, each `None` ending one) with `colour` blended over `ground`
/// by its coverage, only inside `clip`; pixels it does not touch are left
/// as they are. Contours that overlap cover their overlap once.
pub(crate) fn fill_polygon<D>(
    target: &mut D,
    clip: Rect,
    points: &[Option<(i64, i64)>],
    colour: D::Color,
    ground: D::Color,
    raster: &mut GlyphRasteriser,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    let mut bounds: Option<Edges> = None;
    for &(x, y) in points.iter().flatten() {
        let (left, top) = (x.div_euclid(ONE), y.div_euclid(ONE));
        let (right, bottom) = (ceil(x), ceil(y));
        let here = Edges {
            left,
            top,
            right,
            bottom,
        };
        bounds = Some(bounds.map_or(here, |seen| seen.union(here)));
    }
    let Some(bounds) = bounds else {
        return Ok(());
    };
    let Some(visible) = bounds.intersection(Edges::of(clip)) else {
        return Ok(());
    };
    for tile in tiles(visible, bounds) {
        let origin = (tile.left.saturating_mul(ONE), tile.top.saturating_mul(ONE));
        let mut path = [Command::Close; 32];
        let mut used = 0_usize;
        let mut starting = true;
        for point in points {
            let command = match *point {
                Some((x, y)) => {
                    let at = point_at(x.saturating_sub(origin.0), y.saturating_sub(origin.1));
                    if core::mem::replace(&mut starting, false) {
                        Command::MoveTo(at)
                    } else {
                        Command::LineTo(at)
                    }
                }
                None => {
                    starting = true;
                    Command::Close
                }
            };
            if let Some(slot) = path.get_mut(used) {
                *slot = command;
                used = used.saturating_add(1);
            }
        }
        let mut coverage = [[0_u8; TILE_WIDTH]; TILE_HEIGHT];
        cover(raster, path.get(..used).unwrap_or(&[]), tile, &mut coverage);
        for (row, y) in coverage.iter().zip(tile.top..tile.bottom) {
            for (&c, x) in row.iter().zip(tile.left..tile.right) {
                if c > 0 && visible.has(x, y) {
                    pixel(target, x, y, blend(colour, ground, c))?;
                }
            }
        }
    }
    Ok(())
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

    /// Smaller by `by` each side.
    fn inset(self, by: i64) -> Self {
        Self {
            left: self.left.saturating_add(by),
            top: self.top.saturating_add(by),
            right: self.right.saturating_sub(by),
            bottom: self.bottom.saturating_sub(by),
        }
    }

    /// Whether pixel (`x`, `y`) lies inside.
    fn has(self, x: i64, y: i64) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// Whether `other` lies wholly inside.
    fn holds(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && other.right <= self.right
            && other.bottom <= self.bottom
            && other.left < other.right
            && other.top < other.bottom
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let overlap = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (overlap.left < overlap.right && overlap.top < overlap.bottom).then_some(overlap)
    }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// The tiles of a 64 x 16 grid from `bounds`' corner - the shape's own -
/// that meet `visible`, each cut to `bounds` but never to the area being
/// painted: a pixel's coverage is worked out the same way whatever is
/// repainted, and wherever the shape stands, so a shape moved by whole
/// pixels - scrolled - has exactly the pixels drawn there afresh.
fn tiles(visible: Edges, bounds: Edges) -> impl Iterator<Item = Edges> {
    let (width, height) = (TILE_WIDTH as i64, TILE_HEIGHT as i64);
    let grid = |from: i64, at: i64, step: i64| {
        from.saturating_add(
            at.saturating_sub(from)
                .div_euclid(step)
                .saturating_mul(step),
        )
    };
    let first_top = grid(bounds.top, visible.top, height);
    let first_left = grid(bounds.left, visible.left, width);
    (first_top..visible.bottom)
        .step_by(TILE_HEIGHT)
        .flat_map(move |top| {
            (first_left..visible.right)
                .step_by(TILE_WIDTH)
                .filter_map(move |left| {
                    Edges {
                        left,
                        top,
                        right: left.saturating_add(width),
                        bottom: top.saturating_add(height),
                    }
                    .intersection(bounds)
                })
        })
}

/// A point of a path, from 1/256 pixel right of and below a tile's corner,
/// as the rasteriser takes it: y up, held within `i32`.
fn point_at(x: i64, y_down: i64) -> nowayos_raster::Point {
    let clamp = |value: i64| i32::try_from(value.clamp(-(1 << 30), 1 << 30)).unwrap_or(0);
    nowayos_raster::Point::new(clamp(x), clamp(y_down.saturating_neg()))
}

/// `value` 1/256 pixels in whole pixels, rounded up.
fn ceil(value: i64) -> i64 {
    value
        .div_euclid(ONE)
        .saturating_add(i64::from(value.rem_euclid(ONE) != 0))
}

/// The outline of the rectangle `edges` with corners of `radii` pixels
/// (top left, top right, bottom right, bottom left), each a cubic quarter
/// circle, from 1/256 pixel right of and below `tile`'s corner.
fn rounded_path(edges: Edges, radii: [i64; 4], tile: Edges) -> [Command; 10] {
    // In 1/256 pixel from the tile's corner, so control points need no
    // rounding to whole pixels.
    let fine = |x: i64, y: i64| point_at(x, y);
    let (left, top, right, bottom) = (
        edges.left.saturating_sub(tile.left).saturating_mul(ONE),
        edges.top.saturating_sub(tile.top).saturating_mul(ONE),
        edges.right.saturating_sub(tile.left).saturating_mul(ONE),
        edges.bottom.saturating_sub(tile.top).saturating_mul(ONE),
    );
    let [tl, tr, br, bl] = radii.map(|radius| radius.saturating_mul(ONE));
    let pull = |radius: i64| radius.saturating_mul(KAPPA) / 10_000;
    [
        Command::MoveTo(fine(left.saturating_add(tl), top)),
        Command::LineTo(fine(right.saturating_sub(tr), top)),
        Command::CurveTo(
            fine(right.saturating_sub(tr).saturating_add(pull(tr)), top),
            fine(right, top.saturating_add(tr).saturating_sub(pull(tr))),
            fine(right, top.saturating_add(tr)),
        ),
        Command::LineTo(fine(right, bottom.saturating_sub(br))),
        Command::CurveTo(
            fine(right, bottom.saturating_sub(br).saturating_add(pull(br))),
            fine(right.saturating_sub(br).saturating_add(pull(br)), bottom),
            fine(right.saturating_sub(br), bottom),
        ),
        Command::LineTo(fine(left.saturating_add(bl), bottom)),
        Command::CurveTo(
            fine(left.saturating_add(bl).saturating_sub(pull(bl)), bottom),
            fine(left, bottom.saturating_sub(bl).saturating_add(pull(bl))),
            fine(left, bottom.saturating_sub(bl)),
        ),
        Command::LineTo(fine(left, top.saturating_add(tl))),
        Command::CurveTo(
            fine(left, top.saturating_add(tl).saturating_sub(pull(tl))),
            fine(left.saturating_add(tl).saturating_sub(pull(tl)), top),
            fine(left.saturating_add(tl), top),
        ),
        Command::Close,
    ]
}

/// Each pixel of `tile`'s coverage of `path` (from 1/256 pixel right of
/// and below the tile's corner), row by row, into `out`; 0 where the
/// rasteriser refuses the path.
fn cover(
    raster: &mut GlyphRasteriser,
    path: &[Command],
    tile: Edges,
    out: &mut [[u8; TILE_WIDTH]; TILE_HEIGHT],
) {
    let width = u32::try_from(tile.right.saturating_sub(tile.left)).unwrap_or(0);
    let height = u32::try_from(tile.bottom.saturating_sub(tile.top)).unwrap_or(0);
    let mut flat = [0_u8; TILE_WIDTH * TILE_HEIGHT];
    let (Some(scale), Ok(w)) = (Scale::new(64, 256), usize::try_from(width)) else {
        return;
    };
    let placement = Placement::new(0, 0, width, height);
    if raster
        .render(path.iter().copied(), scale, placement, &mut flat)
        .is_err()
    {
        return;
    }
    for (row, line) in out.iter_mut().zip(flat.chunks(w.max(1))) {
        if let Some(target) = row.get_mut(..line.len()) {
            target.copy_from_slice(line);
        }
    }
}

/// Fills `tile` with `colour`.
fn fill<D>(target: &mut D, tile: Edges, colour: D::Color) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
        i32::try_from(tile.left),
        i32::try_from(tile.top),
        u32::try_from(tile.right.saturating_sub(tile.left)),
        u32::try_from(tile.bottom.saturating_sub(tile.top)),
    ) else {
        return Ok(());
    };
    target.fill_solid(
        &Rectangle::new(Point::new(x, y), Size::new(width, height)),
        colour,
    )
}

/// Three colours weighted by `weights`, which add up to 255, per channel in
/// the colour type's own depth, rounded to the nearest whole value.
fn mix3<C>(colours: [C; 3], weights: [u32; 3]) -> C
where
    C: RgbColor + From<Rgb888>,
{
    for (colour, &weight) in colours.iter().zip(&weights) {
        if weight == FULL {
            return *colour;
        }
    }
    let channel = |value: fn(&C) -> u8| -> u8 {
        let sum = colours
            .iter()
            .zip(&weights)
            .map(|(colour, &weight)| u32::from(value(colour)).saturating_mul(weight))
            .fold(FULL / 2, u32::saturating_add);
        u8::try_from(sum / FULL).unwrap_or(u8::MAX)
    };
    C::from(Rgb888::new(
        widen(channel(C::r), C::MAX_R),
        widen(channel(C::g), C::MAX_G),
        widen(channel(C::b), C::MAX_B),
    ))
}

/// Sets one pixel.
fn pixel<D>(target: &mut D, x: i64, y: i64, colour: D::Color) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
        return Ok(());
    };
    target.draw_iter(core::iter::once(Pixel(Point::new(x, y), colour)))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use embedded_graphics::pixelcolor::Rgb565;
    use nowayos_testkit::Framebuffer;
    use proptest::prelude::*;

    const PAINT: Paint<Rgb888> = Paint {
        fill: Rgb888::new(0xFF, 0xFF, 0xFF),
        border: Rgb888::new(0x1F, 0x5F, 0xBF),
        ground: Rgb888::new(0xF6, 0xF4, 0xEF),
    };

    fn drawn(shape: Rounded, clip: Rect) -> Framebuffer {
        drawn_inside(shape, clip, Inside::Fill)
    }

    fn drawn_inside(shape: Rounded, clip: Rect, inside: Inside) -> Framebuffer {
        let mut fb = Framebuffer::filled(64, 48, Rgb888::BLACK).unwrap();
        let mut raster: Box<GlyphRasteriser> = Box::default();
        shape
            .draw_inside(&mut fb, clip, PAINT, inside, &mut raster)
            .unwrap();
        fb
    }

    #[test]
    fn a_corner_is_left_outside_fill_inside_and_blended_between() {
        let shape = Rounded {
            rect: Rect::new(4, 4, 40, 24),
            radius: Radii::all(8),
            border: 2,
        };
        let fb = drawn(shape, Rect::new(0, 0, 64, 48));
        // Outside the shape, and the corner's outermost pixel, wholly
        // outside the circle: untouched.
        assert_eq!(fb.pixel(3, 3), Some(Rgb888::BLACK));
        assert_eq!(fb.pixel(4, 4), Some(Rgb888::BLACK));
        // The middle of the top edge: border; the middle: fill.
        assert_eq!(fb.pixel(24, 4), Some(PAINT.border));
        assert_eq!(fb.pixel(24, 5), Some(PAINT.border));
        assert_eq!(fb.pixel(24, 6), Some(PAINT.fill));
        assert_eq!(fb.pixel(24, 16), Some(PAINT.fill));
        // The left edge, below the corner: border, then fill.
        assert_eq!(fb.pixel(4, 16), Some(PAINT.border));
        assert_eq!(fb.pixel(6, 16), Some(PAINT.fill));
        // On the arc: neither untouched nor the border, but between.
        let arc = fb.pixel(6, 6).unwrap();
        assert_ne!(arc, Rgb888::BLACK);
        assert_ne!(arc, PAINT.border);
        // The four corners are mirror images, to within a level of
        // rounding.
        for (x, y) in [(5, 6), (6, 5), (7, 7), (9, 5), (4, 9)] {
            let first = fb.pixel(x, y).unwrap();
            for (mx, my) in [(47 - x, y), (x, 31 - y), (47 - x, 31 - y)] {
                let other = fb.pixel(mx, my).unwrap();
                let near = |a: u8, b: u8| a.abs_diff(b) <= 1;
                assert!(
                    near(first.r(), other.r())
                        && near(first.g(), other.g())
                        && near(first.b(), other.b()),
                    "({x}, {y}) {first:?} against ({mx}, {my}) {other:?}"
                );
            }
        }
    }

    #[test]
    fn no_border_and_no_radius_draw_a_plain_fill() {
        let shape = Rounded {
            rect: Rect::new(2, 2, 10, 10),
            radius: Radii::all(0),
            border: 0,
        };
        let fb = drawn(shape, Rect::new(0, 0, 64, 48));
        for y in 2..12 {
            for x in 2..12 {
                assert_eq!(fb.pixel(x, y), Some(PAINT.fill));
            }
        }
        assert_eq!(fb.pixel(12, 2), Some(Rgb888::BLACK));
    }

    #[test]
    fn a_circle_is_a_square_with_corners_half_its_side() {
        let side = 40;
        let shape = Rounded {
            rect: Rect::new(4, 4, side, side),
            radius: Radii::all(side / 2),
            border: 0,
        };
        let fb = drawn(shape, Rect::new(0, 0, 64, 48));
        // Its area is a circle's: pi r squared, to within a pixel or two.
        let ink: f64 = (0..48)
            .flat_map(|y| (0..64).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = fb.pixel(x, y).unwrap();
                if p == Rgb888::BLACK {
                    0.0
                } else {
                    // Fill over ground, never touched outside: the share
                    // of fill in the red channel, 0xF6 to 0xFF.
                    f64::from(p.r() - 0xF6) / 9.0
                }
            })
            .sum();
        let circle = core::f64::consts::PI * 20.0 * 20.0;
        assert!((ink - circle).abs() < 40.0, "{ink} against {circle}");
        assert_eq!(fb.pixel(24, 24), Some(PAINT.fill));
    }

    #[test]
    fn a_ring_can_keep_what_lies_inside() {
        let shape = Rounded {
            rect: Rect::new(4, 4, 40, 24),
            radius: Radii::all(6),
            border: 2,
        };
        let fb = drawn_inside(shape, Rect::new(0, 0, 64, 48), Inside::Keep);
        assert_eq!(fb.pixel(24, 16), Some(Rgb888::BLACK), "inside, untouched");
        assert_eq!(fb.pixel(24, 4), Some(PAINT.border));
    }

    #[test]
    fn mixing_keeps_whole_colours_and_works_in_the_colour_types_depth() {
        let [a, b, c] = [
            Rgb565::new(31, 0, 0),
            Rgb565::new(0, 63, 0),
            Rgb565::new(0, 0, 31),
        ];
        assert_eq!(mix3([a, b, c], [255, 0, 0]), a);
        assert_eq!(mix3([a, b, c], [0, 0, 255]), c);
        // 85 of each: a third of each channel, rounded - 10 of 31 and 21
        // of 63.
        assert_eq!(mix3([a, b, c], [85, 85, 85]), Rgb565::new(10, 21, 10));
    }

    /// A tick's two strokes, which overlap at the corner, cover it once and
    /// leave no notch.
    #[test]
    fn a_polygon_of_overlapping_contours_covers_their_overlap_once() {
        let mut fb = Framebuffer::filled(64, 48, Rgb888::BLACK).unwrap();
        let mut raster: Box<GlyphRasteriser> = Box::default();
        let px = |x: i64, y: i64| Some((x * 256, y * 256));
        let points = [
            px(4, 4),
            px(20, 4),
            px(20, 20),
            px(4, 20),
            None,
            px(12, 12),
            px(28, 12),
            px(28, 28),
            px(12, 28),
            None,
        ];
        fill_polygon(
            &mut fb,
            Rect::new(0, 0, 64, 48),
            &points,
            PAINT.border,
            PAINT.ground,
            &mut raster,
        )
        .unwrap();
        assert_eq!(fb.pixel(16, 16), Some(PAINT.border), "the overlap");
        assert_eq!(fb.pixel(6, 6), Some(PAINT.border));
        assert_eq!(fb.pixel(26, 26), Some(PAINT.border));
        assert_eq!(fb.pixel(30, 30), Some(Rgb888::BLACK));
    }

    proptest! {
        /// Any part of a shape drawn alone gives exactly what a full draw
        /// gives there, and nothing outside the part: so a dirty repaint
        /// equals a full one.
        #[test]
        fn a_clipped_draw_equals_the_full_draw_inside_the_clip(
            x in 0_i32..20, y in 0_i32..16, w in 1_u32..44, h in 1_u32..32,
            radii in proptest::array::uniform4(0_u32..30), border in 0_u32..6,
            cx in 0_i32..64, cy in 0_i32..48, cw in 1_u32..64, ch in 1_u32..48,
        ) {
            let shape = Rounded { rect: Rect::new(x, y, w, h), radius: Radii(radii), border };
            let full = drawn(shape, Rect::new(0, 0, 64, 48));
            let clip = Rect::new(cx, cy, cw, ch);
            let part = drawn(shape, clip);
            for py in 0..48_u32 {
                for px in 0..64_u32 {
                    let point = nowayos_display::Point::new(px as i32, py as i32);
                    let expected = if clip.contains(point) {
                        full.pixel(px, py)
                    } else {
                        Some(Rgb888::BLACK)
                    };
                    prop_assert_eq!(part.pixel(px, py), expected, "{} {}", px, py);
                }
            }
        }

        /// A shape never draws outside its rectangle, at any place or size
        /// - far out at layout's extremes included - and never panics.
        #[test]
        fn a_shape_stays_inside_its_rectangle(
            x in -(1_i32 << 24)..(1 << 24), y in -(1_i32 << 24)..(1 << 24),
            w in 0_u32..80, h in 0_u32..60,
            radii in proptest::array::uniform4(any::<u32>()), border in any::<u32>(),
        ) {
            let rect = Rect::new(x.clamp(-40, 60), y.clamp(-30, 40), w, h);
            let shape = Rounded { rect, radius: Radii(radii), border };
            let fb = drawn(shape, Rect::new(0, 0, 64, 48));
            for py in 0..48_u32 {
                for px in 0..64_u32 {
                    if !rect.contains(nowayos_display::Point::new(px as i32, py as i32)) {
                        prop_assert_eq!(fb.pixel(px, py), Some(Rgb888::BLACK));
                    }
                }
            }
            // Far out, it draws nothing on the screen and does not panic.
            let far = Rounded { rect: Rect::new(x, y, w, h), radius: Radii(radii), border };
            let _ = drawn(far, Rect::new(0, 0, 64, 48));
        }
    }
}
