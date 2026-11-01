// SPDX-License-Identifier: MIT OR Apache-2.0
//! Payload transfers to whole frames.
//!
//! From UVC 1.5, section 2.4.3: every payload transfer that carries video
//! starts with a payload header (table 2-5): its length, then bmHeaderInfo,
//! with the Frame ID that toggles at each new frame, End of Frame
//! (optional), the presentation time and source clock fields' bits, Still
//! Image, Error and End of Header, then those fields (table 2-6: 4 and 6
//! bytes). From
//! the Motion-JPEG Payload 1.5: on an isochronous endpoint each interval
//! carries one payload transfer, header first, in up to three transactions
//! (section 3.2), and each frame is one JPEG that starts with SOI and ends
//! with EOI (section 3.3).
//!
//! **Whole frames, or none.** [`Assembler`] takes each payload transfer -
//! one isochronous interval's data, already copied out of DMA memory -
//! checks its header and writes what follows into the slot a
//! [`FrameSink`] gives it, then gives the slot to the client when the frame
//! ends: at End of Frame, or when the Frame ID toggles. A frame is dropped:
//!
//! * **never queued, when the client is behind** - no slot is free as it
//!   starts - and counted ([`Counts::behind`]);
//! * **when damaged**: a payload with its error bit, an interval missed
//!   while it was arriving ([`Assembler::lost`]), a payload header that
//!   does not check, or a frame that is not a whole JPEG - not starting
//!   with SOI, or with no EOI ([`Counts::damaged`]);
//! * when too large for a slot ([`Counts::too_large`]).
//!
//! The first frame seen is never taken: it may have started before the
//! stream did. A frame given to the client ends at its last EOI, so
//! padding after it is not sent on. Nothing else in a frame is read.

use core::fmt;

/// Start of Image (Motion-JPEG Payload 1.5, section 3.3).
pub const SOI: [u8; 2] = [0xFF, 0xD8];

/// End of Image (Motion-JPEG Payload 1.5, section 3.3).
pub const EOI: [u8; 2] = [0xFF, 0xD9];

/// bmHeaderInfo's bits (UVC 1.5, table 2-5).
const FID: u8 = 0x01;
const END_OF_FRAME: u8 = 0x02;
const PTS: u8 = 0x04;
const SCR: u8 = 0x08;
const STILL: u8 = 0x20;
const ERROR: u8 = 0x40;

/// A payload header, checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// bHeaderLength: where the payload's data starts.
    pub len: usize,
    /// The Frame ID bit.
    pub fid: bool,
    /// End of Frame: this payload ends its frame.
    pub eof: bool,
    /// The Error bit: something went wrong in the camera.
    pub error: bool,
    /// Still Image.
    pub still: bool,
}

/// Why a payload header was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// A payload of one byte: no room for a header.
    Truncated,
    /// bHeaderLength under 2, past the payload, or too short for the fields
    /// its bits say follow.
    BadLength {
        /// bHeaderLength.
        len: u8,
        /// The payload's bytes.
        payload: usize,
    },
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("a payload of one byte"),
            Self::BadLength { len, payload } => {
                write!(f, "a header of {len} bytes in a payload of {payload}")
            }
        }
    }
}

/// The header at the start of `payload`, a non-empty payload transfer.
///
/// # Errors
///
/// [`HeaderError`] for a payload of one byte, or a bHeaderLength under 2,
/// past the payload's end, or shorter than 2 bytes and the presentation
/// time's 4 and source clock's 6 its bits say it holds. A longer header is
/// taken: the fields after those are for future use (bmHeaderInfo's End of
/// Header).
pub fn header(payload: &[u8]) -> Result<Header, HeaderError> {
    let (Some(&len), Some(&info)) = (payload.first(), payload.get(1)) else {
        return Err(HeaderError::Truncated);
    };
    let needed = 2u8
        .saturating_add(if info & PTS != 0 { 4 } else { 0 })
        .saturating_add(if info & SCR != 0 { 6 } else { 0 });
    if len < needed || usize::from(len) > payload.len() {
        return Err(HeaderError::BadLength {
            len,
            payload: payload.len(),
        });
    }
    Ok(Header {
        len: usize::from(len),
        fid: info & FID != 0,
        eof: info & END_OF_FRAME != 0,
        error: info & ERROR != 0,
        still: info & STILL != 0,
    })
}

