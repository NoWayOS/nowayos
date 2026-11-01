// SPDX-License-Identifier: MIT OR Apache-2.0
//! QEMU's firmware configuration device (fw_cfg), through its DMA interface:
//! a test device that proves DMA end to end in QEMU. QEMU only, and
//! test only - it holds nothing a product needs.
//!
//! Written from QEMU's fw_cfg specification ("QEMU Firmware Configuration
//! (fw_cfg) Device", `docs/specs/fw_cfg.rst`, read as published at
//! qemu.org on 2026-10-03; the document is GPL-2.0, and only the interface
//! it describes was used - no QEMU source code was read). The project owner
//! accepted that for test-only code: never use this module in a
//! product PD. On the Arm `virt` machine the device's registers are
//! memory-mapped:
//!
//! | Offset | Register | Use here |
//! | --- | --- | --- |
//! | 0x00 | Data, 64 bits | not used |
//! | 0x08 | Selector, 16 bits, big-endian | not used: a DMA request selects its own item |
//! | 0x10 | DMA address, 64 bits, big-endian | reading gives the signature `"QEMU CFG"` when DMA is available; writing the physical address of a request starts it |
//!
//! A DMA request is 16 bytes, big-endian: a 32-bit control word (bit 0
//! error, 1 read, 2 skip, 3 select, 4 write; with select, the item's key in
//! bits 31:16), a 32-bit length and a 64-bit address. The device reads the
//! request from that physical address, transfers the data, and clears the
//! control word - or leaves the error bit set.
//!
//! A write to the DMA address register makes the device read and write
//! memory, so it is not reachable through any safe wrapper:
//! [`FwCfg::read_item`] and [`FwCfg::write_item`] are the only ways to start
//! a transfer, and they write the request themselves, with both addresses
//! computed by the [`DmaRegion`] and checked to lie inside it.
//!
//! Items with a name - such as `etc/ramfb`, QEMU's simple framebuffer -
//! are found through the file directory, item [`FILE_DIR_KEY`]: a
//! big-endian 32-bit count, then one 64-byte entry per file - a 32-bit size,
//! a 16-bit key, 16 reserved bits and a 56-byte name padded with zero bytes
//! ([`find_file`]).

use core::mem::{offset_of, size_of};

use safe_mmio::fields::ReadPure;
use safe_mmio::{UniqueMmioPointer, field, field_shared};

use crate::cpu::dsb;
use crate::device::RegisterBlock;
use crate::{Device, DmaRegion, HwError};

/// What the DMA address register reads as when the DMA interface exists:
/// `"QEMU CFG"`, big-endian.
pub const DMA_SIGNATURE: u64 = 0x5145_4d55_2043_4647;

/// The size of a DMA request in DMA memory.
pub const REQUEST_LEN: usize = 16;

/// The key of the signature item, which reads as `"QEMU"`.
pub const SIGNATURE_KEY: u16 = 0x0000;

/// The key of the file directory.
pub const FILE_DIR_KEY: u16 = 0x0019;

/// The size of one file-directory entry.
pub const FILE_ENTRY_LEN: usize = 64;

/// The size of a file name in a file-directory entry, padding included.
pub const FILE_NAME_LEN: usize = 56;

/// A named item in fw_cfg's file directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct File {
    /// The key that selects it.
    pub key: u16,
    /// Its size in bytes.
    pub size: u32,
}

/// The file called `name` in a copy of the file directory's entries (the
/// directory after its count), or `None`. A name longer than
/// [`FILE_NAME_LEN`] is never found; an entry whose name fills all 56 bytes
/// matches only that whole name.
#[must_use]
pub fn find_file(entries: &[u8], name: &[u8]) -> Option<File> {
    if name.is_empty() || name.len() > FILE_NAME_LEN {
        return None;
    }
    entries.chunks_exact(FILE_ENTRY_LEN).find_map(|entry| {
        let (size, rest) = entry.split_first_chunk::<4>()?;
        let (key, rest) = rest.split_first_chunk::<2>()?;
        let entry_name = rest.get(2..)?;
        let padding = entry_name.get(name.len()..)?;
        (entry_name.get(..name.len())? == name && padding.first().is_none_or(|&b| b == 0)).then(
            || File {
                key: u16::from_be_bytes(*key),
                size: u32::from_be_bytes(*size),
            },
        )
    })
}

