// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-camera-test`: a test instrument, the client of the USB
//! host PDs' rings of a camera's frames in `rpi5b_2gb/usb-camera-test`
//! (`nowayos_usb::video::ring`): one ring from each of the Pi 5's two
//! controllers' PDs, as the camera may be on either.
//!
//! Ring `n`'s frames region is its read-only region `2n`, the host's page
//! read-only region `2n + 1`, its own page read-write region `n`, and the
//! host notifies it on channel `n`. On each notification it checks the
//! host's layout and count (`ring::ready`), and for each new frame copies
//! the slot's record out and checks it (`Record::decode`), copies the
//! frame's first and last two bytes out and checks they are a JPEG's SOI
//! and EOI - nothing else of a frame is read - counts it and releases it.
//! At its first frame it checks the host's offer (`ring::offer`) is
//! that frame's size and interval. It says so for the first frame, and
//! every 300 frames how many, their
//! sizes and the host's drops. It wants frames from the start - word
//! `WANTED` of its page set to 1 - and stops wanting them once it has
//! dumped one, so the host's log shows its camera stopping.
//!
//! **One frame saved.** At its first frame [`DUMP_AFTER_S`] seconds after
//! its first, it prints that frame on serial, holding its slot: base64 in
//! lines of [`LINE`] bytes, each with its offset and CRC-32, then a line
//! with the whole frame's length and CRC-32, twice - so a line a PD
//! printing at the same time broke can be taken from the other copy.
//! `tools/serial-frame` saves it from the serial log as a JPEG on the host.
//! The host drops the frames that come meanwhile, as the client is behind.
//!
//! A ring's layout, a record or a count refused is said on a line starting
//! "camera-test: ring N:", and a frame that is not a whole JPEG with "not a
//! JPEG": the expect file forbids both. A
//! test instrument: never in a demo or product system. No `unsafe` code;
//! its regions are bound through `nowayos-region`, and it reads the counter
//! through `nowayos-hw`.

#![no_std]
#![no_main]

use core::fmt;

use nowayos_hw::cpu;
use nowayos_region::{Frame, ReadOnly, ReadWrite, RegionError, Words, read_only, read_write};
use nowayos_usb::video::Rate;
use nowayos_usb::video::frames::{EOI, SOI};
use nowayos_usb::video::ring::{self, Layout, RECORD_LEN, RELEASED, Record, WANTED, word};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// The rings: one from each controller's PD.
const RINGS: usize = 2;

/// Seconds after its first frame that it dumps one: past the ten minutes
/// counted, and the host's report at 600 s.
pub const DUMP_AFTER_S: u64 = 605;

/// The frames between two reports.
const REPORT_EVERY: u64 = 300;

/// The seconds of frames whose end it reports.
const RUN_S: u64 = 600;

/// The bytes of a frame in each line of its dump: 64 characters of base64.
pub const LINE: usize = 48;

/// The copies of the frame dumped.
const COPIES: u32 = 2;

/// One ring of frames, as this client sees it.
struct Ring {
    frames: Frame<ReadOnly>,
    host: Words<ReadOnly>,
    mine: Words<ReadWrite>,
    layout: Option<Layout>,
    released: u32,
    /// A record or count refused: the ring is no longer read.
    refused: bool,
}

/// What it has seen.
#[derive(Default)]
struct Counts {
    frames: u64,
    bytes: u64,
    smallest: u32,
    largest: u32,
}

/// The client.
struct Client {
    rings: [Option<Ring>; RINGS],
    counts: Counts,
    /// When the first frame came, on the counter.
    first_at: Option<u64>,
    reported_run: bool,
    dumped: bool,
}

/// Bytes as base64 (RFC 4648's alphabet, padded).
struct Base64<'a>(&'a [u8]);

impl fmt::Display for Base64<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let sextet = |value: u32, shift: u32| {
            let index = usize::try_from((value >> shift) & 0x3F).unwrap_or(0);
            char::from(ALPHABET.get(index).copied().unwrap_or(b'='))
        };
        for chunk in self.0.chunks(3) {
            let byte = |n: usize| u32::from(chunk.get(n).copied().unwrap_or(0));
            let value = (byte(0) << 16) | (byte(1) << 8) | byte(2);
            let mut out = [sextet(value, 18), sextet(value, 12), '=', '='];
            if chunk.len() > 1 {
                out[2] = sextet(value, 6);
            }
            if chunk.len() > 2 {
                out[3] = sextet(value, 0);
            }
            for c in out {
                write!(f, "{c}")?;
            }
        }
        Ok(())
    }
}

