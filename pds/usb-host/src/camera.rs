// SPDX-License-Identifier: MIT OR Apache-2.0
//! A camera's video: its isochronous stream's payloads assembled
//! into whole Motion JPEG frames (`nowayos_usb::video::frames`) and put
//! into the ring of frame slots a client reads
//! (`nowayos_usb::video::ring`), never decoded.
//!
//! The ring's three regions, as the system gives them ([`Regions`]): the
//! frames region and this PD's page, read-write, which the client maps
//! read-only, and the client's page, read-only, which only the client
//! writes - read-write regions 1 and 2 and read-only region 1 for a camera
//! alone ([`ALONE`]), 3, 4 and 3 beside an audio client, for a call
//! ([`BESIDE_AUDIO`]). Each whole frame goes into a slot no frame
//! unread holds, behind its record, and the client is notified (channel
//! 2, the audio client's beside audio, as it is the same client); a frame
//! that starts with no slot free is
//! dropped, never queued, as is a damaged one. Every [`STREAM_REPORT_MS`] while it streams, the
//! camera's line says how many frames went, their sizes, the drops and
//! why, and the busiest tick.
//!
//! **It streams only while its client wants frames**: the client's
//! word [`WANTED`], checked each tick ([`ring::wanted`]), drives the camera's
//! capture gate (`nowayos_usb::capture`) - its interface at alternate setting
//! 0 while no frames are wanted, its commit made again before each start.
//! Its frame size and interval are offered on the host's page from when it
//! is set up until it goes, so the client knows them before it wants any.

use nowayos_display::FrameMemory;
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_region::{Frame, ReadOnly, ReadWrite, RegionError, Words, read_only, read_write};
use nowayos_usb::capture::Gate;
use nowayos_usb::context::{interval_micros, isoch_interval};
use nowayos_usb::enumerate::{Enumeration, Video};
use nowayos_usb::trb::{Completion, ISOCH_MAX};
use nowayos_usb::video::Rate;
use nowayos_usb::video::frames::{Assembler, FrameSink};
use nowayos_usb::video::ring::{
    self, Layout, Offer, RECORD_LEN, RELEASED, Record, VERSION, WANTED, word,
};
use sel4_microkit::{Channel, debug_println};

use crate::{Gated, Gating, Playing, STREAM_AHEAD_US, STREAM_REPORT_MS, Source, UsbHost, due};

/// Where a system puts the ring of frames: its regions, and the channel
/// its client is notified on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Regions {
    /// The frames region, read-write.
    frames: usize,
    /// This PD's page, read-write.
    mine: usize,
    /// The client's page, read-only.
    theirs: usize,
    /// The channel to the client.
    client: Channel,
}

/// A camera alone: the regions and channel an audio client would
/// otherwise have.
pub(crate) const ALONE: Regions = Regions {
    frames: 1,
    mine: 2,
    theirs: 1,
    client: Channel::new(2),
};

/// Beside an audio client, for a call, whose rings take
/// read-write and read-only regions 1 and 2: the same client, notified on
/// its one channel, 2 (a system has one channel between two PDs).
pub(crate) const BESIDE_AUDIO: Regions = Regions {
    frames: 3,
    mine: 4,
    theirs: 3,
    client: Channel::new(2),
};

/// The slots of the ring of frames.
pub const FRAME_SLOTS: u32 = 4;

/// The ring of frame slots to the client, and the frame being written.
pub(crate) struct FrameRing {
    frames: Frame<ReadWrite>,
    mine: Words<ReadWrite>,
    theirs: Words<ReadOnly>,
    layout: Layout,
    /// The channel to the client.
    client: Channel,
    /// Frames written: the next frame's number.
    written: u32,
    /// The client's count released, as last taken.
    released: u32,
    /// The client's words refused: a count moving back or past what was
    /// written, a frames-wanted word neither 0 nor 1.
    refused: u32,
    /// When a word refused was last said, on the counter.
    warned: Option<u64>,
    /// The frames' size and interval, for each record.
    width: u16,
    height: u16,
    interval: u32,
}

