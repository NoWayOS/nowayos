// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-dma-demo`: a device writes into this PD's DMA memory, by the
//! physical address the PD learned from the system description.
//!
//! QEMU's firmware configuration device (fw_cfg) has a DMA interface: given
//! the physical address of a request, it reads the request from memory and
//! writes the item asked for wherever the request says. So if the physical
//! address this PD learned for its DMA region (`<setvar region_paddr>`) were
//! wrong, the signature item `"QEMU"` would not appear in the region. The
//! PD then asks for a transfer that would run past the end of the region,
//! which `nowayos-hw` refuses before the device hears of it.
//!
//! Everything raw - the register, the physical address arithmetic, the DMA
//! memory itself - is in `nowayos-hw`; this PD has no `unsafe` code (the
//! workspace lints forbid it). QEMU only: fw_cfg is a QEMU device, and a test
//! device - nothing a product needs.

#![no_std]
#![no_main]

use nowayos_hw::fw_cfg::{FwCfg, REQUEST_LEN, SIGNATURE_KEY};
use nowayos_hw::{HwError, device, dma_region};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The device number the system description binds fw_cfg to.
const FW_CFG: usize = 0;

/// The DMA region number the system description binds.
const DMA: usize = 0;

/// Where in the region the request goes; the data follows it.
const AT: usize = 0x100;

/// Where the data goes.
const DATA_AT: usize = AT + REQUEST_LEN;

#[protection_domain]
fn init() -> DmaDemoPd {
    debug_println!("NOWAYOS: dma-demo: starting");
    if let Err(error) = run() {
        debug_println!("NOWAYOS: dma-demo: gave up: {error}");
    }
    DmaDemoPd
}

fn run() -> Result<(), HwError> {
    let mut fw_cfg = FwCfg::new(device(FW_CFG)?)?;
    let mut region = dma_region(DMA)?;
    debug_println!("NOWAYOS: dma-demo: found QEMU's fw_cfg device, with DMA");
    debug_println!(
        "NOWAYOS: dma-demo: DMA region of {} bytes at physical address {:#x}",
        region.len(),
        region.paddr()
    );

    fw_cfg.read_item(SIGNATURE_KEY, &mut region, AT, 4)?;
    let mut signature = [0; 4];
    region.read(DATA_AT, &mut signature)?;
    if &signature == b"QEMU" {
        debug_println!(
            "NOWAYOS: dma-demo: the device wrote \"QEMU\" into the DMA region by its physical address"
        );
    } else {
        debug_println!("NOWAYOS: dma-demo: gave up: the device wrote {signature:?}, not \"QEMU\"");
        return Ok(());
    }

    let past_the_end = u32::try_from(region.len()).unwrap_or(u32::MAX);
    match fw_cfg.read_item(SIGNATURE_KEY, &mut region, AT, past_the_end) {
        Err(HwError::OutsideRegion { .. }) => debug_println!(
            "NOWAYOS: dma-demo: a transfer past the end of the region was refused before the device saw it"
        ),
        Ok(()) => debug_println!(
            "NOWAYOS: dma-demo: gave up: a transfer past the end of the region was made"
        ),
        Err(error) => return Err(error),
    }
    debug_println!("NOWAYOS: dma-demo: done");
    Ok(())
}

/// Event handler for the DMA demo PD. It has no channels or interrupts, so
/// the default `Handler` behaviour (nothing to do) is all it needs.
struct DmaDemoPd;

impl Handler for DmaDemoPd {
    type Error = Infallible;
}
