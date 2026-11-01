// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`BoundedVec`]: a vector with a compile-time capacity.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::{Deref, DerefMut};

use crate::CapacityError;

/// A vector that holds at most `N` items, stored inline.
///
/// Storage is a fully initialised `[T; N]`; the first [`len`](Self::len)
/// slots are the vector's items and the rest hold `T::default()`. It
/// dereferences to a slice of the live items, so every read-only slice
/// method (`iter`, `get`, `first`, `binary_search`, …) is available.
///
/// Operations that would exceed `N` fail and return the rejected value; the
/// vector is left unchanged.
///
/// ```
/// use nowayos_bounded::BoundedVec;
///
/// let mut v: BoundedVec<u8, 2> = BoundedVec::new();
/// assert_eq!(v.push(1), Ok(()));
/// assert_eq!(v.push(2), Ok(()));
/// assert_eq!(v.push(3), Err(3)); // full: the value comes back
/// assert_eq!(v.as_slice(), &[1, 2]);
/// ```
#[derive(Clone)]
pub struct BoundedVec<T, const N: usize> {
    items: [T; N],
    len: usize,
}

impl<T: Default, const N: usize> BoundedVec<T, N> {
    /// Creates an empty vector. Every slot starts as `T::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: core::array::from_fn(|_| T::default()),
            len: 0,
        }
    }

    /// Creates a vector holding a copy of `values`.
    ///
    /// # Errors
    ///
    /// [`CapacityError`] if `values` has more than `N` items.
    pub fn from_slice(values: &[T]) -> Result<Self, CapacityError>
    where
        T: Clone,
    {
        let mut vec = Self::new();
        vec.extend_from_slice(values)?;
        Ok(vec)
    }

    /// Removes and returns the last item, or `None` if the vector is empty.
    ///
    /// The vacated slot is reset to `T::default()`, which may leave bytes
    /// of an enum payload behind (see [`Wipe`](crate::Wipe)). The same
    /// applies to [`remove`](Self::remove), [`swap_remove`](Self::swap_remove),
    /// [`truncate`](Self::truncate), [`clear`](Self::clear) and
    /// [`retain`](Self::retain): wipe an item in place first if its data must
    /// not linger and its value is not needed.
    pub fn pop(&mut self) -> Option<T> {
        let last = self.len.checked_sub(1)?;
        let value = core::mem::take(self.items.get_mut(last)?);
        self.len = last;
        Some(value)
    }

    /// Removes and returns the item at `index`, shifting later items down.
    ///
    /// Returns `None` (and changes nothing) if `index` is out of range.
    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        }
        // Rotate the removed item to the end of the live range, then pop it.
        self.items.get_mut(index..self.len)?.rotate_left(1);
        self.pop()
    }

    /// Removes and returns the item at `index`, moving the last item into its
    /// place. O(1), but does not preserve order.
    ///
    /// Returns `None` (and changes nothing) if `index` is out of range.
    pub fn swap_remove(&mut self, index: usize) -> Option<T> {
        let last = self.len.checked_sub(1)?;
        if index > last {
            return None;
        }
        self.as_mut_slice().swap(index, last);
        self.pop()
    }

    /// Shortens the vector to `len` items, resetting the removed slots to
    /// `T::default()`. Does nothing if `len` is not shorter than the vector.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        if let Some(removed) = self.items.get_mut(len..self.len) {
            for slot in removed {
                *slot = T::default();
            }
        }
        self.len = len;
    }

    /// Removes every item, resetting every slot to `T::default()`. That may
    /// leave bytes of enum payloads behind; [`Wipe::wipe`](crate::Wipe::wipe)
    /// overwrites the items in place first.
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Keeps only the items for which `keep` returns `true`, preserving their
    /// order.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let mut kept = 0usize;
        for index in 0..self.len {
            let keep_this = self.items.get(index).is_some_and(&mut keep);
            if keep_this {
                if let Some(live) = self.items.get_mut(..self.len) {
                    live.swap(kept, index);
                }
                kept = kept.saturating_add(1);
            }
        }
        self.truncate(kept);
    }
}

impl<T, const N: usize> BoundedVec<T, N> {
    /// The maximum number of items, `N`.
    pub const CAPACITY: usize = N;

    /// The maximum number of items, `N`.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The number of items currently held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the vector holds no items.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the vector holds `N` items, so the next push will fail.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len >= N
    }

    /// How many more items fit.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        N.saturating_sub(self.len)
    }

    /// Appends `value`.
    ///
    /// # Errors
    ///
    /// Returns `Err(value)` — handing the value back — if the vector is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let Some(new_len) = self.len.checked_add(1) else {
            return Err(value);
        };
        match self.items.get_mut(self.len) {
            Some(slot) => {
                *slot = value;
                self.len = new_len;
                Ok(())
            }
            None => Err(value),
        }
    }

    /// Inserts `value` at `index`, shifting later items up.
    ///
    /// # Errors
    ///
    /// Returns `Err(value)` if the vector is full or `index > len`.
    pub fn insert(&mut self, index: usize, value: T) -> Result<(), T> {
        if index > self.len {
            return Err(value);
        }
        self.push(value)?;
        // The new value is last; rotate it back to `index`.
        if let Some(shifted) = self.items.get_mut(index..self.len) {
            shifted.rotate_right(1);
        }
        Ok(())
    }

    /// Appends clones of all of `values`, or none of them.
    ///
    /// # Errors
    ///
    /// [`CapacityError`] if they do not all fit; the vector is unchanged.
    pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), CapacityError>
    where
        T: Clone,
    {
        let required = self.len.saturating_add(values.len());
        if required > N {
            return Err(CapacityError {
                capacity: N,
                required,
            });
        }
        let target = self
            .items
            .get_mut(self.len..required)
            .ok_or(CapacityError {
                capacity: N,
                required,
            })?;
        target.clone_from_slice(values);
        self.len = required;
        Ok(())
    }

    /// The live items as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.items.get(..self.len).unwrap_or(&[])
    }

    /// The live items as a mutable slice. Items can be changed or reordered,
    /// but the length cannot.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.items.get_mut(..self.len).unwrap_or(&mut [])
    }
}

