// SPDX-License-Identifier: MIT OR Apache-2.0
//! The device's outlines against `swash`: at every face and size
//! `glyphs.toml` lists, the outlines `nowayos-glyphs` holds, drawn by
//! `nowayos-raster` as a device draws them, measure exactly as `swash`
//! rasterising the pinned font does and draw what it draws, to within a
//! level or two of coverage. Everything is made in memory: the check needs
//! nothing checked in but the fonts and the outlines.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::as_conversions,
    reason = "test code: a failed check panics"
)]

use std::path::PathBuf;

use nowayos_glyphs::Face;
use nowayos_raster::Rasteriser;

use crate::raster::{self, FaceAtlas, KernRow};
use crate::spec::{self, Spec};

/// Small enough to build on a test thread's stack: larger glyphs are drawn
/// in bands.
type Big = Rasteriser<4096, 32_768>;

/// The face and size points `glyphs.toml` lists: the check covers every one.
const POINTS: usize = 30;

/// The crate whose fonts the tool reads and whose outlines the device draws.
fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/nowayos-glyphs")
}

/// The device's face for `glyphs.toml`'s face `name`.
fn device_face(name: &str) -> &'static Face {
    match name {
        "regular" => &nowayos_glyphs::REGULAR,
        "bold" => &nowayos_glyphs::BOLD,
        "display" => &nowayos_glyphs::DISPLAY,
        "italic" => &nowayos_glyphs::ITALIC,
        "bolditalic" => &nowayos_glyphs::BOLDITALIC,
        "code" => &nowayos_glyphs::CODE,
        "codebold" => &nowayos_glyphs::CODEBOLD,
        other => panic!("glyphs.toml names a face nowayos-glyphs does not hold: {other}"),
    }
}

/// Every face of `glyphs.toml`, rasterised by `swash` at each of its sizes,
/// with the device's face for it.
fn faces() -> Vec<(&'static Face, FaceAtlas)> {
    let dir = crate_dir();
    let spec = Spec::load(&dir).unwrap();
    let inputs = spec::check_pins(&dir, &spec).unwrap();
    let mut faces = Vec::new();
    for (face, (chars, _)) in spec.faces.iter().zip(&spec.face_sets) {
        let pinned = spec.file(&face.file).unwrap();
        let data = &inputs[&face.file];
        let atlas = raster::rasterise(&face.file, data, &pinned.version, &spec, chars, &face.sizes)
            .unwrap();
        faces.push((device_face(&face.name), atlas));
    }
    let points: usize = faces.iter().map(|(_, atlas)| atlas.sizes.len()).sum();
    assert_eq!(points, POINTS, "face and size points in glyphs.toml");
    faces
}

/// `swash`'s pair kerning of `left` then `right` at `size`, in 1/64 pixel.
fn kerning_64(kerning: &[KernRow], left: char, right: char, size: u16, upem: u16) -> i32 {
    kerning
        .iter()
        .find(|pair| pair.left == left && pair.right == right)
        .map_or(0, |pair| {
            raster::to_64(i64::from(pair.units), size, upem).unwrap()
        })
}

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
fn outlines_measure_exactly_as_swash() {
    for (face, atlas) in faces() {
        for size in &atlas.sizes {
            let scale = face.scale(u32::from(size.size) * 64).unwrap();
            let line = face.line_metrics(scale);
            assert_eq!(
                [line.ascent(), line.descent(), line.line_gap()],
                size.line,
                "{} px",
                size.size
            );
            for glyph in &size.glyphs {
                let outline = face.glyph(glyph.ch).unwrap();
                assert_eq!(
                    scale.to_64(outline.advance()),
                    glyph.advance,
                    "{} px {}",
                    size.size,
                    raster::show(glyph.ch)
                );
            }
            for left in size.glyphs.iter().step_by(7) {
                for right in size.glyphs.iter().step_by(3) {
                    let units = face.kerning_units(left.ch, right.ch);
                    assert_eq!(
                        scale.to_64(units),
                        kerning_64(&atlas.kerning, left.ch, right.ch, size.size, atlas.upem),
                        "{} px {}{}",
                        size.size,
                        raster::show(left.ch),
                        raster::show(right.ch)
                    );
                }
            }
        }
    }
}

#[test]
fn outlines_draw_what_swash_draws() {
    let mut rasteriser: Box<Big> = Box::default();
    let mut total = Tally::default();
    let mut points = 0;
    for (face, atlas) in faces() {
        for size in &atlas.sizes {
            points += 1;
            let scale = face.outline_scale(u32::from(size.size) * 64).unwrap();
            let mut tally = Tally::default();
            let mut offset = 0usize;
            for glyph in &size.glyphs {
                let len = usize::from(glyph.width) * usize::from(glyph.height);
                let theirs_data = &size.coverage[offset..offset + len];
                offset += len;
                let outline = face.glyph(glyph.ch).unwrap();
                let placement = outline.placement(scale);
                let mut out = vec![0; placement.len()];
                rasteriser
                    .render(outline.path(), scale, placement, &mut out)
                    .unwrap();
                tally.glyphs += 1;
                // Compare on the union of the two boxes: coverage outside
                // either box is 0.
                let at =
                    |left: i32, top: i32, width: u32, height: u32, data: &[u8], x: i32, y: i32| {
                        let (cx, cy) = (x - left, top - y);
                        if cx < 0 || cy < 0 || cx >= width as i32 || cy >= height as i32 {
                            0
                        } else {
                            data[(cy as usize) * width as usize + cx as usize]
                        }
                    };
                let (left, top) = (i32::from(glyph.left), i32::from(glyph.top));
                let (width, height) = (u32::from(glyph.width), u32::from(glyph.height));
                let same_box = (
                    placement.left(),
                    placement.top(),
                    placement.width(),
                    placement.height(),
                ) == (left, top, width, height);
                tally.same_box += usize::from(same_box);
                let x0 = placement.left().min(left);
                let x1 = (placement.left() + placement.width() as i32).max(left + width as i32);
                let y1 = placement.top().max(top);
                let y0 = (placement.top() - placement.height() as i32).min(top - height as i32);
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
                        let theirs = at(left, top, width, height, theirs_data, x, y);
                        let diff = ours.abs_diff(theirs);
                        tally.pixels += 1;
                        tally.same_pixels += usize::from(diff == 0);
                        tally.sum_diff += u64::from(diff);
                        tally.max_diff = tally.max_diff.max(diff);
                    }
                }
            }
            assert_eq!(
                offset,
                size.coverage.len(),
                "{} px: every glyph's coverage read",
                size.size
            );
            println!(
                "{:>3} px {:>3} glyphs: {:>3} same box, {:>6.2}% of {:>7} pixels the same, mean difference {:.3}, largest {}",
                size.size,
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
    }
    assert_eq!(points, POINTS);
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
