// SPDX-License-Identifier: MIT OR Apache-2.0
//! Model-based property tests: random operation sequences are applied both
//! to a bounded container and to the equivalent `std` type (with the
//! capacity rule applied by hand); the two must always agree.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::VecDeque;

use nowayos_bounded::{BoundedQueue, BoundedStr, BoundedVec, Wipe};
use proptest::prelude::*;

const CAP: usize = 5;

#[derive(Debug, Clone)]
enum VecOp {
    Push(u8),
    Pop,
    Insert(usize, u8),
    Remove(usize),
    SwapRemove(usize),
    Truncate(usize),
    RetainEven,
    Clear,
}

fn vec_op() -> impl Strategy<Value = VecOp> {
    prop_oneof![
        4 => any::<u8>().prop_map(VecOp::Push),
        2 => Just(VecOp::Pop),
        2 => (0..CAP + 2, any::<u8>()).prop_map(|(i, v)| VecOp::Insert(i, v)),
        2 => (0..CAP + 2).prop_map(VecOp::Remove),
        1 => (0..CAP + 2).prop_map(VecOp::SwapRemove),
        1 => (0..CAP + 2).prop_map(VecOp::Truncate),
        1 => Just(VecOp::RetainEven),
        1 => Just(VecOp::Clear),
    ]
}

#[derive(Debug, Clone)]
enum QueueOp {
    Push(u8),
    ForcePush(u8),
    Pop,
    Clear,
}

fn queue_op() -> impl Strategy<Value = QueueOp> {
    prop_oneof![
        4 => any::<u8>().prop_map(QueueOp::Push),
        1 => any::<u8>().prop_map(QueueOp::ForcePush),
        3 => Just(QueueOp::Pop),
        1 => Just(QueueOp::Clear),
    ]
}

#[derive(Debug, Clone)]
enum StrOp {
    Push(char),
    PushStr(String),
    Pop,
    Clear,
    Wipe,
}

fn str_op() -> impl Strategy<Value = StrOp> {
    prop_oneof![
        4 => any::<char>().prop_map(StrOp::Push),
        2 => ".{0,6}".prop_map(StrOp::PushStr),
        2 => Just(StrOp::Pop),
        1 => Just(StrOp::Clear),
        1 => Just(StrOp::Wipe),
    ]
}

/// Applies `op` to `real` and to the `model`.
fn apply_str_op<const N: usize>(real: &mut BoundedStr<N>, model: &mut String, op: &StrOp) {
    match op {
        StrOp::Push(c) => {
            let fits = model.len() + c.len_utf8() <= N;
            assert_eq!(real.push(*c).is_ok(), fits);
            if fits {
                model.push(*c);
            }
        }
        StrOp::PushStr(s) => {
            let fits = model.len() + s.len() <= N;
            assert_eq!(real.push_str(s).is_ok(), fits);
            if fits {
                model.push_str(s);
            }
        }
        StrOp::Pop => assert_eq!(real.pop(), model.pop()),
        StrOp::Clear => {
            real.clear();
            model.clear();
        }
        StrOp::Wipe => {
            real.wipe();
            model.clear();
        }
    }
}

