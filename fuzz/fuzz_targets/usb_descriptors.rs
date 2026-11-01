// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: what a USB device says about itself, as the USB host PD
//! reads it while it enumerates the device - its device and
//! configuration descriptors, copied out of DMA memory - and the
//! enumeration those answers drive (`nowayos_usb::enumerate`).
//!
//! A device is not trusted, so the bytes are either the fuzzer's as they
//! are, or a keyboard-and-mouse receiver's configuration - with or without
//! another HID interface between the two, invented as `nowayos-usb`'s
//! tests lay them out - with some bytes changed and cut to any length, so the descriptor walk's checks are reached from the first
//! inputs. Checks:
//!
//! * a device descriptor that decodes has a control packet size USB allows
//!   (8, 16, 32, 64, or 512 for USB 3) and at least one configuration;
//! * the walk of a configuration never panics, never reads past the bytes
//!   given or the length the configuration claims, and every interface it
//!   finds - a boot keyboard or mouse, or another HID interface that names
//!   a report descriptor, a candidate absolute pointer - has an interrupt
//!   IN endpoint numbered 1 to 15 and a packet size of 1 to 1024 bytes, at
//!   most two of them, each interface once, and a candidate is never kept
//!   in place of a boot keyboard or mouse;
//! * the walk for audio streams never panics, and every stream it
//!   finds is an alternate setting other than 0 with an isochronous endpoint
//!   numbered 1 to 15, 1 to 1024 bytes a packet and 1 to 3 transactions,
//!   at most two of them, each interface once, and any format it gives has
//!   1 to 8 channels of 1 to 4 bytes, its bits fitting, at a rate above 0;
//!   a speaker's configuration, invented as `nowayos-usb`'s tests lay it
//!   out, is among the bases changed; and for a device at either
//!   speed and any rate asked for, a stream not refused has a format of one
//!   or two channels and runs at a rate from 8 to 192 kHz that its format
//!   takes, a refused one at none, and a feedback endpoint is an IN
//!   endpoint of at least 3 bytes for an OUT stream - an asynchronous
//!   speaker's configuration, with two settings and a feedback endpoint, is
//!   among the bases too; and a feedback value read from any bytes is
//!   within an eighth of the stream's own rate, or refused;
//! * an enumeration fed any sequence of outcomes - the fuzzer's bytes as
//!   the descriptors among them - never panics, asks to poll only with a
//!   slot and a boot interface or, for a machine that streams, an audio
//!   stream selected or a camera's video streaming, none of them
//!   refused, sends data to the device only to set a sampling frequency,
//!   its 3 bytes (this machine looks for no camera, so sends no probe),
//!   and once stopped stays stopped.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_usb::audio::{self, MAX_CHANNELS, MAX_RATE, MAX_STREAMS, MIN_RATE};
use nowayos_usb::descriptor::{
    self, BootDevice, ConfigHeader, DeviceDescriptor, MAX_BOOT_INTERFACES, boot_interfaces,
};
use nowayos_usb::enumerate::{Enumeration, Outcome, Step, Video};
use nowayos_usb::port::Speed;
use nowayos_usb::trb::Completion;

/// A receiver presenting a keyboard (interface 0) and a mouse (interface
/// 1), with HID descriptors, an OUT endpoint and an alternate setting.
const RECEIVER: [u8; 82] = [
    9, 2, 82, 0, 2, 1, 0, 0xA0, 50, //
    9, 4, 0, 0, 1, 3, 1, 1, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
    7, 5, 0x81, 3, 8, 0, 10, //
    9, 4, 1, 0, 2, 3, 1, 2, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 52, 0, //
    7, 5, 0x02, 3, 8, 0, 10, //
    7, 5, 0x82, 3, 4, 0, 10, //
    9, 4, 1, 1, 1, 3, 1, 2, 0, //
    7, 5, 0x83, 3, 64, 0, 1, //
];

