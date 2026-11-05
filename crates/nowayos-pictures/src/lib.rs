// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pictures - photographs and illustrations - converted at build time
//! (ADR-0012): the device parses no image file. `tools/pictures` decodes
//! each PNG or JPEG a crate's `pictures.toml` pins, resamples it to each
//! device scale the product uses and writes its pixels beside a generated
//! module of statics ([`Picture`]), one a picture, so a program links only
//! the pictures its screens name.
//!
//! A picture has a size in layout units and, for each scale it was made
//! at, [`Pixels`]: premultiplied RGBA, four bytes a pixel, row by row, the
//! picture's size at that scale to the nearest whole pixel. Drawn at
//! exactly that size it is copied; at any other - a box it is fitted to
//! ([`Fit`]), a scale it was not made at - it is sampled from the nearest
//! larger [`Pixels`] it has ([`Picture::best`], [`Pixels::sample`]).
//!
//! ```
//! use nowayos_pictures::{Fit, samples};
//!
//! let landscape = &samples::LANDSCAPE;
//! assert_eq!((landscape.width(), landscape.height()), (160, 100));
//! let at_two = landscape.best(320, 200);
//! assert_eq!((at_two.quarters(), at_two.width(), at_two.height()), (8, 320, 200));
//! // Fitted into a square box, contained: as wide as the box, centred.
//! let placed = Fit::Contain.place((320, 200), (100, 100));
//! assert_eq!((placed.left, placed.top, placed.width, placed.height), (0, 18, 100, 63));
//! ```
//!
//! # Where the pixels come from
//!
//! `make atlases` runs `tools/pictures` on every crate whose
//! `pictures.toml` it names, and `make atlases-check`, in `make ci`, makes
//! them again and fails on any difference. A [`Pixels`] whose bytes are not
//! exactly its size's does not compile, nor does a [`Picture`] whose pixels
//! are not its size at their scale.

#![no_std]
#![forbid(unsafe_code)]

// The generated samples name this crate as a product's generated module
// would.
extern crate self as nowayos_pictures;

pub mod samples;

#[cfg(test)]
mod tests;

/// A picture at one device scale: premultiplied RGBA, four bytes a pixel,
/// row by row.
#[derive(Debug)]
pub struct Pixels {
    quarters: u8,
    width: u16,
    height: u16,
    rgba: &'static [u8],
}

impl Pixels {
    /// `width` x `height` pixels made at `quarters` quarter pixels a layout
    /// unit. Does not compile, as a static, unless `rgba` holds exactly four
    /// bytes a pixel, the scale is 4 to 16 quarters and neither side is 0.
    #[must_use]
    pub const fn new(quarters: u8, width: u16, height: u16, rgba: &'static [u8]) -> Self {
        assert!(
            quarters >= 4 && quarters <= 16,
            "a scale is 4 to 16 quarters"
        );
        assert!(width > 0 && height > 0, "a picture has pixels");
        let pixels = (width as usize).saturating_mul(height as usize);
        assert!(
            matches!(pixels.checked_mul(4), Some(n) if n == rgba.len()),
            "a picture's pixels are four bytes each"
        );
        Self {
            quarters,
            width,
            height,
            rgba,
        }
    }

    /// The scale it was made at, in quarter pixels a layout unit.
    #[must_use]
    pub const fn quarters(&self) -> u32 {
        self.quarters as u32
    }

    /// Its width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width as u32
    }

    /// Its height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height as u32
    }

    /// Its bytes: premultiplied red, green, blue and alpha, a pixel at a
    /// time, row by row.
    #[must_use]
    pub const fn rgba(&self) -> &'static [u8] {
        self.rgba
    }

    /// Its pixels, borrowed ([`View`]).
    #[must_use]
    pub const fn view(&self) -> View<'static> {
        View {
            width: self.width as u32,
            height: self.height as u32,
            rgba: self.rgba,
        }
    }

    /// The pixel at `x`, `y`, premultiplied: `None` outside.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.view().at(x, y)
    }

    /// Pixel `x`, `y` of the picture drawn `width` x `height` pixels, as
    /// [`View::sample`].
    #[must_use]
    pub fn sample(&self, x: u32, y: u32, width: u32, height: u32) -> [u8; 4] {
        self.view().sample(x, y, width, height)
    }
}

