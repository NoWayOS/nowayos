// SPDX-License-Identifier: MIT OR Apache-2.0
//! The glyph bench: how long `nowayos-raster` takes to draw the two
//! themes' type scale - every size each draws at device scale 1, every
//! character - from the glyph outlines, at device scales 1, 1.25, 1.5, 2,
//! 3 and 4, timed by the CPU's counter (`nowayos-hw`). What it would take
//! to hold the same as atlases is the coverage it prints. A measurement
//! for the board as much as for QEMU, whose times are an emulator's.
//!
//! The rasteriser and the buffer it draws into live in `init`'s frame: the
//! system gives the PD a `stack_size` of 0x400000.

#![no_std]
#![no_main]

use nowayos_glyphs::{BOLD, DISPLAY, Face, REGULAR};
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_raster::Rasteriser;
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// Each size, in pixels per em, the light and large-text themes draw at
/// device scale 1, once, with its face.
fn type_scale() -> [(&'static Face, u32); 12] {
    [
        (&REGULAR, 14),
        (&REGULAR, 16),
        (&REGULAR, 18),
        (&REGULAR, 24),
        (&BOLD, 14),
        (&BOLD, 16),
        (&BOLD, 18),
        (&BOLD, 20),
        (&BOLD, 24),
        (&BOLD, 28),
        (&BOLD, 36),
        (&DISPLAY, 96),
    ]
}

/// The device scales measured, in quarters, and as they are printed.
const SCALES: [(u32, &str); 6] = [
    (4, "1"),
    (5, "1.25"),
    (6, "1.5"),
    (8, "2"),
    (12, "3"),
    (16, "4"),
];

#[protection_domain]
fn init() -> Bench {
    debug_println!("NOWAYOS: glyph-bench: starting");
    let mut rasteriser: Rasteriser<4096, 262_144> = Rasteriser::new();
    let mut out = [0_u8; 262_144];
    for (quarters, name) in SCALES {
        let started = counter();
        let (mut glyphs, mut bytes, mut failed) = (0_u64, 0_u64, 0_u64);
        for (face, size) in type_scale() {
            let size64 = size.saturating_mul(16).saturating_mul(quarters);
            let Some(scale) = face.outline_scale(size64) else {
                failed = failed.saturating_add(1);
                continue;
            };
            for outline in face.glyphs() {
                let placement = outline.placement(scale);
                match rasteriser.render(outline.path(), scale, placement, &mut out) {
                    Ok(()) => {
                        glyphs = glyphs.saturating_add(1);
                        bytes = bytes.saturating_add(placement.len() as u64);
                    }
                    Err(_) => failed = failed.saturating_add(1),
                }
            }
        }
        let ticks = counter().saturating_sub(started);
        let micros = ticks
            .saturating_mul(1_000_000)
            .checked_div(counter_frequency())
            .unwrap_or(0);
        let per_glyph = micros.saturating_mul(1000).checked_div(glyphs).unwrap_or(0);
        debug_println!(
            "NOWAYOS: glyph-bench: scale {name}: {glyphs} glyphs, {bytes} bytes of coverage, in {micros} us, {per_glyph} ns a glyph"
        );
        if failed > 0 {
            debug_println!("NOWAYOS: glyph-bench: failed to draw {failed} glyphs at scale {name}");
        }
    }
    debug_println!("NOWAYOS: glyph-bench: done");
    Bench
}

/// Nothing to do once measured.
struct Bench;

impl Handler for Bench {
    type Error = Infallible;
}
