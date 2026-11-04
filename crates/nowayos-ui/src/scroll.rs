// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scrolling: scrollbars, what a scroll moves of what is already drawn, and
//! a fling that slows to a stop on the runtime's clock.
//!
//! A scroll view ([`NodeKind::Scroll`]) shows a scrollbar along each axis
//! its child is larger than it: a thumb as long, along the view, as the
//! part showing is of the whole, at where that part is; it can be dragged
//! ([`Tree::scrollbars`]). When a view scrolls, what was drawn of its
//! child moves with it and only the strip that exposes is drawn again:
//! [`Tree::moves`] lists the moves for whoever owns the pixels to make,
//! before drawing what is marked. A touch lifted while it moves its view
//! goes on moving it, slower and slower, as long as the loop that shows the
//! tree calls [`Tree::advance`].

use nowayos_bounded::BoundedVec;
use nowayos_display::{Point, Rect};

use crate::{MAX_DEPTH, Node, NodeId, NodeKind, Tree};

/// A scrollbar's width, in layout units.
pub const SCROLLBAR_WIDTH: u32 = 6;
/// The least length of a scrollbar's thumb, in layout units.
pub const SCROLLBAR_LEAST: u32 = 24;
/// How far beyond a thumb's sides, in layout units, a press or touch still
/// takes it.
pub const SCROLLBAR_REACH: u32 = 9;
/// The most moves recorded between two repaints; one more marks its
/// view for repainting instead.
pub const MAX_MOVES: usize = 4;
/// How long a fling takes to lose most of its speed: its speed falls by
/// this many milliseconds' worth each millisecond, as a share.
const FLING_FALL_MS: i64 = 325;
/// The least speed, in layout units a second, at which a lifted touch
/// flings its view, and below which a fling stops.
const FLING_LEAST: u32 = 60;

/// What a scroll moved of what is already drawn: the pixels in `area`, by
/// `dx` right and `dy` down. Those it moves out of `area` are dropped and
/// those it leaves behind are marked for repainting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Move {
    /// The part of the screen whose pixels move: what a view shows.
    pub area: Rect,
    /// How far right, in pixels (negative: left).
    pub dx: i64,
    /// How far down, in pixels (negative: up).
    pub dy: i64,
}

/// A scrollbar: its track along one side of its view, and its thumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrollbar {
    /// Where the thumb runs.
    pub track: Rect,
    /// The thumb: as long as the part showing is of the whole.
    pub thumb: Rect,
}

/// A view's scrollbars: down its right side, and along its bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scrollbars {
    /// Down its right side, when its child is taller than it.
    pub vertical: Option<Scrollbar>,
    /// Along its bottom, when its child is wider than it.
    pub horizontal: Option<Scrollbar>,
}

/// A scroll view moving on after a touch lifted: its speed, in 1/256 pixel
/// a second, and what it has not yet moved, in 1/256 pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fling {
    pub(crate) view: NodeId,
    pub(crate) speed: (i64, i64),
    pub(crate) rest: (i64, i64),
}

/// A thumb held by the pointer or a finger: its view, which axis, where the
/// hold started along it and the view's offset then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThumbHold {
    pub(crate) view: NodeId,
    pub(crate) vertical: bool,
    pub(crate) from: i64,
    pub(crate) offset: u32,
}

/// `rect` moved `dx` right and `dy` down.
fn shifted(rect: Rect, dx: i64, dy: i64) -> Rect {
    Rect::from_edges(
        rect.left().saturating_add(dx),
        rect.top().saturating_add(dy),
        rect.right().saturating_add(dx),
        rect.bottom().saturating_add(dy),
    )
}

/// What a scroll leaves the presenter: its moves.
pub(crate) type Moves = BoundedVec<Move, MAX_MOVES>;

impl<const N: usize, const T: usize> Tree<N, T> {
    /// The moves of what is already drawn since the last
    /// [`clear_dirty`](Self::clear_dirty), in order: whoever owns the
    /// pixels makes them, each within its area, before drawing what
    /// [`dirty_rects`](Self::dirty_rects) marks.
    #[must_use]
    pub fn moves(&self) -> &[Move] {
        self.moves.as_slice()
    }

