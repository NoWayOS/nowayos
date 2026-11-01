// SPDX-License-Identifier: MIT OR Apache-2.0
//! The USB host PD's audio client: the two rings to one client PD,
//! the microphone's sound out and the speaker's in, and the channel the PD
//! notifies it on (`nowayos_audio`'s module documentation has the channel).
//!
//! **The PD never waits for its client.** What the microphone gives goes
//! onto a lossy ring, so a client that falls behind loses the oldest
//! sound; the speaker takes what the client has sent, and plays silence
//! when it has sent nothing, counted. A record for the speaker in any other
//! format than the speaker's, or one that does not decode, is dropped and
//! counted. Which devices stream, and in which formats, the client learns
//! from a [`Devices`] record, sent when either changes and every second.
//!
//! **The microphone captures only while the client asks**: a
//! [`Capture`] record on the ring to the PD starts or stops it, and until
//! the first the PD captures nothing ([`Client::capture`]). The ring is read
//! for it every tick, a speaker or none.

use nowayos_audio::{
    Assembler, CAPTURE_KIND, CAPTURED_KIND, Capture, Captured, Devices, FROM_SLOTS, Format,
    LossyProducer, PLAY_KIND, Play, SLOT_LEN, TO_SLOTS,
};
use nowayos_ipc::Consumer;
use nowayos_region::{
    RegionError, RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer,
};
use sel4_microkit::Channel;

/// The channel to the client: the PD notifies it on every tick it sent a
/// record or took one.
const CLIENT: Channel = Channel::new(2);

/// The regions of the ring from the PD - its messages read-write, its
/// control read-only - and of the ring to it - its control read-write, its
/// messages read-only.
const FROM_REGION: usize = 1;
const TO_REGION: usize = 2;

/// How many ticks apart the devices record is sent again: a second of 8 ms
/// ticks.
const DEVICES_EVERY: u32 = 125;

/// How many records one frame's look at the ring to the PD may pass - the
/// ones it drops - before it plays silence.
const LOOKS: usize = 4;

/// The rings to one client PD, and what streams through them.
pub struct Client {
    from: RingTx<FROM_SLOTS, SLOT_LEN>,
    to: RingRx<TO_SLOTS, SLOT_LEN>,
    generation: u32,
    /// The speaker's stream, by its number in `nowayos_hw::xhci`, and its
    /// format.
    speaker: Option<(usize, Format)>,
    /// The microphone's.
    microphone: Option<(usize, Format)>,
    assembler: Assembler<CAPTURED_KIND>,
    /// The record the speaker plays from, and its frames played.
    playing: Option<(Play, usize)>,
    /// Whether the client has sent the speaker anything since it started:
    /// silence before that is not the client's.
    fed: bool,
    /// Frames of silence the speaker played because the client had sent
    /// none, since it started.
    silent: u64,
    /// Records for the speaker dropped: in another format, or not
    /// decoding.
    dropped: u32,
    /// Whether a record went or came this tick.
    active: bool,
    ticks: u32,
    /// Whether the client last asked the microphone to capture.
    capture: bool,
}

/// Binds the client's rings: `Ok(None)` if the system maps neither.
///
/// # Errors
///
/// Any [`RegionError`] binding them, but for the first not mapped at all.
pub fn bind() -> Result<Option<Client>, RegionError> {
    let messages = match read_write(FROM_REGION) {
        Err(RegionError::NotMapped { .. }) => return Ok(None),
        other => other?,
    };
    let from = ring_producer(messages, read_only(FROM_REGION)?)?;
    let to = ring_consumer(read_write(TO_REGION)?, read_only(TO_REGION)?)?;
    Ok(Some(Client {
        from,
        to,
        generation: 0,
        speaker: None,
        microphone: None,
        assembler: Assembler::new(),
        playing: None,
        fed: false,
        silent: 0,
        dropped: 0,
        active: false,
        ticks: 0,
        capture: false,
    }))
}

impl Client {
    /// Takes stream `stream` as the client's speaker, in `format`; `false`
    /// if it has one already.
    pub fn take_speaker(&mut self, stream: usize, format: Format) -> bool {
        if self.speaker.is_some() {
            return false;
        }
        self.speaker = Some((stream, format));
        self.playing = None;
        self.fed = false;
        self.silent = 0;
        self.changed();
        true
    }

    /// Takes stream `stream` as the client's microphone, in `format`;
    /// `false` if it has one already.
    pub fn take_microphone(&mut self, stream: usize, format: Format) -> bool {
        if self.microphone.is_some() {
            return false;
        }
        self.microphone = Some((stream, format));
        self.assembler.start(format);
        self.changed();
        true
    }

    /// Stream `stream` stopped: if it was the speaker or the microphone,
    /// the client has it no more.
    pub fn release(&mut self, stream: usize) {
        if self.speaker.is_some_and(|(id, _)| id == stream) {
            self.speaker = None;
            self.playing = None;
            self.changed();
        }
        if self.microphone.is_some_and(|(id, _)| id == stream) {
            self.microphone = None;
            self.assembler.stop();
            self.changed();
        }
    }

