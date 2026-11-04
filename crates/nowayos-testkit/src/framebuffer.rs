// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Framebuffer`]: an in-memory draw target.

use core::convert::Infallible;
use std::fmt;

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::Point;

/// The largest framebuffer this kit will allocate: 16 777 216 pixels
/// (4096 × 4096). Keeps a malformed golden file from exhausting memory.
pub const MAX_PIXELS: u64 = 1 << 24;

/// A framebuffer could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferError {
    /// Width × height exceeds [`MAX_PIXELS`].
    TooLarge {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
}

impl fmt::Display for FramebufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { width, height } => {
                write!(f, "{width}x{height} exceeds the {MAX_PIXELS}-pixel limit")
            }
        }
    }
}

impl std::error::Error for FramebufferError {}

/// An RGB888 framebuffer in memory. Pixels drawn outside it are ignored, as
/// on a real display.
///
/// ```
/// use nowayos_testkit::Framebuffer;
/// use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::{PrimitiveStyle, Rectangle}};
///
/// let mut fb = Framebuffer::new(8, 8).unwrap();
/// Rectangle::new(Point::new(2, 2), Size::new(3, 3))
///     .into_styled(PrimitiveStyle::with_fill(Rgb888::RED))
///     .draw(&mut fb)
///     .unwrap();
/// assert_eq!(fb.count(Rgb888::RED), 9);
/// assert_eq!(fb.pixel(2, 2), Some(Rgb888::RED));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<Rgb888>,
}

/// How two framebuffers differ. See [`Framebuffer::diff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diff {
    /// The sizes differ (then no pixels are compared).
    pub size_mismatch: bool,
    /// Number of pixels that differ.
    pub differing: usize,
    /// The first differing pixel, in reading order.
    pub first: Option<(u32, u32)>,
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.size_mismatch {
            return f.write_str("sizes differ");
        }
        write!(f, "{} pixels differ", self.differing)?;
        if let Some((x, y)) = self.first {
            write!(f, ", first at ({x}, {y})")?;
        }
        Ok(())
    }
}

impl Framebuffer {
    /// A black framebuffer of `width` × `height`.
    ///
    /// # Errors
    ///
    /// [`FramebufferError::TooLarge`] beyond [`MAX_PIXELS`].
    pub fn new(width: u32, height: u32) -> Result<Self, FramebufferError> {
        Self::filled(width, height, Rgb888::BLACK)
    }

    /// A framebuffer of `width` × `height` filled with `color`.
    ///
    /// # Errors
    ///
    /// [`FramebufferError::TooLarge`] beyond [`MAX_PIXELS`].
    pub fn filled(width: u32, height: u32, color: Rgb888) -> Result<Self, FramebufferError> {
        let count = u64::from(width).saturating_mul(u64::from(height));
        if count > MAX_PIXELS {
            return Err(FramebufferError::TooLarge { width, height });
        }
        let count =
            usize::try_from(count).map_err(|_| FramebufferError::TooLarge { width, height })?;
        Ok(Self {
            width,
            height,
            pixels: vec![color; count],
        })
    }

