// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-audio-loop`: a test instrument - the audio client of
//! one or two USB host PDs, through the rings a product's client uses
//! (`nowayos_audio`), so a person hears what a call would carry.
//!
//! Each USB host PD is a side, its rings and channel this PD's (below).
//! From the [`Devices`] records it learns which side has a speaker and
//! which a microphone, the first of each, and their formats. Then:
//!
//! * **with a microphone**, in turns: it asks the microphone to capture
//!   while it records and to stop while it plays back (a [`Capture`]
//!   record each time); it records [`RECORD_SECONDS`] of the
//!   microphone's sound into its recording region while the speaker plays
//!   a [`TONE_HZ`] tone for [`TONE_SECONDS`] and then silence; then it
//!   plays the recording back, converted to the speaker's rate and
//!   channels - linearly, a test instrument's conversion, not a product's;
//!   and again;
//! * **with no microphone**, the speaker plays the tone without end - what
//!   QEMU's USB speaker, which has none, writes to its WAV file.
//!
//! It keeps [`LEAD`] records queued for the speaker, topped up whenever the
//! USB host PD notifies it - every tick it took or sent a record - and
//! never more, so what it plays is heard within a tenth of a second.
//!
//! On serial, each side's devices when they change; every
//! [`REPORT_SECONDS`] of sound sent to the speaker; and each recording's
//! format, its loudest sample - a level, never the sound - its records lost
//! and its gaps, then that it is played back. Never in a product's image.
//!
//! Capabilities (from the system description), for side `k` (0 or 1): the
//! ring from its USB host PD - its control region read-write
//! (`nowayos_rw_region_{2k}`), its messages read-only
//! (`nowayos_ro_region_{2k}`) - and the ring to it - its messages
//! read-write (`nowayos_rw_region_{2k+1}`), its control read-only
//! (`nowayos_ro_region_{2k+1}`) - and channel `k`, on which it is notified;
//! and its recording region, read-write (`nowayos_rw_region_4`). No
//! devices, no `unsafe`.

#![no_std]
#![no_main]

use nowayos_audio::{
    CAPTURED_KIND, Capture, Captured, DEVICES_KIND, Devices, FROM_SLOTS, Format, LossyConsumer,
    MAX_PCM, Play, SLOT_LEN, TO_SLOTS,
};
use nowayos_display::FrameMemory;
use nowayos_ipc::{Header, Producer, decode};
use nowayos_region::{
    Frame, ReadWrite, RegionError, RingRx, RingTx, read_only, read_write, ring_consumer,
    ring_producer,
};
use nowayos_usb::isoch::Tone;
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// The USB host PDs it can be the client of.
const SIDES: usize = 2;

/// Its recording region.
const RECORDING: usize = 4;

/// The seconds of the microphone each turn records.
const RECORD_SECONDS: u32 = 5;

/// The tone, and how long it plays at the start of each turn.
const TONE_HZ: u32 = 440;
const TONE_SECONDS: u32 = 1;
const TONE_AMPLITUDE: u16 = 0x2000;

/// The records it keeps queued for the speaker.
const LEAD: u32 = 16;

/// How often it says how much it has sent the speaker.
const REPORT_SECONDS: u64 = 10;

/// One USB host PD's rings.
struct Side {
    from: RingRx<FROM_SLOTS, SLOT_LEN>,
    to: RingTx<TO_SLOTS, SLOT_LEN>,
    devices: Option<Devices>,
}

/// Where it is in a turn.
#[derive(Clone, Copy, Debug)]
enum Phase {
    /// Recording: this many frames so far.
    Listening(usize),
    /// Playing `frames` frames of recording back; this far through, in
    /// 65536ths of a recorded frame.
    Playing { frames: usize, at: u64 },
}

/// The test instrument's state between notifications.
struct AudioLoop {
    name: &'static str,
    sides: [Option<Side>; SIDES],
    recording: Option<Frame<ReadWrite>>,
    /// The side with the speaker, and its format.
    speaker: Option<(usize, Format)>,
    /// The side with the microphone, and its format.
    microphone: Option<(usize, Format)>,
    phase: Phase,
    tone: Option<Tone>,
    /// Frames sent the speaker this turn, and in all.
    turn_frames: u64,
    sent: u64,
    reports: u64,
    /// Of the recording being made: its loudest sample, its records lost
    /// and its gaps.
    peak: u16,
    lost: u64,
    gaps: u32,
    /// What the microphone's side was last asked, if anything.
    capturing: Option<bool>,
}

