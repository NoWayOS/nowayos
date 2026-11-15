// SPDX-License-Identifier: MIT OR Apache-2.0
//! A frame as a BMP file: 24 bits a pixel, uncompressed, which every
//! browser shows and which costs nothing to make.

use embedded_graphics::pixelcolor::RgbColor;
use nowayos_testkit::Framebuffer;

/// The file header (14 bytes) and the info header (40).
const HEADERS: u32 = 54;

/// The frame as a BMP file, or `None` if it has no pixels or is too large
/// for the format's 32-bit sizes.
#[must_use]
pub fn encode(frame: &Framebuffer) -> Option<Vec<u8>> {
    let (width, height) = (frame.width(), frame.height());
    if width == 0 || height == 0 {
        return None;
    }
    // Each row is padded to a multiple of four bytes.
    let row = width
        .checked_mul(3)?
        .checked_add(3)?
        .checked_div(4)?
        .checked_mul(4)?;
    let pixels = row.checked_mul(height)?;
    let size = pixels.checked_add(HEADERS)?;
    let mut out = Vec::with_capacity(usize::try_from(size).ok()?);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&HEADERS.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&i32::try_from(width).ok()?.to_le_bytes());
    // A negative height: the rows run from the top, as the frame's do.
    out.extend_from_slice(&i32::try_from(height).ok()?.checked_neg()?.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixels.to_le_bytes());
    // 72 dots an inch, in pixels a metre.
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let padding = usize::try_from(row.checked_sub(width.checked_mul(3)?)?).ok()?;
    for line in frame.pixels().chunks(usize::try_from(width).ok()?) {
        for pixel in line {
            out.extend_from_slice(&[pixel.b(), pixel.g(), pixel.r()]);
        }
        out.extend(core::iter::repeat_n(0u8, padding));
    }
    Some(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed step fails the test"
)]
mod tests {
    use super::*;
    use embedded_graphics::pixelcolor::Rgb888;

    #[test]
    fn a_frame_is_headers_then_padded_rows_from_the_top() {
        let pixels = vec![
            Rgb888::new(1, 2, 3),
            Rgb888::new(4, 5, 6),
            Rgb888::new(7, 8, 9),
            Rgb888::new(10, 11, 12),
            Rgb888::new(13, 14, 15),
            Rgb888::new(16, 17, 18),
        ];
        let frame = Framebuffer::from_pixels(3, 2, pixels).unwrap();
        let bmp = encode(&frame).unwrap();
        // Rows of 9 bytes padded to 12.
        assert_eq!(bmp.len(), 54 + 2 * 12);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 78);
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), -2);
        // Blue, green, red; the first row first.
        assert_eq!(&bmp[54..66], &[3, 2, 1, 6, 5, 4, 9, 8, 7, 0, 0, 0]);
        assert_eq!(&bmp[66..69], &[12, 11, 10]);
    }
}
