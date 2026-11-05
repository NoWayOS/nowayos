// SPDX-License-Identifier: MIT OR Apache-2.0
//! Long text: paragraphs of any length the application sets at run time,
//! kept in the tree's text store - `T` bytes a [`Tree<N, T>`](crate::Tree)
//! holds, which each long text takes a part of, its capacity, when it is
//! built - so nothing allocates (builder contract v16).
//!
//! A long text wraps onto as many lines as it needs at its node's width; a
//! line break in it starts a new paragraph, an empty line between two. It
//! is set as a slot is ([`Tree::set_slot`](crate::Tree::set_slot)), and
//! only the lines that differ are repainted; when the number of lines
//! changes the tree is laid out again, as last time, and repainted whole.

use nowayos_display::Rect;

use crate::text::{check_printable, wrap};
use crate::{TextMetrics, UiError};

/// The largest long text, in bytes: 64 KiB less one, the most a `u16`
/// counts.
pub const MAX_LONG_TEXT: u32 = 65_535;

/// A tree's store of long texts: `T` bytes, given out in order as each long
/// text is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextStore<const T: usize> {
    bytes: [u8; T],
    /// How many bytes are given out.
    used: u32,
}

impl<const T: usize> TextStore<T> {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; T],
            used: 0,
        }
    }

    /// Gives out `capacity` bytes: their start. `None` when fewer are left.
    pub(crate) fn take(&mut self, capacity: u32) -> Option<u32> {
        let start = self.used;
        let end = start.checked_add(capacity)?;
        if usize::try_from(end).ok()? > T {
            return None;
        }
        self.used = end;
        Some(start)
    }

    /// The bytes from `start` given back: the last taken, when the node
    /// they were for could not be made.
    pub(crate) fn give_back(&mut self, start: u32) {
        if start <= self.used {
            self.used = start;
        }
    }

    /// Whether the `capacity` bytes from `start` are all zero.
    pub(crate) fn is_zeroed(&self, start: u32, capacity: u32) -> bool {
        let range = usize::try_from(start)
            .ok()
            .and_then(|start| Some(start..start.checked_add(usize::try_from(capacity).ok()?)?));
        range
            .and_then(|range| self.bytes.get(range))
            .is_some_and(|bytes| bytes.iter().all(|&byte| byte == 0))
    }

    /// Replaces bytes `from..to` of the `len` at `start` - of `capacity` -
    /// with `insert`, in place: the rest moves along, and bytes it leaves
    /// past the new end are zeroed. The new length, or `None` - nothing
    /// changed - if it would not fit.
    pub(crate) fn splice(
        &mut self,
        (start, capacity, len): (u32, u32, u32),
        (from, to): (usize, usize),
        insert: &[u8],
    ) -> Option<u32> {
        let start = usize::try_from(start).ok()?;
        let capacity = usize::try_from(capacity).ok()?;
        let len = usize::try_from(len).ok()?;
        if from > to || to > len {
            return None;
        }
        let new_len = len
            .checked_sub(to.checked_sub(from)?)?
            .checked_add(insert.len())?;
        if new_len > capacity {
            return None;
        }
        let area = self.bytes.get_mut(start..start.checked_add(capacity)?)?;
        let moved_to = from.checked_add(insert.len())?;
        area.copy_within(to..len, moved_to);
        area.get_mut(from..moved_to)?.copy_from_slice(insert);
        if new_len < len {
            area.get_mut(new_len..len)?.fill(0);
        }
        u32::try_from(new_len).ok()
    }

    /// Every byte given back, zeroed: for a tree cleared to build again.
    pub(crate) fn clear(&mut self) {
        self.bytes.fill(0);
        self.used = 0;
    }

    /// The text at `start`, `len` bytes: empty if that is not text.
    pub(crate) fn get(&self, start: u32, len: u32) -> &str {
        let range = usize::try_from(start)
            .ok()
            .and_then(|start| Some(start..start.checked_add(usize::try_from(len).ok()?)?));
        range
            .and_then(|range| self.bytes.get(range))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }

    /// Puts `text` at `start`, zeroing the rest of its `capacity`.
    pub(crate) fn put(&mut self, start: u32, capacity: u32, text: &str) -> Option<()> {
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(usize::try_from(capacity).ok()?)?;
        let place = self.bytes.get_mut(start..end)?;
        let (written, rest) = place.split_at_mut_checked(text.len())?;
        written.copy_from_slice(text.as_bytes());
        rest.fill(0);
        Some(())
    }
}

/// The lines of `text` at `width` pixels: each paragraph wrapped, an empty
/// one a blank line.
pub fn lines<'t, 'm>(text: &'t str, width: u32, metrics: &'m TextMetrics) -> Lines<'t, 'm> {
    Lines {
        paragraphs: text.split('\n'),
        current: None,
        width,
        metrics,
        empty: text.is_empty(),
    }
}

/// The lines of a long text, one at a time ([`lines`]).
#[derive(Debug, Clone)]
pub struct Lines<'t, 'm> {
    paragraphs: core::str::Split<'t, char>,
    current: Option<crate::text::Wrap<'t, 'm>>,
    width: u32,
    metrics: &'m TextMetrics,
    empty: bool,
}

impl<'t> Iterator for Lines<'t, '_> {
    type Item = &'t str;

    fn next(&mut self) -> Option<&'t str> {
        if self.empty {
            return None;
        }
        if let Some(line) = self.current.as_mut().and_then(Iterator::next) {
            return Some(line);
        }
        // The next paragraph's first line; an empty paragraph, or one of
        // spaces, a blank line.
        let paragraph = self.paragraphs.next()?;
        let mut wrapped = wrap(paragraph, self.width, self.metrics);
        let first = wrapped.next();
        self.current = Some(wrapped);
        // An empty line at the paragraph's own place in the text.
        Some(first.unwrap_or_else(|| paragraph.get(..0).unwrap_or("")))
    }
}

/// Checks each paragraph of `text` as any one line of text is checked
/// ([`check_printable`]): a line break between paragraphs is the only
/// control character a long text keeps.
///
/// # Errors
///
/// As [`check_printable`], for the first paragraph that fails.
pub fn check_paragraphs(text: &str) -> Result<(), UiError> {
    text.split('\n').try_for_each(check_printable)
}

/// The lines that differ when `old` becomes `new`, laid out from the top
/// of `content` a line high each: `None` when they take another number of
/// lines, and so the node another height.
pub(crate) fn changed_lines<'a>(
    old: &'a str,
    new: &'a str,
    content: Rect,
    metrics: &'a TextMetrics,
) -> Option<impl Iterator<Item = Rect> + 'a> {
    let width = content.size.width;
    if lines(old, width, metrics).count() != lines(new, width, metrics).count() {
        return None;
    }
    let height = i64::from(metrics.line_height());
    Some(
        lines(old, width, metrics)
            .zip(lines(new, width, metrics))
            .zip(0_i64..)
            .filter(|((was, now), _)| was != now)
            .map(move |(_, index)| {
                let top = content.top().saturating_add(index.saturating_mul(height));
                Rect::from_edges(
                    content.left(),
                    top,
                    content.right(),
                    top.saturating_add(height),
                )
            }),
    )
}
