// SPDX-License-Identifier: MIT OR Apache-2.0
//! DMA memory: a memory region known by virtual address, physical address and
//! size, which a driver hands to its device by offset (point 4);
//! and I/O regions, memory a driver hands to its device but never maps.
//! With `device.rs`, `pl011.rs`, `fw_cfg.rs`, `cpu.rs` and
//! `virtio.rs`, the only `unsafe` code in `nowayos-hw`.
//!
//! Two things here need `unsafe`:
//!
//! 1. **The patched symbols**: for each DMA region a virtual address and a
//!    size, which a `<map>` binds (`setvar_vaddr`, `setvar_size`), and a
//!    physical address, which a `<setvar region_paddr>` binds; for each I/O
//!    region only the physical address. Defined with `export_name` and, on
//!    the seL4 target, `link_section = ".data"`, for the reasons `device.rs`
//!    gives. A physical address starts as `usize::MAX`, not zero, because
//!    zero is a valid physical address on some boards: an unpatched symbol
//!    must not look like one.
//! 2. **[`dma_region`]**, which turns a patched address into a
//!    `&'static [AtomicU8]` of the region's bytes.
//!
//! A device may write the region at any moment, so it is viewed as atomic
//! bytes, as `nowayos-region` views memory another PD shares: every bit
//! pattern is valid, and every access is atomic. A driver copies what it
//! reads out before checking it.
//!
//! A [`DmaRegion`] is only ever created from the patched symbols (or, in this
//! module's tests, from a test page), and its fields are private to this
//! module so nothing else can forge one. Its physical address is never handed
//! out raw for arithmetic: [`DmaRegion::device_address`] computes the address
//! of a span, checked to lie inside the region.
//!
//! An [`IoRegion`] is the same, without a mapping: a network driver hands
//! its device the packet buffers its client maps, by physical address, and
//! never sees what is in them. Microkit patches only the address
//! (a `<setvar>` has no size), so the PD states the size it assumes, and
//! the system tests check each description gives the region that size
//! (`tools/test_tools.py`); [`IoRegion::device_address`] then checks every
//! span against it. `tools/check-system` refuses a description that maps an
//! I/O region into the PD that learns its address.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::HwError;

/// DMA regions a PD can bind: numbers 0 to 3.
pub const DMA_REGIONS: usize = 4;

/// What a physical-address symbol holds until the Microkit tool patches it.
const UNPATCHED: usize = usize::MAX;

// ---- The symbols the Microkit tool patches ---------------------------------

/// One DMA region's patched symbols, and whether this PD has bound it.
struct Patched {
    vaddr: &'static AtomicUsize,
    vaddr_symbol: &'static str,
    size: &'static AtomicUsize,
    size_symbol: &'static str,
    paddr: &'static AtomicUsize,
    paddr_symbol: &'static str,
    taken: AtomicBool,
}

/// Defines the three symbols for each DMA region, and the table of them. See
/// the module documentation for why the attributes are needed and why they
/// are sound.
macro_rules! patched_table {
    ($table:ident: $([$vaddr:ident = $vaddr_symbol:literal, $size:ident = $size_symbol:literal,
                      $paddr:ident = $paddr_symbol:literal]),* $(,)?) => {
        $(
            #[unsafe(export_name = $vaddr_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $vaddr: AtomicUsize = AtomicUsize::new(0);

            #[unsafe(export_name = $size_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $size: AtomicUsize = AtomicUsize::new(0);

            #[unsafe(export_name = $paddr_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $paddr: AtomicUsize = AtomicUsize::new(UNPATCHED);
        )*

        static $table: [Patched; DMA_REGIONS] = [
            $(Patched {
                vaddr: &$vaddr,
                vaddr_symbol: $vaddr_symbol,
                size: &$size,
                size_symbol: $size_symbol,
                paddr: &$paddr,
                paddr_symbol: $paddr_symbol,
                taken: AtomicBool::new(false),
            }),*
        ];
    };
}

patched_table!(DMA_SYMBOLS:
    [M0_VADDR = "nowayos_dma_0_vaddr", M0_SIZE = "nowayos_dma_0_size", M0_PADDR = "nowayos_dma_0_paddr"],
    [M1_VADDR = "nowayos_dma_1_vaddr", M1_SIZE = "nowayos_dma_1_size", M1_PADDR = "nowayos_dma_1_paddr"],
    [M2_VADDR = "nowayos_dma_2_vaddr", M2_SIZE = "nowayos_dma_2_size", M2_PADDR = "nowayos_dma_2_paddr"],
    [M3_VADDR = "nowayos_dma_3_vaddr", M3_SIZE = "nowayos_dma_3_size", M3_PADDR = "nowayos_dma_3_paddr"],
);

