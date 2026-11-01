// SPDX-License-Identifier: MIT OR Apache-2.0
//! IEEE 802.3 clause 22 management frames through the GEM's
//! `phy_management` register (UG1087): a write of the frame starts the
//! shift on MDIO, `network_status`'s `man_done` says when it is over, and a
//! read's data is then in the low 16 bits.
//!
//! The register's bits: 31 must be 0 and 30 must be 1 (a clause 22 frame),
//! 29:28 the operation (`10` read, `01` write), 27:23 the PHY's address,
//! 22:18 the register's, 17:16 must be written `10` (the turnaround), and
//! 15:0 the data. A PHY answering a read drives the turnaround's second bit
//! low, so it reads back `10`; with no PHY on the bus, MDIO's pull-up
//! leaves it `11` and the data all ones (seen on the Pi 5 with its PHY held
//! in reset). [`read_reply`] refuses that.

/// The highest clause 22 PHY or register address: five bits.
pub const MAX_ADDRESS: u8 = 31;

/// Bits 31:30 for a clause 22 frame: 0 then 1.
const CLAUSE_22: u32 = 0b01 << 30;
/// Bits 29:28: a read.
const OPERATION_READ: u32 = 0b10 << 28;
/// Bits 29:28: a write.
const OPERATION_WRITE: u32 = 0b01 << 28;
/// Bits 17:16: the turnaround, written `10`.
const TURNAROUND: u32 = 0b10 << 16;
/// The turnaround's bits.
const TURNAROUND_MASK: u32 = 0b11 << 16;

/// Why a management frame was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdioError {
    /// A PHY or register address over [`MAX_ADDRESS`].
    Address,
    /// No PHY drove the reply: the turnaround read back other than `10`.
    NoReply,
}

/// The frame that reads register `register` of the PHY at `phy`.
///
/// # Errors
///
/// [`MdioError::Address`] for an address over five bits.
pub fn read_frame(phy: u8, register: u8) -> Result<u32, MdioError> {
    Ok(CLAUSE_22 | OPERATION_READ | addresses(phy, register)? | TURNAROUND)
}

/// The frame that writes `data` to register `register` of the PHY at `phy`.
///
/// # Errors
///
/// [`MdioError::Address`] for an address over five bits.
pub fn write_frame(phy: u8, register: u8, data: u16) -> Result<u32, MdioError> {
    Ok(CLAUSE_22 | OPERATION_WRITE | addresses(phy, register)? | TURNAROUND | u32::from(data))
}

/// The data of a read's reply, `value` as `phy_management` holds it once
/// the frame is over.
///
/// # Errors
///
/// [`MdioError::NoReply`] if no PHY drove the turnaround.
pub fn read_reply(value: u32) -> Result<u16, MdioError> {
    if value & TURNAROUND_MASK != TURNAROUND {
        return Err(MdioError::NoReply);
    }
    Ok((value & 0xFFFF) as u16)
}

/// Bits 27:18: the PHY's address then the register's.
fn addresses(phy: u8, register: u8) -> Result<u32, MdioError> {
    if phy > MAX_ADDRESS || register > MAX_ADDRESS {
        return Err(MdioError::Address);
    }
    Ok(u32::from(phy) << 23 | u32::from(register) << 18)
}
