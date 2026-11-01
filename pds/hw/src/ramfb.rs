// SPDX-License-Identifier: MIT OR Apache-2.0
//! QEMU's `ramfb` display device: a linear framebuffer in guest
//! memory, which QEMU shows as its screen once told where it is, in which
//! format and size, through fw_cfg's `etc/ramfb` file. Used by the
//! test-pattern display PD (`nowayos-ramfb`) and the display PD of the
//! interactive demo (`nowayos-display-ramfb`).
//!
//! Written from: QEMU's fw_cfg specification (`docs/specs/fw_cfg.rst`,
//! test-only use) for the DMA write, and the configuration's layout
//! in edk2's `OvmfPkg/QemuRamfbDxe/QemuRamfb.c` (BSD-2-Clause-Patent): a
//! 64-bit address, then the format (a DRM fourcc), flags, width, height and
//! stride as 32-bit fields, all big-endian. No QEMU source code was read.
//! QEMU only: fw_cfg is QEMU's own device. No `unsafe` code.

use crate::fw_cfg::{File, FwCfg, REQUEST_LEN};
use crate::{DmaRegion, HwError};

/// The name of ramfb's file in fw_cfg's file directory.
pub const FILE_NAME: &[u8] = b"etc/ramfb";

/// DRM_FORMAT_XRGB8888 (`XR24`): 32 bits a pixel, blue in the lowest byte.
pub const XRGB8888: u32 = 0x3432_5258;

/// Bytes of ramfb's configuration.
pub const CONFIG_LEN: usize = 28;

/// [`CONFIG_LEN`] as fw_cfg's transfer length.
const CONFIG_LEN_U32: u32 = 28;

/// ramfb's configuration for an Xrgb8888 framebuffer at device address
/// `address`: the address, then format, flags, width, height and stride,
/// big-endian.
#[must_use]
pub fn config(address: u64, width: u32, height: u32, stride: u32) -> [u8; CONFIG_LEN] {
    let mut config = [0; CONFIG_LEN];
    if let Some((head, rest)) = config.split_first_chunk_mut::<8>() {
        *head = address.to_be_bytes();
        for (slot, value) in rest
            .chunks_exact_mut(4)
            .zip([XRGB8888, 0, width, height, stride])
        {
            slot.copy_from_slice(&value.to_be_bytes());
        }
    }
    config
}

/// Has QEMU show `framebuffer` as its screen: `height` rows of `stride`
/// bytes from the region's start, `width` Xrgb8888 pixels each. `file` is
/// ramfb's file ([`FILE_NAME`], from [`FwCfg::file`]); the request and the
/// configuration go in `requests` at `at`.
///
/// # Errors
///
/// [`HwError::OutsideRegion`] if the frame does not fit in `framebuffer`
/// (nothing is handed to QEMU then), or what [`FwCfg::write_item`] reports.
pub fn show(
    fw_cfg: &mut FwCfg,
    file: File,
    requests: &mut DmaRegion,
    at: usize,
    framebuffer: &DmaRegion,
    (width, height, stride): (u32, u32, u32),
) -> Result<(), HwError> {
    let bytes = usize::try_from(stride)
        .ok()
        .zip(usize::try_from(height).ok())
        .and_then(|(stride, height)| stride.checked_mul(height))
        .ok_or(HwError::OutsideRegion {
            offset: 0,
            len: usize::MAX,
            size: framebuffer.len(),
        })?;
    // The span QEMU will read, checked to lie inside the framebuffer region.
    let address = framebuffer.device_address(0, bytes)?;
    requests.write(
        at.saturating_add(REQUEST_LEN),
        &config(address, width, height, stride),
    )?;
    fw_cfg.write_item(file.key, requests, at, CONFIG_LEN_U32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_is_big_endian_in_edk2s_order() {
        let mut expected = Vec::new();
        expected.extend_from_slice(&0x1234_5678_9abc_def0_u64.to_be_bytes());
        for value in [XRGB8888, 0, 160, 120, 640] {
            expected.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(
            config(0x1234_5678_9abc_def0, 160, 120, 640).to_vec(),
            expected
        );
        assert_eq!(&XRGB8888.to_le_bytes(), b"XR24");
    }
}
