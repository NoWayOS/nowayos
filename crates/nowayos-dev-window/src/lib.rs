// SPDX-License-Identifier: MIT OR Apache-2.0
//! An application on `nowayos-ui`, running on the developer's machine, shown
//! in a browser window on that machine.
//!
//! A developer's tool, for the host only: nothing here goes on a device, and
//! no browser comes any nearer to one. A program gives [`Server::run`] its
//! [`Application`] - the same Rust that runs in its protection domains,
//! built for the host - and opens `http://localhost:8017/`. The page shows
//! each frame the application draws, and sends what is clicked and typed on
//! it back as canonical input events ([`nowayos_input::InputEvent`]), as an
//! input PD would.
//!
//! An application that is one `nowayos-ui` tree - the usual one - needs only
//! `ui::UiApplication`, and `ui::Ui` is its [`Application`] (the `ui`
//! feature, on unless turned off).
//!
//! **Time** is the window's: milliseconds from [`START`], passing as the
//! clock on the wall does. The application is told the time with every
//! batch of input and about fifty times a second between.
//!
//! **A session** can be kept in a file ([`Options::session`]): every batch
//! of input with its time. A program started again with that file plays it
//! back first, so an application rebuilt after an edit comes back to the
//! screen it was on. *Start again* on the page forgets it.
//!
//! **A notice** is text shown above the screen: a build that failed, say.
//! Whatever restarts the program posts it (`POST /notice`, empty to clear).
//!
//! **Features.** `window` is the window itself, and brings in the host's
//! frame (the test kit); `ui`, on unless turned off, adds the adapter for
//! one tree and with it the UI core and renderer. With neither, the crate is
//! the two parsers alone ([`events`], [`http`]) - what the fuzz harness
//! takes, so that no image code comes into its workspace.
//!
//! **Who may connect.** The window listens on the loopback address unless
//! told otherwise ([`Options::listen`]; in a container, every address of the
//! container, published to the host's loopback only). It answers only
//! requests that name this machine as their host, and changes nothing
//! without a header another site's page cannot send ([`http::OURS`]). All
//! of what it reads is bounded and refused if it is not exactly what the
//! page sends ([`http`], [`events`]). It has no encryption and no sign-in:
//! it is for one developer on one machine, showing synthetic data.
//!
//! Provisional.

#[cfg(feature = "window")]
pub mod bmp;
pub mod events;
pub mod http;
#[cfg(feature = "window")]
mod server;
#[cfg(feature = "ui")]
pub mod ui;

#[cfg(feature = "window")]
pub use server::{Application, Options, PORT, START, Server};
