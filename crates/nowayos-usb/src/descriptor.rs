// SPDX-License-Identifier: MIT OR Apache-2.0
//! Device and configuration descriptors (USB 2.0, sections 9.6.1 to 9.6.6)
//! and the HID interfaces among them that speak the boot protocol (HID 1.11,
//! sections 4.2 and 4.3).
//!
//! **A descriptor is what a device says about itself, and a device is not
//! trusted.** Every function here takes bytes already copied out of DMA
//! memory, checks each descriptor's length and type before reading it, never
//! reads past the bytes it was given or the length the configuration claims,
//! and returns a typed [`DescriptorError`] for anything it does not accept.

use core::fmt;

use crate::le16;
use crate::request::descriptor_type;

/// The bytes of a device descriptor.
pub const DEVICE_DESCRIPTOR_LEN: usize = 18;

/// The bytes of a configuration descriptor's header.
pub const CONFIG_HEADER_LEN: usize = 9;

/// The most boot-protocol interfaces taken from one configuration: a
/// keyboard and a mouse, as a combined receiver presents them.
pub const MAX_BOOT_INTERFACES: usize = 2;

/// HID's interface class.
pub const CLASS_HID: u8 = 3;

/// HID's boot interface subclass.
pub const SUBCLASS_BOOT: u8 = 1;

/// A hub's device class (USB 2.0, section 11.23.1): not supported yet.
pub const CLASS_HUB: u8 = 9;

/// The report descriptor's type, in a HID descriptor's list (HID 1.11,
/// section 7.1).
pub const REPORT_DESCRIPTOR: u8 = 0x22;

/// Why a descriptor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorError {
    /// Fewer bytes than the descriptor needs.
    TooShort {
        /// The bytes there were.
        len: usize,
        /// The bytes needed.
        needed: usize,
    },
    /// A descriptor of another type where this one belongs.
    WrongType {
        /// The type found.
        found: u8,
        /// The type expected.
        expected: u8,
    },
    /// A descriptor whose bLength is shorter than its type needs, or runs
    /// past the end.
    BadLength {
        /// Where it starts.
        at: usize,
        /// Its bLength.
        length: u8,
    },
    /// A default control endpoint packet size USB does not allow.
    BadPacketSize(u8),
    /// A device with no configurations.
    NoConfigurations,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooShort { len, needed } => write!(f, "{len} bytes, not {needed}"),
            Self::WrongType { found, expected } => {
                write!(f, "descriptor type {found:#04x}, not {expected:#04x}")
            }
            Self::BadLength { at, length } => {
                write!(f, "a descriptor at byte {at} of length {length}")
            }
            Self::BadPacketSize(size) => write!(f, "control packet size {size}"),
            Self::NoConfigurations => f.write_str("no configurations"),
        }
    }
}

/// Checks the first two bytes of a descriptor of `expected` type, at least
/// `needed` bytes long, at the start of `bytes`.
fn header(bytes: &[u8], expected: u8, needed: usize) -> Result<(), DescriptorError> {
    let (&length, &found) = match bytes {
        [length, found, ..] => (length, found),
        _ => {
            return Err(DescriptorError::TooShort {
                len: bytes.len(),
                needed,
            });
        }
    };
    if found != expected {
        return Err(DescriptorError::WrongType { found, expected });
    }
    if usize::from(length) < needed {
        return Err(DescriptorError::BadLength { at: 0, length });
    }
    if bytes.len() < needed {
        return Err(DescriptorError::TooShort {
            len: bytes.len(),
            needed,
        });
    }
    Ok(())
}

/// The default control endpoint's packet size from a device descriptor's
/// first eight bytes (bMaxPacketSize0, offset 7): 8, 16, 32 or 64 for USB
/// 2.0 and earlier; for USB 3 (bcdUSB 3.0 or later) an exponent, which must
/// be 9: 512 bytes.
///
/// # Errors
///
/// [`DescriptorError`] if the bytes are not a device descriptor's start, or
/// the size is not one USB allows.
pub fn max_packet0(prefix: &[u8]) -> Result<u16, DescriptorError> {
    header(prefix, descriptor_type::DEVICE, 8)?;
    if prefix.first().copied().unwrap_or(0) < 18 {
        return Err(DescriptorError::BadLength {
            at: 0,
            length: prefix.first().copied().unwrap_or(0),
        });
    }
    let usb = le16(prefix, 2).unwrap_or(0);
    let size = prefix.get(7).copied().unwrap_or(0);
    match (usb >= 0x0300, size) {
        (false, 8 | 16 | 32 | 64) => Ok(u16::from(size)),
        (true, 9) => Ok(512),
        _ => Err(DescriptorError::BadPacketSize(size)),
    }
}

