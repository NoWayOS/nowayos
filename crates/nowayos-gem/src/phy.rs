// SPDX-License-Identifier: MIT OR Apache-2.0
//! An Ethernet PHY through its clause 22 standard registers (IEEE 802.3
//! clause 22.2.4, and clause 40.5 for 1000BASE-T's): nothing
//! vendor-specific here (the Broadcom PHY's RGMII delays are
//! [`crate::brcm`]'s). The driver restarts auto-negotiation with every
//! ability advertised, then reads the registers below; [`Link::resolve`]
//! names the link they describe, by the priority of clause 28 (Annex
//! 28B.3): 1000BASE-T full duplex, 1000BASE-T half, 100BASE-TX full,
//! 100BASE-TX half, 10BASE-T full, 10BASE-T half.

/// Register 0, the control register.
pub const BMCR: u8 = 0;
/// Register 1, the status register.
pub const BMSR: u8 = 1;
/// Registers 2 and 3, the PHY identifier.
pub const PHYID1: u8 = 2;
/// See [`PHYID1`].
pub const PHYID2: u8 = 3;
/// Register 4, the auto-negotiation advertisement.
pub const ANAR: u8 = 4;
/// Register 5, the link partner's ability.
pub const ANLPAR: u8 = 5;
/// Register 9, 1000BASE-T control (clause 40.5.1.1).
pub const GBCR: u8 = 9;
/// Register 10, 1000BASE-T status (clause 40.5.1.1).
pub const GBSR: u8 = 10;

/// BMCR bits.
pub mod bmcr {
    /// Power down.
    pub const POWER_DOWN: u16 = 1 << 11;
    /// Isolate the PHY from the MII.
    pub const ISOLATE: u16 = 1 << 10;
    /// Auto-negotiation enable.
    pub const AN_ENABLE: u16 = 1 << 12;
    /// Restart auto-negotiation (self-clearing).
    pub const AN_RESTART: u16 = 1 << 9;
    /// Loopback: the PHY returns what the MAC sends (clause 22.2.4.1.2).
    pub const LOOPBACK: u16 = 1 << 14;
    /// Speed selection, least significant bit (with auto-negotiation off).
    pub const SPEED_LSB: u16 = 1 << 13;
    /// Duplex mode: full (with auto-negotiation off).
    pub const FULL_DUPLEX: u16 = 1 << 8;
    /// Speed selection, most significant bit: with bit 13 clear, 1000 Mb/s.
    pub const SPEED_MSB: u16 = 1 << 6;
}

/// BMSR bits.
pub mod bmsr {
    /// Auto-negotiation complete.
    pub const AN_COMPLETE: u16 = 1 << 5;
    /// Link status (latched low: read twice for the present state).
    pub const LINK: u16 = 1 << 2;
    /// Extended status (register 15) present: a 1000 Mb/s PHY.
    pub const EXTENDED_STATUS: u16 = 1 << 8;
}

/// ANAR and ANLPAR technology bits (clause 28.2.1.2, Annex 28B.2).
pub mod ability {
    /// 100BASE-T4.
    pub const T4: u16 = 1 << 9;
    /// 100BASE-TX full duplex.
    pub const TX_FULL: u16 = 1 << 8;
    /// 100BASE-TX.
    pub const TX_HALF: u16 = 1 << 7;
    /// 10BASE-T full duplex.
    pub const T10_FULL: u16 = 1 << 6;
    /// 10BASE-T.
    pub const T10_HALF: u16 = 1 << 5;
    /// The selector field: IEEE 802.3.
    pub const SELECTOR_802_3: u16 = 0x0001;
}

/// GBCR bits: what this PHY advertises at 1000 Mb/s.
pub mod gbcr {
    /// 1000BASE-T full duplex.
    pub const FULL: u16 = 1 << 9;
    /// 1000BASE-T half duplex.
    pub const HALF: u16 = 1 << 8;
}

/// GBSR bits: what the link partner advertised at 1000 Mb/s.
pub mod gbsr {
    /// The link partner is 1000BASE-T full duplex capable.
    pub const PARTNER_FULL: u16 = 1 << 11;
    /// The link partner is 1000BASE-T half duplex capable.
    pub const PARTNER_HALF: u16 = 1 << 10;
}

