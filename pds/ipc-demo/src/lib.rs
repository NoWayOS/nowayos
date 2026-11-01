// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared definitions for the IPC demonstration,
//! `systems/ipc-demo.system`.
//!
//! A producer PD (`nowayos-ipc-producer`) sends a fixed, synthetic sequence
//! of [`InputEvent`]s through a `nowayos-ipc` ring in shared memory and
//! notifies a consumer PD (`nowayos-ipc-consumer`). The consumer copies each
//! record out of shared memory, validates and decodes it, and reports it on
//! the debug console. One record is deliberately malformed, and the consumer
//! must reject it and carry on.
//!
//! Both PDs use this crate, so they agree on the ring geometry, the sequence
//! and the report lines. The tests run the same sequence through an
//! in-memory ring of the same geometry and check the consumer's lines
//! against `systems/ipc-demo.expect`, so the expected QEMU output is also
//! checked on the host, without seL4.
//!
//! Provisional: a demonstration, not a product protocol.

#![cfg_attr(not(test), no_std)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use core::fmt;

use nowayos_input::{INPUT_EVENT_KIND, InputError, InputEvent, InputLimits, KeyCode, TouchPhase};
use nowayos_ipc::{
    DecodeError, EncodeError, HEADER_LEN, Message, Producer, ProducerMemory, Reader, Record,
    SendError, Writer, encode, encoded_len,
};

/// Start of every producer line on the debug console.
pub const PRODUCER_TAG: &str = "NOWAYOS: ipc-producer:";

/// Start of every consumer line on the debug console.
pub const CONSUMER_TAG: &str = "NOWAYOS: ipc-consumer:";

/// Slots in the ring. Deliberately small: the 11-record sequence laps the
/// ring, so the demonstration exercises back-pressure — the producer fills
/// the ring, the consumer drains it and notifies back, the producer
/// continues.
pub const RING_SLOTS: usize = 4;

/// Bytes per slot: room for every record in this demonstration.
pub const RING_SLOT: usize = 32;

const _: () = assert!(encoded_len::<InputEvent>() <= RING_SLOT);
const _: () = assert!(encoded_len::<SequenceEnd>() <= RING_SLOT);

/// Width of the screen the consumer validates events against, in pixels.
pub const SCREEN_WIDTH: u16 = 800;

/// Height of the screen the consumer validates events against, in pixels.
pub const SCREEN_HEIGHT: u16 = 480;

/// The limits the consumer validates every event against.
#[must_use]
pub const fn screen_limits() -> Option<InputLimits> {
    InputLimits::new(SCREEN_WIDTH, SCREEN_HEIGHT)
}

/// Message kind of [`SequenceEnd`], from the `0x7Fxx` range NoWayOS keeps
/// for demonstrations and tests; never used on a product channel.
pub const SEQUENCE_END_KIND: u16 = 0x7F00;

/// The end-of-sequence marker: the producer's last record, stating how many
/// records it sent before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceEnd {
    /// Records sent before this one.
    pub records: u32,
}

impl Message for SequenceEnd {
    const KIND: u16 = SEQUENCE_END_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 4;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u32(self.records)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            records: input.read_u32()?,
        })
    }
}

/// One record of the demonstration sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoRecord {
    /// A valid input event.
    Event(InputEvent),
    /// An input-event record whose event tag is [`MALFORMED_TAG`], which no
    /// decoder accepts: what a faulty or hostile producer might send.
    MalformedEvent,
    /// The end-of-sequence marker ([`SequenceEnd`]).
    End,
}

/// The event tag in [`DemoRecord::MalformedEvent`]. `nowayos-input` defines
/// tags 1 to 5 only.
pub const MALFORMED_TAG: u8 = 0x7F;

