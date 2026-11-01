// SPDX-License-Identifier: MIT OR Apache-2.0
//! A camera's VideoStreaming interface, from a copy of its whole
//! configuration: the Motion JPEG frame sizes and rates it offers,
//! the alternate settings of its video endpoint, and the size, rate and
//! setting chosen.
//!
//! From UVC 1.5: the class-specific VC interface header (table 3-3, for the
//! release the camera follows), the VS interface input header (table 3-14,
//! for the video endpoint's address), the rule that format and frame
//! descriptors sit in alternate setting 0 and that an isochronous
//! interface's alternate setting 0 has no bandwidth (section 2.4.3), and the
//! standard VS isochronous and bulk video data endpoints (section 3.10.1);
//! from the Motion-JPEG Payload 1.5: the format descriptor (table 3-1) and
//! the frame descriptors after it (tables 3-2 to 3-4); and from USB 2.0's
//! endpoint descriptor, as `audio` reads it, wMaxPacketSize's packet size
//! and, at high speed, its transactions each microframe.
//!
//! **Within fixed bounds.** The walk keeps at most [`MAX_FRAMES`] frame
//! sizes, [`MAX_RATES`] rates each and [`MAX_ALTERNATES`] alternate
//! settings, the first of each, and counts the frame sizes it could not
//! keep; a frame descriptor too short for the rates it claims, or with a
//! size or interval of 0, is left out. A short interface or endpoint
//! descriptor refuses the configuration, as the other walks do.

use core::fmt;

use super::{CLASS_VIDEO, CS_INTERFACE, INTERVAL_30, Rate, SUBCLASS_CONTROL, SUBCLASS_STREAMING};
use crate::audio::Stream;
use crate::descriptor::{DescriptorError, walk};
use crate::request::descriptor_type;
use crate::{le16, le32};

/// VC_HEADER, the class-specific VC interface header's subtype (UVC 1.5,
/// table A-5).
const VC_HEADER: u8 = 0x01;

/// VS_INPUT_HEADER (UVC 1.5, table A-6).
const VS_INPUT_HEADER: u8 = 0x01;

/// VS_FORMAT_MJPEG (UVC 1.5, table A-6).
const VS_FORMAT_MJPEG: u8 = 0x06;

/// VS_FRAME_MJPEG (UVC 1.5, table A-6).
const VS_FRAME_MJPEG: u8 = 0x07;

/// Every format descriptor's subtype (UVC 1.5, table A-6): uncompressed,
/// Motion JPEG, MPEG-2 TS, DV, frame-based, stream-based, H.264, H.264
/// simulcast, VP8 and VP8 simulcast. Another format's descriptor ends the
/// Motion JPEG format's frames.
const FORMATS: [u8; 10] = [0x04, 0x06, 0x0A, 0x0C, 0x10, 0x12, 0x13, 0x15, 0x16, 0x18];

/// The most Motion JPEG frame sizes kept.
pub const MAX_FRAMES: usize = 16;

/// The most discrete rates kept for each frame size.
pub const MAX_RATES: usize = 8;

/// The most alternate settings of the video endpoint kept.
pub const MAX_ALTERNATES: usize = 8;

/// The frame size chosen where a camera offers it.
pub const PREFERRED: (u16, u16) = (1280, 720);

/// The frame intervals a frame size offers, in 100 ns units (Motion-JPEG
/// Payload 1.5, tables 3-3 and 3-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rates {
    /// A list: the first [`MAX_RATES`], each at least 1.
    Discrete {
        /// The intervals, `count` of them in use.
        intervals: [u32; MAX_RATES],
        /// How many: 1 to [`MAX_RATES`].
        count: u8,
    },
    /// A range: from `min` (the fastest) to `max` in steps of `step`.
    Continuous {
        /// The shortest interval, at least 1.
        min: u32,
        /// The longest, at least `min`.
        max: u32,
        /// The step, at least 1 unless `min` is `max`.
        step: u32,
    },
}

impl Rates {
    /// A list's intervals; a range has none listed.
    #[must_use]
    pub fn listed(&self) -> &[u32] {
        match self {
            Self::Discrete { intervals, count } => {
                intervals.get(..usize::from(*count)).unwrap_or_default()
            }
            Self::Continuous { .. } => &[],
        }
    }

