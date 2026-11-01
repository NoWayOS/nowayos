// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host controller's capability registers and extended capabilities
//! (xHCI 1.2, sections 5.3 and 7).
//!
//! The capability registers give the offsets of every other register: the
//! operational registers at CAPLENGTH, the runtime registers at RTSOFF, the
//! doorbells at DBOFF, the extended capabilities at xECP. A driver maps a
//! fixed window and must never follow an offset out of it, so
//! [`Capabilities::decode`] checks the values and [`Capabilities::layout`]
//! checks that every register the driver uses lies inside the window, before
//! any is touched.
//!
//! The values are also the controller's identity: its interface version and
//! how many slots, interrupters and ports it has, which a driver compares
//! with the board's datasheet (on the Raspberry Pi 5, RP1's chapter 5: 64
//! device slots and four interrupters per controller).

use core::fmt;

use crate::bits;

/// The capability registers' dwords a driver reads: 0x00 to 0x1C.
pub const CAPABILITY_WORDS: usize = 8;

/// Offsets within the operational registers (section 5.4).
pub mod operational {
    /// USBCMD.
    pub const USBCMD: usize = 0x00;
    /// USBSTS.
    pub const USBSTS: usize = 0x04;
    /// PAGESIZE.
    pub const PAGESIZE: usize = 0x08;
    /// DNCTRL.
    pub const DNCTRL: usize = 0x14;
    /// CRCR, 64 bits.
    pub const CRCR: usize = 0x18;
    /// DCBAAP, 64 bits.
    pub const DCBAAP: usize = 0x30;
    /// CONFIG.
    pub const CONFIG: usize = 0x38;
    /// The first port's register set.
    pub const PORTS: usize = 0x400;
    /// The bytes of each port's register set.
    pub const PORT_STRIDE: usize = 0x10;
}

/// Offsets within an interrupter's register set (section 5.5.2), from the
/// runtime registers' base plus 0x20 for interrupter 0.
pub mod interrupter {
    /// The first interrupter's register set, from the runtime base.
    pub const BASE: usize = 0x20;
    /// IMAN.
    pub const IMAN: usize = 0x00;
    /// IMOD.
    pub const IMOD: usize = 0x04;
    /// ERSTSZ.
    pub const ERSTSZ: usize = 0x08;
    /// ERSTBA, 64 bits.
    pub const ERSTBA: usize = 0x10;
    /// ERDP, 64 bits.
    pub const ERDP: usize = 0x18;
    /// The bytes of one interrupter's register set.
    pub const LEN: usize = 0x20;
}

/// USBCMD bits (section 5.4.1).
pub mod usbcmd {
    /// Run/Stop.
    pub const RUN: u32 = 1;
    /// Host Controller Reset.
    pub const RESET: u32 = 1 << 1;
    /// Interrupter Enable.
    pub const INTERRUPTS: u32 = 1 << 2;
    /// Host System Error Enable.
    pub const HSEE: u32 = 1 << 3;
}

/// USBSTS bits (section 5.4.2).
pub mod usbsts {
    /// HCHalted.
    pub const HALTED: u32 = 1;
    /// Host System Error (RW1C).
    pub const HOST_SYSTEM_ERROR: u32 = 1 << 2;
    /// Event Interrupt (RW1C).
    pub const EVENT_INTERRUPT: u32 = 1 << 3;
    /// Port Change Detect (RW1C).
    pub const PORT_CHANGE: u32 = 1 << 4;
    /// Controller Not Ready.
    pub const NOT_READY: u32 = 1 << 11;
    /// Host Controller Error.
    pub const CONTROLLER_ERROR: u32 = 1 << 12;
}

