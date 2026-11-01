// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rounded rectangles: a button's body, a card, a notice, a header
//! bar - filled, with a border of their own, and their corners
//! anti-aliased over the colour known to lie beneath them.
//!
//! Every pixel's colour depends only on the shape and the pixel's place,
//! never on what is already in the framebuffer or on the area being
//! painted: a corner pixel is split into 8 x 8 samples, each inside the
//! fill, inside the border or outside the shape, and its colour is the three
//! colours weighted by their counts - in the colour type's own depth, in
//! integers. So a dirty repaint of any part of a shape gives exactly the
//! pixels of a full repaint.

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use nowayos_display::Rect;

use crate::text::widen;

/// Samples each way in a corner pixel: 8 x 8 = 64 levels of coverage.
const SAMPLES: i64 = 8;

/// Every sample of a pixel.
const SAMPLE_COUNT: u32 = 64;

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

/// A rectangle with rounded corners and a border inside its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rounded {
    /// The shape's bounds.
    pub rect: Rect,
    /// The corners' radius, in pixels; at most half the shorter side.
    pub radius: u32,
    /// The border's width, in pixels, inside `rect`; 0 for none.
    pub border: u32,
}

impl Rounded {
    /// The radius and border used: each at most half the shorter side.
    fn clamped(self) -> (i64, i64) {
        let half = i64::from(self.rect.size.width.min(self.rect.size.height) / 2);
        (
            i64::from(self.radius).min(half),
            i64::from(self.border).min(half),
        )
    }