    /// The interval to ask for when `want` is wanted: a list's interval
    /// nearest `want` that is not faster than it (within 0.1%), or its
    /// slowest if all are faster; in a range, `want` on the range's steps,
    /// or the range's nearer end.
    #[must_use]
    pub fn pick(&self, want: u32) -> u32 {
        match *self {
            Self::Discrete { .. } => {
                let floor = want.saturating_sub(want / 1000);
                let listed = self.listed();
                listed
                    .iter()
                    .copied()
                    .filter(|interval| *interval >= floor)
                    .min_by_key(|interval| interval.abs_diff(want))
                    .or_else(|| listed.iter().copied().max())
                    .unwrap_or(want)
            }
            Self::Continuous { min, max, step } => {
                if want <= min || step == 0 {
                    return min;
                }
                if want >= max {
                    return max;
                }
                // The nearest step: min + round((want - min) / step) * step.
                let steps = want
                    .saturating_sub(min)
                    .saturating_add(step / 2)
                    .checked_div(step)
                    .unwrap_or(0);
                min.saturating_add(steps.saturating_mul(step)).min(max)
            }
        }
    }
}

impl fmt::Display for Rates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Discrete { .. } => {
                for (n, interval) in self.listed().iter().enumerate() {
                    if n > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", Rate(*interval))?;
                }
                f.write_str(" a second")
            }
            Self::Continuous { min, max, .. } => {
                write!(f, "{} to {} a second", Rate(max), Rate(min))
            }
        }
    }
}

/// One Motion JPEG frame size (Motion-JPEG Payload 1.5, table 3-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSize {
    /// bFrameIndex, from 1: what the probe asks for.
    pub index: u8,
    /// wWidth, at least 1.
    pub width: u16,
    /// wHeight, at least 1.
    pub height: u16,
    /// dwDefaultFrameInterval, as the camera gave it.
    pub default_interval: u32,
    /// The intervals it offers.
    pub rates: Rates,
}

impl FrameSize {
    const fn area(&self) -> u32 {
        (self.width as u32).saturating_mul(self.height as u32)
    }
}

/// An alternate setting of the VideoStreaming interface with its
/// isochronous video endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alternate {
    /// bAlternateSetting: never 0, which has no bandwidth.
    pub alternate: u8,
    /// The endpoint's packet size, wMaxPacketSize bits 10:0: 1 to 1024.
    pub max_packet: u16,
    /// Its transactions each interval, 1 to 3: one more than
    /// wMaxPacketSize bits 12:11.
    pub transactions: u8,
    /// Its bInterval, as the device gave it.
    pub interval: u8,
}

impl Alternate {
    /// The most bytes one interval carries: one payload transfer.
    #[must_use]
    pub const fn bandwidth(&self) -> u32 {
        (self.max_packet as u32).saturating_mul(self.transactions as u32)
    }
}

/// A camera: the first VideoStreaming interface of a configuration that has
/// an input header naming an IN video endpoint, with what it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Camera {
    /// bcdUVC from the VideoControl interface's header: 0x0150 for UVC
    /// 1.5; 0 if there was none.
    pub uvc: u16,
    /// The VideoStreaming interface's bInterfaceNumber.
    pub interface: u8,
    /// Its video endpoint's address, IN: bit 7 set, number 1 to 15.
    pub endpoint: u8,
    /// The first Motion JPEG format's bFormatIndex, if it has one.
    pub format: Option<u8>,
    frames: [Option<FrameSize>; MAX_FRAMES],
    /// Motion JPEG frame descriptors past [`MAX_FRAMES`], or unreadable:
    /// not kept.
    pub frames_left_out: u16,
    alternates: [Option<Alternate>; MAX_ALTERNATES],
    /// The video endpoint's packet size, if it is a bulk endpoint in
    /// alternate setting 0.
    pub bulk: Option<u16>,
}

impl Camera {
    const fn new(interface: u8, endpoint: u8, uvc: u16) -> Self {
        Self {
            uvc,
            interface,
            endpoint,
            format: None,
            frames: [None; MAX_FRAMES],
            frames_left_out: 0,
            alternates: [None; MAX_ALTERNATES],
            bulk: None,
        }
    }

    /// Its Motion JPEG frame sizes, in the order it listed them.
    pub fn frames(&self) -> impl Iterator<Item = &FrameSize> {
        self.frames.iter().flatten()
    }

    /// The alternate settings of its isochronous video endpoint.
    pub fn alternates(&self) -> impl Iterator<Item = &Alternate> {
        self.alternates.iter().flatten()
    }

