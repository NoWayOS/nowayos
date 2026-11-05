// SPDX-License-Identifier: MIT OR Apache-2.0
//! The device scale: how many pixels make one layout unit, from 1 to 4 in
//! quarter steps ([`DeviceScale`]).
//!
//! A screen is laid out in layout units and drawn at its own pixels: at
//! scale 2 a 1920 x 1080 screen is 960 x 540 units, at 4 a 3840 x 2160 one
//! is too, and at 2.5 a 2560 x 1440 one is 1024 x 576. A length is turned
//! into pixels once, rounded to the nearest whole pixel
//! ([`DeviceScale::px`]), so every edge falls on a whole pixel at every
//! scale and nothing is ever blurred; text is drawn at its size times the
//! scale from the glyph outlines ([`DeviceScale::size64`]), never an image
//! enlarged.

use crate::Size;

/// The least layout a screen is given when no product says otherwise: the
/// 960 x 540 units the Pi 5's 1920 x 1080 window has at scale 2, which a
/// 3840 x 2160 screen also has at scale 4.
pub const DEFAULT_LAYOUT: Size = Size::new(960, 540);

/// How many pixels make one layout unit: 1 to 4, in quarter steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceScale(u8);

impl DeviceScale {
    /// One pixel a unit.
    pub const ONE: Self = Self(4);
    /// Two pixels a unit: the Pi 5's 1920 x 1080 window.
    pub const TWO: Self = Self(8);
    /// The largest: four pixels a unit, a 3840 x 2160 screen.
    pub const MAX: Self = Self(16);
    /// The least: one pixel a unit.
    pub const MIN: Self = Self::ONE;

    /// The scale of `quarters` quarter pixels a unit: 4 is 1, 6 is 1.5, 16
    /// is 4. `None` below 4 or above 16.
    #[must_use]
    pub const fn from_quarters(quarters: u32) -> Option<Self> {
        if quarters >= 4 && quarters <= 16 {
            Some(Self(quarters as u8))
        } else {
            None
        }
    }

    /// The scale of `whole` pixels a unit, 1 to 4.
    #[must_use]
    pub const fn whole(whole: u32) -> Option<Self> {
        match whole.checked_mul(4) {
            Some(quarters) => Self::from_quarters(quarters),
            None => None,
        }
    }

    /// Quarter pixels a unit: 4 to 16.
    #[must_use]
    pub const fn quarters(self) -> u32 {
        self.0 as u32
    }

    /// Whether it is a whole number of pixels a unit.
    #[must_use]
    pub const fn is_whole(self) -> bool {
        self.0.is_multiple_of(4)
    }

    /// `units` layout units in pixels, to the nearest whole pixel (a half
    /// rounds up).
    #[must_use]
    pub const fn px(self, units: u32) -> u32 {
        let pixels = self.px_u64(units as u64);
        if pixels > u32::MAX as u64 {
            u32::MAX
        } else {
            pixels as u32
        }
    }

    /// `units` layout units, which may be negative, in pixels, to the
    /// nearest whole pixel (a half rounds away from zero).
    #[must_use]
    pub const fn px_signed(self, units: i64) -> i64 {
        let magnitude = self.px_u64(units.unsigned_abs());
        let magnitude = if magnitude > i64::MAX as u64 {
            i64::MAX
        } else {
            magnitude as i64
        };
        if units < 0 {
            magnitude.saturating_neg()
        } else {
            magnitude
        }
    }

    const fn px_u64(self, units: u64) -> u64 {
        units.saturating_mul(self.0 as u64).saturating_add(2) / 4
    }

    /// A size of `units` layout units per em in 1/64 pixel per em: exact,
    /// as a quarter of a 64th is a 256th.
    #[must_use]
    pub const fn size64(self, units: u32) -> u32 {
        units.saturating_mul(16).saturating_mul(self.0 as u32)
    }

    /// A screen of `pixels` in layout units: each side divided by the
    /// scale, rounded down.
    #[must_use]
    pub const fn layout_of(self, pixels: Size) -> Size {
        Size::new(self.units_of(pixels.width), self.units_of(pixels.height))
    }
}

