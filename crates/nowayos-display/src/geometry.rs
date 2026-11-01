// SPDX-License-Identifier: MIT OR Apache-2.0
//! Points, sizes, rectangles and insets with overflow-safe maths.

/// A position on screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    /// Horizontal position; grows to the right.
    pub x: i32,
    /// Vertical position; grows downwards.
    pub y: i32,
}

impl Point {
    /// `(0, 0)`, the top-left corner of the screen.
    pub const ORIGIN: Self = Self { x: 0, y: 0 };

    /// A point at `(x, y)`.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// This point moved by `(dx, dy)`, or `None` if either coordinate would
    /// overflow.
    #[must_use]
    pub const fn checked_offset(self, dx: i32, dy: i32) -> Option<Self> {
        match (self.x.checked_add(dx), self.y.checked_add(dy)) {
            (Some(x), Some(y)) => Some(Self { x, y }),
            _ => None,
        }
    }

    /// This point moved by `(dx, dy)`, clamped to the `i32` range.
    #[must_use]
    pub const fn saturating_offset(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
        }
    }
}

/// A width and height, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// Zero by zero.
    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
    };

    /// A size of `width` × `height`.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Whether either dimension is zero, so the size covers no pixels.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Number of pixels covered. Cannot overflow: `u32 × u32` fits in `u64`.
    #[must_use]
    pub const fn area(self) -> u64 {
        (self.width as u64).saturating_mul(self.height as u64)
    }

    /// Component-wise sum, clamped to `u32::MAX`.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self {
            width: self.width.saturating_add(other.width),
            height: self.height.saturating_add(other.height),
        }
    }

    /// Component-wise difference, clamped at zero.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self {
            width: self.width.saturating_sub(other.width),
            height: self.height.saturating_sub(other.height),
        }
    }

    /// Component-wise maximum.
    #[must_use]
    pub fn component_max(self, other: Self) -> Self {
        Self {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    /// Component-wise minimum.
    #[must_use]
    pub fn component_min(self, other: Self) -> Self {
        Self {
            width: self.width.min(other.width),
            height: self.height.min(other.height),
        }
    }
}

/// Space to leave inside each edge of a rectangle — padding or a border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Insets {
    /// Pixels inside the left edge.
    pub left: u32,
    /// Pixels inside the top edge.
    pub top: u32,
    /// Pixels inside the right edge.
    pub right: u32,
    /// Pixels inside the bottom edge.
    pub bottom: u32,
}

impl Insets {
    /// No inset on any side.
    pub const ZERO: Self = Self::uniform(0);

    /// The same inset on all four sides.
    #[must_use]
    pub const fn uniform(value: u32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }

    /// `horizontal` on the left and right, `vertical` on the top and bottom.
    #[must_use]
    pub const fn symmetric(horizontal: u32, vertical: u32) -> Self {
        Self {
            left: horizontal,
            top: vertical,
            right: horizontal,
            bottom: vertical,
        }
    }

    /// Left plus right, clamped to `u32::MAX`.
    #[must_use]
    pub const fn horizontal(self) -> u32 {
        self.left.saturating_add(self.right)
    }

    /// Top plus bottom, clamped to `u32::MAX`.
    #[must_use]
    pub const fn vertical(self) -> u32 {
        self.top.saturating_add(self.bottom)
    }

    /// The total size the insets take up.
    #[must_use]
    pub const fn size(self) -> Size {
        Size::new(self.horizontal(), self.vertical())
    }
}

/// An axis-aligned rectangle: its top-left corner and its size.
///
/// The right and bottom edges are exclusive — a 2×2 rectangle at the origin
/// contains `(0, 0)` and `(1, 1)` but not `(2, 2)`. Edge positions are
/// returned as `i64` because `x + width` can exceed `i32::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Top-left corner.
    pub origin: Point,
    /// Width and height.
    pub size: Size,
}

impl Rect {
    /// An empty rectangle at the origin.
    pub const EMPTY: Self = Self::new(0, 0, 0, 0);

    /// A rectangle at `(x, y)` of `width` × `height`.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    /// A rectangle of `size` with its corner at the origin.
    #[must_use]
    pub const fn from_size(size: Size) -> Self {
        Self {
            origin: Point::ORIGIN,
            size,
        }
    }

    /// The rectangle spanning the given edges (`right` and `bottom`
    /// exclusive), or as much of it as the types can hold. The corner is
    /// clamped into the `i32` range and the size, measured from the clamped
    /// corner, into `u32`, so the result never reaches outside the given
    /// edges. Inverted or equal edges give an empty rectangle, and so does a
    /// span that starts beyond `i32::MAX` (no [`Point`] lies in it).
    #[must_use]
    pub fn from_edges(left: i64, top: i64, right: i64, bottom: i64) -> Self {
        let x = clamp_i32(left);
        let y = clamp_i32(top);
        Self::new(x, y, span(left, x, right), span(top, y, bottom))
    }

