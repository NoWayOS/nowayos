// SPDX-License-Identifier: MIT OR Apache-2.0
//! The frame bench (#10077): `nowayos-frame-bench`'s two screens - a
//! launcher and an article - drawn at 1920 x 1080, 2560 x 1440 and
//! 3840 x 2160, in 16 and 32 bits a pixel, into a framebuffer-sized
//! region: a first and a second full redraw, a scroll step, a press and a
//! slot change, each timed by the CPU's counter (`nowayos-hw`); then the
//! frame copied to a second region in pages, loaded and stored as the
//! display PD copies a frame to the screen. One core. A measurement for
//! the board; QEMU's times are an emulator's.
//!
//! The glyph store and the tree live in `init`'s frame: the system gives
//! the PD a `stack_size` of 0x800000.

#![no_std]
#![no_main]

use nowayos_display::{DeviceScale, FrameMemory, FramebufferLayout, Rect};
use nowayos_frame_times::{FORMATS, MOST_FRAME, SIZES, Screen, Which, build, time};
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_render_eg::{
    DEFAULT_BYTES, DEFAULT_SLOTS, Glyphs, RawFramebuffer, Rgb888, Slot, Theme,
};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// Bytes copied at a time, as the display PD copies a frame.
const PAGE: usize = 4096;

#[protection_domain]
fn init() -> Bench {
    debug_println!("NOWAYOS: frame-bench: starting");
    let mut pixels = [0_u8; DEFAULT_BYTES];
    let mut slots = [Slot::EMPTY; DEFAULT_SLOTS];
    let mut tree = Screen::new();
    match run(&mut tree, &mut pixels, &mut slots) {
        Ok(()) => debug_println!("NOWAYOS: frame-bench: done"),
        Err(why) => debug_println!("NOWAYOS: frame-bench: failed: {why}"),
    }
    Bench
}

/// Every screen at every size and depth, then each size's copy.
fn run(tree: &mut Screen, pixels: &mut [u8], slots: &mut [Slot]) -> Result<(), &'static str> {
    let mut frame = nowayos_region::read_write(0)
        .and_then(|region| region.into_frame(MOST_FRAME))
        .map_err(|_| "the frame region")?;
    let mut screen = nowayos_region::read_write(1)
        .and_then(|region| region.into_frame(MOST_FRAME))
        .map_err(|_| "the screen region")?;
    let mut clock = counter;
    let micros = |ticks: u64| {
        ticks
            .saturating_mul(1_000_000)
            .checked_div(counter_frequency())
            .unwrap_or(0)
    };
    let theme = Theme::<Rgb888>::light();
    for (width, height, size, quarters) in SIZES {
        let scale = DeviceScale::from_quarters(quarters).ok_or("a scale")?;
        let viewport = Rect::new(0, 0, width, height);
        for (format, depth) in FORMATS {
            let layout =
                FramebufferLayout::packed(width, height, format).map_err(|_| "a layout")?;
            for which in Which::ALL {
                let mut glyphs = Glyphs::new(&mut *pixels, &mut *slots);
                let mut target = RawFramebuffer::new(layout, &mut frame).map_err(|_| "a target")?;
                build(tree, which, &theme, viewport, scale).map_err(|_| "a screen")?;
                let times = time(tree, &mut target, &theme, &mut glyphs, &mut clock)
                    .map_err(|_| "a step")?;
                debug_println!(
                    "NOWAYOS: frame-bench: {size} {depth} {}: first {} full {} scroll {} press {} slot {} us",
                    which.name(),
                    micros(times.first),
                    micros(times.full),
                    micros(times.scroll),
                    micros(times.press),
                    micros(times.slot),
                );
            }
            // The frame copied to the screen, a page at a time.
            let bytes = usize::try_from(layout.stride())
                .unwrap_or(0)
                .saturating_mul(usize::try_from(height).unwrap_or(0));
            let mut page = [0_u8; PAGE];
            let started = counter();
            let mut at = 0_usize;
            while at < bytes {
                let take = PAGE.min(bytes.saturating_sub(at));
                let into = page.get_mut(..take).ok_or("a page")?;
                if !frame.load(at, into) || !screen.store(at, into) {
                    return Err("a copy");
                }
                at = at.saturating_add(take);
            }
            debug_println!(
                "NOWAYOS: frame-bench: {size} {depth} copy: {} us",
                micros(counter().saturating_sub(started))
            );
        }
    }
    Ok(())
}

/// Nothing to do once measured.
struct Bench;

impl Handler for Bench {
    type Error = Infallible;
}