/// A device descriptor, decoded and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDescriptor {
    /// bcdUSB: the USB release, binary-coded decimal.
    pub usb: u16,
    /// bDeviceClass: 0 when each interface gives its own.
    pub class: u8,
    /// bDeviceSubClass.
    pub subclass: u8,
    /// bDeviceProtocol.
    pub protocol: u8,
    /// The default control endpoint's packet size, in bytes.
    pub max_packet0: u16,
    /// idVendor.
    pub vendor: u16,
    /// idProduct.
    pub product: u16,
    /// bcdDevice.
    pub release: u16,
    /// bNumConfigurations: at least 1.
    pub configurations: u8,
}

impl DeviceDescriptor {
    /// Decodes a device descriptor from a copy of its 18 bytes.
    ///
    /// # Errors
    ///
    /// [`DescriptorError`] for anything that is not one.
    pub fn decode(bytes: &[u8]) -> Result<Self, DescriptorError> {
        header(bytes, descriptor_type::DEVICE, DEVICE_DESCRIPTOR_LEN)?;
        let max_packet0 = max_packet0(bytes)?;
        let byte = |at: usize| bytes.get(at).copied().unwrap_or(0);
        let word = |at: usize| le16(bytes, at).unwrap_or(0);
        let configurations = byte(17);
        if configurations == 0 {
            return Err(DescriptorError::NoConfigurations);
        }
        Ok(Self {
            usb: word(2),
            class: byte(4),
            subclass: byte(5),
            protocol: byte(6),
            max_packet0,
            vendor: word(8),
            product: word(10),
            release: word(12),
            configurations,
        })
    }
}

/// A configuration descriptor's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigHeader {
    /// wTotalLength: the configuration's bytes, its interfaces' and
    /// endpoints' included; at least 9.
    pub total_len: u16,
    /// bNumInterfaces.
    pub interfaces: u8,
    /// bConfigurationValue: what SET_CONFIGURATION takes.
    pub value: u8,
}

impl ConfigHeader {
    /// Decodes the header from a copy of at least its 9 bytes.
    ///
    /// # Errors
    ///
    /// [`DescriptorError`] for anything that is not one.
    pub fn decode(bytes: &[u8]) -> Result<Self, DescriptorError> {
        header(bytes, descriptor_type::CONFIGURATION, CONFIG_HEADER_LEN)?;
        let total_len = le16(bytes, 2).unwrap_or(0);
        if usize::from(total_len) < CONFIG_HEADER_LEN {
            return Err(DescriptorError::TooShort {
                len: usize::from(total_len),
                needed: CONFIG_HEADER_LEN,
            });
        }
        Ok(Self {
            total_len,
            interfaces: bytes.get(4).copied().unwrap_or(0),
            value: bytes.get(5).copied().unwrap_or(0),
        })
    }
}

/// Which kind of HID interface it is: a boot device (HID 1.11, section
/// 4.3), or one whose reports only its report descriptor lays out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootDevice {
    /// Boot subclass, protocol 1: a keyboard.
    Keyboard,
    /// Boot subclass, protocol 2: a mouse.
    Mouse,
    /// Any other HID interface with a report descriptor: perhaps an
    /// absolute pointer - a touchscreen or a tablet - which reading its
    /// report descriptor tells ([`report`](crate::report)).
    Absolute,
}