#[protection_domain]
fn init() -> AudioLoop {
    let name = pd_name().unwrap_or("audio-loop");
    debug_println!("NOWAYOS: {name}: starting");
    let mut pd = AudioLoop {
        name,
        sides: [None, None],
        recording: None,
        speaker: None,
        microphone: None,
        phase: Phase::Listening(0),
        tone: None,
        turn_frames: 0,
        sent: 0,
        reports: 0,
        peak: 0,
        lost: 0,
        gaps: 0,
        capturing: None,
    };
    for (k, side) in pd.sides.iter_mut().enumerate() {
        match bind(k) {
            Ok(bound) => *side = bound,
            Err(error) => debug_println!("NOWAYOS: {name}: side {k}: its rings: {error}"),
        }
    }
    match read_write(RECORDING).and_then(|region| region.into_frame(0)) {
        Ok(frame) => pd.recording = Some(frame),
        Err(error) => debug_println!("NOWAYOS: {name}: no recording region: {error}"),
    }
    let sides = pd.sides.iter().flatten().count();
    debug_println!("NOWAYOS: {name}: ready: {sides} USB host PDs to hear from");
    pd
}

/// Side `k`'s rings: `Ok(None)` if the system maps none of them.
fn bind(k: usize) -> Result<Option<Side>, RegionError> {
    let from_index = k.saturating_mul(2);
    let to_index = from_index.saturating_add(1);
    let control = match read_write(from_index) {
        Err(RegionError::NotMapped { .. }) => return Ok(None),
        other => other?,
    };
    Ok(Some(Side {
        from: ring_consumer(control, read_only(from_index)?)?,
        to: ring_producer(read_write(to_index)?, read_only(to_index)?)?,
        devices: None,
    }))
}

/// The 16-bit sample at byte `at` of `bytes`, little-endian; 0 past the end.
fn sample_at(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([
        bytes.get(at).copied().unwrap_or(0),
        bytes.get(at.saturating_add(1)).copied().unwrap_or(0),
    ])
}

impl AudioLoop {
    /// Takes every record waiting from side `k` - at most a ring's worth
    /// twice over, so a side that never stops cannot hold this PD.
    fn drain(&mut self, k: usize) {
        let mut devices = None;
        for _ in 0..FROM_SLOTS.saturating_mul(2) {
            let Some(side) = self.sides.get(k).and_then(Option::as_ref) else {
                return;
            };
            let Ok(mut rx) = LossyConsumer::new(&side.from) else {
                return;
            };
            let record = rx.try_recv_record();
            if self.microphone.is_some_and(|(side, _)| side == k) {
                self.lost = self.lost.saturating_add(rx.lost());
            }
            let Some(record) = record else {
                break;
            };
            match Header::parse(&record).map(|header| header.kind) {
                Ok(DEVICES_KIND) => {
                    if let Ok(found) = decode::<Devices>(&record) {
                        devices = Some(found);
                    }
                }
                Ok(CAPTURED_KIND) => {
                    if let Ok(captured) = decode::<Captured>(&record) {
                        self.captured(k, &captured);
                    }
                }
                _ => {}
            }
        }
        if let Some(devices) = devices {
            self.devices(k, devices);
        }
    }

    /// Side `k`'s devices, as its USB host PD last said.
    fn devices(&mut self, k: usize, devices: Devices) {
        let name = self.name;
        let Some(side) = self.sides.get_mut(k).and_then(Option::as_mut) else {
            return;
        };
        if side.devices == Some(devices) {
            return;
        }
        side.devices = Some(devices);
        match devices.speaker {
            Some(format) => debug_println!(
                "NOWAYOS: {name}: side {k}: a speaker, {} Hz, {} channels",
                format.rate(),
                format.channels()
            ),
            None => debug_println!("NOWAYOS: {name}: side {k}: no speaker"),
        }
        match devices.microphone {
            Some(format) => debug_println!(
                "NOWAYOS: {name}: side {k}: a microphone, {} Hz, {} channels",
                format.rate(),
                format.channels()
            ),
            None => debug_println!("NOWAYOS: {name}: side {k}: no microphone"),
        }
        // The first side with each, again.
        let speaker = self.sides.iter().enumerate().find_map(|(k, side)| {
            side.as_ref()
                .and_then(|side| side.devices)
                .and_then(|devices| devices.speaker)
                .map(|format| (k, format))
        });
        let microphone = self.sides.iter().enumerate().find_map(|(k, side)| {
            side.as_ref()
                .and_then(|side| side.devices)
                .and_then(|devices| devices.microphone)
                .map(|format| (k, format))
        });
        if speaker != self.speaker {
            self.speaker = speaker;
            self.tone =
                speaker.and_then(|(_, format)| Tone::new(TONE_HZ, format.rate(), TONE_AMPLITUDE));
        }
        if microphone != self.microphone {
            self.microphone = microphone;
            self.capturing = None;
            self.start_turn();
        }
    }

