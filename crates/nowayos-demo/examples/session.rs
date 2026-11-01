// SPDX-License-Identifier: MIT OR Apache-2.0
//! `make demo`: runs the demo's scripted session on the host, prints each
//! action as it happens, and checks the frame after every step against its
//! golden image in `session/`.
//!
//! Each frame is drawn as the UI PD draws it on the Pi 5 and in QEMU:
//! the Pi 5's 1920 x 1064 window at device scale 2, into a frame
//! in the monitor's own format, r5g6b5 (`RawFramebuffer`), so a golden
//! image holds exactly the colours the monitor shows - each channel back
//! from 5 or 6 bits to 8 as `PixelFormat::decode` gives it.
//!
//! On a mismatch the actual frame is written beside the golden one as
//! `NN-STEP.actual.png`, and the run fails. After an intended change,
//! regenerate the images with `NOWAYOS_UPDATE_SNAPSHOTS=1 make demo` and
//! review them like any other change.

use std::path::PathBuf;
use std::process::ExitCode;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_demo::{Demo, HEIGHT, SCALE, SCREEN, SESSION, WIDTH, action_name};
use nowayos_display::{FramebufferLayout, PixelFormat};
use nowayos_render_eg::{RawFramebuffer, Theme, render};
use nowayos_testkit::{Framebuffer, check_snapshot};

/// The demo's tree drawn into an r5g6b5 frame of the screen's size, as the
/// UI PD draws it, and read back as 8 bits a channel.
fn draw(demo: &Demo, theme: &Theme<Rgb888>) -> Option<Framebuffer> {
    let layout = FramebufferLayout::packed(WIDTH, HEIGHT, PixelFormat::Rgb565).ok()?;
    let mut bytes = vec![0_u8; layout.byte_len()];
    let mut target = RawFramebuffer::new(layout, &mut bytes[..]).ok()?;
    if let Err(error) = render(demo.tree(), &mut target, theme) {
        match error {}
    }
    let mut pixels = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let at = layout.pixel_offset(x, y)?;
            let (r, g, b) = layout.format().decode(bytes.get(at..)?)?;
            pixels.push(Rgb888::new(r, g, b));
        }
    }
    Framebuffer::from_pixels(WIDTH, HEIGHT, pixels)
}

fn main() -> ExitCode {
    let theme: Theme<Rgb888> = Theme::light();
    let mut demo = match Demo::new(theme.metrics_at(SCALE), SCREEN) {
        Ok(demo) => demo,
        Err(error) => {
            eprintln!("demo: the first screen could not be built: {error}");
            return ExitCode::FAILURE;
        }
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("session");
    let mut failures = 0_u32;
    for (number, step) in SESSION.iter().enumerate() {
        for event in step.events {
            match demo.handle(*event) {
                Ok(Some(action)) => println!(
                    "demo: step {number:02} {}: action {}",
                    step.name,
                    action_name(action)
                ),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("demo: step {number:02} {}: {error}", step.name);
                    return ExitCode::FAILURE;
                }
            }
        }
        let Some(frame) = draw(&demo, &theme) else {
            eprintln!("demo: the frame could not be drawn");
            return ExitCode::FAILURE;
        };
        let golden = dir.join(format!("{number:02}-{}.png", step.name));
        match check_snapshot(&frame, &golden) {
            Ok(()) => println!(
                "demo: step {number:02} {}: frame matches {}",
                step.name,
                golden.display()
            ),
            Err(error) => {
                eprintln!("demo: step {number:02} {}: {error}", step.name);
                failures = failures.saturating_add(1);
            }
        }
    }
    if failures > 0 {
        eprintln!(
            "demo: FAIL - {failures} of {} frame(s) differ",
            SESSION.len()
        );
        return ExitCode::FAILURE;
    }
    println!(
        "demo: PASS - {} steps, every frame as approved",
        SESSION.len()
    );
    ExitCode::SUCCESS
}
