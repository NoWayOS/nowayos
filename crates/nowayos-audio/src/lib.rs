// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-audio`: the audio channel between the USB host PD and a client
//! PD - a call's sound from a microphone and to a speaker, as
//! 16-bit PCM, with no codec on the station.
//!
//! Two rings over Microkit regions, each written by one side only:
//!
//! * **from the USB host PD** - a lossy ring ([`LossyProducer`] and
//!   [`LossyConsumer`], [`FROM_SLOTS`] slots): [`Captured`] records, the microphone's sound,
//!   and every second a [`Devices`] record, the formats of the speaker and
//!   the microphone attached. The USB host PD never waits for its client: a
//!   client that falls behind loses the oldest records, and a record's
//!   [`Pcm::discontinuity`] says sound was lost before it;
//! * **to the USB host PD** - an ordinary ring (`nowayos_ipc::Producer`,
//!   [`TO_SLOTS`] slots, about a third of a second at 48 kHz in stereo):
//!   [`Play`] records for the speaker, in its own format. The client waits
//!   for room; the USB host PD takes what the speaker plays each tick, and
//!   plays silence when the ring is empty, never waiting either. And a
//!   [`Capture`] record whenever the client wants the microphone started or
//!   stopped: the USB host PD streams a microphone only while its
//!   client last asked it to, and at first it does not.
//!
//! **A record is the device's own sound**, at the device's own rate and
//! channels, as [`Devices`] gives them: 16-bit signed samples, little-endian,
//! a frame of one sample per channel, one or two channels. NoWayOS does not
//! resample; a client that needs another rate converts it itself. A
//! [`Play`] record in another format than the speaker's is dropped.
//!
//! **What is heard or said is a person's**, so no record's [`Debug`] shows
//! its samples, only how many.
//!
//! Each record decodes from a copy already out of shared memory, with every
//! field checked, and has exactly one encoding (`src/proofs.rs`, `make
//! kani`; `fuzz/fuzz_targets/audio_records.rs`).
//!
//! Provisional: the first client is a remote-display session PD.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

mod lossy;
#[cfg(kani)]
mod proofs;

pub use lossy::{LossyConsumer, LossyProducer};

/// The bytes of one ring slot, on both rings.
pub const SLOT_LEN: usize = 1024;

/// Slots on the ring from the USB host PD: 32 records of up to
/// [`MAX_PCM`] bytes, 160 ms of 48 kHz stereo, the newest kept.
pub const FROM_SLOTS: usize = 32;

/// Slots on the ring to the USB host PD: 64, 320 ms of 48 kHz stereo - more
/// than the 186 ms a remote host sends in one block.
pub const TO_SLOTS: usize = 64;

/// The bytes of a ring's messages region: its index, its slots and its
/// epoch (`nowayos_region`'s layout), rounded up to whole pages.
#[must_use]
pub const fn messages_region(slots: usize) -> usize {
    let bytes = slots.saturating_mul(SLOT_LEN).saturating_add(8);
    bytes.div_ceil(4096).saturating_mul(4096)
}

/// The most bytes of samples one record carries: 240 stereo frames - 5 ms
/// at 48 kHz - or 480 mono ones. A multiple of every frame's size.
pub const MAX_PCM: usize = 960;

/// The lowest and highest rates a record may give, in hertz.
pub const MIN_RATE: u32 = 8_000;
/// See [`MIN_RATE`].
pub const MAX_RATE: u32 = 192_000;

/// The most channels a record may carry.
pub const MAX_CHANNELS: u8 = 2;

/// The message kinds on the audio channel.
pub const CAPTURED_KIND: u16 = 0x0A01;
/// See [`CAPTURED_KIND`].
pub const PLAY_KIND: u16 = 0x0A02;
/// See [`CAPTURED_KIND`].
pub const DEVICES_KIND: u16 = 0x0A03;
/// [`Capture`]'s kind.
pub const CAPTURE_KIND: u16 = 0x0A04;

/// The layout version of every record here.
pub const VERSION: u16 = 1;

/// [`Pcm::flags`]: sound was lost, or the stream began, just before this
/// record.
const DISCONTINUITY: u8 = 0x01;

/// The bytes of a PCM record's payload before its samples.
const PCM_PREFIX: usize = 12;

