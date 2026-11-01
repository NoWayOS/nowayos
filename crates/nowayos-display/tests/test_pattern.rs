// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Pi 5's framebuffer test (`systems/fb-test.expect`) prints the
//! hash of the display test pattern and of every byte it wrote to show it at
//! 2x in r5g6b5. This test computes both from `test_pattern`, `scale_row`
//! and `Placement`, as the PD does, so a pattern drawn or converted
//! differently on the board fails `check-serial`, and a change here fails
//! this test until the expect file follows.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics"
)]

use std::path::PathBuf;

use nowayos_display::{
    FrameHash, PixelFormat, Placement, Size, TEST_PATTERN, scale_row, test_pattern,
};

/// The pattern's hash as Xrgb8888 rows, and the hash of the r5g6b5 rows
/// showing it on a 1920 x 1080 screen: each scaled row, as often as the
/// scale says.
fn hashes() -> (u64, u64, Placement) {
    let placement = Placement::centred(TEST_PATTERN, Size::new(1920, 1080)).unwrap();
    let (mut pattern, mut written) = (FrameHash::new(), FrameHash::new());
    let mut row = vec![0u8; TEST_PATTERN.width as usize * 4];
    let mut scaled = vec![0u8; placement.size().width as usize * 2];
    for y in 0..TEST_PATTERN.height {
        for (x, pixel) in (0u32..).zip(row.chunks_exact_mut(4)) {
            let (r, g, b) = test_pattern(x, y);
            pixel.copy_from_slice(&[b, g, r, 0]);
        }
        pattern.update(&row);
        let len = scale_row(
            PixelFormat::Xrgb8888,
            &row,
            PixelFormat::Rgb565,
            placement.scale(),
            &mut scaled,
        )
        .unwrap();
        assert_eq!(len, scaled.len());
        for _ in 0..placement.scale() {
            written.update(&scaled);
        }
    }
    (pattern.finish(), written.finish(), placement)
}

#[test]
fn the_pi_5s_framebuffer_test_expects_the_patterns_hashes() {
    let (pattern, written, placement) = hashes();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expect = std::fs::read_to_string(root.join("../../systems/fb-test.expect")).unwrap();
    let lines: Vec<&str> = expect.lines().collect();
    let size = placement.size();
    for wanted in [
        format!(
            "NOWAYOS: fb-test: the {}x{} test pattern, hash {pattern:#018x}, shown at {}x at ({}, {})",
            TEST_PATTERN.width,
            TEST_PATTERN.height,
            placement.scale(),
            placement.x(),
            placement.y()
        ),
        format!(
            "NOWAYOS: fb-test: wrote {}x{} r5g6b5 pixels, hash {written:#018x}; read back the same",
            size.width, size.height
        ),
    ] {
        assert!(
            lines.contains(&wanted.as_str()),
            "systems/fb-test.expect lacks: {wanted}"
        );
    }
}
