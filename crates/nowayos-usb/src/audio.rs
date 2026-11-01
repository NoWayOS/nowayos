// SPDX-License-Identifier: MIT OR Apache-2.0
//! USB audio streaming interfaces: which alternate setting
//! of an audio streaming interface to stream, its isochronous data endpoint's
//! packets and interval, the PCM format and rate it streams in, the
//! feedback endpoint an asynchronous speaker paces its host with, and the
//! samples in its packets.
//!
//! From usb.org's *Device Class Definition for Audio Devices*, release 1.0
//! (the interface class and subclass codes of appendix A, the class-specific
//! AS interface descriptor of section 4.5.2, the AS isochronous audio data
//! endpoint descriptors of section 4.6.1 - bRefresh and bSynchAddress, and
//! the Sampling Frequency control of bmAttributes D0 - the isochronous synch
//! endpoint of sections 3.7.2.2 and 4.6.2, and the Sampling Frequency
//! Control request of section 5.2.3.2.3.1), its *Device Class Definition for
//! Audio Data Formats*, release 1.0 (the Type I format type descriptor of
//! section 2.2.5, with its continuous range or discrete rates; the PCM
//! format tag of appendix A.1.1; PCM samples left-justified in their
//! subframes, section 2.2.6.1), and the *Universal Serial Bus
//! Specification*, revision 2.0 (the interface and endpoint descriptors of
//! sections 9.6.5 and 9.6.6: an endpoint's transfer, synchronisation and
//! usage types, its packet size and, at high speed, its transactions each
//! microframe).
//!
//! **A descriptor is what a device says about itself, and a device is not
//! trusted.** [`streams_at`] walks a copy already out of DMA memory with
//! every length checked (`descriptor::walk`), never reads past a
//! descriptor's own bytes, keeps at most [`MAX_STREAMS`] and at most
//! [`MAX_RATES`] rates a format, and takes no format it cannot check: a
//! format type descriptor with a channel count, subframe or bit resolution
//! out of range, or too short for the rates it claims, leaves the stream
//! with no format rather than refusing the configuration.
//!
//! **What NoWayOS streams**: of each audio streaming interface's
//! alternate settings with an isochronous data endpoint, the best - one it
//! supports, then one of 16-bit samples, then one that takes the rate asked
//! for - and, of the rates that setting takes and its packets hold, the one
//! nearest the rate asked for. It supports Type I PCM of one or two
//! channels, subframes of 1 to 4 bytes, at 8 to 192 kHz; any other stream
//! is found, so its driver can say why, and refused ([`Refusal`]).

use core::fmt;

use crate::context::{interval_micros, isoch_interval};
use crate::descriptor::{DescriptorError, walk};
use crate::le16;
use crate::port::Speed;
use crate::request::descriptor_type;

/// The audio interface class (Audio 1.0, table A-1).
pub const CLASS_AUDIO: u8 = 1;

/// The audio streaming interface subclass (Audio 1.0, table A-2).
pub const SUBCLASS_STREAMING: u8 = 2;

/// The class-specific interface descriptor type (Audio 1.0, table A-4).
pub const CS_INTERFACE: u8 = 0x24;

/// The class-specific endpoint descriptor type (Audio 1.0, table A-4).
pub const CS_ENDPOINT: u8 = 0x25;

/// AS_GENERAL, the class-specific AS interface descriptor's subtype
/// (Audio 1.0, table A-6); EP_GENERAL, the class-specific endpoint
/// descriptor's (table A-8), has the same value.
const GENERAL: u8 = 0x01;

/// FORMAT_TYPE, the format type descriptor's subtype (Audio 1.0, table A-6).
const FORMAT_TYPE: u8 = 0x02;

/// FORMAT_TYPE_I (Audio Data Formats 1.0, table A-4).
const FORMAT_TYPE_I: u8 = 0x01;

/// The PCM format tag (Audio Data Formats 1.0, table A-1).
pub const PCM: u16 = 0x0001;

/// The class-specific endpoint descriptor's bmAttributes D0: the endpoint
/// has a Sampling Frequency control (Audio 1.0, section 4.6.1.2).
const SAMPLING_FREQUENCY: u8 = 0x01;

/// The most audio streams taken from one configuration: a headset's
/// speaker and microphone.
pub const MAX_STREAMS: usize = 2;

/// The most discrete rates kept of one format: those listed first.
pub const MAX_RATES: usize = 8;

/// The rate a stream is set to when its driver asks for none: 48 kHz.
pub const DEFAULT_RATE: u32 = 48_000;

/// The most channels NoWayOS streams: a call's sound is mono or stereo.
pub const MAX_CHANNELS: u8 = 2;

/// The lowest and highest rates NoWayOS streams, in hertz.
pub const MIN_RATE: u32 = 8_000;
/// See [`MIN_RATE`].
pub const MAX_RATE: u32 = 192_000;

/// The microseconds in a second.
const MICROS: u64 = 1_000_000;

/// The sampling frequencies a format takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rates {
    /// Any rate from `low` to `high` hertz.
    Range {
        /// The lowest, at least 1.
        low: u32,
        /// The highest, at least `low`.
        high: u32,
    },
    /// The rates listed: `count` of them, 1 to [`MAX_RATES`], the first
    /// the device gave, each at least 1.
    Discrete {
        /// How many of `list` are rates.
        count: u8,
        /// The rates, then zeros.
        list: [u32; MAX_RATES],
    },
}