impl<T: Default, const N: usize> Default for BoundedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for BoundedVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for BoundedVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for BoundedVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl<T: PartialEq, const N: usize> PartialEq for BoundedVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq, const N: usize> Eq for BoundedVec<T, N> {}

impl<T: Hash, const N: usize> Hash for BoundedVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl<T: Clone + Default, const N: usize> TryFrom<&[T]> for BoundedVec<T, N> {
    type Error = CapacityError;

    fn try_from(values: &[T]) -> Result<Self, CapacityError> {
        Self::from_slice(values)
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a BoundedVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut BoundedVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn push_until_full_then_reject() {
        let mut v: BoundedVec<u32, 3> = BoundedVec::new();
        assert!(v.is_empty());
        for i in 0..3 {
            v.push(i).unwrap();
        }
        assert!(v.is_full());
        assert_eq!(v.remaining(), 0);
        assert_eq!(v.push(99), Err(99));
        assert_eq!(v.as_slice(), &[0, 1, 2]);
    }

    #[test]
    fn zero_capacity_is_always_full() {
        let mut v: BoundedVec<u8, 0> = BoundedVec::new();
        assert!(v.is_full());
        assert_eq!(v.push(1), Err(1));
        assert_eq!(v.pop(), None);
        assert_eq!(v.remove(0), None);
        assert_eq!(v.insert(0, 1), Err(1));
    }

    #[test]
    fn pop_resets_the_vacated_slot() {
        let mut v: BoundedVec<u8, 2> = BoundedVec::from_slice(&[7, 8]).unwrap();
        assert_eq!(v.pop(), Some(8));
        assert_eq!(v.items, [7, 0]);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn remove_preserves_order() {
        let mut v: BoundedVec<u8, 5> = BoundedVec::from_slice(&[1, 2, 3, 4]).unwrap();
        assert_eq!(v.remove(1), Some(2));
        assert_eq!(v.as_slice(), &[1, 3, 4]);
        assert_eq!(v.remove(3), None);
        assert_eq!(v.items, [1, 3, 4, 0, 0]);
    }

    #[test]
    fn swap_remove_moves_last_into_place() {
        let mut v: BoundedVec<u8, 4> = BoundedVec::from_slice(&[1, 2, 3, 4]).unwrap();
        assert_eq!(v.swap_remove(0), Some(1));
        assert_eq!(v.as_slice(), &[4, 2, 3]);
        assert_eq!(v.swap_remove(3), None);
    }

    #[test]
    fn insert_shifts_up_and_rejects_bad_index() {
        let mut v: BoundedVec<u8, 4> = BoundedVec::from_slice(&[1, 3]).unwrap();
        v.insert(1, 2).unwrap();
        v.insert(3, 4).unwrap();
        assert_eq!(v.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(v.insert(0, 0), Err(0)); // full
        let mut w: BoundedVec<u8, 4> = BoundedVec::new();
        assert_eq!(w.insert(1, 5), Err(5)); // past the end
    }

    #[test]
    fn extend_is_all_or_nothing() {
        let mut v: BoundedVec<u8, 3> = BoundedVec::from_slice(&[1]).unwrap();
        let err = v.extend_from_slice(&[2, 3, 4]).unwrap_err();
        assert_eq!(
            err,
            CapacityError {
                capacity: 3,
                required: 4
            }
        );
        assert_eq!(v.as_slice(), &[1]);
        v.extend_from_slice(&[2, 3]).unwrap();
        assert_eq!(v.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn clear_and_truncate_reset_slots() {
        let mut v: BoundedVec<u8, 4> = BoundedVec::from_slice(&[1, 2, 3, 4]).unwrap();
        v.truncate(2);
        assert_eq!(v.items, [1, 2, 0, 0]);
        v.truncate(10);
        assert_eq!(v.len(), 2);
        v.clear();
        assert_eq!(v.items, [0; 4]);
        assert!(v.is_empty());
    }

    #[test]
    fn retain_keeps_order() {
        let mut v: BoundedVec<u8, 6> = BoundedVec::from_slice(&[1, 2, 3, 4, 5, 6]).unwrap();
        v.retain(|x| x % 2 == 0);
        assert_eq!(v.as_slice(), &[2, 4, 6]);
        assert_eq!(v.items[3..], [0, 0, 0]);
    }

    #[test]
    fn equality_ignores_spare_slots() {
        let mut a: BoundedVec<u8, 3> = BoundedVec::from_slice(&[1, 2]).unwrap();
        let b: BoundedVec<u8, 3> = BoundedVec::from_slice(&[1]).unwrap();
        assert_ne!(a, b);
        a.pop();
        assert_eq!(a, b);
    }

    #[test]
    fn works_with_non_copy_items() {
        #[derive(Debug, Default, PartialEq)]
        struct Owned(Option<std::boxed::Box<u8>>);
        let mut v: BoundedVec<Owned, 2> = BoundedVec::new();
        v.push(Owned(Some(std::boxed::Box::new(1)))).unwrap();
        assert_eq!(v.pop(), Some(Owned(Some(std::boxed::Box::new(1)))));
    }
}
