// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed, bounded messages between protection domains.
//!
//! Protection domains (PDs) on Microkit exchange data through shared memory
//! regions signalled by notifications.
//! This crate is the typed layer on top, following the
//! message rules: messages are **explicit,
//! bounded, versioned and validated on receipt**, and no object graphs cross
//! a PD boundary.
//!
//! It has two halves:
//!
//! * **The wire format** — [`Message`], [`encode`], [`decode`]. A message is a
//!   fixed-size, plain-data record behind an 8-byte header naming its kind,
//!   layout version and payload length. Payloads are read and written with
//!   the bounds-checked [`Reader`] and [`Writer`]. Decoding never panics:
//!   every malformed input is a typed [`DecodeError`].
//! * **The ring** — [`Producer`] and [`Consumer`], a single-producer,
//!   single-consumer queue of fixed-size slots over shared memory, seen by
//!   each side through its own view ([`ProducerMemory`],
//!   [`ConsumerMemory`]). The consumer side enforces
//!   **copy-then-validate**: a slot is copied out of shared memory into
//!   private memory *before* anything looks at it, so the other PD cannot
//!   change a message between its check and its use (a double-fetch bug).
//!
//! # Wire layout
//!
//! All integers are little-endian.
//!
//! | Offset | Size | Field | Meaning |
//! | --- | --- | --- | --- |
//! | 0 | 2 | `kind` | Message type, unique on its channel |
//! | 2 | 2 | `version` | Layout version of that type |
//! | 4 | 2 | `payload_len` | Payload bytes that follow |
//! | 6 | 2 | reserved | Must be zero |
//! | 8 | `payload_len` | payload | Exactly [`Message::PAYLOAD_LEN`] bytes |
//!
//! The header is 8 bytes so a payload in an 8-byte-aligned slot starts
//! 8-byte aligned; the reserved field is checked, not ignored, so it can be
//! given a meaning later without old readers misreading new messages.
//!
//! Kind numbers are allocated per channel protocol, by the crate that
//! defines the channel's messages (for example `nowayos-input` for the input
//! channel). A consumer rejects a record whose kind or version it does not
//! expect.
//!
//! # Status
//!
//! Provisional: this API may change, including whether NoWayOS adopts
//! the seL4 Device Driver Framework's queue layout as-is (an open question). The views of real Microkit regions
//! need `unsafe` and live in an approved-`unsafe` crate (`nowayos-region`
//! in the `pds` workspace), not here; this crate has none.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod codec;
mod error;
#[cfg(kani)]
mod proofs;
mod ring;
mod wire;

pub use codec::{Reader, Writer};
pub use error::{DecodeError, EncodeError, RecvError, RingError, SendError};
pub use ring::{
    Consumer, ConsumerMemory, EpochChange, EpochWatch, InMemoryRing, MAX_CAPACITY, Producer,
    ProducerMemory, Record, RingMemory,
};
pub use wire::{
    HEADER_LEN, Header, MAX_PAYLOAD_LEN, Message, decode, decode_exact, encode, encoded_len,
};
