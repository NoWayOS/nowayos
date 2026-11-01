// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`BoundedQueue`]: a first-in, first-out ring with a compile-time capacity.

use core::fmt;

/// A first-in, first-out queue of at most `N` items, stored inline as a ring.
///
/// Pushing onto a full queue fails and hands the value back
/// ([`push_back`](Self::push_back)); a caller that prefers to drop the
/// oldest item instead uses [`force_push_back`](Self::force_push_back). Which
/// policy is right is the caller's decision — for an input-event queue,
/// dropping new events under flood is usually safer than dropping old ones.
///
/// ```
/// use nowayos_bounded::BoundedQueue;
///
/// let mut q: BoundedQueue<u8, 2> = BoundedQueue::new();
/// q.push_back(1).unwrap();
/// q.push_back(2).unwrap();
/// assert_eq!(q.push_back(3), Err(3));
/// assert_eq!(q.pop_front(), Some(1));
/// q.push_back(3).unwrap(); // wraps around the ring
/// assert_eq!(q.iter().copied().collect::<Vec<_>>(), [2, 3]);
/// ```
#[derive(Clone)]
pub struct BoundedQueue<T, const N: usize> {
    items: [T; N],
    /// Physical index of the front item.
    head: usize,
    len: usize,
}

impl<T: Default, const N: usize> BoundedQueue<T, N> {
    /// Creates an empty queue. Every slot starts as `T::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: core::array::from_fn(|_| T::default()),
            head: 0,
            len: 0,
        }
    }

    /// Removes and returns the front (oldest) item, resetting its slot to
    /// `T::default()`. Returns `None` if the queue is empty.
    ///
    /// Resetting the slot may leave bytes of an enum payload behind (see
    /// [`Wipe`](crate::Wipe)); wipe the item in place first if its data
    /// must not linger and its value is not needed.
    pub fn pop_front(&mut self) -> Option<T> {
        let remaining = self.len.checked_sub(1)?;
        // Work out the new head before changing anything, so every early
        // return leaves the queue as it was.
        let next_head = if remaining == 0 {
            0
        } else {
            self.head.checked_add(1)?.checked_rem(N)?
        };
        let value = core::mem::take(self.items.get_mut(self.head)?);
        self.head = next_head;
        self.len = remaining;
        Some(value)
    }

    /// Appends `value`, dropping the oldest item if the queue is full.
    ///
    /// Returns the item that was dropped to make room, if any. With `N == 0`
    /// nothing can be stored, and `value` itself is returned.
    pub fn force_push_back(&mut self, value: T) -> Option<T> {
        match self.push_back(value) {
            Ok(()) => None,
            Err(value) => {
                let evicted = self.pop_front();
                match self.push_back(value) {
                    Ok(()) => evicted,
                    Err(value) => Some(value),
                }
            }
        }
    }

    /// Removes every item, resetting every slot to `T::default()`. That may
    /// leave bytes of enum payloads behind; [`Wipe::wipe`](crate::Wipe::wipe)
    /// overwrites the items in place first.
    pub fn clear(&mut self) {
        for slot in &mut self.items {
            *slot = T::default();
        }
        self.head = 0;
        self.len = 0;
    }
}

impl<T: crate::Wipe, const N: usize> BoundedQueue<T, N> {
    /// Wipes every queued item in place, front to back, leaving the queue's
    /// length unchanged. [`Wipe for BoundedQueue`](crate::Wipe) then
    /// empties it.
    pub(crate) fn wipe_items(&mut self) {
        for position in 0..self.len {
            let item = self
                .physical_index(position)
                .and_then(|index| self.items.get_mut(index));
            if let Some(item) = item {
                item.wipe();
            }
        }
    }
}

impl<T, const N: usize> BoundedQueue<T, N> {
    /// The maximum number of items, `N`.
    pub const CAPACITY: usize = N;