    /// Scroll view `id`'s scrollbars, as last laid out: none for a node
    /// that is not one, or along an axis its child fits.
    #[must_use]
    pub fn scrollbars(&self, id: NodeId) -> Scrollbars {
        let (Some(view), Some((x, y)), Some((most_x, most_y))) = (
            self.get(id),
            self.scroll_position(id),
            self.scroll_range(id),
        ) else {
            return Scrollbars::default();
        };
        let content = view.content();
        let width = self.scale.px(SCROLLBAR_WIDTH);
        let least = self.scale.px(SCROLLBAR_LEAST);
        let both = most_x > 0 && most_y > 0;
        // A track's length: the view's side, less the other bar's corner.
        let side = |length: u32| {
            if both {
                length.saturating_sub(width)
            } else {
                length
            }
        };
        let thumb = |track: u32, shown: u32, offset: u32, most: u32| {
            let whole = u64::from(shown).saturating_add(u64::from(most));
            let length = u64::from(track)
                .saturating_mul(u64::from(shown))
                .checked_div(whole)
                .unwrap_or(0);
            let length = u32::try_from(length).unwrap_or(track).max(least).min(track);
            let at = u64::from(track.saturating_sub(length))
                .saturating_mul(u64::from(offset))
                .checked_div(u64::from(most))
                .unwrap_or(0);
            (u32::try_from(at).unwrap_or(0), length)
        };
        let vertical = (most_y > 0).then(|| {
            let length = side(content.size.height);
            let left = content.right().saturating_sub(i64::from(width));
            let track = Rect::from_edges(
                left,
                content.top(),
                content.right(),
                content.top().saturating_add(i64::from(length)),
            );
            let (at, long) = thumb(length, content.size.height, y, most_y);
            let top = content.top().saturating_add(i64::from(at));
            let thumb = Rect::from_edges(
                left,
                top,
                content.right(),
                top.saturating_add(i64::from(long)),
            );
            Scrollbar { track, thumb }
        });
        let horizontal = (most_x > 0).then(|| {
            let length = side(content.size.width);
            let top = content.bottom().saturating_sub(i64::from(width));
            let track = Rect::from_edges(
                content.left(),
                top,
                content.left().saturating_add(i64::from(length)),
                content.bottom(),
            );
            let (at, long) = thumb(length, content.size.width, x, most_x);
            let left = content.left().saturating_add(i64::from(at));
            let thumb = Rect::from_edges(
                left,
                top,
                left.saturating_add(i64::from(long)),
                content.bottom(),
            );
            Scrollbar { track, thumb }
        });
        Scrollbars {
            vertical,
            horizontal,
        }
    }

    /// The thumb under `point`, if any - the innermost view's - within
    /// [`SCROLLBAR_REACH`] of it across its track: its view and whether it
    /// is the vertical one.
    pub(crate) fn thumb_at(&self, point: Point) -> Option<(NodeId, bool)> {
        let reach = i64::from(self.scale.px(SCROLLBAR_REACH));
        let mut found = None;
        for id in self.scroll_views() {
            let seen = self.get(id).map(|view| self.seen(view));
            let bars = self.scrollbars(id);
            for (bar, vertical) in [(bars.vertical, true), (bars.horizontal, false)] {
                let Some(bar) = bar else {
                    continue;
                };
                let grown = if vertical {
                    Rect::from_edges(
                        bar.thumb.left().saturating_sub(reach),
                        bar.thumb.top(),
                        bar.thumb.right(),
                        bar.thumb.bottom(),
                    )
                } else {
                    Rect::from_edges(
                        bar.thumb.left(),
                        bar.thumb.top().saturating_sub(reach),
                        bar.thumb.right(),
                        bar.thumb.bottom(),
                    )
                };
                let visible = seen
                    .flatten()
                    .and_then(|seen| grown.intersection(&seen))
                    .is_some_and(|shown| shown.contains(point));
                if visible {
                    found = Some((id, vertical));
                }
            }
        }
        found
    }

    /// What view `node` shows of the screen: its content, inside every
    /// view it is in and the viewport.
    pub(crate) fn seen(&self, node: &Node) -> Option<Rect> {
        let content = node.content().intersection(&self.viewport)?;
        match self.scroll_clip(node) {
            Some(outer) => content.intersection(&outer),
            None => Some(content),
        }
    }

