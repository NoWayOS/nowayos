// SPDX-License-Identifier: MIT OR Apache-2.0
//! Control requests: the 8-byte setup packet (USB 2.0, section 9.3), the
//! standard requests a host needs to enumerate a device (section 9.4), and
//! the HID class requests that put a keyboard or mouse in its boot protocol
//! (HID 1.11, section 7.2); and, for audio streams, the audio
//! class request that sets an endpoint's sampling frequency (Audio 1.0,
//! sections 5.2.3.2.1 and 5.2.3.2.3.1), whose Data Stage the host sends.

/// Descriptor types (USB 2.0, table 9-5).
pub mod descriptor_type {
    /// DEVICE.
    pub const DEVICE: u8 = 1;
    /// CONFIGURATION.
    pub const CONFIGURATION: u8 = 2;
    /// INTERFACE.
    pub const INTERFACE: u8 = 4;
    /// ENDPOINT.
    pub const ENDPOINT: u8 = 5;
    /// HID (HID 1.11, section 7.1).
    pub const HID: u8 = 0x21;
    /// A HID report descriptor (HID 1.11, section 7.1).
    pub const REPORT: u8 = 0x22;
}

/// A setup packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    /// bmRequestType: direction, type and recipient.
    pub request_type: u8,
    /// bRequest.
    pub request: u8,
    /// wValue.
    pub value: u16,
    /// wIndex.
    pub index: u16,
    /// wLength: the bytes of the Data Stage.
    pub length: u16,
}

/// bmRequestType: device to host, standard, device.
const IN_STANDARD_DEVICE: u8 = 0x80;
/// bmRequestType: host to device, standard, device.
const OUT_STANDARD_DEVICE: u8 = 0x00;
/// bmRequestType: device to host, standard, interface.
const IN_STANDARD_INTERFACE: u8 = 0x81;
/// bmRequestType: host to device, standard, interface.
const OUT_STANDARD_INTERFACE: u8 = 0x01;
/// bmRequestType: host to device, class, interface.
const OUT_CLASS_INTERFACE: u8 = 0x21;

/// GET_DESCRIPTOR.
const GET_DESCRIPTOR: u8 = 6;
/// SET_CONFIGURATION.
const SET_CONFIGURATION: u8 = 9;
/// SET_INTERFACE.
const SET_INTERFACE: u8 = 11;
/// HID SET_IDLE.
const SET_IDLE: u8 = 0x0A;
/// HID SET_PROTOCOL.
const SET_PROTOCOL: u8 = 0x0B;

impl Setup {
    /// The packet's bytes, little-endian as on the wire.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 8] {
        let [v0, v1] = self.value.to_le_bytes();
        let [i0, i1] = self.index.to_le_bytes();
        let [l0, l1] = self.length.to_le_bytes();
        [self.request_type, self.request, v0, v1, i0, i1, l0, l1]
    }

    /// Whether the Data Stage, if any, is from the device.
    #[must_use]
    pub const fn inbound(self) -> bool {
        self.request_type & 0x80 != 0
    }

    /// GET_DESCRIPTOR of descriptor `kind`, number `index`, `length` bytes.
    #[must_use]
    pub const fn get_descriptor(kind: u8, index: u8, length: u16) -> Self {
        Self {
            request_type: IN_STANDARD_DEVICE,
            request: GET_DESCRIPTOR,
            value: u16::from_be_bytes([kind, index]),
            index: 0,
            length,
        }
    }

    /// GET_DESCRIPTOR of interface `interface`'s report descriptor,
    /// `length` bytes (HID 1.11, section 7.1.1: a standard request to the
    /// interface).
    #[must_use]
    pub const fn get_report_descriptor(interface: u8, length: u16) -> Self {
        Self {
            request_type: IN_STANDARD_INTERFACE,
            request: GET_DESCRIPTOR,
            value: u16::from_be_bytes([descriptor_type::REPORT, 0]),
            index: interface as u16,
            length,
        }
    }

    /// SET_CONFIGURATION to configuration `value`.
    #[must_use]
    pub const fn set_configuration(value: u8) -> Self {
        Self {
            request_type: OUT_STANDARD_DEVICE,
            request: SET_CONFIGURATION,
            value: value as u16,
            index: 0,
            length: 0,
        }
    }

    /// SET_INTERFACE: interface `interface` to its alternate setting
    /// `alternate` (USB 2.0, section 9.4.10) - an audio streaming
    /// interface's setting with an isochronous endpoint, say.
    #[must_use]
    pub const fn set_interface(interface: u8, alternate: u8) -> Self {
        Self {
            request_type: OUT_STANDARD_INTERFACE,
            request: SET_INTERFACE,
            value: alternate as u16,
            index: interface as u16,
            length: 0,
        }
    }

    /// HID SET_PROTOCOL to the boot protocol (0), for `interface`.
    #[must_use]
    pub const fn set_boot_protocol(interface: u8) -> Self {
        Self {
            request_type: OUT_CLASS_INTERFACE,
            request: SET_PROTOCOL,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }

    /// HID SET_IDLE with duration 0, for `interface`: report only when
    /// something changes (HID 1.11, section 7.2.4).
    #[must_use]
    pub const fn set_idle(interface: u8) -> Self {
        Self {
            request_type: OUT_CLASS_INTERFACE,
            request: SET_IDLE,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }
}

/// bmRequestType: host to device, class, endpoint.
const OUT_CLASS_ENDPOINT: u8 = 0x22;
/// The audio class's SET_CUR (Audio 1.0, table A-9).
const SET_CUR: u8 = 0x01;
/// SAMPLING_FREQ_CONTROL, an endpoint control selector (Audio 1.0, table
/// A-19).
const SAMPLING_FREQ_CONTROL: u8 = 0x01;

/// The most bytes a request this driver makes sends in its Data Stage.
pub const OUT_DATA_MAX: usize = 64;

/// The bytes a request sends in its Data Stage: 1 to
/// [`OUT_DATA_MAX`] of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutData {
    bytes: [u8; OUT_DATA_MAX],
    len: u8,
}

