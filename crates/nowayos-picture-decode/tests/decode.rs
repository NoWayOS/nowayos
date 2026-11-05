// SPDX-License-Identifier: MIT OR Apache-2.0
//! Decoding pictures received at run time (#10084): NoWayOS's sample JPEG
//! and PNG decode to their size, premultiplied; hostile pictures - a
//! header claiming a huge size, a decompression bomb, truncated or
//! corrupted bytes, the wrong format - are refused with a typed reason,
//! never a panic. A bomb's inflating stops at the size its header gives
//! (`zune-png` limits it so); past its bounded heap the PD would stop and
//! be restarted.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_picture_decode::decode;
use nowayos_picture_wire::PIXEL_BYTES;
use nowayos_picture_wire::{Format, Refusal};
use proptest::prelude::*;

const LANDSCAPE: &[u8] = include_bytes!("../../nowayos-pictures/pictures/landscape.jpg");
const BADGE: &[u8] = include_bytes!("../../nowayos-pictures/pictures/badge.png");
const BOMB: &[u8] = include_bytes!("pictures/bomb.png");
const HUGE: &[u8] = include_bytes!("pictures/huge.png");

#[test]
fn the_sample_jpeg_and_png_decode_premultiplied() {
    let landscape = decode(Format::Jpeg, LANDSCAPE).unwrap();
    assert_eq!((landscape.width, landscape.height), (640, 400));
    assert_eq!(landscape.rgba.len(), 640 * 400 * 4);
    // Opaque everywhere.
    assert!(landscape.rgba.chunks_exact(4).all(|p| p[3] == 255));
    let badge = decode(Format::Png, BADGE).unwrap();
    assert_eq!((badge.width, badge.height), (192, 192));
    let pixels = &badge.rgba[..];
    assert_eq!(pixels.len(), 192 * 192 * 4);
    // Its corner is transparent - premultiplied, so black too - and its
    // middle opaque; no colour is ever above its alpha.
    assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
    let middle = (96 * 192 + 96) * 4;
    assert_eq!(pixels[middle + 3], 255);
    assert!(
        pixels
            .chunks_exact(4)
            .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
    );
}

#[test]
fn hostile_pictures_are_refused_within_bounds() {
    // A header claiming 100000 x 100000: refused before it is decoded.
    assert_eq!(decode(Format::Png, HUGE), Err(Refusal::TooLarge));
    // A bomb: a 1024 x 1024 header over sixteen times the rows. Inflating
    // stops at the size the header gives.
    let bombed = decode(Format::Png, BOMB).map(|p| (p.width, p.height, p.rgba.len()));
    assert!(
        matches!(
            bombed,
            Ok((1024, 1024, PIXEL_BYTES)) | Err(Refusal::Malformed)
        ),
        "{bombed:?}"
    );
    // Truncated, corrupt or the wrong format: malformed.
    for (format, bytes) in [
        (Format::Jpeg, &LANDSCAPE[..LANDSCAPE.len() / 2]),
        (Format::Png, &BADGE[..BADGE.len() / 2]),
        (Format::Png, &BADGE[..20]),
        (Format::Jpeg, BADGE),
        (Format::Png, LANDSCAPE),
        (Format::Png, &[][..]),
    ] {
        assert!(decode(format, bytes).is_err(), "{format:?} {}", bytes.len());
    }
    // More than the region holds: too large, whatever it says.
    let long = vec![0_u8; (1 << 20) + 1];
    assert_eq!(decode(Format::Png, &long), Err(Refusal::TooLarge));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any bytes, as either format: a picture or a typed refusal, never a
    /// panic.
    #[test]
    fn any_bytes_decode_or_are_refused(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        for format in [Format::Png, Format::Jpeg] {
            if let Ok(picture) = decode(format, &bytes) {
                let (width, height) = (usize::from(picture.width), usize::from(picture.height));
                prop_assert_eq!(width * height * 4, picture.rgba.len());
            }
        }
    }

    /// The samples with bytes changed anywhere: decoded or refused, never
    /// a panic, never more pixels than the region holds.
    #[test]
    fn corrupted_samples_decode_or_are_refused(
        at in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
    ) {
        for (format, sample) in [(Format::Png, BADGE), (Format::Jpeg, LANDSCAPE)] {
            let mut bytes = sample.to_vec();
            for (index, byte) in &at {
                let i = index.index(bytes.len());
                bytes[i] = *byte;
            }
            if let Ok(picture) = decode(format, &bytes) {
                let (width, height) = (usize::from(picture.width), usize::from(picture.height));
                prop_assert_eq!(width * height * 4, picture.rgba.len());
                prop_assert!(picture.rgba.len() <= PIXEL_BYTES);
            }
        }
    }
}
