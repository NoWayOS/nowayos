// SPDX-License-Identifier: MIT OR Apache-2.0
//! A root hub port's status and control register, PORTSC (xHCI 1.2,
//! section 5.4.8).
//!
//! PORTSC mixes bits of every kind: read-only status (Current Connect
//! Status, the port's speed), bits software sets (Port Power, Port Reset),
//! change bits a write of 1 clears (RW1C), and Port Enabled/Disabled, which
//! a write of 1 *disables*. Writing back what was read would clear every
//! pending change and disable the port, so a driver never does: it writes
//! [`PortStatus::write_value`], which keeps the bits that must keep their
//! value and sets only the one change asked for.

use crate::bits;

const CONNECTED: u32 = 1;
const ENABLED: u32 = 1 << 1;
const OVER_CURRENT: u32 = 1 << 3;
const RESET: u32 = 1 << 4;
const POWER: u32 = 1 << 9;
/// The change bits, 17 to 23: connect, enable, warm reset, over-current,
/// reset, link state and config error changes. All RW1C.
const CHANGES: u32 = 0x7F << 17;
/// Bits a write must give their current value, or it changes them: Port
/// Power, Port Indicator Control (15:14) and the wake enables (27:25).
const PRESERVE: u32 = POWER | (0x3 << 14) | (0x7 << 25);

/// A port's speed, as PORTSC's Port Speed field gives it with the default
/// Protocol Speed IDs (xHCI 1.2, section 7.2.2.1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    /// Full-speed, USB 1.1: 12 Mb/s.
    Full,
    /// Low-speed, USB 1.1: 1.5 Mb/s.
    Low,
    /// High-speed, USB 2.0: 480 Mb/s.
    High,
    /// SuperSpeed, USB 3.0: 5 Gb/s.
    Super,
    /// SuperSpeedPlus, USB 3.1 and later.
    SuperPlus,
}

impl Speed {
    /// The speed for Protocol Speed ID `id` (1 to 5), the value a slot
    /// context's Speed field takes too.
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Full),
            2 => Some(Self::Low),
            3 => Some(Self::High),
            4 => Some(Self::Super),
            5 => Some(Self::SuperPlus),
            _ => None,
        }
    }

    /// The Protocol Speed ID: what a slot context's Speed field holds.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Low => 2,
            Self::High => 3,
            Self::Super => 4,
            Self::SuperPlus => 5,
        }
    }

    /// The default control endpoint's packet size before the device
    /// descriptor is read (xHCI 1.2, section 4.3.3, and USB 2.0, section
    /// 5.5.3): 8 at low speed, 8 at full speed until the descriptor gives
    /// the true size, 64 at high speed, 512 at SuperSpeed.
    #[must_use]
    pub const fn initial_max_packet(self) -> u16 {
        match self {
            Self::Low | Self::Full => 8,
            Self::High => 64,
            Self::Super | Self::SuperPlus => 512,
        }
    }

    /// A short name for a log line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Full => "full-speed",
            Self::Low => "low-speed",
            Self::High => "high-speed",
            Self::Super => "SuperSpeed",
            Self::SuperPlus => "SuperSpeedPlus",
        }
    }
}

/// One read of a port's PORTSC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortStatus(pub u32);

impl PortStatus {
    /// Current Connect Status: a device is attached.
    #[must_use]
    pub const fn connected(self) -> bool {
        self.0 & CONNECTED != 0
    }

    /// Port Enabled: the port has been reset (USB 2.0) or trained (USB 3)
    /// and passes traffic.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.0 & ENABLED != 0
    }

    /// Port Reset: a reset is under way.
    #[must_use]
    pub const fn resetting(self) -> bool {
        self.0 & RESET != 0
    }

    /// Port Power.
    #[must_use]
    pub const fn powered(self) -> bool {
        self.0 & POWER != 0
    }

    /// Over-current Active.
    #[must_use]
    pub const fn over_current(self) -> bool {
        self.0 & OVER_CURRENT != 0
    }

    /// The Port Link State field, bits 8:5.
    #[must_use]
    pub const fn link_state(self) -> u8 {
        bits(self.0, 5, 4) as u8
    }

    /// The attached device's speed, if a device is attached and the field
    /// holds a default Protocol Speed ID.
    #[must_use]
    pub const fn speed(self) -> Option<Speed> {
        if !self.connected() {
            return None;
        }
        Speed::from_id(bits(self.0, 10, 4) as u8)
    }

    /// The change bits set, 17 to 23, in place.
    #[must_use]
    pub const fn changes(self) -> u32 {
        self.0 & CHANGES
    }

    /// Connect Status Change.
    #[must_use]
    pub const fn connect_changed(self) -> bool {
        self.0 & (1 << 17) != 0
    }

    /// Port Reset Change: a reset this driver started has finished.
    #[must_use]
    pub const fn reset_changed(self) -> bool {
        self.0 & (1 << 21) != 0
    }

    /// The value to write to PORTSC for `write`, given this read of it:
    /// the bits that must keep their value keep it, every other bit is 0 -
    /// so no change bit is cleared and the port is not disabled - except
    /// what `write` sets.
    #[must_use]
    pub const fn write_value(self, write: PortWrite) -> u32 {
        let kept = self.0 & PRESERVE;
        match write {
            PortWrite::Power => kept | POWER,
            PortWrite::Reset => kept | RESET,
            PortWrite::ClearChanges => kept | self.changes(),
        }
    }
}