/// CRC-32 as IEEE 802.3 and zlib compute it: reflected, polynomial
/// 0xEDB88320, from all ones, the result inverted.
#[derive(Clone, Copy)]
struct Crc32(u32);

impl Crc32 {
    const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                let low = self.0 & 1;
                self.0 = (self.0 >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(low));
            }
        }
    }

    const fn finish(self) -> u32 {
        !self.0
    }

    fn of(bytes: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(bytes);
        crc.finish()
    }
}

/// Binds ring `n`'s regions.
fn bind(n: usize) -> Result<Ring, RegionError> {
    let first = n.saturating_mul(2);
    Ok(Ring {
        frames: read_only(first)?.into_frame(0)?,
        host: read_only(first.saturating_add(1))?.into_words()?,
        mine: read_write(n)?.into_words()?,
        layout: None,
        released: 0,
        refused: false,
    })
}

#[protection_domain]
fn init() -> Client {
    debug_println!("NOWAYOS: camera-test: starting");
    let mut rings = [None, None];
    for (n, slot) in rings.iter_mut().enumerate() {
        match bind(n) {
            Ok(ring) => {
                ring.mine.store(RELEASED, 0);
                ring.mine.store(WANTED, 1);
                *slot = Some(ring);
            }
            Err(error) => debug_println!("NOWAYOS: camera-test: ring {n}: gave up: {error}"),
        }
    }
    Client {
        rings,
        counts: Counts::default(),
        first_at: None,
        reported_run: false,
        dumped: false,
    }
}

impl Client {
    /// Takes every new frame on ring `n`.
    fn take(&mut self, n: usize) {
        let Some(Some(ring)) = self.rings.get_mut(n) else {
            return;
        };
        if ring.refused {
            return;
        }
        let layout = match ring.layout {
            Some(layout) => layout,
            None => {
                let load = |index| ring.host.load(index).unwrap_or(0);
                match Layout::check(
                    load(word::VERSION),
                    load(word::SLOTS),
                    load(word::SLOT_BYTES),
                    ring.frames.len(),
                ) {
                    Ok(layout) => {
                        debug_println!(
                            "NOWAYOS: camera-test: a ring of {} frame slots of {} bytes",
                            layout.slots(),
                            layout.slot_bytes()
                        );
                        ring.layout = Some(layout);
                        layout
                    }
                    Err(error) => {
                        debug_println!(
                            "NOWAYOS: camera-test: ring {n}: its layout refused: {error}"
                        );
                        ring.refused = true;
                        return;
                    }
                }
            }
        };
        let written = ring.host.load(word::WRITTEN).unwrap_or(ring.released);
        let ready = match ring::ready(&layout, ring.released, written) {
            Ok(ready) => ready,
            Err(error) => {
                debug_println!("NOWAYOS: camera-test: ring {n}: a count refused: {error}");
                ring.refused = true;
                return;
            }
        };
        for _ in 0..ready {
            let Some(Some(ring)) = self.rings.get(n) else {
                return;
            };
            let sequence = ring.released;
            let at = layout.slot_at(sequence);
            // The record copied out before it is checked.
            let mut bytes = [0u8; RECORD_LEN];
            let record = if ring.frames.load(at, &mut bytes) {
                Record::decode(&bytes, sequence, &layout)
            } else {
                Err(ring::RingError::Layout {
                    slots: layout.slots(),
                    slot_bytes: layout.slot_bytes(),
                })
            };
            match record {
                Ok(record) => self.check(n, at.saturating_add(RECORD_LEN), &record),
                Err(error) => {
                    debug_println!("NOWAYOS: camera-test: ring {n}: a record refused: {error}");
                    if let Some(Some(ring)) = self.rings.get_mut(n) {
                        ring.refused = true;
                    }
                    return;
                }
            }
            if let Some(Some(ring)) = self.rings.get_mut(n) {
                ring.released = ring.released.wrapping_add(1);
                ring.mine.store(RELEASED, ring.released);
            }
        }
    }