/// Where whole frames go: a slot at a time, of the client's ring.
pub trait FrameSink {
    /// A slot for a new frame, and the bytes it holds; `None` when the
    /// client is behind - every slot holds a frame it has not read. The
    /// same slot is given again until a frame is published into it.
    fn begin(&mut self) -> Option<usize>;

    /// Writes `bytes` at byte `at` of the slot begun; `false`, writing
    /// nothing, if they do not fit.
    fn write(&mut self, at: usize, bytes: &[u8]) -> bool;

    /// The frame in the slot begun is whole: its first `len` bytes go to
    /// the client.
    fn publish(&mut self, len: usize);
}

/// Why a frame was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// A payload's Error bit.
    Error,
    /// An interval missed while it arrived.
    Lost,
    /// A payload header that did not check.
    BadHeader,
    /// Larger than a slot.
    TooLarge,
    /// Not starting with SOI, or with no EOI.
    NotJpeg,
}

impl Damage {
    /// What it was, for a log line.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Error => "a payload's error bit",
            Self::Lost => "an interval missed",
            Self::BadHeader => "a payload header refused",
            Self::TooLarge => "too large for a slot",
            Self::NotJpeg => "not a whole JPEG",
        }
    }
}

/// What an assembler has done, from its start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Frames given to the client.
    pub frames: u64,
    /// Their bytes.
    pub bytes: u64,
    /// The smallest and the largest, in bytes; 0 before the first.
    pub smallest: usize,
    /// The largest.
    pub largest: usize,
    /// Frames dropped with no slot free: the client behind.
    pub behind: u64,
    /// Frames dropped damaged: an error bit, an interval missed, a bad
    /// header, or not a whole JPEG.
    pub damaged: u64,
    /// Frames dropped as too large for a slot.
    pub too_large: u64,
    /// Payload headers refused.
    pub bad_headers: u64,
    /// Intervals reported missed.
    pub lost: u64,
}

/// A frame being written into a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    fid: bool,
    len: usize,
    room: usize,
    damage: Option<Damage>,
    head: [u8; 2],
    /// Where the last EOI seen ends.
    eoi: Option<usize>,
    /// The last byte written, for an EOI split between two payloads.
    prev: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Since the start: waiting for a frame to begin - its Frame ID, once
    /// one payload has given one.
    Syncing(Option<bool>),
    /// The frame of this Frame ID ended at its End of Frame.
    Between(bool),
    /// A frame arriving into a slot.
    Collecting(Frame),
    /// A frame being dropped as it arrives: no slot was free.
    Skipping(bool),
}

