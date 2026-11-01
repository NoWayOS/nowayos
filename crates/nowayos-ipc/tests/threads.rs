// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ring between two real threads. On a weakly ordered host (AArch64,
//! including Apple silicon) a missing acquire/release shows up here as a
//! torn or stale record, which the checksum-like payload detects.
//!
//! A detection must fail the test promptly and say why, so failures travel
//! as values: either side that finds a problem raises a shared stop flag,
//! the other side sees it and stops too, and every wait has a deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nowayos_ipc::{
    Consumer, DecodeError, EncodeError, InMemoryRing, Message, Producer, Reader, RecvError,
    SendError, Writer,
};

/// A sequence number plus 20 bytes derived from it; any tearing between
/// the two is a decode error.
#[derive(Debug, PartialEq)]
struct Sequenced(u32);

impl Message for Sequenced {
    const KIND: u16 = 0x5E0;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 24;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u32(self.0)?;
        for k in 0..20u32 {
            out.write_u8(self.0.wrapping_mul(31).wrapping_add(k) as u8)?;
        }
        Ok(())
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let n = input.read_u32()?;
        for k in 0..20u32 {
            if input.read_u8()? != n.wrapping_mul(31).wrapping_add(k) as u8 {
                return Err(DecodeError::InvalidValue { field: "check" });
            }
        }
        Ok(Self(n))
    }
}

type Ring = InMemoryRing<8, 32>;

/// How long either side waits for the other before failing the run.
const DEADLINE: Duration = Duration::from_secs(60);

/// Sends `count` records in order. At `corrupt_at` it sends 32 bytes of
/// garbage instead — exactly what a torn record looks like to the consumer.
fn produce(
    ring: &Ring,
    count: u32,
    corrupt_at: Option<u32>,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    let mut tx = Producer::new(ring).map_err(|err| format!("producer: {err}"))?;
    for n in 0..count {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(format!(
                    "producer stopped at record {n}: the consumer failed"
                ));
            }
            if Instant::now() > deadline {
                return Err(format!("producer timed out at record {n}"));
            }
            let sent = if corrupt_at == Some(n) {
                tx.try_send_bytes(&[0xFF; 32])
            } else {
                tx.try_send(&Sequenced(n))
            };
            match sent {
                Ok(()) => break,
                Err(SendError::Full) => thread::yield_now(),
                Err(err) => return Err(format!("send failed: {err}")),
            }
        }
    }
    Ok(())
}

/// Receives `count` records and checks each is whole and in order.
fn consume(ring: &Ring, count: u32, stop: &AtomicBool, deadline: Instant) -> Result<(), String> {
    let mut rx = Consumer::new(ring).map_err(|err| format!("consumer: {err}"))?;
    let mut expected = 0;
    while expected < count {
        if Instant::now() > deadline {
            return Err(format!("consumer timed out waiting for record {expected}"));
        }
        match rx.try_recv::<Sequenced>() {
            Ok(Some(Sequenced(n))) if n == expected => expected += 1,
            Ok(Some(Sequenced(n))) => {
                return Err(format!(
                    "out of order or lost: got {n}, expected {expected}"
                ));
            }
            Ok(None) if stop.load(Ordering::Relaxed) => {
                return Err(format!("the producer stopped before record {expected}"));
            }
            Ok(None) => thread::yield_now(),
            Err(RecvError::Decode(err)) => {
                return Err(format!("torn record at {expected}: {err}"));
            }
            Err(err) => return Err(format!("receive failed: {err}")),
        }
    }
    Ok(())
}

/// Runs a producer thread and a consumer on this thread over one ring.
fn exchange(count: u32, corrupt_at: Option<u32>) -> Result<(), String> {
    let ring = Ring::new();
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + DEADLINE;
    thread::scope(|scope| {
        let producer = scope.spawn(|| {
            let result = produce(&ring, count, corrupt_at, &stop, deadline);
            if result.is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            result
        });
        let consumed = consume(&ring, count, &stop, deadline);
        if consumed.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        let produced = producer
            .join()
            .unwrap_or_else(|_| Err("the producer panicked".to_owned()));
        // The consumer's verdict comes first: it is the one that checks.
        consumed.and(produced)
    })
}

#[test]
fn producer_and_consumer_threads_agree() {
    assert_eq!(exchange(20_000, None), Ok(()));
}

#[test]
fn a_detected_torn_record_fails_promptly() {
    // Regression (review 2026-10-01): when the consumer detected a torn
    // record, the test hung instead of failing — the producer retried a
    // full ring for ever — and printed nothing under output capture.
    let started = Instant::now();
    let result = exchange(20_000, Some(100));
    assert!(
        matches!(&result, Err(why) if why.starts_with("torn record at 100")),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
}
