// SPDX-License-Identifier: MIT OR Apache-2.0
//! Display geometry, framebuffer pixel formats and dirty-rectangle tracking.
//!
//! The pieces every NoWayOS display and UI component shares, with no
//! dependency on a renderer:
//!
//! * [`Point`], [`Size`], [`Rect`] and [`Insets`] — screen geometry whose
//!   maths never overflows or panics. Edge calculations are done in `i64`
//!   and results that fall outside the representable range saturate.
//! * [`PixelFormat`] and [`FramebufferLayout`] — how pixels are laid out in a
//!   framebuffer, validated once so that every later offset calculation is
//!   checked.
//! * [`Placement`] and [`scale_row`] — a frame shown on a screen, at its
//!   own pixels or scaled by a whole number and centred, and converted to
//!   the screen's pixel format, a row at a time; and [`test_pattern`], the
//!   display test pattern every display PD draws first.
//! * [`DirtyRegion`] — a bounded set of rectangles that need redrawing, so a
//!   software renderer repaints only what changed.
//! * The **UI-to-display contract, v0** — [`Configure`] (version 2, which
//!   carries the display's device scale), [`Present`] and
//!   [`PresentDone`], the `nowayos-ipc` records between the UI PD and a
//!   display PD, and the two ends that check them, [`UiEnd`] and
//!   [`DisplayEnd`]. The UI owns its framebuffer, the display maps it
//!   read-only, and the UI holds no device capability.
//!   Provisional: the first display PD may revisit
//!   it.
//!
//! Coordinates follow the usual screen convention: `x` grows right, `y` grows
//! down, and a rectangle covers `x .. x + width` (right and bottom edges
//! exclusive).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod contract;
mod dirty;
mod ends;
mod geometry;
mod hash;
mod pixel;
#[cfg(kani)]
mod proofs;
mod scale;

pub use contract::{
    CONFIGURE_KIND, Configure, ContractError, MAX_DAMAGE_RECTS, MAX_DIMENSION, MAX_RECORD_LEN,
    MAX_SCALE, PRESENT_DONE_KIND, PRESENT_KIND, Present, PresentDone,
};
pub use dirty::DirtyRegion;
pub use ends::{DisplayEnd, Spans, UiEnd, UiEvent, ValidPresent};
pub use geometry::{Insets, Point, Rect, Size};
pub use hash::FrameHash;
pub use pixel::{FrameMemory, FramebufferLayout, LayoutError, PixelFormat};
pub use scale::{
    FrameChoice, KIOSK_FRAME, LEAST_SCREEN, Placement, TEST_PATTERN, TEST_PATTERN_BARS,
    device_scale, frame_for, scale_row, test_pattern,
};