    /// Whether the rectangle covers no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.size.is_empty()
    }

    /// Number of pixels covered.
    #[must_use]
    pub const fn area(&self) -> u64 {
        self.size.area()
    }

    /// Left edge (inclusive).
    #[must_use]
    pub const fn left(&self) -> i64 {
        self.origin.x as i64
    }

    /// Top edge (inclusive).
    #[must_use]
    pub const fn top(&self) -> i64 {
        self.origin.y as i64
    }

    /// Right edge (exclusive).
    #[must_use]
    pub const fn right(&self) -> i64 {
        (self.origin.x as i64).saturating_add(self.size.width as i64)
    }

    /// Bottom edge (exclusive).
    #[must_use]
    pub const fn bottom(&self) -> i64 {
        (self.origin.y as i64).saturating_add(self.size.height as i64)
    }

    /// Whether `point` lies inside the rectangle.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        let (x, y) = (i64::from(point.x), i64::from(point.y));
        x >= self.left() && x < self.right() && y >= self.top() && y < self.bottom()
    }

    /// Whether `other` lies entirely inside this rectangle. An empty `other`
    /// is inside if its corner is within this rectangle's edges.
    #[must_use]
    pub fn contains_rect(&self, other: &Self) -> bool {
        other.left() >= self.left()
            && other.top() >= self.top()
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The overlap of two rectangles, or `None` if they do not overlap.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let left = self.left().max(other.left());
        let top = self.top().max(other.top());
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > left && bottom > top).then(|| Self::from_edges(left, top, right, bottom))
    }

    /// Whether the rectangles share at least one pixel.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.intersection(other).is_some()
    }

    /// The smallest rectangle containing both. An empty rectangle
    /// contributes nothing. Saturates at the edges of the coordinate range.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        match (self.is_empty(), other.is_empty()) {
            (true, true) => Self::EMPTY,
            (true, false) => *other,
            (false, true) => *self,
            (false, false) => Self::from_edges(
                self.left().min(other.left()),
                self.top().min(other.top()),
                self.right().max(other.right()),
                self.bottom().max(other.bottom()),
            ),
        }
    }

    /// The rectangle moved by `(dx, dy)`, or `None` if its corner would leave
    /// the `i32` range.
    #[must_use]
    pub const fn checked_translate(&self, dx: i32, dy: i32) -> Option<Self> {
        match self.origin.checked_offset(dx, dy) {
            Some(origin) => Some(Self {
                origin,
                size: self.size,
            }),
            None => None,
        }
    }

    /// The rectangle shrunk by `insets`. If the insets are larger than the
    /// rectangle, the result is empty and lies within the original.
    #[must_use]
    pub fn inset(&self, insets: Insets) -> Self {
        let right = self.right();
        let bottom = self.bottom();
        let left = self
            .left()
            .saturating_add(i64::from(insets.left))
            .min(right);
        let top = self.top().saturating_add(i64::from(insets.top)).min(bottom);
        let new_right = right.saturating_sub(i64::from(insets.right)).max(left);
        let new_bottom = bottom.saturating_sub(i64::from(insets.bottom)).max(top);
        Self::from_edges(left, top, new_right, new_bottom)
    }

    /// The centre point, rounded towards the top-left.
    #[must_use]
    pub fn center(&self) -> Point {
        let x = self.left().saturating_add(i64::from(self.size.width / 2));
        let y = self.top().saturating_add(i64::from(self.size.height / 2));
        Point::new(clamp_i32(x), clamp_i32(y))
    }
}

/// Clamps an `i64` into the `i32` range.
fn clamp_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// Clamps an `i64` into the `u32` range.
fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(if value < 0 { 0 } else { u32::MAX })
}

