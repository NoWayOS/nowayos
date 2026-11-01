// SPDX-License-Identifier: MIT OR Apache-2.0
//! Isochronous streams: the bookkeeping of one isochronous
//! endpoint's transfers, with no hardware in it, so it is tested here.
//!
//! From Intel's *eXtensible Host Controller Interface for Universal Serial
//! Bus (xHCI)*, revision 1.2: an isochronous transfer descriptor (TD) is one
//! service interval's data, and the controller takes one TD from the
//! endpoint's ring each interval (section 4.11.2.3); a TD it cannot serve in
//! its interval completes with a Missed Service Error, naming its TRB
//! (4.10.3.2); a ring empty when the interval comes raises one Ring Underrun
//! (OUT) or Ring Overrun (IN) event, naming no TD, and the endpoint waits
//! for its doorbell (4.10.3.1).
//!
//! **A bounded queue per endpoint, refilled each tick.** The USB host PD is
//! polled, so it keeps [`Queue`]'s TDs - at most [`MAX_QUEUED`], each with a
//! buffer of its own - queued ahead of the controller, enough to cover the
//! time between two of its ticks, and tops them up on every tick. **A
//! missed interval is counted, never fatal:** [`Stats::missed`] counts every
//! TD not served (a Missed Service Error, any other error, a TD the
//! controller passed over without an event) and every time the ring ran
//! empty - once, however many intervals the gap lasted - and the stream goes
//! on.
//!
//! [`FrameClock`] says how many audio frames each interval's packet carries
//! at a given rate (44 or 45 at 44.1 kHz every millisecond, say), and
//! [`Tone`] makes a test tone's samples: a sine from a phase accumulator and
//! Bhaskara I's approximation, in integers, as `no_std` has no `sin`.

use crate::audio::Pcm;
use crate::trb::Completion;

/// The most TDs one endpoint keeps queued: a one-page ring of 256 TRBs, less
/// its Link TRB.
pub const MAX_QUEUED: usize = 255;

/// One endpoint's counts, from its start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// TDs served: their interval's data moved.
    pub served: u64,
    /// The bytes they moved.
    pub bytes: u64,
    /// Intervals missed: TDs not served, and each time the ring ran empty.
    pub missed: u64,
    /// Of those, the times the ring ran empty.
    pub empty: u64,
}

/// Why a completion was refused: it names no TD in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotQueued;

/// A TD in flight: its TRB's index on the ring, the buffer it moves and
/// the bytes it asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Td {
    trb: u8,
    buffer: u8,
    len: u16,
}

/// A TD the controller completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completed {
    /// Its buffer: an IN TD's data is there until a new TD takes it.
    pub buffer: usize,
    /// The bytes it moved: what it asked for, less the residue the
    /// controller reported.
    pub len: usize,
}

/// One isochronous endpoint's TDs in flight, oldest first, and its counts.
/// The controller serves TDs in the order they were queued, so each buffer
/// is reused in turn: the one a new TD takes was the oldest's.
#[derive(Clone, Copy, Debug)]
pub struct Queue {
    tds: [Td; MAX_QUEUED],
    head: usize,
    len: usize,
    buffers: usize,
    next_buffer: usize,
    stats: Stats,
}

impl Queue {
    /// An empty queue whose TDs take turns with `buffers` buffers: 1 to
    /// [`MAX_QUEUED`], so at most that many TDs are ever in flight.
    #[must_use]
    pub const fn new(buffers: usize) -> Self {
        let buffers = if buffers == 0 {
            1
        } else if buffers > MAX_QUEUED {
            MAX_QUEUED
        } else {
            buffers
        };
        Self {
            tds: [Td {
                trb: 0,
                buffer: 0,
                len: 0,
            }; MAX_QUEUED],
            head: 0,
            len: 0,
            buffers,
            next_buffer: 0,
            stats: Stats {
                served: 0,
                bytes: 0,
                missed: 0,
                empty: 0,
            },
        }
    }

