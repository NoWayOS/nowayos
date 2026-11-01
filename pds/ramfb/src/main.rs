// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-ramfb`: the display PD for QEMU's `ramfb` device, chosen
//! as the QEMU device closest to the Pi 5's firmware
//! framebuffer: a linear framebuffer in memory, set up once, then drawn into.
//!
//! The PD draws the display test pattern (`nowayos_display::test_pattern`)
//! into its framebuffer - a DMA region,
//! one 2 MiB page so that it is physically contiguous - then tells QEMU
//! where the framebuffer is, in which format and size, by writing a 28-byte
//! configuration to fw_cfg's `etc/ramfb` file (`nowayos-hw`'s `ramfb`).
//! From then on QEMU shows the framebuffer's memory, and `tools/qemu-test`
//! compares a screendump with the approved image
//! `systems/display-test.screen.png`.
//!
//! Written from QEMU's fw_cfg specification (`docs/specs/fw_cfg.rst`,
//! test-only use) and edk2's ramfb configuration layout (see
//! `nowayos_hw::ramfb`). No QEMU source code was read. QEMU only: fw_cfg is
//! QEMU's own device, so this PD never ships on a device; the Pi 5's display
//! PD is a driver of its own.
//!
//! Everything raw - the registers, DMA and the physical addresses - is in
//! `nowayos-hw`; this PD has no `unsafe` code (the workspace lints forbid
//! it).

#![no_std]
#![no_main]

use nowayos_display::test_pattern;
use nowayos_hw::fw_cfg::FwCfg;
use nowayos_hw::{DmaRegion, HwError, device, dma_region, ramfb};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The device number the system description binds fw_cfg to.
const FW_CFG: usize = 0;

/// The DMA region for fw_cfg's requests, its file directory and the
/// configuration.
const REQUESTS: usize = 0;

/// The DMA region QEMU scans out.
const FRAMEBUFFER: usize = 1;

/// The screen: the Raspberry Pi Touch Display's 800 x 480.
const WIDTH: u32 = 800;
const HEIGHT: u32 = 480;

/// Bytes a pixel.
const BYTES_PER_PIXEL: usize = 4;

/// Bytes a row (the stride): `WIDTH` pixels of `BYTES_PER_PIXEL`.
const STRIDE: u32 = 3200;
const ROW: usize = 3200;

#[protection_domain]
fn init() -> RamfbPd {
    debug_println!("NOWAYOS: ramfb: starting");
    if let Err(error) = run() {
        debug_println!("NOWAYOS: ramfb: gave up: {error}");
    }
    RamfbPd
}

fn run() -> Result<(), HwError> {
    let mut fw_cfg = FwCfg::new(device(FW_CFG)?)?;
    let mut requests = dma_region(REQUESTS)?;
    let mut framebuffer = dma_region(FRAMEBUFFER)?;
    debug_println!("NOWAYOS: ramfb: found QEMU's fw_cfg device, with DMA");

    let Some(file) = fw_cfg.file(ramfb::FILE_NAME, &mut requests, 0)? else {
        debug_println!(
            "NOWAYOS: ramfb: gave up: fw_cfg has no etc/ramfb - QEMU needs -device ramfb"
        );
        return Ok(());
    };
    debug_println!("NOWAYOS: ramfb: found etc/ramfb ({} bytes)", file.size);

    draw_test_pattern(&mut framebuffer)?;
    ramfb::show(
        &mut fw_cfg,
        file,
        &mut requests,
        0,
        &framebuffer,
        (WIDTH, HEIGHT, STRIDE),
    )?;
    debug_println!("NOWAYOS: ramfb: showing an {WIDTH}x{HEIGHT} test pattern");
    Ok(())
}

/// Draws the display test pattern (`nowayos_display::test_pattern`, which
/// the Pi 5's framebuffer test draws too), one row at a time.
fn draw_test_pattern(framebuffer: &mut DmaRegion) -> Result<(), HwError> {
    let mut row = [0u8; ROW];
    let mut offset = 0usize;
    for y in 0..HEIGHT {
        for (x, pixel) in (0u32..).zip(row.chunks_exact_mut(BYTES_PER_PIXEL)) {
            let (red, green, blue) = test_pattern(x, y);
            pixel.copy_from_slice(&[blue, green, red, 0]);
        }
        framebuffer.write(offset, &row)?;
        offset = offset.saturating_add(ROW);
    }
    Ok(())
}

/// Event handler for the display PD. It has no channels or interrupts yet,
/// so the default `Handler` behaviour (nothing to do) is all it needs.
struct RamfbPd;

impl Handler for RamfbPd {
    type Error = Infallible;
}