/// Pixels borrowed for drawing: a picture's [`Pixels`], or one decoded at
/// run time (#10084) and held in memory of the program's own - premultiplied
/// RGBA, four bytes a pixel, row by row.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
}

impl<'a> View<'a> {
    /// `width` x `height` pixels in `rgba`: `None` unless it holds exactly
    /// four bytes a pixel and neither side is 0.
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: &'a [u8]) -> Option<Self> {
        let bytes = u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(4)?;
        (width > 0 && height > 0 && u64::try_from(rgba.len()).ok()? == bytes).then_some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Its width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Its height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The pixel at `x`, `y`, premultiplied: `None` outside.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width() || y >= self.height() {
            return None;
        }
        let start = usize::try_from(
            u64::from(y)
                .checked_mul(u64::from(self.width()))?
                .checked_add(u64::from(x))?
                .checked_mul(4)?,
        )
        .ok()?;
        let pixel = self.rgba.get(start..start.checked_add(4)?)?;
        pixel.try_into().ok()
    }

    /// Pixel `x`, `y` of the pixels drawn `width` x `height` pixels,
    /// premultiplied: exactly its own pixel when that is its size, else
    /// sampled between the four nearest of its own, each weighted by how
    /// near it lies (bilinear, in integers, so every machine gives the same
    /// bytes). Fully transparent outside the drawing or for an empty one.
    #[must_use]
    pub fn sample(&self, x: u32, y: u32, width: u32, height: u32) -> [u8; 4] {
        if x >= width || y >= height {
            return [0; 4];
        }
        if width == self.width() && height == self.height() {
            return self.at(x, y).unwrap_or([0; 4]);
        }
        let (x0, x1, fx) = axis(x, width, self.width());
        let (y0, y1, fy) = axis(y, height, self.height());
        let corner = |x, y| self.at(x, y).unwrap_or([0; 4]);
        let (a, b, c, d) = (
            corner(x0, y0),
            corner(x1, y0),
            corner(x0, y1),
            corner(x1, y1),
        );
        let mut out = [0; 4];
        for (channel, value) in out.iter_mut().enumerate() {
            let get = |pixel: [u8; 4]| u64::from(pixel.get(channel).copied().unwrap_or(0));
            let top = get(a)
                .saturating_mul(256_u64.saturating_sub(fx))
                .saturating_add(get(b).saturating_mul(fx));
            let bottom = get(c)
                .saturating_mul(256_u64.saturating_sub(fx))
                .saturating_add(get(d).saturating_mul(fx));
            let both = top
                .saturating_mul(256_u64.saturating_sub(fy))
                .saturating_add(bottom.saturating_mul(fy))
                .saturating_add(32_768);
            *value = u8::try_from(both.wrapping_shr(16)).unwrap_or(u8::MAX);
        }
        out
    }
}

/// Where along one axis of `source` pixels pixel `at` of `drawn` falls:
/// the two source pixels either side of its centre and the second's
/// weight, 0 to 256.
fn axis(at: u32, drawn: u32, source: u32) -> (u32, u32, u64) {
    // The centre of pixel `at` in the source, in 1/256 pixel, less half a
    // pixel: (at + 1/2) * source / drawn - 1/2.
    let centre = (u64::from(at).saturating_mul(2).saturating_add(1))
        .saturating_mul(u64::from(source))
        .saturating_mul(128)
        .checked_div(u64::from(drawn))
        .unwrap_or(0);
    let last = u64::from(source.saturating_sub(1)).saturating_mul(256);
    let position = centre.saturating_sub(128).min(last);
    let first = u32::try_from(position.wrapping_shr(8)).unwrap_or(0);
    let second = first.saturating_add(1).min(source.saturating_sub(1));
    (first, second, position & 0xFF)
}

/// A picture: its size in layout units and its pixels at each device
/// scale it was made at, in order of scale. Two pictures are equal when
/// they are the same picture, never by comparing their pixels.
#[derive(Debug)]
pub struct Picture {
    width: u32,
    height: u32,
    pixels: &'static [Pixels],
}

