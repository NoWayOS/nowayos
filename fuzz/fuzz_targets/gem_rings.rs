// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-gem`'s descriptor rings (AC 3) - the
//! receive and transmit rings of a Cadence GEM MAC (RP1's on the Pi 5) in
//! memory the MAC writes as it likes, between any of the driver's
//! operations.
//!
//! Checks: nothing panics; neither ring hands back a buffer it does not
//! hold, or one twice; every frame passed on is an Ethernet frame's length;
//! the rings' own counts of what they hold match the fuzzer's.

#![no_main]

use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_gem::ring::{DescriptorMemory, MAX_FRAME_LEN, MIN_FRAME_LEN, Received, RxRing, TxRing};

const N: usize = 8;
const WORDS: usize = 2 * N * 4;

struct Memory([u32; WORDS]);

impl DescriptorMemory for Memory {
    fn read_word(&self, offset: usize) -> Option<u32> {
        if !offset.is_multiple_of(4) {
            return None;
        }
        self.0.get(offset / 4).copied()
    }

    fn write_word(&mut self, offset: usize, value: u32) -> Option<()> {
        if !offset.is_multiple_of(4) {
            return None;
        }
        *self.0.get_mut(offset / 4)? = value;
        Some(())
    }

    fn barrier(&self) {}
}

/// A buffer's bus address: 2048-byte buffers above RP1's DMA offset.
fn address(buffer: usize) -> u64 {
    0x10_0000_0000_u64.saturating_add(u64::try_from(buffer).unwrap_or(0).saturating_mul(2048))
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut memory = Memory([0; WORDS]);
    let mut rx = RxRing::<N>::new(0);
    let mut tx = TxRing::<N>::new(N * 16);
    assert!(rx.init(&mut memory).is_ok() && tx.init(&mut memory).is_ok());
    let mut rx_held = BTreeSet::new();
    let mut tx_held = BTreeSet::new();
    while !input.is_empty() {
        match input.byte() % 6 {
            0 => {
                let buffer = usize::from(input.byte() % 16);
                if !rx_held.contains(&buffer)
                    && rx.offer(&mut memory, buffer, address(buffer)) == Ok(true)
                {
                    rx_held.insert(buffer);
                }
            }
            1 => {
                let taken = rx.take(&mut memory);
                assert!(taken.is_ok(), "the receive ring's own error: {taken:?}");
                match taken {
                    Ok(Some(Received::Frame { buffer, len })) => {
                        assert!(
                            (MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len),
                            "a frame of {len} bytes"
                        );
                        assert!(
                            rx_held.remove(&buffer),
                            "receive buffer {buffer} handed back, not held"
                        );
                    }
                    Ok(Some(Received::Dropped { buffer, .. })) => {
                        assert!(
                            rx_held.remove(&buffer),
                            "receive buffer {buffer} handed back, not held"
                        );
                    }
                    _ => {}
                }
            }
            2 => {
                let buffer = usize::from(input.byte() % 16);
                let len = usize::from(input.u16() % 1600);
                if !tx_held.contains(&buffer) {
                    match tx.send(&mut memory, buffer, address(buffer), len) {
                        Ok(true) => {
                            tx_held.insert(buffer);
                        }
                        Ok(false) => {}
                        Err(_) => assert!(!(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len)),
                    }
                }
            }
            3 => {
                let reclaimed = tx.reclaim(&mut memory);
                assert!(
                    reclaimed.is_ok(),
                    "the transmit ring's own error: {reclaimed:?}"
                );
                if let Ok(Some(sent)) = reclaimed {
                    assert!(
                        tx_held.remove(&sent.buffer),
                        "transmit buffer {} handed back, not held",
                        sent.buffer
                    );
                }
            }
            // The MAC writes any word, or sets a word's ownership or used bit.
            4 => {
                let word = usize::from(input.byte()) % WORDS;
                if let Some(slot) = memory.0.get_mut(word) {
                    *slot = input.u32();
                }
            }
            _ => {
                let word = usize::from(input.byte()) % WORDS;
                if let Some(slot) = memory.0.get_mut(word) {
                    *slot |= if word % 4 == 1 {
                        0x8000_C000 | u32::from(input.u16() & 0x7FF)
                    } else {
                        1
                    };
                }
            }
        }
        assert_eq!(rx.held(), rx_held.len());
        assert_eq!(tx.held(), tx_held.len());
    }
});