const _: () = {
    assert!(MAX_PCM.is_multiple_of(WORD));
    assert!(MAX_PCM.is_multiple_of(4));
    assert!(nowayos_ipc::HEADER_LEN + PCM_PREFIX + MAX_PCM <= SLOT_LEN);
};

/// A stream's format: its rate and its channels, each in range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    rate: u32,
    channels: u8,
}

impl Format {
    /// `rate` hertz, [`MIN_RATE`] to [`MAX_RATE`], and `channels`, 1 to
    /// [`MAX_CHANNELS`]; `None` otherwise.
    #[must_use]
    pub const fn new(rate: u32, channels: u8) -> Option<Self> {
        if rate >= MIN_RATE && rate <= MAX_RATE && channels >= 1 && channels <= MAX_CHANNELS {
            Some(Self { rate, channels })
        } else {
            None
        }
    }

    /// Frames a second.
    #[must_use]
    pub const fn rate(&self) -> u32 {
        self.rate
    }

    /// Samples a frame.
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// The bytes of one frame: two for each channel.
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        (self.channels as usize).saturating_mul(2)
    }
}

/// Some of a stream's sound: up to [`MAX_PCM`] bytes of whole frames of
/// 16-bit samples in `format`, and where they fall in the stream.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Pcm<const KIND: u16> {
    format: Format,
    flags: u8,
    position: u32,
    /// A whole number of frames, at least one.
    len: usize,
    /// The samples, then zeros: so a record has one encoding.
    bytes: [u8; MAX_PCM],
}

/// USB host PD → client: the microphone's sound.
pub type Captured = Pcm<CAPTURED_KIND>;

/// Client → USB host PD: sound for the speaker.
pub type Play = Pcm<PLAY_KIND>;

impl<const KIND: u16> Pcm<KIND> {
    /// A record of `bytes` - little-endian 16-bit samples, whole frames of
    /// `format`, from one frame to [`MAX_PCM`] bytes - at frame `position`
    /// of its stream; `discontinuity` if sound was lost, or the stream
    /// began, just before it. `None` for any other length.
    #[must_use]
    pub fn from_bytes(
        format: Format,
        position: u32,
        discontinuity: bool,
        bytes: &[u8],
    ) -> Option<Self> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(format.frame_bytes()) {
            return None;
        }
        let mut record = Self {
            format,
            flags: if discontinuity { DISCONTINUITY } else { 0 },
            position,
            len: bytes.len(),
            bytes: [0; MAX_PCM],
        };
        record.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(record)
    }

    /// A record of `samples`, as [`Self::from_bytes`] takes them.
    #[must_use]
    pub fn from_samples(
        format: Format,
        position: u32,
        discontinuity: bool,
        samples: &[i16],
    ) -> Option<Self> {
        let mut bytes = [0u8; MAX_PCM];
        let len = samples.len().checked_mul(2).filter(|len| *len <= MAX_PCM)?;
        for (pair, sample) in bytes.chunks_exact_mut(2).zip(samples) {
            pair.copy_from_slice(&sample.to_le_bytes());
        }
        Self::from_bytes(format, position, discontinuity, bytes.get(..len)?)
    }

    /// Its format.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// The frame of its stream its first frame is, counted from the
    /// stream's start: frames sent before it, wrapping at 2^32.
    #[must_use]
    pub const fn position(&self) -> u32 {
        self.position
    }

    /// Whether sound was lost, or the stream began, just before it.
    #[must_use]
    pub const fn discontinuity(&self) -> bool {
        self.flags & DISCONTINUITY != 0
    }

    /// Its frames: at least one.
    #[must_use]
    pub const fn frames(&self) -> usize {
        match self.len.checked_div(self.format.frame_bytes()) {
            Some(frames) => frames,
            None => 0,
        }
    }

    /// Its samples' bytes: little-endian, frame by frame.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    /// Its samples, frame by frame.
    pub fn samples(&self) -> impl Iterator<Item = i16> + '_ {
        self.bytes().chunks_exact(2).map(|pair| {
            i16::from_le_bytes([
                pair.first().copied().unwrap_or(0),
                pair.get(1).copied().unwrap_or(0),
            ])
        })
    }
}

