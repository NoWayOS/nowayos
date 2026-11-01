// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: what a USB camera sends, as the USB host PD reads it
//! (`nowayos_usb::video`) - its configuration, its answer to the
//! probe and its payload transfers, each copied out of DMA memory - and the
//! frame ring's records and counts a client or a host could write.
//!
//! A camera is not trusted, so the configuration is either the fuzzer's
//! bytes as they are, or an invented webcam's - laid out as `nowayos-usb`'s
//! tests lay it out - with some bytes changed and cut to any length, so the
//! walk's checks are reached from the first inputs. Checks:
//!
//! * the camera walk never panics, and keeps its bounds: at most 16 frame
//!   sizes of at least 1 x 1, each interval above 0, at most 8 alternate
//!   settings other than 0 of 1 to 3,072 bytes an interval, an IN endpoint;
//!   a choice it makes is one of those sizes, at an interval above 0;
//! * an answer to the probe never panics, and one taken has an interval and
//!   a frame size above 0;
//! * payload transfers - the fuzzer's bytes cut into intervals, some
//!   reported missed - never panic the assembler, and every frame it gives
//!   the client fits its slot, starts with SOI and ends with EOI, each
//!   counted;
//! * the ring's checks: a layout taken fits its region, a record taken
//!   names its frame and fits its slot, a count taken never puts more
//!   frames in flight than slots, a client's frames-wanted word is taken
//!   only as 0 or 1, and the host's offer only as a whole size and
//!   interval;
//! * an enumeration that looks for a camera, fed any sequence of outcomes -
//!   the webcam's configuration and the fuzzer's bytes among them - never
//!   panics, polls only with a slot and something to use, keeps a commit
//!   only for a video that streams, and once stopped stays stopped.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_usb::enumerate::{Enumeration, Outcome, Step, Video};
use nowayos_usb::port::Speed;
use nowayos_usb::trb::Completion;
use nowayos_usb::video::frames::{Assembler, EOI, FrameSink, SOI};
use nowayos_usb::video::ring::{self, Layout, RECORD_LEN, Record};
use nowayos_usb::video::{camera, probe};

/// A high-speed device descriptor of no class: its class is in its
/// interfaces.
const DEVICE: [u8; 18] = [
    18, 1, 0x00, 0x02, 0xEF, 2, 1, 64, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01, 1, 2, 0, 1,
];

/// An invented webcam: a VideoControl interface, a VideoStreaming interface
/// with a Motion JPEG format of two frame sizes, discrete and continuous
/// rates, and two alternate settings of IN endpoint 0x81.
fn webcam() -> Vec<u8> {
    let mut c = vec![9, 2, 0, 0, 2, 1, 0, 0x80, 250];
    c.extend_from_slice(&[9, 4, 0, 0, 1, 0x0E, 0x01, 0, 0]);
    c.extend_from_slice(&[13, 0x24, 0x01, 0x50, 0x01, 13, 0, 0, 0, 0, 0, 1, 1]);
    c.extend_from_slice(&[9, 4, 1, 0, 0, 0x0E, 0x02, 0, 0]);
    c.extend_from_slice(&[14, 0x24, 0x01, 1, 0, 0, 0x81, 0, 3, 0, 0, 0, 1, 0]);
    c.extend_from_slice(&[11, 0x24, 0x06, 1, 2, 1, 1, 0, 0, 0, 0]);
    // 1280x720 at 30 and 15 a second.
    c.extend_from_slice(&[34, 0x24, 0x07, 1, 0, 0x00, 0x05, 0xD0, 0x02]);
    c.extend_from_slice(&[0; 12]);
    c.extend_from_slice(&333_333u32.to_le_bytes());
    c.push(2);
    c.extend_from_slice(&333_333u32.to_le_bytes());
    c.extend_from_slice(&666_666u32.to_le_bytes());
    // 640x480 from 5 to 30 a second.
    c.extend_from_slice(&[38, 0x24, 0x07, 2, 0, 0x80, 0x02, 0xE0, 0x01]);
    c.extend_from_slice(&[0; 12]);
    c.extend_from_slice(&333_333u32.to_le_bytes());
    c.push(0);
    for value in [333_333u32, 2_000_000, 1] {
        c.extend_from_slice(&value.to_le_bytes());
    }
    for (alternate, packet) in [(1u8, 0x0200u16), (2, 0x1400)] {
        c.extend_from_slice(&[9, 4, 1, alternate, 1, 0x0E, 0x02, 0, 0]);
        let [low, high] = packet.to_le_bytes();
        c.extend_from_slice(&[7, 5, 0x81, 0x05, low, high, 1]);
    }
    let total = u16::try_from(c.len()).unwrap_or(0).to_le_bytes();
    if let Some(slot) = c.get_mut(2..4) {
        slot.copy_from_slice(&total);
    }
    c
}

/// A client's ring in memory: one slot of `room` bytes.
struct Sink {
    slot: Vec<u8>,
    published: usize,
    full: bool,
}

impl FrameSink for Sink {
    fn begin(&mut self) -> Option<usize> {
        (!self.full).then_some(self.slot.len())
    }

