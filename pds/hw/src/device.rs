// SPDX-License-Identifier: MIT OR Apache-2.0
//! Binding a device's registers: the patched symbols, and turning a patched
//! address into a register view. With `pl011.rs` and `cpu.rs`, the only
//! `unsafe` code in `nowayos-hw`.
//!
//! Two things here need `unsafe`:
//!
//! 1. **The patched symbols** (`patched_table!`). The Microkit tool writes
//!    each device mapping's virtual address and size into symbols named by
//!    the system description (`setvar_vaddr`, `setvar_size`). They are
//!    defined here with `export_name`, which fixes their names, and, on the
//!    seL4 target, `link_section = ".data"`, which puts them where the tool
//!    can rewrite them: a zero-initialised variable would otherwise be placed
//!    in `.bss`, which has no bytes in the program image to rewrite. Both
//!    attributes are `unsafe` because a second definition of the same symbol
//!    name, or an unsuitable section, would be undefined behaviour. Every
//!    name starts with `nowayos_device_` and is defined nowhere else, and
//!    `.data` is an ordinary writable data section. The symbols are
//!    `AtomicUsize`, so the compiler cannot assume they still hold their
//!    initial zero, and each is one machine word wide, as the tool checks
//!    before patching. (The same reasoning as `nowayos-region`'s symbols.)
//! 2. **[`Device::view`]**, which turns a device's address into a
//!    `safe-mmio` pointer to its register block after checking everything
//!    that can be checked at run time; and [`RegisterBlock`], the `unsafe`
//!    trait a register layout implements to promise it matches its device.
//!
//! A [`Device`] is only ever created from the patched symbols (or, in this
//! module's tests, from a test page). That is the invariant `view` rests on,
//! and its fields are private to this module so nothing else can forge one.

use core::mem::{align_of, size_of};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use safe_mmio::UniqueMmioPointer;

use crate::HwError;

/// Devices a PD can bind: numbers 0 to 3.
pub const DEVICES: usize = 4;

// ---- The symbols the Microkit tool patches ---------------------------------

/// One device mapping's patched symbols, and whether this PD has bound it.
struct Patched {
    vaddr: &'static AtomicUsize,
    vaddr_symbol: &'static str,
    size: &'static AtomicUsize,
    size_symbol: &'static str,
    taken: AtomicBool,
}

impl Patched {
    const fn new(
        vaddr: &'static AtomicUsize,
        vaddr_symbol: &'static str,
        size: &'static AtomicUsize,
        size_symbol: &'static str,
    ) -> Self {
        Self {
            vaddr,
            vaddr_symbol,
            size,
            size_symbol,
            taken: AtomicBool::new(false),
        }
    }
}

/// Defines the address and size symbols for the devices, and the table of
/// them. See the module documentation for why the attributes are needed and
/// why they are sound.
macro_rules! patched_table {
    ($table:ident: $([$vaddr:ident = $vaddr_symbol:literal, $size:ident = $size_symbol:literal]),* $(,)?) => {
        $(
            #[unsafe(export_name = $vaddr_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $vaddr: AtomicUsize = AtomicUsize::new(0);

            #[unsafe(export_name = $size_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $size: AtomicUsize = AtomicUsize::new(0);
        )*

        static $table: [Patched; DEVICES] = [
            $(Patched::new(&$vaddr, $vaddr_symbol, &$size, $size_symbol)),*
        ];
    };
}

patched_table!(DEVICE_SYMBOLS:
    [D0_VADDR = "nowayos_device_0_vaddr", D0_SIZE = "nowayos_device_0_size"],
    [D1_VADDR = "nowayos_device_1_vaddr", D1_SIZE = "nowayos_device_1_size"],
    [D2_VADDR = "nowayos_device_2_vaddr", D2_SIZE = "nowayos_device_2_size"],
    [D3_VADDR = "nowayos_device_3_vaddr", D3_SIZE = "nowayos_device_3_size"],
);

// ---- Register blocks -------------------------------------------------------

/// A device's register block: a layout this crate may view a device's
/// registers through.
///
/// # Safety
///
/// An implementor must be `repr(C)`, with every register at the offset and
/// of the width its device's documentation gives (each checked at compile
/// time), and made only of `safe-mmio` field wrappers, arrays of them, and
/// padding that is never accessed. Each wrapper must match what an access
/// to that register does, as `safe-mmio`'s `UniqueMmioPointer::new`
/// requires: `ReadPure` or `ReadPureWrite` only where a read has no side
/// effect; `ReadOnly`, `ReadWrite` or `WriteOnly` only where the access is
/// safe to make at any time - in particular, no register that can make the
/// device read or write memory (DMA) is ever reachable through a safe
/// wrapper.
pub(crate) unsafe trait RegisterBlock {}

// ---- Devices ---------------------------------------------------------------