/// Names its format and length, never its samples: they are what a person
/// said or will hear.
impl<const KIND: u16> fmt::Debug for Pcm<KIND> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pcm")
            .field("format", &self.format)
            .field("position", &self.position)
            .field("discontinuity", &self.discontinuity())
            .field("frames", &self.frames())
            .finish_non_exhaustive()
    }
}

impl<const KIND: u16> Message for Pcm<KIND> {
    const KIND: u16 = KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = PCM_PREFIX + MAX_PCM;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let len = u16::try_from(self.len)
            .ok()
            .filter(|len| {
                let len = usize::from(*len);
                len > 0 && len <= MAX_PCM && len.is_multiple_of(self.format.frame_bytes())
            })
            .ok_or(EncodeError::InvalidValue { field: "len" })?;
        out.write_u32(self.format.rate)?;
        out.write_u8(self.format.channels)?;
        out.write_u8(self.flags)?;
        out.write_u16(len)?;
        out.write_u32(self.position)?;
        out.write_bytes(&self.bytes)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let rate = input.read_u32()?;
        let channels = input.read_u8()?;
        let flags = input.read_u8()?;
        let len = usize::from(input.read_u16()?);
        let position = input.read_u32()?;
        let bytes: [u8; MAX_PCM] = input.read_array()?;
        let format =
            Format::new(rate, channels).ok_or(DecodeError::InvalidValue { field: "format" })?;
        if flags & !DISCONTINUITY != 0 {
            return Err(DecodeError::InvalidValue { field: "flags" });
        }
        if len == 0 || len > MAX_PCM || !len.is_multiple_of(format.frame_bytes()) {
            return Err(DecodeError::InvalidValue { field: "len" });
        }
        if !zero_from(&bytes, len) {
            return Err(DecodeError::NonZeroPadding);
        }
        Ok(Self {
            format,
            flags,
            position,
            len,
            bytes,
        })
    }
}

/// The bytes of a sample area checked at a time: [`zero_from`] reads it as
/// sixteen-byte words, so that its loop runs [`MAX_PCM`] / 16 times
/// whatever the length - few enough steps for Kani to prove the decoder
/// whole.
const WORD: usize = 16;

/// Whether every byte of `bytes` from `len` on is zero.
fn zero_from(bytes: &[u8; MAX_PCM], len: usize) -> bool {
    // The word `len` falls in, and how many of its bytes are samples: the
    // low bytes of its little-endian value, so its padding is the bits
    // above.
    let (Some(first), Some(data)) = (len.checked_div(WORD), len.checked_rem(WORD)) else {
        return false;
    };
    let Some(padding) = u32::try_from(data.saturating_mul(8))
        .ok()
        .and_then(|bits| u128::MAX.checked_shl(bits))
    else {
        return false;
    };
    bytes.chunks_exact(WORD).enumerate().all(|(index, chunk)| {
        let Ok(word) = <[u8; WORD]>::try_from(chunk) else {
            return false;
        };
        let mask = match index.cmp(&first) {
            core::cmp::Ordering::Less => 0,
            core::cmp::Ordering::Equal => padding,
            core::cmp::Ordering::Greater => u128::MAX,
        };
        u128::from_le_bytes(word) & mask == 0
    })
}

/// USB host PD → client: the audio devices attached, by the format each
/// streams in - a speaker's and a microphone's, or none. Sent when either
/// changes and every second, so a client that lost one learns again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Devices {
    /// Counts each change of device: a client that sees it move knows a
    /// device came or went, even between two of the same format.
    pub generation: u32,
    /// The speaker's format: what [`Play`] records must be in.
    pub speaker: Option<Format>,
    /// The microphone's format: what [`Captured`] records are in.
    pub microphone: Option<Format>,
}

/// Writes a format, or none as zeros.
fn write_format(out: &mut Writer<'_>, format: Option<Format>) -> Result<(), EncodeError> {
    let (rate, channels) = format.map_or((0, 0), |format| (format.rate, format.channels));
    out.write_u32(rate)?;
    out.write_u8(channels)?;
    out.write_zeros(3)
}

/// Reads a format, or none from zeros.
fn read_format(input: &mut Reader<'_>, field: &'static str) -> Result<Option<Format>, DecodeError> {
    let rate = input.read_u32()?;
    let channels = input.read_u8()?;
    input.read_zeros(3)?;
    match (rate, channels) {
        (0, 0) => Ok(None),
        _ => Format::new(rate, channels)
            .map(Some)
            .ok_or(DecodeError::InvalidValue { field }),
    }
}

