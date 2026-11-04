// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "tests"
)]

use proptest::prelude::*;

use super::*;

/// One font unit is 1/256 pixel: 64/64 pixel per em of 256 units.
fn fine() -> Scale {
    Scale::new(64, 256).unwrap()
}

fn polygon(points: &[(i32, i32)]) -> Vec<Command> {
    let mut path = Vec::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        let point = Point::new(x, y);
        path.push(if i == 0 {
            Command::MoveTo(point)
        } else {
            Command::LineTo(point)
        });
    }
    path.push(Command::Close);
    path
}

fn bounds(path: &[Command]) -> (Point, Point) {
    let mut min = Point::new(i32::MAX, i32::MAX);
    let mut max = Point::new(i32::MIN, i32::MIN);
    let mut take = |p: Point| {
        min = Point::new(min.x.min(p.x), min.y.min(p.y));
        max = Point::new(max.x.max(p.x), max.y.max(p.y));
    };
    for command in path {
        match *command {
            Command::MoveTo(p) | Command::LineTo(p) => take(p),
            Command::QuadTo(a, b) => {
                take(a);
                take(b);
            }
            Command::CurveTo(a, b, c) => {
                take(a);
                take(b);
                take(c);
            }
            Command::Close => {}
        }
    }
    (min, max)
}

fn draw<const CELLS: usize>(path: &[Command], scale: Scale) -> (Placement, Vec<u8>) {
    let (min, max) = bounds(path);
    let placement = Placement::of(min, max, scale).unwrap();
    let mut rasteriser: Box<Rasteriser<4096, CELLS>> = Box::default();
    let mut out = vec![0xAA; placement.len()];
    rasteriser
        .render(path.iter().copied(), scale, placement, &mut out)
        .unwrap();
    (placement, out)
}

#[test]
fn a_half_covered_pixel_is_half_covered() {
    // From half a pixel to two pixels across, one pixel high.
    let (placement, out) = draw::<64>(
        &polygon(&[(128, 0), (512, 0), (512, 256), (128, 256)]),
        fine(),
    );
    assert_eq!((placement.left(), placement.top()), (0, 1));
    assert_eq!(out, vec![128, 255]);
}

#[test]
fn a_diagonal_halves_its_pixels() {
    // A right triangle filling half of a 4 x 4 pixel square.
    let (_, out) = draw::<64>(&polygon(&[(0, 0), (1024, 0), (0, 1024)]), fine());
    for row in 0..4 {
        for column in 0..4 {
            let c = out[row * 4 + column];
            // The diagonal runs from the top-left corner to the bottom
            // right: below it everything, above it nothing, on it half.
            let expected = match column.cmp(&row) {
                core::cmp::Ordering::Less => 255,
                core::cmp::Ordering::Equal => 128,
                core::cmp::Ordering::Greater => 0,
            };
            assert!(
                (i32::from(c) - expected).abs() <= 1,
                "row {row} column {column}: {c}, not {expected}"
            );
        }
    }
}

#[test]
fn either_winding_draws_the_same() {
    let forward = [(0, 0), (900, 100), (700, 800), (100, 600)];
    let mut backward = forward;
    backward.reverse();
    assert_eq!(
        draw::<256>(&polygon(&forward), fine()),
        draw::<256>(&polygon(&backward), fine())
    );
}

#[test]
fn overlapping_contours_cover_once() {
    let mut path = polygon(&[(0, 0), (512, 0), (512, 512), (0, 512)]);
    path.extend(polygon(&[(0, 0), (512, 0), (512, 512), (0, 512)]));
    let (_, out) = draw::<64>(&path, fine());
    assert!(out.iter().all(|&c| c == 255));
}

#[test]
fn a_hole_is_left_empty() {
    // An outer square one way round and an inner one the other: a ring.
    let mut path = polygon(&[(0, 0), (1536, 0), (1536, 1536), (0, 1536)]);
    path.extend(polygon(&[
        (512, 512),
        (512, 1024),
        (1024, 1024),
        (1024, 512),
    ]));
    let (_, out) = draw::<64>(&path, fine());
    // The hole is rows and columns 2 and 3 of six.
    assert_eq!(out[2 * 6 + 2], 0);
    assert_eq!(out[3 * 6 + 3], 0);
    assert_eq!(out[6 + 1], 255);
}

#[test]
fn a_quadratic_curve_bulges_as_drawn() {
    // A half-disc-like bump: its middle column is taller than its edges.
    let path = [
        Command::MoveTo(Point::new(0, 0)),
        Command::QuadTo(Point::new(1024, 2048), Point::new(2048, 0)),
        Command::Close,
    ];
    let (placement, out) = draw::<4096>(&path, fine());
    let width = placement.width() as usize;
    let column = |x: usize| -> u32 {
        (0..placement.height() as usize)
            .map(|y| u32::from(out[y * width + x]))
            .sum()
    };
    assert!(column(width / 2) > column(1));
    // The area under it is two thirds of its box: 2/3 x 8 x 4 pixels.
    let total: u32 = out.iter().map(|&c| u32::from(c)).sum();
    let expected = 2.0 / 3.0 * 8.0 * 4.0 * 255.0;
    assert!(
        (f64::from(total) - expected).abs() < expected * 0.02,
        "{total}"
    );
}