/// A device's register window that the system description maps into this
/// PD, bound but not yet viewed.
///
/// Get one from [`device`], then hand it to the device's module, such as
/// [`Pl011::new`](crate::pl011::Pl011::new), which views it once.
#[derive(Debug)]
pub struct Device {
    vaddr: usize,
    size: usize,
}

/// Binds device `index` (0 to 3) from the patched symbols
/// `nowayos_device_{index}_vaddr` and `nowayos_device_{index}_size`. Each
/// device can be bound once.
///
/// # Errors
///
/// [`HwError::NoSuchDevice`] if `index` is not below [`DEVICES`];
/// [`HwError::NotMapped`] or [`HwError::SizeUnknown`] if the system
/// description does not bind the symbols; [`HwError::AlreadyTaken`] the
/// second time.
pub fn device(index: usize) -> Result<Device, HwError> {
    let patched = DEVICE_SYMBOLS
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
    if patched.taken.swap(true, Ordering::Relaxed) {
        return Err(HwError::AlreadyTaken {
            symbol: patched.vaddr_symbol,
        });
    }
    Ok(Device { vaddr, size })
}

impl Device {
    /// The virtual address the device's registers are mapped at in this PD.
    #[must_use]
    pub const fn vaddr(&self) -> usize {
        self.vaddr
    }

    /// The size of the mapping in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// A device over test memory, for other modules' tests: `vaddr` is
    /// the exposed address of memory standing in for the registers, valid
    /// for `size` bytes while the device or its view is used - or an
    /// address never read, for a test that a view is refused first.
    #[cfg(test)]
    pub(crate) const fn for_test(vaddr: usize, size: usize) -> Self {
        Self { vaddr, size }
    }

    /// Views the device's registers as a `T`, consuming the device, so its
    /// registers only ever have one view.
    pub(crate) fn view<T: RegisterBlock>(self) -> Result<UniqueMmioPointer<'static, T>, HwError> {
        check_fits::<T>(self.vaddr, self.size)?;
        // The registers are device memory the Microkit tool mapped for us,
        // outside any Rust allocation, so the pointer carries exposed
        // provenance.
        let ptr = NonNull::new(core::ptr::with_exposed_provenance_mut::<T>(self.vaddr))
            .ok_or(HwError::OutOfRange { vaddr: self.vaddr })?;
        // SAFETY: `UniqueMmioPointer::new` needs a pointer that is properly
        // aligned and valid for MMIO of a `T`, mapped as device memory, with
        // no other alias used while the view exists, and a `T` whose wrapped
        // fields are safe to access.
        // * Aligned, and `size_of::<T>()` bytes from it inside the mapping
        //   without wrapping: checked above.
        // * Mapped for the rest of the program as this device's registers,
        //   uncached: the Microkit tool patched `vaddr` and `size` from one
        //   `<map>` element, Microkit never unmaps a region while the PD
        //   exists, and tools/check-system refuses, when the image is built,
        //   a `nowayos_device_*` symbol on anything but an uncached map of a
        //   device's exact register window named in the board file.
        // * No other alias: a device is bound once (`device`) and viewed
        //   once (`view` consumes it); the description maps a memory region
        //   at most once into a PD and a device into one PD only (both
        //   checked by tools/check-system). The kernel itself writes to the
        //   UART in Microkit's debug configuration, but not through this
        //   PD's address space, so no Rust alias exists.
        // * The fields are safe to access: `T: RegisterBlock`.
        Ok(unsafe { UniqueMmioPointer::new(ptr) })
    }
}

impl Device {
    /// Views the whole window as 32-bit words that have no safe access at
    /// all, consuming the device: for a device whose own registers say where
    /// its other registers are (an xHCI controller's capability registers),
    /// so no fixed layout fits it. The device's module reads and
    /// writes each word through `safe-mmio`'s `read_unsafe` and
    /// `write_unsafe`, stating at each why that access is safe, and checks
    /// every index against the window first (`get` refuses one past it).
    pub(crate) fn view_words(self) -> Result<UniqueMmioPointer<'static, [u32]>, HwError> {
        let align = align_of::<u32>();
        if !self.vaddr.is_multiple_of(align) {
            return Err(HwError::Misaligned {
                vaddr: self.vaddr,
                align,
            });
        }
        let words = self.size.checked_div(size_of::<u32>()).unwrap_or(0);
        if words == 0 {
            return Err(HwError::TooSmall {
                size: self.size,
                needed: size_of::<u32>(),
            });
        }
        if self.vaddr.checked_add(self.size).is_none() {
            return Err(HwError::OutOfRange { vaddr: self.vaddr });
        }
        let ptr = NonNull::new(core::ptr::with_exposed_provenance_mut::<u32>(self.vaddr))
            .ok_or(HwError::OutOfRange { vaddr: self.vaddr })?;
        let words = NonNull::slice_from_raw_parts(ptr, words);
        // SAFETY: `UniqueMmioPointer::new` needs a pointer that is properly
        // aligned and valid for MMIO of the slice, mapped as device memory,
        // with no other alias used while the view exists; any wrapped field
        // must be safe to access.
        // * Aligned to 4, and the words inside the mapping without wrapping:
        //   checked above.
        // * Mapped as this device's registers, uncached, for the rest of the
        //   program, and no other alias: as for `view` - the Microkit tool
        //   patched `vaddr` and `size` from one `<map>`, tools/check-system
        //   allows a `nowayos_device_*` symbol only on an uncached map of a
        //   device's exact window, into one PD only, and the device is bound
        //   once and viewed once.
        // * No wrapped field: the words are plain `u32`, which safe-mmio
        //   reads and writes only through its `unsafe` methods.
        Ok(unsafe { UniqueMmioPointer::new(words) })
    }
}