/// What the driver advertises: every 10 and 100 Mb/s ability but 100BASE-T4
/// (an RGMII PHY has none), with IEEE 802.3's selector.
pub const ADVERTISE: u16 = ability::TX_FULL
    | ability::TX_HALF
    | ability::T10_FULL
    | ability::T10_HALF
    | ability::SELECTOR_802_3;

/// What the driver advertises at 1000 Mb/s: full duplex only (the GEM runs
/// 1000 Mb/s in full duplex only, UG1085 chapter 34).
pub const ADVERTISE_1000: u16 = gbcr::FULL;

/// A link's speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    /// 10 Mb/s.
    Mbps10,
    /// 100 Mb/s.
    Mbps100,
    /// 1000 Mb/s.
    Mbps1000,
}

impl Speed {
    /// The speed in Mb/s.
    #[must_use]
    pub const fn mbps(self) -> u32 {
        match self {
            Self::Mbps10 => 10,
            Self::Mbps100 => 100,
            Self::Mbps1000 => 1000,
        }
    }
}

/// A link's duplex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    /// Half duplex.
    Half,
    /// Full duplex.
    Full,
}

/// The link a PHY's registers describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// No link, or auto-negotiation not complete.
    Down,
    /// A link both ends can run, at the highest priority they share.
    Up {
        /// Its speed.
        speed: Speed,
        /// Its duplex.
        duplex: Duplex,
    },
    /// Link and auto-negotiation complete, but no ability in common - or
    /// only one the GEM cannot run (1000 Mb/s half duplex, 100BASE-T4).
    NoCommonMode,
}

/// The five registers the link is resolved from, as read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    /// Register 1 (read twice, the second value).
    pub bmsr: u16,
    /// Register 4.
    pub anar: u16,
    /// Register 5.
    pub anlpar: u16,
    /// Register 9 (zero for a PHY without extended status).
    pub gbcr: u16,
    /// Register 10 (zero for a PHY without extended status).
    pub gbsr: u16,
}

impl Link {
    /// The link `registers` describe: down unless the PHY reports link and
    /// auto-negotiation complete; otherwise the highest-priority mode both
    /// ends advertise (Annex 28B.3) that the GEM runs.
    #[must_use]
    pub const fn resolve(registers: Registers) -> Self {
        let Registers {
            bmsr: status,
            anar: ours,
            anlpar: theirs,
            gbcr: ours_1000,
            gbsr: theirs_1000,
        } = registers;
        if status & bmsr::LINK == 0 || status & bmsr::AN_COMPLETE == 0 {
            return Self::Down;
        }
        if ours_1000 & gbcr::FULL != 0 && theirs_1000 & gbsr::PARTNER_FULL != 0 {
            return Self::Up {
                speed: Speed::Mbps1000,
                duplex: Duplex::Full,
            };
        }
        let common = ours & theirs;
        if common & ability::TX_FULL != 0 {
            Self::Up {
                speed: Speed::Mbps100,
                duplex: Duplex::Full,
            }
        } else if common & ability::TX_HALF != 0 {
            Self::Up {
                speed: Speed::Mbps100,
                duplex: Duplex::Half,
            }
        } else if common & ability::T10_FULL != 0 {
            Self::Up {
                speed: Speed::Mbps10,
                duplex: Duplex::Full,
            }
        } else if common & ability::T10_HALF != 0 {
            Self::Up {
                speed: Speed::Mbps10,
                duplex: Duplex::Half,
            }
        } else {
            Self::NoCommonMode
        }
    }
}

/// A PHY's identifier from registers 2 and 3 (clause 22.2.4.3.1): the OUI's
/// bits 3 to 24, the model and the revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyId {
    /// Register 2.
    pub id1: u16,
    /// Register 3.
    pub id2: u16,
}

impl PhyId {
    /// The manufacturer's model number, register 3's bits 9:4.
    #[must_use]
    pub const fn model(self) -> u16 {
        (self.id2 >> 4) & 0x3F
    }

    /// The revision, register 3's bits 3:0.
    #[must_use]
    pub const fn revision(self) -> u16 {
        self.id2 & 0xF
    }

    /// Whether a PHY answered at all: all zeros or all ones is no PHY.
    #[must_use]
    pub const fn is_present(self) -> bool {
        !(self.id1 == 0 && self.id2 == 0 || self.id1 == 0xFFFF && self.id2 == 0xFFFF)
    }
}