    /// Asks the microphone's side to capture while a recording is being
    /// made and to stop while it plays back: a capture record each
    /// time that changes.
    fn ask_capture(&mut self) {
        let Some((k, _)) = self.microphone else {
            return;
        };
        let on = matches!(self.phase, Phase::Listening(_));
        if self.capturing == Some(on) {
            return;
        }
        let Some(Some(side)) = self.sides.get(k) else {
            return;
        };
        let Ok(mut tx) = Producer::new(&side.to) else {
            return;
        };
        if tx.try_send(&Capture { on }).is_ok() {
            self.capturing = Some(on);
            debug_println!(
                "NOWAYOS: {}: side {k}: the microphone asked to {}",
                self.name,
                if on { "capture" } else { "stop" }
            );
        }
    }

    /// A new turn: recording from the start, the tone first.
    fn start_turn(&mut self) {
        self.phase = Phase::Listening(0);
        self.turn_frames = 0;
        self.peak = 0;
        self.lost = 0;
        self.gaps = 0;
    }

    /// The frames of the microphone the recording holds: its seconds, or
    /// as many as the region takes.
    fn record_frames(&self) -> usize {
        let Some((_, format)) = self.microphone else {
            return 0;
        };
        let room = self
            .recording
            .as_ref()
            .map_or(0, Frame::len)
            .checked_div(format.frame_bytes())
            .unwrap_or(0);
        usize::try_from(format.rate().saturating_mul(RECORD_SECONDS))
            .unwrap_or(0)
            .min(room)
    }

    /// The microphone's sound from side `k`: into the recording, while
    /// one is being made.
    fn captured(&mut self, k: usize, captured: &Captured) {
        let Some((side, format)) = self.microphone else {
            return;
        };
        let Phase::Listening(recorded) = self.phase else {
            return;
        };
        if side != k || captured.format() != format {
            return;
        }
        if captured.discontinuity() && recorded > 0 {
            self.gaps = self.gaps.saturating_add(1);
        }
        let total = self.record_frames();
        let frame_bytes = format.frame_bytes();
        let take = captured.frames().min(total.saturating_sub(recorded));
        let bytes = captured
            .bytes()
            .get(..take.saturating_mul(frame_bytes))
            .unwrap_or_default();
        for pair in bytes.chunks_exact(2) {
            let level = sample_at(pair, 0).unsigned_abs();
            self.peak = self.peak.max(level);
        }
        if let Some(recording) = self.recording.as_mut() {
            recording.store(recorded.saturating_mul(frame_bytes), bytes);
        }
        let recorded = recorded.saturating_add(take);
        self.phase = Phase::Listening(recorded);
        if recorded >= total && total > 0 {
            debug_println!(
                "NOWAYOS: {}: recorded {RECORD_SECONDS} s from the microphone ({} Hz, {} channels): its loudest sample {} of 32767, {} records lost, {} gaps; playing it back",
                self.name,
                format.rate(),
                format.channels(),
                self.peak,
                self.lost,
                self.gaps
            );
            self.phase = Phase::Playing {
                frames: recorded,
                at: 0,
            };
        }
    }

    /// The speaker's next frame, in its format, into `out`.
    fn next_frame(&mut self, speaker: Format, out: &mut [i16; 2]) {
        let mut sample = 0i16;
        match self.phase {
            Phase::Listening(_) => {
                let tone_frames = u64::from(speaker.rate()).saturating_mul(u64::from(TONE_SECONDS));
                if self.microphone.is_none() || self.turn_frames < tone_frames {
                    sample = self.tone.as_mut().map_or(0, Tone::sample);
                }
                *out = [sample, sample];
            }
            Phase::Playing { frames, at } => {
                *out = self.recorded_frame(frames, at, speaker);
                // The recording's rate over the speaker's, in 65536ths.
                let rate = self.microphone.map_or(0, |(_, format)| format.rate());
                let step = (u64::from(rate) << 16)
                    .checked_div(u64::from(speaker.rate()))
                    .unwrap_or(1 << 16);
                let at = at.saturating_add(step);
                if usize::try_from(at >> 16).unwrap_or(usize::MAX) >= frames {
                    debug_println!("NOWAYOS: {}: played it back", self.name);
                    self.start_turn();
                } else {
                    self.phase = Phase::Playing { frames, at };
                }
            }
        }
        self.turn_frames = self.turn_frames.saturating_add(1);
    }