impl OutData {
    /// `bytes`, if there are 1 to [`OUT_DATA_MAX`] of them.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Option<Self> {
        let mut data = Self {
            bytes: [0; OUT_DATA_MAX],
            len: u8::try_from(bytes.len()).ok().filter(|len| *len > 0)?,
        };
        data.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(data)
    }

    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }
}

impl Setup {
    /// The audio class's SET_CUR of isochronous endpoint `endpoint`'s
    /// Sampling Frequency control to `rate` hertz, and the 3 bytes its Data
    /// Stage sends, tSampleFreq, little-endian (Audio 1.0, sections
    /// 5.2.3.2.1 and 5.2.3.2.3.1). A rate above 0x7FFFFF, the control's
    /// most, is sent as that.
    #[must_use]
    pub fn set_sampling_frequency(endpoint: u8, rate: u32) -> (Self, OutData) {
        let [low, middle, high, _] = rate.min(0x7F_FFFF).to_le_bytes();
        let mut bytes = [0; OUT_DATA_MAX];
        if let Some(head) = bytes.get_mut(..3) {
            head.copy_from_slice(&[low, middle, high]);
        }
        let setup = Self {
            request_type: OUT_CLASS_ENDPOINT,
            request: SET_CUR,
            value: u16::from_be_bytes([SAMPLING_FREQ_CONTROL, 0]),
            index: endpoint as u16,
            length: 3,
        };
        (setup, OutData { bytes, len: 3 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sampling_frequency_request_and_its_data() {
        let (setup, data) = Setup::set_sampling_frequency(0x84, 44_100);
        assert_eq!(setup.to_bytes(), [0x22, 0x01, 0x00, 0x01, 0x84, 0, 3, 0]);
        assert!(!setup.inbound());
        assert_eq!(data.bytes(), [0x44, 0xAC, 0x00]);
        assert_eq!(
            Setup::set_sampling_frequency(1, u32::MAX).1.bytes(),
            [0xFF, 0xFF, 0x7F]
        );
        assert_eq!(OutData::new(&[]), None);
        assert_eq!(OutData::new(&[0; OUT_DATA_MAX + 1]), None);
        assert_eq!(
            OutData::new(&[1, 2]).map(|data| data.bytes().len()),
            Some(2)
        );
    }

    #[test]
    fn packets_on_the_wire() {
        assert_eq!(
            Setup::get_descriptor(descriptor_type::DEVICE, 0, 18).to_bytes(),
            [0x80, 6, 0, 1, 0, 0, 18, 0]
        );
        assert_eq!(
            Setup::get_descriptor(descriptor_type::CONFIGURATION, 0, 0x22).to_bytes(),
            [0x80, 6, 0, 2, 0, 0, 0x22, 0]
        );
        assert_eq!(
            Setup::set_configuration(1).to_bytes(),
            [0, 9, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            Setup::set_boot_protocol(1).to_bytes(),
            [0x21, 0x0B, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(
            Setup::set_idle(0).to_bytes(),
            [0x21, 0x0A, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            Setup::set_interface(1, 2).to_bytes(),
            [0x01, 0x0B, 2, 0, 1, 0, 0, 0]
        );
        assert!(Setup::get_descriptor(1, 0, 8).inbound());
        assert!(!Setup::set_configuration(1).inbound());
    }
}