/// Binds the ring of frames' regions where `at` says and lays out its
/// slots; `None`, said why, if the system gives none.
pub(crate) fn frame_ring(name: &str, at: Regions) -> Option<FrameRing> {
    let bound = (|| -> Result<_, RegionError> {
        Ok((
            read_write(at.frames)?.into_frame(0)?,
            read_write(at.mine)?.into_words()?,
            read_only(at.theirs)?.into_words()?,
        ))
    })();
    let (frames, mine, theirs) = match bound {
        Ok(bound) => bound,
        Err(error) => {
            debug_println!("NOWAYOS: {name}: no ring of frames for a camera's client: {error}");
            return None;
        }
    };
    let layout = match Layout::new(FRAME_SLOTS, frames.len()) {
        Ok(layout) => layout,
        Err(error) => {
            debug_println!("NOWAYOS: {name}: the ring of frames: {error}");
            return None;
        }
    };
    for (index, value) in [
        (word::SLOTS, layout.slots()),
        (word::SLOT_BYTES, layout.slot_bytes()),
        (word::WRITTEN, 0),
        (word::BEHIND, 0),
        (word::DAMAGED, 0),
        (word::VERSION, VERSION),
    ] {
        mine.store(index, value);
    }
    debug_println!(
        "NOWAYOS: {name}: a ring of {} frame slots of {} bytes for a camera's client",
        layout.slots(),
        layout.slot_bytes()
    );
    Some(FrameRing {
        frames,
        mine,
        theirs,
        layout,
        client: at.client,
        written: 0,
        released: 0,
        refused: 0,
        warned: None,
        width: 0,
        height: 0,
        interval: 0,
    })
}

impl FrameRing {
    /// The camera's offer on the host's page: the interval, then
    /// the size, so a client that sees the size sees its interval.
    fn offer(&self, offer: Offer) {
        self.mine.store(word::INTERVAL, offer.interval);
        self.mine.store(word::SIZE, offer.size_word());
    }

    /// The camera gone: its offer withdrawn, the size first.
    pub(crate) fn withdraw(&self) {
        self.mine.store(word::SIZE, 0);
        self.mine.store(word::INTERVAL, 0);
    }
}

impl FrameSink for FrameRing {
    fn begin(&mut self) -> Option<usize> {
        // The client's count, checked: one that moves back or past what was
        // written is refused, and nothing taken as released.
        if let Some(found) = self.theirs.load(RELEASED) {
            match ring::released(self.written, self.released, found) {
                Ok(taken) => self.released = taken,
                Err(_) => self.refused = self.refused.saturating_add(1),
            }
        }
        (ring::free(&self.layout, self.written, self.released) > 0).then(|| self.layout.room())
    }

    fn write(&mut self, at: usize, bytes: &[u8]) -> bool {
        let fits = at
            .checked_add(bytes.len())
            .is_some_and(|end| end <= self.layout.room());
        let offset = self
            .layout
            .slot_at(self.written)
            .checked_add(RECORD_LEN)
            .and_then(|start| start.checked_add(at));
        match offset {
            Some(offset) if fits => self.frames.store(offset, bytes),
            _ => false,
        }
    }

    fn publish(&mut self, len: usize) {
        let record = Record {
            sequence: self.written,
            len: u32::try_from(len).unwrap_or(u32::MAX),
            width: self.width,
            height: self.height,
            interval: self.interval,
        };
        // The record, then the count with release ordering: the client
        // sees both once it sees the count.
        if self
            .frames
            .store(self.layout.slot_at(self.written), &record.to_bytes())
        {
            self.written = self.written.wrapping_add(1);
            self.mine.store(word::WRITTEN, self.written);
            self.client.notify();
        }
    }
}

/// The camera's video, set up to stream.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CameraStream {
    /// Its stream and capture gate.
    pub(crate) gating: Gating,
    pub(crate) assembler: Assembler,
    /// The stream's intervals missed, when last looked at.
    missed: u64,
    started: u64,
    reports: u64,
    /// The longest tick since the last report, on the counter.
    busiest: u64,
}

