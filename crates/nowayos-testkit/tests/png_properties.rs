// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the PNG reader: arbitrary bytes, and a valid image
//! with a byte changed or cut short, never panic it; every framebuffer
//! survives a write / read round-trip.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_testkit::Framebuffer;
use proptest::prelude::*;

fn framebuffer(width: u32, height: u32, seed: u64) -> Framebuffer {
    let count = (width * height) as usize;
    let pixels: Vec<Rgb888> = (0..count)
        .map(|i| {
            let v = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(i as u64);
            Rgb888::new(v as u8, (v >> 8) as u8, (v >> 16) as u8)
        })
        .collect();
    Framebuffer::from_pixels(width, height, pixels).unwrap()
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = Framebuffer::from_png(&bytes);
    }

    #[test]
    fn a_changed_or_shortened_image_never_panics(
        width in 1u32..8,
        height in 1u32..8,
        seed in any::<u64>(),
        at in any::<prop::sample::Index>(),
        value in any::<u8>(),
        cut in any::<prop::sample::Index>(),
    ) {
        let bytes = framebuffer(width, height, seed).to_png().unwrap();
        let mut changed = bytes.clone();
        changed[at.index(bytes.len())] = value;
        let _ = Framebuffer::from_png(&changed);
        let _ = Framebuffer::from_png(&bytes[..cut.index(bytes.len())]);
    }

    #[test]
    fn round_trip(width in 1u32..16, height in 1u32..16, seed in any::<u64>()) {
        let fb = framebuffer(width, height, seed);
        prop_assert_eq!(Framebuffer::from_png(&fb.to_png().unwrap()), Ok(fb));
    }
}
