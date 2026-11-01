// SPDX-License-Identifier: MIT OR Apache-2.0
//! PNG images: the golden-image format.
//!
//! Golden images are 8-bit RGB PNG, so that a pull request on GitHub, and
//! any image viewer, shows them. The writer uses no row filter and the
//! strongest compression, which gives the smallest files for flat interface
//! screens. The reader accepts any well-formed 8-bit RGB PNG without
//! interlacing, and rejects anything larger than [`MAX_PIXELS`] before
//! allocating it. Both are the `png` crate's (MIT OR Apache-2.0).

use std::fmt;
use std::io::Cursor;

use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use png::{
    BitDepth, ColorType, Decoder, DeflateCompression, Encoder, Filter, Limits, Transformations,
};

use crate::{Framebuffer, MAX_PIXELS};

/// The most the decoder may allocate, in bytes: a frame of [`MAX_PIXELS`]
/// (48 MiB) and 2 MiB besides.
const DECODE_LIMIT: usize = 52_428_800;

/// Why bytes are not an acceptable PNG image, or a frame could not be
/// encoded as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PngError {
    /// Not a well-formed PNG: a bad signature, chunk, checksum or compressed
    /// stream, truncated, or over the decoder's memory limit. The decoder's
    /// own description.
    Malformed(String),
    /// A PNG, but not 8-bit RGB without interlacing: the only kind the
    /// writer produces.
    Unsupported(String),
    /// Larger than [`MAX_PIXELS`].
    BadSize {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
    /// The encoder refused the frame (one with no pixels).
    Encode(String),
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "not a well-formed PNG: {why}"),
            Self::Unsupported(what) => {
                write!(f, "a PNG of {what}, not 8-bit RGB without interlacing")
            }
            Self::BadSize { width, height } => write!(f, "unacceptable PNG size {width}x{height}"),
            Self::Encode(why) => write!(f, "the frame could not be encoded as PNG: {why}"),
        }
    }
}

impl std::error::Error for PngError {}

impl Framebuffer {
    /// Encodes the framebuffer as an 8-bit RGB PNG, with no row filter and
    /// the strongest compression.
    ///
    /// # Errors
    ///
    /// [`PngError::Encode`] if the encoder refuses the frame: one with no
    /// pixels.
    pub fn to_png(&self) -> Result<Vec<u8>, PngError> {
        let encode = |error: png::EncodingError| PngError::Encode(error.to_string());
        let mut data = Vec::with_capacity(self.pixels().len().saturating_mul(3));
        for pixel in self.pixels() {
            data.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
        }
        let mut out = Vec::new();
        let mut encoder = Encoder::new(&mut out, self.width(), self.height());
        encoder.set_color(ColorType::Rgb);
        encoder.set_depth(BitDepth::Eight);
        encoder.set_deflate_compression(DeflateCompression::Level(9));
        encoder.set_filter(Filter::NoFilter);
        let mut writer = encoder.write_header().map_err(encode)?;
        writer.write_image_data(&data).map_err(encode)?;
        writer.finish().map_err(encode)?;
        Ok(out)
    }