    /// Checks ring `n`'s frame `record` names, at `at` in its frames
    /// region: a whole JPEG; counts it, reports, and dumps it when due.
    fn check(&mut self, n: usize, at: usize, record: &Record) {
        let Some(Some(ring)) = self.rings.get(n) else {
            return;
        };
        let len = usize::try_from(record.len).unwrap_or(0);
        let (mut head, mut tail) = ([0u8; 2], [0u8; 2]);
        let whole = len >= 4
            && ring.frames.load(at, &mut head)
            && ring
                .frames
                .load(at.saturating_add(len).saturating_sub(2), &mut tail)
            && head == SOI
            && tail == EOI;
        if !whole {
            debug_println!(
                "NOWAYOS: camera-test: frame {}: not a JPEG ({len} bytes)",
                record.sequence
            );
            return;
        }
        let behind = ring.host.load(word::BEHIND).unwrap_or(0);
        let damaged = ring.host.load(word::DAMAGED).unwrap_or(0);
        // The size first: the host stores it after the interval.
        let size = ring.host.load(word::SIZE).unwrap_or(0);
        let offered = ring::offer(size, ring.host.load(word::INTERVAL).unwrap_or(0));
        let now = cpu::counter();
        let counts = &mut self.counts;
        counts.frames = counts.frames.saturating_add(1);
        counts.bytes = counts.bytes.saturating_add(u64::from(record.len));
        counts.largest = counts.largest.max(record.len);
        counts.smallest = if counts.smallest == 0 {
            record.len
        } else {
            counts.smallest.min(record.len)
        };
        let first_at = *self.first_at.get_or_insert(now);
        if counts.frames == 1 {
            debug_println!("NOWAYOS: camera-test: the first frame: a whole JPEG");
            debug_println!(
                "NOWAYOS: camera-test: frame {}: {}x{} at {} a second, {} bytes",
                record.sequence,
                record.width,
                record.height,
                Rate(record.interval),
                record.len
            );
            match offered {
                Some(offer)
                    if (offer.width, offer.height, offer.interval)
                        == (record.width, record.height, record.interval) =>
                {
                    debug_println!("NOWAYOS: camera-test: the host's offer is its frames'");
                }
                _ => debug_println!(
                    "NOWAYOS: camera-test: ring {n}: the host's offer, {offered:?}, is not its frames'"
                ),
            }
        }
        let elapsed = now
            .wrapping_sub(first_at)
            .checked_div(cpu::counter_frequency())
            .unwrap_or(0);
        let report = |title: &str, counts: &Counts| {
            debug_println!(
                "NOWAYOS: camera-test: {title}{} frames, {} to {} bytes, {} on average, in {elapsed} s; the host dropped {behind} with the client behind, {damaged} damaged or too large",
                counts.frames,
                counts.smallest,
                counts.largest,
                counts.bytes.checked_div(counts.frames).unwrap_or(0)
            );
        };
        if counts.frames.is_multiple_of(REPORT_EVERY) {
            report("", counts);
        }
        if elapsed >= RUN_S && !self.reported_run {
            self.reported_run = true;
            debug_println!("NOWAYOS: camera-test: {RUN_S} s of frames");
            report("after ten minutes: ", counts);
        }
        if elapsed >= DUMP_AFTER_S && !self.dumped {
            self.dumped = true;
            self.dump(n, at, record);
            for ring in self.rings.iter().flatten() {
                ring.mine.store(WANTED, 0);
            }
            debug_println!("NOWAYOS: camera-test: frames no longer wanted");
        }
    }

    /// Prints ring `n`'s frame `record`, at `at`, base64 with a CRC-32 a
    /// line, [`COPIES`] times.
    fn dump(&self, n: usize, at: usize, record: &Record) {
        let Some(Some(ring)) = self.rings.get(n) else {
            return;
        };
        let len = usize::try_from(record.len).unwrap_or(0);
        debug_println!(
            "NOWAYOS: camera-test: frame {}: {len} bytes, as base64 in lines of {LINE} bytes, each with its CRC-32, {COPIES} times",
            record.sequence
        );
        let mut line = [0u8; LINE];
        for _ in 0..COPIES {
            let mut whole = Crc32::new();
            let mut offset = 0usize;
            while offset < len {
                let take = LINE.min(len.saturating_sub(offset));
                let part = line.get_mut(..take).unwrap_or_default();
                if !ring.frames.load(at.saturating_add(offset), part) {
                    break;
                }
                whole.update(part);
                debug_println!(
                    "NOWAYOS: camera-test: jpeg {} {offset:06x} {} {:08x}",
                    record.sequence,
                    Base64(part),
                    Crc32::of(part)
                );
                offset = offset.saturating_add(take);
            }
            debug_println!(
                "NOWAYOS: camera-test: jpeg {} end {len} {:08x}",
                record.sequence,
                whole.finish()
            );
        }
        debug_println!("NOWAYOS: camera-test: the frame's dump ends");
    }
}

impl Handler for Client {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        for n in 0..RINGS {
            if channels.contains(Channel::new(n)) {
                self.take(n);
            }
        }
        Ok(())
    }
}
