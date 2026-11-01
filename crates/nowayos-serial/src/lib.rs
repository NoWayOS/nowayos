// SPDX-License-Identifier: MIT OR Apache-2.0
//! The byte stream between a serial driver PD and its one client.
//!
//! A serial device - a UART, a virtio console - carries bytes, not
//! messages. Its driver PD and its client pass them as [`Chunk`]s on
//! `nowayos-ipc` rings: up to [`CHUNK_LEN`] bytes each, in order. The
//! **transmit ring** carries the client's bytes to the driver, which sends
//! them; the **receive ring** carries what the device received to the
//! client. A system gives a driver only the rings of the directions it uses,
//! so a one-way link has one ring, and the other direction has no memory to
//! travel through.
//!
//! The chunks are bytes, not records: a boundary between two chunks means
//! nothing, and the client frames its own data. The driver never looks
//! inside.
//!
//! Both ends use the ring geometry here: [`RING_SLOTS`] slots of
//! [`RING_SLOT`] bytes, which fit one 4 KiB region for the messages (with
//! `nowayos-region`'s ring layout) and one for the control.
//!
//! # Wire layout of a chunk
//!
//! A `nowayos-ipc` record of kind [`Chunk::KIND`], version 1, whose payload
//! is a little-endian `u16` length, from 1 to [`CHUNK_LEN`], then
//! [`CHUNK_LEN`] bytes: the chunk's bytes, then zeros. A length out of range
//! or padding that is not zero is refused, so every chunk has one encoding.
//!
//! Provisional: for the virtio console driver in QEMU, either way
//! or one way only.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use nowayos_ipc::{DecodeError, EncodeError, HEADER_LEN, Message, Reader, Writer};

/// The most bytes one chunk carries.
pub const CHUNK_LEN: usize = 118;

/// Slots in each ring.
pub const RING_SLOTS: usize = 16;

/// Bytes in each slot: one record, header and payload.
pub const RING_SLOT: usize = 128;

// A chunk's record fills a slot exactly.
const _: () = assert!(HEADER_LEN + 2 + CHUNK_LEN == RING_SLOT);

/// Up to [`CHUNK_LEN`] bytes of the stream, at least one.
#[derive(Clone, PartialEq, Eq)]
pub struct Chunk {
    /// From 1 to `CHUNK_LEN`.
    len: usize,
    bytes: [u8; CHUNK_LEN],
}

impl Chunk {
    /// A chunk of `bytes`, or `None` if there are none or more than
    /// [`CHUNK_LEN`].
    #[must_use]
    pub fn new(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        let mut chunk = Self {
            len: bytes.len(),
            bytes: [0; CHUNK_LEN],
        };
        chunk.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(chunk)
    }

    /// The chunk's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

impl core::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Chunk").field("len", &self.len).finish()
    }
}

impl Message for Chunk {
    const KIND: u16 = 1;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 2 + CHUNK_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let len =
            u16::try_from(self.len).map_err(|_| EncodeError::InvalidValue { field: "len" })?;
        out.write_u16(len)?;
        out.write_bytes(self.bytes())?;
        out.write_zeros(CHUNK_LEN.saturating_sub(self.len))
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let len = usize::from(input.read_u16()?);
        if len == 0 || len > CHUNK_LEN {
            return Err(DecodeError::InvalidValue { field: "len" });
        }
        let bytes = input.read_bytes(len)?;
        input.read_zeros(CHUNK_LEN.saturating_sub(len))?;
        Self::new(bytes).ok_or(DecodeError::InvalidValue { field: "len" })
    }
}

/// Splits `bytes` into chunks, in order: `ceil(len / CHUNK_LEN)` of them,
/// all full but the last.
pub fn chunks(bytes: &[u8]) -> impl Iterator<Item = Chunk> + '_ {
    bytes.chunks(CHUNK_LEN).filter_map(Chunk::new)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{Consumer, InMemoryRing, Producer, decode, encode};
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_chunk_holds_one_to_chunk_len_bytes() {
        assert!(Chunk::new(&[]).is_none());
        assert!(Chunk::new(&[7; CHUNK_LEN + 1]).is_none());
        assert_eq!(Chunk::new(b"abc").unwrap().bytes(), b"abc");
        assert_eq!(Chunk::new(&[9; CHUNK_LEN]).unwrap().bytes(), [9; CHUNK_LEN]);
    }

    #[test]
    fn a_chunk_fills_one_slot() {
        let mut slot = [0u8; RING_SLOT + 1];
        assert_eq!(encode(&Chunk::new(b"x").unwrap(), &mut slot), Ok(RING_SLOT));
    }

    #[test]
    fn lengths_out_of_range_and_stray_padding_are_refused() {
        let mut record = [0u8; RING_SLOT];
        encode(&Chunk::new(b"hello").unwrap(), &mut record).unwrap();
        let len_at = HEADER_LEN;
        let mut bad = record;
        bad[len_at..len_at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode::<Chunk>(&bad),
            Err(DecodeError::InvalidValue { field: "len" })
        );
        bad[len_at..len_at + 2].copy_from_slice(&((CHUNK_LEN + 1) as u16).to_le_bytes());
        assert_eq!(
            decode::<Chunk>(&bad),
            Err(DecodeError::InvalidValue { field: "len" })
        );
        let mut bad = record;
        bad[RING_SLOT - 1] = 1;
        assert_eq!(decode::<Chunk>(&bad), Err(DecodeError::NonZeroPadding));
    }

    #[test]
    fn a_stream_crosses_a_ring_in_order() {
        let ring: InMemoryRing<RING_SLOTS, RING_SLOT> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        let stream: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut received = Vec::new();
        for chunk in chunks(&stream) {
            if tx.try_send(&chunk).is_err() {
                while let Some(chunk) = rx.try_recv::<Chunk>().unwrap() {
                    received.extend_from_slice(chunk.bytes());
                }
                tx.try_send(&chunk).unwrap();
            }
        }
        while let Some(chunk) = rx.try_recv::<Chunk>().unwrap() {
            received.extend_from_slice(chunk.bytes());
        }
        assert_eq!(received, stream);
    }

    proptest! {
        #[test]
        fn chunks_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..600)) {
            let mut joined = Vec::new();
            let mut count = 0;
            for chunk in chunks(&bytes) {
                let mut record = [0u8; RING_SLOT];
                encode(&chunk, &mut record).unwrap();
                let back = decode::<Chunk>(&record).unwrap();
                prop_assert_eq!(&back, &chunk);
                joined.extend_from_slice(back.bytes());
                count += 1;
            }
            prop_assert_eq!(joined, bytes.clone());
            prop_assert_eq!(count, bytes.len().div_ceil(CHUNK_LEN));
        }

        #[test]
        fn any_record_decodes_or_is_refused(record in prop::collection::vec(any::<u8>(), 0..2 * RING_SLOT)) {
            if let Ok(chunk) = decode::<Chunk>(&record) {
                let mut again = [0u8; RING_SLOT];
                encode(&chunk, &mut again).unwrap();
                prop_assert_eq!(&again[..], &record[..RING_SLOT]);
            }
        }
    }
}
