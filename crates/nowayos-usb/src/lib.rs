// SPDX-License-Identifier: MIT OR Apache-2.0
//! USB host logic for NoWayOS's USB driver PD: everything a
//! USB host does that needs no hardware, so it is tested on the host and
//! fuzzed.
//!
//! Written from public specifications only, under the clean-room practice:
//! Intel's *eXtensible Host
//! Controller Interface for Universal Serial Bus (xHCI)*, revision 1.2
//! (registers, rings, contexts, the device initialisation sequence of
//! section 4.3), usb.org's *Universal Serial Bus Specification*, revision 2.0
//! (chapter 9: requests and descriptors), and usb.org's *Device Class
//! Definition for Human Interface Devices (HID)*, version 1.11 (section 7.2's
//! class requests and appendix B, the boot protocol), with its *HID Usage
//! Tables* for the keyboard page `nowayos-input` already numbers keys by;
//! and for audio streams usb.org's *Device Class Definition for
//! Audio Devices* and *for Audio Data Formats*, both release 1.0, and for
//! video its *Device Class Definition for Video Devices* and
//! *Motion-JPEG Payload*, both revision 1.5. No driver source of any kind
//! was read.
//!
//! * [`caps`] - the host controller's capability registers and extended
//!   capabilities, decoded and checked before a driver trusts any offset
//!   they give;
//! * [`port`] - a root hub port's status and control register: what it
//!   says, and the value to write for one change without disturbing the
//!   others;
//! * [`trb`] - Transfer Request Blocks: the commands and transfers a driver
//!   writes, and the events a controller writes back, decoded from a copy
//!   and checked field by field;
//! * [`context`] - the input contexts a driver builds for a device, and the
//!   slot context it reads back;
//! * [`request`] - control requests: standard (USB 2.0 chapter 9) and HID
//!   class (HID 1.11 section 7.2);
//! * [`descriptor`] - device and configuration descriptors, walked with
//!   every length checked, and the HID interfaces that speak the boot
//!   protocol;
//! * [`enumerate`] - one device's enumeration, step by step, as a state
//!   machine the driver feeds what the controller reports;
//! * [`hid`] - boot-protocol keyboard and mouse reports turned into
//!   canonical [`InputEvent`](nowayos_input::InputEvent)s;
//! * [`report`] - HID report descriptors, walked within fixed bounds, and
//!   the absolute pointer one describes - a touchscreen or a tablet - whose
//!   reports become touches or pointer moves;
//! * [`audio`] - USB audio streaming interfaces: the alternate
//!   setting with an isochronous data endpoint, and its PCM format;
//! * [`isoch`] - an isochronous endpoint's transfers in flight and its
//!   counts of intervals served and missed, the frames each packet carries,
//!   and a test tone's samples;
//! * [`video`] - a webcam's VideoStreaming interface: its Motion
//!   JPEG sizes and rates, the probe and commit that set one, payload
//!   headers removed and whole frames assembled into the slots of a ring a
//!   client reads, which says when the client wants frames and what the
//!   camera offers;
//! * [`capture`] - a microphone's or a camera's capture gate: the control requests that keep it at alternate setting 0
//!   until its client asks, and whether its transfers may be queued.
//!
//! **Nothing a controller or a device writes is trusted.** Every decoder
//! takes bytes the caller has already copied out of DMA memory, checks them
//! and returns a typed error for anything it does not accept; none panics
//! (`make fuzz` runs them on arbitrary bytes).
//!
//! Provisional: USB 1.1 and 2.0 devices on a controller's root ports -
//! keyboards and mice through the boot protocol, any other HID interface
//! whose report descriptor, walked within bounds, lays out one absolute
//! pointer (a tablet or a touchscreen's first contact), and audio streaming
//! interfaces' isochronous endpoints, and a camera's Motion JPEG
//! video on its isochronous endpoint; a microphone streamed only
//! while its client asks (`capture`). Hubs, other classes, bulk
//! endpoints and USB 3 streams come later.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod audio;
pub mod caps;
pub mod capture;
pub mod context;
pub mod descriptor;
pub mod enumerate;
pub mod hid;
pub mod isoch;
pub mod port;
#[cfg(kani)]
mod proofs;
pub mod report;
pub mod request;
pub mod trb;
pub mod video;

/// The little-endian `u32` at byte `at` of `bytes`, if it has four bytes
/// there.
pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let word: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(word))
}

/// The little-endian `u16` at byte `at` of `bytes`.
pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let word: [u8; 2] = bytes.get(at..end)?.try_into().ok()?;
    Some(u16::from_le_bytes(word))
}

/// Writes `value` little-endian at byte `at` of `out`; `None`, writing
/// nothing, if it does not fit.
pub(crate) fn put32(out: &mut [u8], at: usize, value: u32) -> Option<()> {
    let end = at.checked_add(4)?;
    out.get_mut(at..end)?.copy_from_slice(&value.to_le_bytes());
    Some(())
}

/// Bits `low..low + width` of `value`, shifted down: `width` is at most 31,
/// so the mask never overflows.
pub(crate) const fn bits(value: u32, low: u32, width: u32) -> u32 {
    let mask = match 1u32.checked_shl(width) {
        Some(bit) => bit.wrapping_sub(1),
        None => u32::MAX,
    };
    match value.checked_shr(low) {
        Some(shifted) => shifted & mask,
        None => 0,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_helpers_check_their_bounds() {
        let bytes = [1, 2, 3, 4, 5];
        assert_eq!(le32(&bytes, 0), Some(0x0403_0201));
        assert_eq!(le32(&bytes, 1), Some(0x0504_0302));
        assert_eq!(le32(&bytes, 2), None);
        assert_eq!(le32(&bytes, usize::MAX), None);
        assert_eq!(le16(&bytes, 3), Some(0x0504));
        assert_eq!(le16(&bytes, 4), None);
        let mut out = [0u8; 6];
        assert_eq!(put32(&mut out, 2, 0xAABB_CCDD), Some(()));
        assert_eq!(out, [0, 0, 0xDD, 0xCC, 0xBB, 0xAA]);
        assert_eq!(put32(&mut out, 3, 1), None);
        assert_eq!(out, [0, 0, 0xDD, 0xCC, 0xBB, 0xAA], "nothing written");
    }

    #[test]
    fn bit_fields() {
        assert_eq!(bits(0x0300_0440, 24, 8), 3);
        assert_eq!(bits(0x0300_0440, 8, 11), 4);
        assert_eq!(bits(0xFFFF_FFFF, 0, 31), 0x7FFF_FFFF);
        assert_eq!(bits(0xFFFF_FFFF, 0, 32), 0xFFFF_FFFF);
        assert_eq!(bits(0xFFFF_FFFF, 40, 4), 0);
    }
}