impl Rates {
    /// The rates listed, or a range's two ends.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let (list, count): (&[u32], usize) = match self {
            Self::Range { low, .. } => (core::slice::from_ref(low), 1),
            Self::Discrete { count, list } => (list.as_slice(), usize::from(*count)),
        };
        let high = match self {
            Self::Range { low, high } if high != low => Some(*high),
            _ => None,
        };
        list.iter().take(count).copied().chain(high)
    }

    /// Whether `rate` is one it takes.
    #[must_use]
    pub fn takes(&self, rate: u32) -> bool {
        match self {
            Self::Range { low, high } => (*low..=*high).contains(&rate),
            Self::Discrete { .. } => self.iter().any(|listed| listed == rate),
        }
    }

    /// Whether it takes more than one rate: a request then sets one.
    #[must_use]
    pub fn several(&self) -> bool {
        match self {
            Self::Range { low, high } => low != high,
            Self::Discrete { count, .. } => *count > 1,
        }
    }

    /// The rate it takes nearest `wanted` - the higher of two as near - of
    /// those from [`MIN_RATE`] to `most`; `None` if it takes none of them.
    #[must_use]
    pub fn nearest(&self, wanted: u32, most: u32) -> Option<u32> {
        let most = most.min(MAX_RATE);
        match self {
            Self::Range { low, high } => {
                let low = (*low).max(MIN_RATE);
                let high = (*high).min(most);
                (low <= high).then(|| wanted.clamp(low, high))
            }
            Self::Discrete { .. } => self
                .iter()
                .filter(|rate| (MIN_RATE..=most).contains(rate))
                .min_by_key(|rate| (rate.abs_diff(wanted), u32::MAX.saturating_sub(*rate))),
        }
    }

    /// Its one rate, when it takes only one.
    #[must_use]
    pub fn single(&self) -> Option<u32> {
        (!self.several()).then(|| self.iter().next()).flatten()
    }
}

/// A PCM format, as an interface's Type I format type descriptor gives it
/// (Audio Data Formats 1.0, section 2.2.5), checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pcm {
    /// bNrChannels: 1 to 8.
    pub channels: u8,
    /// bSubframeSize: the bytes of one channel's sample, 1 to 4.
    pub subframe: u8,
    /// bBitResolution: the bits of a subframe in use, 1 to 8 times
    /// `subframe`.
    pub bits: u8,
    /// The rates it takes.
    pub rates: Rates,
}

impl Pcm {
    /// The rate the stream runs at when the endpoint takes only one; with a
    /// choice, a sampling frequency request sets it, so `None`.
    #[must_use]
    pub fn fixed_rate(&self) -> Option<u32> {
        self.rates.single()
    }

    /// The bytes of one frame: a subframe for each channel.
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        (self.channels as usize).saturating_mul(self.subframe as usize)
    }

    /// Whether its samples are 16 bits in 2 bytes: what a client's records
    /// carry, so nothing is cut or padded.
    #[must_use]
    pub const fn sixteen_bit(&self) -> bool {
        self.subframe == 2 && self.bits == 16
    }

    /// Takes one frame from `frame` - a subframe for each channel - into
    /// `out` as 16-bit samples, and returns how many: the channels, or 0
    /// if `frame` or `out` is too short.
    pub fn read_frame(&self, frame: &[u8], out: &mut [i16]) -> usize {
        let subframe = usize::from(self.subframe);
        let channels = usize::from(self.channels);
        if subframe == 0 || frame.len() < self.frame_bytes() || out.len() < channels {
            return 0;
        }
        for (sample, bytes) in out
            .iter_mut()
            .zip(frame.chunks_exact(subframe))
            .take(channels)
        {
            *sample = sample16(bytes);
        }
        channels
    }

    /// Writes one frame of 16-bit `samples` - one a channel - into `out`
    /// in this format, and returns its bytes: 0, writing nothing, if
    /// `samples` is not a channel's each or `out` is too short.
    pub fn write_frame(&self, samples: &[i16], out: &mut [u8]) -> usize {
        let subframe = usize::from(self.subframe);
        let bytes = self.frame_bytes();
        if subframe == 0 || samples.len() != usize::from(self.channels) || out.len() < bytes {
            return 0;
        }
        for (sample, subframe) in samples.iter().zip(out.chunks_exact_mut(subframe)) {
            put_sample16(subframe, *sample);
        }
        bytes
    }
}

/// The 16-bit sample in a subframe of PCM - little-endian, left-justified
/// (Audio Data Formats 1.0, section 2.2.6.1): its two most significant
/// bytes; a one-byte subframe's byte as the high byte.
#[must_use]
pub fn sample16(subframe: &[u8]) -> i16 {
    match subframe {
        [] => 0,
        [only] => i16::from_le_bytes([0, *only]),
        [.., low, high] => i16::from_le_bytes([*low, *high]),
    }
}

/// Fills a subframe with `sample`, left-justified: its two bytes at the
/// top, zeros below; a one-byte subframe takes its high byte.
pub fn put_sample16(subframe: &mut [u8], sample: i16) {
    let [low, high] = sample.to_le_bytes();
    match subframe {
        [] => {}
        [only] => *only = high,
        [below @ .., top_low, top_high] => {
            below.fill(0);
            *top_low = low;
            *top_high = high;
        }
    }
}

/// An asynchronous speaker's feedback endpoint (Audio 1.0, sections 3.7.2.2
/// and 4.6.2): the isochronous IN endpoint its data endpoint's
/// bSynchAddress names, from which the host reads how many frames to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feedback {
    /// Its address: IN, number 1 to 15.
    pub endpoint: u8,
    /// Its packet size: 3 to 1024 bytes.
    pub max_packet: u16,
    /// Its bInterval, as the device gave it.
    pub interval: u8,
    /// Its bRefresh: a new value every 2^bRefresh frames, 1 to 9 (0 if the
    /// device gave none).
    pub refresh: u8,
}

impl Feedback {
    /// The endpoint as a stream of its own, for the host controller to set
    /// up: IN, one transaction an interval, polled no more often than its
    /// value changes - every 2^bRefresh frames at full speed.
    #[must_use]
    pub fn as_stream(&self, interface: u8, alternate: u8, speed: Speed) -> Stream {
        let interval = match speed {
            Speed::Low | Speed::Full => self.interval.max(self.refresh.saturating_add(1)),
            Speed::High | Speed::Super | Speed::SuperPlus => self.interval,
        };
        Stream {
            interface,
            alternate,
            endpoint: self.endpoint,
            max_packet: self.max_packet,
            transactions: 1,
            interval,
            sync: 0,
            format: None,
            max_packets_only: false,
            rate: 0,
            rate_control: false,
            feedback: None,
            refusal: None,
        }
    }
}

