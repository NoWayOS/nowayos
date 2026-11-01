// SPDX-License-Identifier: MIT OR Apache-2.0
//! The counting wrapper: a `GlobalAlloc` that forwards every call to the
//! allocator it wraps and keeps three counts. All of the crate's unsafe
//! code is here.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Called when the wrapped allocator refuses a request: the request, and
/// the bytes in use when it came. It runs inside the allocator, so it must
/// not allocate (printing to the debug console does not).
pub type RefusalHook = fn(Layout, usize);

/// An allocator that counts what passes through it.
///
/// The counts are atomics so that a `static` of it is `Sync`; a PD has one
/// thread, so they never race. Each saturates rather than wraps.
#[derive(Debug)]
pub struct CountingHeap<A> {
    inner: A,
    in_use: AtomicUsize,
    peak: AtomicUsize,
    refusals: AtomicUsize,
    hook: Option<RefusalHook>,
}

impl<A> CountingHeap<A> {
    /// Wraps `inner`, calling `hook` on each refusal.
    pub const fn new(inner: A, hook: Option<RefusalHook>) -> Self {
        Self {
            inner,
            in_use: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            refusals: AtomicUsize::new(0),
            hook,
        }
    }

    /// The wrapped allocator.
    pub const fn inner(&self) -> &A {
        &self.inner
    }

    /// Bytes requested and not yet freed.
    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Relaxed)
    }

    /// The most bytes in use at once since the start, or since the last
    /// [`reset_peak`](Self::reset_peak).
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Starts the peak again from what is in use now.
    pub fn reset_peak(&self) {
        self.peak.store(self.in_use(), Ordering::Relaxed);
    }

    /// How many requests the wrapped allocator has refused.
    pub fn refusals(&self) -> usize {
        self.refusals.load(Ordering::Relaxed)
    }

    fn grew(&self, by: usize) {
        let now = self.in_use().saturating_add(by);
        self.in_use.store(now, Ordering::Relaxed);
        if now > self.peak() {
            self.peak.store(now, Ordering::Relaxed);
        }
    }

    fn shrank(&self, by: usize) {
        self.in_use
            .store(self.in_use().saturating_sub(by), Ordering::Relaxed);
    }

    fn refused(&self, layout: Layout) {
        self.refusals
            .store(self.refusals().saturating_add(1), Ordering::Relaxed);
        if let Some(hook) = self.hook {
            hook(layout, self.in_use());
        }
    }

    /// Counts the outcome of a request for `layout` that returned `ptr`.
    fn counted(&self, ptr: *mut u8, layout: Layout) -> *mut u8 {
        if ptr.is_null() {
            self.refused(layout);
        } else {
            self.grew(layout.size());
        }
        ptr
    }
}

// SAFETY: every method forwards to `inner` with exactly the arguments it
// was given, so `inner`'s guarantees are this allocator's; the counting
// around each call touches only the atomics, never the memory.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingHeap<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's guarantees for `alloc` (a non-zero size) are
        // passed on unchanged.
        let ptr = unsafe { self.inner.alloc(layout) };
        self.counted(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        self.counted(ptr, layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller promises `ptr` came from this allocator (so
        // from `inner`) with `layout`.
        unsafe { self.inner.dealloc(ptr, layout) };
        self.shrank(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller promises `ptr` came from this allocator with
        // `layout`, and that `new_size` is valid for it.
        let moved = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if moved.is_null() {
            // The old block is untouched and still counted.
            match Layout::from_size_align(new_size, layout.align()) {
                Ok(wanted) => self.refused(wanted),
                Err(_) => self.refused(layout),
            }
        } else {
            self.shrank(layout.size());
            self.grew(new_size);
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::multiple_unsafe_ops_per_block,
        reason = "test code: a failed check panics, and one block walks a sequence of calls"
    )]

    use std::alloc::System;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// An allocator that refuses everything.
    struct Refuses;

    // SAFETY: it never hands out memory, so it never breaks a promise.
    unsafe impl GlobalAlloc for Refuses {
        unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
            core::ptr::null_mut()
        }
        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    static HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn hook(layout: Layout, in_use: usize) {
        assert_eq!(layout.size(), 64);
        assert_eq!(in_use, 0);
        HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn counts_in_use_and_the_peak_through_alloc_realloc_and_dealloc() {
        let heap = CountingHeap::new(System, None);
        let small = Layout::from_size_align(100, 8).unwrap();
        let big = Layout::from_size_align(300, 8).unwrap();
        // SAFETY: non-zero sizes; each block is freed with its own layout.
        unsafe {
            let a = heap.alloc(small);
            let b = heap.alloc_zeroed(big);
            assert!(!a.is_null() && !b.is_null());
            assert_eq!(*b, 0);
            assert_eq!((heap.in_use(), heap.peak()), (400, 400));
            heap.dealloc(b, big);
            assert_eq!((heap.in_use(), heap.peak()), (100, 400));
            let a = heap.realloc(a, small, 1000);
            assert!(!a.is_null());
            assert_eq!((heap.in_use(), heap.peak()), (1000, 1000));
            heap.reset_peak();
            let a2 = heap.realloc(a, Layout::from_size_align(1000, 8).unwrap(), 10);
            assert_eq!((heap.in_use(), heap.peak()), (10, 1000));
            heap.dealloc(a2, Layout::from_size_align(10, 8).unwrap());
        }
        assert_eq!((heap.in_use(), heap.refusals()), (0, 0));
    }

    #[test]
    fn a_refusal_is_counted_reported_and_not_in_use() {
        let heap = CountingHeap::new(Refuses, Some(hook));
        let layout = Layout::from_size_align(64, 8).unwrap();
        // SAFETY: a non-zero size; nothing is handed out.
        let ptr = unsafe { heap.alloc(layout) };
        assert!(ptr.is_null());
        // SAFETY: as above.
        let ptr = unsafe { heap.alloc_zeroed(layout) };
        assert!(ptr.is_null());
        assert_eq!((heap.in_use(), heap.peak(), heap.refusals()), (0, 0, 2));
        assert_eq!(HOOK_CALLS.load(Ordering::Relaxed), 2);
    }
}