#[test]
fn cubic_curves_are_drawn() {
    let path = [
        Command::MoveTo(Point::new(0, 0)),
        Command::CurveTo(
            Point::new(0, 1024),
            Point::new(1024, 1024),
            Point::new(1024, 0),
        ),
        Command::Close,
    ];
    let (_, out) = draw::<4096>(&path, fine());
    assert!(out.contains(&255));
}

#[test]
fn placement_rounds_out_to_whole_pixels() {
    let scale = Scale::new(18 * 64, 1000).unwrap();
    let placement = Placement::of(Point::new(-10, -200), Point::new(500, 700), scale).unwrap();
    // -0.18 to 9 pixels across, -3.6 to 12.6 up.
    assert_eq!(
        (
            placement.left(),
            placement.top(),
            placement.width(),
            placement.height()
        ),
        (-1, 13, 10, 17)
    );
    assert_eq!(
        Placement::of(Point::new(1, 0), Point::new(0, 0), scale),
        None
    );
}

#[test]
fn errors_are_typed_and_leave_nothing_drawn() {
    let path = polygon(&[(0, 0), (4096, 0), (4096, 256), (0, 256)]);
    let placement = Placement::of(Point::new(0, 0), Point::new(4096, 256), fine()).unwrap();
    let mut small: Rasteriser<16, 8> = Rasteriser::new();
    let mut out = vec![1; placement.len()];
    assert_eq!(
        small.render(path.iter().copied(), fine(), placement, &mut out),
        Err(RasterError::TooLarge)
    );
    let mut few: Rasteriser<1, 64> = Rasteriser::new();
    assert_eq!(
        few.render(path.iter().copied(), fine(), placement, &mut out),
        Err(RasterError::TooComplex)
    );
    let mut short = vec![0; 3];
    assert_eq!(
        few.render(path.iter().copied(), fine(), placement, &mut short),
        Err(RasterError::Buffer)
    );
}

#[test]
fn scale_matches_the_atlas_rounding() {
    // The atlases' metrics: units x size x 64 / upem, half away from zero.
    let scale = Scale::new(24 * 64, 1000).unwrap();
    assert_eq!(scale.to_64(857), 1316);
    assert_eq!(scale.to_64(-123), -189);
    assert_eq!(Scale::new(0, 1000), None);
    assert_eq!(Scale::new(64, 0), None);
    assert_eq!(Scale::new(MAX_SIZE64 + 1, 1000), None);
}

#[test]
fn the_cache_draws_each_glyph_once() {
    let (mut pixels, mut slots) = (vec![0; 64], vec![Slot::EMPTY; 8]);
    let mut cache = GlyphCache::new(&mut pixels, &mut slots);
    let key = GlyphKey {
        face: 1,
        size64: 1152,
        ch: 'a',
    };
    let placement = Placement::of(Point::new(0, 0), Point::new(512, 512), fine()).unwrap();
    let mut draws = 0;
    for _ in 0..3 {
        let glyph = cache
            .get_or_draw(key, placement, |pixels| {
                draws += 1;
                pixels.fill(7);
                Ok(())
            })
            .unwrap();
        assert_eq!(glyph.coverage(), &[7, 7, 7, 7]);
    }
    assert_eq!(draws, 1);
    assert_eq!(cache.get(key).unwrap().placement(), placement);
}

#[test]
fn a_full_cache_empties_and_starts_again() {
    let (mut pixels, mut slots) = (vec![0; 10], vec![Slot::EMPTY; 64]);
    let mut cache = GlyphCache::new(&mut pixels, &mut slots);
    let placement = Placement::of(Point::new(0, 0), Point::new(512, 512), fine()).unwrap();
    for ch in ['a', 'b', 'c'] {
        let key = GlyphKey {
            face: 0,
            size64: 64,
            ch,
        };
        cache.get_or_draw(key, placement, |_| Ok(())).unwrap();
    }
    // Two of four bytes each fit in ten; the third emptied it first.
    assert_eq!((cache.len(), cache.used(), cache.clears()), (1, 4, 1));
    let huge = Placement::of(Point::new(0, 0), Point::new(4096, 4096), fine()).unwrap();
    let key = GlyphKey {
        face: 0,
        size64: 64,
        ch: 'z',
    };
    assert_eq!(
        cache.get_or_draw(key, huge, |_| Ok(())).err(),
        Some(RasterError::TooLarge)
    );
    assert_eq!(
        cache
            .get_or_draw(key, placement, |_| Err(RasterError::TooComplex))
            .err(),
        Some(RasterError::TooComplex)
    );
    assert!(cache.get(key).is_none());
}