const CONTROL_ERROR: u32 = 1 << 0;
const CONTROL_READ: u32 = 1 << 1;
const CONTROL_SELECT: u32 = 1 << 3;
const CONTROL_WRITE: u32 = 1 << 4;

/// How many times to look at the control word before giving up. QEMU
/// finishes a transfer before the triggering write returns, so the first
/// look normally suffices.
const POLLS: usize = 1000;

/// fw_cfg's registers on the Arm `virt` machine.
#[repr(C)]
pub(crate) struct Registers {
    /// 0x00 data and 0x08 selector registers: not used.
    _data_and_selector: [u64; 2],
    /// 0x10 DMA address register. Reading has no side effect (the
    /// signature); writing starts a transfer, so it is written only by
    /// [`FwCfg::read_item`], through `write_unsafe`.
    dma: ReadPure<u64>,
}

const _: () = {
    assert!(offset_of!(Registers, dma) == 0x10);
    assert!(size_of::<Registers>() == 0x18);
};

// SAFETY: `repr(C)`; the DMA address register at the offset the
// specification gives (asserted above), 64 bits wide. Its wrapper is
// `ReadPure`: a read returns the signature and has no side effect, and no
// safe write exists - the only write is `FwCfg::read_item`'s, which gives the
// device addresses inside this PD's own DMA region. The data and selector
// registers are padding, never accessed.
unsafe impl RegisterBlock for Registers {}

/// A safe view of fw_cfg's registers. Make one with [`FwCfg::new`].
#[derive(Debug)]
pub struct FwCfg<'a> {
    regs: UniqueMmioPointer<'a, Registers>,
}

impl FwCfg<'static> {
    /// Views `device` as fw_cfg, after checking that its DMA interface is
    /// there.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::NoSignature`] if the DMA
    /// address register does not read as [`DMA_SIGNATURE`].
    pub fn new(device: Device) -> Result<Self, HwError> {
        Self::checked(device.view()?)
    }
}

impl<'a> FwCfg<'a> {
    fn checked(regs: UniqueMmioPointer<'a, Registers>) -> Result<Self, HwError> {
        let read = u64::from_be(field_shared!(regs, dma).read());
        if read != DMA_SIGNATURE {
            return Err(HwError::NoSignature {
                expected: "fw_cfg with DMA",
                read,
            });
        }
        Ok(Self { regs })
    }

    /// Reads the first `len` bytes of item `key` into `region`, by DMA. The
    /// request goes at `at` in the region and the data straight after it, at
    /// `at + REQUEST_LEN`.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the request and the data do not both
    /// fit in the region - checked before the device is told anything;
    /// [`HwError::TransferFailed`] if the device reports an error or does
    /// not finish.
    pub fn read_item(
        &mut self,
        key: u16,
        region: &mut DmaRegion,
        at: usize,
        len: u32,
    ) -> Result<(), HwError> {
        self.transfer(key, region, at, len, CONTROL_READ)
    }

    /// Writes `len` bytes to item `key`, by DMA, from `region` at
    /// `at + REQUEST_LEN`, where the caller has put them; the request goes
    /// at `at`. Only items QEMU lets the guest write accept it, such as
    /// `etc/ramfb`.
    ///
    /// # Errors
    ///
    /// As [`FwCfg::read_item`].
    pub fn write_item(
        &mut self,
        key: u16,
        region: &mut DmaRegion,
        at: usize,
        len: u32,
    ) -> Result<(), HwError> {
        self.transfer(key, region, at, len, CONTROL_WRITE)
    }

