// SPDX-License-Identifier: MIT OR Apache-2.0
//! A Cadence GEM Ethernet MAC's rules, with no hardware: what a
//! NIC driver PD needs to drive one - the Raspberry Pi 5's, in RP1 - kept
//! here where the host can test it, so that the driver's `unsafe` register
//! accesses (`nowayos-hw`'s `gem`) are only the accesses.
//!
//! * [`regs`]: the registers and bits the driver uses, and the values it
//!   writes, computed.
//! * [`mdio`]: IEEE 802.3 clause 22 management frames, through the MAC's
//!   PHY maintenance register, and a reply no PHY drove told apart.
//! * [`phy`]: the PHY's clause 22 standard registers, and the link they
//!   describe, resolved by clause 28's priority (Annex 28B.3) - no
//!   vendor-specific register.
//! * [`brcm`]: the one vendor setting - a Broadcom PHY's RGMII clock
//!   delays, both turned on, as the board's `rgmii-id` link needs.
//! * [`ring`]: the receive and transmit descriptor rings in the driver's
//!   DMA memory, 64-bit addressing. The device writes them, so every word
//!   read back is copied, then checked: ownership, a whole frame in one
//!   buffer, a length an Ethernet frame can have, and the buffer's address
//!   unchanged. A frame that fails is dropped and counted, and its buffer
//!   comes back to the driver; nothing a device writes can make the ring
//!   hand out a buffer it was not given, or one twice.
//!
//! # Sources
//!
//! Written from public documents only (clean room): AMD's *Zynq
//! UltraScale+ Device Technical Reference Manual* (UG1085 v2.5, chapter
//! 34, GEM Ethernet: the descriptor formats, tables 34-5 to 34-9, and the
//! programming sequences) and *Zynq UltraScale+ Devices Register Reference*
//! (UG1087, the GEM module's registers) - a GEM_GXL r1p06, where RP1's is
//! GEM_GXL 1p09 (Raspberry Pi's *RP1 Peripherals*, 2023-11-07, chapter 7),
//! the same module (its revision register reads module 7 on the board);
//! and IEEE Std 802.3 clauses 22 and 28 for the PHY. For [`brcm`], the
//! register facts of FreeBSD's Broadcom PHY driver (`brgphy`, BSD-4-Clause),
//! no code. No Linux, U-Boot or other GPL source.
//!
//! # Status
//!
//! Provisional: with the Pi 5's NIC driver PD.

#![no_std]
#![forbid(unsafe_code)]

pub mod brcm;
pub mod mdio;
pub mod phy;
pub mod regs;
pub mod ring;

#[cfg(test)]
mod tests;
