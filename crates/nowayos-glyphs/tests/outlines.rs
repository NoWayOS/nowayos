// SPDX-License-Identifier: OFL-1.1
//! The outlines against the atlases: at every size an atlas exists for,
//! the outlines drawn by `nowayos-raster` measure exactly as the atlas
//! does and draw what it holds, to within a level or two of coverage.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "tests"
)]

use nowayos_glyphs::{ATLASES, Atlas, Face};
use nowayos_raster::Rasteriser;

/// The face each atlas was made from.
fn face_of(atlas: &Atlas) -> &'static Face {
    atlas.face()
}

/// Small enough to build on a test thread's stack: larger glyphs are drawn in bands.
type Big = Rasteriser<4096, 32_768>;

#[derive(Default)]
struct Tally {
    glyphs: usize,
    same_box: usize,
    pixels: usize,
    same_pixels: usize,
    sum_diff: u64,
    max_diff: u8,
}

#[test]
fn outlines_measure_exactly_as_the_atlases() {
    for atlas in ATLASES {
        let face = face_of(atlas);
        let scale = face.scale(u32::from(atlas.size()) * 64).unwrap();
        assert_eq!(
            face.line_metrics(scale),
            atlas.line_metrics(),
            "{} px",
            atlas.size()
        );
        for glyph in atlas.glyphs() {
            let outline = face.glyph(glyph.ch()).unwrap();
            assert_eq!(
                scale.to_64(outline.advance()),
                glyph.advance(),
                "{:?}",
                glyph.ch()
            );
        }
        for left in atlas.glyphs().iter().step_by(7) {
            for right in atlas.glyphs().iter().step_by(3) {
                let units = face.kerning_units(left.ch(), right.ch());
                assert_eq!(
                    scale.to_64(units),
                    atlas.kerning(left.ch(), right.ch()),
                    "{:?}{:?}",
                    left.ch(),
                    right.ch()
                );
            }
        }
    }
}

#[test]
fn outlines_draw_what_the_atlases_hold() {
    let mut rasteriser: Box<Big> = Box::default();
    let mut total = Tally::default();
    for atlas in ATLASES {
        let face = face_of(atlas);
        let scale = face.outline_scale(u32::from(atlas.size()) * 64).unwrap();
        let mut tally = Tally::default();
        for glyph in atlas.glyphs() {
            let outline = face.glyph(glyph.ch()).unwrap();
            let placement = outline.placement(scale);
            let mut out = vec![0; placement.len()];
            rasteriser
                .render(outline.path(), scale, placement, &mut out)
                .unwrap();
            tally.glyphs += 1;
            // Compare on the atlas's box: coverage outside either box is 0.
            let at = |left: i32, top: i32, width: u32, height: u32, data: &[u8], x: i32, y: i32| {
                let (cx, cy) = (x - left, top - y);
                if cx < 0 || cy < 0 || cx >= width as i32 || cy >= height as i32 {
                    0
                } else {
                    data[(cy as usize) * width as usize + cx as usize]
                }
            };
            let same_box = (
                placement.left(),
                placement.top(),
                placement.width(),
                placement.height(),
            ) == (
                i32::from(glyph.left()),
                i32::from(glyph.top()),
                u32::from(glyph.width()),
                u32::from(glyph.height()),
            );
            tally.same_box += usize::from(same_box);
            let x0 = placement.left().min(i32::from(glyph.left()));
            let x1 = (placement.left() + placement.width() as i32)
                .max(i32::from(glyph.left()) + i32::from(glyph.width()));
            let y1 = placement.top().max(i32::from(glyph.top()));
            let y0 = (placement.top() - placement.height() as i32)
                .min(i32::from(glyph.top()) - i32::from(glyph.height()));
            for y in (y0 + 1)..=y1 {
                for x in x0..x1 {
                    let ours = at(
                        placement.left(),
                        placement.top(),
                        placement.width(),
                        placement.height(),
                        &out,
                        x,
                        y,
                    );
                    let theirs = at(
                        i32::from(glyph.left()),
                        i32::from(glyph.top()),
                        u32::from(glyph.width()),
                        u32::from(glyph.height()),
                        glyph.coverage(),
                        x,
                        y,
                    );
                    let diff = ours.abs_diff(theirs);
                    tally.pixels += 1;
                    tally.same_pixels += usize::from(diff == 0);
                    tally.sum_diff += u64::from(diff);
                    tally.max_diff = tally.max_diff.max(diff);
                }
            }
        }
        println!(
            "{:>3} px {:>3} glyphs: {:>3} same box, {:>6.2}% of {:>7} pixels the same, mean difference {:.3}, largest {}",
            atlas.size(),
            tally.glyphs,
            tally.same_box,
            tally.same_pixels as f64 * 100.0 / tally.pixels as f64,
            tally.pixels,
            tally.sum_diff as f64 / tally.pixels as f64,
            tally.max_diff
        );
        total.glyphs += tally.glyphs;
        total.same_box += tally.same_box;
        total.pixels += tally.pixels;
        total.same_pixels += tally.same_pixels;
        total.sum_diff += tally.sum_diff;
        total.max_diff = total.max_diff.max(tally.max_diff);
    }
    let mean = total.sum_diff as f64 / total.pixels as f64;
    println!(
        "all: {} glyphs, {} same box, {:.2}% of {} pixels the same, mean difference {mean:.3}, largest {}",
        total.glyphs,
        total.same_box,
        total.same_pixels as f64 * 100.0 / total.pixels as f64,
        total.pixels,
        total.max_diff
    );
    // Under a level on average; no pixel more than an eighth of full coverage out.
    assert!(mean < 1.0, "mean difference {mean}");
    assert!(
        total.max_diff <= 32,
        "largest difference {}",
        total.max_diff
    );
}