    /// Builds a framebuffer from row-major pixels.
    ///
    /// Returns `None` if `pixels` does not hold exactly `width × height`
    /// pixels, or that exceeds [`MAX_PIXELS`].
    #[must_use]
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<Rgb888>) -> Option<Self> {
        let count = u64::from(width).saturating_mul(u64::from(height));
        (count <= MAX_PIXELS && u64::try_from(pixels.len()).ok() == Some(count)).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Moves the pixels in `area`, clipped to the framebuffer, `dx` right
    /// and `dy` down: what that carries out of the area is dropped, and
    /// what it leaves behind keeps the pixels it held - as a UI PD moves
    /// what a scroll moved.
    pub fn move_area(&mut self, area: &embedded_graphics::primitives::Rectangle, dx: i64, dy: i64) {
        let left = i64::from(area.top_left.x).max(0);
        let top = i64::from(area.top_left.y).max(0);
        let right = i64::from(area.top_left.x)
            .saturating_add(i64::from(area.size.width))
            .min(i64::from(self.width));
        let bottom = i64::from(area.top_left.y)
            .saturating_add(i64::from(area.size.height))
            .min(i64::from(self.height));
        let before = self.pixels.clone();
        let width = i64::from(self.width);
        let at = |x: i64, y: i64| usize::try_from(y.saturating_mul(width).saturating_add(x)).ok();
        for y in top..bottom {
            for x in left..right {
                let (from_x, from_y) = (x.saturating_sub(dx), y.saturating_sub(dy));
                let inside = (left..right).contains(&from_x) && (top..bottom).contains(&from_y);
                if inside
                    && let (Some(to), Some(from)) = (at(x, y), at(from_x, from_y))
                    && let (Some(pixel), Some(slot)) =
                        (before.get(from).copied(), self.pixels.get_mut(to))
                {
                    *slot = pixel;
                }
            }
        }
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

    /// The pixels, row by row.
    #[must_use]
    pub fn pixels(&self) -> &[Rgb888] {
        &self.pixels
    }

    /// The pixel at `(x, y)`, if inside.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb888> {
        self.index(x, y).and_then(|i| self.pixels.get(i).copied())
    }

    /// How many pixels are exactly `color`.
    #[must_use]
    pub fn count(&self, color: Rgb888) -> usize {
        self.pixels.iter().filter(|&&p| p == color).count()
    }

    /// `None` if the two are identical; otherwise how they differ.
    #[must_use]
    pub fn diff(&self, other: &Self) -> Option<Diff> {
        if self.width != other.width || self.height != other.height {
            return Some(Diff {
                size_mismatch: true,
                differing: 0,
                first: None,
            });
        }
        let mut differing = 0usize;
        let mut first = None;
        for (index, (a, b)) in self.pixels.iter().zip(&other.pixels).enumerate() {
            if a != b {
                differing = differing.saturating_add(1);
                if first.is_none() {
                    first = self.coordinates(index);
                }
            }
        }
        (differing > 0).then_some(Diff {
            size_mismatch: false,
            differing,
            first,
        })
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = u64::from(y)
            .checked_mul(u64::from(self.width))?
            .checked_add(u64::from(x))?;
        usize::try_from(index).ok()
    }

    fn coordinates(&self, index: usize) -> Option<(u32, u32)> {
        let index = u32::try_from(index).ok()?;
        Some((
            index.checked_rem(self.width)?,
            index.checked_div(self.width)?,
        ))
    }

    pub(crate) fn set(&mut self, x: u32, y: u32, color: Rgb888) {
        if let Some(pixel) = self.index(x, y).and_then(|i| self.pixels.get_mut(i)) {
            *pixel = color;
        }
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(Point { x, y }, color) in pixels {
            if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
                self.set(x, y, color);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn ignores_pixels_outside() {
        let mut fb = Framebuffer::new(2, 2).unwrap();
        fb.draw_iter([
            Pixel(Point::new(-1, 0), Rgb888::RED),
            Pixel(Point::new(0, 2), Rgb888::RED),
            Pixel(Point::new(1, 1), Rgb888::GREEN),
        ])
        .unwrap();
        assert_eq!(fb.count(Rgb888::RED), 0);
        assert_eq!(fb.pixel(1, 1), Some(Rgb888::GREEN));
        assert_eq!(fb.pixel(2, 0), None);
    }

    #[test]
    fn size_limit() {
        assert_eq!(
            Framebuffer::new(4097, 4096),
            Err(FramebufferError::TooLarge {
                width: 4097,
                height: 4096
            })
        );
        assert!(Framebuffer::new(0, 0).unwrap().pixels().is_empty());
    }

    #[test]
    fn diff_reports_count_and_first() {
        let a = Framebuffer::new(3, 3).unwrap();
        let mut b = a.clone();
        assert_eq!(a.diff(&b), None);
        b.set(2, 1, Rgb888::WHITE);
        b.set(0, 2, Rgb888::WHITE);
        assert_eq!(
            a.diff(&b),
            Some(Diff {
                size_mismatch: false,
                differing: 2,
                first: Some((2, 1))
            })
        );
        let c = Framebuffer::new(3, 4).unwrap();
        assert!(a.diff(&c).unwrap().size_mismatch);
    }

    #[test]
    fn from_pixels_checks_the_count() {
        assert!(Framebuffer::from_pixels(2, 2, vec![Rgb888::BLACK; 4]).is_some());
        assert!(Framebuffer::from_pixels(2, 2, vec![Rgb888::BLACK; 5]).is_none());
    }
}