impl Picture {
    /// A picture `width` x `height` layout units with `pixels`. Does not
    /// compile, as a static, unless it has pixels, in rising order of scale,
    /// each its size at its scale to the nearest whole pixel (a half rounds
    /// up).
    #[must_use]
    pub const fn new(width: u32, height: u32, pixels: &'static [Pixels]) -> Self {
        assert!(!pixels.is_empty(), "a picture has pixels at some scale");
        let mut rest = pixels;
        let mut last = 0;
        while let [made, others @ ..] = rest {
            assert!(made.quarters > last, "a picture's pixels rise in scale");
            last = made.quarters;
            assert!(
                made.width as u64 == scaled(width, made.quarters)
                    && made.height as u64 == scaled(height, made.quarters),
                "a picture's pixels are its size at their scale"
            );
            rest = others;
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Its width, in layout units.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Its height, in layout units.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Its pixels at every scale it was made at, in rising order.
    #[must_use]
    pub const fn pixels(&self) -> &'static [Pixels] {
        self.pixels
    }

    /// The pixels to draw it from at `width` x `height` pixels: those of
    /// exactly that size if it has them; else the smallest at least that
    /// large each way, which sampling only shrinks; else its largest.
    #[must_use]
    pub fn best(&self, width: u32, height: u32) -> &'static Pixels {
        let pixels = self.pixels;
        let exact = pixels
            .iter()
            .find(|made| made.width() == width && made.height() == height);
        let larger = || {
            pixels
                .iter()
                .find(|made| made.width() >= width && made.height() >= height)
        };
        match exact.or_else(larger).or_else(|| pixels.last()) {
            Some(made) => made,
            // `new` refuses a picture without pixels.
            None => &EMPTY,
        }
    }
}

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self, other) || core::ptr::eq(self.pixels, other.pixels)
    }
}

impl Eq for Picture {}

static EMPTY: Pixels = Pixels {
    quarters: 4,
    width: 0,
    height: 0,
    rgba: &[],
};

/// `units` layout units at `quarters`, to the nearest whole pixel.
const fn scaled(units: u32, quarters: u8) -> u64 {
    (units as u64)
        .saturating_mul(quarters as u64)
        .saturating_add(2)
        / 4
}

/// How a picture fills a box of another shape: CSS's `object-fit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Stretched to the box, its shape lost: CSS's default.
    #[default]
    Fill,
    /// As large as fits inside the box, its shape kept, centred: the box
    /// may show beneath it on two sides.
    Contain,
    /// As small as covers the box, its shape kept, centred: it is cut on
    /// two sides.
    Cover,
}

/// Where a picture is drawn in a box: from its top-left corner, which may
/// lie outside the box for [`Fit::Cover`], `width` x `height` pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// From the box's left edge, in pixels.
    pub left: i64,
    /// From the box's top edge, in pixels.
    pub top: i64,
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
}

impl Fit {
    /// Where a picture `picture` pixels (width, height) is drawn in a box
    /// `area` pixels: each side to the nearest whole pixel, centred (a half
    /// pixel to the left or top). Nothing for an empty picture or box.
    #[must_use]
    pub fn place(self, picture: (u32, u32), area: (u32, u32)) -> Placed {
        let ((pw, ph), (aw, ah)) = (picture, area);
        let nothing = Placed {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        };
        if pw == 0 || ph == 0 || aw == 0 || ah == 0 {
            return nothing;
        }
        // Whether the picture is wider for its height than the box is.
        let wider = u64::from(pw).saturating_mul(u64::from(ah))
            > u64::from(aw).saturating_mul(u64::from(ph));
        let side = |numerator: u32, of: u32, denominator: u32| {
            let exact = u64::from(numerator)
                .saturating_mul(u64::from(of))
                .saturating_mul(2)
                .checked_div(u64::from(denominator))
                .unwrap_or(0);
            u32::try_from(exact.saturating_add(1) / 2).unwrap_or(u32::MAX)
        };
        let (width, height) = match self {
            Self::Fill => (aw, ah),
            // As wide as the box, or as high.
            Self::Contain if wider => (aw, side(ph, aw, pw)),
            Self::Contain => (side(pw, ah, ph), ah),
            Self::Cover if wider => (side(pw, ah, ph), ah),
            Self::Cover => (aw, side(ph, aw, pw)),
        };
        let centre = |area: u32, drawn: u32| {
            (i64::from(area).saturating_sub(i64::from(drawn))).div_euclid(2)
        };
        Placed {
            left: centre(aw, width),
            top: centre(ah, height),
            width,
            height,
        }
    }
}