/// A HID interface the driver may use - a boot keyboard or mouse, or a
/// candidate absolute pointer, whose report descriptor decides - and its
/// interrupt IN endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootInterface {
    /// bInterfaceNumber.
    pub interface: u8,
    /// A boot keyboard or mouse, or a candidate absolute pointer.
    pub device: BootDevice,
    /// The endpoint's address, IN (bit 7 set), number 1 to 15.
    pub endpoint: u8,
    /// Its wMaxPacketSize: bytes per report, 1 to 1024.
    pub max_packet: u16,
    /// Its bInterval, as the device gave it.
    pub interval: u8,
    /// The length of its report descriptor, from its HID descriptor; 0 if
    /// it gave none.
    pub report_len: u16,
}

/// A configuration's usable HID interfaces: up to [`MAX_BOOT_INTERFACES`],
/// in the order the configuration lists them; when it lists more, boot
/// keyboards and mice are kept before candidate absolute pointers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootInterfaces {
    found: [Option<BootInterface>; MAX_BOOT_INTERFACES],
}

impl BootInterfaces {
    /// The interfaces found.
    pub fn iter(&self) -> impl Iterator<Item = &BootInterface> {
        self.found.iter().flatten()
    }

    /// How many were found.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether none was.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keeps the interfaces at the positions `keep` marks, in order, and
    /// forgets the rest: those that turned out to be of no use.
    pub fn keep(&mut self, keep: [bool; MAX_BOOT_INTERFACES]) {
        let mut kept = Self::default();
        for (interface, wanted) in self.iter().zip(keep) {
            if wanted {
                kept.add(*interface);
            }
        }
        *self = kept;
    }

    /// Forgets the interface numbered `interface`: one that turned out to
    /// be of no use.
    pub fn remove(&mut self, interface: u8) {
        for slot in &mut self.found {
            if slot.is_some_and(|found| found.interface == interface) {
                *slot = None;
            }
        }
    }

    /// Records `interface`, unless its number is already recorded or there
    /// is no room. When there is none, a boot keyboard or mouse takes the
    /// place of the last interface that is only a candidate absolute
    /// pointer: a composite device's keyboard and mouse are never crowded
    /// out by interfaces whose report descriptors may lay out nothing.
    fn add(&mut self, interface: BootInterface) {
        if self
            .iter()
            .any(|found| found.interface == interface.interface)
        {
            return;
        }
        if let Some(slot) = self.found.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(interface);
            return;
        }
        if interface.device == BootDevice::Absolute {
            return;
        }
        if let Some(last) = self
            .found
            .iter()
            .rposition(|slot| slot.is_some_and(|found| found.device == BootDevice::Absolute))
        {
            let mut keep = [true; MAX_BOOT_INTERFACES];
            if let Some(flag) = keep.get_mut(last) {
                *flag = false;
            }
            self.keep(keep);
            self.add(interface);
        }
    }
}

/// Walks a copy of a whole configuration as [`boot_interfaces`] does - its
/// header, then each descriptor after it, every length checked - and hands
/// `visit` each descriptor's offset and bytes (its bLength of them, at least
/// 2). The walk stops at wTotalLength or the end of the bytes given,
/// whichever is first, and a descriptor the buffer cuts short ends it.
///
/// # Errors
///
/// [`DescriptorError`] if the header is not a configuration's, a
/// descriptor's length is under 2 or runs past wTotalLength, or `visit`
/// refuses one.
pub(crate) fn walk(
    config: &[u8],
    mut visit: impl FnMut(usize, &[u8]) -> Result<(), DescriptorError>,
) -> Result<(), DescriptorError> {
    let header = ConfigHeader::decode(config)?;
    let end = usize::from(header.total_len).min(config.len());
    let cut = usize::from(header.total_len) > config.len();
    let mut at = usize::from(config.first().copied().unwrap_or(0));
    while at < end {
        let length = config.get(at).copied().unwrap_or(0);
        let next = at.saturating_add(usize::from(length));
        if length >= 2 && next > end && cut {
            break;
        }
        if length < 2 || next > end {
            return Err(DescriptorError::BadLength { at, length });
        }
        visit(at, config.get(at..next).unwrap_or_default())?;
        at = next;
    }
    Ok(())
}

/// The interface a walk is in: its number, whether it is the first
/// alternate setting, and which boot device it is, if any.
#[derive(Clone, Copy)]
struct Current {
    interface: u8,
    device: Option<BootDevice>,
    report_len: u16,
}

