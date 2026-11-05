// SPDX-License-Identifier: MIT OR Apache-2.0
//! The frame bench on the host (#10077): each screen at each size and
//! depth, its steps timed in microseconds, and a frame's copy - what the
//! display PD does with each frame. Run it optimised:
//! `cargo run --release -p nowayos-frame-times --example host`.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a host measurement: a failure stops it, and it prints its table"
)]

use std::time::Instant;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::{DeviceScale, FramebufferLayout, Rect};
use nowayos_frame_times::{FORMATS, SIZES, Screen, Which, build, time};
use nowayos_render_eg::{DEFAULT_BYTES, DEFAULT_SLOTS, Glyphs, RawFramebuffer, Slot, Theme};

fn main() {
    let started = Instant::now();
    let mut clock = || u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let mut memory = vec![0_u8; nowayos_frame_times::MOST_FRAME];
    let mut copy = vec![0_u8; nowayos_frame_times::MOST_FRAME];
    let mut tree: Box<Screen> = Box::default();
    println!(
        "frame-bench (host): microseconds; first and full redraws, a scroll step, a press, a slot change; a frame's copy"
    );
    for (width, height, size, quarters) in SIZES {
        let scale = DeviceScale::from_quarters(quarters).unwrap();
        let viewport = Rect::new(0, 0, width, height);
        for (format, depth) in FORMATS {
            let layout = FramebufferLayout::packed(width, height, format).unwrap();
            let bytes = layout.stride() as usize * height as usize;
            for which in Which::ALL {
                let (mut pixels, mut slots) =
                    (vec![0; DEFAULT_BYTES], vec![Slot::EMPTY; DEFAULT_SLOTS]);
                let mut glyphs = Glyphs::new(&mut pixels, &mut slots);
                let mut target = RawFramebuffer::new(layout, &mut memory[..bytes]).unwrap();
                // The framebuffer stores each pixel in its layout's format.
                let theme = Theme::<Rgb888>::light();
                build(&mut tree, which, &theme, viewport, scale).unwrap();
                let times = time(&mut tree, &mut target, &theme, &mut glyphs, &mut clock).unwrap();
                let before = clock();
                copy[..bytes].copy_from_slice(std::hint::black_box(&memory[..bytes]));
                std::hint::black_box(&copy);
                let copied = clock() - before;
                let us = |ns: u64| ns / 1000;
                println!(
                    "{size} {depth} {}: first {} full {} scroll {} press {} slot {} copy {}",
                    which.name(),
                    us(times.first),
                    us(times.full),
                    us(times.scroll),
                    us(times.press),
                    us(times.slot),
                    us(copied),
                );
            }
        }
    }
}
