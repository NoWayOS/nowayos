// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mouse pointer: an arrow, black outline and white fill, so it
//! shows on any ground, drawn over the screen by the UI loop and never part
//! of a tree - so never in a golden image. Its shape says what a press will
//! do there (#10082, [`Cursor`]): an arrow, a hand, an I-beam or
//! not-allowed.
//!
//! Each shape is a mask in layout units with a hot spot - the arrow's tip,
//! the hand's fingertip, the middle of the I-beam and of the circle - put
//! at the pointer's position; at a device scale each unit's edges fall on
//! the nearest whole pixels, as the theme's sizes do: at scale 2 each unit
//! is 2 x 2 pixels, at 1.5 the units are 1 and 2 pixels in turn. So every
//! shape is crisp at every scale.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use nowayos_display::{DeviceScale, Point, Rect};
use nowayos_ui::Cursor;

use crate::to_eg;

/// The arrow's width in layout units.
pub const POINTER_WIDTH: u32 = 12;

/// The arrow's height in layout units.
pub const POINTER_HEIGHT: u32 = 19;

/// The arrow, a row at a time: `B` outline, `W` fill, `.` left as it is.
/// Drawn for NoWayOS; the classic shape, tip at the top left.
const ARROW: [&[u8]; POINTER_HEIGHT as usize] = [
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

/// A pointing hand, its fingertip at the top. Drawn for NoWayOS.
const HAND: [&[u8]; 19] = [
    b".....BB.........",
    b"....BWWB........",
    b"....BWWB........",
    b"....BWWB........",
    b"....BWWB........",
    b"....BWWBBB......",
    b"....BWWBWWBBB...",
    b"....BWWBWWBWWBB.",
    b".BB.BWWBWWBWWBWB",
    b"BWWBBWWWWWWWWWWB",
    b"BWWWBWWWWWWWWWWB",
    b".BWWBWWWWWWWWWWB",
    b"..BWWWWWWWWWWWWB",
    b"..BWWWWWWWWWWWB.",
    b"...BWWWWWWWWWWB.",
    b"...BWWWWWWWWWB..",
    b"....BWWWWWWWWB..",
    b"....BWWWWWWWWB..",
    b"....BBBBBBBBBB..",
];

/// An I-beam, for text. Drawn for NoWayOS.
const I_BEAM: [&[u8]; 17] = [
    b"BBB.BBB", b"BWWBWWB", b"BBBWBBB", b"..BWB..", b"..BWB..", b"..BWB..", b"..BWB..", b"..BWB..",
    b"..BWB..", b"..BWB..", b"..BWB..", b"..BWB..", b"..BWB..", b"..BWB..", b"BBBWBBB", b"BWWBWWB",
    b"BBB.BBB",
];

/// A circle struck through: nothing here can be pressed. Drawn for NoWayOS.
const NOT_ALLOWED: [&[u8]; 17] = [
    b".....BBBBBBB.....",
    b"...BBBWWWWWBBB...",
    b"..BBWWWWWWWWWBB..",
    b".BBWWWBBBBBWWWBB.",
    b".BWWWWBB..BBWWWB.",
    b"BBWWWWWBB..BBWWBB",
    b"BWWBBWWWBB..BBWWB",
    b"BWWBBBWWWBB..BWWB",
    b"BWWB.BBWWWBB.BWWB",
    b"BWWB..BBWWWBBBWWB",
    b"BWWBB..BBWWWBBWWB",
    b"BBWWBB..BBWWWWWBB",
    b".BWWWBB..BBWWWWB.",
    b".BBWWWBBBBBWWWBB.",
    b"..BBWWWWWWWWWBB..",
    b"...BBBWWWWWBBB...",
    b".....BBBBBBB.....",
];

/// A shape's mask and its hot spot, in layout units from its top left.
const fn mask(cursor: Cursor) -> (&'static [&'static [u8]], u32, u32) {
    match cursor {
        Cursor::Arrow => (&ARROW, 0, 0),
        Cursor::Hand => (&HAND, 5, 0),
        Cursor::Text => (&I_BEAM, 3, 8),
        Cursor::NotAllowed => (&NOT_ALLOWED, 8, 8),
    }
}

/// The pixels `cursor` covers with its hot spot at `at`, at device scale
/// `scale`. Saturating: near `i32::MAX` it is cut short, never wrapped.
#[must_use]
pub fn pointer_area(at: Point, cursor: Cursor, scale: DeviceScale) -> Rect {
    let (rows, hot_x, hot_y) = mask(cursor);
    let width = rows.first().map_or(0, |row| row.len());
    let width = i64::try_from(width).unwrap_or(0);
    let height = i64::try_from(rows.len()).unwrap_or(0);
    // The edges the mask's first and last units are drawn at, as
    // [`render_pointer`] places them.
    let edge = |from: i32, hot: u32, units: i64| {
        i64::from(from).saturating_add(scale.px_signed(units.saturating_sub(i64::from(hot))))
    };
    Rect::from_edges(
        edge(at.x, hot_x, 0),
        edge(at.y, hot_y, 0),
        edge(at.x, hot_x, width),
        edge(at.y, hot_y, height),
    )
}

