// SPDX-License-Identifier: MIT OR Apache-2.0
//! Framebuffer pixel formats and memory layout.

use core::fmt;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::{Rect, Size};

/// How one pixel is stored in framebuffer memory.
///
/// Names describe the value of a little-endian pixel word from its most to
/// its least significant bits, as Linux DRM "fourcc" names do; each variant
/// also states the byte order in memory, which is what actually matters to a
/// driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 16 bits: a little-endian word `rrrrrggg gggbbbbb`.
    Rgb565,
    /// 24 bits: bytes red, green, blue.
    Rgb888,
    /// 24 bits: bytes blue, green, red.
    Bgr888,
    /// 32 bits: a little-endian word `0x__RRGGBB`, so bytes blue, green, red,
    /// unused. The usual firmware framebuffer layout.
    Xrgb8888,
    /// 32 bits: a little-endian word `0x__BBGGRR`, so bytes red, green, blue,
    /// unused.
    Xbgr8888,
}

impl PixelFormat {
    /// Bytes each pixel occupies.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u8 {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 | Self::Bgr888 => 3,
            Self::Xrgb8888 | Self::Xbgr8888 => 4,
        }
    }

    /// Writes the colour `(r, g, b)` in this format to the start of `out`,
    /// returning the number of bytes written. Unused bytes are written as
    /// zero. Returns `None` (writing nothing) if `out` is too short.
    pub fn encode(self, r: u8, g: u8, b: u8, out: &mut [u8]) -> Option<usize> {
        match self {
            Self::Rgb565 => {
                let word = (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3);
                write_bytes(out, &word.to_le_bytes())
            }
            Self::Rgb888 => write_bytes(out, &[r, g, b]),
            Self::Bgr888 => write_bytes(out, &[b, g, r]),
            Self::Xrgb8888 => write_bytes(out, &[b, g, r, 0]),
            Self::Xbgr8888 => write_bytes(out, &[r, g, b, 0]),
        }
    }
}

impl PixelFormat {
    /// The colour of the pixel at the start of `bytes`, as (red, green,
    /// blue): what [`encode`](Self::encode) stored, but for the low bits a
    /// 16-bit format drops, which come back as copies of the high bits (so
    /// full white is white again). `None` if `bytes` is shorter than a
    /// pixel.
    #[must_use]
    pub fn decode(self, bytes: &[u8]) -> Option<(u8, u8, u8)> {
        let pixel = bytes.get(..usize::from(self.bytes_per_pixel()))?;
        match (self, pixel) {
            (Self::Rgb565, &[low, high]) => {
                let word = u16::from_le_bytes([low, high]);
                let red = u8::try_from(word >> 11).ok()?;
                let green = u8::try_from((word >> 5) & 0x3F).ok()?;
                let blue = u8::try_from(word & 0x1F).ok()?;
                Some((
                    (red << 3) | (red >> 2),
                    (green << 2) | (green >> 4),
                    (blue << 3) | (blue >> 2),
                ))
            }
            (Self::Rgb888, &[r, g, b]) | (Self::Xbgr8888, &[r, g, b, _]) => Some((r, g, b)),
            (Self::Bgr888, &[b, g, r]) | (Self::Xrgb8888, &[b, g, r, _]) => Some((r, g, b)),
            _ => None,
        }
    }
}

/// Copies `bytes` to the start of `out`, or writes nothing if it is too short.
fn write_bytes(out: &mut [u8], bytes: &[u8]) -> Option<usize> {
    out.get_mut(..bytes.len())?.copy_from_slice(bytes);
    Some(bytes.len())
}

/// Why a [`FramebufferLayout`] was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// Width or height is zero.
    Empty,
    /// A row's stride is shorter than its pixels.
    StrideTooSmall {
        /// The stride given, in bytes.
        stride: u32,
        /// The minimum stride for the width and format, in bytes.
        minimum: u64,
    },
    /// The framebuffer would not fit in this machine's address space.
    TooLarge,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("framebuffer has a zero dimension"),
            Self::StrideTooSmall { stride, minimum } => {
                write!(f, "stride {stride} is below the minimum {minimum}")
            }
            Self::TooLarge => f.write_str("framebuffer is too large for the address space"),
        }
    }
}

impl core::error::Error for LayoutError {}

