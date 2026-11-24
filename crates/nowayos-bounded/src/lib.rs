// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixed-capacity containers for `no_std` code that must not allocate.
//!
//! NoWayOS components keep IPC queues, input events, UI trees and strings
//! that enter trusted code in **bounded, static storage**.
//! This crate provides the two containers they
//! share:
//!
//! * [`BoundedVec<T, N>`] — a vector holding at most `N` items;
//! * [`BoundedStr<N>`] — a UTF-8 string of at most `N` bytes, always valid.
//!
//! Every capacity is fixed at compile time. Nothing here allocates, and
//! nothing panics: an operation that would exceed capacity returns an error
//! (handing the rejected value back where there is one) and leaves the
//! container unchanged.
//!
//! The crate contains no `unsafe` code. That has two visible costs, both
//! deliberate: storage is always fully initialised, so `BoundedVec` needs
//! `T: Default` to create or shrink itself, and
//! [`BoundedStr::as_str`] re-checks UTF-8 on each call (cheap for the short
//! strings these containers are sized for) instead of trusting an unchecked
//! conversion.
//!
//! # Removing data
//!
//! [`BoundedStr`] zeroes the bytes it lets go of ([`BoundedStr::pop`],
//! [`BoundedStr::clear`]), and [`BoundedStr::is_zeroed`] lets a test prove
//! it: that every byte of a wiped string's storage is zero, not merely that
//! the string is empty. [`BoundedVec`] resets a
//! vacated slot to `T::default()` — which, when the default is a smaller
//! enum variant (`None` replacing a `Some(text)`), may rewrite only the
//! variant tag and leave the old payload's bytes in the container's
//! storage. For data that must not linger, such as what a person typed,
//! use [`Wipe`]: it overwrites values in place before they are let go.
//! Even then this is careful hygiene in safe code, not a zeroisation
//! guarantee against the compiler (see [`Wipe`]); secrets need a
//! purpose-built zeroising type, and do not belong in UI or kiosk state at
//! all.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod error;
mod string;
mod vec;
mod wipe;

pub use error::{CapacityError, FromUtf8Error};
pub use string::BoundedStr;
pub use vec::BoundedVec;
pub use wipe::Wipe;