/// The size along one axis of the span `start..end` once its start has
/// been clamped to `clamped`: measured from the clamped start, so the far
/// end is kept where the type allows, and zero if the span is empty or
/// starts past the clamped start (beyond `i32::MAX`, where nothing can be
/// addressed).
fn span(start: i64, clamped: i32, end: i64) -> u32 {
    let clamped = i64::from(clamped);
    if end <= start || clamped < start {
        0
    } else {
        clamp_u32(end.saturating_sub(clamped))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    #[test]
    fn contains_uses_exclusive_far_edges() {
        let r = Rect::new(0, 0, 2, 2);
        assert!(r.contains(Point::new(0, 0)));
        assert!(r.contains(Point::new(1, 1)));
        assert!(!r.contains(Point::new(2, 1)));
        assert!(!r.contains(Point::new(1, 2)));
        assert!(!r.contains(Point::new(-1, 0)));
        assert!(!Rect::new(5, 5, 0, 10).contains(Point::new(5, 5)));
    }

    #[test]
    fn edges_do_not_overflow_at_the_extremes() {
        let r = Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        assert_eq!(r.right(), i64::from(i32::MAX) + i64::from(u32::MAX));
        assert!(r.contains(Point::new(i32::MAX, i32::MAX)));
        assert_eq!(r.area(), u64::from(u32::MAX) * u64::from(u32::MAX));
    }

    #[test]
    fn intersection_and_disjoint() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(&b), Some(Rect::new(5, 5, 5, 5)));
        // Touching edges share no pixel.
        assert_eq!(a.intersection(&Rect::new(10, 0, 5, 5)), None);
        assert!(!a.intersects(&Rect::new(-5, -5, 5, 5)));
    }

    #[test]
    fn union_ignores_empty_and_saturates() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.union(&Rect::EMPTY), a);
        assert_eq!(Rect::new(50, 50, 0, 0).union(&a), a);
        assert_eq!(a.union(&Rect::new(20, 30, 5, 5)), Rect::new(0, 0, 25, 35));
        let far_left = Rect::new(i32::MIN, 0, 1, 1);
        let far_right = Rect::new(i32::MAX - 1, 0, 1, 1);
        let u = far_left.union(&far_right);
        assert_eq!(u.origin.x, i32::MIN);
        assert_eq!(u.size.width, u32::MAX); // true width is 2^32 - 1
    }

    #[test]
    fn inset_shrinks_and_never_inverts() {
        let r = Rect::new(10, 20, 100, 50);
        assert_eq!(r.inset(Insets::symmetric(8, 4)), Rect::new(18, 24, 84, 42));
        let over = r.inset(Insets::uniform(80));
        assert!(over.is_empty());
        assert!(r.contains_rect(&over));
        assert_eq!(r.inset(Insets::ZERO), r);
    }

    #[test]
    fn translate_is_checked() {
        let r = Rect::new(i32::MAX - 1, 0, 4, 4);
        assert_eq!(
            r.checked_translate(1, 0),
            Some(Rect::new(i32::MAX, 0, 4, 4))
        );
        assert_eq!(r.checked_translate(2, 0), None);
    }

    #[test]
    fn from_edges_clamps() {
        assert_eq!(Rect::from_edges(10, 10, 5, 5), Rect::new(10, 10, 0, 0));
        let huge = Rect::from_edges(i64::MIN, 0, i64::MAX, 1);
        assert_eq!(huge.origin.x, i32::MIN);
        assert_eq!(huge.size.width, u32::MAX);
        // Cut off at the far negative edge, the right edge is kept.
        let cut = Rect::from_edges(i64::from(i32::MIN) - 50, 0, 10, 1);
        assert_eq!(cut.origin.x, i32::MIN);
        assert_eq!(cut.right(), 10);
    }

    #[test]
    fn from_edges_beyond_the_i32_range_is_empty() {
        // Regression (review 2026-10-01): equal edges past i32::MAX gave a
        // non-empty rectangle measured from the clamped corner.
        let edge = i64::from(i32::MAX) + 5;
        assert!(Rect::from_edges(edge, 0, edge, 1).is_empty());
        assert!(Rect::from_edges(0, edge, 1, edge).is_empty());
        // A rectangle that starts past the largest i32 coordinate covers no
        // pixel a `Point` can name.
        assert!(Rect::from_edges(edge, 0, edge + 10, 1).is_empty());
        assert!(Rect::from_edges(0, edge, 1, edge + 10).is_empty());
    }

    #[test]
    fn oversized_insets_at_the_far_corner_give_an_empty_rect() {
        // Regression (review 2026-10-01): this inset was 100 x 100.
        let r = Rect::new(i32::MAX, i32::MAX, 100, 100);
        let inner = r.inset(Insets::uniform(200));
        assert!(inner.is_empty(), "{inner:?}");
        assert!(r.contains_rect(&inner));
    }

    #[test]
    fn center_and_insets_totals() {
        assert_eq!(Rect::new(0, 0, 11, 5).center(), Point::new(5, 2));
        let i = Insets {
            left: u32::MAX,
            top: 1,
            right: 1,
            bottom: 2,
        };
        assert_eq!(i.horizontal(), u32::MAX);
        assert_eq!(i.size(), Size::new(u32::MAX, 3));
    }
}