/// The frames an asynchronous speaker asks for each interval of
/// `interval_us` microseconds, in 65536ths of a frame, from one packet of
/// its feedback endpoint: 3 bytes of frames per 1 ms frame in 10.14 fixed
/// point (Audio 1.0, section 3.7.2.2), or 4 bytes of frames per frame -
/// per 125 us microframe at high speed - in 16.16. `None` for any other
/// length, or a value more than an eighth from `nominal`, the frames the
/// stream's rate gives each interval, in the same units: a device is not
/// trusted to set the pace.
#[must_use]
pub fn feedback_q16(packet: &[u8], speed: Speed, interval_us: u32, nominal: u64) -> Option<u64> {
    let (per_frame, frame_us) = match packet {
        [a, b, c] => (u64::from(u32::from_le_bytes([*a, *b, *c, 0])) << 2, 1000u64),
        [a, b, c, d] => (
            u64::from(u32::from_le_bytes([*a, *b, *c, *d])),
            if matches!(speed, Speed::Low | Speed::Full) {
                1000
            } else {
                125
            },
        ),
        _ => return None,
    };
    let value = per_frame
        .saturating_mul(u64::from(interval_us))
        .checked_div(frame_us)?;
    (value.abs_diff(nominal) <= nominal >> 3).then_some(value)
}

/// The frames `rate` gives each interval of `interval_us`, in 65536ths.
#[must_use]
pub const fn nominal_q16(rate: u32, interval_us: u32) -> u64 {
    match (rate as u64)
        .saturating_mul(interval_us as u64)
        .saturating_mul(65_536)
        .checked_div(MICROS)
    {
        Some(value) => value,
        None => 0,
    }
}

/// Why NoWayOS does not stream an audio stream it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No Type I PCM format this walk could check.
    NoFormat,
    /// More channels than [`MAX_CHANNELS`].
    Channels(u8),
    /// No rate from [`MIN_RATE`] to [`MAX_RATE`] that its packets hold.
    NoRate,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFormat => f.write_str("no PCM format this driver reads"),
            Self::Channels(channels) => write!(f, "{channels} channels, more than two"),
            Self::NoRate => f.write_str("no rate from 8 to 192 kHz that its packets hold"),
        }
    }
}

/// An audio streaming interface's alternate setting with an isochronous
/// data endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stream {
    /// bInterfaceNumber.
    pub interface: u8,
    /// bAlternateSetting: never 0, the setting with no bandwidth.
    pub alternate: u8,
    /// The endpoint's address: IN (bit 7 set) for a microphone, OUT for a
    /// speaker; number 1 to 15.
    pub endpoint: u8,
    /// Its packet size, wMaxPacketSize bits 10:0: 1 to 1024.
    pub max_packet: u16,
    /// Its transactions each interval, 1 to 3: at high speed one more than
    /// wMaxPacketSize bits 12:11; 1 otherwise.
    pub transactions: u8,
    /// Its bInterval, as the device gave it.
    pub interval: u8,
    /// Its synchronisation type, bmAttributes bits 3:2: 1 asynchronous, 2
    /// adaptive, 3 synchronous.
    pub sync: u8,
    /// The PCM format it streams, if its interface gave one this walk
    /// could check.
    pub format: Option<Pcm>,
    /// The class-specific endpoint descriptor's MaxPacketsOnly: the
    /// endpoint takes only packets of `max_packet` bytes.
    pub max_packets_only: bool,
    /// The rate it is to run at: of those its format takes and
    /// its packets hold, the nearest the rate asked for; 0 when refused.
    pub rate: u32,
    /// Whether its endpoint has a Sampling Frequency control.
    pub rate_control: bool,
    /// An asynchronous speaker's feedback endpoint, if it names one.
    pub feedback: Option<Feedback>,
    /// Why NoWayOS does not stream it, if it does not.
    pub refusal: Option<Refusal>,
}

impl Stream {
    /// Whether the device sends (a microphone) rather than receives.
    #[must_use]
    pub const fn inbound(&self) -> bool {
        self.endpoint & 0x80 != 0
    }

    /// The most bytes one interval carries: a packet each transaction.
    #[must_use]
    pub const fn interval_bytes(&self) -> u16 {
        self.max_packet.saturating_mul(self.transactions as u16)
    }

    /// xHCI's Max Burst Size for it: the transactions beyond the first.
    #[must_use]
    pub const fn max_burst(&self) -> u8 {
        self.transactions.saturating_sub(1)
    }

    /// Whether its rate is set by a request after its setting is selected:
    /// its endpoint has the control, or its format takes several rates.
    #[must_use]
    pub fn sets_rate(&self) -> bool {
        self.refusal.is_none()
            && self.rate != 0
            && (self.rate_control || self.format.is_some_and(|format| format.rates.several()))
    }

    /// How it ranks against another setting of its interface: one NoWayOS
    /// streams, then one of 16-bit samples, then one at the rate asked for.
    fn rank(&self, wanted: u32) -> (bool, bool, bool) {
        (
            self.refusal.is_none(),
            self.format.is_some_and(|format| format.sixteen_bit()),
            self.rate == wanted,
        )
    }
}

/// A configuration's audio streams: up to [`MAX_STREAMS`], one per
/// interface, in the order the configuration lists them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streams {
    found: [Option<Stream>; MAX_STREAMS],
}

impl Streams {
    /// The streams found.
    pub fn iter(&self) -> impl Iterator<Item = &Stream> {
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

    /// Stream `n`, in order.
    #[must_use]
    pub fn get(&self, n: usize) -> Option<Stream> {
        self.iter().nth(n).copied()
    }

    /// Records `stream`, in place of its interface's setting found before
    /// if it ranks above it, or in a free place.
    fn offer(&mut self, stream: Stream, wanted: u32) {
        if let Some(found) = self
            .found
            .iter_mut()
            .flatten()
            .find(|found| found.interface == stream.interface)
        {
            if stream.rank(wanted) > found.rank(wanted) {
                *found = stream;
            }
            return;
        }
        if let Some(slot) = self.found.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(stream);
        }
    }
}

