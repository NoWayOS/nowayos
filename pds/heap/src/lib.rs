// SPDX-License-Identifier: MIT OR Apache-2.0
//! A protection domain's heap:
//! sel4-dlmalloc over a fixed static arena - what sel4-microkit's
//! `heap_size` declares - wrapped so that the PD can see what it uses.
//!
//! * The arena's size is a constant the PD sets from measurement; nothing
//!   grows it. When dlmalloc cannot serve a request the allocation fails,
//!   the PD's refusal hook reports it, and Rust's allocation-error handler
//!   stops the PD (a panic, which aborts): nothing carries on half done.
//! * [`CountingHeap`] counts the bytes requested and not yet freed, the
//!   peak since the last [`CountingHeap::reset_peak`], and the refusals.
//!   The counts are of what callers ask for, not dlmalloc's chunk headers,
//!   which is why an arena carries a margin above the peak it must hold.
//!
//! A PD declares its heap with [`declare_heap!`], in place of
//! `#[protection_domain(heap_size = ...)]`. A PD whose working memory is
//! large declares one in a memory region instead, [`declare_region_heap!`],
//! and gives it the region first in its `init` ([`give_region`]): a static
//! arena is part of the program image, which the loader copies at boot; a
//! region is not. Unsafe code is confined to two modules:
//! `counting`, the `GlobalAlloc` implementation that forwards each call to
//! the arena's allocator (Miri runs it, `make pds-miri`), and `region`, the
//! region's patched symbols (`tools/unsafe-allowlist.toml`).

#![cfg_attr(not(test), no_std)]

#[allow(
    unsafe_code,
    reason = "a global allocator is an `unsafe impl GlobalAlloc`, forwarding unsafe calls"
)]
mod counting;
#[allow(
    unsafe_code,
    reason = "the region's address and size are symbols the Microkit tool patches: `unsafe(export_name)` and `unsafe(link_section)`"
)]
mod region;

pub use counting::{CountingHeap, RefusalHook};
pub use one_shot_mutex::sync::RawOneShotMutex;
pub use region::{RegionHeapError, RegionPdHeap, give_region};
pub use sel4_dlmalloc::{DeferredStaticDlmalloc, StaticDlmalloc, StaticHeap};

/// A PD's heap: dlmalloc over a static arena, counted.
pub type PdHeap = CountingHeap<StaticDlmalloc<RawOneShotMutex>>;

/// Declares the PD's global allocator: a `static $name` [`PdHeap`] over a
/// static arena of `$size` bytes, calling `$hook` (a [`RefusalHook`], or
/// `None`) when the arena refuses a request.
///
/// ```ignore
/// nowayos_heap::declare_heap!(HEAP, ARENA_BYTES, Some(report_refusal));
/// ```
#[macro_export]
macro_rules! declare_heap {
    ($name:ident, $size:expr, $hook:expr) => {
        static ARENA: $crate::StaticHeap<{ $size }> = $crate::StaticHeap::new();

        #[global_allocator]
        static $name: $crate::PdHeap =
            $crate::CountingHeap::new($crate::StaticDlmalloc::new(ARENA.bounds()), $hook);
    };
}

/// Declares the PD's global allocator: a `static $name` [`RegionPdHeap`],
/// whose arena is the memory region the system description maps with
/// `setvar_vaddr="nowayos_heap_region_vaddr"` and
/// `setvar_size="nowayos_heap_region_size"`, calling `$hook` (a
/// [`RefusalHook`], or `None`) when the arena refuses a request. It serves
/// nothing until the PD's `init` gives it the region with [`give_region`].
///
/// ```ignore
/// nowayos_heap::declare_region_heap!(HEAP, Some(report_refusal));
/// // first in init:
/// nowayos_heap::give_region(&HEAP)?;
/// ```
#[macro_export]
macro_rules! declare_region_heap {
    ($name:ident, $hook:expr) => {
        #[global_allocator]
        static $name: $crate::RegionPdHeap =
            $crate::CountingHeap::new($crate::DeferredStaticDlmalloc::new(), $hook);
    };
}
