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
