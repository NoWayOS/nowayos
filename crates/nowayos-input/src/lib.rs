// SPDX-License-Identifier: MIT OR Apache-2.0
//! Canonical input events — the only input an application ever sees.
//!
//! Keyboards, mice and touch controllers are untrusted peripherals, and the
//! transport they arrive over will change: USB during development, perhaps
//! an external sanitising microcontroller on hardened hardware. So applications never parse USB HID, PS/2
//! or any device protocol. An input protection domain turns whatever the
//! device sends into a small, bounded [`InputEvent`], and that is all the UI
//! receives (Architecture → Stable Messages).
//!
//! This crate provides:
//!
//! * [`InputEvent`], [`KeyCode`] and [`TouchPhase`] — the canonical events,
//!   with a fixed 12-byte wire encoding ([`nowayos_ipc::Message`]) that is
//!   validated field by field on receipt;
//! * [`InputLimits`] — validation and normalisation against the declared
//!   screen size and the number of buttons and touch points;
//! * [`PointerTracker`] — relative pointer motion to clamped absolute
//!   positions;
//! * [`RateLimiter`] — a token bucket that bounds the event rate, driven by
//!   ticks the caller supplies (this crate has no clock);
//! * [`keymap`] — a minimal US-English key-to-character mapping for
//!   bring-up;
//! * [`evdev`] — virtio-input's Linux evdev events turned into canonical
//!   events, dropping and counting what it does not know;
//! * [`broker`] — the input broker's decisions: each source's events
//!   normalised against the screen and held to the source's own rate;
//! * [`terminal`] — the bytes a terminal sends for the keys typed in it
//!   turned into key presses: a development route from a serial line,
//!   dropping and counting what it does not know.
//!
//! Provisional: this API may change as more input devices arrive.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

pub mod broker;
pub mod evdev;
mod event;
pub mod keymap;
mod limits;
#[cfg(kani)]
mod proofs;
mod rate;
pub mod terminal;

pub use event::{INPUT_EVENT_KIND, InputEvent, KeyCode, TouchPhase};
pub use limits::{InputError, InputLimits, PointerTracker};
pub use rate::RateLimiter;
