// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`DirtyRegion`]: the bounded set of screen areas that need redrawing.

use nowayos_bounded::BoundedVec;

use crate::Rect;

/// Up to `N` rectangles of the screen that need redrawing.
///
/// A software renderer at kiosk resolutions cannot afford to repaint the
/// whole screen for every change; it records what changed here and repaints
/// only those areas. Storage is bounded, so the tracker trades precision for
/// space instead of growing:
///
/// * rectangles are clipped to the screen [`bounds`](Self::bounds), and
///   empty ones are ignored;
/// * a rectangle already covered by a stored one is ignored;
/// * overlapping rectangles are merged into their bounding box;
/// * when all `N` slots are used, the new rectangle is merged into the stored
///   one whose bounding box grows least.
///
/// Every pixel ever added stays covered until [`clear`](Self::clear); the
/// covered area may grow beyond what was added, never shrink below it.
///
/// ```
/// use nowayos_display::{DirtyRegion, Rect};
///
/// let mut dirty: DirtyRegion<4> = DirtyRegion::new(Rect::new(0, 0, 100, 100));
/// dirty.add(Rect::new(10, 10, 20, 20));
/// dirty.add(Rect::new(20, 20, 20, 20)); // overlaps: merged
/// assert_eq!(dirty.rects(), &[Rect::new(10, 10, 30, 30)]);
/// dirty.add(Rect::new(90, 90, 50, 50)); // clipped to the screen
/// assert_eq!(dirty.rects()[1], Rect::new(90, 90, 10, 10));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRegion<const N: usize> {
    bounds: Rect,
    rects: BoundedVec<Rect, N>,
}

impl<const N: usize> DirtyRegion<N> {
    /// An empty region for a screen covering `bounds`.
    ///
    /// `N` must be at least 1; `DirtyRegion<0>` fails to compile when used.
    #[must_use]
    pub fn new(bounds: Rect) -> Self {
        const { assert!(N > 0, "a DirtyRegion needs room for at least one rectangle") };
        Self {
            bounds,
            rects: BoundedVec::new(),
        }
    }

    /// The screen area rectangles are clipped to.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Changes the screen area (for example after a resize), discards what
    /// was recorded and marks the whole new area dirty.
    pub fn reset(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.mark_all();
    }

    /// Whether nothing needs redrawing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// The rectangles that need redrawing. They lie within the bounds and are
    /// non-empty; they may overlap only where the capacity forced a merge.
    #[must_use]
    pub fn rects(&self) -> &[Rect] {
        self.rects.as_slice()
    }

    /// The whole screen needs redrawing.
    pub fn mark_all(&mut self) {
        self.rects.clear();
        self.add(self.bounds);
    }

    /// Everything has been redrawn.
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// Records that `rect` needs redrawing.
    pub fn add(&mut self, rect: Rect) {
        let Some(mut pending) = rect.intersection(&self.bounds) else {
            return;
        };
        // Each pass either finishes or removes one stored rectangle, so this
        // runs at most N + 1 times.
        loop {
            if self
                .rects
                .iter()
                .any(|stored| stored.contains_rect(&pending))
            {
                return;
            }
            if let Some(index) = self
                .rects
                .iter()
                .position(|stored| stored.intersects(&pending))
            {
                if let Some(stored) = self.rects.swap_remove(index) {
                    pending = pending.union(&stored);
                }
                continue;
            }
            match self.rects.push(pending) {
                Ok(()) => return,
                Err(rejected) => {
                    let Some(index) = self.least_growth(&rejected) else {
                        return; // Unreachable: N > 0 and the list is full.
                    };
                    if let Some(stored) = self.rects.swap_remove(index) {
                        pending = rejected.union(&stored);
                    }
                }
            }
        }
    }

    /// Index of the stored rectangle whose bounding box with `rect` adds the
    /// least area.
    fn least_growth(&self, rect: &Rect) -> Option<usize> {
        self.rects
            .iter()
            .enumerate()
            .min_by_key(|(_, stored)| stored.union(rect).area().saturating_sub(stored.area()))
            .map(|(index, _)| index)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 100, 100);

    #[test]
    fn ignores_empty_offscreen_and_covered() {
        let mut d: DirtyRegion<4> = DirtyRegion::new(SCREEN);
        d.add(Rect::new(10, 10, 0, 5));
        d.add(Rect::new(200, 200, 10, 10));
        assert!(d.is_empty());
        d.add(Rect::new(0, 0, 50, 50));
        d.add(Rect::new(10, 10, 5, 5));
        assert_eq!(d.rects(), &[Rect::new(0, 0, 50, 50)]);
    }

    #[test]
    fn merges_chains_of_overlaps() {
        let mut d: DirtyRegion<4> = DirtyRegion::new(SCREEN);
        d.add(Rect::new(0, 0, 10, 10));
        d.add(Rect::new(20, 0, 10, 10));
        assert_eq!(d.rects().len(), 2);
        // Overlaps both: all three collapse into one.
        d.add(Rect::new(5, 0, 20, 5));
        assert_eq!(d.rects(), &[Rect::new(0, 0, 30, 10)]);
    }

    #[test]
    fn full_region_merges_with_least_growth() {
        let mut d: DirtyRegion<2> = DirtyRegion::new(SCREEN);
        d.add(Rect::new(0, 0, 10, 10));
        d.add(Rect::new(80, 80, 10, 10));
        d.add(Rect::new(85, 70, 5, 5)); // near the second
        assert_eq!(d.rects().len(), 2);
        assert!(d.rects().contains(&Rect::new(0, 0, 10, 10)));
        assert!(d.rects().contains(&Rect::new(80, 70, 10, 20)));
    }

    #[test]
    fn mark_all_and_reset() {
        let mut d: DirtyRegion<3> = DirtyRegion::new(SCREEN);
        d.add(Rect::new(1, 1, 1, 1));
        d.mark_all();
        assert_eq!(d.rects(), &[SCREEN]);
        d.reset(Rect::new(0, 0, 10, 10));
        assert_eq!(d.rects(), &[Rect::new(0, 0, 10, 10)]);
        d.clear();
        assert!(d.is_empty());
    }

    #[test]
    fn single_slot_becomes_bounding_box() {
        let mut d: DirtyRegion<1> = DirtyRegion::new(SCREEN);
        d.add(Rect::new(0, 0, 1, 1));
        d.add(Rect::new(98, 98, 1, 1));
        assert_eq!(d.rects(), &[Rect::new(0, 0, 99, 99)]);
    }
}
