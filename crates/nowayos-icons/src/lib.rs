// SPDX-License-Identifier: ISC
//! NoWayOS's icons: a few of Lucide's icons, drawn in advance into
//! 8-bit alpha masks at every device scale the display contract allows, so
//! the device draws an icon - tinted with whatever colour the theme gives
//! it - without ever parsing an SVG file.
//!
//! Each [`Icon`] is [`SIZE`] layout units square and has a [`Mask`] for
//! each device scale from 1 to 4 in quarter steps, found by the scale's
//! quarters: 24 pixels at scale 1 (4 quarters), 30 at 1.25, 48 at 2, 96 at
//! 4. A mask holds one coverage byte a pixel, row by row: 0 for none, 255
//! for full.
//!
//! ```
//! use nowayos_icons::{FUEL, SIZE};
//!
//! let mask = FUEL.mask(8).unwrap();
//! assert_eq!(u32::from(mask.size()), SIZE * 2);
//! assert_eq!(mask.coverage().len(), 48 * 48);
//! assert_eq!(FUEL.mask(5).unwrap().size(), 30);
//! assert!(FUEL.mask(3).is_none());
//! ```
//!
//! # Only the icons used
//!
//! Each icon is a static of its own, and nothing in NoWayOS names one: a
//! program links the icons its screens name and no other. [`ALL`] lists
//! them for tests and tools, never for a device.
//!
//! # Where the masks come from
//!
//! `tools/icon-masks`, a host tool, draws them from the SVG files in
//! `lucide/` that `icons.toml` names - each pinned by its sha256 - and
//! writes the coverage files and `src/masks/mod.rs` (`make atlases`). They
//! are checked in; `make atlases-check`, in `make ci`, draws them again and
//! fails on any difference. A mask whose coverage is not exactly its size
//! squared does not compile.
//!
//! # Licence
//!
//! The masks are made from Lucide's icons, under the ISC licence, and so is
//! this whole crate: `ISC`, whatever licence NoWayOS's own crates take
//! ([`NOTICE`]; the licence is `LICENSE-LUCIDE.txt` beside this crate's
//! `Cargo.toml`). `make image` puts that notice beside every image, and the
//! image's SBOM names this crate with its licence.

#![no_std]
#![forbid(unsafe_code)]

mod masks;

pub use masks::*;

/// An icon's size, in layout units, each way.
pub const SIZE: u32 = 24;

/// Lucide's copyright line and licence, which every copy of the icons
/// carries.
pub const NOTICE: &str = include_str!("../LICENSE-LUCIDE.txt");

/// One icon at one size: its coverage, one byte a pixel, row by row.
#[derive(Debug, PartialEq, Eq)]
pub struct Mask {
    size: u16,
    coverage: &'static [u8],
}

impl Mask {
    /// A mask `size` pixels square. Does not compile, as a static, unless
    /// `coverage` holds exactly `size * size` bytes.
    #[must_use]
    pub const fn new(size: u16, coverage: &'static [u8]) -> Self {
        let side = size as usize;
        assert!(
            matches!(side.checked_mul(side), Some(n) if n == coverage.len()),
            "a mask's coverage is its size squared"
        );
        Self { size, coverage }
    }

    /// Its size, in pixels, each way.
    #[must_use]
    pub const fn size(&self) -> u16 {
        self.size
    }

    /// Its coverage, one byte a pixel, row by row.
    #[must_use]
    pub const fn coverage(&self) -> &'static [u8] {
        self.coverage
    }

    /// The coverage of the pixel `x` across and `y` down; `None` outside.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Option<u8> {
        let size = u32::from(self.size);
        if x >= size || y >= size {
            return None;
        }
        let index = y.checked_mul(size)?.checked_add(x)?;
        self.coverage.get(usize::try_from(index).ok()?).copied()
    }
}

/// How many device scales an icon has a mask for: every quarter step from
/// 1 to 4.
pub const SCALES: usize = 13;

/// One icon: its name and a [`Mask`] for each device scale, from 1 to 4
/// in quarter steps.
#[derive(Debug, PartialEq, Eq)]
pub struct Icon {
    name: &'static str,
    masks: [Mask; SCALES],
}

impl Icon {
    /// The icon `name` with `masks`, for scales 1, 1.25 and so on to 4.
    #[must_use]
    pub const fn new(name: &'static str, masks: [Mask; SCALES]) -> Self {
        Self { name, masks }
    }

    /// Its Lucide name, as TSX names it: `fuel`, `user-check`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Its mask at a device scale of `quarters` quarter pixels a layout
    /// unit, 4 to 16 (`nowayos_display::DeviceScale::quarters`); `None`
    /// for any other.
    #[must_use]
    pub fn mask(&self, quarters: u32) -> Option<&Mask> {
        let index = usize::try_from(quarters.checked_sub(4)?).ok()?;
        self.masks.get(index)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code")]

    use super::*;

    /// Every icon has a mask at each scale, `SIZE` units square, drawn
    /// with ink inside and an empty border, and the notice is Lucide's.
    #[test]
    fn every_icon_has_a_mask_at_each_scale() {
        for icon in ALL {
            for quarters in 4..=16 {
                let mask = icon.mask(quarters).unwrap();
                assert_eq!(
                    u32::from(mask.size()) * 4,
                    SIZE * quarters,
                    "{}",
                    icon.name()
                );
                let inked = mask.coverage().iter().filter(|&&c| c > 0).count();
                assert!(inked > 0, "{} at {quarters}/4 draws nothing", icon.name());
                assert_eq!(
                    mask.at(0, 0),
                    Some(0),
                    "{}: the corner is empty",
                    icon.name()
                );
            }
            assert!(icon.mask(3).is_none() && icon.mask(17).is_none());
        }
        let names: [&str; 6] = ALL.map(Icon::name);
        assert_eq!(
            names,
            [
                "clipboard-list",
                "fuel",
                "house",
                "message-square",
                "shield-check",
                "user-check"
            ]
        );
        assert!(NOTICE.starts_with("ISC License"));
        assert_eq!(FUEL.mask(4).unwrap().at(24, 0), None);
    }
}