    /// The maximum number of items, `N`.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The number of items currently queued.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the queue holds no items.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the queue holds `N` items, so the next push will fail.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len >= N
    }

    /// How many more items fit.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        N.saturating_sub(self.len)
    }

    /// Appends `value` at the back.
    ///
    /// # Errors
    ///
    /// Returns `Err(value)` — handing the value back — if the queue is full.
    pub fn push_back(&mut self, value: T) -> Result<(), T> {
        if self.len >= N {
            return Err(value);
        }
        let Some(new_len) = self.len.checked_add(1) else {
            return Err(value);
        };
        let Some(index) = self.physical_index(self.len) else {
            return Err(value);
        };
        match self.items.get_mut(index) {
            Some(slot) => {
                *slot = value;
                self.len = new_len;
                Ok(())
            }
            None => Err(value),
        }
    }

    /// The front (oldest) item, if any.
    #[must_use]
    pub fn front(&self) -> Option<&T> {
        self.get(0)
    }

    /// The back (newest) item, if any.
    #[must_use]
    pub fn back(&self) -> Option<&T> {
        self.get(self.len.checked_sub(1)?)
    }

    /// The item `position` places from the front, if there is one.
    #[must_use]
    pub fn get(&self, position: usize) -> Option<&T> {
        if position >= self.len {
            return None;
        }
        self.items.get(self.physical_index(position)?)
    }

    /// Iterates from front (oldest) to back (newest).
    #[must_use]
    pub fn iter(&self) -> Iter<'_, T, N> {
        Iter {
            queue: self,
            position: 0,
        }
    }

    /// Maps a logical position (0 = front) to an index into `items`.
    /// `None` only if `N == 0`, which callers already exclude.
    fn physical_index(&self, position: usize) -> Option<usize> {
        self.head.checked_add(position)?.checked_rem(N)
    }
}

impl<T: Default, const N: usize> Default for BoundedQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for BoundedQueue<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: PartialEq, const N: usize> PartialEq for BoundedQueue<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq, const N: usize> Eq for BoundedQueue<T, N> {}

impl<'a, T, const N: usize> IntoIterator for &'a BoundedQueue<T, N> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T, N>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over a [`BoundedQueue`], front to back.
#[derive(Debug, Clone)]
pub struct Iter<'a, T, const N: usize> {
    queue: &'a BoundedQueue<T, N>,
    position: usize,
}

impl<'a, T, const N: usize> Iterator for Iter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let item = self.queue.get(self.position)?;
        self.position = self.position.saturating_add(1);
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.queue.len.saturating_sub(self.position);
        (left, Some(left))
    }
}

impl<T, const N: usize> ExactSizeIterator for Iter<'_, T, N> {}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    fn contents<const N: usize>(q: &BoundedQueue<u8, N>) -> std::vec::Vec<u8> {
        q.iter().copied().collect()
    }

    #[test]
    fn fifo_order() {
        let mut q: BoundedQueue<u8, 4> = BoundedQueue::new();
        for i in 1..=4 {
            q.push_back(i).unwrap();
        }
        assert!(q.is_full());
        assert_eq!(q.push_back(5), Err(5));
        assert_eq!(q.front(), Some(&1));
        assert_eq!(q.back(), Some(&4));
        for i in 1..=4 {
            assert_eq!(q.pop_front(), Some(i));
        }
        assert_eq!(q.pop_front(), None);
    }

    #[test]
    fn wraps_around_repeatedly() {
        let mut q: BoundedQueue<u8, 3> = BoundedQueue::new();
        for round in 0..10u8 {
            q.push_back(round).unwrap();
            q.push_back(round.wrapping_add(100)).unwrap();
            assert_eq!(q.pop_front(), Some(round));
            assert_eq!(q.pop_front(), Some(round.wrapping_add(100)));
            assert!(q.is_empty());
        }
    }

    #[test]
    fn pop_resets_slots_and_clear_wipes_all() {
        let mut q: BoundedQueue<u8, 3> = BoundedQueue::new();
        q.push_back(7).unwrap();
        q.push_back(8).unwrap();
        q.pop_front();
        assert_eq!(q.items, [0, 8, 0]);
        q.clear();
        assert_eq!(q.items, [0, 0, 0]);
        assert!(q.is_empty());
    }

    #[test]
    fn force_push_evicts_oldest() {
        let mut q: BoundedQueue<u8, 2> = BoundedQueue::new();
        assert_eq!(q.force_push_back(1), None);
        assert_eq!(q.force_push_back(2), None);
        assert_eq!(q.force_push_back(3), Some(1));
        assert_eq!(contents(&q), [2, 3]);
    }

    #[test]
    fn zero_capacity_rejects_everything() {
        let mut q: BoundedQueue<u8, 0> = BoundedQueue::new();
        assert_eq!(q.push_back(1), Err(1));
        assert_eq!(q.force_push_back(2), Some(2));
        assert_eq!(q.pop_front(), None);
        assert_eq!(q.front(), None);
        assert_eq!(q.iter().count(), 0);
    }

    #[test]
    fn iterator_reports_exact_size() {
        let mut q: BoundedQueue<u8, 4> = BoundedQueue::new();
        q.push_back(1).unwrap();
        q.push_back(2).unwrap();
        let mut it = q.iter();
        assert_eq!(it.len(), 2);
        it.next();
        assert_eq!(it.len(), 1);
    }
}
