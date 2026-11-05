// SPDX-License-Identifier: MIT OR Apache-2.0
//! Composing in bounded tiles (#10079, ADR-0027): an area holding a shadow
//! or a see-through node is drawn tile by tile into a buffer the renderer
//! may read back, then copied to the target. Everything else is drawn
//! straight to the target, as before.
//!
//! A tile is at most [`TILE_WIDTH`] x [`TILE_HEIGHT`] pixels; a see-through
//! group keeps one copy of it, at most [`GROUPS`] deep. Nothing grows with
//! the screen.

use core::convert::Infallible;

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::{OriginDimensions, Point as EgPoint, Size};
use nowayos_display::Rect;

use crate::text::blend;

/// A tile's most width, in pixels.
pub const TILE_WIDTH: u32 = 128;

/// A tile's most height, in pixels.
pub const TILE_HEIGHT: u32 = 64;

/// Pixels in a tile.
const CELLS: usize = (TILE_WIDTH * TILE_HEIGHT) as usize;

/// See-through groups inside one another a tile keeps a copy for; one
/// deeper draws at its own opacity times the group around it.
pub const GROUPS: usize = 2;

/// What the renderer draws an area onto: a target, and what it can do
/// beyond drawing - shade what is drawn for a shadow, keep it for a
/// see-through group and blend the group over it. A target drawn straight
/// does none of these; a [`Tile`] does all.
pub(crate) trait Canvas: DrawTarget {
    /// Darkens what is drawn toward `colour` by `alpha(x, y)` out of 255
    /// at each pixel of `area`.
    fn shade(&mut self, area: Rect, colour: Self::Color, alpha: impl Fn(i64, i64) -> u8);

    /// Starts a see-through group; `false` if it cannot.
    fn begin(&mut self) -> bool;

    /// Ends the group begun last, blending what it drew in `area` at
    /// `opacity` out of 255.
    fn end(&mut self, area: Rect, opacity: u8);
}

/// A target drawn straight: an area with no effect in it.
pub(crate) struct Straight<'t, D>(pub &'t mut D);

impl<D: DrawTarget> embedded_graphics::geometry::Dimensions for Straight<'_, D> {
    fn bounding_box(&self) -> embedded_graphics::primitives::Rectangle {
        self.0.bounding_box()
    }
}

impl<D: DrawTarget> DrawTarget for Straight<'_, D> {
    type Color = D::Color;
    type Error = D::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.0.draw_iter(pixels)
    }

    fn fill_contiguous<I>(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        colors: I,
    ) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.0.fill_contiguous(area, colors)
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        self.0.fill_solid(area, color)
    }
}

impl<D: DrawTarget> Canvas for Straight<'_, D> {
    fn shade(&mut self, _: Rect, _: Self::Color, _: impl Fn(i64, i64) -> u8) {}

    fn begin(&mut self) -> bool {
        false
    }

    fn end(&mut self, _: Rect, _: u8) {}
}

impl<C: RgbColor + From<Rgb888>> Canvas for Tile<C> {
    fn shade(&mut self, area: Rect, colour: C, alpha: impl Fn(i64, i64) -> u8) {
        Tile::shade(self, area, colour, alpha);
    }

    fn begin(&mut self) -> bool {
        Tile::begin(self)
    }

    fn end(&mut self, area: Rect, opacity: u8) {
        Tile::end(self, area, opacity);
    }
}

/// A tile of the screen, drawn into and read back: a [`DrawTarget`] at the
/// screen's coordinates, keeping only what falls inside it.
pub(crate) struct Tile<C> {
    area: Rect,
    pixels: [C; CELLS],
    saved: [[C; CELLS]; GROUPS],
    depth: usize,
}

impl<C: RgbColor + From<Rgb888>> Tile<C> {
    /// A tile over `area` - at most a tile's size - filled with `ground`.
    pub(crate) fn new(area: Rect, ground: C) -> Self {
        Self {
            area,
            pixels: [ground; CELLS],
            saved: [[ground; CELLS]; GROUPS],
            depth: 0,
        }
    }

    /// The index of the pixel at `(x, y)` on the screen, if inside.
    fn index(&self, x: i64, y: i64) -> Option<usize> {
        let dx = usize::try_from(x.checked_sub(self.area.left())?).ok()?;
        let dy = usize::try_from(y.checked_sub(self.area.top())?).ok()?;
        let width = usize::try_from(self.area.size.width).ok()?;
        let height = usize::try_from(self.area.size.height).ok()?;
        (dx < width && dy < height)
            .then(|| dy.checked_mul(width)?.checked_add(dx))
            .flatten()
    }