/// A data endpoint as a walk first sees it.
#[derive(Clone, Copy, Default)]
struct Data {
    address: u8,
    max_packet: u16,
    transactions: u8,
    interval: u8,
    sync: u8,
    /// bSynchAddress: 0 for none.
    synch: u8,
}

/// What the walk has seen of the alternate setting it is in.
#[derive(Clone, Copy, Default)]
struct Setting {
    interface: u8,
    alternate: u8,
    streaming: bool,
    pcm: bool,
    format: Option<Pcm>,
    endpoint: Option<Data>,
    feedback: Option<Feedback>,
    max_packets_only: bool,
    rate_control: bool,
}

impl Setting {
    /// The stream this setting offers, if it is a streaming interface's
    /// setting other than 0 with an isochronous data endpoint, at the rate
    /// nearest `wanted` its packets at `speed` hold - or refused, and why.
    fn stream(&self, speed: Speed, wanted: u32) -> Option<Stream> {
        let data = self.endpoint?;
        if !self.streaming || self.alternate == 0 {
            return None;
        }
        let inbound = data.address & 0x80 != 0;
        let mut stream = Stream {
            interface: self.interface,
            alternate: self.alternate,
            endpoint: data.address,
            max_packet: data.max_packet,
            transactions: data.transactions,
            interval: data.interval,
            sync: data.sync,
            format: if self.pcm { self.format } else { None },
            max_packets_only: self.max_packets_only,
            rate: 0,
            rate_control: self.rate_control,
            // Only an asynchronous speaker is paced by its device.
            feedback: self
                .feedback
                .filter(|_| data.sync == 1 && !inbound && data.synch != 0),
            refusal: None,
        };
        let interval_us = interval_micros(isoch_interval(speed, data.interval));
        stream.refusal = match stream.format {
            None => Some(Refusal::NoFormat),
            Some(format) if format.channels > MAX_CHANNELS => {
                Some(Refusal::Channels(format.channels))
            }
            Some(format) => {
                // The highest rate whose frames each interval, rounded up,
                // fill no more than an interval's bytes.
                let frames = usize::from(stream.interval_bytes())
                    .checked_div(format.frame_bytes())
                    .unwrap_or(0);
                let most = u64::try_from(frames)
                    .unwrap_or(0)
                    .saturating_mul(MICROS)
                    .checked_div(u64::from(interval_us))
                    .unwrap_or(0);
                match format
                    .rates
                    .nearest(wanted, u32::try_from(most).unwrap_or(u32::MAX))
                {
                    Some(rate) => {
                        stream.rate = rate;
                        None
                    }
                    None => Some(Refusal::NoRate),
                }
            }
        };
        Some(stream)
    }
}

/// A Type I format type descriptor's PCM format, if every field is in
/// range and the descriptor holds the rates it claims: 6 bytes for a
/// continuous range, 3 for each discrete rate. Of discrete rates, the first
/// [`MAX_RATES`] are kept; a rate of 0 is no format.
fn type_i(descriptor: &[u8]) -> Option<Pcm> {
    let byte = |at: usize| descriptor.get(at).copied();
    let rate_at = |at: usize| -> Option<u32> {
        let rate = u32::from_le_bytes([
            byte(at)?,
            byte(at.saturating_add(1))?,
            byte(at.saturating_add(2))?,
            0,
        ]);
        (rate != 0).then_some(rate)
    };
    if byte(3)? != FORMAT_TYPE_I {
        return None;
    }
    let (channels, subframe, bits, count) = (byte(4)?, byte(5)?, byte(6)?, byte(7)?);
    let needed = if count == 0 {
        6
    } else {
        usize::from(count).saturating_mul(3)
    };
    if descriptor.len() < needed.saturating_add(8)
        || !(1..=8).contains(&channels)
        || !(1..=4).contains(&subframe)
        || bits == 0
        || u32::from(bits) > u32::from(subframe).saturating_mul(8)
    {
        return None;
    }
    let rates = if count == 0 {
        let (low, high) = (rate_at(8)?, rate_at(11)?);
        if low > high {
            return None;
        }
        Rates::Range { low, high }
    } else {
        let mut list = [0u32; MAX_RATES];
        let kept = usize::from(count).min(MAX_RATES);
        for (index, slot) in list.iter_mut().enumerate().take(kept) {
            *slot = rate_at(index.saturating_mul(3).saturating_add(8))?;
        }
        Rates::Discrete {
            count: u8::try_from(kept).ok()?,
            list,
        }
    };
    Some(Pcm {
        channels,
        subframe,
        bits,
        rates,
    })
}

/// Walks a copy of a whole configuration and returns its audio streams, as
/// [`streams_at`] does, for a full-speed device at [`DEFAULT_RATE`].
///
/// # Errors
///
/// As for [`streams_at`].
pub fn streams(config: &[u8]) -> Result<Streams, DescriptorError> {
    streams_at(config, Speed::Full, DEFAULT_RATE)
}