// ---- DMA regions -----------------------------------------------------------

/// A memory region this PD maps and whose physical address it knows, so it
/// can hand parts of it to a device. Get one from [`dma_region`].
#[derive(Debug)]
pub struct DmaRegion {
    bytes: &'static [AtomicU8],
    paddr: u64,
}

/// Binds DMA region `index` (0 to 3) from the patched symbols
/// `nowayos_dma_{index}_vaddr`, `_size` and `_paddr`. Each region can be
/// bound once.
///
/// # Errors
///
/// [`HwError::NoSuchDevice`] if `index` is not below [`DMA_REGIONS`];
/// [`HwError::NotMapped`], [`HwError::SizeUnknown`] or
/// [`HwError::PhysicalAddressUnknown`] if the system description does not
/// bind a symbol; [`HwError::OutOfRange`] if the region would run past the
/// end of the virtual or physical address space; [`HwError::AlreadyTaken`]
/// the second time.
pub fn dma_region(index: usize) -> Result<DmaRegion, HwError> {
    let patched = DMA_SYMBOLS
        .get(index)
        .ok_or(HwError::NoSuchDevice { index })?;
    let vaddr = patched.vaddr.load(Ordering::Relaxed);
    if vaddr == 0 {
        return Err(HwError::NotMapped {
            symbol: patched.vaddr_symbol,
        });
    }
    let size = patched.size.load(Ordering::Relaxed);
    if size == 0 {
        return Err(HwError::SizeUnknown {
            symbol: patched.size_symbol,
        });
    }
    let paddr = patched.paddr.load(Ordering::Relaxed);
    if paddr == UNPATCHED {
        return Err(HwError::PhysicalAddressUnknown {
            symbol: patched.paddr_symbol,
        });
    }
    let paddr = u64::try_from(paddr).map_err(|_| HwError::OutOfRange { vaddr })?;
    if vaddr.checked_add(size).is_none()
        || u64::try_from(size)
            .ok()
            .and_then(|size| paddr.checked_add(size))
            .is_none()
    {
        return Err(HwError::OutOfRange { vaddr });
    }
    if patched.taken.swap(true, Ordering::Relaxed) {
        return Err(HwError::AlreadyTaken {
            symbol: patched.vaddr_symbol,
        });
    }
    // The region is memory the Microkit tool mapped for us, outside any Rust
    // allocation, so the pointer carries exposed provenance.
    let ptr = core::ptr::with_exposed_provenance::<AtomicU8>(vaddr);
    // SAFETY: `from_raw_parts` needs `size` valid, initialised `AtomicU8`s at
    // a non-null, aligned `ptr`, not mutated except through atomics while the
    // slice lives, within one allocation of at most `isize::MAX` bytes.
    // * Non-null and aligned: `vaddr` is not 0 (checked above), and
    //   `AtomicU8` needs no alignment.
    // * Valid for `size` bytes for the rest of the program: the Microkit tool
    //   patched `vaddr` and `size` from one `<map>` element, and never unmaps
    //   a memory region while the PD exists; the range does not wrap
    //   (checked above), and Microkit regions are far below `isize::MAX`.
    // * Initialised, and any bit pattern a valid `AtomicU8`: Microkit zeroes
    //   a memory region at boot, and afterwards it holds whatever this PD,
    //   another PD or a device wrote.
    // * Mutated only atomically as far as Rust can tell: this PD reaches the
    //   bytes only through this slice (the region is bound once, here, and
    //   tools/check-system refuses a description that maps a region twice
    //   into one PD), and a device's or another PD's writes are not Rust
    //   accesses, so they are like writes from another thread to atomics.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, size) };
    Ok(DmaRegion { bytes, paddr })
}

impl DmaRegion {
    /// The region's size in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the region has no bytes (never, once bound).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The region's physical address, for a log line. Hand a device
    /// [`device_address`](Self::device_address) instead.
    #[must_use]
    pub const fn paddr(&self) -> u64 {
        self.paddr
    }

