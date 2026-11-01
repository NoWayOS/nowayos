// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the UI-to-display contract: random records never
//! panic either end and are decoded only in their one canonical form, valid
//! messages round-trip, and an accepted Present's byte ranges cover exactly
//! its rectangles' pixels and nothing outside the framebuffer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{
    Configure, DisplayEnd, FramebufferLayout, MAX_DAMAGE_RECTS, MAX_RECORD_LEN, PixelFormat, Point,
    Present, PresentDone, Rect, UiEnd, UiEvent,
};
use nowayos_ipc::{HEADER_LEN, Message, decode, encode};
use proptest::prelude::*;

fn format() -> impl Strategy<Value = PixelFormat> {
    prop_oneof![
        Just(PixelFormat::Rgb565),
        Just(PixelFormat::Rgb888),
        Just(PixelFormat::Bgr888),
        Just(PixelFormat::Xrgb8888),
        Just(PixelFormat::Xbgr8888),
    ]
}

/// Small layouts, some with padded rows.
fn layout() -> impl Strategy<Value = FramebufferLayout> {
    (1u32..24, 1u32..16, 0u32..8, format()).prop_map(|(w, h, pad, f)| {
        let stride = w * u32::from(f.bytes_per_pixel()) + pad;
        FramebufferLayout::new(w, h, stride, f).unwrap()
    })
}

/// A layout and up to the maximum number of non-empty rectangles inside it.
fn layout_and_damage() -> impl Strategy<Value = (FramebufferLayout, Vec<Rect>)> {
    layout().prop_flat_map(|l| {
        let (w, h) = (l.width(), l.height());
        let rect = (0..w, 0..h).prop_flat_map(move |(x, y)| {
            (1..=w - x, 1..=h - y).prop_map(move |(rw, rh)| Rect::new(x as i32, y as i32, rw, rh))
        });
        (Just(l), prop::collection::vec(rect, 0..=MAX_DAMAGE_RECTS))
    })
}

fn record<M: Message>(message: &M) -> [u8; MAX_RECORD_LEN] {
    let mut buf = [0u8; MAX_RECORD_LEN];
    encode(message, &mut buf).unwrap();
    buf
}

/// Replaces a valid record's payload with arbitrary bytes and checks the
/// decoder: no panic, and anything it accepts re-encodes to the same bytes.
fn canonical_or_refused<M: Message>(valid: &M, payload: &[u8]) -> Result<(), TestCaseError> {
    let mut buf = record(valid);
    buf[HEADER_LEN..HEADER_LEN + M::PAYLOAD_LEN].copy_from_slice(&payload[..M::PAYLOAD_LEN]);
    if let Ok(message) = decode::<M>(&buf) {
        prop_assert_eq!(record(&message), buf);
    }
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_payloads_never_panic_and_are_canonical(payload in any::<[u8; 72]>()) {
        let l = FramebufferLayout::packed(4, 4, PixelFormat::Rgb565).unwrap();
        canonical_or_refused(&Configure::new(l).unwrap(), &payload)?;
        canonical_or_refused(&Present::new(0, &[]).unwrap(), &payload)?;
        canonical_or_refused(&PresentDone { sequence: 0 }, &payload)?;
    }

    #[test]
    fn arbitrary_records_never_panic_either_end(
        bytes in prop::collection::vec(any::<u8>(), 0..=MAX_RECORD_LEN + 8),
        kind in 0x0200u16..0x0205,
        l in layout(),
    ) {
        let mut bytes = bytes;
        // Aim most records at a real kind so the payload checks run.
        if bytes.len() >= 2 {
            bytes[..2].copy_from_slice(&kind.to_le_bytes());
        }
        let mut ui = UiEnd::new(l.byte_len());
        let _ = ui.receive(&bytes);
        let mut display = DisplayEnd::new(l).unwrap();
        if let Ok(present) = display.receive(&bytes) {
            for span in present.spans() {
                prop_assert!(span.end <= l.byte_len());
            }
        }
    }

    #[test]
    fn valid_messages_round_trip(
        (l, damage) in layout_and_damage(),
        sequence in any::<u32>(),
    ) {
        let configure = Configure::new(l).unwrap();
        prop_assert_eq!(decode::<Configure>(&record(&configure)), Ok(configure));
        let present = Present::new(sequence, &damage).unwrap();
        prop_assert_eq!(decode::<Present>(&record(&present)), Ok(present.clone()));
        let done = PresentDone { sequence };
        prop_assert_eq!(decode::<PresentDone>(&record(&done)), Ok(done));
    }

    #[test]
    fn accepted_spans_cover_exactly_the_damage((l, damage) in layout_and_damage()) {
        let mut ui = UiEnd::new(l.byte_len());
        let mut display = DisplayEnd::new(l).unwrap();
        prop_assert_eq!(
            ui.receive(&record(&display.configure())),
            Ok(UiEvent::Configured(l))
        );
        let present = ui.present(&damage).unwrap();
        let accepted = display.receive(&record(&present)).unwrap();

        let source: Vec<u8> = (0..l.byte_len()).map(|i| (i % 255) as u8 + 1).collect();
        let mut target = vec![0u8; l.byte_len()];
        for span in accepted.spans() {
            prop_assert!(span.end <= l.byte_len());
            target[span.clone()].copy_from_slice(&source[span]);
        }
        let bpp = usize::from(l.format().bytes_per_pixel());
        for y in 0..l.height() {
            for x in 0..l.width() {
                let at = l.pixel_offset(x, y).unwrap();
                let point = Point::new(x as i32, y as i32);
                let damaged = damage.iter().any(|r| r.contains(point));
                let copied = target[at..at + bpp] == source[at..at + bpp];
                prop_assert_eq!(copied, damaged, "pixel ({}, {})", x, y);
            }
        }
        prop_assert_eq!(
            ui.receive(&record(&accepted.done())),
            Ok(UiEvent::PresentDone(present.sequence()))
        );
    }

    #[test]
    fn honest_ends_stay_in_step(
        frames in prop::collection::vec(layout_and_damage(), 1..12),
    ) {
        let l = FramebufferLayout::packed(24, 16, PixelFormat::Xrgb8888).unwrap();
        let mut ui = UiEnd::new(l.byte_len());
        let mut display = DisplayEnd::new(l).unwrap();
        ui.receive(&record(&display.configure())).unwrap();
        for (_, damage) in frames {
            // Every generated rectangle fits a 24 x 16 layout.
            let present = ui.present(&damage).unwrap();
            prop_assert!(!ui.can_draw());
            let accepted = display.receive(&record(&present)).unwrap();
            prop_assert_eq!(accepted.damage(), present.damage());
            ui.receive(&record(&accepted.done())).unwrap();
            prop_assert!(ui.can_draw());
        }
    }
}