/// Walks a copy of a whole configuration and returns its audio streams:
/// for each audio streaming interface, its best alternate setting (other
/// than 0) with an isochronous data endpoint - a microphone's IN or a
/// speaker's OUT - for a device at `speed`, with the rate nearest `wanted`
/// its format takes and its packets hold (the module documentation, "What
/// NoWayOS streams"), or refused and why. At most [`MAX_STREAMS`], the
/// first interfaces.
///
/// The walk ends as [`boot_interfaces`](crate::descriptor::boot_interfaces)'
/// does: at wTotalLength or the end of the bytes given.
///
/// # Errors
///
/// [`DescriptorError`] if the header is not a configuration's, or any
/// descriptor's length is under 2, runs past wTotalLength, or is shorter
/// than an interface or endpoint descriptor needs.
pub fn streams_at(config: &[u8], speed: Speed, wanted: u32) -> Result<Streams, DescriptorError> {
    let mut found = Streams::default();
    let mut current = Setting::default();
    walk(config, |at, descriptor| {
        let byte = |offset: usize| descriptor.get(offset).copied().unwrap_or(0);
        let length = byte(0);
        match byte(1) {
            descriptor_type::INTERFACE => {
                if length < 9 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                if let Some(stream) = current.stream(speed, wanted) {
                    found.offer(stream, wanted);
                }
                current = Setting {
                    interface: byte(2),
                    alternate: byte(3),
                    streaming: byte(5) == CLASS_AUDIO && byte(6) == SUBCLASS_STREAMING,
                    ..Setting::default()
                };
            }
            CS_INTERFACE if current.streaming => match byte(2) {
                GENERAL if length >= 7 => current.pcm = le16(descriptor, 5) == Some(PCM),
                FORMAT_TYPE if length >= 8 && current.format.is_none() => {
                    current.format = type_i(descriptor);
                }
                _ => {}
            },
            descriptor_type::ENDPOINT => {
                if length < 7 {
                    return Err(DescriptorError::BadLength { at, length });
                }
                let (address, attributes) = (byte(2), byte(3));
                let packet = le16(descriptor, 4).unwrap_or(0);
                let max_packet = packet & 0x7FF;
                let extra = (packet >> 11) & 0x3;
                // bRefresh and bSynchAddress: an audio endpoint descriptor's
                // last two bytes, 9 in all (Audio 1.0, sections 4.6.1.1 and
                // 4.6.2.1).
                let (refresh, synch) = if length >= 9 {
                    (byte(7), byte(8))
                } else {
                    (0, 0)
                };
                let isochronous = attributes & 0x3 == 0x1;
                let valid = isochronous && address & 0x0F != 0 && (1..=1024).contains(&max_packet);
                match current.endpoint {
                    // Isochronous, a data endpoint (usage, bits 5:4, 00); a
                    // size USB allows and no reserved burst value.
                    None if current.streaming
                        && valid
                        && (attributes >> 4) & 0x3 == 0
                        && extra < 3 =>
                    {
                        current.endpoint = Some(Data {
                            address,
                            max_packet,
                            transactions: u8::try_from(extra).unwrap_or(0).saturating_add(1),
                            interval: byte(6),
                            sync: (attributes >> 2) & 0x3,
                            synch,
                        });
                    }
                    // The synch endpoint its data endpoint names: IN, with
                    // room for a feedback value.
                    Some(data)
                        if valid
                            && current.feedback.is_none()
                            && data.synch != 0
                            && address == data.synch
                            && address & 0x80 != 0
                            && max_packet >= 3 =>
                    {
                        current.feedback = Some(Feedback {
                            endpoint: address,
                            max_packet,
                            interval: byte(6),
                            refresh,
                        });
                    }
                    _ => {}
                }
            }
            // The class-specific endpoint descriptor for the data endpoint
            // just seen: its controls and MaxPacketsOnly, bmAttributes D0
            // and D7.
            CS_ENDPOINT if current.endpoint.is_some() && length >= 7 && byte(2) == GENERAL => {
                current.max_packets_only = byte(3) & 0x80 != 0;
                current.rate_control = byte(3) & SAMPLING_FREQUENCY != 0;
            }
            _ => {}
        }
        Ok(())
    })?;
    if let Some(stream) = current.stream(speed, wanted) {
        found.offer(stream, wanted);
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

    /// An invented full-speed USB speaker: an audio control interface (0)
    /// and a streaming interface (1) whose alternate setting 1 takes
    /// 48 kHz 16-bit stereo PCM on isochronous OUT endpoint 1, 192 bytes
    /// every 1 ms frame, adaptive.
    pub(crate) const SPEAKER_CONFIG: [u8; 100] = [
        9, 2, 100, 0, 2, 1, 0, 0x80, 50, // configuration 1, 2 interfaces
        9, 4, 0, 0, 0, 1, 1, 0, 0, // interface 0: audio control
        9, 0x24, 1, 0x00, 0x01, 30, 0, 1, 1, // AC header, 30 bytes, interface 1
        12, 0x24, 2, 1, 0x01, 0x01, 0, 2, 3, 0, 0, 0, // input terminal 1: USB streaming
        9, 0x24, 3, 2, 0x01, 0x03, 0, 1, 0, // output terminal 2: speaker
        9, 4, 1, 0, 0, 1, 2, 0, 0, // interface 1, alternate 0: no endpoint
        9, 4, 1, 1, 1, 1, 2, 0, 0, // interface 1, alternate 1
        7, 0x24, 1, 1, 1, 0x01, 0x00, // AS general: terminal 1, PCM
        11, 0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xBB, 0x00, // Type I: 2 x 16 bits, 48000 Hz
        9, 5, 0x01, 0x09, 192, 0, 1, 0, 0, // endpoint 0x01: isochronous, adaptive, 192 bytes
        7, 0x25, 1, 0x00, 0, 0, 0, // class-specific endpoint
    ];

    /// An invented high-speed camera: a video interface it does not use,
    /// then an audio streaming interface (3) for its microphone - 16 kHz
    /// 16-bit mono on IN endpoint 4, 32 bytes every 1 ms (bInterval 4) -
    /// whose alternate setting 1 has a feedback endpoint only, and 2 the
    /// data endpoint and two discrete rates.
    const CAMERA_CONFIG: [u8; 105] = [
        9, 2, 105, 0, 3, 1, 0, 0x80, 250, //
        9, 4, 0, 0, 1, 0x0E, 2, 0, 0, // a video streaming interface
        7, 5, 0x81, 0x05, 0x00, 0x0C, 1, // an isochronous endpoint: not audio
        9, 4, 3, 0, 0, 1, 2, 0, 0, // interface 3, alternate 0
        9, 4, 3, 1, 1, 1, 2, 0, 0, // alternate 1
        7, 5, 0x83, 0x11, 3, 0, 4, // a feedback endpoint: not data
        9, 4, 3, 2, 1, 1, 2, 0, 0, // alternate 2
        7, 0x24, 1, 1, 1, 0x01, 0x00, //
        14, 0x24, 2, 1, 1, 2, 16, 2, 0x80, 0x3E, 0x00, 0x00, 0x7D, 0x00, // 16000 or 32000 Hz
        9, 5, 0x84, 0x05, 32, 0, 4, 0, 0, // endpoint 0x84: isochronous, asynchronous
        7, 0x25, 1, 0x81, 0, 0, 0, // MaxPacketsOnly, sampling frequency control
        9, 4, 3, 3, 1, 1, 2, 0, 0, // alternate 3: a second setting, not taken
    ];

    /// An invented full-speed headset's speaker, asynchronous: two
    /// settings of its streaming interface (1) - alternate 1, 24-bit stereo
    /// at 48 kHz; alternate 2, 16-bit stereo at 44.1 or 48 kHz, its data
    /// endpoint naming feedback endpoint 0x82, which follows it (bRefresh
    /// 5), and its sampling frequency control.
    pub(crate) const ASYNC_SPEAKER_CONFIG: [u8; 116] = [
        9, 2, 116, 0, 2, 1, 0, 0x80, 50, //
        9, 4, 1, 0, 0, 1, 2, 0, 0, // interface 1, alternate 0
        9, 4, 1, 1, 1, 1, 2, 0, 0, // alternate 1: 24-bit
        7, 0x24, 1, 1, 1, 0x01, 0x00, //
        11, 0x24, 2, 1, 2, 3, 24, 1, 0x80, 0xBB, 0x00, //
        9, 5, 0x01, 0x05, 32, 1, 1, 0, 0, // 288 bytes, asynchronous, no synch endpoint
        7, 0x25, 1, 0x00, 0, 0, 0, //
        9, 4, 1, 2, 2, 1, 2, 0, 0, // alternate 2: 16-bit, two endpoints
        7, 0x24, 1, 1, 1, 0x01, 0x00, //
        14, 0x24, 2, 1, 2, 2, 16, 2, 0x44, 0xAC, 0x00, 0x80, 0xBB, 0x00, // 44100 or 48000
        9, 5, 0x01, 0x05, 200, 0, 1, 0, 0x82, // OUT 0x01, 200 bytes, synch 0x82
        7, 0x25, 1, 0x01, 0, 0, 0, // sampling frequency control
        9, 5, 0x82, 0x11, 3, 0, 1, 5, 0, // IN 0x82: feedback, 3 bytes, bRefresh 5
    ];

    #[test]
    fn a_speakers_stream() {
        let streams = streams(&SPEAKER_CONFIG).unwrap();
        let mut list = [0u32; MAX_RATES];
        list[0] = 48_000;
        assert_eq!(
            streams.iter().copied().collect::<Vec<_>>(),
            [Stream {
                interface: 1,
                alternate: 1,
                endpoint: 0x01,
                max_packet: 192,
                transactions: 1,
                interval: 1,
                sync: 2,
                format: Some(Pcm {
                    channels: 2,
                    subframe: 2,
                    bits: 16,
                    rates: Rates::Discrete { count: 1, list },
                }),
                max_packets_only: false,
                rate: 48_000,
                rate_control: false,
                feedback: None,
                refusal: None,
            }]
        );
        let stream = streams.get(0).unwrap();
        assert!(!stream.inbound());
        assert_eq!((stream.interval_bytes(), stream.max_burst()), (192, 0));
        assert_eq!(stream.format.unwrap().fixed_rate(), Some(48_000));
        assert_eq!(stream.format.unwrap().frame_bytes(), 4);
        assert!(!stream.sets_rate(), "one rate and no control: no request");
        assert_eq!(streams.get(1), None);
        // Asked for 44.1 kHz, it still runs at the one rate it takes.
        let stream = streams_at(&SPEAKER_CONFIG, Speed::Full, 44_100)
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(stream.rate, 48_000);
    }

    #[test]
    fn a_cameras_microphone_skips_its_video_and_feedback_endpoints() {
        let found = streams_at(&CAMERA_CONFIG, Speed::High, DEFAULT_RATE).unwrap();
        assert_eq!(found.len(), 1);
        let mic = found.get(0).unwrap();
        assert_eq!(
            (mic.interface, mic.alternate, mic.endpoint, mic.max_packet),
            (3, 2, 0x84, 32)
        );
        assert!(mic.inbound() && mic.max_packets_only && mic.rate_control);
        assert_eq!((mic.interval, mic.sync), (4, 1));
        let format = mic.format.unwrap();
        assert_eq!(format.rates.iter().collect::<Vec<_>>(), [16_000, 32_000]);
        assert_eq!(format.fixed_rate(), None, "two rates: a request picks");
        // 32 bytes every 1 ms hold 16 frames: 16 kHz, not 32.
        assert_eq!((mic.rate, mic.refusal), (16_000, None));
        assert!(mic.sets_rate());
        assert_eq!(mic.feedback, None, "a microphone is not paced");
        // At full speed bInterval 4 is 8 ms: 16 frames are 2 kHz, and no
        // rate it takes fits.
        let mic = streams(&CAMERA_CONFIG).unwrap().get(0).unwrap();
        assert_eq!((mic.rate, mic.refusal), (0, Some(Refusal::NoRate)));
        assert!(!mic.sets_rate());
    }

    #[test]
    fn of_a_speakers_settings_the_16_bit_one_with_its_feedback_endpoint() {
        for (wanted, rate) in [(48_000, 48_000), (44_100, 44_100), (8_000, 44_100)] {
            let found = streams_at(&ASYNC_SPEAKER_CONFIG, Speed::Full, wanted).unwrap();
            assert_eq!(found.len(), 1);
            let speaker = found.get(0).unwrap();
            assert_eq!((speaker.alternate, speaker.rate), (2, rate), "{wanted}");
            assert!(speaker.sets_rate());
            assert_eq!(
                speaker.feedback,
                Some(Feedback {
                    endpoint: 0x82,
                    max_packet: 3,
                    interval: 1,
                    refresh: 5,
                })
            );
            // Its feedback endpoint, as a stream to set up: polled every
            // 2^5 frames.
            let feedback = speaker.feedback.unwrap().as_stream(1, 2, Speed::Full);
            assert!(feedback.inbound());
            assert_eq!((feedback.interval, feedback.max_packet), (6, 3));
        }
        // With alternate 2 cut off, the 24-bit setting is taken.
        let mut config = ASYNC_SPEAKER_CONFIG[..61].to_vec();
        config[2] = 61;
        let speaker = streams(&config).unwrap().get(0).unwrap();
        assert_eq!((speaker.alternate, speaker.rate), (1, 48_000));
        assert_eq!(speaker.format.unwrap().subframe, 3);
        assert_eq!(speaker.feedback, None, "no synch address, no feedback");
    }

    #[test]
    fn a_headset_has_two_streams_and_a_third_finds_no_place() {
        // The speaker's configuration with two more streaming interfaces,
        // 2 (a microphone) and 3.
        let mut config = SPEAKER_CONFIG.to_vec();
        for (number, address) in [(2u8, 0x82u8), (3, 0x83)] {
            config.extend_from_slice(&[9, 4, number, 1, 1, 1, 2, 0, 0]);
            config.extend_from_slice(&[9, 5, address, 0x05, 100, 0, 1, 0, 0]);
        }
        let total = u16::try_from(config.len()).unwrap();
        config[2..4].copy_from_slice(&total.to_le_bytes());
        let streams = streams(&config).unwrap();
        assert_eq!(
            streams
                .iter()
                .map(|found| (found.interface, found.endpoint))
                .collect::<Vec<_>>(),
            [(1, 0x01), (2, 0x82)]
        );
        // No AS general or format descriptor: no format, refused.
        let mic = streams.get(1).unwrap();
        assert_eq!((mic.format, mic.refusal), (None, Some(Refusal::NoFormat)));
        assert_eq!(
            Refusal::NoFormat.to_string(),
            "no PCM format this driver reads"
        );
    }

    #[test]
    fn a_high_bandwidth_endpoint_and_formats_out_of_range() {
        let mut config = SPEAKER_CONFIG;
        // wMaxPacketSize 0x1400: 1024 bytes, 3 transactions a microframe.
        config[88..90].copy_from_slice(&0x1400u16.to_le_bytes());
        let stream = streams(&config).unwrap().get(0).unwrap();
        assert_eq!((stream.max_packet, stream.transactions), (1024, 3));
        assert_eq!((stream.interval_bytes(), stream.max_burst()), (3072, 2));
        // Bits 12:11 of 3 are reserved: no stream.
        config[88..90].copy_from_slice(&0x1C00u16.to_le_bytes());
        assert!(streams(&config).unwrap().is_empty());

        // A format with a resolution wider than its subframe, no channels,
        // claiming more rates than it holds, of another type, or with a
        // subframe of 5 bytes, is no format.
        for (at, value) in [(79, 17u8), (77, 0), (80, 2), (76, 2), (78, 5)] {
            let mut config = SPEAKER_CONFIG;
            config[at] = value;
            assert_eq!(
                streams(&config).unwrap().get(0).unwrap().format,
                None,
                "byte {at}"
            );
        }
        // Three channels are a format, but refused.
        let mut config = SPEAKER_CONFIG;
        config[77] = 3;
        let stream = streams(&config).unwrap().get(0).unwrap();
        assert_eq!(stream.refusal, Some(Refusal::Channels(3)));
        // A rate of 0 is no format; one below 8 kHz is refused.
        let mut config = SPEAKER_CONFIG;
        config[81..84].copy_from_slice(&[0, 0, 0]);
        assert_eq!(streams(&config).unwrap().get(0).unwrap().format, None);
        config[81..84].copy_from_slice(&7_999u32.to_le_bytes()[..3]);
        let stream = streams(&config).unwrap().get(0).unwrap();
        assert_eq!(stream.refusal, Some(Refusal::NoRate));
        // A format other than PCM is no format.
        let mut config = SPEAKER_CONFIG;
        config[71] = 2;
        assert_eq!(streams(&config).unwrap().get(0).unwrap().format, None);
        // An interface that is not audio streaming has no stream.
        let mut config = SPEAKER_CONFIG;
        config[62] = 0xFF;
        assert!(streams(&config).unwrap().is_empty());
        // Cut short by the buffer before its endpoint: no stream. A short
        // endpoint descriptor is refused, as the boot walk refuses one.
        assert!(streams(&SPEAKER_CONFIG[..90]).unwrap().is_empty());
        let mut config = SPEAKER_CONFIG;
        config[84] = 5;
        assert!(streams(&config).is_err());
    }

    #[test]
    fn a_continuous_range_and_the_nearest_rate() {
        // The speaker's format as a range, 8 to 96 kHz.
        let mut config = SPEAKER_CONFIG.to_vec();
        let range = [
            14u8, 0x24, 2, 1, 2, 2, 16, 0, 0x40, 0x1F, 0x00, 0x00, 0x77, 0x01,
        ];
        config.splice(73..84, range);
        let total = u16::try_from(config.len()).unwrap();
        config[2..4].copy_from_slice(&total.to_le_bytes());
        let stream = streams_at(&config, Speed::Full, 44_100)
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            stream.format.unwrap().rates,
            Rates::Range {
                low: 8_000,
                high: 96_000
            }
        );
        assert_eq!(stream.rate, 44_100);
        // 192 bytes a frame hold 48 stereo frames: 48 kHz at most.
        let stream = streams_at(&config, Speed::Full, 96_000)
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(stream.rate, 48_000);
        assert!(stream.sets_rate());

        let mut list = [0u32; MAX_RATES];
        list[..3].copy_from_slice(&[16_000, 44_100, 48_000]);
        let rates = Rates::Discrete { count: 3, list };
        assert_eq!(
            rates.nearest(46_050, MAX_RATE),
            Some(48_000),
            "a tie goes up"
        );
        assert_eq!(rates.nearest(46_000, MAX_RATE), Some(44_100));
        assert_eq!(rates.nearest(48_000, 45_000), Some(44_100));
        assert_eq!(rates.nearest(48_000, 10_000), None);
        assert!(rates.takes(44_100) && !rates.takes(32_000) && rates.several());
        assert_eq!(rates.single(), None);
        let range = Rates::Range {
            low: 1_000,
            high: 300_000,
        };
        assert_eq!(range.nearest(1, MAX_RATE), Some(MIN_RATE));
        assert_eq!(range.nearest(u32::MAX, u32::MAX), Some(MAX_RATE));
        assert_eq!(range.iter().collect::<Vec<_>>(), [1_000, 300_000]);
    }

    #[test]
    fn samples_are_the_top_of_their_subframes() {
        assert_eq!(sample16(&[]), 0);
        assert_eq!(sample16(&[0x80]), i16::MIN);
        assert_eq!(sample16(&[0x34, 0x12]), 0x1234);
        assert_eq!(sample16(&[0xFF, 0x34, 0x12]), 0x1234);
        assert_eq!(sample16(&[0xFF, 0xFF, 0x00, 0x80]), i16::MIN);
        let mut subframe = [0xAAu8; 4];
        put_sample16(&mut subframe, -2);
        assert_eq!(subframe, [0, 0, 0xFE, 0xFF]);
        let mut one = [0u8; 1];
        put_sample16(&mut one, 0x7F00);
        assert_eq!(one, [0x7F]);
        let format = Pcm {
            channels: 2,
            subframe: 3,
            bits: 24,
            rates: Rates::Range {
                low: 48_000,
                high: 48_000,
            },
        };
        let mut frame = [0u8; 7];
        assert_eq!(format.write_frame(&[1, -1], &mut frame), 6);
        assert_eq!(frame, [0, 1, 0, 0, 0xFF, 0xFF, 0]);
        let mut out = [0i16; 2];
        assert_eq!(format.read_frame(&frame, &mut out), 2);
        assert_eq!(out, [1, -1]);
        assert_eq!(format.read_frame(&frame[..5], &mut out), 0, "a part frame");
        assert_eq!(
            format.write_frame(&[1], &mut frame),
            0,
            "one channel of two"
        );
        assert_eq!(format.fixed_rate(), Some(48_000));
    }

    #[test]
    fn feedback_is_held_near_the_rate() {
        let nominal = nominal_q16(48_000, 1000);
        assert_eq!(nominal, 48 << 16);
        // 48.0 frames a frame in 10.14: 0x0C0000.
        assert_eq!(
            feedback_q16(&[0x00, 0x00, 0x0C], Speed::Full, 1000, nominal),
            Some(48 << 16)
        );
        // 48.25 a frame.
        assert_eq!(
            feedback_q16(&[0x00, 0x10, 0x0C], Speed::Full, 1000, nominal),
            Some((48 << 16) + (1 << 14))
        );
        // 16.16 a microframe at high speed: 6.0 a 125 us microframe, every
        // 1 ms interval 48.
        assert_eq!(
            feedback_q16(&[0, 0, 6, 0], Speed::High, 1000, nominal),
            Some(48 << 16)
        );
        // Far from the rate, or a length that is neither: refused.
        assert_eq!(
            feedback_q16(&[0, 0, 0x10], Speed::Full, 1000, nominal),
            None
        );
        assert_eq!(feedback_q16(&[0, 0], Speed::Full, 1000, nominal), None);
        assert_eq!(feedback_q16(&[0; 5], Speed::Full, 1000, nominal), None);
        assert_eq!(nominal_q16(44_100, 1000), 2_890_137);
    }

    proptest! {
        #[test]
        fn walking_never_panics_and_keeps_its_rules(
            bytes in proptest::collection::vec(any::<u8>(), 0..300),
            high: bool,
            wanted in 0u32..400_000,
        ) {
            let speed = if high { Speed::High } else { Speed::Full };
            if let Ok(found) = streams_at(&bytes, speed, wanted) {
                prop_assert!(found.len() <= MAX_STREAMS);
                for stream in found.iter() {
                    prop_assert!(stream.alternate != 0);
                    prop_assert!((1..=15).contains(&(stream.endpoint & 0x0F)));
                    prop_assert!((1..=1024).contains(&stream.max_packet));
                    prop_assert!((1..=3).contains(&stream.transactions));
                    if let Some(format) = stream.format {
                        prop_assert!((1..=8).contains(&format.channels));
                        prop_assert!((1..=4).contains(&format.subframe));
                        prop_assert!(format.rates.iter().all(|rate| rate != 0));
                    }
                    if stream.refusal.is_none() {
                        let format = stream.format.unwrap();
                        prop_assert!(format.channels <= MAX_CHANNELS);
                        prop_assert!(format.rates.takes(stream.rate));
                        prop_assert!((MIN_RATE..=MAX_RATE).contains(&stream.rate));
                    } else {
                        prop_assert_eq!(stream.rate, 0);
                    }
                    if let Some(feedback) = stream.feedback {
                        prop_assert!(feedback.endpoint & 0x80 != 0 && !stream.inbound());
                        prop_assert!(feedback.max_packet >= 3);
                    }
                }
            }
        }

        #[test]
        fn a_speaker_with_a_byte_changed_never_panics(at in 0usize..116, value: u8, cut in 0usize..117) {
            let mut config = ASYNC_SPEAKER_CONFIG;
            config[at] = value;
            let _ = streams(&config[..cut]);
            let mut config = SPEAKER_CONFIG;
            config[at % 100] = value;
            let _ = streams_at(&config[..cut.min(100)], Speed::High, 44_100);
        }

        #[test]
        fn a_sample_written_reads_back(sample: i16, subframe in 2usize..=4) {
            let mut bytes = [0u8; 4];
            put_sample16(&mut bytes[..subframe], sample);
            prop_assert_eq!(sample16(&bytes[..subframe]), sample);
        }
    }
}