/// Payload transfers in, whole Motion JPEG frames out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assembler {
    state: State,
    counts: Counts,
    last: Option<Damage>,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    /// An assembler waiting for its first whole frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Syncing(None),
            counts: Counts {
                frames: 0,
                bytes: 0,
                smallest: 0,
                largest: 0,
                behind: 0,
                damaged: 0,
                too_large: 0,
                bad_headers: 0,
                lost: 0,
            },
            last: None,
        }
    }

    /// The counts so far.
    #[must_use]
    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// Why the last frame dropped as damaged or too large was, if one was.
    #[must_use]
    pub const fn last_damage(&self) -> Option<Damage> {
        self.last
    }

    /// An interval was missed - its data never came: the frame arriving,
    /// if any, is damaged.
    pub fn lost(&mut self) {
        self.counts.lost = self.counts.lost.saturating_add(1);
        if let State::Collecting(frame) = &mut self.state {
            frame.damage.get_or_insert(Damage::Lost);
        }
    }

    /// The stream stopped and starts again: a frame left part
    /// written is given up, uncounted - its slot was never published - and,
    /// as at the start, the next frame to begin is awaited.
    pub fn restart(&mut self) {
        self.state = State::Syncing(None);
    }

    /// One payload transfer, `data` already copied out of DMA memory: its
    /// header checked, its data written into the frame it belongs to, and
    /// a frame it ends given to the client through `sink` if whole.
    pub fn payload(&mut self, data: &[u8], sink: &mut impl FrameSink) {
        if data.is_empty() {
            return;
        }
        let Ok(header) = header(data) else {
            self.counts.bad_headers = self.counts.bad_headers.saturating_add(1);
            if let State::Collecting(frame) = &mut self.state {
                frame.damage.get_or_insert(Damage::BadHeader);
            }
            return;
        };
        let body = data.get(header.len..).unwrap_or_default();
        match self.state {
            State::Syncing(None) => {
                self.state = if header.eof {
                    State::Between(header.fid)
                } else {
                    State::Syncing(Some(header.fid))
                };
            }
            State::Syncing(Some(fid)) | State::Skipping(fid) if header.fid == fid => {
                if header.eof {
                    self.state = State::Between(fid);
                }
            }
            // Its frame has ended: a header with no new frame.
            State::Between(fid) if header.fid == fid => {}
            State::Collecting(frame) if header.fid == frame.fid => {
                self.append(frame, header, body, sink);
            }
            State::Collecting(frame) => {
                // The Frame ID toggled with no End of Frame: the frame
                // ended at the payload before.
                self.finish(frame, sink);
                self.start(header, body, sink);
            }
            _ => self.start(header, body, sink),
        }
    }

    /// A new frame starts with this payload: into a slot, or dropped if
    /// none is free.
    fn start(&mut self, header: Header, body: &[u8], sink: &mut impl FrameSink) {
        match sink.begin() {
            Some(room) => {
                let frame = Frame {
                    fid: header.fid,
                    len: 0,
                    room,
                    damage: None,
                    head: [0; 2],
                    eoi: None,
                    prev: None,
                };
                self.append(frame, header, body, sink);
            }
            None => {
                self.counts.behind = self.counts.behind.saturating_add(1);
                self.state = if header.eof {
                    State::Between(header.fid)
                } else {
                    State::Skipping(header.fid)
                };
            }
        }
    }

    /// Writes a payload's data into `frame`, noting its first bytes and
    /// where its last EOI ends, and ends the frame at End of Frame.
    fn append(&mut self, mut frame: Frame, header: Header, body: &[u8], sink: &mut impl FrameSink) {
        if header.error {
            frame.damage.get_or_insert(Damage::Error);
        }
        if frame.damage.is_none() && !body.is_empty() {
            match frame.len.checked_add(body.len()) {
                Some(end) if end <= frame.room && sink.write(frame.len, body) => {
                    for (offset, byte) in body.iter().copied().enumerate() {
                        let at = frame.len.saturating_add(offset);
                        if let Some(slot) = frame.head.get_mut(at) {
                            *slot = byte;
                        }
                        if frame.prev == Some(0xFF) && byte == 0xD9 {
                            frame.eoi = Some(at.saturating_add(1));
                        }
                        frame.prev = Some(byte);
                    }
                    frame.len = end;
                }
                _ => frame.damage = Some(Damage::TooLarge),
            }
        }
        if header.eof {
            self.finish(frame, sink);
            self.state = State::Between(header.fid);
        } else {
            self.state = State::Collecting(frame);
        }
    }

    /// `frame` has ended: given to the client if whole, dropped and
    /// counted otherwise.
    fn finish(&mut self, frame: Frame, sink: &mut impl FrameSink) {
        let whole = match (frame.damage, frame.eoi) {
            (Some(damage), _) => Err(damage),
            (None, Some(end)) if frame.head == SOI => Ok(end),
            (None, _) => Err(Damage::NotJpeg),
        };
        match whole {
            Ok(len) => {
                sink.publish(len);
                let counts = &mut self.counts;
                counts.frames = counts.frames.saturating_add(1);
                counts.bytes = counts
                    .bytes
                    .saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
                counts.largest = counts.largest.max(len);
                counts.smallest = if counts.smallest == 0 {
                    len
                } else {
                    counts.smallest.min(len)
                };
            }
            Err(damage) => {
                if damage == Damage::TooLarge {
                    self.counts.too_large = self.counts.too_large.saturating_add(1);
                } else {
                    self.counts.damaged = self.counts.damaged.saturating_add(1);
                }
                self.last = Some(damage);
            }
        }
    }
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

    /// A client's ring in memory: `slots` slots of `room` bytes, the frames
    /// published, and whether the client is behind.
    pub(crate) struct Ring {
        pub(crate) room: usize,
        pub(crate) slot: Vec<u8>,
        pub(crate) published: Vec<Vec<u8>>,
        pub(crate) full: bool,
    }

    impl Ring {
        pub(crate) fn new(room: usize) -> Self {
            Self {
                room,
                slot: std::vec![0; room],
                published: Vec::new(),
                full: false,
            }
        }
    }

    impl FrameSink for Ring {
        fn begin(&mut self) -> Option<usize> {
            (!self.full).then_some(self.room)
        }

        fn write(&mut self, at: usize, bytes: &[u8]) -> bool {
            match self.slot.get_mut(at..at + bytes.len()) {
                Some(target) => {
                    target.copy_from_slice(bytes);
                    true
                }
                None => false,
            }
        }

        fn publish(&mut self, len: usize) {
            self.published.push(self.slot[..len].to_vec());
        }
    }

    /// A synthetic test pattern as a JPEG's framing: SOI, an APP0 segment,
    /// `n` bytes of a counting pattern standing in for the scan - never an
    /// image of anything - and EOI.
    pub(crate) fn jpeg(n: usize, seed: u8) -> Vec<u8> {
        let mut out = std::vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, seed, 0];
        out.extend((0..n).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) & 0x7F));
        out.extend_from_slice(&EOI);
        out
    }

    /// A payload: a 12-byte header with PTS and SCR (as many cameras send),
    /// then `data`.
    pub(crate) fn payload(fid: bool, eof: bool, data: &[u8]) -> Vec<u8> {
        let info = 0x80 | SCR | PTS | u8::from(fid) | if eof { END_OF_FRAME } else { 0 };
        let mut out = std::vec![12, info, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        out.extend_from_slice(data);
        out
    }

    /// `frame` cut into payloads of at most `size` bytes of data with Frame
    /// ID `fid`, the last with End of Frame if `eof`.
    pub(crate) fn payloads(frame: &[u8], fid: bool, size: usize, eof: bool) -> Vec<Vec<u8>> {
        let chunks: Vec<&[u8]> = frame.chunks(size).collect();
        let last = chunks.len() - 1;
        chunks
            .iter()
            .enumerate()
            .map(|(n, chunk)| payload(fid, eof && n == last, chunk))
            .collect()
    }

    fn feed(assembler: &mut Assembler, ring: &mut Ring, list: &[Vec<u8>]) {
        for one in list {
            assembler.payload(one, ring);
        }
    }

    #[test]
    fn headers() {
        assert_eq!(
            header(&[2, 0x83]),
            Ok(Header {
                len: 2,
                fid: true,
                eof: true,
                error: false,
                still: false
            })
        );
        assert_eq!(
            header(&[12, 0x8C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF])
                .unwrap()
                .len,
            12
        );
        assert_eq!(
            header(&[2, 0x60]).map(|h| (h.error, h.still)),
            Ok((true, true))
        );
        // Longer than its fields need: taken.
        assert_eq!(header(&[4, 0x80, 0, 0]).unwrap().len, 4);
        assert_eq!(header(&[2]), Err(HeaderError::Truncated));
        for bad in [
            &[1u8, 0][..],
            &[3, 0, 0][..2],
            &[6, 0x0C, 0, 0, 0, 0],
            &[2, 0x04],
        ] {
            assert!(
                matches!(header(bad), Err(HeaderError::BadLength { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn whole_frames_with_headers_removed_the_first_never_taken() {
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        let (a, b, c) = (jpeg(1000, 1), jpeg(2500, 2), jpeg(700, 3));
        // The tail of a frame begun before the stream: not taken.
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&a[300..], false, 1012, true),
        );
        feed(&mut assembler, &mut ring, &payloads(&b, true, 1012, true));
        // Header-only payloads between frames, as cameras send.
        feed(
            &mut assembler,
            &mut ring,
            &[payload(true, false, &[]), payload(true, true, &[])],
        );
        feed(&mut assembler, &mut ring, &payloads(&c, false, 1012, true));
        assert_eq!(ring.published, [b.clone(), c.clone()]);
        let counts = assembler.counts();
        assert_eq!((counts.frames, counts.damaged, counts.behind), (2, 0, 0));
        assert_eq!(counts.bytes, (b.len() + c.len()) as u64);
        assert_eq!((counts.smallest, counts.largest), (c.len(), b.len()));
    }

    #[test]
    fn a_stream_started_again_gives_up_the_frame_it_was_writing() {
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        let (a, b, c, d) = (jpeg(900, 7), jpeg(800, 8), jpeg(700, 9), jpeg(600, 10));
        feed(&mut assembler, &mut ring, &[payload(true, true, &[])]);
        feed(&mut assembler, &mut ring, &payloads(&a, false, 300, true));
        // Stopped part-way through b, and started again.
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&b[..400], true, 300, false),
        );
        assembler.restart();
        // c has b's Frame ID: never joined to it, and awaited past, as the
        // first frame at the start is.
        feed(&mut assembler, &mut ring, &payloads(&c, true, 300, true));
        feed(&mut assembler, &mut ring, &payloads(&d, false, 300, true));
        assert_eq!(ring.published, [a, d]);
        assert_eq!(assembler.counts().damaged, 0);
    }

    #[test]
    fn a_frame_ends_when_its_frame_id_toggles_with_no_end_of_frame() {
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        let (a, b) = (jpeg(900, 4), jpeg(400, 5));
        feed(
            &mut assembler,
            &mut ring,
            &[payload(true, false, &[1, 2, 3])],
        );
        feed(&mut assembler, &mut ring, &payloads(&a, false, 300, false));
        feed(&mut assembler, &mut ring, &payloads(&b, true, 300, false));
        assert_eq!(ring.published, [a]);
        // Padding after EOI, then the next toggle: the frame ends at its EOI.
        let mut padded = b.clone();
        padded.extend_from_slice(&[0; 40]);
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        feed(&mut assembler, &mut ring, &[payload(false, true, &[])]);
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&padded, true, 300, false),
        );
        feed(
            &mut assembler,
            &mut ring,
            &[payload(false, false, &[0xFF, 0xD8])],
        );
        assert_eq!(ring.published, [b]);
    }

    #[test]
    fn an_eoi_split_between_two_payloads_is_found() {
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        let frame = jpeg(10, 6);
        let cut = frame.len() - 1;
        feed(&mut assembler, &mut ring, &[payload(true, true, &[])]);
        feed(
            &mut assembler,
            &mut ring,
            &[
                payload(false, false, &frame[..cut]),
                payload(false, true, &frame[cut..]),
            ],
        );
        assert_eq!(ring.published, [frame]);
    }

    #[test]
    fn damaged_frames_are_dropped_and_counted() {
        let start = || {
            let mut assembler = Assembler::new();
            let mut ring = Ring::new(4096);
            feed(&mut assembler, &mut ring, &[payload(true, true, &[])]);
            (assembler, ring)
        };
        let frame = jpeg(2000, 7);
        let parts = payloads(&frame, false, 500, true);

        // A payload with its Error bit.
        let (mut assembler, mut ring) = start();
        let mut bad = parts.clone();
        bad[1][1] |= ERROR;
        feed(&mut assembler, &mut ring, &bad);
        assert!(ring.published.is_empty());
        assert_eq!(assembler.last_damage(), Some(Damage::Error));

        // An interval missed in the middle.
        let (mut assembler, mut ring) = start();
        feed(&mut assembler, &mut ring, &parts[..2]);
        assembler.lost();
        feed(&mut assembler, &mut ring, &parts[2..]);
        assert!(ring.published.is_empty());
        assert_eq!(assembler.last_damage(), Some(Damage::Lost));

        // A header longer than its payload.
        let (mut assembler, mut ring) = start();
        feed(&mut assembler, &mut ring, &parts[..2]);
        assembler.payload(&[40, 0x80, 0], &mut ring);
        feed(&mut assembler, &mut ring, &parts[2..]);
        assert_eq!(assembler.last_damage(), Some(Damage::BadHeader));
        assert_eq!(assembler.counts().bad_headers, 1);

        // Its first payload lost before it came: no SOI. Its last lost: no
        // EOI, so it ends at the next toggle, damaged.
        let (mut assembler, mut ring) = start();
        feed(&mut assembler, &mut ring, &parts[1..]);
        assert_eq!(assembler.last_damage(), Some(Damage::NotJpeg));
        let (mut assembler, mut ring) = start();
        let cut = &parts[..parts.len() - 1];
        feed(&mut assembler, &mut ring, cut);
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&jpeg(50, 8), true, 500, true),
        );
        assert_eq!(assembler.last_damage(), Some(Damage::NotJpeg));
        assert_eq!(ring.published, [jpeg(50, 8)], "the next frame is whole");
        assert_eq!(assembler.counts().damaged, 1);

        // Too large for its slot.
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(1500);
        feed(&mut assembler, &mut ring, &[payload(true, true, &[])]);
        feed(&mut assembler, &mut ring, &parts);
        assert_eq!(assembler.last_damage(), Some(Damage::TooLarge));
        assert_eq!(
            (assembler.counts().too_large, assembler.counts().damaged),
            (1, 0)
        );
    }

    #[test]
    fn a_frame_starting_while_the_client_is_behind_is_dropped_never_queued() {
        let mut assembler = Assembler::new();
        let mut ring = Ring::new(4096);
        feed(&mut assembler, &mut ring, &[payload(true, true, &[])]);
        ring.full = true;
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&jpeg(800, 9), false, 300, true),
        );
        // The client catches up mid-frame: that frame is still dropped,
        // and the next taken.
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&jpeg(800, 10), true, 300, false)[..1],
        );
        ring.full = false;
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&jpeg(800, 10), true, 300, true)[1..],
        );
        feed(
            &mut assembler,
            &mut ring,
            &payloads(&jpeg(600, 11), false, 300, true),
        );
        assert_eq!(ring.published, [jpeg(600, 11)]);
        assert_eq!(assembler.counts().behind, 2);
    }

    proptest! {
        #[test]
        fn any_payloads_never_panic_and_only_whole_jpegs_go_out(
            list in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), 0..80),
            lose in proptest::collection::vec(any::<bool>(), 0..80),
            room in 0usize..300,
        ) {
            let mut assembler = Assembler::new();
            let mut ring = Ring::new(room);
            for (one, lost) in list.iter().zip(lose.iter().chain(core::iter::repeat(&false))) {
                if *lost {
                    assembler.lost();
                }
                assembler.payload(one, &mut ring);
            }
            for frame in &ring.published {
                prop_assert!(frame.len() <= room);
                prop_assert_eq!(&frame[..2], &SOI);
                prop_assert_eq!(&frame[frame.len() - 2..], &EOI);
            }
            prop_assert_eq!(assembler.counts().frames, ring.published.len() as u64);
        }

        #[test]
        fn frames_cut_anywhere_come_out_whole(
            sizes in proptest::collection::vec(1usize..400, 1..8),
            chunk in 1usize..200,
            eof: bool,
        ) {
            let mut assembler = Assembler::new();
            let mut ring = Ring::new(1024);
            let frames: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(n, size)| jpeg(*size, n as u8)).collect();
            assembler.payload(&payload(true, true, &[]), &mut ring);
            for (n, frame) in frames.iter().enumerate() {
                for one in payloads(frame, n % 2 == 1, chunk, eof) {
                    assembler.payload(&one, &mut ring);
                }
            }
            // With no End of Frame, the last ends only when another starts.
            let whole = if eof { frames.len() } else { frames.len() - 1 };
            prop_assert_eq!(&ring.published[..], &frames[..whole]);
        }
    }
}
