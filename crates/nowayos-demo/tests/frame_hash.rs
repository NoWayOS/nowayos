// SPDX-License-Identifier: MIT OR Apache-2.0
//! The display stub in `systems/ui-demo` prints the hash of every frame the
//! UI PD drew in QEMU as the input PD played the session;
//! `systems/ui-demo.expect` says which hash each must be. This test computes
//! them from the approved golden images of the session's steps, so the
//! chain is closed: a changed golden image fails here, and a frame drawn
//! differently in QEMU fails `make qemu-test SYSTEM=ui-demo`.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics"
)]

use std::path::{Path, PathBuf};

use embedded_graphics::pixelcolor::RgbColor;
use nowayos_demo::{HEIGHT, SESSION, WIDTH};
use nowayos_display::{FrameHash, FramebufferLayout, PixelFormat};
use nowayos_testkit::Framebuffer;

/// The frame the UI PD draws for golden image `name`: the Pi 5's window,
/// 1920 x 1080, in the display's format, r5g6b5, packed. The
/// golden image holds the colours r5g6b5 gives back, so each converts to
/// exactly the bytes the UI stored.
fn frame(root: &Path, name: &str) -> Vec<u8> {
    let golden =
        Framebuffer::from_png(&std::fs::read(root.join("session").join(name)).unwrap()).unwrap();
    assert_eq!((golden.width(), golden.height()), (WIDTH, HEIGHT), "{name}");
    let layout = FramebufferLayout::packed(WIDTH, HEIGHT, PixelFormat::Rgb565).unwrap();
    let mut frame = vec![0_u8; layout.byte_len()];
    for y in 0..layout.height() {
        for x in 0..layout.width() {
            let pixel = golden.pixel(x, y).unwrap();
            assert!(layout.write_pixel(&mut frame, x, y, (pixel.r(), pixel.g(), pixel.b())));
            let at = layout.pixel_offset(x, y).unwrap();
            assert_eq!(
                PixelFormat::Rgb565.decode(&frame[at..]),
                Some((pixel.r(), pixel.g(), pixel.b())),
                "{name}: ({x}, {y}) is not a colour r5g6b5 gives back"
            );
        }
    }
    frame
}

/// The hash of golden image `name` as the display PD sees it.
fn golden_hash(root: &Path, name: &str) -> u64 {
    FrameHash::of(&frame(root, name))
}

/// The hash of what the Pi 5's framebuffer window holds once the display
/// PD has shown golden image `name`: the frame at the
/// monitor's own pixels, 1080 rows of 3840 bytes from the window's first
/// byte - screen row 0 - which the display PD reads back and hashes.
fn scan_out_hash(root: &Path, name: &str) -> u64 {
    let frame = frame(root, name);
    let mut hash = FrameHash::new();
    for row in frame.chunks_exact(3840) {
        hash.update(row);
    }
    hash.finish()
}

#[test]
fn the_ui_demo_on_the_pi_expects_the_scan_out_hash_of_every_golden_step() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expect =
        std::fs::read_to_string(root.join("../../systems/rpi5b_2gb/ui-demo.expect")).unwrap();
    let frames: Vec<&str> = expect
        .lines()
        .filter(|line| line.contains(" on the framebuffer, scan-out hash "))
        .collect();
    assert_eq!(frames.len(), SESSION.len(), "one frame per step");
    for (number, (step, line)) in SESSION.iter().zip(frames).enumerate() {
        let hash = scan_out_hash(&root, &format!("{number:02}-{}.png", step.name));
        assert_eq!(
            line,
            format!(
                "NOWAYOS: display-stub: frame {number} on the framebuffer, scan-out hash {hash:#018x}"
            ),
            "systems/rpi5b_2gb/ui-demo.expect, step {number} ({})",
            step.name
        );
    }
}

#[test]
fn the_ui_demo_expects_the_hash_of_every_golden_step() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expect = std::fs::read_to_string(root.join("../../systems/ui-demo.expect")).unwrap();
    let frames: Vec<&str> = expect
        .lines()
        .filter(|line| line.starts_with("NOWAYOS: display-stub: frame "))
        .collect();
    assert_eq!(frames.len(), SESSION.len(), "one frame per step");
    for (number, (step, line)) in SESSION.iter().zip(frames).enumerate() {
        let hash = golden_hash(&root, &format!("{number:02}-{}.png", step.name));
        assert_eq!(
            line,
            format!("NOWAYOS: display-stub: frame {number} hash {hash:#018x}"),
            "systems/ui-demo.expect, step {number} ({})",
            step.name
        );
    }
}

/// `systems/ui-interactive.qmp` plays the same session with QEMU's keyboard
/// and tablet: after each step it waits for the display to show the
/// step's golden image - by this hash - and then compares QEMU's screen
/// with that image. This test ties each wait to its image, in session order,
/// and the system's final approved screen to the last step's image.
#[test]
fn the_interactive_demo_waits_for_and_compares_every_golden_step() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("../../systems/ui-interactive.qmp")).unwrap();
    let steps: Vec<&str> = script
        .lines()
        .filter(|line| line.starts_with("wait ") || line.starts_with("screen "))
        .collect();
    assert_eq!(
        steps.len(),
        SESSION.len().saturating_mul(2),
        "a wait and a screen per step"
    );
    for (number, (step, pair)) in SESSION.iter().zip(steps.chunks(2)).enumerate() {
        let name = format!("{number:02}-{}.png", step.name);
        let hash = golden_hash(&root, &name);
        assert_eq!(
            pair,
            [
                format!("wait NOWAYOS: display: showing {hash:#018x}").as_str(),
                format!("screen ../crates/nowayos-demo/session/{name}").as_str(),
            ],
            "systems/ui-interactive.qmp, step {number}"
        );
    }
    let last = format!(
        "{:02}-{}.png",
        SESSION.len().saturating_sub(1),
        SESSION.last().unwrap().name
    );
    let image = |path: PathBuf| Framebuffer::from_png(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        image(root.join("../../systems/ui-interactive.screen.png")),
        image(root.join("session").join(last)),
        "systems/ui-interactive.screen.png shows the last step's golden image"
    );
}