    /// The physical address a device must use for the `len` bytes at
    /// `offset` in the region.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] unless all of `offset..offset + len` is
    /// inside the region. The arithmetic is checked, so a device is never
    /// given an address outside it.
    pub fn device_address(&self, offset: usize, len: usize) -> Result<u64, HwError> {
        self.span(offset, len)?;
        u64::try_from(offset)
            .ok()
            .and_then(|offset| self.paddr.checked_add(offset))
            .ok_or(HwError::OutsideRegion {
                offset,
                len,
                size: self.len(),
            })
    }

    /// Copies `bytes` into the region at `offset`.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if they do not fit; nothing is written.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), HwError> {
        for (cell, byte) in self.span(offset, bytes.len())?.iter().zip(bytes) {
            cell.store(*byte, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Copies the region's bytes at `offset` into `out` - which a device may
    /// have written, so check them only after copying.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if `out` does not fit; nothing is read.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), HwError> {
        let span = self.span(offset, out.len())?;
        for (byte, cell) in out.iter_mut().zip(span) {
            *byte = cell.load(Ordering::Relaxed);
        }
        Ok(())
    }

    /// The `len` bytes at `offset`, if they are all inside the region.
    fn span(&self, offset: usize, len: usize) -> Result<&[AtomicU8], HwError> {
        offset
            .checked_add(len)
            .and_then(|end| self.bytes.get(offset..end))
            .ok_or(HwError::OutsideRegion {
                offset,
                len,
                size: self.len(),
            })
    }
}

// ---- I/O regions -------------------------------------------------------------

/// I/O regions a PD can name: numbers 0 to 3.
pub const IO_REGIONS: usize = 4;

/// One I/O region's patched symbol, and whether this PD has bound it.
struct IoPatched {
    paddr: &'static AtomicUsize,
    paddr_symbol: &'static str,
    taken: AtomicBool,
}

/// Defines the physical-address symbol for each I/O region, and the table of
/// them, as `patched_table!` does for DMA regions.
macro_rules! io_table {
    ($table:ident: $([$paddr:ident = $paddr_symbol:literal]),* $(,)?) => {
        $(
            #[unsafe(export_name = $paddr_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $paddr: AtomicUsize = AtomicUsize::new(UNPATCHED);
        )*

        static $table: [IoPatched; IO_REGIONS] = [
            $(IoPatched {
                paddr: &$paddr,
                paddr_symbol: $paddr_symbol,
                taken: AtomicBool::new(false),
            }),*
        ];
    };
}

io_table!(IO_SYMBOLS:
    [I0_PADDR = "nowayos_io_0_paddr"],
    [I1_PADDR = "nowayos_io_1_paddr"],
    [I2_PADDR = "nowayos_io_2_paddr"],
    [I3_PADDR = "nowayos_io_3_paddr"],
);

/// The page size every Microkit memory region is aligned to, at least.
const PAGE: usize = 4096;

/// Memory this PD hands to a device but never maps - another PD's packet
/// buffers: known by physical address and size only. Get one
/// from [`io_region`].
#[derive(Debug)]
pub struct IoRegion {
    paddr: u64,
    size: usize,
}

/// Binds I/O region `index` (0 to 3) from the patched symbol
/// `nowayos_io_{index}_paddr`, as `size` bytes: the size this PD assumes,
/// which the system tests check the description gives the region (the
/// module documentation says why). Each region can be bound once.
///
/// # Errors
///
/// [`HwError::NoSuchDevice`] if `index` is not below [`IO_REGIONS`];
/// [`HwError::PhysicalAddressUnknown`] if the system description does not
/// bind the symbol; [`HwError::IoRegionInvalid`] if the address is not
/// page-aligned, `size` is 0, or the region would run past the end of the
/// physical address space; [`HwError::AlreadyTaken`] the second time.
pub fn io_region(index: usize, size: usize) -> Result<IoRegion, HwError> {
    let patched = IO_SYMBOLS
        .get(index)
        .ok_or(HwError::NoSuchDevice { index })?;
    let paddr = patched.paddr.load(Ordering::Relaxed);
    if paddr == UNPATCHED {
        return Err(HwError::PhysicalAddressUnknown {
            symbol: patched.paddr_symbol,
        });
    }
    let invalid = HwError::IoRegionInvalid {
        symbol: patched.paddr_symbol,
        paddr,
        size,
    };
    if size == 0 || !paddr.is_multiple_of(PAGE) {
        return Err(invalid);
    }
    let paddr = u64::try_from(paddr).map_err(|_| invalid)?;
    if u64::try_from(size)
        .ok()
        .and_then(|size| paddr.checked_add(size))
        .is_none()
    {
        return Err(invalid);
    }
    if patched.taken.swap(true, Ordering::Relaxed) {
        return Err(HwError::AlreadyTaken {
            symbol: patched.paddr_symbol,
        });
    }
    Ok(IoRegion { paddr, size })
}

impl IoRegion {
    /// The region's size in bytes, as the PD stated it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.size
    }