    /// The TDs in flight.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether none is.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The most TDs in flight at once: one for each buffer.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.buffers
    }

    /// The counts so far.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// The buffer the next TD takes, if one is free.
    #[must_use]
    pub const fn next_buffer(&self) -> Option<usize> {
        if self.len < self.buffers {
            Some(self.next_buffer)
        } else {
            None
        }
    }

    fn at(&self, offset: usize) -> usize {
        self.head
            .saturating_add(offset)
            .checked_rem(MAX_QUEUED)
            .unwrap_or(0)
    }

    /// Records a TD of `len` bytes queued at TRB index `trb` (below 256),
    /// with the buffer [`Self::next_buffer`] gave; returns that buffer, or
    /// `None`, recording nothing, when every buffer is in flight or the
    /// index is out of range.
    pub fn push(&mut self, trb: usize, len: u16) -> Option<usize> {
        let buffer = self.next_buffer()?;
        let td = Td {
            trb: u8::try_from(trb).ok()?,
            buffer: u8::try_from(buffer).ok()?,
            len,
        };
        let end = self.at(self.len);
        *self.tds.get_mut(end)? = td;
        self.len = self.len.saturating_add(1);
        self.next_buffer = buffer
            .saturating_add(1)
            .checked_rem(self.buffers)
            .unwrap_or(0);
        Some(buffer)
    }

    /// The TD at TRB index `trb` completed with `code`, `residue` of its
    /// bytes not moved; returns its buffer - to read an IN TD's data from
    /// before a new TD takes it - and the bytes moved. TDs queued before it
    /// that got no event of their own are retired as missed: the controller
    /// has passed them.
    ///
    /// # Errors
    ///
    /// [`NotQueued`] if no TD in flight is at `trb`: a completion for a TD
    /// never queued, or one already retired. Nothing changes.
    pub fn complete(
        &mut self,
        trb: usize,
        code: Completion,
        residue: usize,
    ) -> Result<Completed, NotQueued> {
        let position = (0..self.len)
            .find(|offset| {
                self.tds
                    .get(self.at(*offset))
                    .is_some_and(|td| usize::from(td.trb) == trb)
            })
            .ok_or(NotQueued)?;
        let td = self.tds.get(self.at(position)).copied().ok_or(NotQueued)?;
        let skipped = u64::try_from(position).unwrap_or(u64::MAX);
        self.stats.missed = self.stats.missed.saturating_add(skipped);
        let moved = usize::from(td.len).saturating_sub(residue);
        if code.ok() {
            self.stats.served = self.stats.served.saturating_add(1);
            self.stats.bytes = self
                .stats
                .bytes
                .saturating_add(u64::try_from(moved).unwrap_or(u64::MAX));
        } else {
            self.stats.missed = self.stats.missed.saturating_add(1);
        }
        self.head = self.at(position.saturating_add(1));
        self.len = self.len.saturating_sub(position.saturating_add(1));
        Ok(Completed {
            buffer: usize::from(td.buffer),
            len: moved,
        })
    }

    /// The ring ran empty when an interval came (a Ring Underrun or Ring
    /// Overrun): counted as one interval missed.
    pub fn ran_empty(&mut self) {
        self.stats.missed = self.stats.missed.saturating_add(1);
        self.stats.empty = self.stats.empty.saturating_add(1);
    }
}

/// How many audio frames each interval's packet carries at `rate` frames a
/// second, an interval every `interval_us` microseconds: the whole frames
/// due by the end of each interval, less those already sent, so the
/// fractions carry over (at 44.1 kHz every millisecond, nine packets of 44
/// frames and one of 45 in every ten).
///
/// An asynchronous speaker sets the pace itself: once its
/// feedback endpoint has spoken, [`FrameClock::pace`] makes each packet
/// carry the frames it asked for, its fractions carried over in turn.
#[derive(Clone, Copy, Debug)]
pub struct FrameClock {
    per_interval: u64,
    carried: u64,
    /// The frames each interval the device asked for, in 65536ths.
    paced: Option<u64>,
    carried_q16: u64,
}

/// The microseconds in a second.
const MICROS: u64 = 1_000_000;

impl FrameClock {
    /// A clock for `rate` frames a second, a packet every `interval_us`
    /// microseconds.
    #[must_use]
    pub const fn new(rate: u32, interval_us: u32) -> Self {
        Self {
            per_interval: (rate as u64).saturating_mul(interval_us as u64),
            carried: 0,
            paced: None,
            carried_q16: 0,
        }
    }

    /// From now on, `q16` 65536ths of a frame each interval: what an
    /// asynchronous speaker's feedback asked for
    /// (`nowayos_usb::audio::feedback_q16`).
    pub const fn pace(&mut self, q16: u64) {
        self.paced = Some(q16);
    }