/// The memory layout of a framebuffer: dimensions, row stride and format.
///
/// Validated on construction, so [`pixel_offset`](Self::pixel_offset) is a
/// checked lookup that can never point outside [`byte_len`](Self::byte_len).
///
/// ```
/// use nowayos_display::{FramebufferLayout, PixelFormat};
///
/// let fb = FramebufferLayout::packed(640, 480, PixelFormat::Xrgb8888).unwrap();
/// assert_eq!(fb.byte_len(), 640 * 480 * 4);
/// assert_eq!(fb.pixel_offset(1, 1), Some(640 * 4 + 4));
/// assert_eq!(fb.pixel_offset(640, 0), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FramebufferLayout {
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
    byte_len: usize,
}

impl FramebufferLayout {
    /// A layout whose rows are `stride` bytes apart (rows may be padded).
    ///
    /// # Errors
    ///
    /// [`LayoutError`] if a dimension is zero, the stride cannot hold a row,
    /// or the whole buffer would not fit in `usize`.
    pub fn new(
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    ) -> Result<Self, LayoutError> {
        if width == 0 || height == 0 {
            return Err(LayoutError::Empty);
        }
        let minimum = u64::from(width).saturating_mul(u64::from(format.bytes_per_pixel()));
        if u64::from(stride) < minimum {
            return Err(LayoutError::StrideTooSmall { stride, minimum });
        }
        let total = u64::from(stride).saturating_mul(u64::from(height));
        let byte_len = usize::try_from(total).map_err(|_| LayoutError::TooLarge)?;
        Ok(Self {
            width,
            height,
            stride,
            format,
            byte_len,
        })
    }

    /// A layout with no padding between rows.
    ///
    /// # Errors
    ///
    /// As for [`new`](Self::new); also [`LayoutError::TooLarge`] if one row
    /// would exceed `u32::MAX` bytes.
    pub fn packed(width: u32, height: u32, format: PixelFormat) -> Result<Self, LayoutError> {
        let stride = width
            .checked_mul(u32::from(format.bytes_per_pixel()))
            .ok_or(LayoutError::TooLarge)?;
        Self::new(width, height, stride, format)
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes from the start of one row to the start of the next.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// The pixel format.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Width and height.
    #[must_use]
    pub const fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    /// The whole screen as a rectangle at the origin.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        Rect::from_size(self.size())
    }

    /// Total bytes the framebuffer occupies (`stride × height`).
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Byte offset of pixel `(x, y)`, or `None` if it is off-screen.
    #[must_use]
    pub fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let row = u64::from(y).checked_mul(u64::from(self.stride))?;
        let column = u64::from(x).checked_mul(u64::from(self.format.bytes_per_pixel()))?;
        usize::try_from(row.checked_add(column)?).ok()
    }

    /// Writes colour `(r, g, b)` at pixel `(x, y)` of `buffer`. Returns
    /// `false` (writing nothing) if the pixel is off-screen or `buffer` is
    /// shorter than the layout says.
    pub fn write_pixel(&self, buffer: &mut [u8], x: u32, y: u32, rgb: (u8, u8, u8)) -> bool {
        self.store_pixel(&mut &mut *buffer, x, y, rgb)
    }

    /// Stores colour `(r, g, b)` at pixel `(x, y)` of `memory`, in this
    /// layout's format. Returns `false` (storing nothing) if the pixel is
    /// off-screen or `memory` is shorter than the layout says.
    pub fn store_pixel<M: FrameMemory>(
        &self,
        memory: &mut M,
        x: u32,
        y: u32,
        rgb: (u8, u8, u8),
    ) -> bool {
        let Some(offset) = self.pixel_offset(x, y) else {
            return false;
        };
        let mut encoded = [0; 4];
        self.format
            .encode(rgb.0, rgb.1, rgb.2, &mut encoded)
            .and_then(|len| encoded.get(..len))
            .is_some_and(|bytes| memory.store(offset, bytes))
    }
}

/// Memory a framebuffer's pixels are stored in: ordinary bytes this program
/// owns (`&mut [u8]`), or bytes shared with another protection domain
/// (`&[AtomicU8]`), which another PD may read - or, if misconfigured, write -
/// at any moment, so every access is atomic.
pub trait FrameMemory {
    /// The number of bytes.
    fn frame_len(&self) -> usize;

    /// Copies `bytes` to `offset`. Returns `false` (copying nothing) if they
    /// do not fit.
    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool;

    /// Copies the `len` bytes at `from` to `to`, as if through a buffer:
    /// the two may overlap. Returns `false` (copying nothing) if either
    /// does not fit, or if this memory cannot copy within itself - then
    /// its owner draws the bytes again instead.
    fn copy_within(&mut self, from: usize, to: usize, len: usize) -> bool {
        let _ = (from, to, len);
        false
    }
}