/// The fixed, synthetic sequence the producer sends: a click, a Tab key
/// press, the malformed record, an Enter key press, a tap, and the end
/// marker.
pub const SEQUENCE: [DemoRecord; 11] = [
    DemoRecord::Event(InputEvent::PointerMove { x: 120, y: 80 }),
    DemoRecord::Event(InputEvent::PointerButton {
        button: 0,
        down: true,
    }),
    DemoRecord::Event(InputEvent::PointerButton {
        button: 0,
        down: false,
    }),
    DemoRecord::Event(InputEvent::KeyDown(KeyCode::TAB)),
    DemoRecord::Event(InputEvent::KeyUp(KeyCode::TAB)),
    DemoRecord::MalformedEvent,
    DemoRecord::Event(InputEvent::KeyDown(KeyCode::ENTER)),
    DemoRecord::Event(InputEvent::KeyUp(KeyCode::ENTER)),
    DemoRecord::Event(InputEvent::Touch {
        id: 0,
        x: 300,
        y: 200,
        phase: TouchPhase::Start,
    }),
    DemoRecord::Event(InputEvent::Touch {
        id: 0,
        x: 300,
        y: 200,
        phase: TouchPhase::End,
    }),
    DemoRecord::End,
];

/// Records in [`SEQUENCE`] before the end marker: what [`SequenceEnd`]
/// announces.
pub const RECORDS_BEFORE_END: u32 = 10;

/// Sends one record of the sequence through `tx`.
///
/// # Errors
///
/// As for [`Producer::try_send`]; [`SendError::Full`] means "try again once
/// the consumer has drained the ring".
pub fn send<R: ProducerMemory<RING_SLOT> + ?Sized>(
    tx: &mut Producer<'_, R, RING_SLOT>,
    record: &DemoRecord,
) -> Result<(), SendError> {
    match record {
        DemoRecord::Event(event) => tx.try_send(event),
        DemoRecord::End => tx.try_send(&SequenceEnd {
            records: RECORDS_BEFORE_END,
        }),
        DemoRecord::MalformedEvent => tx.try_send_bytes(&malformed_event_bytes()?),
    }
}

/// A well-formed header announcing an input event, followed by an event
/// payload whose tag is [`MALFORMED_TAG`].
fn malformed_event_bytes() -> Result<[u8; RING_SLOT], EncodeError> {
    let mut bytes = [0u8; RING_SLOT];
    encode(&InputEvent::KeyDown(KeyCode::A), &mut bytes)?;
    if let Some(tag) = bytes.get_mut(HEADER_LEN) {
        *tag = MALFORMED_TAG;
    }
    Ok(bytes)
}

/// What the consumer made of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// A valid event, within the screen limits.
    Event(InputEvent),
    /// The end-of-sequence marker.
    End(SequenceEnd),
    /// A record that failed validation. It has still been consumed.
    Rejected(Rejection),
}

/// Why the consumer rejected a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// The record is malformed.
    Malformed(DecodeError),
    /// The record is well formed but of a kind this channel does not carry.
    UnknownKind(u16),
    /// The event is well formed but outside the screen or device limits.
    OutsideLimits(InputError),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(err) => write!(f, "malformed: {err}"),
            Self::UnknownKind(kind) => write!(f, "unknown message kind {kind:#06x}"),
            Self::OutsideLimits(err) => write!(f, "outside limits: {err}"),
        }
    }
}

/// Validates and decodes one record.
///
/// `record` is a [`Record`]: a copy the consumer has already taken out of
/// shared memory, so nothing here can change while it is being checked
/// (copy, then validate).
#[must_use]
pub fn receive(record: &Record<RING_SLOT>, limits: &InputLimits) -> Received {
    let header = match record.header() {
        Ok(header) => header,
        Err(err) => return Received::Rejected(Rejection::Malformed(err)),
    };
    match header.kind {
        INPUT_EVENT_KIND => match record.decode::<InputEvent>() {
            Ok(event) => match limits.validate(event) {
                Ok(event) => Received::Event(event),
                Err(err) => Received::Rejected(Rejection::OutsideLimits(err)),
            },
            Err(err) => Received::Rejected(Rejection::Malformed(err)),
        },
        SEQUENCE_END_KIND => match record.decode::<SequenceEnd>() {
            Ok(end) => Received::End(end),
            Err(err) => Received::Rejected(Rejection::Malformed(err)),
        },
        other => Received::Rejected(Rejection::UnknownKind(other)),
    }
}

