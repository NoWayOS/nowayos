// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Wipe`]: overwriting data in place before it is let go.

use crate::{BoundedQueue, BoundedStr, BoundedVec};

/// Overwrites the data a value holds, in place, and leaves the value empty.
///
/// # Why removing is not enough
///
/// Removing an item from a container ([`BoundedVec::pop`], `remove`,
/// `truncate`, `clear`; [`BoundedQueue::pop_front`], `clear`) resets its
/// slot to `T::default()`. When the default is a smaller enum variant than
/// the item — `None` replacing a `Some(BoundedStr)`, or an `Empty` variant
/// replacing a `Text(..)` one — that assignment may write only the variant
/// tag, and the old payload's bytes can stay behind in the container's
/// storage. Wiping overwrites them while they are still part of the value.
/// So, for data that must not linger, such as what a person typed into a
/// kiosk:
///
/// * empty a container with [`wipe`](Self::wipe) rather than `clear`;
/// * wipe an item in place before removing it, if its value is not needed
///   (`if let Some(last) = v.last_mut() { last.wipe() }`, then `v.pop()`).
///
/// ```
/// use nowayos_bounded::{BoundedStr, BoundedVec, Wipe};
///
/// let mut answers: BoundedVec<Option<BoundedStr<16>>, 4> = BoundedVec::new();
/// answers.push(Some(BoundedStr::try_from_str("synthetic").unwrap())).unwrap();
/// answers.wipe(); // zeroes the string's bytes, then empties the vector
/// assert!(answers.is_empty());
/// ```
///
/// # Limits
///
/// Wiping is ordinary safe code. [`BoundedStr`] follows its zeroing with
/// [`core::hint::black_box`], which in practice stops the compiler removing
/// the stores even when nothing reads the value afterwards, but Rust
/// documents that hint as best effort only. Copies made earlier — by moves,
/// clones or a container shifting its items — are not reached. A
/// compiler-proof guarantee needs volatile writes (for example the
/// `zeroize` crate), which this crate, having no `unsafe`, does not make.
pub trait Wipe {
    /// Overwrites this value's data in place and leaves it empty.
    fn wipe(&mut self);
}

impl<const N: usize> Wipe for BoundedStr<N> {
    /// Zeroes every byte of storage ([`BoundedStr::clear`]).
    fn wipe(&mut self) {
        self.clear();
    }
}

impl<T: Wipe> Wipe for Option<T> {
    /// Wipes the value inside, if any, then becomes `None`.
    fn wipe(&mut self) {
        if let Some(value) = self {
            value.wipe();
        }
        *self = None;
    }
}

impl<T: Wipe, const N: usize> Wipe for [T; N] {
    /// Wipes every element.
    fn wipe(&mut self) {
        for element in self {
            element.wipe();
        }
    }
}

impl<T: Wipe + Default, const N: usize> Wipe for BoundedVec<T, N> {
    /// Wipes every item in place, then empties the vector.
    fn wipe(&mut self) {
        for item in self.as_mut_slice() {
            item.wipe();
        }
        self.clear();
    }
}

impl<T: Wipe + Default, const N: usize> Wipe for BoundedQueue<T, N> {
    /// Wipes every queued item in place, then empties the queue.
    fn wipe(&mut self) {
        self.wipe_items();
        self.clear();
    }
}

/// Implements [`Wipe`] for plain values: overwriting the value is wiping it.
macro_rules! wipe_by_default {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Wipe for $ty {
                /// Overwrites the value with its default (zero, `false`,
                /// `'\0'`).
                fn wipe(&mut self) {
                    *self = <$ty>::default();
                }
            }
        )*
    };
}

wipe_by_default!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, bool, char,
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::vec::Vec;

    std::thread_local! {
        /// What each `Probe` held when it was wiped, in order.
        static WIPED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }

    fn take_wiped() -> Vec<u8> {
        WIPED.with(|wiped| core::mem::take(&mut *wiped.borrow_mut()))
    }

    /// Stands in for data whose bytes safe code cannot inspect once its
    /// variant has gone: it records being wiped, and what it held then.
    #[derive(Debug, Default, Clone, PartialEq)]
    struct Probe(u8);

    impl Wipe for Probe {
        fn wipe(&mut self) {
            WIPED.with(|wiped| wiped.borrow_mut().push(self.0));
            self.0 = 0;
        }
    }

    /// The shape the review found leaking: a default variant smaller than
    /// the one holding text.
    #[derive(Debug, Default)]
    enum Field {
        #[default]
        Empty,
        Text(BoundedStr<8>),
    }

    impl Wipe for Field {
        fn wipe(&mut self) {
            if let Self::Text(text) = self {
                text.wipe();
            }
            *self = Self::Empty;
        }
    }

    fn synthetic() -> BoundedStr<8> {
        BoundedStr::try_from_str("qwerty").unwrap()
    }

    #[test]
    fn a_string_wipe_zeroes_every_byte() {
        let mut s = synthetic();
        s.wipe();
        assert!(s.is_empty());
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn an_option_wipes_its_value_before_becoming_none() {
        let mut probe = Some(Probe(7));
        probe.wipe();
        assert_eq!(probe, None);
        assert_eq!(take_wiped(), [7], "wiped while still Some");
        let mut none: Option<Probe> = None;
        none.wipe();
        assert_eq!(take_wiped(), []);
        let mut text = Some(synthetic());
        text.wipe();
        assert_eq!(text, None);
    }

    #[test]
    fn a_vec_wipes_each_live_item_in_place_then_empties() {
        let mut v: BoundedVec<Option<Probe>, 4> = BoundedVec::new();
        for item in [Some(Probe(1)), None, Some(Probe(3))] {
            v.push(item).unwrap();
        }
        v.wipe();
        assert!(v.is_empty());
        assert_eq!(take_wiped(), [1, 3]);
        // Empty again, so it can be reused.
        v.push(Some(Probe(9))).unwrap();
        assert_eq!(v.as_slice(), [Some(Probe(9))]);
    }

    #[test]
    fn a_queue_wipes_each_live_item_across_the_wrap() {
        let mut q: BoundedQueue<Probe, 3> = BoundedQueue::new();
        for value in 1..=3 {
            q.push_back(Probe(value)).unwrap();
        }
        q.pop_front();
        q.push_back(Probe(4)).unwrap(); // wraps round the ring
        assert_eq!(take_wiped(), []);
        q.wipe();
        assert!(q.is_empty());
        assert_eq!(take_wiped(), [2, 3, 4]);
    }

    #[test]
    fn enum_payloads_are_wiped_before_the_variant_changes() {
        let mut v: BoundedVec<Field, 2> = BoundedVec::new();
        v.push(Field::Text(synthetic())).unwrap();
        v.push(Field::Empty).unwrap();
        v.wipe();
        assert!(v.is_empty());
        let mut q: BoundedQueue<Field, 2> = BoundedQueue::new();
        q.push_back(Field::Text(synthetic())).unwrap();
        q.wipe();
        assert!(q.is_empty());
    }

    #[test]
    fn arrays_and_plain_values() {
        let mut digits = [1u8, 2, 3];
        digits.wipe();
        assert_eq!(digits, [0; 3]);
        let mut flags = [true, false];
        flags.wipe();
        assert_eq!(flags, [false; 2]);
        let mut letter = 'x';
        letter.wipe();
        assert_eq!(letter, '\0');
        let mut nested: [Option<Probe>; 2] = [Some(Probe(5)), None];
        nested.wipe();
        assert_eq!(nested, [None, None]);
        assert_eq!(take_wiped(), [5]);
    }
}
