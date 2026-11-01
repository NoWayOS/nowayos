// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests (AC 3): whatever a MAC writes into the descriptor
//! memory, between any of the driver's operations, neither ring panics,
//! hands back a buffer it does not hold or one twice, or passes on a frame
//! of a length outside an Ethernet frame's; and a MAC that behaves gets
//! every frame through.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::BTreeSet;

use nowayos_gem::ring::{DescriptorMemory, MAX_FRAME_LEN, MIN_FRAME_LEN, Received, RxRing, TxRing};
use proptest::prelude::*;

const N: usize = 8;
const BYTES: usize = 2 * N * 16;

struct Memory(Vec<u32>);

impl DescriptorMemory for Memory {
    fn read_word(&self, offset: usize) -> Option<u32> {
        offset
            .is_multiple_of(4)
            .then(|| self.0.get(offset / 4).copied())
            .flatten()
    }

    fn write_word(&mut self, offset: usize, value: u32) -> Option<()> {
        offset
            .is_multiple_of(4)
            .then(|| self.0.get_mut(offset / 4).map(|word| *word = value))
            .flatten()
    }

    fn barrier(&self) {}
}

#[derive(Clone, Debug)]
enum Op {
    Offer(usize),
    Take,
    Send(usize, usize),
    Reclaim,
    MacWrites(usize, u32),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..2 * N).prop_map(Op::Offer),
        Just(Op::Take),
        (0..2 * N, 0..1600usize).prop_map(|(buffer, len)| Op::Send(buffer, len)),
        Just(Op::Reclaim),
        (0..BYTES / 4, any::<u32>()).prop_map(|(word, value)| Op::MacWrites(word, value)),
        // Words the MAC is likely to write: ownership and status.
        (
            0..BYTES / 4,
            prop_oneof![Just(1u32), Just(0xC000 | 100), Just(1 << 31)]
        )
            .prop_map(|(word, value)| Op::MacWrites(word, value)),
    ]
}

fn address(buffer: usize) -> u64 {
    0x10_0000_0000 + buffer as u64 * 2048
}

proptest! {
    #[test]
    fn a_hostile_mac_never_breaks_either_ring(ops in proptest::collection::vec(op(), 0..300)) {
        let mut memory = Memory(vec![0; BYTES / 4]);
        let mut rx = RxRing::<N>::new(0);
        let mut tx = TxRing::<N>::new(N * 16);
        rx.init(&mut memory).unwrap();
        tx.init(&mut memory).unwrap();
        let mut rx_held = BTreeSet::new();
        let mut tx_held = BTreeSet::new();
        for op in ops {
            match op {
                Op::Offer(buffer) => {
                    if rx_held.contains(&buffer) {
                        continue;
                    }
                    if rx.offer(&mut memory, buffer, address(buffer)).unwrap() {
                        rx_held.insert(buffer);
                    }
                }
                Op::Take => {
                    if let Some(received) = rx.take(&mut memory).unwrap() {
                        let buffer = match received {
                            Received::Frame { buffer, len } => {
                                prop_assert!((MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len));
                                buffer
                            }
                            Received::Dropped { buffer, .. } => buffer,
                        };
                        prop_assert!(rx_held.remove(&buffer), "handed back {buffer}, not held");
                    }
                }
                Op::Send(buffer, len) => {
                    if tx_held.contains(&buffer) {
                        continue;
                    }
                    match tx.send(&mut memory, buffer, address(buffer), len) {
                        Ok(true) => {
                            tx_held.insert(buffer);
                        }
                        Ok(false) => {}
                        Err(_) => prop_assert!(!(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len)),
                    }
                }
                Op::Reclaim => {
                    if let Some(sent) = tx.reclaim(&mut memory).unwrap() {
                        prop_assert!(tx_held.remove(&sent.buffer), "handed back {}, not held", sent.buffer);
                    }
                }
                Op::MacWrites(word, value) => memory.0[word] = value,
            }
            prop_assert_eq!(rx.held(), rx_held.len());
            prop_assert_eq!(tx.held(), tx_held.len());
        }
    }

    #[test]
    fn a_mac_that_behaves_gets_every_frame_through(lens in proptest::collection::vec(MIN_FRAME_LEN..=MAX_FRAME_LEN, 1..40)) {
        let mut memory = Memory(vec![0; BYTES / 4]);
        let mut rx = RxRing::<N>::new(0);
        rx.init(&mut memory).unwrap();
        for buffer in 0..N {
            prop_assert!(rx.offer(&mut memory, buffer, address(buffer)).unwrap());
        }
        for (i, len) in lens.iter().enumerate() {
            // The MAC writes the frame into slot i mod N and hands it back.
            let at = (i % N) * 16 / 4;
            memory.0[at + 1] = *len as u32 | 1 << 14 | 1 << 15;
            memory.0[at] |= 1;
            let received = rx.take(&mut memory).unwrap();
            let buffer = i % N;
            prop_assert_eq!(received, Some(Received::Frame { buffer, len: *len }));
            prop_assert!(rx.offer(&mut memory, buffer, address(buffer)).unwrap());
        }
    }
}