impl Message for Devices {
    const KIND: u16 = DEVICES_KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = 20;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u32(self.generation)?;
        write_format(out, self.speaker)?;
        write_format(out, self.microphone)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            generation: input.read_u32()?,
            speaker: read_format(input, "speaker")?,
            microphone: read_format(input, "microphone")?,
        })
    }
}

/// Client → USB host PD: whether the microphone is to capture. The
/// USB host PD streams a microphone only while its client last asked it to,
/// and not at all before the first record, so nothing is taken in while no
/// call is listening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Capture; or stop, the microphone back at its setting that streams
    /// nothing.
    pub on: bool,
}

impl Message for Capture {
    const KIND: u16 = CAPTURE_KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = 4;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u8(u8::from(self.on))?;
        out.write_zeros(3)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let on = match input.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(DecodeError::InvalidValue { field: "on" }),
        };
        input.read_zeros(3)?;
        Ok(Self { on })
    }
}

/// Makes a stream's records: frames go in as they come, and a record goes
/// out each time [`MAX_PCM`] bytes are in, or when flushed - so a record
/// never waits longer than the caller's flushes apart.
#[derive(Clone, Copy)]
pub struct Assembler<const KIND: u16> {
    format: Option<Format>,
    /// Frames sent so far: the next record's position.
    position: u32,
    /// Whether the next record follows lost sound, or starts the stream.
    discontinuity: bool,
    len: usize,
    bytes: [u8; MAX_PCM],
}