    fn add_frame(&mut self, frame: FrameSize) {
        if self.frames().any(|kept| kept.index == frame.index) {
            return;
        }
        match self.frames.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(frame),
            None => self.frames_left_out = self.frames_left_out.saturating_add(1),
        }
    }

    fn add_alternate(&mut self, alternate: Alternate) {
        if self
            .alternates()
            .any(|kept| kept.alternate == alternate.alternate)
        {
            return;
        }
        if let Some(slot) = self.alternates.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(alternate);
        }
    }

    /// The size, rate and alternate setting to stream: [`PREFERRED`] if
    /// offered, else the largest size that fits within it, else the
    /// smallest; the rate nearest 30 a second not faster than it
    /// ([`Rates::pick`]); and the alternate setting with the most
    /// bandwidth, which carries whatever the camera negotiates.
    ///
    /// # Errors
    ///
    /// [`Refused::NoMotionJpeg`] for a camera with no Motion JPEG frame
    /// size; [`Refused::Bulk`] for one whose video endpoint is bulk;
    /// [`Refused::NoEndpoint`] for one with no alternate setting that has
    /// its isochronous video endpoint.
    pub fn choose(&self) -> Result<Choice, Refused> {
        let format = self.format.ok_or(Refused::NoMotionJpeg)?;
        let (want_width, want_height) = PREFERRED;
        let exact = self
            .frames()
            .find(|frame| (frame.width, frame.height) == PREFERRED);
        let within = self
            .frames()
            .filter(|frame| frame.width <= want_width && frame.height <= want_height)
            .fold(None::<&FrameSize>, |best, frame| match best {
                Some(best) if best.area() >= frame.area() => Some(best),
                _ => Some(frame),
            });
        let smallest = self
            .frames()
            .fold(None::<&FrameSize>, |best, frame| match best {
                Some(best) if best.area() <= frame.area() => Some(best),
                _ => Some(frame),
            });
        let frame = exact
            .or(within)
            .or(smallest)
            .copied()
            .ok_or(Refused::NoMotionJpeg)?;
        let alternate = self
            .alternates()
            .fold(None::<&Alternate>, |best, alternate| match best {
                Some(best) if best.bandwidth() >= alternate.bandwidth() => Some(best),
                _ => Some(alternate),
            })
            .copied()
            .ok_or(if self.bulk.is_some() {
                Refused::Bulk
            } else {
                Refused::NoEndpoint
            })?;
        Ok(Choice {
            interface: self.interface,
            endpoint: self.endpoint,
            format,
            frame: frame.index,
            width: frame.width,
            height: frame.height,
            interval: frame.rates.pick(INTERVAL_30),
            alternate,
            probe_len: probe_len(self.uvc),
        })
    }
}

/// The bytes of the Video Probe and Commit controls for a camera of release
/// `uvc`: 48 from UVC 1.5 (table 4-75), 34 from 1.1 - through bMaxVersion,
/// the fields 1.1 added (1.5's revision history) - and 26 before, through
/// dwMaxPayloadTransferSize.
#[must_use]
pub const fn probe_len(uvc: u16) -> u16 {
    if uvc >= 0x0150 {
        48
    } else if uvc >= 0x0110 {
        34
    } else {
        26
    }
}

/// What to stream: the camera's choice, set by the probe and commit
/// ([`super::probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    /// The VideoStreaming interface.
    pub interface: u8,
    /// Its video endpoint's address.
    pub endpoint: u8,
    /// The Motion JPEG format's bFormatIndex.
    pub format: u8,
    /// The frame size's bFrameIndex.
    pub frame: u8,
    /// The frame size.
    pub width: u16,
    /// The frame size.
    pub height: u16,
    /// The frame interval asked for, in 100 ns units.
    pub interval: u32,
    /// The alternate setting that streams it.
    pub alternate: Alternate,
    /// The probe and commit's length for the camera's release.
    pub probe_len: u16,
}

impl Choice {
    /// The video endpoint as the USB host PD's isochronous streams take
    /// any endpoint: `audio`'s [`Stream`], with no PCM format.
    #[must_use]
    pub const fn stream(&self) -> Stream {
        Stream {
            interface: self.interface,
            alternate: self.alternate.alternate,
            endpoint: self.endpoint,
            max_packet: self.alternate.max_packet,
            transactions: self.alternate.transactions,
            interval: self.alternate.interval,
            // Asynchronous (UVC 1.5, section 3.10.1.1).
            sync: 1,
            format: None,
            max_packets_only: false,
            // Not audio: no rate to set, no feedback.
            rate: 0,
            rate_control: false,
            feedback: None,
            refusal: None,
        }
    }
}

