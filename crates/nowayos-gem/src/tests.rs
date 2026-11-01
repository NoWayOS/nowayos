// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unit tests: the values written to the registers, MDIO frames and replies,
//! the link resolved from the PHY's registers, and both rings with a MAC
//! that does what UG1085 says.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::mdio::{MdioError, read_frame, read_reply, write_frame};
use crate::phy::{ADVERTISE, Duplex, Link, PhyId, Registers, Speed, ability, bmsr, gbcr, gbsr};
use crate::regs::{self, DMA_CONFIG_VALUE, module_and_revision, network_config, station_address};
use crate::ring::{DescriptorMemory, Drop, Received, RingError, RxRing, Sent, TxRing};

/// Descriptor memory in a vector, with the barriers counted.
pub(crate) struct Memory {
    pub(crate) words: Vec<u32>,
    pub(crate) barriers: core::cell::Cell<usize>,
}

impl Memory {
    pub(crate) fn new(bytes: usize) -> Self {
        Self {
            words: vec![0xDEAD_BEEF; bytes / 4],
            barriers: core::cell::Cell::new(0),
        }
    }

    pub(crate) fn word(&self, offset: usize) -> u32 {
        self.words[offset / 4]
    }

    pub(crate) fn set(&mut self, offset: usize, value: u32) {
        self.words[offset / 4] = value;
    }
}

impl DescriptorMemory for Memory {
    fn read_word(&self, offset: usize) -> Option<u32> {
        if !offset.is_multiple_of(4) {
            return None;
        }
        self.words.get(offset / 4).copied()
    }

    fn write_word(&mut self, offset: usize, value: u32) -> Option<()> {
        if !offset.is_multiple_of(4) {
            return None;
        }
        *self.words.get_mut(offset / 4)? = value;
        Some(())
    }

    fn barrier(&self) {
        self.barriers.set(self.barriers.get() + 1);
    }
}

#[test]
fn dma_config_takes_2048_byte_buffers_64_bit_addresses_and_incr16() {
    assert_eq!(DMA_CONFIG_VALUE, 0x4020_0710);
}

#[test]
fn network_config_keeps_the_bus_width_and_mdc_divider_only() {
    // RP1's value as found on the board: a 128-bit bus, MDC divider 5.
    let found = 0x0054_0000 | 0xFFFF;
    assert_eq!(
        network_config(found, Speed::Mbps1000, Duplex::Full),
        0x0054_0000 | 1 << 17 | 1 << 10 | 1 << 1
    );
    assert_eq!(
        network_config(found, Speed::Mbps100, Duplex::Half),
        0x0054_0000 | 1 << 17 | 1
    );
    assert_eq!(network_config(0, Speed::Mbps10, Duplex::Half), 1 << 17);
}