/// The consumer's running count of accepted and rejected records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Valid events accepted.
    pub accepted: u32,
    /// Records rejected.
    pub rejected: u32,
}

impl Tally {
    /// Counts one received record (the end marker is not counted).
    pub fn count(&mut self, received: &Received) {
        match received {
            Received::Event(_) => self.accepted = self.accepted.saturating_add(1),
            Received::Rejected(_) => self.rejected = self.rejected.saturating_add(1),
            Received::End(_) => {}
        }
    }

    /// Records counted.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.accepted.saturating_add(self.rejected)
    }
}

/// The consumer's report for record number `number` (counting from 0), after
/// it has been counted in `tally`. Displayed after [`CONSUMER_TAG`].
#[must_use]
pub const fn report<'a>(number: u32, received: &'a Received, tally: &'a Tally) -> Report<'a> {
    Report {
        number,
        received,
        tally,
    }
}

/// See [`report`].
#[derive(Debug, Clone, Copy)]
pub struct Report<'a> {
    number: u32,
    received: &'a Received,
    tally: &'a Tally,
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let number = self.number;
        match self.received {
            Received::Event(event) => write!(f, "record {number}: {event:?}"),
            Received::Rejected(why) => write!(f, "record {number} rejected: {why}"),
            Received::End(end) if end.records == self.tally.total() => write!(
                f,
                "end of sequence: {} records announced, {} accepted, {} rejected",
                end.records, self.tally.accepted, self.tally.rejected
            ),
            Received::End(end) => write!(
                f,
                "end of sequence MISMATCH: {} records announced, {} received",
                end.records,
                self.tally.total()
            ),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::format;
    use std::string::{String, ToString};
    use std::vec::Vec;

    use nowayos_ipc::{Consumer, InMemoryRing};

    use super::*;

    /// Runs the whole sequence through a ring of the demonstration's
    /// geometry, batch by batch as the PDs do, and returns the consumer's
    /// report lines and the batch sizes.
    fn run_demo() -> (Vec<String>, Vec<usize>) {
        let ring: InMemoryRing<RING_SLOTS, RING_SLOT> = InMemoryRing::new();
        let limits = screen_limits().unwrap();
        let mut next = 0;
        let mut number = 0u32;
        let mut tally = Tally::default();
        let mut lines = Vec::new();
        let mut batches = Vec::new();
        while next < SEQUENCE.len() {
            // Producer: send until the ring is full (a new Producer each
            // time, as the PD does on each notification).
            let mut tx = Producer::new(&ring).unwrap();
            let first = next;
            while let Some(record) = SEQUENCE.get(next) {
                match send(&mut tx, record) {
                    Ok(()) => next += 1,
                    Err(SendError::Full) => break,
                    Err(err) => panic!("send failed: {err}"),
                }
            }
            batches.push(next - first);
            // Consumer: drain.
            let mut rx = Consumer::new(&ring).unwrap();
            while let Some(record) = rx.try_recv_record().unwrap() {
                let received = receive(&record, &limits);
                tally.count(&received);
                lines.push(format!(
                    "{CONSUMER_TAG} {}",
                    report(number, &received, &tally)
                ));
                number += 1;
            }
        }
        (lines, batches)
    }

    #[test]
    fn the_sequence_crosses_the_ring_in_three_batches() {
        let (lines, batches) = run_demo();
        assert_eq!(batches, [4, 4, 3]);
        assert_eq!(lines.len(), SEQUENCE.len());
        assert_eq!(
            lines[5],
            "NOWAYOS: ipc-consumer: record 5 rejected: malformed: invalid value in field `tag`"
        );
        assert_eq!(
            lines[10],
            "NOWAYOS: ipc-consumer: end of sequence: 10 records announced, 9 accepted, 1 rejected"
        );
    }

    #[test]
    fn the_consumer_lines_match_the_qemu_expect_file() {
        // The lines tools/qemu-test checks for in QEMU, in order.
        let expect = include_str!("../../../systems/ipc-demo.expect");
        let expected: Vec<&str> = expect
            .lines()
            .filter(|line| line.starts_with(CONSUMER_TAG))
            .collect();
        let (lines, _) = run_demo();
        let starting: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|line| !line.contains(" record ") && !line.contains("end of sequence"))
            .collect();
        let reports: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|line| line.contains(" record ") || line.contains("end of sequence"))
            .collect();
        assert_eq!(starting.len(), 1, "one start-up line: {starting:?}");
        assert_eq!(reports, lines);
    }

    #[test]
    fn records_before_end_matches_the_sequence() {
        let position = SEQUENCE.iter().position(|r| *r == DemoRecord::End).unwrap();
        assert_eq!(position, SEQUENCE.len() - 1);
        assert_eq!(RECORDS_BEFORE_END as usize, position);
    }

    #[test]
    fn every_event_in_the_sequence_is_valid() {
        let limits = screen_limits().unwrap();
        for record in SEQUENCE {
            if let DemoRecord::Event(event) = record {
                assert_eq!(limits.validate(event), Ok(event));
            }
        }
    }

    #[test]
    fn the_malformed_record_has_a_valid_header_and_a_bad_tag() {
        let bytes = malformed_event_bytes().unwrap();
        let header = nowayos_ipc::Header::parse(&bytes).unwrap();
        assert_eq!(header.kind, INPUT_EVENT_KIND);
        assert_eq!(usize::from(header.payload_len), InputEvent::PAYLOAD_LEN);
        assert_eq!(bytes[HEADER_LEN], MALFORMED_TAG);
        assert_eq!(
            nowayos_ipc::decode::<InputEvent>(&bytes),
            Err(DecodeError::InvalidValue { field: "tag" })
        );
    }

    /// Sends raw bytes and returns what the consumer makes of them.
    fn receive_bytes(bytes: &[u8]) -> Received {
        let ring: InMemoryRing<RING_SLOTS, RING_SLOT> = InMemoryRing::new();
        Producer::new(&ring).unwrap().try_send_bytes(bytes).unwrap();
        let record = Consumer::new(&ring)
            .unwrap()
            .try_recv_record()
            .unwrap()
            .unwrap();
        receive(&record, &screen_limits().unwrap())
    }

    #[test]
    fn other_bad_records_are_rejected_with_a_reason() {
        // An event off the 800 x 480 screen.
        let mut buf = [0u8; RING_SLOT];
        encode(&InputEvent::PointerMove { x: 900, y: 10 }, &mut buf).unwrap();
        let received = receive_bytes(&buf);
        assert_eq!(
            received,
            Received::Rejected(Rejection::OutsideLimits(InputError::OutsideScreen {
                x: 900,
                y: 10
            }))
        );
        // A kind this channel does not carry.
        let received = receive_bytes(&[0x34, 0x12, 1, 0, 0, 0, 0, 0]);
        assert_eq!(received, Received::Rejected(Rejection::UnknownKind(0x1234)));
        assert_eq!(
            Rejection::UnknownKind(0x1234).to_string(),
            "unknown message kind 0x1234"
        );
        // An end marker of a version this consumer does not read.
        let received = receive_bytes(&[0x00, 0x7F, 2, 0, 4, 0, 0, 0, 10, 0, 0, 0]);
        assert_eq!(
            received,
            Received::Rejected(Rejection::Malformed(DecodeError::UnsupportedVersion {
                expected: 1,
                found: 2
            }))
        );
        // An all-zero slot: kind 0 is not a kind this channel carries.
        assert_eq!(
            receive_bytes(&[0; RING_SLOT]),
            Received::Rejected(Rejection::UnknownKind(0))
        );
    }

    #[test]
    fn a_short_end_count_is_reported_as_a_mismatch() {
        let tally = Tally {
            accepted: 3,
            rejected: 0,
        };
        let received = Received::End(SequenceEnd { records: 4 });
        assert_eq!(
            report(4, &received, &tally).to_string(),
            "end of sequence MISMATCH: 4 records announced, 3 received"
        );
    }
}