    fn write(&mut self, at: usize, bytes: &[u8]) -> bool {
        match at
            .checked_add(bytes.len())
            .and_then(|end| self.slot.get_mut(at..end))
        {
            Some(target) => {
                target.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }

    fn publish(&mut self, len: usize) {
        let frame = self.slot.get(..len).unwrap_or_default();
        assert!(len >= 4 && len <= self.slot.len(), "a frame past its slot");
        assert!(
            frame.starts_with(&SOI) && frame.ends_with(&EOI),
            "not a whole JPEG"
        );
        self.published = self.published.saturating_add(1);
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let base = webcam();
    let mut changed = base.clone();
    let config: &[u8] = match input.byte() % 3 {
        0 => data,
        _ => {
            for _ in 0..(input.byte() % 8) {
                let at = usize::from(input.u16())
                    .checked_rem(changed.len())
                    .unwrap_or(0);
                if let Some(byte) = changed.get_mut(at) {
                    *byte = input.byte();
                }
            }
            let cut = usize::from(input.u16())
                .checked_rem(changed.len().saturating_add(1))
                .unwrap_or(0);
            changed.get(..cut).unwrap_or_default()
        }
    };

    // The camera walk and its choice.
    let mut choice = None;
    if let Ok(Some(found)) = camera::camera(config) {
        assert!(found.endpoint & 0x80 != 0 && found.endpoint & 0x0F != 0);
        assert!(found.frames().count() <= camera::MAX_FRAMES);
        assert!(found.alternates().count() <= camera::MAX_ALTERNATES);
        for frame in found.frames() {
            assert!(frame.width != 0 && frame.height != 0);
            assert!(frame.rates.listed().iter().all(|interval| *interval != 0));
        }
        for alternate in found.alternates() {
            assert!(alternate.alternate != 0);
            assert!((1..=3072).contains(&alternate.bandwidth()));
        }
        if let Ok(chosen) = found.choose() {
            assert!(chosen.interval != 0);
            assert!(found.frames().any(|frame| frame.index == chosen.frame
                && (frame.width, frame.height) == (chosen.width, chosen.height)));
            choice = Some(chosen);
        }
    }

    // The probe's answer.
    let answer: [u8; probe::PROBE_MAX] = input.array();
    let cut = usize::from(input.byte()) % (probe::PROBE_MAX + 1);
    if let Some(chosen) = choice
        && let Ok(found) = probe::answer(answer.get(..cut).unwrap_or_default(), &chosen)
    {
        assert!(found.interval != 0 && found.max_frame != 0);
    }

    // The ring's records and counts.
    let layout = Layout::check(
        if input.byte() & 1 == 0 {
            ring::VERSION
        } else {
            input.u32()
        },
        input.u32() % 20,
        input.u32(),
        usize::from(input.u16()).saturating_mul(256),
    );
    let record: [u8; RECORD_LEN] = input.array();
    let (sequence, before, written, found) = (input.u32(), input.u32(), input.u32(), input.u32());
    if let Ok(layout) = layout {
        let total = (layout.slots() as usize).saturating_mul(layout.slot_bytes() as usize);
        assert!(
            layout
                .slot_at(sequence)
                .saturating_add(layout.slot_bytes() as usize)
                <= total
        );
        if let Ok(taken) = Record::decode(&record, sequence, &layout) {
            assert!(taken.sequence == sequence && taken.len as usize <= layout.room());
        }
        if let Ok(ready) = ring::ready(&layout, before, found) {
            assert!(ready <= layout.slots());
        }
        if written.wrapping_sub(before) <= layout.slots()
            && let Ok(taken) = ring::released(written, before, found)
        {
            assert!(written.wrapping_sub(taken) <= written.wrapping_sub(before));
        }
    }
    if let Ok(on) = ring::wanted(found) {
        assert!(u32::from(on) == found);
    }
    if let Some(offer) = ring::offer(written, found) {
        assert!(offer.width > 0 && offer.height > 0 && offer.interval > 0);
        assert!(offer.size_word() == written);
    }

    // An enumeration that looks for a camera.
    let mut machine_steps = 0u8;
    let (mut machine, _) = Enumeration::start_with_camera(1, None);
    let mut stopped = false;
    while machine_steps < 24 && !input.is_empty() {
        machine_steps = machine_steps.saturating_add(1);
        let value = input.byte();
        let outcome = match input.byte() % 8 {
            0 => Outcome::PortEnabled(Speed::High),
            1 => Outcome::SlotEnabled(value),
            2 => Outcome::CommandDone,
            3 => Outcome::ControlData(&DEVICE),
            4 => Outcome::ControlData(&base),
            5 => Outcome::ControlData(config),
            6 => Outcome::ControlDone,
            _ => Outcome::ControlFailed(Completion(value)),
        };
        let step = machine.on(outcome);
        assert!(
            machine.video_commit().is_none() || matches!(machine.video(), Video::Streaming(..))
        );
        if stopped {
            assert!(matches!(step, Step::Stop(_)), "{step:?} after a stop");
        }
        match step {
            Step::Stop(_) => stopped = true,
            Step::Poll => {
                assert!(machine.polling() && machine.slot() != 0);
                assert!(
                    !machine.boot().is_empty()
                        || machine.selected().count() > 0
                        || matches!(machine.video(), Video::Streaming(..))
                );
            }
            _ => {}
        }
    }

    // Payload transfers: the rest of the bytes, a length byte before each,
    // some intervals reported missed.
    let room = usize::from(input.u16()) % 4096;
    let mut sink = Sink {
        slot: vec![0; room],
        published: 0,
        full: false,
    };
    let mut assembler = Assembler::new();
    let rest = input.rest();
    let mut at = 0usize;
    while let Some(&head) = rest.get(at) {
        let len = usize::from(head & 0x7F);
        if head & 0x80 != 0 && len < 4 {
            assembler.lost();
        }
        sink.full = len == 5;
        let start = at.saturating_add(1);
        let end = start.saturating_add(len).min(rest.len());
        assembler.payload(rest.get(start..end).unwrap_or_default(), &mut sink);
        at = end.max(start);
    }
    assert_eq!(assembler.counts().frames, sink.published as u64);
});