/// What the atlases would cost at each device scale: every glyph's ink box
/// at every size of the type scale, one byte a pixel, and what drawing
/// them all from outlines takes on this machine.
#[test]
#[ignore = "a measurement: cargo test --release -- --ignored --nocapture"]
fn measure_atlases_and_drawing_at_each_scale() {
    let mut rasteriser: Box<Big> = Box::default();
    for (quarters, name) in [
        (4, "1"),
        (5, "1.25"),
        (6, "1.5"),
        (8, "2"),
        (12, "3"),
        (16, "4"),
    ] {
        let mut bytes = 0usize;
        let mut glyphs = 0usize;
        let started = std::time::Instant::now();
        // Each size the two themes' type scales draw at scale 1, once.
        let base: [(&Face, u32, &Atlas); 12] = {
            use nowayos_glyphs::*;
            [
                (&REGULAR, 14, &REGULAR_14),
                (&REGULAR, 16, &REGULAR_16),
                (&REGULAR, 18, &REGULAR_18),
                (&REGULAR, 24, &REGULAR_24),
                (&BOLD, 14, &BOLD_14),
                (&BOLD, 16, &BOLD_16),
                (&BOLD, 18, &BOLD_18),
                (&BOLD, 20, &BOLD_20),
                (&BOLD, 24, &BOLD_24),
                (&BOLD, 28, &BOLD_28),
                (&BOLD, 36, &BOLD_36),
                (&DISPLAY, 96, &DISPLAY_96),
            ]
        };
        for (face, size, atlas) in base {
            let size64 = size * 64 * quarters / 4;
            let scale = face.outline_scale(size64).unwrap();
            for glyph in atlas.glyphs() {
                let outline = face.glyph(glyph.ch()).unwrap();
                let placement = outline.placement(scale);
                let mut out = vec![0; placement.len()];
                rasteriser
                    .render(outline.path(), scale, placement, &mut out)
                    .unwrap();
                bytes += placement.len();
                glyphs += 1;
            }
        }
        let took = started.elapsed();
        println!(
            "scale {name:>4}: {glyphs} glyphs, {:.2} MB of coverage as atlases; drawn from outlines in {:.1} ms, {:.2} us a glyph",
            bytes as f64 / 1e6,
            took.as_secs_f64() * 1e3,
            took.as_secs_f64() * 1e6 / glyphs as f64
        );
    }
}