/// Checks that a `T` fits at `vaddr` in a mapping of `size` bytes: aligned,
/// large enough, and not running past the end of the address space.
pub(crate) fn check_fits<T>(vaddr: usize, size: usize) -> Result<(), HwError> {
    let align = align_of::<T>();
    if !vaddr.is_multiple_of(align) {
        return Err(HwError::Misaligned { vaddr, align });
    }
    let needed = size_of::<T>();
    if size < needed {
        return Err(HwError::TooSmall { size, needed });
    }
    if vaddr.checked_add(needed).is_none() {
        return Err(HwError::OutOfRange { vaddr });
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! These run under Miri too: binding and viewing make no register
    //! access, so no inline assembly runs.

    use super::*;

    /// A page of test memory standing in for a device's register window.
    #[repr(C, align(4096))]
    struct TestPage([u32; 1024]);

    static PAGE: TestPage = TestPage([0; 1024]);

    /// A register block for these tests only: one word.
    #[repr(C)]
    struct OneWord(safe_mmio::fields::ReadPure<u32>);

    // SAFETY: test only, viewed over `PAGE`, which is ordinary memory where
    // a read has no side effect; no register is ever accessed in these
    // tests.
    unsafe impl RegisterBlock for OneWord {}

    /// A register block bigger than a page.
    #[repr(C)]
    struct TwoPages([safe_mmio::fields::ReadPure<u32>; 2048]);

    // SAFETY: as for `OneWord`; only ever refused by `check_fits`.
    unsafe impl RegisterBlock for TwoPages {}

    fn page_vaddr() -> usize {
        core::ptr::from_ref(&PAGE).expose_provenance()
    }

    /// Patches device `index`'s symbols as the Microkit tool would.
    fn patch(index: usize, vaddr: usize, size: usize) {
        DEVICE_SYMBOLS[index].vaddr.store(vaddr, Ordering::Relaxed);
        DEVICE_SYMBOLS[index].size.store(size, Ordering::Relaxed);
    }

    // Each test uses its own device number: the symbols are global, and
    // tests run in parallel.

    #[test]
    fn a_patched_device_binds_once_and_views_its_registers() {
        patch(0, page_vaddr(), 4096);
        let device = device(0).unwrap();
        assert_eq!((device.vaddr(), device.size()), (page_vaddr(), 4096));
        let mut view = device.view::<OneWord>().unwrap();
        assert_eq!(view.ptr_mut().addr(), page_vaddr());
        assert_eq!(
            super::device(0).unwrap_err(),
            HwError::AlreadyTaken {
                symbol: "nowayos_device_0_vaddr"
            }
        );
    }

    #[test]
    fn unpatched_symbols_are_reported_by_name() {
        assert_eq!(
            device(1).unwrap_err(),
            HwError::NotMapped {
                symbol: "nowayos_device_1_vaddr"
            }
        );
        patch(2, page_vaddr(), 0);
        assert_eq!(
            device(2).unwrap_err(),
            HwError::SizeUnknown {
                symbol: "nowayos_device_2_size"
            }
        );
        assert_eq!(
            device(DEVICES).unwrap_err(),
            HwError::NoSuchDevice { index: DEVICES }
        );
    }

    #[test]
    fn a_view_must_fit_the_mapping() {
        patch(3, page_vaddr(), 4096);
        assert_eq!(
            device(3).unwrap().view::<TwoPages>().unwrap_err(),
            HwError::TooSmall {
                size: 4096,
                needed: 8192
            }
        );
    }

    #[test]
    fn check_fits_refuses_misaligned_short_and_wrapping_views() {
        assert_eq!(
            check_fits::<OneWord>(0x1002, 4096),
            Err(HwError::Misaligned {
                vaddr: 0x1002,
                align: 4
            })
        );
        assert_eq!(
            check_fits::<OneWord>(0x1000, 3),
            Err(HwError::TooSmall { size: 3, needed: 4 })
        );
        assert_eq!(
            check_fits::<OneWord>(usize::MAX - 3, 4096),
            Err(HwError::OutOfRange {
                vaddr: usize::MAX - 3
            })
        );
        assert_eq!(check_fits::<OneWord>(0x1000, 4), Ok(()));
        assert_eq!(check_fits::<OneWord>(usize::MAX - 7, 4), Ok(()));
    }
}