/// Why a camera's video is not streamed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No Motion JPEG frame size this walk could read.
    NoMotionJpeg,
    /// Its video endpoint is bulk: bulk transfers are not built yet.
    Bulk,
    /// No alternate setting has its isochronous video endpoint.
    NoEndpoint,
    /// A request failed: which.
    Request(&'static str),
    /// The probe's answer: what was wrong with it.
    Answer(super::probe::AnswerError),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMotionJpeg => f.write_str("no Motion JPEG frame size"),
            Self::Bulk => f.write_str("a bulk video endpoint, which this driver does not stream"),
            Self::NoEndpoint => {
                f.write_str("no alternate setting with its isochronous video endpoint")
            }
            Self::Request(which) => write!(f, "{which} failed"),
            Self::Answer(error) => write!(f, "the probe's answer: {error}"),
        }
    }
}

/// A Motion JPEG frame descriptor's frame size, if its size and intervals
/// are all at least 1 and it holds the intervals it claims: 38 bytes for a
/// range, 26 and 4 for each listed interval (Motion-JPEG Payload 1.5, table
/// 3-2).
fn frame_size(descriptor: &[u8]) -> Option<FrameSize> {
    let byte = |at: usize| descriptor.get(at).copied();
    let (index, width, height) = (byte(3)?, le16(descriptor, 5)?, le16(descriptor, 7)?);
    let default_interval = le32(descriptor, 21)?;
    let kind = byte(25)?;
    if index == 0 || width == 0 || height == 0 {
        return None;
    }
    let rates = if kind == 0 {
        let (min, max, step) = (
            le32(descriptor, 26)?,
            le32(descriptor, 30)?,
            le32(descriptor, 34)?,
        );
        if min == 0 || min > max || (step == 0 && min != max) {
            return None;
        }
        Rates::Continuous { min, max, step }
    } else {
        let needed = usize::from(kind).saturating_mul(4).saturating_add(26);
        if descriptor.len() < needed {
            return None;
        }
        let mut intervals = [0u32; MAX_RATES];
        let mut count = 0u8;
        for n in 0..usize::from(kind) {
            let interval = le32(descriptor, n.saturating_mul(4).saturating_add(26))?;
            if interval == 0 {
                return None;
            }
            if let Some(slot) = intervals.get_mut(usize::from(count)) {
                *slot = interval;
                count = count.saturating_add(1);
            }
        }
        Rates::Discrete { intervals, count }
    };
    Some(FrameSize {
        index,
        width,
        height,
        default_interval,
        rates,
    })
}

/// What the walk has seen of the interface it is in.
#[derive(Clone, Copy, Default)]
struct Current {
    interface: u8,
    alternate: u8,
    control: bool,
    streaming: bool,
    /// Whether the frame descriptors that follow are the Motion JPEG
    /// format's.
    mjpeg: bool,
}