    /// The file called `name`, from the file directory read into `region`
    /// (the request at `at`, the directory after it), or `None`.
    ///
    /// # Errors
    ///
    /// As [`FwCfg::read_item`]: [`HwError::OutsideRegion`] if the directory
    /// does not fit in the region.
    pub fn file(
        &mut self,
        name: &[u8],
        region: &mut DmaRegion,
        at: usize,
    ) -> Result<Option<File>, HwError> {
        self.read_item(FILE_DIR_KEY, region, at, 4)?;
        let data_at = at.saturating_add(REQUEST_LEN);
        let mut count = [0; 4];
        region.read(data_at, &mut count)?;
        let entries = usize::try_from(u32::from_be_bytes(count))
            .ok()
            .and_then(|count| count.checked_mul(FILE_ENTRY_LEN))
            .and_then(|bytes| bytes.checked_add(4))
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or(HwError::OutsideRegion {
                offset: data_at,
                len: usize::MAX,
                size: region.len(),
            })?;
        self.read_item(FILE_DIR_KEY, region, at, entries)?;
        let mut entry = [0; FILE_ENTRY_LEN];
        let mut offset = data_at.saturating_add(4);
        let end = data_at.saturating_add(usize::try_from(entries).unwrap_or(usize::MAX));
        while offset < end {
            region.read(offset, &mut entry)?;
            if let Some(found) = find_file(&entry, name) {
                return Ok(Some(found));
            }
            offset = offset.saturating_add(FILE_ENTRY_LEN);
        }
        Ok(None)
    }

    /// One DMA transfer of `len` bytes between item `key` and `region` at
    /// `at + REQUEST_LEN`, in the direction `direction` gives
    /// ([`CONTROL_READ`]: the device writes the region; [`CONTROL_WRITE`]: it
    /// reads it), with the request at `at`.
    fn transfer(
        &mut self,
        key: u16,
        region: &mut DmaRegion,
        at: usize,
        len: u32,
        direction: u32,
    ) -> Result<(), HwError> {
        let data_at = at.checked_add(REQUEST_LEN).ok_or(HwError::OutsideRegion {
            offset: at,
            len: REQUEST_LEN,
            size: region.len(),
        })?;
        let data_len = usize::try_from(len).map_err(|_| HwError::OutsideRegion {
            offset: data_at,
            len: usize::MAX,
            size: region.len(),
        })?;
        let request_address = region.device_address(at, REQUEST_LEN)?;
        let data_address = region.device_address(data_at, data_len)?;

        let control = (u32::from(key) << 16) | CONTROL_SELECT | direction;
        let mut request = [0; REQUEST_LEN];
        let (head, address) = request.split_at_mut(8);
        let (control_bytes, length_bytes) = head.split_at_mut(4);
        control_bytes.copy_from_slice(&control.to_be_bytes());
        length_bytes.copy_from_slice(&len.to_be_bytes());
        address.copy_from_slice(&data_address.to_be_bytes());
        region.write(at, &request)?;

        // The request must be in memory before the device is told where it
        // is.
        dsb();
        // SAFETY: `write_unsafe` needs the write to be safe to make. It starts
        // a DMA transfer, which is safe because of what the device is given:
        // `request_address` is the device address of the 16-byte request at
        // `at` in this PD's DMA region, written just above, and the request
        // asks the device only to write (`CONTROL_READ`) or read
        // (`CONTROL_WRITE`) `len` bytes at `data_address`, the span straight
        // after it - both computed by `DmaRegion` and checked to lie inside the
        // region, which the system description maps into this PD and whose
        // physical address it granted (tools/check-system); the device clears
        // the request's control word when done. The region is borrowed
        // mutably for this call, so no safe code can change the request or the
        // data while the device uses them; QEMU finishes the transfer before
        // this write returns.
        unsafe {
            field!(self.regs, dma).write_unsafe(ReadPure(request_address.to_be()));
        }
        dsb();

        for _ in 0..POLLS {
            let mut word = [0; 4];
            region.read(at, &mut word)?;
            let status = u32::from_be_bytes(word);
            if status & CONTROL_ERROR != 0 {
                return Err(HwError::TransferFailed {
                    device: "fw_cfg",
                    status,
                });
            }
            if status == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(HwError::TransferFailed {
            device: "fw_cfg",
            status: control,
        })
    }
}

#[cfg(all(test, not(miri)))]
#[allow(clippy::unwrap_used, reason = "test code: a failed check panics")]
mod tests {
    //! Ordinary memory stands in for the device's registers. No device runs,
    //! so these check the signature and the refusals made before a transfer
    //! starts; QEMU runs the transfer itself (`systems/dma-demo`).