    /// Draws the shape into `target`, only inside `clip`.
    pub(crate) fn draw<D>(
        self,
        target: &mut D,
        clip: Rect,
        paint: Paint<D::Color>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget,
        D::Color: RgbColor + From<Rgb888>,
    {
        let Some(visible) = self.rect.intersection(&clip) else {
            return Ok(());
        };
        let (radius, border) = self.clamped();
        let (left, top, right, bottom) = (
            self.rect.left(),
            self.rect.top(),
            self.rect.right(),
            self.rect.bottom(),
        );
        for y in visible.top()..visible.bottom() {
            // A row in the border's top or bottom band is border colour
            // between the corners; any other row is fill colour there.
            let edge_row = y < top.saturating_add(border) || y >= bottom.saturating_sub(border);
            let corner_row = y < top.saturating_add(radius) || y >= bottom.saturating_sub(radius);
            // Columns drawn sample by sample: the corners, in a corner row.
            let (inner_left, inner_right) = if corner_row {
                (left.saturating_add(radius), right.saturating_sub(radius))
            } else {
                (left, right)
            };
            // The straight part of the row: border, fill, border.
            let middle = if edge_row { paint.border } else { paint.fill };
            let runs = [
                (
                    inner_left,
                    left.saturating_add(border).max(inner_left),
                    paint.border,
                ),
                (
                    left.saturating_add(border).max(inner_left),
                    right.saturating_sub(border).min(inner_right),
                    middle,
                ),
                (
                    right.saturating_sub(border).min(inner_right),
                    inner_right,
                    paint.border,
                ),
            ];
            for (from, to, colour) in runs {
                run(
                    target,
                    y,
                    from.max(visible.left()),
                    to.min(visible.right()),
                    colour,
                )?;
            }
            if !corner_row {
                continue;
            }
            // The corners' pixels, each from its samples.
            let centre_y = if y < top.saturating_add(radius) {
                top.saturating_add(radius)
            } else {
                bottom.saturating_sub(radius)
            };
            let corners = [
                (left, inner_left, left.saturating_add(radius)),
                (inner_right, right, right.saturating_sub(radius)),
            ];
            for (from, to, centre_x) in corners {
                for x in from.max(visible.left())..to.min(visible.right()) {
                    let [inside, edge, outside] = samples(
                        x,
                        y,
                        (centre_x, centre_y),
                        radius,
                        radius.saturating_sub(border),
                    );
                    let colour = mix3(
                        [paint.fill, paint.border, paint.ground],
                        [inside, edge, outside],
                    );
                    pixel(target, x, y, colour)?;
                }
            }
        }
        Ok(())
    }
}

/// How many of the 64 samples of pixel (`x`, `y`) lie inside the inner
/// circle of radius `inner` round `centre`, in the ring out to `outer`, and
/// beyond it. For a pixel in a corner square, which lies wholly on the
/// outer side of the corner's centre each way. An inner radius of 0 has no
/// inside in the corner: the border's inner edge is beyond it.
fn samples(x: i64, y: i64, centre: (i64, i64), outer: i64, inner: i64) -> [u32; 3] {
    // In 1/16 pixel: sample i of 8 lies at (2i + 1)/16 across the pixel.
    let scale = SAMPLES.saturating_mul(2);
    let (cx, cy) = (
        centre.0.saturating_mul(scale),
        centre.1.saturating_mul(scale),
    );
    let outer2 = outer.saturating_mul(scale).saturating_pow(2);
    let inner2 = inner.saturating_mul(scale).saturating_pow(2);
    let mut counts = [0_u32; 3];
    for j in 0..SAMPLES {
        let sy = y
            .saturating_mul(scale)
            .saturating_add(j.saturating_mul(2).saturating_add(1));
        let dy2 = sy.saturating_sub(cy).saturating_pow(2);
        for i in 0..SAMPLES {
            let sx = x
                .saturating_mul(scale)
                .saturating_add(i.saturating_mul(2).saturating_add(1));
            let d2 = sx.saturating_sub(cx).saturating_pow(2).saturating_add(dy2);
            let slot = if inner > 0 && d2 <= inner2 {
                0
            } else if d2 <= outer2 {
                1
            } else {
                2
            };
            if let Some(count) = counts.get_mut(slot) {
                *count = count.saturating_add(1);
            }
        }
    }
    counts
}

/// Three colours weighted by `weights`, which add up to 64, per channel in
/// the colour type's own depth, rounded to the nearest whole value.
fn mix3<C>(colours: [C; 3], weights: [u32; 3]) -> C
where
    C: RgbColor + From<Rgb888>,
{
    for (colour, &weight) in colours.iter().zip(&weights) {
        if weight == SAMPLE_COUNT {
            return *colour;
        }
    }
    let channel = |value: fn(&C) -> u8| -> u8 {
        let sum = colours
            .iter()
            .zip(&weights)
            .map(|(colour, &weight)| u32::from(value(colour)).saturating_mul(weight))
            .fold(SAMPLE_COUNT / 2, u32::saturating_add);
        u8::try_from(sum / SAMPLE_COUNT).unwrap_or(u8::MAX)
    };
    C::from(Rgb888::new(
        widen(channel(C::r), C::MAX_R),
        widen(channel(C::g), C::MAX_G),
        widen(channel(C::b), C::MAX_B),
    ))
}

/// Fills row `y` from `from` to `to`, if any of it.
fn run<D>(target: &mut D, y: i64, from: i64, to: i64, colour: D::Color) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let (Ok(x), Ok(y), Ok(width)) = (
        i32::try_from(from),
        i32::try_from(y),
        u32::try_from(to.saturating_sub(from)),
    ) else {
        return Ok(());
    };
    if width == 0 {
        return Ok(());
    }
    target.fill_solid(
        &Rectangle::new(Point::new(x, y), Size::new(width, 1)),
        colour,
    )
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
        let mut fb = Framebuffer::filled(64, 48, Rgb888::BLACK).unwrap();
        shape.draw(&mut fb, clip, PAINT).unwrap();
        fb
    }

    #[test]
    fn a_corner_is_ground_outside_fill_inside_and_blended_between() {
        let shape = Rounded {
            rect: Rect::new(4, 4, 40, 24),
            radius: 8,
            border: 2,
        };
        let fb = drawn(shape, Rect::new(0, 0, 64, 48));
        // Outside the shape: untouched.
        assert_eq!(fb.pixel(3, 3), Some(Rgb888::BLACK));
        // The corner's outermost pixel lies outside the circle: ground.
        assert_eq!(fb.pixel(4, 4), Some(PAINT.ground));
        // The middle of the top edge: border; the middle: fill.
        assert_eq!(fb.pixel(24, 4), Some(PAINT.border));
        assert_eq!(fb.pixel(24, 5), Some(PAINT.border));
        assert_eq!(fb.pixel(24, 6), Some(PAINT.fill));
        assert_eq!(fb.pixel(24, 16), Some(PAINT.fill));
        // The left edge, below the corner: border, then fill.
        assert_eq!(fb.pixel(4, 16), Some(PAINT.border));
        assert_eq!(fb.pixel(6, 16), Some(PAINT.fill));
        // On the arc: neither the ground nor the border, but between.
        let arc = fb.pixel(6, 6).unwrap();
        assert_ne!(arc, PAINT.ground);
        assert_ne!(arc, PAINT.border);
        // The four corners are mirror images.
        for (x, y) in [(4, 4), (5, 6), (6, 5), (7, 7), (9, 5)] {
            let mirrored = [
                fb.pixel(x, y),
                fb.pixel(47 - x, y),
                fb.pixel(x, 31 - y),
                fb.pixel(47 - x, 31 - y),
            ];
            assert!(mirrored.iter().all(|p| *p == mirrored[0]), "{x}, {y}");
        }
    }

    #[test]
    fn no_border_and_no_radius_draw_a_plain_fill() {
        let shape = Rounded {
            rect: Rect::new(2, 2, 10, 10),
            radius: 0,
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
    fn mixing_keeps_whole_colours_and_works_in_the_colour_types_depth() {
        let [a, b, c] = [
            Rgb565::new(31, 0, 0),
            Rgb565::new(0, 63, 0),
            Rgb565::new(0, 0, 31),
        ];
        assert_eq!(mix3([a, b, c], [64, 0, 0]), a);
        assert_eq!(mix3([a, b, c], [0, 0, 64]), c);
        // Half and half: 15.5 rounds to 16 of 31, 31.5 to 32 of 63.
        assert_eq!(mix3([a, b, c], [32, 32, 0]), Rgb565::new(16, 32, 0));
    }

    proptest! {
        /// Any part of a shape drawn alone gives exactly what a full draw
        /// gives there, and nothing outside the part: so a dirty repaint
        /// equals a full one.
        #[test]
        fn a_clipped_draw_equals_the_full_draw_inside_the_clip(
            x in 0_i32..20, y in 0_i32..16, w in 1_u32..44, h in 1_u32..32,
            radius in 0_u32..30, border in 0_u32..6,
            cx in 0_i32..64, cy in 0_i32..48, cw in 1_u32..64, ch in 1_u32..48,
        ) {
            let shape = Rounded { rect: Rect::new(x, y, w, h), radius, border };
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
    }
}