impl<const KIND: u16> Default for Assembler<KIND> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const KIND: u16> Assembler<KIND> {
    /// An assembler with no stream: it takes nothing until [`Self::start`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            format: None,
            position: 0,
            discontinuity: true,
            len: 0,
            bytes: [0; MAX_PCM],
        }
    }

    /// Starts a stream in `format`, at position 0, its first record marked
    /// as a discontinuity; anything held from before is dropped.
    pub const fn start(&mut self, format: Format) {
        *self = Self {
            format: Some(format),
            ..Self::new()
        };
    }

    /// Ends the stream: nothing is taken until the next start.
    pub const fn stop(&mut self) {
        *self = Self::new();
    }

    /// Its stream's format, if one is started.
    #[must_use]
    pub const fn format(&self) -> Option<Format> {
        self.format
    }

    /// Notes that sound was lost: the next record is marked as a
    /// discontinuity.
    pub const fn lost(&mut self) {
        self.discontinuity = true;
    }

    /// Takes one frame - one sample a channel; a frame of another length,
    /// or one with no stream started, is dropped - and sends a record
    /// through `send` when it fills one.
    pub fn push_frame(&mut self, frame: &[i16], send: impl FnMut(&Pcm<KIND>)) {
        let Some(format) = self.format else {
            return;
        };
        if frame.len() != usize::from(format.channels) {
            return;
        }
        for sample in frame {
            if let Some(pair) = self.bytes.get_mut(self.len..self.len.saturating_add(2)) {
                pair.copy_from_slice(&sample.to_le_bytes());
                self.len = self.len.saturating_add(2);
            }
        }
        if self.len.saturating_add(format.frame_bytes()) > MAX_PCM {
            self.flush(send);
        }
    }

    /// Sends what it holds as a record, if anything.
    pub fn flush(&mut self, mut send: impl FnMut(&Pcm<KIND>)) {
        let Some(format) = self.format else {
            return;
        };
        let held = self.bytes.get(..self.len).unwrap_or_default();
        if let Some(record) = Pcm::from_bytes(format, self.position, self.discontinuity, held) {
            send(&record);
            let frames = u32::try_from(record.frames()).unwrap_or(0);
            self.position = self.position.wrapping_add(frames);
            self.discontinuity = false;
        }
        self.len = 0;
    }
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
    use nowayos_ipc::{HEADER_LEN, InMemoryRing, decode, encode};
    use proptest::prelude::*;

    const STEREO_48K: Format = match Format::new(48_000, 2) {
        Some(format) => format,
        None => panic!("a valid format"),
    };

    #[test]
    fn formats_in_range_only() {
        assert_eq!(Format::new(48_000, 2), Some(STEREO_48K));
        assert_eq!(STEREO_48K.frame_bytes(), 4);
        for (rate, channels) in [(7_999, 1), (192_001, 1), (48_000, 0), (48_000, 3)] {
            assert_eq!(Format::new(rate, channels), None, "{rate} {channels}");
        }
    }

    #[test]
    fn a_record_round_trips_and_keeps_its_samples() {
        let samples = [1i16, -1, 0x1234, -0x1234];
        let record = Captured::from_samples(STEREO_48K, 480, true, &samples).unwrap();
        assert_eq!((record.frames(), record.position()), (2, 480));
        assert!(record.discontinuity());
        assert_eq!(record.samples().collect::<Vec<_>>(), samples);
        let mut slot = [0u8; SLOT_LEN];
        let len = encode(&record, &mut slot).unwrap();
        assert_eq!(len, HEADER_LEN + PCM_PREFIX + MAX_PCM);
        assert_eq!(decode::<Captured>(&slot[..len]), Ok(record));
        // The other direction's kind is refused.
        assert!(decode::<Play>(&slot[..len]).is_err());
        // Its Debug shows no sample.
        let shown = format!("{record:?}");
        assert!(
            shown.contains("frames: 2") && !shown.contains("4660"),
            "{shown}"
        );
    }

    #[test]
    fn lengths_that_are_not_whole_frames_are_refused() {
        assert!(Play::from_samples(STEREO_48K, 0, false, &[]).is_none());
        assert!(Play::from_samples(STEREO_48K, 0, false, &[1, 2, 3]).is_none());
        assert!(Play::from_samples(STEREO_48K, 0, false, &[0; MAX_PCM / 2]).is_some());
        assert!(Play::from_samples(STEREO_48K, 0, false, &[0; MAX_PCM / 2 + 2]).is_none());
    }

    /// The encoded bytes of a stereo record of two frames, to break.
    fn encoded() -> Vec<u8> {
        let record = Play::from_samples(STEREO_48K, 7, false, &[1, 2, 3, 4]).unwrap();
        let mut slot = [0u8; SLOT_LEN];
        let len = encode(&record, &mut slot).unwrap();
        slot[..len].to_vec()
    }

    #[test]
    fn a_record_breaking_a_rule_is_refused() {
        let at = HEADER_LEN;
        let cases: [(usize, u8, &str); 7] = [
            (at, 0, "rate below 8 kHz"),
            (at + 4, 3, "three channels"),
            (at + 4, 0, "no channels"),
            (at + 5, 2, "an unknown flag"),
            (at + 6, 0, "no samples"),
            (at + 6, 6, "a frame and a half"),
            (at + PCM_PREFIX + 8, 1, "a sample past the length"),
        ];
        for (byte, value, why) in cases {
            let mut bytes = encoded();
            if byte == at {
                bytes[at..at + 4].copy_from_slice(&[0; 4]);
            } else {
                bytes[byte] = value;
            }
            assert!(decode::<Play>(&bytes).is_err(), "{why}");
        }
        assert!(decode::<Play>(&encoded()).is_ok());
    }

    #[test]
    fn devices_round_trip_with_none_as_zeros() {
        let devices = Devices {
            generation: 3,
            speaker: Some(STEREO_48K),
            microphone: None,
        };
        let mut slot = [0u8; 64];
        let len = encode(&devices, &mut slot).unwrap();
        assert_eq!(decode::<Devices>(&slot[..len]), Ok(devices));
        // A rate with no channels is neither a format nor none.
        let mut broken = slot;
        broken[HEADER_LEN + 12..HEADER_LEN + 16].copy_from_slice(&16_000u32.to_le_bytes());
        assert!(decode::<Devices>(&broken[..len]).is_err());
        // The padding after a format's channels must be zero.
        let mut broken = slot;
        broken[HEADER_LEN + 9] = 1;
        assert!(decode::<Devices>(&broken[..len]).is_err());
    }

    #[test]
    fn capture_round_trips_and_takes_only_its_two_values() {
        for on in [false, true] {
            let mut slot = [0u8; 64];
            let len = encode(&Capture { on }, &mut slot).unwrap();
            assert_eq!(decode::<Capture>(&slot[..len]), Ok(Capture { on }));
        }
        let mut slot = [0u8; 64];
        let len = encode(&Capture { on: true }, &mut slot).unwrap();
        // Neither on nor off.
        let mut broken = slot;
        broken[HEADER_LEN] = 2;
        assert!(decode::<Capture>(&broken[..len]).is_err());
        // The padding must be zero.
        let mut broken = slot;
        broken[HEADER_LEN + 3] = 1;
        assert!(decode::<Capture>(&broken[..len]).is_err());
    }

    #[test]
    fn the_assembler_sends_full_records_then_the_rest_on_a_flush() {
        let mono = Format::new(16_000, 1).unwrap();
        let mut assembler = Assembler::<CAPTURED_KIND>::new();
        let mut sent = Vec::new();
        // Nothing before a start.
        assembler.push_frame(&[1], |record| sent.push(*record));
        assembler.flush(|record| sent.push(*record));
        assert!(sent.is_empty());
        assembler.start(mono);
        for n in 0..500i16 {
            assembler.push_frame(&[n], |record| sent.push(*record));
        }
        // A stereo frame on a mono stream is dropped.
        assembler.push_frame(&[1, 2], |record| sent.push(*record));
        assert_eq!(sent.len(), 1, "480 frames fill a record");
        assembler.flush(|record| sent.push(*record));
        assembler.flush(|record| sent.push(*record));
        assert_eq!(sent.len(), 2, "an empty flush sends nothing");
        assert_eq!((sent[0].position(), sent[0].frames()), (0, 480));
        assert_eq!((sent[1].position(), sent[1].frames()), (480, 20));
        assert!(sent[0].discontinuity() && !sent[1].discontinuity());
        assert_eq!(sent[1].samples().next(), Some(480));
        assembler.lost();
        assembler.push_frame(&[9], |record| sent.push(*record));
        assembler.flush(|record| sent.push(*record));
        assert!(sent[2].discontinuity());
        assert_eq!(sent[2].position(), 500);
    }

    #[test]
    fn records_cross_the_lossy_ring_and_a_slow_client_loses_the_oldest() {
        let ring: InMemoryRing<FROM_SLOTS, SLOT_LEN> = InMemoryRing::new();
        let mut tx = LossyProducer::new(&ring).unwrap();
        let mut rx = LossyConsumer::new(&ring).unwrap();
        for position in 0..40u32 {
            let record = Captured::from_samples(STEREO_48K, position, false, &[0; 4]).unwrap();
            tx.send(&record).unwrap();
        }
        let first = rx.try_recv::<Captured>().unwrap().unwrap();
        assert_eq!(first.position(), 40 - (FROM_SLOTS as u32 - 1));
        assert_eq!(rx.lost(), 40 - (FROM_SLOTS as u64 - 1));
    }

    #[test]
    fn the_regions_hold_their_rings() {
        assert_eq!(messages_region(FROM_SLOTS), 0x9000);
        assert_eq!(messages_region(TO_SLOTS), 0x11000);
    }

    proptest! {
        #[test]
        fn decoding_any_payload_never_panics_and_accepts_one_encoding(
            payload in proptest::collection::vec(any::<u8>(), PCM_PREFIX + MAX_PCM),
        ) {
            let mut bytes = encoded();
            bytes[HEADER_LEN..].copy_from_slice(&payload);
            if let Ok(record) = decode::<Play>(&bytes) {
                let mut again = [0u8; SLOT_LEN];
                let len = encode(&record, &mut again).unwrap();
                prop_assert_eq!(&again[..len], &bytes[..]);
                prop_assert!(record.frames() >= 1);
            }
        }

        #[test]
        fn records_built_from_samples_round_trip(
            channels in 1u8..=2,
            rate in MIN_RATE..=MAX_RATE,
            frames in 1usize..=240,
            position: u32,
            discontinuity: bool,
            seed: i16,
        ) {
            let format = Format::new(rate, channels).unwrap();
            let samples: Vec<i16> = (0..frames * usize::from(channels))
                .map(|n| seed.wrapping_mul(n as i16))
                .collect();
            let record = Captured::from_samples(format, position, discontinuity, &samples).unwrap();
            let mut slot = [0u8; SLOT_LEN];
            let len = encode(&record, &mut slot).unwrap();
            let back = decode::<Captured>(&slot[..len]).unwrap();
            prop_assert_eq!(back, record);
            prop_assert_eq!(back.samples().collect::<Vec<_>>(), samples);
        }
    }
}