/// Walks a copy of a whole configuration and returns its camera - its first
/// VideoStreaming interface whose input header names an IN endpoint - with
/// its first Motion JPEG format's frame sizes and rates and the alternate
/// settings of its video endpoint; `None` if it has none. A camera with no
/// Motion JPEG format is returned, so it can be refused by name
/// ([`Camera::choose`]).
///
/// The walk ends as the other walks do: at wTotalLength or the end of the
/// bytes given.
///
/// # Errors
///
/// [`DescriptorError`] if the header is not a configuration's, or any
/// descriptor's length is under 2, runs past wTotalLength, or is shorter
/// than an interface or endpoint descriptor needs.
pub fn camera(config: &[u8]) -> Result<Option<Camera>, DescriptorError> {
    let mut found: Option<Camera> = None;
    let mut uvc = 0u16;
    let mut current = Current::default();
    walk(config, |at, descriptor| {
        let byte = |offset: usize| descriptor.get(offset).copied().unwrap_or(0);
        let length = byte(0);
        match byte(1) {
            descriptor_type::INTERFACE => {
                if length < 9 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                current = Current {
                    interface: byte(2),
                    alternate: byte(3),
                    control: byte(5) == CLASS_VIDEO && byte(6) == SUBCLASS_CONTROL,
                    streaming: byte(5) == CLASS_VIDEO && byte(6) == SUBCLASS_STREAMING,
                    mjpeg: false,
                };
            }
            CS_INTERFACE if current.control => {
                if byte(2) == VC_HEADER && length >= 12 && uvc == 0 {
                    uvc = le16(descriptor, 3).unwrap_or(0);
                }
            }
            CS_INTERFACE if current.streaming && current.alternate == 0 => {
                let subtype = byte(2);
                match found.as_mut() {
                    None if subtype == VS_INPUT_HEADER && length >= 13 => {
                        let endpoint = byte(6);
                        if endpoint & 0x80 != 0 && endpoint & 0x0F != 0 {
                            found = Some(Camera::new(current.interface, endpoint, uvc));
                        }
                    }
                    Some(camera) if camera.interface == current.interface => {
                        if FORMATS.contains(&subtype) {
                            current.mjpeg = subtype == VS_FORMAT_MJPEG
                                && length >= 11
                                && camera.format.is_none();
                            if current.mjpeg {
                                camera.format = Some(byte(3));
                            }
                        } else if subtype == VS_FRAME_MJPEG && current.mjpeg {
                            match frame_size(descriptor) {
                                Some(frame) => camera.add_frame(frame),
                                None => {
                                    camera.frames_left_out =
                                        camera.frames_left_out.saturating_add(1);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            descriptor_type::ENDPOINT => {
                if length < 7 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                let Some(camera) = found.as_mut() else {
                    return Ok(());
                };
                if !current.streaming
                    || current.interface != camera.interface
                    || byte(2) != camera.endpoint
                {
                    return Ok(());
                }
                let packet = le16(descriptor, 4).unwrap_or(0);
                let max_packet = packet & 0x7FF;
                let extra = (packet >> 11) & 0x3;
                match byte(3) & 0x3 {
                    // Isochronous, outside alternate setting 0; a size USB
                    // allows and no reserved transaction count.
                    0x1 if current.alternate != 0
                        && (1..=1024).contains(&max_packet)
                        && extra < 3 =>
                    {
                        camera.add_alternate(Alternate {
                            alternate: current.alternate,
                            max_packet,
                            transactions: u8::try_from(extra).unwrap_or(0).saturating_add(1),
                            interval: byte(6),
                        });
                    }
                    // Bulk, in alternate setting 0 alone (UVC 1.5, section
                    // 2.4.3).
                    0x2 if current.alternate == 0 && max_packet != 0 => {
                        camera.bulk = Some(max_packet);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    })?;
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
    use std::vec::Vec;

    /// A configuration's 9-byte header, its wTotalLength filled in by
    /// [`finish`].
    fn header(interfaces: u8) -> Vec<u8> {
        std::vec![9, 2, 0, 0, interfaces, 1, 0, 0x80, 250]
    }

    /// Sets wTotalLength to the bytes there are.
    pub(crate) fn finish(mut config: Vec<u8>) -> Vec<u8> {
        let total = u16::try_from(config.len()).unwrap();
        config[2..4].copy_from_slice(&total.to_le_bytes());
        config
    }

    /// A Motion JPEG frame descriptor with discrete intervals.
    fn frame(index: u8, width: u16, height: u16, intervals: &[u32]) -> Vec<u8> {
        let mut out = std::vec![26 + 4 * intervals.len() as u8, 0x24, 0x07, index, 0];
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&1_000_000u32.to_le_bytes()); // dwMinBitRate
        out.extend_from_slice(&50_000_000u32.to_le_bytes()); // dwMaxBitRate
        out.extend_from_slice(&(u32::from(width) * u32::from(height) * 2).to_le_bytes());
        out.extend_from_slice(&intervals[0].to_le_bytes()); // dwDefaultFrameInterval
        out.push(intervals.len() as u8);
        for interval in intervals {
            out.extend_from_slice(&interval.to_le_bytes());
        }
        out
    }

    /// An isochronous IN endpoint 0x81 descriptor.
    fn endpoint(packet: u16) -> Vec<u8> {
        let [low, high] = packet.to_le_bytes();
        std::vec![7, 5, 0x81, 0x05, low, high, 1]
    }

    /// An invented high-speed webcam: a VideoControl interface (0, UVC
    /// 1.0), a VideoStreaming interface (1) whose input header names IN
    /// endpoint 0x81, an uncompressed format whose one frame size must not
    /// be taken, a Motion JPEG format (index 2) with 640x480, 1280x720 and
    /// 1920x1080 at 30, 15 and 5 a second, and alternate settings 1 to 3
    /// at 128 bytes, 3 x 1024 and 2 x 1024 a microframe.
    pub(crate) fn webcam() -> Vec<u8> {
        let mut c = header(2);
        c.extend_from_slice(&[9, 4, 0, 0, 1, 0x0E, 0x01, 0, 0]);
        c.extend_from_slice(&[
            13, 0x24, 0x01, 0x00, 0x01, 13, 0, 0x80, 0x8D, 0x5B, 0x00, 1, 1,
        ]);
        c.extend_from_slice(&[9, 4, 1, 0, 0, 0x0E, 0x02, 0, 0]);
        c.extend_from_slice(&[14, 0x24, 0x01, 2, 0, 0, 0x81, 0, 3, 0, 0, 0, 1, 0]);
        // Uncompressed format 1 and its frame: 320x240, never Motion JPEG.
        c.extend_from_slice(&[27, 0x24, 0x04, 1, 1]);
        c.extend_from_slice(&[0u8; 22]);
        let mut raw = frame(1, 320, 240, &[INTERVAL_30]);
        raw[2] = 0x05;
        c.extend_from_slice(&raw);
        // Motion JPEG format 2, three frames, then a colour matching
        // descriptor.
        c.extend_from_slice(&[11, 0x24, 0x06, 2, 3, 1, 1, 0, 0, 0, 0]);
        for (index, (width, height)) in [(640, 480), (1280, 720), (1920, 1080)].iter().enumerate() {
            c.extend_from_slice(&frame(
                index as u8 + 1,
                *width,
                *height,
                &[INTERVAL_30, 666_666, 2_000_000],
            ));
        }
        c.extend_from_slice(&[6, 0x24, 0x0D, 1, 1, 4]);
        for (alternate, packet) in [(1u8, 128u16), (2, 0x1400), (3, 0x0C00)] {
            c.extend_from_slice(&[9, 4, 1, alternate, 1, 0x0E, 0x02, 0, 0]);
            c.extend_from_slice(&endpoint(packet));
        }
        finish(c)
    }

    #[test]
    fn a_webcams_motion_jpeg_sizes_rates_and_settings() {
        let camera = camera(&webcam()).unwrap().unwrap();
        assert_eq!(
            (camera.uvc, camera.interface, camera.endpoint, camera.format),
            (0x0100, 1, 0x81, Some(2))
        );
        let sizes: Vec<(u8, u16, u16)> = camera
            .frames()
            .map(|frame| (frame.index, frame.width, frame.height))
            .collect();
        assert_eq!(sizes, [(1, 640, 480), (2, 1280, 720), (3, 1920, 1080)]);
        let first = camera.frames().next().unwrap();
        assert_eq!(first.rates.listed(), [INTERVAL_30, 666_666, 2_000_000]);
        assert_eq!(std::format!("{}", first.rates), "30, 15, 5 a second");
        assert_eq!(
            camera
                .alternates()
                .map(|alternate| (alternate.alternate, alternate.bandwidth()))
                .collect::<Vec<_>>(),
            [(1, 128), (2, 3072), (3, 2048)]
        );
        assert_eq!(camera.bulk, None);
        let choice = camera.choose().unwrap();
        assert_eq!(
            (
                choice.format,
                choice.frame,
                choice.width,
                choice.height,
                choice.interval
            ),
            (2, 2, 1280, 720, INTERVAL_30)
        );
        assert_eq!(choice.alternate.alternate, 2, "the most bandwidth");
        assert_eq!(choice.probe_len, 26, "UVC 1.0");
        let stream = choice.stream();
        assert_eq!(
            (
                stream.endpoint,
                stream.alternate,
                stream.max_packet,
                stream.transactions
            ),
            (0x81, 2, 1024, 3)
        );
        assert!(stream.inbound() && stream.format.is_none());
    }

    #[test]
    fn a_camera_with_no_motion_jpeg_is_refused() {
        let mut config = webcam();
        // Make the Motion JPEG format a frame-based one: its frames are no
        // longer read.
        let at = config
            .windows(4)
            .position(|w| w == [11, 0x24, 0x06, 2])
            .unwrap();
        config[at + 2] = 0x10;
        let camera = camera(&config).unwrap().unwrap();
        assert_eq!(camera.format, None);
        assert_eq!(camera.frames().count(), 0);
        assert_eq!(camera.choose(), Err(Refused::NoMotionJpeg));
        assert_eq!(
            std::format!("{}", Refused::NoMotionJpeg),
            "no Motion JPEG frame size"
        );
    }

    #[test]
    fn a_device_with_no_video_streaming_interface_is_no_camera() {
        use crate::audio::tests::SPEAKER_CONFIG;
        assert_eq!(camera(&SPEAKER_CONFIG).unwrap(), None);
    }

    #[test]
    fn sizes_and_rates_chosen_where_the_preferred_is_not_offered() {
        let at = |frames: &[(u16, u16)]| {
            let mut camera = Camera::new(1, 0x81, 0x0150);
            camera.format = Some(1);
            for (index, (width, height)) in frames.iter().enumerate() {
                camera.add_frame(FrameSize {
                    index: index as u8 + 1,
                    width: *width,
                    height: *height,
                    default_interval: INTERVAL_30,
                    rates: Rates::Discrete {
                        intervals: [INTERVAL_30; MAX_RATES],
                        count: 1,
                    },
                });
            }
            camera.add_alternate(Alternate {
                alternate: 1,
                max_packet: 1024,
                transactions: 1,
                interval: 1,
            });
            let choice = camera.choose().unwrap();
            assert_eq!(choice.probe_len, 48, "UVC 1.5");
            (choice.width, choice.height)
        };
        assert_eq!(at(&[(640, 480), (1280, 720), (1920, 1080)]), (1280, 720));
        assert_eq!(at(&[(1920, 1080), (960, 540), (640, 480)]), (960, 540));
        assert_eq!(
            at(&[(1920, 1080), (2560, 1440)]),
            (1920, 1080),
            "the smallest"
        );
        assert_eq!(
            at(&[(800, 600), (1024, 576)]),
            (1024, 576),
            "both fit 1280 x 720"
        );

        let discrete = |list: &[u32]| {
            let mut intervals = [0; MAX_RATES];
            intervals[..list.len()].copy_from_slice(list);
            Rates::Discrete {
                intervals,
                count: list.len() as u8,
            }
            .pick(INTERVAL_30)
        };
        assert_eq!(discrete(&[166_666, INTERVAL_30, 666_666]), INTERVAL_30);
        assert_eq!(discrete(&[333_334]), 333_334);
        assert_eq!(
            discrete(&[166_666, 400_000, 1_000_000]),
            400_000,
            "25, not 60"
        );
        assert_eq!(
            discrete(&[166_666, 200_000]),
            200_000,
            "all faster: the slowest"
        );
        let range = |min, max, step| Rates::Continuous { min, max, step }.pick(INTERVAL_30);
        assert_eq!(range(166_666, 2_000_000, 1), INTERVAL_30);
        assert_eq!(
            range(100_000, 2_000_000, 100_000),
            300_000,
            "the nearest step"
        );
        assert_eq!(range(500_000, 2_000_000, 1), 500_000);
        assert_eq!(range(100_000, 200_000, 1), 200_000);
        assert_eq!(range(INTERVAL_30, INTERVAL_30, 0), INTERVAL_30);
        assert_eq!(
            std::format!(
                "{}",
                Rates::Continuous {
                    min: 166_666,
                    max: 2_000_000,
                    step: 1
                }
            ),
            "5 to 60 a second"
        );
        assert_eq!(probe_len(0x0110), 34);
        assert_eq!(probe_len(0), 26);
    }

    #[test]
    fn bulk_and_missing_endpoints_are_refused_by_name() {
        let mut camera = Camera::new(1, 0x81, 0x0100);
        camera.format = Some(1);
        camera.add_frame(FrameSize {
            index: 1,
            width: 640,
            height: 480,
            default_interval: INTERVAL_30,
            rates: Rates::Continuous {
                min: INTERVAL_30,
                max: INTERVAL_30,
                step: 0,
            },
        });
        assert_eq!(camera.choose(), Err(Refused::NoEndpoint));
        camera.bulk = Some(512);
        assert_eq!(camera.choose(), Err(Refused::Bulk));
    }

    #[test]
    fn a_bulk_camera_and_bad_frame_descriptors() {
        let mut c = header(1);
        c.extend_from_slice(&[9, 4, 0, 0, 1, 0x0E, 0x02, 0, 0]);
        c.extend_from_slice(&[14, 0x24, 0x01, 1, 0, 0, 0x82, 0, 3, 0, 0, 0, 1, 0]);
        c.extend_from_slice(&[11, 0x24, 0x06, 1, 4, 1, 1, 0, 0, 0, 0]);
        c.extend_from_slice(&frame(1, 640, 480, &[INTERVAL_30]));
        // A frame of width 0, one claiming two intervals with room for one,
        // one with an interval of 0, and a range from 2 down to 1: all left
        // out.
        c.extend_from_slice(&frame(2, 0, 480, &[INTERVAL_30]));
        let mut short = frame(3, 320, 240, &[INTERVAL_30]);
        short[25] = 2;
        c.extend_from_slice(&short);
        c.extend_from_slice(&frame(4, 320, 240, &[0]));
        let mut range = std::vec![38, 0x24, 0x07, 5, 0, 0x40, 1, 0xF0, 0];
        range.extend_from_slice(&[0u8; 16]);
        range.push(0);
        for value in [2u32, 1, 1] {
            range.extend_from_slice(&value.to_le_bytes());
        }
        c.extend_from_slice(&range);
        c.extend_from_slice(&[7, 5, 0x82, 0x02, 0x00, 0x02, 0]);
        let camera = camera(&finish(c)).unwrap().unwrap();
        assert_eq!(camera.frames().count(), 1);
        assert_eq!(camera.frames_left_out, 4);
        assert_eq!(camera.bulk, Some(512));
        assert_eq!(camera.choose(), Err(Refused::Bulk));
    }

    #[test]
    fn more_than_it_keeps_and_short_descriptors() {
        let mut c = header(1);
        c.extend_from_slice(&[9, 4, 1, 0, 0, 0x0E, 0x02, 0, 0]);
        c.extend_from_slice(&[14, 0x24, 0x01, 1, 0, 0, 0x81, 0, 3, 0, 0, 0, 1, 0]);
        c.extend_from_slice(&[11, 0x24, 0x06, 1, 20, 1, 1, 0, 0, 0, 0]);
        for index in 1..=20u8 {
            c.extend_from_slice(&frame(
                index,
                160 + u16::from(index),
                120,
                &[INTERVAL_30; 10],
            ));
        }
        for alternate in 1..=10u8 {
            c.extend_from_slice(&[9, 4, 1, alternate, 1, 0x0E, 0x02, 0, 0]);
            c.extend_from_slice(&endpoint(u16::from(alternate) * 64));
        }
        // An endpoint with a reserved transaction count, and another
        // endpoint than the video one: neither kept.
        c.extend_from_slice(&[9, 4, 1, 11, 1, 0x0E, 0x02, 0, 0]);
        c.extend_from_slice(&endpoint(0x1C00 | 512));
        c.extend_from_slice(&[7, 5, 0x83, 0x05, 0, 4, 1]);
        let config = finish(c);
        let camera = camera(&config).unwrap().unwrap();
        assert_eq!(camera.frames().count(), MAX_FRAMES);
        assert_eq!(camera.frames_left_out, 4);
        assert_eq!(
            camera.frames().next().unwrap().rates.listed().len(),
            MAX_RATES
        );
        assert_eq!(camera.alternates().count(), MAX_ALTERNATES);
        // A short endpoint descriptor refuses the configuration.
        let mut short = config.clone();
        let at = short.len() - 7;
        short[at] = 6;
        short.truncate(short.len() - 1);
        let short = finish(short);
        assert!(camera_result_is_err(&short));
    }

    fn camera_result_is_err(config: &[u8]) -> bool {
        camera(config).is_err()
    }

    proptest! {
        #[test]
        fn walking_never_panics_and_keeps_its_bounds(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            if let Ok(Some(found)) = camera(&bytes) {
                prop_assert!(found.frames().count() <= MAX_FRAMES);
                prop_assert!(found.alternates().count() <= MAX_ALTERNATES);
                prop_assert!(found.endpoint & 0x80 != 0);
                for frame in found.frames() {
                    prop_assert!(frame.width != 0 && frame.height != 0);
                    prop_assert!(frame.rates.listed().iter().all(|interval| *interval != 0));
                }
                for alternate in found.alternates() {
                    prop_assert!(alternate.alternate != 0);
                    prop_assert!((1..=3072).contains(&alternate.bandwidth()));
                }
                if let Ok(choice) = found.choose() {
                    prop_assert!(choice.interval != 0);
                    prop_assert!(found.frames().any(|frame| frame.index == choice.frame));
                }
            }
        }

        #[test]
        fn a_webcam_with_a_byte_changed_never_panics(at in 0usize..400, value: u8, cut in 0usize..420) {
            let mut config = webcam();
            let at = at % config.len();
            config[at] = value;
            let cut = cut.min(config.len());
            if let Ok(Some(found)) = camera(&config[..cut]) {
                let _ = found.choose();
            }
        }

        #[test]
        fn a_pick_is_always_offered(min in 1u32..5_000_000, span in 0u32..5_000_000, step in 0u32..1_000_000, want: u32) {
            let max = min.saturating_add(span);
            let step = if min == max { step } else { step.max(1) };
            let picked = Rates::Continuous { min, max, step }.pick(want);
            prop_assert!((min..=max).contains(&picked));
            if step != 0 && picked != max {
                prop_assert_eq!((picked - min) % step, 0);
            }
        }
    }
}