impl FrameMemory for &mut [u8] {
    fn frame_len(&self) -> usize {
        self.len()
    }

    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        offset
            .checked_add(bytes.len())
            .and_then(|end| self.get_mut(offset..end))
            .map(|span| span.copy_from_slice(bytes))
            .is_some()
    }

    fn copy_within(&mut self, from: usize, to: usize, len: usize) -> bool {
        let fits = |at: usize| at.checked_add(len).is_some_and(|end| end <= self.len());
        if !fits(from) || !fits(to) {
            return false;
        }
        match from.checked_add(len) {
            Some(end) => {
                <[u8]>::copy_within(self, from..end, to);
                true
            }
            None => false,
        }
    }
}

impl FrameMemory for &[AtomicU8] {
    fn frame_len(&self) -> usize {
        self.len()
    }

    /// Relaxed byte stores: the frame's bytes reach the reader when the
    /// message announcing them is published with release ordering.
    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let Some(span) = offset
            .checked_add(bytes.len())
            .and_then(|end| self.get(offset..end))
        else {
            return false;
        };
        for (cell, byte) in span.iter().zip(bytes) {
            cell.store(*byte, Ordering::Relaxed);
        }
        true
    }

    /// Relaxed byte loads and stores, a byte at a time, in the order that
    /// keeps an overlapping copy right.
    fn copy_within(&mut self, from: usize, to: usize, len: usize) -> bool {
        let span = |at: usize| at.checked_add(len).and_then(|end| self.get(at..end));
        let (Some(source), Some(target)) = (span(from), span(to)) else {
            return false;
        };
        let copy = |(cell, byte): (&AtomicU8, &AtomicU8)| {
            cell.store(byte.load(Ordering::Relaxed), Ordering::Relaxed);
        };
        if to <= from {
            target.iter().zip(source).for_each(copy);
        } else {
            target.iter().rev().zip(source.iter().rev()).for_each(copy);
        }
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn shared_and_owned_memory_hold_the_same_pixels() {
        let layout = FramebufferLayout::new(3, 2, 14, PixelFormat::Xrgb8888).unwrap();
        let mut owned = [0xEE_u8; 28];
        let shared: [AtomicU8; 28] = core::array::from_fn(|_| AtomicU8::new(0xEE));
        let mut shared_view = &shared[..];
        for (x, y) in [(0, 0), (2, 1), (1, 1)] {
            assert!(layout.write_pixel(&mut owned, x, y, (1, 2, 3)));
            assert!(layout.store_pixel(&mut shared_view, x, y, (1, 2, 3)));
        }
        assert!(
            !layout.store_pixel(&mut shared_view, 3, 0, (1, 2, 3)),
            "off-screen"
        );
        let shared_bytes = shared.map(|cell| cell.load(Ordering::Relaxed));
        assert_eq!(owned, shared_bytes);
        assert_eq!(&owned[..4], &[3, 2, 1, 0]);
        assert_eq!(&owned[12..14], &[0xEE, 0xEE], "row padding untouched");
    }

    #[test]
    fn frame_memory_refuses_what_does_not_fit() {
        let mut bytes = [0_u8; 4];
        let mut owned = &mut bytes[..];
        assert_eq!(owned.frame_len(), 4);
        assert!(owned.store(2, &[9, 9]));
        assert!(!owned.store(3, &[1, 1]));
        assert!(!owned.store(usize::MAX, &[1]));
        assert_eq!(bytes, [0, 0, 9, 9]);
        let cells: [AtomicU8; 4] = core::array::from_fn(|_| AtomicU8::new(0));
        let mut shared = &cells[..];
        assert_eq!(shared.frame_len(), 4);
        assert!(!shared.store(3, &[1, 1]));
        assert!(!shared.store(usize::MAX, &[1]));
        assert!(shared.store(0, &[5]));
        let stored: [u8; 4] = core::array::from_fn(|index| cells[index].load(Ordering::Relaxed));
        assert_eq!(stored, [5, 0, 0, 0]);
        let short = FramebufferLayout::packed(2, 1, PixelFormat::Xrgb8888).unwrap();
        assert!(
            !short.store_pixel(&mut shared, 1, 0, (1, 1, 1)),
            "memory shorter than the layout"
        );
    }

    #[test]
    fn encodes_each_format() {
        let mut out = [0xAAu8; 4];
        assert_eq!(PixelFormat::Rgb888.encode(1, 2, 3, &mut out), Some(3));
        assert_eq!(out, [1, 2, 3, 0xAA]);
        assert_eq!(PixelFormat::Bgr888.encode(1, 2, 3, &mut out), Some(3));
        assert_eq!(out[..3], [3, 2, 1]);
        assert_eq!(PixelFormat::Xrgb8888.encode(1, 2, 3, &mut out), Some(4));
        assert_eq!(out, [3, 2, 1, 0]);
        assert_eq!(PixelFormat::Xbgr8888.encode(1, 2, 3, &mut out), Some(4));
        assert_eq!(out, [1, 2, 3, 0]);
        // Pure red, green and blue in RGB565.
        assert_eq!(PixelFormat::Rgb565.encode(255, 0, 0, &mut out), Some(2));
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 0xF800);
        PixelFormat::Rgb565.encode(0, 255, 0, &mut out);
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 0x07E0);
        PixelFormat::Rgb565.encode(0, 0, 255, &mut out);
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 0x001F);
    }

    #[test]
    fn decode_reads_back_what_encode_wrote() {
        let mut out = [0u8; 4];
        for format in [
            PixelFormat::Rgb888,
            PixelFormat::Bgr888,
            PixelFormat::Xrgb8888,
            PixelFormat::Xbgr8888,
        ] {
            format.encode(1, 2, 3, &mut out).unwrap();
            assert_eq!(format.decode(&out), Some((1, 2, 3)), "{format:?}");
        }
        for (rgb, back) in [
            ((0xFF, 0xFF, 0xFF), (0xFF, 0xFF, 0xFF)),
            ((0, 0, 0), (0, 0, 0)),
            ((0xFF, 0, 0), (0xFF, 0, 0)),
            ((0x12, 0x34, 0x56), (0x10, 0x34, 0x52)),
        ] {
            PixelFormat::Rgb565
                .encode(rgb.0, rgb.1, rgb.2, &mut out)
                .unwrap();
            assert_eq!(PixelFormat::Rgb565.decode(&out), Some(back));
        }
        assert_eq!(PixelFormat::Xrgb8888.decode(&[1, 2, 3]), None);
        assert_eq!(PixelFormat::Rgb565.decode(&[1]), None);
    }

    #[test]
    fn encode_refuses_short_buffers() {
        let mut out = [0u8; 3];
        assert_eq!(PixelFormat::Xrgb8888.encode(1, 2, 3, &mut out), None);
        assert_eq!(out, [0, 0, 0]);
    }

    #[test]
    fn layout_validation() {
        assert_eq!(
            FramebufferLayout::new(0, 10, 40, PixelFormat::Rgb888),
            Err(LayoutError::Empty)
        );
        assert_eq!(
            FramebufferLayout::new(10, 10, 29, PixelFormat::Rgb888),
            Err(LayoutError::StrideTooSmall {
                stride: 29,
                minimum: 30
            })
        );
        assert_eq!(
            FramebufferLayout::packed(u32::MAX, 1, PixelFormat::Xrgb8888),
            Err(LayoutError::TooLarge)
        );
        let padded = FramebufferLayout::new(10, 4, 32, PixelFormat::Rgb888).unwrap();
        assert_eq!(padded.byte_len(), 128);
        assert_eq!(padded.pixel_offset(9, 3), Some(3 * 32 + 27));
    }

    #[test]
    fn offsets_stay_inside_the_buffer() {
        let fb = FramebufferLayout::new(7, 5, 24, PixelFormat::Rgb565).unwrap();
        for y in 0..5 {
            for x in 0..7 {
                let offset = fb.pixel_offset(x, y).unwrap();
                assert!(offset + 2 <= fb.byte_len());
            }
        }
        assert_eq!(fb.pixel_offset(7, 0), None);
        assert_eq!(fb.pixel_offset(0, 5), None);
    }

    #[test]
    fn write_pixel_checks_bounds_and_buffer() {
        let fb = FramebufferLayout::packed(2, 2, PixelFormat::Rgb888).unwrap();
        let mut buffer = [0u8; 12];
        assert!(fb.write_pixel(&mut buffer, 1, 1, (9, 8, 7)));
        assert_eq!(buffer[9..], [9, 8, 7]);
        assert!(!fb.write_pixel(&mut buffer, 2, 0, (1, 1, 1)));
        let mut short = [0u8; 10];
        assert!(!fb.write_pixel(&mut short, 1, 1, (1, 1, 1)));
    }
}
