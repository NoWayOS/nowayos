// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`RawFramebuffer`]: a draw target over raw framebuffer memory, laid out
//! as a validated [`FramebufferLayout`] says.
//!
//! This is how a UI protection domain draws into the framebuffer it shares
//! with the display PD: the same renderer that draws golden images in CI
//! draws here, and each pixel is stored in the layout's pixel format by
//! [`PixelFormat::encode`](nowayos_display::PixelFormat::encode). No
//! allocation, no `unsafe`, no panics: a pixel outside the layout - off any
//! edge, or in a row's padding - is discarded.

use core::convert::Infallible;
use core::fmt;

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::{OriginDimensions, Size};
use embedded_graphics::primitives::Rectangle;
use nowayos_display::{FrameMemory, FramebufferLayout};

/// Bytes of one run of a solid colour [`RawFramebuffer`] stores at once: a
/// fill is stored a row at a time, in runs of up to this many bytes, so a
/// screen at the monitor's own pixels is filled in a few thousand stores
/// rather than a store a pixel.
const RUN: usize = 1024;

/// A framebuffer could not be set up over the memory given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFramebufferError {
    /// The memory is shorter than the layout needs.
    TooShort {
        /// The bytes given.
        len: usize,
        /// The bytes the layout needs.
        needed: usize,
    },
}

impl fmt::Display for RawFramebufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len, needed } => write!(
                f,
                "the framebuffer memory is {len} bytes; the layout needs {needed}"
            ),
        }
    }
}

impl core::error::Error for RawFramebufferError {}

/// A draw target over framebuffer memory `M`, laid out as `layout` says:
/// ordinary bytes (`&mut [u8]`) or bytes shared with another PD
/// (`&[AtomicU8]`, or a shared-region view such as `nowayos-region`'s
/// read-write `Frame`). Colours are [`Rgb888`]; each pixel is converted to
/// the layout's format as it is stored.
///
/// ```
/// use nowayos_display::{FramebufferLayout, PixelFormat};
/// use nowayos_render_eg::RawFramebuffer;
/// use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::{PrimitiveStyle, Rectangle}};
///
/// let layout = FramebufferLayout::packed(4, 2, PixelFormat::Xrgb8888).unwrap();
/// let mut memory = [0u8; 32];
/// let mut fb = RawFramebuffer::new(layout, &mut memory[..]).unwrap();
/// Rectangle::new(Point::new(1, 1), Size::new(9, 9)) // runs off the edge
///     .into_styled(PrimitiveStyle::with_fill(Rgb888::new(0x11, 0x22, 0x33)))
///     .draw(&mut fb)
///     .unwrap();
/// // Pixel (1, 1) is at byte 16 + 4: blue, green, red, unused.
/// assert_eq!(&memory[20..24], &[0x33, 0x22, 0x11, 0]);
/// assert_eq!(&memory[..16], &[0; 16]);
/// ```
#[derive(Debug)]
pub struct RawFramebuffer<M: FrameMemory> {
    layout: FramebufferLayout,
    memory: M,
}

impl<M: FrameMemory> RawFramebuffer<M> {
    /// A framebuffer over `memory`. Bytes past the layout's
    /// [`byte_len`](FramebufferLayout::byte_len) are never written.
    ///
    /// # Errors
    ///
    /// [`RawFramebufferError::TooShort`] if `memory` is shorter than the
    /// layout needs; nothing is written.
    pub fn new(layout: FramebufferLayout, memory: M) -> Result<Self, RawFramebufferError> {
        if memory.frame_len() < layout.byte_len() {
            return Err(RawFramebufferError::TooShort {
                len: memory.frame_len(),
                needed: layout.byte_len(),
            });
        }
        Ok(Self { layout, memory })
    }

    /// The layout.
    #[must_use]
    pub const fn layout(&self) -> FramebufferLayout {
        self.layout
    }

    /// The memory back, when drawing is done.
    pub fn into_memory(self) -> M {
        self.memory
    }

    /// Stores `color` at `(x, y)`, or does nothing if that is outside the
    /// layout.
    fn store(&mut self, x: i32, y: i32, color: Rgb888) {
        if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
            self.layout
                .store_pixel(&mut self.memory, x, y, (color.r(), color.g(), color.b()));
        }
    }
}

impl<M: FrameMemory> OriginDimensions for RawFramebuffer<M> {
    fn size(&self) -> Size {
        Size::new(self.layout.width(), self.layout.height())
    }
}

impl<M: FrameMemory> DrawTarget for RawFramebuffer<M> {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.store(point.x, point.y, color);
        }
        Ok(())
    }

    /// Fills `area`, clipped to the layout, a row at a time: the colour is
    /// encoded once and stored in runs of whole pixels - the same bytes a
    /// pixel at a time would store.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let Some((left, top, width, height)) = self.inside(area) else {
            return Ok(());
        };
        let format = self.layout.format();
        let mut pixel = [0_u8; 4];
        let Some(bytes) = format
            .encode(color.r(), color.g(), color.b(), &mut pixel)
            .and_then(|len| pixel.get(..len))
        else {
            return Ok(());
        };
        // The longest run of whole pixels the buffer holds.
        let mut run = [0_u8; RUN];
        let whole = RUN
            .checked_div(bytes.len())
            .unwrap_or(0)
            .saturating_mul(bytes.len());
        let Some(run) = run.get_mut(..whole) else {
            return Ok(());
        };
        for chunk in run.chunks_exact_mut(bytes.len()) {
            chunk.copy_from_slice(bytes);
        }
        let Some(row_len) = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(bytes.len()))
        else {
            return Ok(());
        };
        for y in top..top.saturating_add(height) {
            let Some(mut offset) = self.layout.pixel_offset(left, y) else {
                continue;
            };
            let mut rest = row_len;
            while rest > 0 {
                let len = rest.min(run.len());
                let Some(part) = run.get(..len) else {
                    break;
                };
                if !self.memory.store(offset, part) {
                    break;
                }
                offset = offset.saturating_add(len);
                rest = rest.saturating_sub(len);
            }
        }
        Ok(())
    }
}

impl<M: FrameMemory> RawFramebuffer<M> {
    /// The part of `area` inside the layout, as its left column, top row,
    /// width and height; `None` if they share no pixel.
    /// Worked in `i64`, so no rectangle a caller gives can overflow it.
    fn inside(&self, area: &Rectangle) -> Option<(u32, u32, u32, u32)> {
        let left = i64::from(area.top_left.x).max(0);
        let top = i64::from(area.top_left.y).max(0);
        let right = i64::from(area.top_left.x)
            .saturating_add(i64::from(area.size.width))
            .min(i64::from(self.layout.width()));
        let bottom = i64::from(area.top_left.y)
            .saturating_add(i64::from(area.size.height))
            .min(i64::from(self.layout.height()));
        if left >= right || top >= bottom {
            return None;
        }
        Some((
            u32::try_from(left).ok()?,
            u32::try_from(top).ok()?,
            u32::try_from(right.saturating_sub(left)).ok()?,
            u32::try_from(bottom.saturating_sub(top)).ok()?,
        ))
    }
}
