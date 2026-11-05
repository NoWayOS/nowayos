// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure fails the test"
)]

use proptest::prelude::*;

use crate::{Fit, Picture, Pixels, samples};

static FOUR: [u8; 16] = [
    255, 0, 0, 255, // red
    0, 255, 0, 255, // green
    0, 0, 255, 255, // blue
    0, 0, 0, 0, // nothing
];
static SQUARE: [Pixels; 1] = [Pixels::new(8, 2, 2, &FOUR)];
static TINY: Picture = Picture::new(1, 1, &SQUARE);

#[test]
fn every_sample_has_its_size_at_each_scale_and_premultiplied_pixels() {
    for (name, picture) in &samples::ALL {
        for pixels in picture.pixels() {
            let quarters = pixels.quarters();
            assert_eq!(
                pixels.width(),
                (picture.width() * quarters + 2) / 4,
                "{name}"
            );
            assert_eq!(
                pixels.height(),
                (picture.height() * quarters + 2) / 4,
                "{name}"
            );
            assert!(
                pixels
                    .rgba()
                    .chunks_exact(4)
                    .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]),
                "{name}: a colour above its alpha"
            );
        }
    }
    // The landscape is opaque; the badge's corners are not.
    let landscape = samples::LANDSCAPE.best(160, 100);
    assert!(landscape.rgba().chunks_exact(4).all(|p| p[3] == 255));
    assert_eq!(samples::BADGE.best(48, 48).at(0, 0), Some([0, 0, 0, 0]));
    assert_eq!(samples::BADGE.best(48, 48).at(24, 2).unwrap()[3], 255);
}

#[test]
fn the_best_pixels_are_exact_then_the_least_larger_then_the_largest() {
    let landscape = &samples::LANDSCAPE;
    assert_eq!(landscape.best(160, 100).quarters(), 4);
    assert_eq!(landscape.best(320, 200).quarters(), 8);
    // 1.5: shrunk from 2's.
    assert_eq!(landscape.best(240, 150).quarters(), 8);
    // Smaller than 1's: shrunk from 1's.
    assert_eq!(landscape.best(80, 50).quarters(), 4);
    // Larger than any: the largest, enlarged.
    assert_eq!(landscape.best(640, 400).quarters(), 8);
    // A box wider than 1's but not higher: 2's.
    assert_eq!(landscape.best(161, 10).quarters(), 8);
}

#[test]
fn drawn_at_its_own_size_each_pixel_is_its_own() {
    let pixels = &SQUARE[0];
    assert_eq!(pixels.sample(0, 0, 2, 2), [255, 0, 0, 255]);
    assert_eq!(pixels.sample(1, 0, 2, 2), [0, 255, 0, 255]);
    assert_eq!(pixels.sample(0, 1, 2, 2), [0, 0, 255, 255]);
    assert_eq!(pixels.sample(1, 1, 2, 2), [0, 0, 0, 0]);
    assert_eq!(pixels.sample(2, 0, 2, 2), [0; 4]);
    assert_eq!(TINY.best(2, 2).width(), 2);
}

#[test]
fn enlarged_a_pixel_blends_its_neighbours_by_distance() {
    let pixels = &SQUARE[0];
    // At 4 x 4, pixel x's centre lies at (x + 1/2) / 2 - 1/2 of the
    // source's: -1/4, clamped to the edge, so red; then 1/4 and 3/4 of the
    // way from red to green; then 5/4, clamped, so green.
    assert_eq!(pixels.sample(0, 0, 4, 4), [255, 0, 0, 255]);
    assert_eq!(pixels.sample(1, 0, 4, 4), [191, 64, 0, 255]);
    assert_eq!(pixels.sample(2, 0, 4, 4), [64, 191, 0, 255]);
    assert_eq!(pixels.sample(3, 0, 4, 4), [0, 255, 0, 255]);
    // Down the right edge, 3/4 of the way from green to nothing: the
    // colour fades with its alpha.
    assert_eq!(pixels.sample(3, 2, 4, 4), [0, 64, 0, 64]);
}

#[test]
fn contain_fits_inside_and_cover_covers_keeping_the_shape() {
    let picture = (320, 200);
    assert_eq!(
        Fit::Contain.place(picture, (100, 100)),
        crate::Placed {
            left: 0,
            top: 18,
            width: 100,
            height: 63
        }
    );
    assert_eq!(
        Fit::Cover.place(picture, (100, 100)),
        crate::Placed {
            left: -30,
            top: 0,
            width: 160,
            height: 100
        }
    );
    assert_eq!(
        Fit::Fill.place(picture, (100, 100)),
        crate::Placed {
            left: 0,
            top: 0,
            width: 100,
            height: 100
        }
    );
    // The same shape: every fit is the box.
    for fit in [Fit::Fill, Fit::Contain, Fit::Cover] {
        let placed = fit.place(picture, (160, 100));
        assert_eq!(
            (placed.left, placed.top, placed.width, placed.height),
            (0, 0, 160, 100)
        );
    }
    // Nothing to place.
    assert_eq!(Fit::Contain.place((0, 5), (10, 10)).width, 0);
    assert_eq!(Fit::Cover.place((5, 5), (0, 10)).width, 0);
}

proptest! {
    #[test]
    fn any_fit_of_any_picture_in_any_box(
        pw in 1_u32..5000, ph in 1_u32..5000, aw in 1_u32..5000, ah in 1_u32..5000,
    ) {
        let contain = Fit::Contain.place((pw, ph), (aw, ah));
        prop_assert!(contain.width <= aw && contain.height <= ah);
        prop_assert!(contain.width == aw || contain.height == ah);
        prop_assert!(contain.left >= 0 && contain.top >= 0);
        let cover = Fit::Cover.place((pw, ph), (aw, ah));
        prop_assert!(cover.width >= aw && cover.height >= ah);
        prop_assert!(cover.width == aw || cover.height == ah);
        prop_assert!(cover.left <= 0 && cover.top <= 0);
        // The shape kept, to within the rounding of one side.
        for placed in [contain, cover] {
            let (w, h) = (u64::from(placed.width), u64::from(placed.height));
            let cross = (w * u64::from(ph)).abs_diff(h * u64::from(pw));
            prop_assert!(cross <= u64::from(pw.max(ph)), "{placed:?}");
            // Centred: the two margins differ by at most a pixel.
            let side = i64::from(aw) - i64::from(placed.width) - placed.left;
            prop_assert!((side - placed.left).abs() <= 1);
        }
    }

    #[test]
    fn sampling_any_size_never_panics_and_stays_premultiplied(
        width in 0_u32..600, height in 0_u32..600, x in 0_u32..700, y in 0_u32..700,
    ) {
        for (_, picture) in &samples::ALL {
            let pixels = picture.best(width, height);
            let [r, g, b, a] = pixels.sample(x, y, width, height);
            prop_assert!(r <= a && g <= a && b <= a);
        }
    }
}