/// A receiver whose second interface is a HID interface outside the boot
/// subclass, between its keyboard and its mouse.
const COMPOSITE: [u8; 84] = [
    9, 2, 84, 0, 3, 1, 0, 0xA0, 50, //
    9, 4, 0, 0, 1, 3, 1, 1, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
    7, 5, 0x81, 3, 8, 0, 10, //
    9, 4, 1, 0, 1, 3, 0, 0, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 40, 0, //
    7, 5, 0x82, 3, 8, 0, 10, //
    9, 4, 2, 0, 1, 3, 1, 2, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 52, 0, //
    7, 5, 0x83, 3, 4, 0, 10, //
];

/// A full-speed USB speaker: an audio control interface and a streaming
/// interface whose alternate setting 1 takes 48 kHz 16-bit stereo PCM on
/// isochronous OUT endpoint 1.
const SPEAKER: [u8; 100] = [
    9, 2, 100, 0, 2, 1, 0, 0x80, 50, //
    9, 4, 0, 0, 0, 1, 1, 0, 0, //
    9, 0x24, 1, 0x00, 0x01, 30, 0, 1, 1, //
    12, 0x24, 2, 1, 0x01, 0x01, 0, 2, 3, 0, 0, 0, //
    9, 0x24, 3, 2, 0x01, 0x03, 0, 1, 0, //
    9, 4, 1, 0, 0, 1, 2, 0, 0, //
    9, 4, 1, 1, 1, 1, 2, 0, 0, //
    7, 0x24, 1, 1, 1, 0x01, 0x00, //
    11, 0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xBB, 0x00, //
    9, 5, 0x01, 0x09, 192, 0, 1, 0, 0, //
    7, 0x25, 1, 0x00, 0, 0, 0, //
];

/// A full-speed headset's asynchronous speaker: alternate 1 24-bit at
/// 48 kHz; alternate 2 16-bit at 44.1 or 48 kHz with feedback endpoint
/// 0x82 and a sampling frequency control.
const ASYNC_SPEAKER: [u8; 116] = [
    9, 2, 116, 0, 2, 1, 0, 0x80, 50, //
    9, 4, 1, 0, 0, 1, 2, 0, 0, //
    9, 4, 1, 1, 1, 1, 2, 0, 0, //
    7, 0x24, 1, 1, 1, 0x01, 0x00, //
    11, 0x24, 2, 1, 2, 3, 24, 1, 0x80, 0xBB, 0x00, //
    9, 5, 0x01, 0x05, 32, 1, 1, 0, 0, //
    7, 0x25, 1, 0x00, 0, 0, 0, //
    9, 4, 1, 2, 2, 1, 2, 0, 0, //
    7, 0x24, 1, 1, 1, 0x01, 0x00, //
    14, 0x24, 2, 1, 2, 2, 16, 2, 0x44, 0xAC, 0x00, 0x80, 0xBB, 0x00, //
    9, 5, 0x01, 0x05, 200, 0, 1, 0, 0x82, //
    7, 0x25, 1, 0x01, 0, 0, 0, //
    9, 5, 0x82, 0x11, 3, 0, 1, 5, 0, //
];

/// A boot keyboard's device descriptor.
const DEVICE: [u8; 18] = [
    18, 1, 0x10, 0x01, 0, 0, 0, 8, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01, 1, 2, 0, 1,
];