    /// Its pixels, row by row, for copying to the target.
    pub(crate) fn pixels(&self) -> impl Iterator<Item = C> + '_ {
        let len = usize::try_from(self.area.size.width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(self.area.size.height).unwrap_or(0));
        self.pixels.iter().copied().take(len)
    }

    /// Darkens what is drawn toward `colour` by `alpha(x, y)` out of 255,
    /// at each pixel of `area`: a shadow.
    pub(crate) fn shade(&mut self, area: Rect, colour: C, alpha: impl Fn(i64, i64) -> u8) {
        let Some(part) = area.intersection(&self.area) else {
            return;
        };
        for y in part.top()..part.bottom() {
            for x in part.left()..part.right() {
                let amount = alpha(x, y);
                if amount == 0 {
                    continue;
                }
                if let Some(cell) = self.index(x, y).and_then(|at| self.pixels.get_mut(at)) {
                    *cell = blend(colour, *cell, amount);
                }
            }
        }
    }

    /// Starts a see-through group: what is drawn so far is kept. `false`
    /// past [`GROUPS`] deep, when the group is drawn into the one around it.
    pub(crate) fn begin(&mut self) -> bool {
        let Some(copy) = self.saved.get_mut(self.depth) else {
            return false;
        };
        *copy = self.pixels;
        self.depth = self.depth.saturating_add(1);
        true
    }

    /// Ends the group begun last: inside `area`, what it drew is blended
    /// over what was kept at `opacity` out of 255.
    pub(crate) fn end(&mut self, area: Rect, opacity: u8) {
        let Some(depth) = self.depth.checked_sub(1) else {
            return;
        };
        self.depth = depth;
        let Some(part) = area.intersection(&self.area) else {
            return;
        };
        for y in part.top()..part.bottom() {
            for x in part.left()..part.right() {
                let Some(at) = self.index(x, y) else {
                    continue;
                };
                let (Some(kept), Some(cell)) = (
                    self.saved.get(depth).and_then(|copy| copy.get(at)).copied(),
                    self.pixels.get_mut(at),
                ) else {
                    continue;
                };
                *cell = blend(*cell, kept, opacity);
            }
        }
    }
}

impl<C: RgbColor + From<Rgb888>> DrawTarget for Tile<C> {
    type Color = C;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(at, colour) in pixels {
            if let Some(cell) = self
                .index(i64::from(at.x), i64::from(at.y))
                .and_then(|index| self.pixels.get_mut(index))
            {
                *cell = colour;
            }
        }
        Ok(())
    }
}

impl<C> OriginDimensions for Tile<C> {
    /// Its size from the screen's origin: drawing is placed at the
    /// screen's coordinates, and what falls outside the tile is dropped.
    fn size(&self) -> Size {
        let right = u32::try_from(self.area.right()).unwrap_or(0);
        let bottom = u32::try_from(self.area.bottom()).unwrap_or(0);
        Size::new(right, bottom)
    }
}

/// How much of a shadow falls at a point `distance` 1/16 pixel outside the
/// shape casting it, for a blur `blur` pixels wide, out of `alpha`: all of
/// it inside, none past the blur, and between them a smooth fall - the
/// smoothstep of the way across, close to a Gaussian's.
pub(crate) fn falloff(distance: i64, blur: u32, alpha: u8) -> u8 {
    if distance <= 0 {
        return alpha;
    }
    let width = i64::from(blur.max(1)).saturating_mul(16);
    if distance >= width {
        return 0;
    }
    // t from 1 at the edge to 0 at the blur's end, in 1/4096.
    let t = (width.saturating_sub(distance))
        .saturating_mul(4096)
        .checked_div(width)
        .unwrap_or(0);
    let smooth = t
        .saturating_mul(t)
        .saturating_mul((3 * 4096_i64).saturating_sub(t.saturating_mul(2)))
        / (4096 * 4096);
    u8::try_from(i64::from(alpha).saturating_mul(smooth) / 4096).unwrap_or(alpha)
}