/// The controller's capability registers, decoded and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// CAPLENGTH: where the operational registers start.
    pub caplength: u8,
    /// HCIVERSION, binary-coded decimal: 0x0110 is xHCI 1.1.
    pub version: u16,
    /// HCSPARAMS1 MaxSlots: device slots, 1 to 255.
    pub max_slots: u8,
    /// HCSPARAMS1 MaxIntrs: interrupters, 1 to 1024.
    pub max_interrupters: u16,
    /// HCSPARAMS1 MaxPorts: root hub ports, 1 to 255.
    pub max_ports: u8,
    /// HCSPARAMS2: scratchpad buffers the controller needs.
    pub scratchpads: u16,
    /// HCSPARAMS2 ERST Max: the event ring segment table may hold
    /// 2^erst_max entries.
    pub erst_max: u8,
    /// HCCPARAMS1 AC64: the controller takes 64-bit addresses.
    pub addressing_64: bool,
    /// HCCPARAMS1 CSZ: contexts are 64 bytes, not 32.
    pub context_64: bool,
    /// HCCPARAMS1 PPC: ports have power switches, so software powers them.
    pub port_power: bool,
    /// HCCPARAMS1 xECP: where the extended capabilities start, in bytes
    /// from the window's start; 0 when there are none.
    pub extended: usize,
    /// DBOFF: where the doorbells start, in bytes.
    pub doorbells: usize,
    /// RTSOFF: where the runtime registers start, in bytes.
    pub runtime: usize,
}

/// Why the capability registers, or the window they are read through, were
/// refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsError {
    /// CAPLENGTH below 0x20 or not a multiple of four.
    CapLength(u8),
    /// An interface version this driver does not know.
    Version(u16),
    /// No slots, no interrupters or no ports.
    Empty,
    /// The runtime registers or doorbells overlap the capability or
    /// operational registers.
    Overlap,
    /// A register the driver uses lies outside the mapped window.
    OutsideWindow {
        /// The offset of the end of what the driver needs.
        needs: usize,
        /// The window's size.
        window: usize,
    },
}

impl fmt::Display for CapsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CapLength(len) => write!(f, "CAPLENGTH {len:#x}"),
            Self::Version(version) => write!(f, "interface version {version:#06x}"),
            Self::Empty => f.write_str("no slots, interrupters or ports"),
            Self::Overlap => f.write_str("register blocks that overlap"),
            Self::OutsideWindow { needs, window } => write!(
                f,
                "registers up to {needs:#x}, beyond the {window:#x}-byte window"
            ),
        }
    }
}

impl Capabilities {
    /// Decodes the capability registers from their first eight dwords,
    /// as read from the window.
    ///
    /// # Errors
    ///
    /// [`CapsError`] for values no xHCI controller gives.
    pub fn decode(words: &[u32; CAPABILITY_WORDS]) -> Result<Self, CapsError> {
        let [cap, hcs1, hcs2, _hcs3, hcc1, dboff, rtsoff, _hcc2] = *words;
        let caplength = bits(cap, 0, 8) as u8;
        if caplength < 0x20 || !caplength.is_multiple_of(4) {
            return Err(CapsError::CapLength(caplength));
        }
        let version = bits(cap, 16, 16) as u16;
        // Versions 0.96 to 1.2 (section 5.3.2's encoding).
        if !(0x0096..=0x0120).contains(&version) {
            return Err(CapsError::Version(version));
        }
        let max_slots = bits(hcs1, 0, 8) as u8;
        let max_interrupters = bits(hcs1, 8, 11) as u16;
        let max_ports = bits(hcs1, 24, 8) as u8;
        if max_slots == 0 || max_interrupters == 0 || max_ports == 0 {
            return Err(CapsError::Empty);
        }
        let high = bits(hcs2, 21, 5);
        let low = bits(hcs2, 27, 5);
        let scratchpads = ((high << 5) | low) as u16;
        let doorbells = (dboff & !0x3) as usize;
        let runtime = (rtsoff & !0x1F) as usize;
        // The runtime registers and the doorbells come after the operational
        // registers' port register sets, and interrupter 0 ends before the
        // doorbells start or starts after the last one ends.
        let ports_end = usize::from(max_ports)
            .saturating_mul(operational::PORT_STRIDE)
            .saturating_add(operational::PORTS)
            .saturating_add(usize::from(caplength));
        let interrupter = runtime.saturating_add(interrupter::BASE);
        let interrupter_end = interrupter.saturating_add(interrupter::LEN);
        let doorbells_end = usize::from(max_slots)
            .saturating_add(1)
            .saturating_mul(4)
            .saturating_add(doorbells);
        if runtime < ports_end
            || doorbells < ports_end
            || (interrupter < doorbells_end && doorbells < interrupter_end)
        {
            return Err(CapsError::Overlap);
        }
        Ok(Self {
            caplength,
            version,
            max_slots,
            max_interrupters,
            max_ports,
            scratchpads,
            erst_max: bits(hcs2, 4, 4) as u8,
            addressing_64: hcc1 & 1 != 0,
            context_64: hcc1 & (1 << 2) != 0,
            port_power: hcc1 & (1 << 3) != 0,
            extended: (bits(hcc1, 16, 16) as usize).saturating_mul(4),
            doorbells,
            runtime,
        })
    }