    /// The frames the next packet carries.
    pub fn next_packet(&mut self) -> usize {
        if let Some(paced) = self.paced {
            let due = self.carried_q16.saturating_add(paced);
            self.carried_q16 = due & 0xFFFF;
            return usize::try_from(due >> 16).unwrap_or(0);
        }
        let due = self.carried.saturating_add(self.per_interval);
        self.carried = due.checked_rem(MICROS).unwrap_or(0);
        usize::try_from(due.checked_div(MICROS).unwrap_or(0)).unwrap_or(0)
    }
}

/// A test tone: a sine wave of a fixed frequency and amplitude, one sample
/// a frame, the same on every channel.
#[derive(Clone, Copy, Debug)]
pub struct Tone {
    phase: u32,
    step: u32,
    amplitude: u16,
}

impl Tone {
    /// A tone of `hz` at `rate` frames a second, peaking at `amplitude`
    /// (at most 32767); `None` unless `hz` is below half of `rate`, the
    /// highest frequency `rate` can carry.
    #[must_use]
    pub fn new(hz: u32, rate: u32, amplitude: u16) -> Option<Self> {
        if hz == 0 || u64::from(hz).saturating_mul(2) >= u64::from(rate) {
            return None;
        }
        // The phase is a fraction of a cycle in 32 bits: each frame moves
        // it on by hz / rate of 2^32.
        let step = (u64::from(hz) << 32).checked_div(u64::from(rate))?;
        Some(Self {
            phase: 0,
            step: u32::try_from(step).ok()?,
            amplitude: amplitude.min(0x7FFF),
        })
    }

    /// The next sample, and the phase moved on a frame.
    pub fn sample(&mut self) -> i16 {
        // Bhaskara I's approximation over half a cycle: with t in [0, 1) of
        // that half, sin(pi t) ~ 16 t (1 - t) / (5 - 4 t (1 - t)), within
        // 0.2% of full scale. x is t in 16 bits, a is t (1 - t) in 32.
        let x = u64::from((self.phase >> 15) & 0xFFFF);
        let a = x.saturating_mul(0x1_0000u64.saturating_sub(x));
        let numerator = u64::from(self.amplitude).saturating_mul(a.saturating_mul(16));
        let denominator = (5u64 << 32).saturating_sub(a.saturating_mul(4));
        let magnitude =
            i16::try_from(numerator.checked_div(denominator).unwrap_or(0)).unwrap_or(i16::MAX);
        // The first half of the cycle is above zero, the second below.
        let below = self.phase & 0x8000_0000 != 0;
        self.phase = self.phase.wrapping_add(self.step);
        if below {
            magnitude.saturating_neg()
        } else {
            magnitude
        }
    }