    use super::*;

    fn registers(signature: u64) -> Registers {
        Registers {
            _data_and_selector: [0; 2],
            dma: ReadPure(signature.to_be()),
        }
    }

    #[test]
    fn a_transfer_that_does_not_fit_is_refused_before_the_device_hears_of_it() {
        static MEMORY: [core::sync::atomic::AtomicU8; 64] =
            [const { core::sync::atomic::AtomicU8::new(0) }; 64];
        let mut region = DmaRegion::for_test(&MEMORY, 0x4100_0000);
        let mut registers = registers(DMA_SIGNATURE);
        let mut fw_cfg = FwCfg::checked(UniqueMmioPointer::from(&mut registers)).unwrap();
        for (at, len) in [(0, 49), (40, 9), (49, 0), (usize::MAX - 8, 4)] {
            assert!(
                matches!(
                    fw_cfg.read_item(SIGNATURE_KEY, &mut region, at, len),
                    Err(HwError::OutsideRegion { .. })
                ),
                "read at {at}, {len} bytes"
            );
            assert!(
                matches!(
                    fw_cfg.write_item(SIGNATURE_KEY, &mut region, at, len),
                    Err(HwError::OutsideRegion { .. })
                ),
                "write at {at}, {len} bytes"
            );
        }
        assert_eq!(
            registers.dma.0,
            DMA_SIGNATURE.to_be(),
            "no transfer started"
        );
        let mut written = [0; 64];
        region.read(0, &mut written).unwrap();
        assert_eq!(written, [0; 64], "no request written");
    }

    #[test]
    fn the_dma_signature_identifies_the_device() {
        let mut good = registers(DMA_SIGNATURE);
        assert!(FwCfg::checked(UniqueMmioPointer::from(&mut good)).is_ok());
        let mut none = registers(0);
        assert_eq!(
            FwCfg::checked(UniqueMmioPointer::from(&mut none)).unwrap_err(),
            HwError::NoSignature {
                expected: "fw_cfg with DMA",
                read: 0
            }
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod directory_tests {
    //! The file directory's parsing, on byte arrays: no device, so these run
    //! under Miri too.

    use super::*;

    fn entry(size: u32, key: u16, name: &[u8]) -> [u8; FILE_ENTRY_LEN] {
        let mut entry = [0; FILE_ENTRY_LEN];
        entry[..4].copy_from_slice(&size.to_be_bytes());
        entry[4..6].copy_from_slice(&key.to_be_bytes());
        entry[8..8 + name.len()].copy_from_slice(name);
        entry
    }

    fn directory(entries: &[[u8; FILE_ENTRY_LEN]]) -> [u8; 4 * FILE_ENTRY_LEN] {
        let mut out = [0; 4 * FILE_ENTRY_LEN];
        for (slot, entry) in out.chunks_exact_mut(FILE_ENTRY_LEN).zip(entries) {
            slot.copy_from_slice(entry);
        }
        out
    }

    #[test]
    fn a_file_is_found_by_its_whole_name() {
        let full = [b'x'; FILE_NAME_LEN];
        let entries = directory(&[
            entry(4, 0x20, b"etc/ramfb2"),
            entry(28, 0x21, b"etc/ramfb"),
            entry(9, 0x22, &full),
        ]);
        assert_eq!(
            find_file(&entries, b"etc/ramfb"),
            Some(File {
                key: 0x21,
                size: 28
            })
        );
        assert_eq!(
            find_file(&entries, b"etc/ramfb2").map(|f| f.key),
            Some(0x20)
        );
        assert_eq!(
            find_file(&entries, b"etc/ram"),
            None,
            "a prefix is not a name"
        );
        assert_eq!(find_file(&entries, &full).map(|f| f.key), Some(0x22));
        assert_eq!(find_file(&entries, &[b'x'; FILE_NAME_LEN - 1]), None);
        assert_eq!(find_file(&entries, &[b'x'; FILE_NAME_LEN + 1]), None);
        assert_eq!(find_file(&entries, b""), None);
        assert_eq!(
            find_file(&entries[..FILE_ENTRY_LEN - 1], b"etc/ramfb2"),
            None
        );
    }
}