    /// The window offsets of the registers a driver uses, for a controller
    /// whose ports and slots are used up to `ports` and `slots`, checked to
    /// lie inside a window of `window` bytes.
    ///
    /// # Errors
    ///
    /// [`CapsError::OutsideWindow`] if any does not.
    pub fn layout(&self, window: usize) -> Result<Layout, CapsError> {
        let operational = usize::from(self.caplength);
        let ports_end = usize::from(self.max_ports)
            .checked_mul(operational::PORT_STRIDE)
            .and_then(|len| len.checked_add(operational::PORTS))
            .and_then(|end| end.checked_add(operational));
        let interrupter_end = self
            .runtime
            .checked_add(interrupter::BASE)
            .and_then(|start| start.checked_add(interrupter::LEN));
        // Doorbell 0 (the controller's) and one per slot.
        let doorbells_end = usize::from(self.max_slots)
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .and_then(|len| len.checked_add(self.doorbells));
        let needs = [ports_end, interrupter_end, doorbells_end]
            .into_iter()
            .try_fold(0usize, |most, end| end.map(|end| most.max(end)))
            .unwrap_or(usize::MAX);
        if needs > window {
            return Err(CapsError::OutsideWindow { needs, window });
        }
        Ok(Layout {
            operational,
            interrupter: self.runtime.saturating_add(interrupter::BASE),
            doorbells: self.doorbells,
            ports: self.max_ports,
            extended: self.extended,
            window,
        })
    }
}

/// Where a controller's registers lie in its window, every one checked to
/// be inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    operational: usize,
    interrupter: usize,
    doorbells: usize,
    ports: u8,
    extended: usize,
    window: usize,
}

impl Layout {
    /// The window offset of operational register `offset` (an
    /// [`operational`] constant), if it is one of the registers inside the
    /// operational block's first 0x40 bytes.
    #[must_use]
    pub fn operational(&self, offset: usize) -> Option<usize> {
        (offset < 0x40 && offset.is_multiple_of(4))
            .then(|| self.operational.checked_add(offset))
            .flatten()
    }

    /// The window offset of port `port`'s PORTSC (ports count from 1).
    #[must_use]
    pub fn portsc(&self, port: u8) -> Option<usize> {
        if port == 0 || port > self.ports {
            return None;
        }
        usize::from(port)
            .checked_sub(1)?
            .checked_mul(operational::PORT_STRIDE)?
            .checked_add(operational::PORTS)?
            .checked_add(self.operational)
    }

    /// The window offset of interrupter 0's register `offset` (an
    /// [`interrupter`] constant).
    #[must_use]
    pub fn interrupter(&self, offset: usize) -> Option<usize> {
        (offset < interrupter::LEN && offset.is_multiple_of(4))
            .then(|| self.interrupter.checked_add(offset))
            .flatten()
    }

    /// The window offset of doorbell `index`: 0 is the controller's, 1 to
    /// `slots` a device slot's.
    #[must_use]
    pub fn doorbell(&self, index: u8, slots: u8) -> Option<usize> {
        (index <= slots)
            .then(|| {
                self.doorbells
                    .checked_add(usize::from(index).checked_mul(4)?)
            })
            .flatten()
    }

    /// The ports the controller has.
    #[must_use]
    pub const fn ports(&self) -> u8 {
        self.ports
    }

    /// Where the extended capabilities start (0: none), and the window's
    /// size, which a walk of them must stay inside.
    #[must_use]
    pub const fn extended(&self) -> (usize, usize) {
        (self.extended, self.window)
    }
}

/// Extended capability IDs (xHCI 1.2, table 7-1).
pub mod extended {
    /// USB Legacy Support.
    pub const LEGACY: u8 = 1;
    /// Supported Protocol.
    pub const PROTOCOL: u8 = 2;
}