    /// Fills `out` with `frames` frames of the tone in `format`'s layout -
    /// each channel's subframe little-endian, the sample in its top bits
    /// (a 3- or 4-byte subframe's low bytes 0) - as far as `out` holds whole
    /// frames; returns the bytes written. A 1-byte subframe, which 16-bit
    /// samples do not fit, is left silent: nothing written.
    pub fn fill(&mut self, out: &mut [u8], frames: usize, format: &Pcm) -> usize {
        let subframe = usize::from(format.subframe);
        let frame_bytes = format.frame_bytes();
        if !(2..=4).contains(&subframe) || frame_bytes == 0 {
            return 0;
        }
        let mut written = 0usize;
        for frame in out.chunks_exact_mut(frame_bytes).take(frames) {
            let sample = self.sample().to_le_bytes();
            for channel in frame.chunks_exact_mut(subframe) {
                // The sample's two bytes at the subframe's top, zeros
                // below.
                let (low, high) = channel.split_at_mut(subframe.saturating_sub(2));
                low.fill(0);
                high.copy_from_slice(&sample);
            }
            written = written.saturating_add(frame_bytes);
        }
        written
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
    use proptest::prelude::*;

    #[test]
    fn tds_take_buffers_in_turn_and_complete_in_order() {
        let mut queue = Queue::new(3);
        assert_eq!(queue.capacity(), 3);
        assert_eq!(queue.next_buffer(), Some(0));
        assert_eq!(queue.push(10, 192), Some(0));
        assert_eq!(queue.push(11, 192), Some(1));
        assert_eq!(queue.push(12, 192), Some(2));
        assert_eq!(queue.next_buffer(), None, "every buffer in flight");
        assert_eq!(queue.push(13, 192), None);
        assert_eq!(queue.len(), 3);
        // The first completes: its buffer is free again, and the next TD
        // takes it.
        assert_eq!(
            queue.complete(10, Completion::SUCCESS, 0),
            Ok(Completed {
                buffer: 0,
                len: 192
            })
        );
        assert_eq!(queue.push(13, 192), Some(0));
        // A short IN packet is served too: 92 bytes of 192 not moved.
        assert_eq!(
            queue.complete(11, Completion::SHORT_PACKET, 92),
            Ok(Completed {
                buffer: 1,
                len: 100
            })
        );
        assert_eq!(
            queue.stats(),
            Stats {
                served: 2,
                bytes: 292,
                missed: 0,
                empty: 0
            }
        );
        // A TD the controller missed: counted, and the stream goes on.
        assert_eq!(
            queue
                .complete(12, Completion::MISSED_SERVICE, 192)
                .map(|done| done.buffer),
            Ok(2)
        );
        assert_eq!(queue.stats().missed, 1);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn a_completion_past_tds_with_no_event_retires_them_as_missed() {
        let mut queue = Queue::new(8);
        for trb in 0..5 {
            queue.push(trb, 4);
        }
        // The TD at TRB 3 completes: those at 0 to 2 were passed over.
        assert_eq!(
            queue
                .complete(3, Completion::SUCCESS, 0)
                .map(|done| done.buffer),
            Ok(3)
        );
        assert_eq!(queue.stats().missed, 3);
        assert_eq!(queue.stats().served, 1);
        assert_eq!(queue.len(), 1);
        // A completion for a TD retired, or never queued, is refused and
        // changes nothing.
        assert_eq!(queue.complete(1, Completion::SUCCESS, 0), Err(NotQueued));
        assert_eq!(queue.complete(200, Completion::SUCCESS, 0), Err(NotQueued));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.stats().missed, 3);
        // Any other error is an interval missed.
        assert!(queue.complete(4, Completion::TRANSACTION, 4).is_ok());
        assert_eq!(queue.stats().missed, 4);
        assert!(queue.is_empty());
        // The ring ran empty: one more, however long the gap.
        queue.ran_empty();
        assert_eq!((queue.stats().missed, queue.stats().empty), (5, 1));
    }

    #[test]
    fn the_queue_wraps_and_is_bounded() {
        let mut queue = Queue::new(1000);
        assert_eq!(queue.capacity(), MAX_QUEUED);
        assert_eq!(Queue::new(0).capacity(), 1);
        // Round the ring several times, TRB indices wrapping at 255 as a
        // one-page ring's do.
        let mut trb = 0usize;
        for _ in 0..MAX_QUEUED {
            assert!(queue.push(trb, 1).is_some());
            trb = (trb + 1) % 255;
        }
        assert_eq!(queue.push(trb, 1), None);
        let mut oldest = 0usize;
        for _ in 0..1000 {
            assert!(queue.complete(oldest, Completion::SUCCESS, 0).is_ok());
            oldest = (oldest + 1) % 255;
            assert!(queue.push(trb, 1).is_some());
            trb = (trb + 1) % 255;
        }
        assert_eq!(queue.stats().missed, 0);
        assert_eq!(queue.stats().served, 1000);
        // A TRB index past what a TD records is refused.
        let mut small = Queue::new(2);
        assert_eq!(small.push(256, 1), None);
        assert!(small.is_empty());
    }

    #[test]
    fn frames_per_packet_carry_their_fractions() {
        let mut clock = FrameClock::new(48_000, 1000);
        assert!((0..100).all(|_| clock.next_packet() == 48));
        let mut clock = FrameClock::new(44_100, 1000);
        let packets: Vec<usize> = (0..10).map(|_| clock.next_packet()).collect();
        assert_eq!(packets.iter().sum::<usize>(), 441);
        assert_eq!(packets.iter().filter(|frames| **frames == 45).count(), 1);
        // High speed every microframe: 6 frames at 48 kHz.
        let mut clock = FrameClock::new(48_000, 125);
        assert_eq!(clock.next_packet(), 6);
        // Over a long run the total is exact.
        let mut clock = FrameClock::new(44_100, 1000);
        assert_eq!(
            (0..60_000).map(|_| clock.next_packet()).sum::<usize>(),
            44_100 * 60
        );
        // Paced by a speaker at 48.25 frames a frame: a 49 every fourth.
        clock.pace((48 << 16) + (1 << 14));
        let packets: Vec<usize> = (0..8).map(|_| clock.next_packet()).collect();
        assert_eq!(packets, [48, 48, 48, 49, 48, 48, 48, 49]);
    }

    fn pcm(channels: u8, subframe: u8) -> Pcm {
        Pcm {
            channels,
            subframe,
            bits: subframe * 8,
            rates: crate::audio::Rates::Range {
                low: 48_000,
                high: 48_000,
            },
        }
    }

    #[test]
    fn a_tone_is_a_sine() {
        // 1 kHz at 48 kHz: 48 samples a cycle; the peak a quarter in.
        let mut tone = Tone::new(1000, 48_000, 10_000).unwrap();
        let samples: Vec<i16> = (0..96).map(|_| tone.sample()).collect();
        assert_eq!(samples[0], 0);
        assert!((9_980..=10_000).contains(&samples[12]), "{}", samples[12]);
        assert!(samples[24].abs() <= 1);
        assert!((-10_000..=-9_980).contains(&samples[36]), "{}", samples[36]);
        for (first, second) in samples[..48].iter().zip(&samples[48..]) {
            assert!((first - second).abs() <= 2, "one cycle as the next");
        }
        // Within 0.2% of full scale of the true sine at every sample.
        for (n, sample) in samples.iter().enumerate() {
            let exact = 10_000.0 * (2.0 * core::f64::consts::PI * n as f64 / 48.0).sin();
            assert!(
                (f64::from(*sample) - exact).abs() < 25.0,
                "sample {n}: {sample} vs {exact}"
            );
        }
        // Frequencies the rate cannot carry are refused.
        assert!(Tone::new(24_000, 48_000, 1).is_none());
        assert!(Tone::new(0, 48_000, 1).is_none());
        assert!(Tone::new(1, 0, 1).is_none());
    }

    #[test]
    fn a_tone_fills_frames_in_the_formats_layout() {
        let mut tone = Tone::new(1000, 48_000, 0x7FFF).unwrap();
        for _ in 0..12 {
            tone.sample();
        }
        // The peak, 16-bit stereo: the same sample on both channels.
        let mut out = [0xAAu8; 10];
        assert_eq!(tone.fill(&mut out, 1, &pcm(2, 2)), 4);
        assert_eq!(out[..2], out[2..4]);
        assert!(i16::from_le_bytes([out[0], out[1]]) > 32_700);
        assert_eq!(out[4..], [0xAA; 6], "nothing past the frame");
        // 24-bit mono: the sample in the top two bytes.
        let mut tone = Tone::new(1000, 48_000, 0x7FFF).unwrap();
        for _ in 0..12 {
            tone.sample();
        }
        let mut out = [0xAAu8; 3];
        assert_eq!(tone.fill(&mut out, 5, &pcm(1, 3)), 3, "as far as out holds");
        assert_eq!(out[0], 0);
        assert!(i16::from_le_bytes([out[1], out[2]]) > 32_700);
        // A 1-byte subframe is left silent.
        assert_eq!(tone.fill(&mut out, 1, &pcm(1, 1)), 0);
    }

    proptest! {
        #[test]
        fn any_order_of_events_keeps_the_queue_bounded_and_the_counts_whole(
            buffers in 0usize..300,
            steps in proptest::collection::vec((0u8..3, any::<u16>(), 0u8..40), 0..600),
        ) {
            let mut queue = Queue::new(buffers);
            for (action, trb, code) in steps {
                match action {
                    0 => { let _ = queue.push(usize::from(trb), 16); }
                    1 => {
                        if let Ok(done) = queue.complete(usize::from(trb), Completion(code), usize::from(trb)) {
                            prop_assert!(done.len <= 16 && done.buffer < queue.capacity());
                        }
                    }
                    _ => queue.ran_empty(),
                }
                prop_assert!(queue.len() <= queue.capacity());
                prop_assert!(queue.capacity() <= MAX_QUEUED);
                let stats = queue.stats();
                prop_assert!(stats.empty <= stats.missed);
            }
        }

        #[test]
        fn a_tone_never_exceeds_its_amplitude(hz in 1u32..24_000, amplitude: u16, frames in 0usize..200) {
            if let Some(mut tone) = Tone::new(hz, 48_000, amplitude) {
                for _ in 0..frames {
                    let sample = i32::from(tone.sample());
                    prop_assert!(sample.abs() <= i32::from(amplitude.min(0x7FFF)));
                }
            }
        }
    }
}
