// SPDX-License-Identifier: MIT OR Apache-2.0
//! The cell that keeps the PD's one generator in a static.
//!
//! A Microkit PD runs one thread and handles its notifications one at a time
//! in it, so its generator needs no lock. A `static` must still be `Sync`, and
//! a request made while the generator is in use - re-entry, which no PD does -
//! must not get a second `&mut` to it: [`PdCell::with`] refuses re-entry
//! instead.

use core::cell::{Cell, UnsafeCell};

pub(crate) struct PdCell<T> {
    busy: Cell<bool>,
    value: UnsafeCell<T>,
}

// SAFETY: a protection domain has exactly one thread, so the cell is never
// reached from two threads at once, and `with` hands out at most one `&mut T`
// at a time, refusing re-entry. For a program with more than one thread this
// would be unsound: the crate is for single-threaded PDs only. (Its tests,
// run by a harness with several threads, take a lock first.)
unsafe impl<T: Send> Sync for PdCell<T> {}

impl<T> PdCell<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            busy: Cell::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Runs `f` on the value; `None`, without running it, if the value is
    /// already in use.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        if self.busy.replace(true) {
            return None;
        }
        // SAFETY: `busy` was false and is now true, so no other reference made
        // by `with` is live, and none can be made until it is cleared below;
        // the PD has one thread (see the `Sync` impl). PDs build with
        // panic = "abort", so `busy` is never left set by an unwinding `f`.
        let result = f(unsafe { &mut *self.value.get() });
        self.busy.set(false);
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::PdCell;

    #[test]
    fn with_refuses_reentry_and_frees_the_value_after() {
        let cell = PdCell::new(1u32);
        let inner = cell.with(|value| {
            *value = value.saturating_add(1);
            cell.with(|again| *again = again.saturating_add(10))
        });
        assert_eq!(inner, Some(None), "re-entry refused, the value untouched");
        assert_eq!(cell.with(|value| *value), Some(2));
    }
}