#[test]
fn a_crowded_table_empties_before_it_fills() {
    let (mut pixels, mut slots) = (vec![0; 4096], vec![Slot::EMPTY; 8]);
    let mut cache = GlyphCache::new(&mut pixels, &mut slots);
    let placement = Placement::of(Point::new(0, 0), Point::new(256, 256), fine()).unwrap();
    for (i, ch) in ('a'..='z').enumerate() {
        let key = GlyphKey {
            face: 0,
            size64: 64,
            ch,
        };
        cache.get_or_draw(key, placement, |_| Ok(())).unwrap();
        assert!(cache.len() <= 6, "after {i}: {}", cache.len());
        assert!(cache.get(key).is_some());
    }
}

fn arb_polygon() -> impl Strategy<Value = Vec<(i32, i32)>> {
    prop::collection::vec((-3000i32..3000, -3000i32..3000), 3..12)
}

proptest! {
    #[test]
    fn drawing_in_bands_draws_what_drawing_at_once_does(points in arb_polygon()) {
        let path = polygon(&points);
        // 64 cells hold a band of a row or two of a box up to 30 pixels
        // wide; 8,192 the whole box at once.
        prop_assert_eq!(draw::<64>(&path, fine()), draw::<8192>(&path, fine()));
    }

    #[test]
    fn any_box_of_an_outline_draws_what_the_whole_draws_there(
        points in arb_polygon(),
        left in -20_i32..20, top in -20_i32..20, width in 1_u32..30, height in 1_u32..30,
    ) {
        // The outline drawn whole, and a box of it drawn alone - the box
        // may reach past the outline on any side - agree to within two
        // levels: the box's sides split lines, which only moves where
        // their areas are rounded.
        let path = polygon(&points);
        let (whole_box, whole) = draw::<8192>(&path, fine());
        let part_box = Placement::new(left, top, width, height);
        let mut rasteriser: Box<Rasteriser<4096, 8192>> = Box::default();
        let mut part = vec![0; part_box.len()];
        rasteriser.render(path.iter().copied(), fine(), part_box, &mut part).unwrap();
        let at = |placement: Placement, data: &[u8], x: i32, y: i32| -> u8 {
            let (column, row) = (x - placement.left(), placement.top() - y);
            if column < 0 || row < 0 || column >= placement.width() as i32 || row >= placement.height() as i32 {
                0
            } else {
                data[row as usize * placement.width() as usize + column as usize]
            }
        };
        for row in 0..height as i32 {
            for column in 0..width as i32 {
                let (x, y) = (left + column, top - row);
                let (part, whole) = (at(part_box, &part, x, y), at(whole_box, &whole, x, y));
                prop_assert!(part.abs_diff(whole) <= 2, "({}, {}): {} against {}", x, y, part, whole);
            }
        }
    }

    #[test]
    fn coverage_is_conserved(points in arb_polygon()) {
        // A polygon that does not cross itself covers its area.
        let path = polygon(&points);
        let (_, out) = draw::<8192>(&path, fine());
        let total: u64 = out.iter().map(|&c| u64::from(c)).sum();
        let mut twice_area: i64 = 0;
        for (i, &(x0, y0)) in points.iter().enumerate() {
            let (x1, y1) = points[(i + 1) % points.len()];
            twice_area += i64::from(x0) * i64::from(y1) - i64::from(x1) * i64::from(y0);
        }
        // Only when it is simple is the shoelace area the covered area:
        // a triangle always is.
        if points.len() == 3 {
            let area = (twice_area.abs() as f64) / 2.0 / 65_536.0 * 255.0;
            let perimeter: f64 = (0..3)
                .map(|i| {
                    let (x0, y0) = points[i];
                    let (x1, y1) = points[(i + 1) % 3];
                    (f64::from(x1 - x0).hypot(f64::from(y1 - y0))) / 256.0
                })
                .sum();
            // Rounding to 8 bits loses at most half a level a pixel along
            // the edges.
            prop_assert!((total as f64 - area).abs() <= perimeter * 2.0 + 255.0, "{} vs {}", total, area);
        }
    }

    #[test]
    fn any_path_draws_without_panicking(
        commands in prop::collection::vec(
            (0u8..5, any::<i32>(), any::<i32>(), any::<i32>(), any::<i32>()),
            0..24,
        ),
        size64 in 1u32..=MAX_SIZE64,
        upem in 1u16..=u16::MAX,
    ) {
        let path: Vec<Command> = commands
            .iter()
            .map(|&(verb, a, b, c, d)| match verb {
                0 => Command::MoveTo(Point::new(a, b)),
                1 => Command::LineTo(Point::new(a, b)),
                2 => Command::QuadTo(Point::new(a, b), Point::new(c, d)),
                3 => Command::CurveTo(Point::new(a, b), Point::new(c, d), Point::new(b, a)),
                _ => Command::Close,
            })
            .collect();
        let scale = Scale::new(size64, upem).unwrap();
        let placement = Placement::of(Point::new(-50, -50), Point::new(50, 50), scale).unwrap();
        let mut rasteriser: Box<Rasteriser<256, 4096>> = Box::default();
        let mut out = vec![0; placement.len().min(1 << 20)];
        let _ = rasteriser.render(path, scale, placement, &mut out);
    }
}