/// Draws `cursor` with its hot spot at `at`, at device scale `scale`,
/// touching only pixels inside `clip`: a run of one colour in a row of the
/// mask is one solid fill.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_pointer<D>(
    target: &mut D,
    at: Point,
    cursor: Cursor,
    scale: DeviceScale,
    clip: Rect,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: From<Rgb888>,
{
    let area = pointer_area(at, cursor, scale);
    if !area.intersects(&clip) {
        return Ok(());
    }
    let (rows, hot_x, hot_y) = mask(cursor);
    // Unit `n`'s edge, `n` units from the hot spot, in pixels.
    let edge = |from: i32, hot: u32, units: i64| {
        i64::from(from).saturating_add(scale.px_signed(units.saturating_sub(i64::from(hot))))
    };
    for (row, units) in rows.iter().zip(0_i64..) {
        let (top, bottom) = (
            edge(at.y, hot_y, units),
            edge(at.y, hot_y, units.saturating_add(1)),
        );
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
                    edge(at.x, hot_x, column),
                    top,
                    edge(at.x, hot_x, column.saturating_add(width)),
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

    fn drawn(at: Point, cursor: Cursor, quarters: u32, clip: Rect) -> Framebuffer {
        let scale = DeviceScale::from_quarters(quarters).unwrap();
        let mut fb = Framebuffer::filled(64, 64, GROUND).unwrap();
        render_pointer(&mut fb, at, cursor, scale, clip).unwrap();
        fb
    }

    #[test]
    fn every_row_of_every_mask_is_outline_fill_or_clear_and_as_wide() {
        for cursor in Cursor::ALL {
            let (rows, hot_x, hot_y) = mask(cursor);
            let width = rows[0].len();
            for row in rows {
                assert_eq!(row.len(), width, "{cursor:?}");
                assert!(row.iter().all(|c| matches!(c, b'B' | b'W' | b'.')));
            }
            // The hot spot is on the shape: outline or fill.
            assert_ne!(rows[hot_y as usize][hot_x as usize], b'.', "{cursor:?}");
        }
    }

    #[test]
    fn each_shape_puts_its_hot_spot_at_the_point() {
        for cursor in Cursor::ALL {
            let (_, hot_x, hot_y) = mask(cursor);
            let at = Point::new(30, 30);
            let mut fb = Framebuffer::filled(64, 64, GROUND).unwrap();
            render_pointer(
                &mut fb,
                at,
                cursor,
                DeviceScale::TWO,
                Rect::new(0, 0, 64, 64),
            )
            .unwrap();
            assert_ne!(fb.pixel(30, 30), Some(GROUND), "{cursor:?}");
            let area = pointer_area(at, cursor, DeviceScale::TWO);
            assert_eq!(
                (area.left(), area.top()),
                (30 - 2 * i64::from(hot_x), 30 - 2 * i64::from(hot_y)),
                "{cursor:?}"
            );
        }
    }

    #[test]
    fn the_tip_is_at_the_point_and_each_unit_is_scale_pixels() {
        let fb = drawn(Point::new(10, 5), Cursor::Arrow, 8, Rect::new(0, 0, 64, 64));
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
            pointer_area(Point::new(10, 5), Cursor::Arrow, DeviceScale::TWO),
            Rect::new(10, 5, 24, 38)
        );
    }

    #[test]
    fn nothing_outside_the_area_is_touched() {
        let at = Point::new(7, 3);
        for cursor in Cursor::ALL {
            let fb = drawn(at, cursor, 4, Rect::new(0, 0, 64, 64));
            let area = pointer_area(at, cursor, DeviceScale::ONE);
            for y in 0..64 {
                for x in 0..64 {
                    if !area.contains(Point::new(x, y)) {
                        assert_eq!(fb.pixel(x as u32, y as u32), Some(GROUND), "{cursor:?}");
                    }
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
            x in -30..70_i32, y in -40..70_i32, scale in 4..=16_u32, shape in 0..4_usize,
            cx in 0..64_i32, cy in 0..64_i32, cw in 0..64_u32, ch in 0..64_u32,
        ) {
            let (at, cursor) = (Point::new(x, y), Cursor::ALL[shape]);
            let clip = Rect::new(cx, cy, cw, ch);
            let full = drawn(at, cursor, scale, Rect::new(0, 0, 64, 64));
            let part = drawn(at, cursor, scale, clip);
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
            x in any::<i32>(), y in any::<i32>(), scale in 4..=16_u32, shape in 0..4_usize,
        ) {
            let cursor = Cursor::ALL[shape];
            let _ = drawn(Point::new(x, y), cursor, scale, Rect::new(0, 0, 64, 64));
            let scale = DeviceScale::from_quarters(scale).unwrap();
            let _ = pointer_area(Point::new(x, y), cursor, scale);
        }
    }
}