/// How far `(x, y)`'s centre lies outside the rounded rectangle `shape`
/// with corner radius `radius`, in 1/16 pixel; 0 or less inside.
pub(crate) fn outside(shape: Rect, radius: u32, x: i64, y: i64) -> i64 {
    let (px, py) = (
        x.saturating_mul(16).saturating_add(8),
        y.saturating_mul(16).saturating_add(8),
    );
    let r = i64::from(radius).saturating_mul(16);
    let (left, top) = (
        shape.left().saturating_mul(16),
        shape.top().saturating_mul(16),
    );
    let (right, bottom) = (
        shape.right().saturating_mul(16),
        shape.bottom().saturating_mul(16),
    );
    // Distance from the rectangle shrunk by the radius, less the radius.
    let dx = (left.saturating_add(r).saturating_sub(px))
        .max(px.saturating_sub(right.saturating_sub(r)))
        .max(0);
    let dy = (top.saturating_add(r).saturating_sub(py))
        .max(py.saturating_sub(bottom.saturating_sub(r)))
        .max(0);
    let length = isqrt(dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)));
    length.saturating_sub(r)
}

/// The whole square root of `n`, rounded down; 0 for none.
fn isqrt(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.saturating_add(1) / 2;
    while y < x {
        x = y;
        y = x.saturating_add(n.checked_div(x).unwrap_or(0)) / 2;
    }
    x
}