impl UsbHost {
    /// Lists what the camera `machine` found offers, and sets its video up
    /// if enumeration did: its stream given the pool of buffers, fed as a
    /// microphone's is while its client wants frames, its payloads
    /// to the frames; or says why not.
    pub(crate) fn start_camera(&mut self, machine: &Enumeration) {
        let name = self.name;
        let (port, slot) = (machine.port(), machine.slot());
        self.list_camera(machine);
        let (choice, answer) = match machine.video() {
            Video::Streaming(choice, answer) => (choice, answer),
            Video::Refused(why) => {
                debug_println!("NOWAYOS: {name}: port {port}: camera: not streamed: {why}");
                return;
            }
            Video::None | Video::Chosen(_) | Video::Answered(..) => return,
        };
        let (Some(xhci), Some(frames), Some(speed)) =
            (self.xhci.as_mut(), self.frames.as_mut(), machine.speed())
        else {
            return;
        };
        let Some(commit) = machine.video_commit() else {
            debug_println!("NOWAYOS: {name}: port {port}: camera: not streamed: no commit kept");
            return;
        };
        let Some(stream) = xhci.stream_of(slot, choice.endpoint) else {
            debug_println!(
                "NOWAYOS: {name}: port {port}: camera: not streamed: no room to stream it"
            );
            return;
        };
        let capacity = match xhci.stream_pool(stream) {
            Ok(capacity) => capacity,
            Err(error) => {
                debug_println!("NOWAYOS: {name}: port {port}: camera: not streamed: {error}");
                return;
            }
        };
        let interval_us = interval_micros(isoch_interval(speed, choice.alternate.interval));
        let depth = usize::try_from(STREAM_AHEAD_US.checked_div(interval_us).unwrap_or(1))
            .unwrap_or(1)
            .clamp(2, capacity.max(2));
        frames.width = choice.width;
        frames.height = choice.height;
        frames.interval = answer.interval;
        frames.offer(Offer {
            width: choice.width,
            height: choice.height,
            interval: answer.interval,
        });
        debug_println!(
            "NOWAYOS: {name}: port {port}: camera: Motion JPEG {}x{} at {} a second, when its client wants frames: interface {}, alternate {}, up to {} bytes every {interval_us} us, {depth} transfers ahead; frames up to {} bytes, payloads up to {}, slots of {} bytes",
            choice.width,
            choice.height,
            Rate(answer.interval),
            choice.interface,
            choice.alternate.alternate,
            choice.alternate.bandwidth(),
            answer.max_frame,
            answer.max_payload,
            frames.layout.room()
        );
        if answer.max_payload > choice.alternate.bandwidth() {
            debug_println!(
                "NOWAYOS: {name}: port {port}: camera: its payloads of up to {} bytes exceed its largest setting's {}",
                answer.max_payload,
                choice.alternate.bandwidth()
            );
        }
        let now = counter();
        if let Some(entry) = self.playing.get_mut(stream) {
            *entry = Some(Playing {
                port,
                slot,
                endpoint: choice.endpoint,
                depth,
                source: Source::Counted,
                started: now,
                reports: 0,
            });
        }
        // First, back to alternate setting 0 until frames are wanted.
        let (gate, ask) = Gate::camera(&choice, commit);
        self.camera = Some(CameraStream {
            gating: Gating::new(stream, port, slot, gate),
            assembler: Assembler::new(),
            missed: 0,
            started: now,
            reports: 0,
            busiest: 0,
        });
        self.gate_ask(Gated::Camera, ask);
    }

    /// Whether the camera's client wants frames: its word [`WANTED`],
    /// checked - one neither 0 nor 1 is counted, said, and taken as a stop.
    pub(crate) fn frames_wanted(&mut self) -> bool {
        let name = self.name;
        let Some(frames) = self.frames.as_mut() else {
            return false;
        };
        match ring::wanted(frames.theirs.load(WANTED).unwrap_or(0)) {
            Ok(on) => on,
            Err(error) => {
                frames.refused = frames.refused.saturating_add(1);
                if due(&mut frames.warned) {
                    debug_println!(
                        "NOWAYOS: {name}: the camera's client: {error} ({} words refused so far)",
                        frames.refused
                    );
                }
                false
            }
        }
    }

    /// The camera's release, its Motion JPEG frame sizes and rates, and
    /// the alternate settings of its video endpoint, on serial.
    fn list_camera(&self, machine: &Enumeration) {
        let name = self.name;
        let port = machine.port();
        let Some(camera) = machine.camera() else {
            return;
        };
        debug_println!(
            "NOWAYOS: {name}: port {port}: camera: UVC {}.{}, interface {}, video endpoint {:#04x}{}",
            camera.uvc >> 8,
            (camera.uvc >> 4) & 0xF,
            camera.interface,
            camera.endpoint,
            if camera.bulk.is_some() { " (bulk)" } else { "" }
        );
        for frame in camera.frames() {
            debug_println!(
                "NOWAYOS: {name}: port {port}: camera: Motion JPEG {}x{} at {}",
                frame.width,
                frame.height,
                frame.rates
            );
        }
        if camera.frames_left_out > 0 {
            debug_println!(
                "NOWAYOS: {name}: port {port}: camera: {} more Motion JPEG frame sizes not kept",
                camera.frames_left_out
            );
        }
        for alternate in camera.alternates() {
            debug_println!(
                "NOWAYOS: {name}: port {port}: camera: alternate setting {}: {} x {} bytes",
                alternate.alternate,
                alternate.transactions,
                alternate.max_packet
            );
        }
    }

