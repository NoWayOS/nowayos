// SPDX-License-Identifier: MIT OR Apache-2.0
//! Decodes a picture received at run time (#10084): the picture decoder
//! PD's work, a PNG or JPEG into premultiplied RGBA, on that PD's bounded
//! heap.
//!
//! The bytes are hostile. They are refused when longer than
//! [`MAX_ENCODED`]; a picture's size is read from its header and refused
//! past [`MAX_SIDE`] or [`MAX_PIXELS`] before anything is decoded, so no
//! header can make the decoder allocate more than that picture needs; and
//! inflating a PNG stops at the size its header gives, so a decompression
//! bomb ends as a malformed picture. The decoders are `zune-png` and
//! `zune-jpeg` (MIT OR Apache-2.0 OR Zlib, MIT taken), `no_std` with
//! `alloc`; any other error is [`Refusal::Malformed`] or
//! [`Refusal::Unsupported`]. What does not fit the heap stops the PD, which
//! is restarted: its client never is.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use nowayos_picture_wire::{Format, MAX_ENCODED, MAX_PIXELS, MAX_SIDE, Refusal, fits, pixel_len};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

/// A decoded picture: its size, and its pixels, premultiplied RGBA, four
/// bytes a pixel, row by row - exactly as many as it has.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    /// Its width, in pixels.
    pub width: u16,
    /// Its height, in pixels.
    pub height: u16,
    /// Its pixels.
    pub rgba: Vec<u8>,
}

impl core::fmt::Debug for Picture {
    /// Its size, never its pixels.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Decodes `bytes` as `format`: the picture, its pixels allocated for its
/// size alone once that size is known to be within bounds.
///
/// # Errors
///
/// [`Refusal::TooLarge`] for more than [`MAX_ENCODED`] bytes, or a picture
/// past [`MAX_SIDE`] or [`MAX_PIXELS`]; [`Refusal::Malformed`] for bytes
/// that are not a whole, well-formed picture; [`Refusal::Unsupported`] for
/// a colour type it does not take.
pub fn decode(format: Format, bytes: &[u8]) -> Result<Picture, Refusal> {
    let mut rgba = Vec::new();
    let (width, height) = decode_into(format, bytes, &mut rgba)?;
    Ok(Picture {
        width,
        height,
        rgba,
    })
}

/// [`decode`], its pixels into `rgba`, which is made as long as they are.
fn decode_into(format: Format, bytes: &[u8], rgba: &mut Vec<u8>) -> Result<(u16, u16), Refusal> {
    if u32::try_from(bytes.len()).map_or(true, |len| len > MAX_ENCODED) {
        return Err(Refusal::TooLarge);
    }
    // Large enough for any header, so a picture's own size, read from it,
    // is checked here - against our bounds - before any of it is decoded.
    let options = DecoderOptions::new_safe()
        .set_max_width(usize::from(u16::MAX))
        .set_max_height(usize::from(u16::MAX))
        // A truncated or corrupt picture is refused, not filled in.
        .set_strict_mode(true)
        .png_set_strip_to_8bit(true)
        .png_set_add_alpha_channel(true)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    match format {
        Format::Png => png(bytes, options, rgba),
        Format::Jpeg => jpeg(bytes, options, rgba),
    }
}

/// `width` x `height` as the bounds allow, or [`Refusal::TooLarge`].
fn bounded(width: usize, height: usize) -> Result<(u16, u16), Refusal> {
    let size = (
        u16::try_from(width).map_err(|_| Refusal::TooLarge)?,
        u16::try_from(height).map_err(|_| Refusal::TooLarge)?,
    );
    if !fits(size.0, size.1) {
        return Err(Refusal::TooLarge);
    }
    Ok(size)
}

fn png(bytes: &[u8], options: DecoderOptions, out: &mut Vec<u8>) -> Result<(u16, u16), Refusal> {
    // The header's size first, by hand: the signature, then IHDR's width
    // and height, big-endian.
    let (width, height) = png_size(bytes).ok_or(Refusal::Malformed)?;
    bounded(width, height)?;
    let mut decoder = zune_png::PngDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|_| Refusal::Malformed)?;
    let (width, height) = decoder.dimensions().ok_or(Refusal::Malformed)?;
    let size = bounded(width, height)?;
    let channels = match decoder.colorspace() {
        Some(ColorSpace::Luma) => 1,
        Some(ColorSpace::LumaA) => 2,
        Some(ColorSpace::RGB) => 3,
        Some(ColorSpace::RGBA) => 4,
        _ => return Err(Refusal::Unsupported),
    };
    let needed = decoder.output_buffer_size().ok_or(Refusal::Malformed)?;
    let pixels = usize::from(size.0).saturating_mul(usize::from(size.1));
    if needed != pixels.saturating_mul(channels) {
        return Err(Refusal::Unsupported);
    }
    let mut decoded = vec![0_u8; needed];
    decoder
        .decode_into(&mut decoded)
        .map_err(|_| Refusal::Malformed)?;
    out.clear();
    out.resize(pixel_len(size.0, size.1), 0);
    premultiply(&decoded, channels, out);
    Ok(size)
}

/// A PNG's width and height from its IHDR chunk, if it starts as a PNG
/// must.
fn png_size(bytes: &[u8]) -> Option<(usize, usize)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let head = bytes.get(..24)?;
    if head.get(..8)? != SIGNATURE || head.get(12..16)? != b"IHDR" {
        return None;
    }
    let word = |at: usize| -> Option<usize> {
        let quad: [u8; 4] = head.get(at..at.checked_add(4)?)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(quad)).ok()
    };
    Some((word(16)?, word(20)?))
}

fn jpeg(bytes: &[u8], options: DecoderOptions, out: &mut Vec<u8>) -> Result<(u16, u16), Refusal> {
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|_| Refusal::Malformed)?;
    let info = decoder.info().ok_or(Refusal::Malformed)?;
    let size = bounded(usize::from(info.width), usize::from(info.height))?;
    let needed = decoder.output_buffer_size().ok_or(Refusal::Malformed)?;
    if needed != pixel_len(size.0, size.1) {
        return Err(Refusal::Unsupported);
    }
    out.clear();
    out.resize(needed, 0);
    // RGBA, every pixel opaque: premultiplied as it is.
    decoder.decode_into(out).map_err(|_| Refusal::Malformed)?;
    Ok(size)
}

/// `decoded`'s pixels, `channels` bytes each (grey, grey and alpha, RGB or
/// RGBA), into `out` as premultiplied RGBA.
fn premultiply(decoded: &[u8], channels: usize, out: &mut [u8]) {
    let times = |colour: u8, alpha: u8| -> u8 {
        let product = u16::from(colour).saturating_mul(u16::from(alpha));
        // Rounded: (c * a + 127) / 255.
        u8::try_from(product.saturating_add(127) / 255).unwrap_or(u8::MAX)
    };
    for (from, to) in decoded.chunks_exact(channels).zip(out.chunks_exact_mut(4)) {
        let [r, g, b, a] = match *from {
            [grey] => [grey, grey, grey, u8::MAX],
            [grey, alpha] => [grey, grey, grey, alpha],
            [r, g, b] => [r, g, b, u8::MAX],
            [r, g, b, alpha] => [r, g, b, alpha],
            _ => [0, 0, 0, 0],
        };
        to.copy_from_slice(&[times(r, a), times(g, a), times(b, a), a]);
    }
}

const _: () = assert!(MAX_SIDE as u32 * MAX_SIDE as u32 == MAX_PIXELS);
