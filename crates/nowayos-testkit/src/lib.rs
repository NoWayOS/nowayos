// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-side test harness for NoWayOS. Uses `std`; never ships on a device.
//!
//! * [`Framebuffer`] — an in-memory RGB framebuffer implementing
//!   `embedded-graphics`' `DrawTarget`, so any renderer can be run in a unit
//!   test or in CI without a screen.
//! * [`image`] — reading and writing PNG, the golden images' format, which
//!   GitHub and any image viewer show.
//! * [`snapshot`] — comparing a rendered frame with a golden image checked
//!   into the repo, and updating it on request
//!   (`NOWAYOS_UPDATE_SNAPSHOTS=1`).
//!
//! This is how CI proves UI rendering
//! and how a designer previews a screen without hardware.

#![forbid(unsafe_code)]
// The PNG reader parses files, so the parser lints apply here too.
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod framebuffer;
pub mod image;
pub mod snapshot;

pub use framebuffer::{Diff, Framebuffer, FramebufferError, MAX_PIXELS};
pub use image::PngError;
pub use snapshot::{SnapshotError, UPDATE_ENV, check_snapshot};

/// Storage for a glyph store on the heap, for host code and tests: 1 MiB
/// of coverage and 1,024 slots, enough for a screen of text at any scale.
/// A UI PD gives `nowayos_render_eg::Glyphs` storage in its own frame
/// instead.
#[must_use]
pub fn glyph_storage() -> (Vec<u8>, Vec<nowayos_raster::Slot>) {
    (vec![0; 1 << 20], vec![nowayos_raster::Slot::EMPTY; 1024])
}

/// Binds `$name` to a `nowayos_render_eg::Glyphs` over fresh storage
/// ([`glyph_storage`]), for a test that draws text:
/// `nowayos_testkit::glyph_store!(glyphs);` then `render(..., &mut glyphs)`.
#[macro_export]
macro_rules! glyph_store {
    ($name:ident) => {
        let (mut pixels, mut slots) = $crate::glyph_storage();
        let mut $name = nowayos_render_eg::Glyphs::new(&mut pixels, &mut slots);
    };
}