/// An extended capability's first dword: its ID, and how far on the next
/// one is, in dwords (0: this is the last).
#[must_use]
pub const fn extended_header(word: u32) -> (u8, u8) {
    (bits(word, 0, 8) as u8, bits(word, 8, 8) as u8)
}

/// The Supported Protocol capability's name string: "USB ".
pub const PROTOCOL_NAME: u32 = 0x2042_5355;

/// What a Supported Protocol capability (section 7.2) says: which ports
/// speak which USB major revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    /// The USB major revision: 2 or 3.
    pub major: u8,
    /// The minor revision, binary-coded decimal: 0x00, 0x10, 0x20.
    pub minor: u8,
    /// The first port, from 1.
    pub first_port: u8,
    /// How many ports from it.
    pub ports: u8,
}

impl Protocol {
    /// Decodes a Supported Protocol capability from its first three dwords.
    /// `None` unless its ID is 2, its name is "USB ", and it names at least
    /// one port, the first from 1, without running past port 255.
    #[must_use]
    pub fn decode(words: [u32; 3]) -> Option<Self> {
        let [header, name, ports] = words;
        let (id, _) = extended_header(header);
        if id != extended::PROTOCOL || name != PROTOCOL_NAME {
            return None;
        }
        let first_port = bits(ports, 0, 8) as u8;
        let count = bits(ports, 8, 8) as u8;
        if first_port == 0 || count == 0 || first_port.checked_add(count.wrapping_sub(1)).is_none()
        {
            return None;
        }
        Some(Self {
            major: bits(header, 24, 8) as u8,
            minor: bits(header, 16, 8) as u8,
            first_port,
            ports: count,
        })
    }

    /// Whether `port` is one of this protocol's.
    #[must_use]
    pub fn has(&self, port: u8) -> bool {
        port.checked_sub(self.first_port)
            .is_some_and(|offset| offset < self.ports)
    }
}

/// The USB Legacy Support capability's semaphores (section 7.1.1): bit 16,
/// HC BIOS Owned, and bit 24, HC OS Owned.
pub mod legacy {
    /// HC BIOS Owned Semaphore.
    pub const BIOS_OWNED: u32 = 1 << 16;
    /// HC OS Owned Semaphore.
    pub const OS_OWNED: u32 = 1 << 24;
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// RP1's usbhost0, as read on the Raspberry Pi 5 from U-Boot's prompt on
    /// 2026-10-07.
    const RP1: [u32; 8] = [
        0x0110_0020,
        0x0300_0440,
        0x1400_00F1,
        0x07FF_000A,
        0x0240_FE6D,
        0x0000_0500,
        0x0000_0460,
        0x0000_002F,
    ];

    #[test]
    fn rp1s_controller_decodes_as_its_datasheet_says() {
        let caps = Capabilities::decode(&RP1).unwrap();
        assert_eq!(caps.caplength, 0x20);
        assert_eq!(caps.version, 0x0110);
        // RP1 Peripherals, chapter 5: 64 device slots, four interrupt
        // vectors per controller.
        assert_eq!(caps.max_slots, 64);
        assert_eq!(caps.max_interrupters, 4);
        assert_eq!(caps.max_ports, 3);
        assert_eq!(caps.scratchpads, 2);
        assert_eq!(caps.erst_max, 15);
        assert!(caps.addressing_64 && caps.context_64 && caps.port_power);
        assert_eq!(caps.extended, 0x900);
        assert_eq!((caps.doorbells, caps.runtime), (0x500, 0x460));
        // Everything the driver uses lies in the first page of its window.
        let layout = caps.layout(0x1000).unwrap();
        assert_eq!(layout.operational(operational::USBCMD), Some(0x20));
        assert_eq!(layout.operational(operational::CONFIG), Some(0x58));
        assert_eq!(layout.operational(0x40), None);
        assert_eq!(layout.portsc(1), Some(0x420));
        assert_eq!(layout.portsc(3), Some(0x440));
        assert_eq!(layout.portsc(0), None);
        assert_eq!(layout.portsc(4), None);
        assert_eq!(layout.interrupter(interrupter::ERDP), Some(0x498));
        assert_eq!(layout.interrupter(interrupter::LEN), None);
        assert_eq!(layout.doorbell(0, 4), Some(0x500));
        assert_eq!(layout.doorbell(4, 4), Some(0x510));
        assert_eq!(layout.doorbell(5, 4), None);
        assert_eq!(layout.extended(), (0x900, 0x1000));
        assert_eq!(
            caps.layout(0x500),
            Err(CapsError::OutsideWindow {
                needs: 0x604,
                window: 0x500
            })
        );
    }