/// Walks a copy of a whole configuration - its header, then every
/// interface, endpoint and class descriptor - and returns the HID
/// interfaces that speak the boot protocol, and every other HID interface
/// whose HID descriptor names a report descriptor ([`BootDevice::Absolute`]),
/// each with its first interrupt IN endpoint. Only alternate
/// setting 0 of an interface counts. At most [`MAX_BOOT_INTERFACES`] are
/// kept: the first, with boot keyboards and mice ahead of candidate
/// absolute pointers.
///
/// The walk stops at the configuration's wTotalLength or the end of the
/// bytes given, whichever is first: a configuration longer than the
/// driver's buffer is read as far as it goes, and a descriptor the buffer
/// cuts short ends the walk.
///
/// # Errors
///
/// [`DescriptorError`] if the header is not a configuration's, or any
/// descriptor's length is under 2, shorter than its type needs, or runs
/// past wTotalLength.
pub fn boot_interfaces(config: &[u8]) -> Result<BootInterfaces, DescriptorError> {
    let header = ConfigHeader::decode(config)?;
    let end = usize::from(header.total_len).min(config.len());
    // The buffer, not wTotalLength, ends the configuration.
    let cut = usize::from(header.total_len) > config.len();
    let mut found = BootInterfaces::default();
    let mut current: Option<Current> = None;
    let mut at = usize::from(config.first().copied().unwrap_or(0));
    while at < end {
        let length = config.get(at).copied().unwrap_or(0);
        let next = at.saturating_add(usize::from(length));
        if length >= 2 && next > end && cut {
            break;
        }
        if length < 2 || next > end {
            return Err(DescriptorError::BadLength { at, length });
        }
        let descriptor = config.get(at..next).unwrap_or_default();
        let byte = |offset: usize| descriptor.get(offset).copied().unwrap_or(0);
        match byte(1) {
            descriptor_type::INTERFACE => {
                if length < 9 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                current = (byte(3) == 0).then(|| Current {
                    interface: byte(2),
                    device: match (byte(5), byte(6), byte(7)) {
                        (CLASS_HID, SUBCLASS_BOOT, 1) => Some(BootDevice::Keyboard),
                        (CLASS_HID, SUBCLASS_BOOT, 2) => Some(BootDevice::Mouse),
                        (CLASS_HID, _, _) => Some(BootDevice::Absolute),
                        _ => None,
                    },
                    report_len: 0,
                });
            }
            descriptor_type::HID => {
                // HID 1.11, section 6.2.1: bNumDescriptors at 5, then a
                // type and a 16-bit length for each, from 6; the report
                // descriptor is type 0x22.
                if length < 9 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                let count = usize::from(byte(5));
                let room = usize::from(length).saturating_sub(6) / 3;
                if let Some(interface) = current.as_mut() {
                    for index in 0..count.min(room) {
                        let entry = index.saturating_mul(3).saturating_add(6);
                        if byte(entry) == REPORT_DESCRIPTOR {
                            interface.report_len =
                                le16(descriptor, entry.saturating_add(1)).unwrap_or(0);
                            break;
                        }
                    }
                }
            }
            descriptor_type::ENDPOINT => {
                if length < 7 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                let address = byte(2);
                let interrupt_in = byte(3) & 0x3 == 0x3 && address & 0x80 != 0;
                let max_packet = le16(descriptor, 4).unwrap_or(0) & 0x7FF;
                if let Some(Current {
                    interface,
                    device: Some(device),
                    report_len,
                }) = current
                    && interrupt_in
                    && address & 0x0F != 0
                    && max_packet != 0
                    && max_packet <= 1024
                    && (device != BootDevice::Absolute || report_len != 0)
                {
                    found.add(BootInterface {
                        interface,
                        device,
                        endpoint: address,
                        max_packet,
                        interval: byte(6),
                        report_len,
                    });
                    // Only an interface's first interrupt IN endpoint.
                    current = Some(Current {
                        interface,
                        device: None,
                        report_len,
                    });
                }
            }
            // HID, class-specific and other descriptors are skipped.
            _ => {}
        }
        at = next;
    }
    Ok(found)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A low-speed boot keyboard's device descriptor (invented values).
    pub(crate) const KEYBOARD_DEVICE: [u8; 18] = [
        18, 1, 0x10, 0x01, 0, 0, 0, 8, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01, 1, 2, 0, 1,
    ];

    /// A configuration with a boot keyboard on interface 0 (endpoint 0x81,
    /// 8 bytes, every 10 ms) and its HID descriptor.
    pub(crate) const KEYBOARD_CONFIG: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 0, 0xA0, 50, // configuration 1
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0: HID, boot, keyboard
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, // HID descriptor
        7, 5, 0x81, 3, 8, 0, 10, // endpoint 0x81, interrupt, 8 bytes, 10 ms
    ];

    /// A receiver presenting a keyboard (interface 0) and a mouse
    /// (interface 1), the mouse with an alternate setting that must be
    /// ignored and an OUT endpoint before its IN one.
    const RECEIVER_CONFIG: [u8; 82] = [
        9, 2, 82, 0, 2, 1, 0, 0xA0, 50, //
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0: keyboard
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
        7, 5, 0x81, 3, 8, 0, 10, //
        9, 4, 1, 0, 2, 3, 1, 2, 0, // interface 1: mouse
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 52, 0, //
        7, 5, 0x02, 3, 8, 0, 10, // an OUT endpoint: not a report source
        7, 5, 0x82, 3, 4, 0, 10, // endpoint 0x82: 4-byte reports
        9, 4, 1, 1, 1, 3, 1, 2, 0, // interface 1, alternate setting 1
        7, 5, 0x83, 3, 64, 0, 1, // ignored
    ];

    #[test]
    fn a_keyboards_descriptors() {
        let device = DeviceDescriptor::decode(&KEYBOARD_DEVICE).unwrap();
        assert_eq!(
            device,
            DeviceDescriptor {
                usb: 0x0110,
                class: 0,
                subclass: 0,
                protocol: 0,
                max_packet0: 8,
                vendor: 0x1234,
                product: 0x5678,
                release: 0x0100,
                configurations: 1,
            }
        );
        assert_eq!(max_packet0(&KEYBOARD_DEVICE[..8]), Ok(8));
        assert_eq!(
            ConfigHeader::decode(&KEYBOARD_CONFIG[..9]),
            Ok(ConfigHeader {
                total_len: 34,
                interfaces: 1,
                value: 1
            })
        );
        let boot = boot_interfaces(&KEYBOARD_CONFIG).unwrap();
        assert_eq!(boot.len(), 1);
        assert_eq!(
            boot.iter().next(),
            Some(&BootInterface {
                interface: 0,
                device: BootDevice::Keyboard,
                endpoint: 0x81,
                max_packet: 8,
                interval: 10,
                report_len: 63,
            })
        );
    }

    #[test]
    fn a_configuration_cut_by_the_buffer_is_read_as_far_as_it_goes() {
        // wTotalLength 82, but only the first 60 bytes fit: the mouse's
        // interrupt endpoint's descriptor, at byte 59, is cut.
        let mut cut = RECEIVER_CONFIG;
        let boot = boot_interfaces(&cut[..60]).unwrap();
        assert_eq!(
            boot.iter().map(|found| found.interface).collect::<Vec<_>>(),
            [0]
        );
        // Cut at a descriptor's boundary, past it: both.
        assert_eq!(boot_interfaces(&cut[..66]).unwrap().len(), 2);
        // A descriptor that runs past wTotalLength itself is refused.
        cut[2] = 60;
        assert_eq!(
            boot_interfaces(&cut[..]),
            Err(DescriptorError::BadLength { at: 59, length: 7 })
        );
    }

    #[test]
    fn a_receivers_two_interfaces() {
        let boot = boot_interfaces(&RECEIVER_CONFIG).unwrap();
        let found: Vec<_> = boot.iter().copied().collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].device, BootDevice::Keyboard);
        assert_eq!(
            found[1],
            BootInterface {
                interface: 1,
                device: BootDevice::Mouse,
                endpoint: 0x82,
                max_packet: 4,
                interval: 10,
                report_len: 52,
            }
        );
    }

    /// A receiver whose second interface is a HID interface outside the
    /// boot subclass - consumer controls, say - between its keyboard and
    /// its mouse.
    pub(crate) const COMPOSITE_CONFIG: [u8; 84] = [
        9, 2, 84, 0, 3, 1, 0, 0xA0, 50, //
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0: keyboard
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
        7, 5, 0x81, 3, 8, 0, 10, //
        9, 4, 1, 0, 1, 3, 0, 0, 0, // interface 1: HID, no subclass
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 40, 0, //
        7, 5, 0x82, 3, 8, 0, 10, //
        9, 4, 2, 0, 1, 3, 1, 2, 0, // interface 2: mouse
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 52, 0, //
        7, 5, 0x83, 3, 4, 0, 10, //
    ];

    /// The configuration a wireless keyboard-and-mouse receiver gave the
    /// USB host PD on the Pi 5, read over USB (2026-10-07): a boot keyboard,
    /// a boot mouse with 10-byte packets every frame, and a third HID
    /// interface outside the boot subclass with a 319-byte report
    /// descriptor.
    const PI_RECEIVER_CONFIG: [u8; 84] = [
        0x09, 0x02, 0x54, 0x00, 0x03, 0x01, 0x00, 0xa0, 0x32, //
        0x09, 0x04, 0x00, 0x00, 0x01, 0x03, 0x01, 0x01, 0x00, //
        0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x39, 0x00, //
        0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x04, //
        0x09, 0x04, 0x01, 0x00, 0x01, 0x03, 0x01, 0x02, 0x00, //
        0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0xdf, 0x00, //
        0x07, 0x05, 0x82, 0x03, 0x0a, 0x00, 0x01, //
        0x09, 0x04, 0x02, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, //
        0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x3f, 0x01, //
        0x07, 0x05, 0x83, 0x03, 0x20, 0x00, 0x01, //
    ];

    #[test]
    fn the_receiver_tried_on_the_pi() {
        let boot = boot_interfaces(&PI_RECEIVER_CONFIG).unwrap();
        let found: Vec<_> = boot.iter().copied().collect();
        assert_eq!(
            found,
            [
                BootInterface {
                    interface: 0,
                    device: BootDevice::Keyboard,
                    endpoint: 0x81,
                    max_packet: 8,
                    interval: 4,
                    report_len: 57,
                },
                BootInterface {
                    interface: 1,
                    device: BootDevice::Mouse,
                    endpoint: 0x82,
                    max_packet: 10,
                    interval: 1,
                    report_len: 223,
                },
            ],
            "the third interface, a candidate, finds no place"
        );
    }

    #[test]
    fn a_composite_devices_keyboard_and_mouse_come_before_other_hid_interfaces() {
        let boot = boot_interfaces(&COMPOSITE_CONFIG).unwrap();
        let found: Vec<_> = boot
            .iter()
            .map(|found| (found.interface, found.device))
            .collect();
        assert_eq!(
            found,
            [(0, BootDevice::Keyboard), (2, BootDevice::Mouse)],
            "the mouse displaces the candidate on interface 1"
        );
        // With room, the candidate stays, in the configuration's order.
        let mut two = BootInterfaces::default();
        two.add(boot.iter().copied().next().unwrap());
        let candidate = BootInterface {
            interface: 1,
            device: BootDevice::Absolute,
            endpoint: 0x82,
            max_packet: 8,
            interval: 10,
            report_len: 40,
        };
        two.add(candidate);
        assert_eq!(two.len(), 2);
        // A candidate never displaces a boot device.
        two.add(BootInterface {
            interface: 3,
            ..candidate
        });
        assert_eq!(
            two.iter().map(|found| found.interface).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    /// A tablet: a HID interface outside the boot subclass, its report
    /// descriptor 74 bytes long.
    pub(crate) const TABLET_CONFIG: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 0, 0xA0, 50, //
        9, 4, 0, 0, 1, 3, 0, 0, 0, // interface 0: HID, no subclass
        9, 0x21, 0x01, 0x01, 0, 1, 0x22, 74, 0, //
        7, 5, 0x81, 3, 8, 0, 10, //
    ];

    #[test]
    fn a_tablets_interface_comes_with_its_report_descriptors_length() {
        let boot = boot_interfaces(&TABLET_CONFIG).unwrap();
        let found: Vec<_> = boot.iter().copied().collect();
        assert_eq!(
            found,
            [BootInterface {
                interface: 0,
                device: BootDevice::Absolute,
                endpoint: 0x81,
                max_packet: 8,
                interval: 10,
                report_len: 74,
            }]
        );
        // With no report descriptor named, it is of no use: not kept.
        let mut no_report = TABLET_CONFIG;
        no_report[24] = 0x23;
        assert!(boot_interfaces(&no_report).unwrap().is_empty());
        // A HID descriptor shorter than its type needs.
        let mut short = TABLET_CONFIG;
        short[18] = 6;
        short[19] = 0x21;
        assert!(boot_interfaces(&short).is_err());
        let mut both = boot;
        both.remove(0);
        assert!(both.is_empty());
    }

    #[test]
    fn a_configuration_cut_short_is_read_as_far_as_it_goes() {
        // The keyboard's configuration claims 34 bytes; only 27 arrive: the
        // interface is seen, its endpoint is not.
        let boot = boot_interfaces(&KEYBOARD_CONFIG[..27]).unwrap();
        assert!(boot.is_empty());
    }

    #[test]
    fn bad_descriptors_are_refused() {
        let mut device = KEYBOARD_DEVICE;
        device[7] = 9;
        assert_eq!(
            DeviceDescriptor::decode(&device),
            Err(DescriptorError::BadPacketSize(9))
        );
        device[2..4].copy_from_slice(&0x0300u16.to_le_bytes());
        assert_eq!(max_packet0(&device), Ok(512));
        let mut device = KEYBOARD_DEVICE;
        device[17] = 0;
        assert_eq!(
            DeviceDescriptor::decode(&device),
            Err(DescriptorError::NoConfigurations)
        );
        device[1] = 2;
        assert_eq!(
            DeviceDescriptor::decode(&device),
            Err(DescriptorError::WrongType {
                found: 2,
                expected: 1
            })
        );
        assert_eq!(
            DeviceDescriptor::decode(&KEYBOARD_DEVICE[..17]),
            Err(DescriptorError::TooShort {
                len: 17,
                needed: 18
            })
        );
        assert_eq!(
            max_packet0(&[8, 1, 0, 2, 0, 0, 0, 64]),
            Err(DescriptorError::BadLength { at: 0, length: 8 })
        );
        // A descriptor of length 0 would loop for ever: refused.
        let mut config = KEYBOARD_CONFIG;
        config[18] = 0;
        assert_eq!(
            boot_interfaces(&config),
            Err(DescriptorError::BadLength { at: 18, length: 0 })
        );
        // One that runs past wTotalLength.
        let mut config = KEYBOARD_CONFIG;
        config[27] = 8;
        assert_eq!(
            boot_interfaces(&config),
            Err(DescriptorError::BadLength { at: 27, length: 8 })
        );
        // A short interface descriptor.
        let mut config = KEYBOARD_CONFIG;
        config[9] = 5;
        config[14] = 4;
        assert!(boot_interfaces(&config).is_err());
        let mut config = KEYBOARD_CONFIG;
        config[2] = 8;
        assert!(boot_interfaces(&config).is_err());
    }

    proptest! {
        #[test]
        fn decoding_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = DeviceDescriptor::decode(&bytes);
            let _ = max_packet0(&bytes);
            let _ = ConfigHeader::decode(&bytes);
            if let Ok(boot) = boot_interfaces(&bytes) {
                prop_assert!(boot.len() <= MAX_BOOT_INTERFACES);
                for interface in boot.iter() {
                    prop_assert!(interface.endpoint & 0x80 != 0 && interface.endpoint & 0x0F != 0);
                    prop_assert!((1..=1024).contains(&interface.max_packet));
                }
            }
        }

        #[test]
        fn a_configuration_with_a_byte_changed_never_panics(at in 0usize..82, value: u8, cut in 0usize..83) {
            let mut config = RECEIVER_CONFIG;
            config[at] = value;
            let _ = boot_interfaces(&config[..cut]);
        }
    }
}
