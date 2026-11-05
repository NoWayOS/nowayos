// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mouse pointer: an arrow, black outline and white fill, so it
//! shows on any ground, drawn over the screen by the UI loop and never part
//! of a tree - so never in a golden image.
//!
//! The arrow is a mask in layout units, its tip at the top left; at a
//! device scale each unit's edges fall on the nearest whole pixels, as the
//! theme's sizes do: at scale 2 each unit is 2 x 2 pixels, at 1.5 the units
//! are 1 and 2 pixels in turn.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use nowayos_display::{DeviceScale, Point, Rect};

use crate::to_eg;

/// The arrow's width in layout units.
pub const POINTER_WIDTH: u32 = 12;

/// The arrow's height in layout units.
pub const POINTER_HEIGHT: u32 = 19;

/// The arrow, a row at a time: `B` outline, `W` fill, `.` left as it is.
/// Drawn for NoWayOS; the classic shape, tip at the top left.
const ARROW: [&[u8; POINTER_WIDTH as usize]; POINTER_HEIGHT as usize] = [
    b"B...........",
    b"BB..........",
    b"BWB.........",
    b"BWWB........",
    b"BWWWB.......",
    b"BWWWWB......",
    b"BWWWWWB.....",
    b"BWWWWWWB....",
    b"BWWWWWWWB...",
    b"BWWWWWWWWB..",
    b"BWWWWWWWWWB.",
    b"BWWWWWWBBBBB",
    b"BWWWBWWB....",
    b"BWWBBWWB....",
    b"BWB..BWWB...",
    b"BB...BWWB...",
    b"B.....BWWB..",
    b"......BWWB..",
    b".......BB...",
];

/// The pixels the pointer covers with its tip at `at`, at device scale
/// `scale`. Saturating: near `i32::MAX` it is cut short, never wrapped.
#[must_use]
pub fn pointer_area(at: Point, scale: DeviceScale) -> Rect {
    Rect::from_edges(
        i64::from(at.x),
        i64::from(at.y),
        i64::from(at.x).saturating_add(i64::from(scale.px(POINTER_WIDTH))),
        i64::from(at.y).saturating_add(i64::from(scale.px(POINTER_HEIGHT))),
    )
}

/// Draws the pointer with its tip at `at`, at device scale `scale`,
/// touching only pixels inside `clip`: a run of one colour in a row of the
/// mask is one solid fill.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_pointer<D>(
    target: &mut D,
    at: Point,
    scale: DeviceScale,
    clip: Rect,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: From<Rgb888>,
{
    if !pointer_area(at, scale).intersects(&clip) {
        return Ok(());
    }
    // Unit `n`'s edge, `n` units from the tip, in pixels.
    let edge = |from: i32, units: i64| i64::from(from).saturating_add(scale.px_signed(units));
    for (row, units) in ARROW.iter().zip(0_i64..) {
        let (top, bottom) = (edge(at.y, units), edge(at.y, units.saturating_add(1)));
        let mut column: i64 = 0;
        for run in row.chunk_by(|a, b| a == b) {
            let width = i64::try_from(run.len()).unwrap_or(0);
            let color = match run.first() {
                Some(b'B') => Some(Rgb888::BLACK),
                Some(b'W') => Some(Rgb888::WHITE),
                _ => None,
            };
            if let Some(color) = color {
                let run = Rect::from_edges(
                    edge(at.x, column),
                    top,
                    edge(at.x, column.saturating_add(width)),
                    bottom,
                );
                if let Some(shown) = run.intersection(&clip) {
                    target.fill_solid(&to_eg(shown), color.into())?;
                }
            }
            column = column.saturating_add(width);
        }
    }
    Ok(())
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
    use nowayos_testkit::Framebuffer;
    use proptest::prelude::*;

    const GROUND: Rgb888 = Rgb888::new(0x20, 0x80, 0x40);

    fn drawn(at: Point, quarters: u32, clip: Rect) -> Framebuffer {
        let scale = DeviceScale::from_quarters(quarters).unwrap();
        let mut fb = Framebuffer::filled(64, 64, GROUND).unwrap();
        render_pointer(&mut fb, at, scale, clip).unwrap();
        fb
    }

    #[test]
    fn every_row_of_the_mask_is_outline_fill_or_clear() {
        for row in ARROW {
            assert!(row.iter().all(|c| matches!(c, b'B' | b'W' | b'.')));
        }
    }

    #[test]
    fn the_tip_is_at_the_point_and_each_unit_is_scale_pixels() {
        let fb = drawn(Point::new(10, 5), 8, Rect::new(0, 0, 64, 64));
        // The tip: outline, two pixels each way at scale 2.
        for (x, y) in [(10, 5), (11, 5), (10, 6), (11, 6)] {
            assert_eq!(fb.pixel(x, y), Some(Rgb888::BLACK));
        }
        // Right of the tip and above it: the ground, untouched.
        assert_eq!(fb.pixel(12, 5), Some(GROUND));
        assert_eq!(fb.pixel(10, 4), Some(GROUND));
        // Row 3, column 1 of the mask is fill: pixels (12, 11) to (13, 12).
        assert_eq!(fb.pixel(12, 11), Some(Rgb888::WHITE));
        assert_eq!(fb.pixel(13, 12), Some(Rgb888::WHITE));
        // The area: 24 x 38 pixels at scale 2.
        assert_eq!(
            pointer_area(Point::new(10, 5), DeviceScale::TWO),
            Rect::new(10, 5, 24, 38)
        );
    }

    #[test]
    fn nothing_outside_the_area_is_touched() {
        let at = Point::new(7, 3);
        let fb = drawn(at, 4, Rect::new(0, 0, 64, 64));
        let area = pointer_area(at, DeviceScale::ONE);
        for y in 0..64 {
            for x in 0..64 {
                if !area.contains(Point::new(x, y)) {
                    assert_eq!(fb.pixel(x as u32, y as u32), Some(GROUND));
                }
            }
        }
    }

    proptest! {
        // A clipped draw gives exactly the full draw's pixels inside the
        // clip and the ground outside it: what lets the UI loop draw the
        // pointer into each repainted area alone.
        #[test]
        fn a_clipped_draw_is_the_full_draw_inside_the_clip(
            x in -30..70_i32, y in -40..70_i32, scale in 4..=16_u32,
            cx in 0..64_i32, cy in 0..64_i32, cw in 0..64_u32, ch in 0..64_u32,
        ) {
            let at = Point::new(x, y);
            let clip = Rect::new(cx, cy, cw, ch);
            let full = drawn(at, scale, Rect::new(0, 0, 64, 64));
            let part = drawn(at, scale, clip);
            for py in 0..64 {
                for px in 0..64 {
                    let expected = if clip.contains(Point::new(px, py)) {
                        full.pixel(px as u32, py as u32)
                    } else {
                        Some(GROUND)
                    };
                    prop_assert_eq!(part.pixel(px as u32, py as u32), expected);
                }
            }
        }

        // Anywhere, at any scale: no panic, no overflow.
        #[test]
        fn any_point_and_scale_draws_without_panicking(
            x in any::<i32>(), y in any::<i32>(), scale in 4..=16_u32,
        ) {
            let _ = drawn(Point::new(x, y), scale, Rect::new(0, 0, 64, 64));
            let _ = pointer_area(Point::new(x, y), DeviceScale::from_quarters(scale).unwrap());
        }
    }
}