    /// Whether the region has no bytes (never, once bound).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The region's physical address, for a log line. Hand a device
    /// [`device_address`](Self::device_address) instead.
    #[must_use]
    pub const fn paddr(&self) -> u64 {
        self.paddr
    }

    /// The physical address a device must use for the `len` bytes at
    /// `offset` in the region.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] unless all of `offset..offset + len` is
    /// inside the region. The arithmetic is checked, so a device is never
    /// given an address outside it.
    pub fn device_address(&self, offset: usize, len: usize) -> Result<u64, HwError> {
        let outside = HwError::OutsideRegion {
            offset,
            len,
            size: self.size,
        };
        let end = offset.checked_add(len).ok_or(outside)?;
        if end > self.size {
            return Err(outside);
        }
        u64::try_from(offset)
            .ok()
            .and_then(|offset| self.paddr.checked_add(offset))
            .ok_or(outside)
    }
}

#[cfg(test)]
impl IoRegion {
    /// A region at a made-up physical address: for other modules' tests.
    pub(crate) const fn for_test(paddr: u64, size: usize) -> Self {
        Self { paddr, size }
    }
}

#[cfg(test)]
impl DmaRegion {
    /// A region over test memory, at a made-up physical address: for other
    /// modules' tests.
    pub(crate) const fn for_test(bytes: &'static [AtomicU8], paddr: u64) -> Self {
        Self { bytes, paddr }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! These run under Miri too: no inline assembly.

    use super::*;

    /// A page of test memory standing in for a DMA region.
    #[repr(C, align(4096))]
    struct TestPage([AtomicU8; 4096]);

    static PAGE: TestPage = TestPage([const { AtomicU8::new(0) }; 4096]);

    /// A physical address for the test page, as the tool might choose.
    const PADDR: usize = 0x4100_0000;

    fn page_vaddr() -> usize {
        core::ptr::from_ref(&PAGE).expose_provenance()
    }

    /// Patches DMA region `index`'s symbols as the Microkit tool would.
    fn patch(index: usize, vaddr: usize, size: usize, paddr: usize) {
        DMA_SYMBOLS[index].vaddr.store(vaddr, Ordering::Relaxed);
        DMA_SYMBOLS[index].size.store(size, Ordering::Relaxed);
        DMA_SYMBOLS[index].paddr.store(paddr, Ordering::Relaxed);
    }

    // Each test uses its own region number: the symbols are global, and
    // tests run in parallel. Region 0 is the only one bound to the page.

    #[test]
    fn a_bound_region_gives_bytes_and_checked_device_addresses() {
        patch(0, page_vaddr(), 4096, PADDR);
        let mut region = dma_region(0).unwrap();
        assert_eq!((region.len(), region.paddr()), (4096, 0x4100_0000));
        assert!(!region.is_empty());

        region.write(4090, b"QEMU").unwrap();
        let mut out = [0; 4];
        region.read(4090, &mut out).unwrap();
        assert_eq!(&out, b"QEMU");
        assert_eq!(PAGE.0[4093].load(Ordering::Relaxed), b'U');

        assert_eq!(region.device_address(0, 4096), Ok(0x4100_0000));
        assert_eq!(region.device_address(4095, 1), Ok(0x4100_0FFF));
        assert_eq!(region.device_address(4096, 0), Ok(0x4100_1000));
        let outside = |offset, len| HwError::OutsideRegion {
            offset,
            len,
            size: 4096,
        };
        assert_eq!(region.device_address(4095, 2), Err(outside(4095, 2)));
        assert_eq!(region.device_address(4097, 0), Err(outside(4097, 0)));
        assert_eq!(
            region.device_address(usize::MAX, 2),
            Err(outside(usize::MAX, 2))
        );
        assert_eq!(region.write(4093, b"QEMU"), Err(outside(4093, 4)));
        assert_eq!(region.read(5000, &mut out), Err(outside(5000, 4)));
        assert_eq!(PAGE.0[4094].load(Ordering::Relaxed), 0, "nothing written");

        assert_eq!(
            dma_region(0).unwrap_err(),
            HwError::AlreadyTaken {
                symbol: "nowayos_dma_0_vaddr"
            }
        );
    }

    #[test]
    fn unpatched_symbols_are_reported_by_name() {
        assert_eq!(
            dma_region(1).unwrap_err(),
            HwError::NotMapped {
                symbol: "nowayos_dma_1_vaddr"
            }
        );
        patch(2, 0x1000, 4096, UNPATCHED);
        assert_eq!(
            dma_region(2).unwrap_err(),
            HwError::PhysicalAddressUnknown {
                symbol: "nowayos_dma_2_paddr"
            }
        );
        patch(2, 0x1000, 0, 0);
        assert_eq!(
            dma_region(2).unwrap_err(),
            HwError::SizeUnknown {
                symbol: "nowayos_dma_2_size"
            }
        );
        assert_eq!(
            dma_region(DMA_REGIONS).unwrap_err(),
            HwError::NoSuchDevice { index: DMA_REGIONS }
        );
    }

    #[test]
    fn a_region_running_past_the_end_of_memory_is_refused() {
        patch(3, usize::MAX - 10, 4096, PADDR);
        assert_eq!(
            dma_region(3).unwrap_err(),
            HwError::OutOfRange {
                vaddr: usize::MAX - 10
            }
        );
        patch(3, 0x1000, 4096, usize::MAX - 10);
        assert_eq!(
            dma_region(3).unwrap_err(),
            HwError::OutOfRange { vaddr: 0x1000 }
        );
    }

    #[test]
    fn an_io_region_gives_only_checked_device_addresses() {
        IO_SYMBOLS[0].paddr.store(0x4200_0000, Ordering::Relaxed);
        let region = io_region(0, 0x4_0000).unwrap();
        assert_eq!((region.len(), region.paddr()), (0x4_0000, 0x4200_0000));
        assert!(!region.is_empty());
        assert_eq!(region.device_address(0x800, 0x800), Ok(0x4200_0800));
        assert_eq!(region.device_address(0x3_f800, 0x800), Ok(0x4203_f800));
        let outside = |offset, len| HwError::OutsideRegion {
            offset,
            len,
            size: 0x4_0000,
        };
        assert_eq!(
            region.device_address(0x3_f800, 0x801),
            Err(outside(0x3_f800, 0x801))
        );
        assert_eq!(
            region.device_address(0x4_0000, 1),
            Err(outside(0x4_0000, 1))
        );
        assert_eq!(
            region.device_address(usize::MAX, 2),
            Err(outside(usize::MAX, 2))
        );
        assert_eq!(
            io_region(0, 0x4_0000).unwrap_err(),
            HwError::AlreadyTaken {
                symbol: "nowayos_io_0_paddr"
            }
        );
    }

    #[test]
    fn an_io_region_must_be_bound_page_aligned_and_inside_memory() {
        assert_eq!(
            io_region(1, 0x1000).unwrap_err(),
            HwError::PhysicalAddressUnknown {
                symbol: "nowayos_io_1_paddr"
            }
        );
        let invalid = |paddr, size| HwError::IoRegionInvalid {
            symbol: "nowayos_io_2_paddr",
            paddr,
            size,
        };
        IO_SYMBOLS[2].paddr.store(0x4200_0100, Ordering::Relaxed);
        assert_eq!(
            io_region(2, 0x1000).unwrap_err(),
            invalid(0x4200_0100, 0x1000)
        );
        IO_SYMBOLS[2].paddr.store(0x4200_0000, Ordering::Relaxed);
        assert_eq!(io_region(2, 0).unwrap_err(), invalid(0x4200_0000, 0));
        IO_SYMBOLS[2]
            .paddr
            .store(usize::MAX - 0xfff, Ordering::Relaxed);
        assert_eq!(
            io_region(2, 0x2000).unwrap_err(),
            invalid(usize::MAX - 0xfff, 0x2000)
        );
        assert_eq!(
            io_region(IO_REGIONS, 0x1000).unwrap_err(),
            HwError::NoSuchDevice { index: IO_REGIONS }
        );
    }
}