    /// Records that view `id`'s child moved `dx` right and `dy` down: what
    /// is drawn of it moves with it, and the strips that exposes, its
    /// scrollbars and whatever already waited for repainting there are
    /// marked. If anything outside the view is drawn over what it shows, the
    /// move is larger than the view, or the moves are full, the view is
    /// marked whole instead.
    pub(crate) fn scrolled(&mut self, id: NodeId, dx: i64, dy: i64) {
        let Some(view) = self.get(id) else {
            return;
        };
        let Some(area) = self.seen(view) else {
            return;
        };
        // Its scrollbars - and those of every view around it, drawn over
        // what it shows - stay where they are: the strips they run in are
        // drawn again, and only what lies beside them moves. A view's
        // strips meet without overlapping, so the dirty region keeps them
        // apart.
        let width = i64::from(self.scale.px(SCROLLBAR_WIDTH));
        let mut right_edge = area.right();
        let mut bottom_edge = area.bottom();
        let mut views = [None; MAX_DEPTH.saturating_add(1)];
        for (slot, around) in views.iter_mut().zip(self.views_around(id)) {
            *slot = Some(around);
        }
        for around in views.into_iter().flatten() {
            let Some(content) = self.get(around).map(Node::content) else {
                continue;
            };
            let bars = self.scrollbars(around);
            let right = bars.vertical.map(|_| {
                Rect::from_edges(
                    content.right().saturating_sub(width),
                    content.top(),
                    content.right(),
                    content.bottom(),
                )
            });
            let right_side = right.map_or(content.right(), |strip| strip.left());
            let bottom = bars.horizontal.map(|_| {
                Rect::from_edges(
                    content.left(),
                    content.bottom().saturating_sub(width),
                    right_side,
                    content.bottom(),
                )
            });
            for (strip, vertical) in [(right, true), (bottom, false)] {
                let Some(strip) = strip.and_then(|strip| strip.intersection(&area)) else {
                    continue;
                };
                if vertical {
                    right_edge = right_edge.min(strip.left());
                } else {
                    bottom_edge = bottom_edge.min(strip.top());
                }
                self.dirty.add(strip);
            }
        }
        let moving = Rect::from_edges(area.left(), area.top(), right_edge, bottom_edge);
        let small = !moving.is_empty()
            && dx.unsigned_abs() < u64::from(moving.size.width)
            && dy.unsigned_abs() < u64::from(moving.size.height);
        if !small || self.moves.is_full() || self.drawn_over(id, area) {
            self.dirty.add(area);
            return;
        }
        // What already waited where the pixels move moved with them.
        let mut waiting = [None; 8];
        for (slot, rect) in waiting.iter_mut().zip(self.dirty.rects()) {
            *slot = rect
                .intersection(&moving)
                .and_then(|inside| shifted(inside, dx, dy).intersection(&moving));
        }
        if self
            .moves
            .push(Move {
                area: moving,
                dx,
                dy,
            })
            .is_err()
        {
            self.dirty.add(area);
            return;
        }
        for rect in waiting.into_iter().flatten() {
            self.dirty.add(rect);
        }
        // The strips the move exposes.
        let (left, top, right, bottom) =
            (moving.left(), moving.top(), moving.right(), moving.bottom());
        if dy > 0 {
            self.dirty
                .add(Rect::from_edges(left, top, right, top.saturating_add(dy)));
        } else if dy < 0 {
            self.dirty.add(Rect::from_edges(
                left,
                bottom.saturating_add(dy),
                right,
                bottom,
            ));
        }
        if dx > 0 {
            self.dirty
                .add(Rect::from_edges(left, top, left.saturating_add(dx), bottom));
        } else if dx < 0 {
            self.dirty.add(Rect::from_edges(
                right.saturating_add(dx),
                top,
                right,
                bottom,
            ));
        }
    }

    /// Whether anything outside view `id` - drawn after it - is drawn over
    /// `area`, which a move of the view's pixels would carry with it.
    fn drawn_over(&self, id: NodeId, area: Rect) -> bool {
        let mut after = false;
        for (other, node) in self.paint_order() {
            if other == id {
                after = true;
                continue;
            }
            if after && !self.is_inside(other, id) && node.rect.intersects(&area) {
                return true;
            }
        }
        false
    }

    /// Whether `id` lies inside `container`.
    pub(crate) fn is_inside(&self, id: NodeId, container: NodeId) -> bool {
        let mut next = self.get(id).and_then(|node| node.parent);
        for _ in 0..=MAX_DEPTH {
            match next {
                Some(parent) if parent == container => return true,
                Some(parent) => next = self.get(parent).and_then(|node| node.parent),
                None => return false,
            }
        }
        false
    }

    /// The scroll views `id` is inside, innermost first.
    pub(crate) fn views_around(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut next = Some(id);
        let mut steps = 0usize;
        core::iter::from_fn(move || {
            while let Some(at) = next {
                steps = steps.saturating_add(1);
                if steps > MAX_DEPTH.saturating_add(1) {
                    return None;
                }
                let node = self.get(at)?;
                next = node.parent;
                if matches!(node.kind, NodeKind::Scroll { .. }) {
                    return Some(at);
                }
            }
            None
        })
    }