proptest! {
    #[test]
    fn bounded_vec_matches_vec_model(ops in proptest::collection::vec(vec_op(), 0..64)) {
        let mut real: BoundedVec<u8, CAP> = BoundedVec::new();
        let mut model: Vec<u8> = Vec::new();
        for op in ops {
            match op {
                VecOp::Push(v) => {
                    let result = real.push(v);
                    if model.len() < CAP {
                        prop_assert_eq!(result, Ok(()));
                        model.push(v);
                    } else {
                        prop_assert_eq!(result, Err(v));
                    }
                }
                VecOp::Pop => prop_assert_eq!(real.pop(), model.pop()),
                VecOp::Insert(i, v) => {
                    let result = real.insert(i, v);
                    if i <= model.len() && model.len() < CAP {
                        prop_assert_eq!(result, Ok(()));
                        model.insert(i, v);
                    } else {
                        prop_assert_eq!(result, Err(v));
                    }
                }
                VecOp::Remove(i) => {
                    let expected = (i < model.len()).then(|| model.remove(i));
                    prop_assert_eq!(real.remove(i), expected);
                }
                VecOp::SwapRemove(i) => {
                    let expected = (i < model.len()).then(|| model.swap_remove(i));
                    prop_assert_eq!(real.swap_remove(i), expected);
                }
                VecOp::Truncate(n) => {
                    real.truncate(n);
                    model.truncate(n);
                }
                VecOp::RetainEven => {
                    real.retain(|x| x % 2 == 0);
                    model.retain(|x| x % 2 == 0);
                }
                VecOp::Clear => {
                    real.clear();
                    model.clear();
                }
            }
            prop_assert_eq!(real.as_slice(), model.as_slice());
            prop_assert!(real.len() <= CAP);
        }
    }

    #[test]
    fn bounded_queue_matches_vecdeque_model(ops in proptest::collection::vec(queue_op(), 0..96)) {
        let mut real: BoundedQueue<u8, CAP> = BoundedQueue::new();
        let mut model: VecDeque<u8> = VecDeque::new();
        for op in ops {
            match op {
                QueueOp::Push(v) => {
                    let result = real.push_back(v);
                    if model.len() < CAP {
                        prop_assert_eq!(result, Ok(()));
                        model.push_back(v);
                    } else {
                        prop_assert_eq!(result, Err(v));
                    }
                }
                QueueOp::ForcePush(v) => {
                    let evicted = if model.len() == CAP { model.pop_front() } else { None };
                    model.push_back(v);
                    prop_assert_eq!(real.force_push_back(v), evicted);
                }
                QueueOp::Pop => prop_assert_eq!(real.pop_front(), model.pop_front()),
                QueueOp::Clear => {
                    real.clear();
                    model.clear();
                }
            }
            let contents: Vec<u8> = real.iter().copied().collect();
            let expected: Vec<u8> = model.iter().copied().collect();
            prop_assert_eq!(contents, expected);
            prop_assert_eq!(real.front(), model.front());
            prop_assert_eq!(real.back(), model.back());
        }
    }

    #[test]
    fn bounded_str_matches_string_model(ops in proptest::collection::vec(str_op(), 0..48)) {
        const BYTES: usize = 12;
        let mut real: BoundedStr<BYTES> = BoundedStr::new();
        let mut model = String::new();
        for op in ops {
            apply_str_op(&mut real, &mut model, &op);
            prop_assert_eq!(real.as_str(), model.as_str());
            // Storage beyond the length is zero after every operation...
            prop_assert!(real.unused_is_zeroed(), "after {:?}", op);
            // ...and all of it after a clear or a wipe.
            if matches!(op, StrOp::Clear | StrOp::Wipe) {
                prop_assert!(real.is_zeroed(), "after {:?}", op);
            }
        }
    }

    /// Whatever a string went through, wiping it leaves every byte of its
    /// storage zero, and the zeroed check sees exactly that.
    #[test]
    fn a_wipe_zeroes_all_storage_whatever_came_before(
        ops in proptest::collection::vec(str_op(), 0..48),
    ) {
        const BYTES: usize = 12;
        let mut real: BoundedStr<BYTES> = BoundedStr::new();
        let mut model = String::new();
        for op in &ops {
            apply_str_op(&mut real, &mut model, op);
        }
        // Before the wipe, the check agrees with what the string holds.
        prop_assert_eq!(real.is_zeroed(), model.is_empty());
        real.wipe();
        prop_assert!(real.is_zeroed());
        prop_assert!(real.is_empty());
        // Strings wiped inside an array are zeroed in place, every one.
        let mut answers: [BoundedStr<BYTES>; 3] = Default::default();
        for (index, answer) in answers.iter_mut().enumerate() {
            let mut model = String::new();
            for op in ops.iter().skip(index) {
                apply_str_op(answer, &mut model, op);
            }
        }
        answers.wipe();
        prop_assert!(answers.iter().all(BoundedStr::is_zeroed));
    }

    #[test]
    fn from_utf8_never_panics_and_agrees_with_core(bytes in proptest::collection::vec(any::<u8>(), 0..24)) {
        let result = BoundedStr::<16>::from_utf8(&bytes);
        match (bytes.len() <= 16, std::str::from_utf8(&bytes)) {
            (true, Ok(s)) => {
                let parsed = result.unwrap();
                prop_assert_eq!(parsed.as_str(), s);
            }
            _ => prop_assert!(result.is_err()),
        }
    }

    #[test]
    fn truncation_is_always_a_valid_prefix(s in ".{0,12}") {
        let t: BoundedStr<7> = BoundedStr::from_str_truncated(&s);
        prop_assert!(s.starts_with(t.as_str()));
        prop_assert!(t.len() <= 7);
        // Maximal: the next character (if any) would not have fitted.
        if let Some(next) = s[t.len()..].chars().next() {
            prop_assert!(t.len() + next.len_utf8() > 7);
        }
    }
}