/// A 4 x 4 ordered dither, in 1/16 of a level: added before a gradient's
/// colour is rounded, so no band shows between levels.
const BAYER: [[i64; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// The colour of a gradient from `from` to `to` at `t` out of 4096, at the
/// pixel `(x, y)`, dithered.
pub(crate) fn gradient_colour<C: RgbColor + From<Rgb888>>(
    from: C,
    to: C,
    t: i64,
    x: i64,
    y: i64,
) -> C {
    let t = t.clamp(0, 4096);
    let dither = BAYER
        .get(usize::try_from(y.rem_euclid(4)).unwrap_or(0))
        .and_then(|row| row.get(usize::try_from(x.rem_euclid(4)).unwrap_or(0)))
        .copied()
        .unwrap_or(0);
    let channel = |a: u8, b: u8, max: u8| {
        let (a, b) = (
            i64::from(crate::text::widen(a, max)),
            i64::from(crate::text::widen(b, max)),
        );
        // In 1/16 of a level, then dithered and rounded down.
        let level = a
            .saturating_mul(16)
            .saturating_add((b.saturating_sub(a)).saturating_mul(t).saturating_mul(16) / 4096)
            .saturating_add(dither);
        u8::try_from((level / 16).clamp(0, 255)).unwrap_or(0)
    };
    C::from(Rgb888::new(
        channel(from.r(), to.r(), C::MAX_R),
        channel(from.g(), to.g(), C::MAX_G),
        channel(from.b(), to.b(), C::MAX_B),
    ))
}

/// Where `(x, y)` lies along a gradient over `area`, out of 4096: along a
/// line at `angle` degrees (180, top to bottom), or out from the centre to
/// the farthest corner if `angle` is `None`.
pub(crate) fn gradient_at(area: Rect, angle: Option<u16>, x: i64, y: i64) -> i64 {
    let (w, h) = (i64::from(area.size.width), i64::from(area.size.height));
    let (cx, cy) = (
        area.left().saturating_mul(2).saturating_add(w),
        area.top().saturating_mul(2).saturating_add(h),
    );
    // Twice the offset from the centre, so halves stay whole.
    let (px, py) = (
        x.saturating_mul(2).saturating_add(1).saturating_sub(cx),
        y.saturating_mul(2).saturating_add(1).saturating_sub(cy),
    );
    match angle {
        None => {
            let reach = isqrt(w.saturating_mul(w).saturating_add(h.saturating_mul(h)));
            let at = isqrt(px.saturating_mul(px).saturating_add(py.saturating_mul(py)));
            at.saturating_mul(4096).checked_div(reach).unwrap_or(0)
        }
        Some(angle) => {
            // The direction, in 1/4096: CSS's 0 is up, 90 right.
            let (sin, cos) = sin_cos(angle);
            let along = px
                .saturating_mul(sin)
                .saturating_sub(py.saturating_mul(cos))
                / 4096;
            // The line's half length, as CSS reaches the corners.
            let half = (w
                .saturating_mul(sin.abs())
                .saturating_add(h.saturating_mul(cos.abs())))
                / 4096;
            along
                .saturating_add(half)
                .saturating_mul(4096)
                .checked_div(half.saturating_mul(2))
                .unwrap_or(0)
        }
    }
}

/// The sine and cosine of `degrees`, in 1/4096: from a table every 15
/// degrees, between them linearly.
fn sin_cos(degrees: u16) -> (i64, i64) {
    const SIN: [i64; 7] = [0, 1060, 2048, 2896, 3547, 3956, 4096];
    let sin_of = |d: i64| -> i64 {
        let d = d.rem_euclid(360);
        let (quarter, within) = (d / 90, d % 90);
        let first = |w: i64| {
            let at = usize::try_from(w / 15).unwrap_or(0);
            let (a, b) = (
                SIN.get(at).copied().unwrap_or(0),
                SIN.get(at.saturating_add(1)).copied().unwrap_or(4096),
            );
            a.saturating_add((b.saturating_sub(a)).saturating_mul(w % 15) / 15)
        };
        match quarter {
            0 => first(within),
            1 => first(90_i64.saturating_sub(within)),
            2 => first(within).saturating_neg(),
            _ => first(90_i64.saturating_sub(within)).saturating_neg(),
        }
    };
    let d = i64::from(degrees);
    (sin_of(d), sin_of(d.saturating_add(90)))
}

/// The tiles covering `area`, row by row.
pub(crate) fn tiles(area: Rect) -> impl Iterator<Item = Rect> {
    let (width, height) = (i64::from(TILE_WIDTH), i64::from(TILE_HEIGHT));
    let rows = (area.top()..area.bottom()).step_by(TILE_HEIGHT as usize);
    rows.flat_map(move |top| {
        (area.left()..area.right())
            .step_by(TILE_WIDTH as usize)
            .map(move |left| {
                Rect::from_edges(
                    left,
                    top,
                    left.saturating_add(width).min(area.right()),
                    top.saturating_add(height).min(area.bottom()),
                )
            })
    })
}

/// `point` as `embedded-graphics` places it, if it fits.
#[allow(dead_code, reason = "kept beside the tile for its tests")]
pub(crate) fn eg(x: i64, y: i64) -> Option<EgPoint> {
    Some(EgPoint::new(i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failure fails the test")]
mod tests {
    use embedded_graphics::pixelcolor::Rgb888;

    use super::*;

    #[test]
    fn a_shadow_falls_smoothly_from_full_to_none() {
        assert_eq!(falloff(0, 8, 100), 100);
        assert_eq!(falloff(8 * 16, 8, 100), 0);
        let half = falloff(4 * 16, 8, 100);
        assert!((40..=60).contains(&half), "{half}");
        assert!(falloff(16, 8, 100) > falloff(32, 8, 100));
    }

    #[test]
    fn a_rounded_shape_s_outside_is_measured_from_its_corners() {
        let shape = Rect::new(10, 10, 40, 30);
        assert!(outside(shape, 6, 20, 20) <= 0);
        assert_eq!(outside(shape, 0, 52, 20), 2 * 16 + 8);
        // Past a rounded corner, farther than past the square one.
        assert!(outside(shape, 8, 50, 40) > outside(shape, 0, 50, 40));
    }

    #[test]
    fn a_gradient_runs_its_whole_way_and_dithers_between_levels() {
        let area = Rect::new(0, 0, 100, 10);
        assert!(gradient_at(area, Some(90), 0, 5) < 100);
        assert!(gradient_at(area, Some(90), 99, 5) > 4000);
        assert!(gradient_at(area, Some(180), 50, 0) < 500);
        assert!(gradient_at(area, None, 50, 5) < 100);
        let (a, b) = (Rgb888::new(0, 0, 0), Rgb888::new(1, 1, 1));
        // Half way between two levels: half the pixels of a 4 x 4 each.
        let ones = (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .filter(|&(x, y)| gradient_colour(a, b, 2048, x, y) == b)
            .count();
        assert_eq!(ones, 8);
    }

    #[test]
    fn a_group_blends_what_it_drew_over_what_was_kept() {
        let mut tile = Tile::new(Rect::new(0, 0, 4, 4), Rgb888::new(0, 0, 0));
        assert!(tile.begin());
        tile.draw_iter([Pixel(EgPoint::new(1, 1), Rgb888::new(200, 200, 200))])
            .unwrap();
        tile.end(Rect::new(0, 0, 4, 4), 128);
        assert_eq!(tile.pixels().nth(5), Some(Rgb888::new(100, 100, 100)));
    }
}