    /// Scrolls the innermost view under the pointer that can still move
    /// `dx` right and `dy` down - a mouse's wheel - by as much of it as it
    /// can; failing that, the view holding focus, or the first. Whether one
    /// moved.
    pub(crate) fn scroll_under_pointer(&mut self, dx: i64, dy: i64) -> bool {
        let under = self.hit_test(self.pointer);
        let fallback = self
            .focus
            .and_then(|id| self.views_around(id).next())
            .or_else(|| self.scroll_node());
        let mut candidates: [Option<NodeId>; 9] = [None; 9];
        if let Some(hit) = under {
            for (slot, view) in candidates.iter_mut().zip(self.views_around(hit)) {
                *slot = Some(view);
            }
        }
        if under.is_none_or(|hit| self.views_around(hit).next().is_none())
            && let Some(first) = candidates.first_mut()
        {
            *first = fallback;
        }
        for view in candidates.into_iter().flatten() {
            if self.scroll_by(view, dx, dy) {
                return true;
            }
        }
        false
    }

    /// Scrolls view `id` by `dx` right and `dy` down, within its range:
    /// whether it moved.
    pub(crate) fn scroll_by(&mut self, id: NodeId, dx: i64, dy: i64) -> bool {
        let Some((x, y)) = self.scroll_position(id) else {
            return false;
        };
        let to = |offset: u32, by: i64| {
            u32::try_from(i64::from(offset).saturating_add(by).max(0)).unwrap_or(u32::MAX)
        };
        self.scroll_view_to(id, to(x, dx), to(y, dy))
            .unwrap_or(false)
    }

    /// Lets `elapsed` milliseconds pass on the runtime's clock: a touch
    /// moving its view is measured for speed, and a fling moves its view on
    /// and slows. Whether a fling is still moving - so the loop calls again
    /// at its next frame.
    pub fn advance(&mut self, elapsed: u32) -> bool {
        let ms = i64::from(elapsed.max(1));
        if let Some(mut drag) = self.drag
            && drag.scrolling
        {
            // The finger's speed over this step, in 1/256 pixel a second,
            // averaged with what it was.
            let (x, y) = (i64::from(self.pointer.x), i64::from(self.pointer.y));
            let step = |now: i64, then: i64| {
                now.saturating_sub(then)
                    .saturating_mul(256_000)
                    .checked_div(ms)
                    .unwrap_or(0)
            };
            let (sx, sy) = (step(x, drag.last.0), step(y, drag.last.1));
            drag.speed = (
                drag.speed.0.saturating_add(sx) / 2,
                drag.speed.1.saturating_add(sy) / 2,
            );
            drag.last = (x, y);
            self.drag = Some(drag);
        }
        let Some(mut fling) = self.fling else {
            return false;
        };
        let moved = |speed: i64, rest: i64| {
            let total = speed
                .saturating_mul(ms)
                .checked_div(1000)
                .unwrap_or(0)
                .saturating_add(rest);
            let whole = total.checked_div(256).unwrap_or(0);
            (whole, total.saturating_sub(whole.saturating_mul(256)))
        };
        let (dx, rest_x) = moved(fling.speed.0, fling.rest.0);
        let (dy, rest_y) = moved(fling.speed.1, fling.rest.1);
        fling.rest = (rest_x, rest_y);
        let went = (dx == 0 && dy == 0) || self.scroll_by(fling.view, dx, dy);
        let slow = |speed: i64| {
            speed
                .saturating_mul(FLING_FALL_MS.saturating_sub(ms).max(0))
                .checked_div(FLING_FALL_MS)
                .unwrap_or(0)
        };
        fling.speed = (slow(fling.speed.0), slow(fling.speed.1));
        let least = i64::from(self.scale.px(FLING_LEAST)).saturating_mul(256);
        let moving = fling
            .speed
            .0
            .unsigned_abs()
            .max(fling.speed.1.unsigned_abs())
            >= least.unsigned_abs();
        self.fling = (went && moving).then_some(fling);
        self.fling.is_some()
    }

    /// A touch that moved its view lifted: if it was moving fast enough,
    /// the view flings on, the way the finger went.
    pub(crate) fn fling_on(&mut self) {
        self.fling = None;
        let Some(drag) = self.drag else {
            return;
        };
        let (Some(view), true) = (drag.view, drag.scrolling) else {
            return;
        };
        let least = i64::from(self.scale.px(FLING_LEAST)).saturating_mul(256);
        let fast =
            drag.speed.0.unsigned_abs().max(drag.speed.1.unsigned_abs()) >= least.unsigned_abs();
        if fast {
            // The content follows the finger: the offset moves against it.
            self.fling = Some(Fling {
                view,
                speed: (drag.speed.0.saturating_neg(), drag.speed.1.saturating_neg()),
                rest: (0, 0),
            });
        }
    }
}