#[test]
fn the_station_address_is_written_in_received_order() {
    assert_eq!(
        station_address([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
        (0x1200_5452, 0x0000_5634)
    );
}

#[test]
fn rp1s_revision_is_a_gem_gxl() {
    assert_eq!(
        module_and_revision(0x0007_0109),
        (regs::GEM_GXL_MODULE, 0x0109)
    );
    assert_eq!(module_and_revision(0x4007_0106).0, regs::GEM_GXL_MODULE);
}

#[test]
fn mdio_frames_are_clause_22() {
    // As written by hand on the board: PHY 1, register 1.
    assert_eq!(read_frame(1, 1), Ok(0x6086_0000));
    assert_eq!(read_frame(1, 0), Ok(0x6082_0000));
    assert_eq!(write_frame(1, 0, 0x1340), Ok(0x5082_1340));
    assert_eq!(read_frame(32, 0), Err(MdioError::Address));
    assert_eq!(write_frame(1, 32, 0), Err(MdioError::Address));
}

#[test]
fn a_reply_no_phy_drove_is_refused() {
    // Read on the board: with the PHY in reset, then out of it.
    assert_eq!(read_reply(0x6087_FFFF), Err(MdioError::NoReply));
    assert_eq!(read_reply(0x6086_7949), Ok(0x7949));
    assert_eq!(read_reply(0x608A_600D), Ok(0x600D));
}

#[test]
fn the_pi_5s_phy_identifies_itself() {
    let id = PhyId {
        id1: 0x600D,
        id2: 0x84A2,
    };
    assert!(id.is_present());
    assert_eq!((id.model(), id.revision()), (0x0A, 0x2));
    assert!(
        !PhyId {
            id1: 0xFFFF,
            id2: 0xFFFF
        }
        .is_present()
    );
    assert!(!PhyId { id1: 0, id2: 0 }.is_present());
}

fn up(anlpar: u16, gbsr: u16) -> Registers {
    Registers {
        bmsr: 0x7949 | bmsr::LINK | bmsr::AN_COMPLETE,
        anar: ADVERTISE,
        anlpar,
        gbcr: gbcr::FULL,
        gbsr,
    }
}

#[test]
fn the_link_is_the_best_mode_both_ends_have() {
    let everything = ability::TX_FULL | ability::TX_HALF | ability::T10_FULL | ability::T10_HALF;
    assert_eq!(
        Link::resolve(up(everything, gbsr::PARTNER_FULL)),
        Link::Up {
            speed: Speed::Mbps1000,
            duplex: Duplex::Full
        }
    );
    assert_eq!(
        Link::resolve(up(everything, gbsr::PARTNER_HALF)),
        Link::Up {
            speed: Speed::Mbps100,
            duplex: Duplex::Full
        }
    );
    assert_eq!(
        Link::resolve(up(ability::TX_HALF | ability::T10_FULL, 0)),
        Link::Up {
            speed: Speed::Mbps100,
            duplex: Duplex::Half
        }
    );
    assert_eq!(
        Link::resolve(up(ability::T10_FULL | ability::T10_HALF, 0)),
        Link::Up {
            speed: Speed::Mbps10,
            duplex: Duplex::Full
        }
    );
    assert_eq!(
        Link::resolve(up(ability::T10_HALF, 0)),
        Link::Up {
            speed: Speed::Mbps10,
            duplex: Duplex::Half
        }
    );
    assert_eq!(Link::resolve(up(ability::T4, 0)), Link::NoCommonMode);
}

#[test]
fn no_link_or_unfinished_negotiation_is_down() {
    // As read on the board with no cable: BMSR 0x7949, no partner.
    let mut registers = up(0, 0);
    registers.bmsr = 0x7949;
    assert_eq!(Link::resolve(registers), Link::Down);
    registers.bmsr = 0x7949 | bmsr::LINK;
    assert_eq!(Link::resolve(registers), Link::Down);
}

const BASE: usize = 0x100;
const N: usize = 4;

/// A MAC that does what UG1085 says: writes a frame of `len` into the
/// receive descriptor at `slot` and hands it back.
fn mac_receives(memory: &mut Memory, slot: usize, len: u32) {
    let at = BASE + slot * 16;
    memory.set(at + 4, len | 1 << 14 | 1 << 15);
    let word0 = memory.word(at);
    memory.set(at, word0 | 1);
}

#[test]
fn receive_buffers_go_round_the_ring_and_come_back_with_their_frames() {
    let mut memory = Memory::new(0x1000);
    let mut ring = RxRing::<N>::new(BASE);
    ring.init(&mut memory).unwrap();
    // Every descriptor the driver's; the last wraps.
    for slot in 0..N {
        assert_eq!(memory.word(BASE + slot * 16) & 1, 1);
    }
    assert_eq!(memory.word(BASE + (N - 1) * 16) & 2, 2);
    for buffer in 0..N {
        let address = 0x10_0001_0000 + buffer as u64 * 2048;
        assert_eq!(ring.offer(&mut memory, buffer * 2048, address), Ok(true));
    }
    assert_eq!(ring.offer(&mut memory, 99, 0x10_0000_0000), Ok(false));
    assert_eq!(ring.held(), N);
    // Handed over: ownership clear, the address in words 0 and 2.
    assert_eq!(memory.word(BASE), 0x0001_0000);
    assert_eq!(memory.word(BASE + 8), 0x10);
    assert_eq!(memory.word(BASE + 3 * 16), 0x0001_1800 | 2);
    assert_eq!(memory.barriers.get(), N);
    assert_eq!(ring.take(&mut memory), Ok(None));
    mac_receives(&mut memory, 0, 342);
    mac_receives(&mut memory, 1, 60);
    assert_eq!(
        ring.take(&mut memory),
        Ok(Some(Received::Frame {
            buffer: 0,
            len: 342
        }))
    );
    assert_eq!(
        ring.take(&mut memory),
        Ok(Some(Received::Frame {
            buffer: 2048,
            len: 60
        }))
    );
    assert_eq!(ring.take(&mut memory), Ok(None));
    // The slots taken are the driver's again; offered again, they go on.
    assert_eq!(memory.word(BASE) & 1, 1);
    assert_eq!(ring.offer(&mut memory, 0, 0x10_0001_0000), Ok(true));
    assert_eq!(ring.held(), N - 1);
}

#[test]
fn a_receive_descriptor_that_fails_its_checks_is_dropped_and_counted() {
    let mut memory = Memory::new(0x1000);
    let mut ring = RxRing::<N>::new(BASE);
    ring.init(&mut memory).unwrap();
    for buffer in 0..N {
        ring.offer(&mut memory, buffer, 0x8000 + buffer as u64 * 2048)
            .unwrap();
    }
    mac_receives(&mut memory, 0, 1515);
    mac_receives(&mut memory, 1, 13);
    // Start of frame only: not whole in its buffer.
    memory.set(BASE + 2 * 16 + 4, 100 | 1 << 14);
    let word0 = memory.word(BASE + 2 * 16);
    memory.set(BASE + 2 * 16, word0 | 1);
    // The MAC moved the buffer.
    mac_receives(&mut memory, 3, 100);
    memory.set(BASE + 3 * 16 + 8, 0x7);
    let dropped: Vec<_> = (0..N)
        .map(|_| ring.take(&mut memory).unwrap().unwrap())
        .collect();
    assert_eq!(
        dropped,
        [
            Received::Dropped {
                buffer: 0,
                why: Drop::Oversize
            },
            Received::Dropped {
                buffer: 1,
                why: Drop::Malformed
            },
            Received::Dropped {
                buffer: 2,
                why: Drop::Malformed
            },
            Received::Dropped {
                buffer: 3,
                why: Drop::Moved
            },
        ]
    );
    assert_eq!((ring.oversize(), ring.malformed(), ring.moved()), (1, 2, 1));
    assert_eq!(ring.held(), 0);
}

#[test]
fn frames_to_send_go_round_the_ring_and_their_buffers_come_back() {
    let mut memory = Memory::new(0x1000);
    let mut ring = TxRing::<N>::new(BASE);
    ring.init(&mut memory).unwrap();
    for slot in 0..N {
        assert_eq!(memory.word(BASE + slot * 16 + 4) >> 31, 1);
    }
    assert_eq!(
        ring.send(&mut memory, 0, 0x10_0002_0000, 13),
        Err(RingError::Length)
    );
    assert_eq!(
        ring.send(&mut memory, 0, 0x10_0002_0000, 1515),
        Err(RingError::Length)
    );
    assert_eq!(
        ring.send(&mut memory, 0, 0x10_0002_0004, 60),
        Err(RingError::Address)
    );
    for buffer in 0..N {
        let address = 0x10_0002_0000 + buffer as u64 * 2048;
        assert_eq!(
            ring.send(&mut memory, buffer, address, 60 + buffer),
            Ok(true)
        );
    }
    assert_eq!(ring.send(&mut memory, 9, 0x10_0000_0000, 60), Ok(false));
    // Handed over: used clear, the length and last buffer set.
    assert_eq!(memory.word(BASE + 4), 60 | 1 << 15);
    assert_eq!(memory.word(BASE + 3 * 16 + 4), 63 | 1 << 15 | 1 << 30);
    assert_eq!(memory.word(BASE + 8), 0x10);
    assert_eq!(ring.reclaim(&mut memory), Ok(None));
    // The MAC sends the first, then the second with a retry limit error.
    let word1 = memory.word(BASE + 4);
    memory.set(BASE + 4, word1 | 1 << 31);
    let word1 = memory.word(BASE + 16 + 4);
    memory.set(BASE + 16 + 4, word1 | 1 << 31 | 1 << 29);
    assert_eq!(
        ring.reclaim(&mut memory),
        Ok(Some(Sent {
            buffer: 0,
            ok: true
        }))
    );
    assert_eq!(
        ring.reclaim(&mut memory),
        Ok(Some(Sent {
            buffer: 1,
            ok: false
        }))
    );
    assert_eq!(ring.reclaim(&mut memory), Ok(None));
    assert_eq!(ring.errors(), 1);
    assert_eq!(ring.held(), N - 2);
}

#[test]
fn a_ring_that_does_not_fit_its_memory_is_the_drivers_error() {
    let mut memory = Memory::new(BASE + 16);
    assert_eq!(
        RxRing::<N>::new(BASE).init(&mut memory),
        Err(RingError::Memory)
    );
    assert_eq!(
        TxRing::<N>::new(BASE).init(&mut memory),
        Err(RingError::Memory)
    );
    assert_eq!(
        RxRing::<N>::new(usize::MAX - 8).init(&mut memory),
        Err(RingError::Memory)
    );
}

#[test]
fn frames_still_queued_are_given_up_in_order() {
    let mut memory = Memory::new(0x1000);
    let mut ring = TxRing::<N>::new(BASE);
    ring.init(&mut memory).unwrap();
    for buffer in 0..3 {
        ring.send(&mut memory, buffer, 0x8000 + buffer as u64 * 2048, 60)
            .unwrap();
    }
    let word1 = memory.word(BASE + 4);
    memory.set(BASE + 4, word1 | 1 << 31);
    assert_eq!(
        ring.reclaim(&mut memory),
        Ok(Some(Sent {
            buffer: 0,
            ok: true
        }))
    );
    assert_eq!(ring.abandon(&mut memory), Ok(Some(1)));
    assert_eq!(ring.abandon(&mut memory), Ok(Some(2)));
    assert_eq!(ring.abandon(&mut memory), Ok(None));
    assert_eq!(ring.held(), 0);
    ring.init(&mut memory).unwrap();
    assert_eq!(ring.send(&mut memory, 5, 0x8000, 60), Ok(true));
    assert_eq!(memory.word(BASE + 4), 60 | 1 << 15);
}

#[test]
fn the_broadcom_phys_rgmii_delays_are_one_bit_each_every_other_kept() {
    use crate::brcm;
    // Selecting each shadow for reading.
    assert_eq!(brcm::misc_select(), 0x7007);
    assert_eq!(brcm::clock_select(), 0x0C00);
    // The receive skew: written with the shadow's write bit and selector,
    // its other bits as read (bits 14:3), the skew bit set.
    assert_eq!(brcm::misc_with_rx_skew(0x0000), 0x8207);
    assert_eq!(brcm::misc_with_rx_skew(0x7007), 0xF207);
    assert_eq!(brcm::misc_with_rx_skew(0x0218), 0x821F);
    // The transmit delay: bits 9:0 as read, the delay bit set.
    assert_eq!(brcm::clock_with_tx_delay(0x0000), 0x8E00);
    assert_eq!(brcm::clock_with_tx_delay(0x0C21), 0x8E21);
    assert_eq!(brcm::clock_with_tx_delay(0xFFFF), 0x8FFF);
    assert!(brcm::rx_skew_on(0x0200) && !brcm::rx_skew_on(0x7DF8));
    assert!(brcm::tx_delay_on(0x0E00) && !brcm::tx_delay_on(0x0C00));
    // Only the Pi 5's PHY, by its identifier.
    assert!(brcm::is_bcm54213pe(PhyId {
        id1: 0x600D,
        id2: 0x84A2
    }));
    assert!(brcm::is_bcm54213pe(PhyId {
        id1: 0x600D,
        id2: 0x84AF
    }));
    assert!(!brcm::is_bcm54213pe(PhyId {
        id1: 0x600D,
        id2: 0x84B2
    }));
    assert!(!brcm::is_bcm54213pe(PhyId {
        id1: 0x0022,
        id2: 0x1622
    }));
}