impl DeviceScale {
    /// `pixels` in whole layout units, rounded down.
    const fn units_of(self, pixels: u32) -> u32 {
        match (pixels as u64).saturating_mul(4).checked_div(self.0 as u64) {
            Some(units) if units <= u32::MAX as u64 => units as u32,
            _ => 0,
        }
    }
}

impl core::fmt::Display for DeviceScale {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let whole = self.0 / 4;
        match self.0.checked_rem(4).unwrap_or(0) {
            0 => write!(f, "{whole}"),
            1 => write!(f, "{whole}.25"),
            2 => write!(f, "{whole}.5"),
            _ => write!(f, "{whole}.75"),
        }
    }
}

/// The device scale for a screen of `pixels` whose product wants at least
/// `least` layout units: the largest quarter step from 1 to 4 that leaves
/// that much, or 1 when even 1 does not.
///
/// ```
/// use nowayos_display::{DEFAULT_LAYOUT, DeviceScale, Size, scale_for};
///
/// assert_eq!(scale_for(Size::new(1920, 1080), DEFAULT_LAYOUT), DeviceScale::TWO);
/// assert_eq!(scale_for(Size::new(3840, 2160), DEFAULT_LAYOUT), DeviceScale::MAX);
/// assert_eq!(scale_for(Size::new(2560, 1440), DEFAULT_LAYOUT).to_string(), "2.5");
/// assert_eq!(scale_for(Size::new(800, 480), DEFAULT_LAYOUT), DeviceScale::ONE);
/// ```
#[must_use]
pub fn scale_for(pixels: Size, least: Size) -> DeviceScale {
    (4..=16)
        .rev()
        .filter_map(DeviceScale::from_quarters)
        .find(|scale| {
            let layout = scale.layout_of(pixels);
            layout.width >= least.width && layout.height >= least.height
        })
        .unwrap_or(DeviceScale::ONE)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn quarter_steps_from_1_to_4_and_no_others() {
        assert_eq!(DeviceScale::from_quarters(3), None);
        assert_eq!(DeviceScale::from_quarters(17), None);
        assert_eq!(DeviceScale::whole(2), Some(DeviceScale::TWO));
        assert_eq!(DeviceScale::whole(5), None);
        let names: Vec<String> = (4..=16)
            .filter_map(DeviceScale::from_quarters)
            .map(|scale| scale.to_string())
            .collect();
        assert_eq!(names.first().unwrap(), "1");
        assert_eq!(names[1], "1.25");
        assert_eq!(names[2], "1.5");
        assert_eq!(names[3], "1.75");
        assert_eq!(names.last().unwrap(), "4");
        assert_eq!(DeviceScale::TWO.size64(18), 18 * 64 * 2);
    }

    proptest! {
        /// Every length is the nearest whole number of pixels, a half
        /// rounding up; a whole scale is exact; and two lengths side by
        /// side are within a pixel of their sum: so edges laid out length
        /// by length fall on whole pixels and never drift more than a
        /// pixel a length from where the scale puts them.
        #[test]
        fn lengths_are_the_nearest_whole_pixels(
            quarters in 4_u32..=16,
            a in 0_u32..1_000_000,
            b in 0_u32..1_000_000,
        ) {
            let scale = DeviceScale::from_quarters(quarters).unwrap();
            let exact = |units: u32| f64::from(units) * f64::from(quarters) / 4.0;
            let px = scale.px(a);
            prop_assert!((f64::from(px) - exact(a)).abs() <= 0.5, "{} at {}", a, scale);
            if scale.is_whole() {
                prop_assert_eq!(px, a * quarters / 4);
            }
            let together = i64::from(scale.px(a + b));
            let apart = i64::from(scale.px(a)) + i64::from(scale.px(b));
            prop_assert!((together - apart).abs() <= 1);
            prop_assert_eq!(scale.px_signed(-i64::from(a)), -i64::from(px));
            prop_assert!(scale.layout_of(Size::new(px, px)).width <= a + 1);
        }
    }
}