    /// A TD of stream `stream` ended - `done` its buffer, bytes and code -
    /// or its ring ran empty (`None`). For the camera's stream: an interval
    /// missed since the last damages the frame arriving, and a TD served
    /// is copied out of DMA memory and its payload assembled.
    pub(crate) fn camera_event(&mut self, stream: usize, done: Option<(usize, usize, Completion)>) {
        let (Some(camera), Some(xhci), Some(frames)) = (
            self.camera.as_mut(),
            self.xhci.as_ref(),
            self.frames.as_mut(),
        ) else {
            return;
        };
        if camera.gating.stream != stream {
            return;
        }
        let missed = xhci
            .stream_stats(stream)
            .map_or(camera.missed, |stats| stats.missed);
        if missed != camera.missed {
            camera.missed = missed;
            camera.assembler.lost();
        }
        let Some((buffer, len, code)) = done else {
            return;
        };
        if !code.ok() {
            return;
        }
        let mut payload = [0u8; ISOCH_MAX as usize];
        match xhci.stream_data(stream, buffer, len, &mut payload) {
            Ok(read) => camera
                .assembler
                .payload(payload.get(..read).unwrap_or_default(), frames),
            Err(_) => camera.assembler.lost(),
        }
    }

    /// The tick that began at `began` (on the counter) is over: every
    /// [`STREAM_REPORT_MS`], the camera's frames, sizes and drops, and the
    /// busiest tick since the last report.
    pub(crate) fn report_camera(&mut self, began: u64) {
        let name = self.name;
        let now = counter();
        let (Some(camera), Some(frames)) = (self.camera.as_mut(), self.frames.as_mut()) else {
            return;
        };
        camera.busiest = camera.busiest.max(now.wrapping_sub(began));
        let frequency = counter_frequency();
        let period = frequency.saturating_mul(STREAM_REPORT_MS) / 1000;
        let due_at = camera
            .reports
            .saturating_add(1)
            .saturating_mul(period)
            .saturating_add(camera.started);
        if period == 0 || now < due_at {
            return;
        }
        camera.reports = camera.reports.saturating_add(1);
        if !camera.gating.gate.feeding() {
            camera.busiest = 0;
            return;
        }
        let seconds = camera
            .reports
            .saturating_mul(STREAM_REPORT_MS)
            .checked_div(1000)
            .unwrap_or(0);
        let counts = camera.assembler.counts();
        let average = counts.bytes.checked_div(counts.frames).unwrap_or(0);
        let busiest_us = camera
            .busiest
            .saturating_mul(1_000_000)
            .checked_div(frequency)
            .unwrap_or(0);
        camera.busiest = 0;
        let port = camera.gating.port;
        frames.mine.store(
            word::BEHIND,
            u32::try_from(counts.behind).unwrap_or(u32::MAX),
        );
        frames.mine.store(
            word::DAMAGED,
            u32::try_from(counts.damaged.saturating_add(counts.too_large)).unwrap_or(u32::MAX),
        );
        debug_println!(
            "NOWAYOS: {name}: port {port}: camera: {seconds} s: {} frames, {} to {} bytes, {average} on average; dropped {} with the client behind, {} damaged, {} too large",
            counts.frames,
            counts.smallest,
            counts.largest,
            counts.behind,
            counts.damaged,
            counts.too_large
        );
        let last = camera
            .assembler
            .last_damage()
            .map_or("none", |damage| damage.name());
        debug_println!(
            "NOWAYOS: {name}: port {port}: camera: {seconds} s: {} intervals missed, {} bad payload headers, the last damage {last}; the busiest tick {busiest_us} us; {} of the client's words not taken",
            counts.lost,
            counts.bad_headers,
            frames.refused
        );
    }
}