    #[test]
    fn impossible_values_are_refused() {
        let mut words = RP1;
        words[0] = 0x0110_0010;
        assert_eq!(
            Capabilities::decode(&words),
            Err(CapsError::CapLength(0x10))
        );
        words[0] = 0x0110_0022;
        assert_eq!(
            Capabilities::decode(&words),
            Err(CapsError::CapLength(0x22))
        );
        words[0] = 0x0200_0020;
        assert_eq!(
            Capabilities::decode(&words),
            Err(CapsError::Version(0x0200))
        );
        words[0] = 0xFFFF_FFFF;
        assert!(Capabilities::decode(&words).is_err());
        let mut words = RP1;
        words[1] = 0x0000_0440;
        assert_eq!(Capabilities::decode(&words), Err(CapsError::Empty));
        let mut words = RP1;
        words[6] = 0x20;
        assert_eq!(Capabilities::decode(&words), Err(CapsError::Overlap));
        let mut words = RP1;
        words[5] = 0x10;
        assert_eq!(Capabilities::decode(&words), Err(CapsError::Overlap));
    }

    #[test]
    fn rp1s_supported_protocols() {
        // The two Supported Protocol capabilities read at 0x910 and 0x920.
        let usb2 = Protocol::decode([0x0200_0402, PROTOCOL_NAME, 0x0018_0201]).unwrap();
        assert_eq!(
            usb2,
            Protocol {
                major: 2,
                minor: 0,
                first_port: 1,
                ports: 2
            }
        );
        assert!(usb2.has(1) && usb2.has(2) && !usb2.has(3) && !usb2.has(0));
        let usb3 = Protocol::decode([0x0300_0002, PROTOCOL_NAME, 0x0000_0103]).unwrap();
        assert_eq!((usb3.major, usb3.first_port, usb3.ports), (3, 3, 1));
        assert_eq!(extended_header(0x0000_0401), (extended::LEGACY, 4));
        assert_eq!(Protocol::decode([0x0200_0401, PROTOCOL_NAME, 0x0201]), None);
        assert_eq!(Protocol::decode([0x0200_0402, 0, 0x0201]), None);
        assert_eq!(Protocol::decode([0x0200_0402, PROTOCOL_NAME, 0x0200]), None);
        assert_eq!(Protocol::decode([0x0200_0402, PROTOCOL_NAME, 0x0001]), None);
        assert_eq!(Protocol::decode([0x0200_0402, PROTOCOL_NAME, 0x02FF]), None);
        assert!(Protocol::decode([0x0200_0402, PROTOCOL_NAME, 0x01FF]).is_some());
    }

    proptest! {
        #[test]
        fn no_offset_leaves_the_window(words in any::<[u32; 8]>(), window in 0usize..0x10000) {
            let Ok(caps) = Capabilities::decode(&words) else { return Ok(()); };
            let Ok(layout) = caps.layout(window) else { return Ok(()); };
            for port in 0..=255u8 {
                if let Some(at) = layout.portsc(port) {
                    prop_assert!(at + 4 <= window);
                }
            }
            for offset in (0..0x40).step_by(4) {
                if let Some(at) = layout.operational(offset) {
                    prop_assert!(at + 4 <= window);
                }
            }
            for offset in (0..0x20).step_by(4) {
                if let Some(at) = layout.interrupter(offset) {
                    prop_assert!(at + 4 <= window);
                }
            }
            for index in 0..=caps.max_slots {
                prop_assert!(layout.doorbell(index, caps.max_slots).unwrap() + 4 <= window);
            }
        }

        #[test]
        fn protocol_decoding_keeps_its_rules(words in any::<[u32; 3]>()) {
            if let Some(protocol) = Protocol::decode(words) {
                prop_assert!(protocol.first_port >= 1 && protocol.ports >= 1);
                prop_assert!(u16::from(protocol.first_port) + u16::from(protocol.ports) - 1 <= 255);
            }
        }
    }
}
