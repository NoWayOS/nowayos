// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: a picture received at run time (`nowayos-picture-decode`),
//! any bytes decoded as a PNG or a JPEG - the picture decoder PD's work.
//!
//! Checks: nothing panics; a picture decoded fits the bounds, and holds
//! exactly its pixels, premultiplied - no colour above its alpha.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_picture_decode::decode;
use nowayos_picture_wire::{Format, fits, pixel_len};

fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let format = if pick % 2 == 0 {
        Format::Png
    } else {
        Format::Jpeg
    };
    if let Ok(picture) = decode(format, bytes) {
        assert!(fits(picture.width, picture.height));
        assert_eq!(picture.rgba.len(), pixel_len(picture.width, picture.height));
        assert!(
            picture
                .rgba
                .chunks_exact(4)
                .all(|p| matches!(*p, [r, g, b, a] if r <= a && g <= a && b <= a))
        );
    }
});