/// How many usable boot keyboard and mouse interfaces a configuration
/// lists, at most two: the walk's own result on a copy whose every other
/// HID interface is made vendor-specific, so none can take a place. `None`
/// when an interface number repeats among interfaces at alternate setting
/// 0 - malformed, and which of the two counts is then the walk's choice.
fn boot_devices_listed(config: &[u8]) -> Option<usize> {
    let mut copy = [0u8; 600];
    let len = config.len().min(copy.len());
    for (to, from) in copy.iter_mut().zip(config) {
        *to = *from;
    }
    let mut numbers = [false; 256];
    let mut at = usize::from(copy.first().copied().unwrap_or(0));
    while at < len {
        let length = copy.get(at).copied().unwrap_or(0);
        if length < 2 {
            break;
        }
        let byte = |offset: usize| copy.get(at.saturating_add(offset)).copied();
        if length >= 9 && byte(1) == Some(4) && byte(3) == Some(0) {
            let seen = numbers.get_mut(usize::from(byte(2).unwrap_or(0)))?;
            if *seen {
                return None;
            }
            *seen = true;
            let boot = byte(6) == Some(1) && matches!(byte(7), Some(1 | 2));
            if byte(5) == Some(3)
                && !boot
                && let Some(class) = copy.get_mut(at.saturating_add(5))
            {
                *class = 0xFF;
            }
        }
        at = at.saturating_add(usize::from(length));
    }
    Some(boot_interfaces(copy.get(..len).unwrap_or_default()).map_or(0, |boot| boot.len()))
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; 600];
    let pick = input.byte() % 6;
    let len = if pick == 0 {
        let rest = input.rest();
        let len = rest.len().min(buf.len());
        for (to, from) in buf.iter_mut().zip(rest) {
            *to = *from;
        }
        len
    } else {
        let base: &[u8] = match pick {
            3 => &COMPOSITE,
            4 => &SPEAKER,
            5 => &ASYNC_SPEAKER,
            _ => &RECEIVER,
        };
        for (to, from) in buf.iter_mut().zip(base) {
            *to = *from;
        }
        for _ in 0..input.byte() % 8 {
            let at = usize::from(input.byte())
                .checked_rem(base.len())
                .unwrap_or(0);
            if let Some(byte) = buf.get_mut(at) {
                *byte = input.byte();
            }
        }
        usize::from(input.byte()).min(base.len())
    };
    let bytes = buf.get(..len).unwrap_or_default();

    if let Ok(device) = DeviceDescriptor::decode(bytes) {
        assert!(matches!(device.max_packet0, 8 | 16 | 32 | 64 | 512));
        assert!(device.configurations >= 1);
    }
    let _ = descriptor::max_packet0(bytes);
    let header = ConfigHeader::decode(bytes);
    if let Ok(boot) = boot_interfaces(bytes) {
        assert!(header.is_ok(), "a walk with no header");
        assert!(boot.len() <= MAX_BOOT_INTERFACES);
        let mut seen = [None; MAX_BOOT_INTERFACES];
        for (slot, interface) in seen.iter_mut().zip(boot.iter()) {
            assert!(interface.endpoint & 0x80 != 0);
            assert!((1..=15).contains(&(interface.endpoint & 0x0F)));
            assert!((1..=1024).contains(&interface.max_packet));
            if interface.device == BootDevice::Absolute {
                assert!(
                    interface.report_len != 0,
                    "a candidate with no report descriptor"
                );
            }
            *slot = Some(interface.interface);
        }
        // Full, a candidate kept means no boot device was left out: walk
        // again counting the boot devices the configuration lists.
        let candidates = boot
            .iter()
            .filter(|found| found.device == BootDevice::Absolute)
            .count();
        if candidates > 0 && boot.len() == MAX_BOOT_INTERFACES {
            let kept_boot = boot.len().saturating_sub(candidates);
            assert!(
                boot_devices_listed(bytes).is_none_or(|listed| listed <= kept_boot),
                "a boot keyboard or mouse crowded out"
            );
        }
        if let [Some(first), Some(second)] = seen {
            assert_ne!(first, second, "one interface twice");
        }
    }

    let speed = if input.byte() & 1 == 0 {
        Speed::Full
    } else {
        Speed::High
    };
    let wanted = input.u32().checked_rem(400_000).unwrap_or(0);
    if let Ok(streams) = audio::streams_at(bytes, speed, wanted) {
        assert!(header.is_ok(), "a walk with no header");
        assert!(streams.len() <= MAX_STREAMS);
        let mut seen = [None; MAX_STREAMS];
        for (slot, stream) in seen.iter_mut().zip(streams.iter()) {
            assert!(stream.alternate != 0);
            assert!((1..=15).contains(&(stream.endpoint & 0x0F)));
            assert!((1..=1024).contains(&stream.max_packet));
            assert!((1..=3).contains(&stream.transactions));
            if let Some(format) = stream.format {
                assert!((1..=8).contains(&format.channels));
                assert!((1..=4).contains(&format.subframe));
                assert!(
                    format.bits != 0
                        && u32::from(format.bits) <= u32::from(format.subframe).saturating_mul(8)
                );
                assert!(format.rates.iter().all(|rate| rate != 0));
            }
            assert!(
                stream.refusal.is_some() || stream.format.is_some(),
                "a stream streamed with no format"
            );
            if stream.refusal.is_some() {
                assert_eq!(stream.rate, 0, "a refused stream with a rate");
            }
            match (stream.refusal, stream.format) {
                (Some(_), _) | (None, None) => {}
                (None, Some(format)) => {
                    assert!((1..=MAX_CHANNELS).contains(&format.channels));
                    assert!((MIN_RATE..=MAX_RATE).contains(&stream.rate));
                    assert!(format.rates.takes(stream.rate));
                    let interval_us = nowayos_usb::context::interval_micros(
                        nowayos_usb::context::isoch_interval(speed, stream.interval),
                    );
                    let nominal = audio::nominal_q16(stream.rate, interval_us);
                    if let Some(value) = audio::feedback_q16(
                        input.rest().get(..3).unwrap_or(&[]),
                        speed,
                        interval_us,
                        nominal,
                    ) {
                        assert!(
                            value.abs_diff(nominal) <= nominal >> 3,
                            "feedback far from the rate"
                        );
                    }
                }
            }
            if let Some(feedback) = stream.feedback {
                assert!(feedback.endpoint & 0x80 != 0 && !stream.inbound());
                assert!(feedback.max_packet >= 3);
            }
            *slot = Some(stream.interface);
        }
        if let [Some(first), Some(second)] = seen {
            assert_ne!(first, second, "one interface twice");
        }
    }

    // An enumeration, its descriptors the fuzzer's bytes now and then; by
    // a machine that streams, or one that does not.
    let port = (input.byte() & 3).saturating_add(1);
    let streaming = input.byte() & 1 != 0;
    let (mut machine, _) = if streaming {
        Enumeration::start_streaming(port, None)
    } else {
        Enumeration::start(port, None)
    };
    machine.want_rate(wanted);
    let mut stopped = false;
    for _ in 0..24 {
        if input.is_empty() {
            break;
        }
        let value = input.byte();
        let outcome = match input.byte().checked_rem(10).unwrap_or(0) {
            0 => Outcome::PortEnabled(
                Speed::from_id(value.checked_rem(6).unwrap_or(0)).unwrap_or(Speed::Full),
            ),
            1 => Outcome::SlotEnabled(value),
            2 => Outcome::CommandDone,
            3 => Outcome::CommandFailed(Completion(value)),
            4 => Outcome::ControlData(bytes.get(..usize::from(value)).unwrap_or(bytes)),
            5 => Outcome::ControlDone,
            6 => Outcome::ControlFailed(Completion(value)),
            7 => Outcome::TimedOut,
            8 => Outcome::ControlData(&DEVICE),
            _ => Outcome::ControlData(&RECEIVER),
        };
        let step = machine.on(outcome);
        if stopped {
            assert!(matches!(step, Step::Stop(_)), "{step:?} after a stop");
        }
        match step {
            Step::Stop(_) => {
                stopped = true;
                assert!(machine.finished() && !machine.polling());
            }
            Step::ControlOut(setup, data) => {
                assert!(streaming, "data sent by a machine that does not stream");
                assert_eq!(
                    (setup.request_type, setup.request, setup.value),
                    (0x22, 0x01, 0x0100)
                );
                assert_eq!(data.bytes().len(), 3, "a rate is 3 bytes");
                if let Ok([low, middle, high]) = <[u8; 3]>::try_from(data.bytes()) {
                    let rate = u32::from_le_bytes([low, middle, high, 0]);
                    assert!(machine.usable().any(|stream| {
                        u16::from(stream.endpoint) == setup.index && stream.rate == rate
                    }));
                }
            }
            Step::Poll => {
                assert!(machine.polling() && machine.slot() != 0);
                assert!(machine.selected().all(|stream| stream.refusal.is_none()));
                if streaming {
                    assert!(
                        !machine.boot().is_empty()
                            || machine.selected().count() > 0
                            || matches!(machine.video(), Video::Streaming(..))
                    );
                } else {
                    assert!(!machine.boot().is_empty() && machine.streams().is_empty());
                }
            }
            _ => {}
        }
    }
});