    /// The recording at `at` - 65536ths of a frame of `frames` - between
    /// the frames either side, in the speaker's channels.
    fn recorded_frame(&self, frames: usize, at: u64, speaker: Format) -> [i16; 2] {
        let (Some((_, format)), Some(recording)) = (self.microphone, self.recording.as_ref())
        else {
            return [0, 0];
        };
        let frame_bytes = format.frame_bytes();
        let read = |index: usize| -> [i16; 2] {
            let mut bytes = [0u8; 4];
            let room = bytes.get_mut(..frame_bytes).unwrap_or_default();
            if index >= frames || !recording.load(index.saturating_mul(frame_bytes), room) {
                return [0, 0];
            }
            let left = sample_at(&bytes, 0);
            let right = if format.channels() > 1 {
                sample_at(&bytes, 2)
            } else {
                left
            };
            [left, right]
        };
        let index = usize::try_from(at >> 16).unwrap_or(usize::MAX);
        let fraction = i32::try_from(at & 0xFFFF).unwrap_or(0);
        let (a, b) = (read(index), read(index.saturating_add(1)));
        let between = |a: i16, b: i16| -> i16 {
            let (a, b) = (i32::from(a), i32::from(b));
            let value = a.saturating_add(b.saturating_sub(a).saturating_mul(fraction) >> 16);
            i16::try_from(value).unwrap_or(0)
        };
        let [left, right] = [between(a[0], b[0]), between(a[1], b[1])];
        if speaker.channels() > 1 {
            [left, right]
        } else {
            let mono = (i32::from(left).saturating_add(i32::from(right))) >> 1;
            [i16::try_from(mono).unwrap_or(0), 0]
        }
    }

    /// Keeps [`LEAD`] records queued for the speaker.
    fn feed_speaker(&mut self) {
        let Some((k, format)) = self.speaker else {
            return;
        };
        let channels = usize::from(format.channels());
        let per_record = MAX_PCM.checked_div(format.frame_bytes()).unwrap_or(0);
        for _ in 0..LEAD {
            let Some(side) = self.sides.get(k).and_then(Option::as_ref) else {
                return;
            };
            let Ok(tx) = Producer::new(&side.to) else {
                return;
            };
            let in_flight = u32::try_from(TO_SLOTS)
                .unwrap_or(0)
                .saturating_sub(tx.free_slots().unwrap_or(0));
            if in_flight >= LEAD {
                return;
            }
            let mut samples = [0i16; MAX_PCM / 2];
            let mut filled = 0usize;
            for _ in 0..per_record {
                let mut frame = [0i16; 2];
                self.next_frame(format, &mut frame);
                for sample in frame.iter().take(channels) {
                    if let Some(slot) = samples.get_mut(filled) {
                        *slot = *sample;
                        filled = filled.saturating_add(1);
                    }
                }
            }
            let position = u32::try_from(self.sent).unwrap_or(0);
            let Some(record) = Play::from_samples(
                format,
                position,
                self.sent == 0,
                samples.get(..filled).unwrap_or_default(),
            ) else {
                return;
            };
            let Some(side) = self.sides.get(k).and_then(Option::as_ref) else {
                return;
            };
            let Ok(mut tx) = Producer::new(&side.to) else {
                return;
            };
            if tx.try_send(&record).is_err() {
                return;
            }
            self.sent = self
                .sent
                .saturating_add(u64::try_from(per_record).unwrap_or(0));
            let seconds = self.sent.checked_div(u64::from(format.rate())).unwrap_or(0);
            if seconds
                >= self
                    .reports
                    .saturating_add(1)
                    .saturating_mul(REPORT_SECONDS)
            {
                self.reports = self.reports.saturating_add(1);
                debug_println!("NOWAYOS: {}: {seconds} s sent to the speaker", self.name);
            }
        }
    }
}

impl Handler for AudioLoop {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        for k in 0..SIDES {
            if channels.contains(Channel::new(k)) {
                self.drain(k);
            }
        }
        self.ask_capture();
        self.feed_speaker();
        Ok(())
    }
}