    /// Decodes a PNG.
    ///
    /// # Errors
    ///
    /// A [`PngError`] for anything but a well-formed 8-bit RGB PNG without
    /// interlacing, of at most [`MAX_PIXELS`].
    pub fn from_png(bytes: &[u8]) -> Result<Self, PngError> {
        let malformed = |error: png::DecodingError| PngError::Malformed(error.to_string());
        let mut decoder = Decoder::new_with_limits(
            Cursor::new(bytes),
            Limits {
                bytes: DECODE_LIMIT,
            },
        );
        decoder.set_transformations(Transformations::IDENTITY);
        let mut reader = decoder.read_info().map_err(malformed)?;
        let info = reader.info();
        let (width, height) = (info.width, info.height);
        if info.color_type != ColorType::Rgb || info.bit_depth != BitDepth::Eight || info.interlaced
        {
            return Err(PngError::Unsupported(format!(
                "{:?} at bit depth {:?}{}",
                info.color_type,
                info.bit_depth,
                if info.interlaced { ", interlaced" } else { "" }
            )));
        }
        let count = u64::from(width).saturating_mul(u64::from(height));
        if width == 0 || height == 0 || count > MAX_PIXELS {
            return Err(PngError::BadSize { width, height });
        }
        let size = reader
            .output_buffer_size()
            .ok_or(PngError::BadSize { width, height })?;
        let mut data = vec![0; size];
        let frame = reader.next_frame(&mut data).map_err(malformed)?;
        reader.finish().map_err(malformed)?;
        let pixels = data
            .get(..frame.buffer_size())
            .unwrap_or(&[])
            .chunks_exact(3)
            .map(|rgb| match rgb {
                [r, g, b] => Rgb888::new(*r, *g, *b),
                _ => Rgb888::BLACK,
            })
            .collect();
        Self::from_pixels(width, height, pixels).ok_or(PngError::BadSize { width, height })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    fn sample() -> Framebuffer {
        let mut fb = Framebuffer::new(3, 2).unwrap();
        fb.set(0, 0, Rgb888::new(1, 2, 3));
        fb.set(2, 1, Rgb888::new(250, 251, 252));
        fb
    }

    /// `width` x `height` pixels of `color_type` at `depth`, `bytes`
    /// bytes each, all zero.
    fn encoded(
        width: u32,
        height: u32,
        color_type: ColorType,
        depth: BitDepth,
        bytes: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = Encoder::new(&mut out, width, height);
        encoder.set_color(color_type);
        encoder.set_depth(depth);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&vec![0; bytes * (width * height) as usize])
            .unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn round_trip() {
        let fb = sample();
        let bytes = fb.to_png().unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(Framebuffer::from_png(&bytes), Ok(fb));
    }

    #[test]
    fn reads_rows_written_with_any_filter() {
        let fb = sample();
        let mut data = Vec::new();
        for pixel in fb.pixels() {
            data.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
        }
        for filter in [Filter::Sub, Filter::Up, Filter::Avg, Filter::Paeth] {
            let mut out = Vec::new();
            let mut encoder = Encoder::new(&mut out, 3, 2);
            encoder.set_color(ColorType::Rgb);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_filter(filter);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&data).unwrap();
            writer.finish().unwrap();
            assert_eq!(Framebuffer::from_png(&out), Ok(fb.clone()), "{filter:?}");
        }
    }

    #[test]
    fn rejects_malformed_files() {
        for bytes in [
            &b""[..],
            b"P6\n1 1\n255\n\x01\x02\x03",
            b"\x89PNG\r\n\x1a\n",
        ] {
            match Framebuffer::from_png(bytes) {
                Err(PngError::Malformed(_)) => {}
                other => panic!("expected Malformed, got {other:?}"),
            }
        }
        let good = sample().to_png().unwrap();
        // Truncated inside the image data, and a flipped bit in a checksum.
        let mut flipped = good.clone();
        let last = flipped.len() - 13;
        flipped[last] ^= 1;
        for bytes in [&good[..good.len() - 20], &flipped[..]] {
            match Framebuffer::from_png(bytes) {
                Err(PngError::Malformed(_)) => {}
                other => panic!("expected Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_other_kinds_of_png() {
        for (color_type, depth, bytes) in [
            (ColorType::Rgba, BitDepth::Eight, 4),
            (ColorType::Grayscale, BitDepth::Eight, 1),
            (ColorType::Rgb, BitDepth::Sixteen, 6),
        ] {
            match Framebuffer::from_png(&encoded(2, 2, color_type, depth, bytes)) {
                Err(PngError::Unsupported(_)) => {}
                other => panic!("expected Unsupported, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_too_large_before_decoding() {
        // The header promises 5000 x 5000; the data is never reached.
        let mut bytes = encoded(1, 1, ColorType::Rgb, BitDepth::Eight, 3);
        let ihdr = &mut bytes[16..24];
        ihdr.copy_from_slice(&[0, 0, 0x13, 0x88, 0, 0, 0x13, 0x88]);
        let crc = crc32(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(
            Framebuffer::from_png(&bytes),
            Err(PngError::BadSize {
                width: 5000,
                height: 5000
            })
        );
    }

    #[test]
    fn a_frame_with_no_pixels_is_not_encoded() {
        let fb = Framebuffer::new(0, 1).unwrap();
        match fb.to_png() {
            Err(PngError::Encode(_)) => {}
            other => panic!("expected Encode, got {other:?}"),
        }
    }

    /// The PNG chunk checksum (CRC-32, ISO 3309).
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
}