/// One change to make through PORTSC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortWrite {
    /// Set Port Power.
    Power,
    /// Start a port reset (USB 2.0 ports: a USB 3 port enables itself).
    Reset,
    /// Clear every change bit that is set (writes 1 to each).
    ClearChanges,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rp1s_port_after_a_reset_with_the_connection_change_still_set() {
        // RP1's port 2 on the Pi 5 (2026-10-07) with a full-speed device,
        // its reset over: connected, enabled, full speed, link in U0 - and
        // the connection change from before the controller ran still set
        // beside the reset change, so the controller raised no event for
        // the reset's end. Clearing writes back exactly the two changes and
        // keeps the power on.
        let after = PortStatus(0x0022_0603);
        assert!(after.connected() && after.enabled() && !after.resetting());
        assert_eq!(after.speed(), Some(Speed::Full));
        assert_eq!(after.link_state(), 0);
        assert!(after.connect_changed() && after.reset_changed());
        assert_eq!(
            after.write_value(PortWrite::ClearChanges),
            0x0022_0200,
            "the two changes, and power"
        );
        assert_eq!(after.write_value(PortWrite::Reset), 0x0000_0210);
    }

    #[test]
    fn rp1s_idle_ports_read_as_powered_and_waiting() {
        // RP1's ports with nothing attached, read on the Pi 5 (2026-10-07):
        // powered, link state 5 (RxDetect), nothing connected.
        let idle = PortStatus(0x0000_02A0);
        assert!(idle.powered() && !idle.connected() && !idle.enabled());
        assert_eq!(idle.link_state(), 5);
        assert_eq!(idle.speed(), None);
        assert_eq!(idle.changes(), 0);
    }

    #[test]
    fn a_connected_low_speed_device() {
        // Connected, powered, low-speed (ID 2), connect status changed.
        let status = PortStatus(CONNECTED | POWER | (2 << 10) | (1 << 17));
        assert_eq!(status.speed(), Some(Speed::Low));
        assert!(status.connect_changed() && !status.reset_changed());
        assert_eq!(Speed::Low.initial_max_packet(), 8);
        assert_eq!(Speed::High.initial_max_packet(), 64);
        assert_eq!(Speed::from_id(Speed::Super.id()), Some(Speed::Super));
        assert_eq!(Speed::from_id(0), None);
        assert_eq!(Speed::from_id(6), None);
    }

    #[test]
    fn writes_never_disable_the_port_or_clear_other_changes() {
        // Enabled, powered, connect and reset changes pending, wake on
        // connect enabled.
        let status = PortStatus(CONNECTED | ENABLED | POWER | (1 << 17) | (1 << 21) | (1 << 25));
        let reset = status.write_value(PortWrite::Reset);
        assert_eq!(reset, POWER | (1 << 25) | RESET);
        let clear = status.write_value(PortWrite::ClearChanges);
        assert_eq!(clear, POWER | (1 << 25) | (1 << 17) | (1 << 21));
        assert_eq!(PortStatus(0).write_value(PortWrite::Power), POWER);
    }

    proptest! {
        #[test]
        fn a_write_never_sets_port_enabled(read: u32) {
            for write in [PortWrite::Power, PortWrite::Reset, PortWrite::ClearChanges] {
                let value = PortStatus(read).write_value(write);
                prop_assert_eq!(value & ENABLED, 0);
                // Port Power keeps its value unless the write powers the port.
                if write != PortWrite::Power {
                    prop_assert_eq!(value & POWER, read & POWER);
                }
                // Only ClearChanges writes change bits, and only those set.
                if write == PortWrite::ClearChanges {
                    prop_assert_eq!(value & CHANGES, read & CHANGES);
                } else {
                    prop_assert_eq!(value & CHANGES, 0);
                }
            }
        }
    }
}