    /// Whether stream `stream` is the client's microphone.
    pub fn is_microphone(&self, stream: usize) -> bool {
        self.microphone.is_some_and(|(id, _)| id == stream)
    }

    /// Whether the client last asked the microphone to capture:
    /// `false` until it first asks.
    pub const fn capture(&self) -> bool {
        self.capture
    }

    /// The next speaker's record the client sent, taking any capture
    /// record before it; a record of another kind, or one that
    /// does not decode, is dropped and counted.
    fn next_record(&mut self) -> Option<Play> {
        for _ in 0..TO_SLOTS {
            let Ok(mut rx) = Consumer::new(&self.to) else {
                return None;
            };
            let Ok(Some(record)) = rx.try_recv_record() else {
                return None;
            };
            self.active = true;
            match record.header().map(|header| header.kind) {
                Ok(PLAY_KIND) => match record.decode::<Play>() {
                    Ok(play) => return Some(play),
                    Err(_) => self.dropped = self.dropped.saturating_add(1),
                },
                Ok(CAPTURE_KIND) => match record.decode::<Capture>() {
                    Ok(capture) => self.capture = capture.on,
                    Err(_) => self.dropped = self.dropped.saturating_add(1),
                },
                _ => self.dropped = self.dropped.saturating_add(1),
            }
        }
        None
    }

    /// The speaker's silence and its records dropped, so far.
    pub const fn speaker_counts(&self) -> (u64, u32) {
        (self.silent, self.dropped)
    }

    /// A device came or went: the client is told at the end of the tick.
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.ticks = DEVICES_EVERY;
    }

    /// The speaker's next frame from the client into `out`, a sample a
    /// channel; `false`, leaving `out` alone, when the client has sent
    /// none - the speaker plays silence.
    pub fn next_frame(&mut self, out: &mut [i16]) -> bool {
        let Some((_, format)) = self.speaker else {
            return false;
        };
        let frame_bytes = format.frame_bytes();
        for _ in 0..LOOKS {
            if let Some((record, played)) = self.playing.as_mut() {
                let at = played.saturating_mul(frame_bytes);
                if let Some(frame) = record.bytes().get(at..at.saturating_add(frame_bytes)) {
                    for (sample, pair) in out.iter_mut().zip(frame.chunks_exact(2)) {
                        *sample = i16::from_le_bytes([
                            pair.first().copied().unwrap_or(0),
                            pair.get(1).copied().unwrap_or(0),
                        ]);
                    }
                    *played = played.saturating_add(1);
                    return true;
                }
                self.playing = None;
            }
            match self.next_record() {
                Some(record) if record.format() == format => {
                    self.playing = Some((record, 0));
                    self.fed = true;
                }
                Some(_) => self.dropped = self.dropped.saturating_add(1),
                None => break,
            }
        }
        if self.fed {
            self.silent = self.silent.saturating_add(1);
        }
        false
    }

    /// One frame from the microphone, a sample a channel: records go to
    /// the client as they fill.
    pub fn captured(&mut self, frame: &[i16]) {
        let (from, assembler) = (&self.from, &mut self.assembler);
        let mut sent = false;
        assembler.push_frame(frame, |record| sent |= send(from, record));
        self.active |= sent;
    }

    /// Sound from the microphone was lost: the next record says so.
    pub const fn lost(&mut self) {
        self.assembler.lost();
    }

    /// The tick's end: what the microphone gave goes out, the devices
    /// record when it is due, and the client is notified if anything went
    /// or came.
    pub fn end_tick(&mut self) {
        // With no speaker to take them, the ring is read here: a capture
        // record is taken, and a speaker's record has nowhere to go.
        if self.speaker.is_none() {
            for _ in 0..TO_SLOTS {
                if self.next_record().is_none() {
                    break;
                }
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        let (from, assembler) = (&self.from, &mut self.assembler);
        let mut sent = false;
        assembler.flush(|record| sent |= send(from, record));
        self.active |= sent;
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks >= DEVICES_EVERY {
            self.ticks = 0;
            let devices = Devices {
                generation: self.generation,
                speaker: self.speaker.map(|(_, format)| format),
                microphone: self.microphone.map(|(_, format)| format),
            };
            if let Ok(mut tx) = LossyProducer::new(&self.from)
                && tx.send(&devices).is_ok()
            {
                self.active = true;
            }
        }
        if self.active {
            CLIENT.notify();
            self.active = false;
        }
    }
}

/// Sends `record` on the lossy ring `from`: whether it went.
fn send(from: &RingTx<FROM_SLOTS, SLOT_LEN>, record: &Captured) -> bool {
    LossyProducer::new(from).is_ok_and(|mut tx| tx.send(record).is_ok())
}
