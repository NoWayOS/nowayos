// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RGMII clock delays of a Broadcom gigabit PHY - the Raspberry Pi 5's
//! BCM54213PE - through two of its vendor registers.
//!
//! An RGMII link needs its clocks delayed against the data in each
//! direction, by about 2 ns. The Pi 5's device tree names the link
//! `rgmii-id`: the PHY adds both delays, the MAC neither. With the PHY's
//! reset released by the driver, the PHY starts with its own defaults, and
//! the MAC then received nothing at all on the board; so the driver
//! turns both delays on:
//!
//! * the receive clock's skew, `RGMII_SKEW_EN` in the auxiliary control
//!   register's (0x18) "miscellaneous control" shadow (selector 7): the
//!   shadow is chosen for reading by writing its selector into bits 2:0 and
//!   again into bits 14:12, read back in bits 14:3, and written with bit 15
//!   set, its selector in bits 2:0;
//! * the transmit clock's delay, `GTXCLK_EN` in shadow register 0x1C's
//!   "clock alignment control" (selector 3, in bits 14:10): chosen by
//!   writing the selector, read back in bits 9:0, and written with bit 15
//!   set and the selector again.
//!
//! Each is read, changed in its one bit and written back, every other bit
//! kept. Register facts only - no code - from FreeBSD's Broadcom PHY driver
//! (`sys/dev/mii/brgphy.c`, `brgphy_bcm54xx_clock_delay`, and
//! `brgphyreg.h`; BSD-4-Clause, Copyright (c) 2000 Bill Paul), which sets
//! the same two bits for the BCM54213PE (read for the receive settings;
//! no GPL source). The PHY is recognised by its identifier.

/// The auxiliary control register.
pub const AUXCTL: u8 = 0x18;
/// Shadow register 0x1C.
pub const SHADOW_1C: u8 = 0x1C;

/// AUXCTL's "miscellaneous control" shadow selector.
const MISC: u16 = 0x07;
/// AUXCTL: where the shadow to read is selected.
const MISC_READ_SHIFT: u16 = 12;
/// AUXCTL: the miscellaneous control shadow's bits, as read.
const MISC_DATA: u16 = 0x7FF8;
/// AUXCTL: write the shadow selected in bits 2:0.
const MISC_WRITE: u16 = 0x8000;
/// The miscellaneous control shadow's receive clock skew (RGMII RX delay).
pub const RX_SKEW: u16 = 0x0200;

/// Shadow 0x1C's "clock alignment control" selector, in bits 14:10.
const CLOCK_ALIGNMENT: u16 = 0x03 << 10;
/// Shadow 0x1C: the selected shadow's bits, as read.
const SHADOW_DATA: u16 = 0x03FF;
/// Shadow 0x1C: write the selected shadow.
const SHADOW_WRITE: u16 = 0x8000;
/// Clock alignment control's transmit clock delay (RGMII TX delay).
pub const TX_DELAY: u16 = 0x0200;

/// The Broadcom PHY these registers are for: OUI 00-10-18 as registers 2
/// and 3 carry it, model 0x0A (the BCM54213PE on the Pi 5, identifier
/// `600d:84a2` on the board), any revision.
#[must_use]
pub const fn is_bcm54213pe(id: crate::phy::PhyId) -> bool {
    id.id1 == 0x600D && id.id2 & 0xFFF0 == 0x84A0
}

/// What to write to [`AUXCTL`] to read the miscellaneous control shadow.
#[must_use]
pub const fn misc_select() -> u16 {
    MISC | MISC << MISC_READ_SHIFT
}

/// What to write to [`AUXCTL`] to set the receive skew in the shadow that
/// read as `read` (after [`misc_select`]), every other bit kept.
#[must_use]
pub const fn misc_with_rx_skew(read: u16) -> u16 {
    MISC_WRITE | MISC | (read & MISC_DATA) | RX_SKEW
}

/// What to write to [`SHADOW_1C`] to read the clock alignment control.
#[must_use]
pub const fn clock_select() -> u16 {
    CLOCK_ALIGNMENT
}

/// What to write to [`SHADOW_1C`] to set the transmit delay in the clock
/// alignment control that read as `read` (after [`clock_select`]), every
/// other bit kept.
#[must_use]
pub const fn clock_with_tx_delay(read: u16) -> u16 {
    SHADOW_WRITE | CLOCK_ALIGNMENT | (read & SHADOW_DATA) | TX_DELAY
}

/// Whether the miscellaneous control shadow, as read, has the receive skew
/// on.
#[must_use]
pub const fn rx_skew_on(read: u16) -> bool {
    read & RX_SKEW != 0
}

/// Whether the clock alignment control, as read, has the transmit delay on.
#[must_use]
pub const fn tx_delay_on(read: u16) -> bool {
    read & SHADOW_DATA & TX_DELAY != 0
}
