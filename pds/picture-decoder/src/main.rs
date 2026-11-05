// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-picture-decoder`: the picture decoder PD (#10084).
//!
//! A picture received at run time is hostile, so it is decoded here, in a
//! PD that holds nothing else: no device, no secret, no other channel. Its
//! client writes a PNG or a JPEG into the encoded region and sends a
//! `Decode` on the request ring. The decoder copies the bytes into its own
//! heap before it reads one, decodes them within bounds
//! (`nowayos-picture-decode`), writes the pixels into the pixel region and
//! answers with a `Decoded`: the picture's size, or why it was refused.
//!
//! Its heap is a memory region of its own, bounded: a picture that needs
//! more stops the PD, as any allocation that fails does, and its parent
//! restarts it at its restart entry (`nowayos-restart`) - the client is
//! never restarted. At each start it begins a new restart epoch on both
//! rings and drops the requests in flight, so the client knows which went
//! unanswered.
//!
//! Regions:
//!
//! | Symbol | Region | Map |
//! | --- | --- | --- |
//! | `nowayos_rw_region_0` | the request ring's control | `rw` |
//! | `nowayos_ro_region_0` | the request ring's messages | `r` |
//! | `nowayos_rw_region_1` | the answer ring's messages | `rw` |
//! | `nowayos_ro_region_1` | the answer ring's control | `r` |
//! | `nowayos_ro_region_2` | the encoded picture, at most `MAX_ENCODED` bytes | `r` |
//! | `nowayos_rw_region_2` | the pixels, `PIXEL_BYTES` | `rw` |
//! | `nowayos_heap_region` | its heap | `rw` |

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::alloc::Layout;

use nowayos_display::FrameMemory;
use nowayos_ipc::{Consumer, Message, Producer};
use nowayos_picture_wire::{Decode, Decoded, MAX_ENCODED, PIXEL_BYTES};
use nowayos_region::{RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer};
use nowayos_restart::{Start, started};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

nowayos_heap::declare_region_heap!(HEAP, Some(report_refusal));

/// Start of every line this program prints.
const TAG: &str = "NOWAYOS: picture-decoder:";

/// The channel to the client.
const CLIENT: Channel = Channel::new(0);

/// Both rings: slots, and bytes per slot.
const SLOTS: usize = 4;
const SLOT: usize = 32;

/// The heap refused a request: say so. The allocation error that follows
/// stops the PD, and its parent restarts it.
fn report_refusal(layout: Layout, in_use: usize) {
    debug_println!("{TAG} its heap refused an allocation: it stops");
    debug_println!("{TAG} {} bytes asked for, {in_use} in use", layout.size());
}

#[protection_domain]
fn init() -> Decoder {
    let how = match started() {
        Start::ColdChecked => "started",
        Start::Restarted => "restarted",
        Start::Cold | Start::NotThroughEntry => "started without its restart entry",
    };
    let Ok(heap) = nowayos_heap::give_region(&HEAP) else {
        debug_println!("{TAG} gave up: its heap is not mapped as it expects");
        return Decoder::Idle;
    };
    let rings = read_write(0)
        .and_then(|control| ring_consumer(control, read_only(0)?))
        .and_then(|requests| Ok((requests, ring_producer(read_write(1)?, read_only(1)?)?)));
    let regions = read_only(2)
        .and_then(|region| region.into_frame(MAX_ENCODED as usize))
        .and_then(|encoded| Ok((encoded, read_write(2)?.into_frame(PIXEL_BYTES)?)));
    let (Ok((requests, answers)), Ok((encoded, pixels))) = (rings, regions) else {
        debug_println!("{TAG} gave up: its regions are not mapped as it expects");
        return Decoder::Idle;
    };
    let (Ok(mut rx), Ok(mut tx)) = (Consumer::new(&requests), Producer::new(&answers)) else {
        debug_println!("{TAG} gave up: a ring is broken");
        return Decoder::Idle;
    };
    let epoch = rx.begin_epoch();
    tx.begin_epoch();
    let dropped = rx.skip_in_flight().unwrap_or(0);
    debug_println!(
        "{TAG} {how}; heap {heap} bytes; restart epoch {epoch}; dropped {dropped} request(s) in flight"
    );
    CLIENT.notify();
    Decoder::Serving(Serving {
        requests,
        answers,
        encoded,
        pixels,
    })
}

/// The decoder: serving, or given up.
enum Decoder {
    Serving(Serving),
    /// Could not start (it said why).
    Idle,
}

/// Its ends of the rings, and its views of the two regions.
struct Serving {
    requests: RingRx<SLOTS, SLOT>,
    answers: RingTx<SLOTS, SLOT>,
    encoded: nowayos_region::Frame<nowayos_region::ReadOnly>,
    pixels: nowayos_region::Frame<nowayos_region::ReadWrite>,
}

impl Handler for Decoder {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if let Self::Serving(serving) = self
            && channels.contains(CLIENT)
        {
            serving.serve();
        }
        Ok(())
    }
}

impl Serving {
    fn serve(&mut self) {
        let Self {
            requests,
            answers,
            encoded,
            pixels,
        } = self;
        let (Ok(mut rx), Ok(mut tx)) = (Consumer::new(requests), Producer::new(answers)) else {
            debug_println!("{TAG} a ring is broken");
            return;
        };
        let mut answered = false;
        while let Ok(Some(record)) = rx.try_recv_record() {
            match record.header().map(|header| header.kind) {
                Ok(Decode::KIND) => {
                    let Ok(request) = record.decode::<Decode>() else {
                        debug_println!("{TAG} refused a malformed request");
                        continue;
                    };
                    let answer = decode(encoded, pixels, request);
                    answered |= tx.try_send(&answer).is_ok();
                }
                _ => debug_println!("{TAG} refused a record"),
            }
        }
        if answered {
            CLIENT.notify();
        }
    }
}

/// Decodes the picture `request` names: copied out of `encoded` first, so
/// the client cannot change it while it is read; its pixels into `pixels`.
fn decode(
    encoded: &nowayos_region::Frame<nowayos_region::ReadOnly>,
    pixels: &mut nowayos_region::Frame<nowayos_region::ReadWrite>,
    request: Decode,
) -> Decoded {
    {
        let mut bytes = vec![0_u8; usize::try_from(request.len).unwrap_or(0)];
        if !encoded.load(0, &mut bytes) {
            return Decoded {
                id: request.id,
                outcome: Err(nowayos_picture_wire::Refusal::TooLarge),
            };
        }
        let outcome = nowayos_picture_decode::decode(request.format, &bytes).and_then(|picture| {
            if pixels.store(0, &picture.rgba) {
                Ok((picture.width, picture.height))
            } else {
                Err(nowayos_picture_wire::Refusal::TooLarge)
            }
        });
        match outcome {
            Ok((width, height)) => {
                debug_println!("{TAG} picture {} decoded: {width}x{height}", request.id);
            }
            Err(refusal) => debug_println!("{TAG} picture {} refused: {refusal:?}", request.id),
        }
        Decoded {
            id: request.id,
            outcome,
        }
    }
}
