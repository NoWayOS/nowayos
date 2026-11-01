// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-fb-test`: spike S2 - can a display PD use the
//! framebuffer the Raspberry Pi 5's firmware sets up?
//!
//! It draws the display test pattern QEMU's ramfb display PD draws
//! (`nowayos_display::test_pattern`), with only the framebuffer's
//! acquisition changed: instead of giving QEMU a buffer through fw_cfg, it
//! takes the board's framebuffer - device 0, the window `firmware_fb` in
//! `platforms/rpi5b_2gb.toml`, with its facts in read-only region 5 - and
//! shows the 800 x 480 pattern on it at the largest whole-number scale that
//! fits, centred (`nowayos-fb`). It prints what the window held before it
//! drew, the pattern's hash, the hash of every byte it wrote, and the hash
//! of what it reads back from the window, which must be the same: then the
//! memory the display hardware scans out holds the pattern. Whether the
//! monitor shows it is for a person to see; the last line says what they
//! should see.
//!
//! It holds the window, uncached, and its facts; no interrupt, no channel
//! (role `display`). No `unsafe` code: the window's binding is
//! `nowayos-hw`'s.

#![no_std]
#![no_main]

use nowayos_display::{FrameHash, PixelFormat, TEST_PATTERN, TEST_PATTERN_BARS, test_pattern};
use nowayos_fb::{FbError, Screen, bind};
use nowayos_hw::Scanout;
use nowayos_hw::cpu::{counter, counter_frequency};
use sel4_microkit::{Handler, Infallible, debug_print, debug_println, protection_domain};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: fb-test:";

/// The device number the system description binds the window to.
const WINDOW: usize = 0;

/// Bytes of one row of the pattern, in Xrgb8888.
const ROW: usize = 3200;

#[protection_domain]
fn init() -> FbTestPd {
    debug_println!("{TAG} starting");
    if let Err(error) = run() {
        debug_println!("{TAG} gave up: {error}");
    }
    FbTestPd
}

fn run() -> Result<(), FbError> {
    let mut screen = bind(WINDOW, TEST_PATTERN, PixelFormat::Xrgb8888)?;
    let mode = screen.mode();
    debug_println!(
        "{TAG} the board's framebuffer: {}x{} r5g6b5, {} bytes a row, its window from row {}",
        mode.width(),
        mode.height(),
        mode.stride(),
        mode.first_row()
    );
    report_before(&mut screen)?;
    let start = counter();
    screen.clear()?;
    let placement = screen.placement();
    let (mut pattern, mut written) = (FrameHash::new(), FrameHash::new());
    let mut row = [0u8; ROW];
    for y in 0..TEST_PATTERN.height {
        for (x, pixel) in (0u32..).zip(row.chunks_exact_mut(4)) {
            let (red, green, blue) = test_pattern(x, y);
            pixel.copy_from_slice(&[blue, green, red, 0]);
        }
        pattern.update(&row);
        screen.show_row(y, &row, &mut written)?;
    }
    let drawn = counter().saturating_sub(start);
    debug_println!(
        "{TAG} the {}x{} test pattern, hash {:#018x}, shown at {}x at ({}, {})",
        TEST_PATTERN.width,
        TEST_PATTERN.height,
        pattern.finish(),
        placement.scale(),
        placement.x(),
        placement.y()
    );
    let mut back = FrameHash::new();
    screen.read_back(|row| back.update(row))?;
    let size = placement.size();
    if back.finish() == written.finish() {
        debug_println!(
            "{TAG} wrote {}x{} r5g6b5 pixels, hash {:#018x}; read back the same",
            size.width,
            size.height,
            written.finish()
        );
    } else {
        debug_println!(
            "{TAG} wrote {}x{} r5g6b5 pixels, hash {:#018x}; read back {:#018x}, which differs",
            size.width,
            size.height,
            written.finish(),
            back.finish()
        );
    }
    let ms = drawn
        .saturating_mul(1000)
        .checked_div(counter_frequency())
        .unwrap_or(0);
    debug_println!("{TAG} cleared the window and drew the pattern in {ms} ms");
    debug_print!("{TAG} on screen: a black border around eight colour bars, left to right");
    for (name, _) in TEST_PATTERN_BARS {
        debug_print!(" {name}");
    }
    debug_println!(", over a grey ramp from black at the left to white at the right");
    Ok(())
}

/// Says what the window held before this PD drew: what the firmware, U-Boot
/// or the kernel left there.
fn report_before(screen: &mut Screen<Scanout>) -> Result<(), FbError> {
    let (mut bytes, mut set) = (0usize, 0usize);
    screen.read_back(|row| {
        bytes = bytes.saturating_add(row.len());
        set = set.saturating_add(row.iter().filter(|&&byte| byte != 0).count());
    })?;
    debug_println!(
        "{TAG} before drawing, {set} of the {bytes} bytes where the pattern goes were not zero"
    );
    Ok(())
}

/// The PD: it has drawn, and has nothing to answer.
struct FbTestPd;

impl Handler for FbTestPd {
    type Error = Infallible;
}
