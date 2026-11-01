// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the geometry maths and the dirty-rectangle tracker.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{DirtyRegion, Insets, Point, Rect};
use proptest::prelude::*;

fn any_rect() -> impl Strategy<Value = Rect> {
    (any::<i32>(), any::<i32>(), any::<u32>(), any::<u32>())
        .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn small_rect() -> impl Strategy<Value = Rect> {
    (-8i32..72, -8i32..72, 0u32..40, 0u32..40).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn small_point() -> impl Strategy<Value = Point> {
    (-10i32..80, -10i32..80).prop_map(|(x, y)| Point::new(x, y))
}

proptest! {
    #[test]
    fn intersection_lies_in_both(a in any_rect(), b in any_rect()) {
        if let Some(i) = a.intersection(&b) {
            prop_assert!(!i.is_empty());
            prop_assert!(a.contains_rect(&i));
            prop_assert!(b.contains_rect(&i));
        }
        prop_assert_eq!(a.intersection(&b), b.intersection(&a));
    }

    #[test]
    fn point_membership_matches_intersection(r in small_rect(), p in small_point()) {
        let in_both = r.contains(p);
        let unit = Rect::new(p.x, p.y, 1, 1);
        prop_assert_eq!(in_both, r.intersection(&unit).is_some());
    }

    #[test]
    fn union_contains_both_non_empty(a in small_rect(), b in small_rect()) {
        let u = a.union(&b);
        if !a.is_empty() {
            prop_assert!(u.contains_rect(&a));
        }
        if !b.is_empty() {
            prop_assert!(u.contains_rect(&b));
        }
    }

    #[test]
    fn inset_result_stays_inside(r in any_rect(), l in any::<u32>(), t in any::<u32>(), ri in any::<u32>(), bo in any::<u32>()) {
        let insets = Insets { left: l, top: t, right: ri, bottom: bo };
        let inner = r.inset(insets);
        // Saturation at the far edges can only shrink the result further.
        prop_assert!(inner.size.width <= r.size.width);
        prop_assert!(inner.size.height <= r.size.height);
        prop_assert!(inner.left() >= r.left());
        prop_assert!(inner.top() >= r.top());
        // Insets as large as the rectangle leave nothing, wherever it is.
        if u64::from(l) + u64::from(ri) >= u64::from(r.size.width)
            || u64::from(t) + u64::from(bo) >= u64::from(r.size.height)
        {
            prop_assert!(inner.is_empty(), "{:?} inset by {:?} gave {:?}", r, insets, inner);
        }
    }

    #[test]
    fn from_edges_never_reaches_outside_the_edges(
        l in prop_oneof![any::<i64>(), -(1i64 << 33)..(1i64 << 33)],
        t in prop_oneof![any::<i64>(), -(1i64 << 33)..(1i64 << 33)],
        r in prop_oneof![any::<i64>(), -(1i64 << 33)..(1i64 << 33)],
        b in prop_oneof![any::<i64>(), -(1i64 << 33)..(1i64 << 33)],
    ) {
        let rect = Rect::from_edges(l, t, r, b);
        if !rect.is_empty() {
            prop_assert!(rect.left() >= l && rect.right() <= r, "{:?} from x {}..{}", rect, l, r);
            prop_assert!(rect.top() >= t && rect.bottom() <= b, "{:?} from y {}..{}", rect, t, b);
        }
        // Edges the types can hold exactly are kept exactly.
        let exact = |lo: i64, hi: i64| {
            i32::try_from(lo).is_ok() && hi > lo && u32::try_from(hi - lo).is_ok()
        };
        if exact(l, r) && exact(t, b) {
            prop_assert_eq!((rect.left(), rect.top(), rect.right(), rect.bottom()), (l, t, r, b));
        }
    }

    #[test]
    fn dirty_region_covers_everything_added(rects in proptest::collection::vec(small_rect(), 0..12)) {
        let screen = Rect::new(0, 0, 64, 64);
        let mut dirty: DirtyRegion<3> = DirtyRegion::new(screen);
        for r in &rects {
            dirty.add(*r);
        }
        prop_assert!(dirty.rects().len() <= 3);
        for stored in dirty.rects() {
            prop_assert!(!stored.is_empty());
            prop_assert!(screen.contains_rect(stored));
        }
        // Every on-screen pixel of every added rectangle is still covered.
        for r in &rects {
            if let Some(visible) = r.intersection(&screen) {
                for y in visible.origin.y..visible.origin.y + visible.size.height as i32 {
                    for x in visible.origin.x..visible.origin.x + visible.size.width as i32 {
                        let p = Point::new(x, y);
                        prop_assert!(dirty.rects().iter().any(|s| s.contains(p)), "pixel {:?} lost", p);
                    }
                }
            }
        }
    }
}
