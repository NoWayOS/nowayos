// SPDX-License-Identifier: MIT OR Apache-2.0
//! A heap whose arena is a memory region: for a PD whose working
//! memory is large - a decoder's caches and coefficients - so that it is
//! not in the program image. A static arena is part of the image, which the
//! loader copies at boot, at a few MB a second on a Pi 5 with its caches
//! off; a memory region is seL4's own memory, zeroed when it is made, and
//! never in the image.
//!
//! The Microkit tool writes the `<map>`'s address and size into this
//! module's two symbols, `nowayos_heap_region_vaddr` and
//! `nowayos_heap_region_size` (`setvar_vaddr` and `setvar_size`), as it does
//! nowayos-region's. Their attributes are `unsafe` for the reasons
//! nowayos-region's `mapping` module gives: the names are defined nowhere
//! else, and `.data` is where the tool can rewrite them. The heap's arena is
//! only ever that address and size, which `tools/check-system` holds to a
//! read-write map of ordinary memory that no other PD maps (its rule 16):
//! the arena is this PD's alone, as a static arena would be.

use core::sync::atomic::{AtomicUsize, Ordering};

use one_shot_mutex::sync::RawOneShotMutex;
use sel4_dlmalloc::{DeferredStaticDlmalloc, StaticHeapBounds};

use crate::CountingHeap;

#[unsafe(export_name = "nowayos_heap_region_vaddr")]
#[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
static VADDR: AtomicUsize = AtomicUsize::new(0);

#[unsafe(export_name = "nowayos_heap_region_size")]
#[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
static SIZE: AtomicUsize = AtomicUsize::new(0);

/// A PD's heap in a memory region: dlmalloc over the region, counted. It
/// serves nothing until [`give_region`] gives it the region.
pub type RegionPdHeap = CountingHeap<DeferredStaticDlmalloc<RawOneShotMutex>>;

/// Why the heap has no region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionHeapError {
    /// The system description maps no region with the heap's symbols.
    NotMapped,
    /// The heap was given its region before.
    AlreadyGiven,
    /// The region does not start on a page, as every region a system
    /// description maps does.
    Misaligned,
}

/// A page: where every mapped region starts.
const PAGE: usize = 4096;

/// Gives `heap` its arena: the memory region the system description maps
/// with `setvar_vaddr="nowayos_heap_region_vaddr"` and
/// `setvar_size="nowayos_heap_region_size"`. Call it first in the PD's
/// `init`, before anything allocates. Returns the arena's size in bytes.
///
/// # Errors
///
/// [`RegionHeapError::NotMapped`] if no such region is mapped;
/// [`RegionHeapError::Misaligned`] if it does not start on a page;
/// [`RegionHeapError::AlreadyGiven`] if `heap` has one.
pub fn give_region(heap: &RegionPdHeap) -> Result<usize, RegionHeapError> {
    let (vaddr, size) = (VADDR.load(Ordering::Relaxed), SIZE.load(Ordering::Relaxed));
    if vaddr == 0 || size == 0 {
        return Err(RegionHeapError::NotMapped);
    }
    if vaddr % PAGE != 0 {
        return Err(RegionHeapError::Misaligned);
    }
    let start = core::ptr::with_exposed_provenance_mut::<u8>(vaddr);
    heap.inner()
        .set_bounds(StaticHeapBounds::new(start, size))
        .map_err(|_| RegionHeapError::AlreadyGiven)?;
    Ok(size)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::undocumented_unsafe_blocks,
    reason = "test code: a failed check panics; the one unsafe call is the allocator's own"
)]
mod tests {
    extern crate std;

    use super::*;
    use core::alloc::{GlobalAlloc, Layout};
    use std::boxed::Box;

    /// A test arena where a region would be: on a page.
    #[repr(C, align(4096))]
    struct Arena([u8; 1 << 20]);

    #[test]
    fn a_heap_serves_from_its_region_once_given_it() {
        let heap: RegionPdHeap = CountingHeap::new(DeferredStaticDlmalloc::new(), None);
        // No region mapped: nothing to give.
        assert_eq!(give_region(&heap), Err(RegionHeapError::NotMapped));
        // The tool's patch, as a test arena: refused off a page.
        let mut arena = Box::new(Arena([0; 1 << 20]));
        let range = arena.0.as_mut_ptr_range();
        VADDR.store(range.start.expose_provenance() + 8, Ordering::Relaxed);
        SIZE.store(arena.0.len() - 8, Ordering::Relaxed);
        assert_eq!(give_region(&heap), Err(RegionHeapError::Misaligned));
        VADDR.store(range.start.expose_provenance(), Ordering::Relaxed);
        SIZE.store(arena.0.len(), Ordering::Relaxed);
        assert_eq!(give_region(&heap), Ok(1 << 20));
        assert_eq!(give_region(&heap), Err(RegionHeapError::AlreadyGiven));
        let layout = Layout::from_size_align(4096, 16).unwrap();
        let ptr = unsafe { heap.alloc(layout) };
        assert!(!ptr.is_null());
        assert!(range.contains(&ptr));
        assert_eq!(heap.in_use(), 4096);
        // More than the region holds is refused.
        let huge = Layout::from_size_align(2 << 20, 16).unwrap();
        assert!(unsafe { heap.alloc(huge) }.is_null());
        assert_eq!(heap.refusals(), 1);
        VADDR.store(0, Ordering::Relaxed);
        SIZE.store(0, Ordering::Relaxed);
        // The heap first: it points into the arena.
        drop(heap);
        drop(arena);
    }
}
