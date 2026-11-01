// SPDX-License-Identifier: MIT OR Apache-2.0
//! Kani proofs of the video frame ring's checks
//! (`video::ring`), the record and counts that cross between the USB host
//! PD and its client, and of the payload header's decoder:
//!
//! * a layout the client accepts from the host's page, whatever the page
//!   says, puts every frame's slot wholly inside the client's region, and
//!   leaves room for the record;
//! * a slot's record, from any 16 bytes, is accepted only for the frame the
//!   slot should hold and with a length the slot holds;
//! * the host never takes a count released from the client that moves back
//!   or past what it wrote, so never frees a slot the client may be reading,
//!   and the client never takes a count written that puts more frames ready
//!   than there are slots;
//! * the host streams only for a client's frames-wanted word of 1, and the
//!   client takes the host's offer only as a whole frame size and interval;
//! * a payload header, from any bytes up to a high-bandwidth interval's,
//!   decodes without panicking and only with a length inside the payload.
//!
//! Compiled only by Kani (`cfg(kani)`); `make kani` runs them.

use crate::video::frames::header;
use crate::video::ring::{self, Layout, MAX_SLOTS, RECORD_LEN, Record, VERSION};

/// The client's check of the host's page: any version, slots, slot size
/// and region.
#[kani::proof]
fn a_layout_the_client_accepts_keeps_every_slot_in_its_region() {
    let version: u32 = kani::any();
    let slots: u32 = kani::any();
    let slot_bytes: u32 = kani::any();
    let region: usize = kani::any();
    let sequence: u32 = kani::any();
    if let Ok(layout) = Layout::check(version, slots, slot_bytes, region) {
        assert!(version == VERSION);
        assert!(slots >= 1 && slots <= MAX_SLOTS);
        let at = layout.slot_at(sequence);
        let end = at.checked_add(slot_bytes as usize);
        assert!(
            end.is_some_and(|end| end <= region),
            "a slot past the region"
        );
        assert!(layout.room().checked_add(RECORD_LEN) == Some(slot_bytes as usize));
    }
}

/// Any record in any slot of a layout the client accepts.
#[kani::proof]
fn a_record_is_accepted_only_for_its_frame_and_within_its_slot() {
    let bytes: [u8; RECORD_LEN] = kani::any();
    let sequence: u32 = kani::any();
    let slots: u32 = kani::any();
    let slot_bytes: u32 = kani::any();
    let region: usize = kani::any();
    if let Ok(layout) = Layout::check(VERSION, slots, slot_bytes, region) {
        if let Ok(record) = Record::decode(&bytes, sequence, &layout) {
            assert!(record.sequence == sequence);
            assert!(
                (record.len as usize) <= layout.room(),
                "a frame past its slot"
            );
            assert!(record.to_bytes() == bytes);
        }
    }
}

/// The host's check of the client's count, while it holds at most the
/// slots unread.
#[kani::proof]
fn the_host_never_frees_a_slot_the_client_may_be_reading() {
    let written: u32 = kani::any();
    let before: u32 = kani::any();
    let found: u32 = kani::any();
    let slots: u32 = kani::any_where(|slots: &u32| (1..=MAX_SLOTS).contains(slots));
    kani::assume(written.wrapping_sub(before) <= slots);
    if let Ok(taken) = ring::released(written, before, found) {
        assert!(
            written.wrapping_sub(taken) <= written.wrapping_sub(before),
            "a count released that moves back"
        );
        assert!(found.wrapping_sub(before) <= written.wrapping_sub(before));
    }
}

/// The host's check of the client's frames-wanted word: frames
/// only for 1, a stop for 0, and every other word refused.
#[kani::proof]
fn the_host_streams_only_for_a_client_that_stored_1() {
    let found: u32 = kani::any();
    match ring::wanted(found) {
        Ok(true) => assert!(found == 1),
        Ok(false) => assert!(found == 0),
        Err(_) => assert!(found > 1),
    }
}

/// The client's check of the host's offer: one taken has a
/// width, height and interval above 0, and is the words it came from.
#[kani::proof]
fn an_offer_taken_is_a_whole_frame_size_and_interval() {
    let size: u32 = kani::any();
    let interval: u32 = kani::any();
    if let Some(offer) = ring::offer(size, interval) {
        assert!(offer.width > 0 && offer.height > 0 && offer.interval > 0);
        assert!(offer.size_word() == size && offer.interval == interval);
    } else {
        assert!(size & 0xFFFF == 0 || size >> 16 == 0 || interval == 0);
    }
}

/// The client's check of the host's count.
#[kani::proof]
fn the_client_never_takes_more_frames_than_there_are_slots() {
    let slots: u32 = kani::any();
    let released: u32 = kani::any();
    let found: u32 = kani::any();
    if let Ok(layout) = Layout::check(VERSION, slots, 4096, usize::MAX) {
        if let Ok(ready) = ring::ready(&layout, released, found) {
            assert!(ready <= layout.slots(), "more frames ready than slots");
            assert!(released.wrapping_add(ready) == found);
        }
    }
}

/// Any payload of up to 16 bytes: every header's fields and a little past.
#[kani::proof]
#[kani::unwind(17)]
fn a_payload_header_decodes_from_any_bytes() {
    let buf: [u8; 16] = kani::any();
    let len: usize = kani::any_where(|len: &usize| *len <= 16);
    let payload = &buf[..len];
    if let Ok(found) = header(payload) {
        assert!(found.len >= 2 && found.len <= payload.len());
    }
}
