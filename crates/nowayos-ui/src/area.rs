// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text areas (#10075): several lines a person types, their bytes in the
//! tree's text store, wrapped at the area's width and shown a few rows at a
//! time, scrolled to keep the caret in view.

use nowayos_bounded::Wipe;
use nowayos_input::KeyCode;

use nowayos_input::keymap;

use crate::edit::{Step, boundary, selection, step};
use crate::{FieldId, NodeId, NodeKind, Style, TextMetrics, Tree, UiError, UiEvent};

/// The most bytes a text area holds: 64 KiB less one.
pub const MAX_AREA: u32 = 65_535;

/// The lines `text` wraps onto at `width` pixels, as byte ranges: each
/// paragraph wrapped ([`crate::long_text::lines`]), an empty one a blank
/// line - and one blank line for no text, where its caret goes.
pub fn line_spans<'t>(
    text: &'t str,
    width: u32,
    metrics: &'t TextMetrics,
) -> impl Iterator<Item = (usize, usize)> + 't {
    let base = text.as_ptr() as usize;
    let empty = text.is_empty().then_some((0, 0));
    crate::long_text::lines(text, width, metrics)
        .map(move |line| {
            let start = (line.as_ptr() as usize).saturating_sub(base);
            (start, start.saturating_add(line.len()))
        })
        .chain(empty)
}

/// The line holding `caret` among `text`'s lines at `width`, and its span:
/// the last whose start is at or before it.
fn line_of(text: &str, caret: usize, width: u32, metrics: &TextMetrics) -> (usize, (usize, usize)) {
    let mut found = (0, (0, 0));
    for (index, span) in line_spans(text, width, metrics).enumerate() {
        if span.0 > caret {
            break;
        }
        found = (index, span);
    }
    found
}

/// The offset in `span` of `text` nearest `x` pixels from the line's
/// start.
fn nearest(text: &str, (start, end): (usize, usize), x: u32, metrics: &TextMetrics) -> usize {
    let line = text.get(start..end).unwrap_or("");
    line.char_indices()
        .map(|(index, _)| index)
        .chain(core::iter::once(line.len()))
        .min_by_key(|&index| {
            metrics
                .text_width(line.get(..index).unwrap_or(""))
                .abs_diff(x)
        })
        .map_or(start, |index| start.saturating_add(index))
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// Creates an empty text area bound to `field`, `rows` lines high,
    /// taking `max_len` bytes of the tree's text store (#10075).
    ///
    /// # Errors
    ///
    /// [`UiError::LongTextOutOfRange`] for 0 bytes or more than
    /// [`MAX_AREA`]; [`UiError::TextStoreFull`]; [`UiError::TreeFull`]; as
    /// [`Tree::text`] for `placeholder`.
    pub fn text_area(
        &mut self,
        field: FieldId,
        rows: u8,
        max_len: u32,
        placeholder: &'static str,
    ) -> Result<NodeId, UiError> {
        if max_len == 0 || max_len > MAX_AREA {
            return Err(UiError::LongTextOutOfRange);
        }
        let start = self
            .texts
            .take(max_len)
            .ok_or(UiError::TextStoreFull { capacity: T })?;
        let kind = NodeKind::TextArea {
            field,
            placeholder,
            start,
            capacity: max_len,
            len: 0,
            caret: 0,
            anchor: 0,
            rows: rows.max(1),
            top: 0,
        };
        self.add(kind, Style::new().insets(crate::INPUT_PADDING))
            .inspect_err(|_| self.texts.give_back(start))
    }

    /// Sets text area `id`'s value: each paragraph checked as one line of
    /// text is, the caret after it.
    pub(crate) fn set_area_value(&mut self, id: NodeId, text: &str) -> Result<(), UiError> {
        crate::long_text::check_paragraphs(text)?;
        let node = self.node(id)?;
        let rect = node.rect;
        let NodeKind::TextArea {
            start, capacity, ..
        } = node.kind
        else {
            return Err(UiError::WrongKind(id));
        };
        let required = u32::try_from(text.len()).unwrap_or(u32::MAX);
        if required > capacity {
            return Err(UiError::ValueTooLong(nowayos_bounded::CapacityError {
                capacity: usize::try_from(capacity).unwrap_or(usize::MAX),
                required: text.len(),
            }));
        }
        self.texts
            .put(start, capacity, text)
            .ok_or(UiError::WrongKind(id))?;
        if let NodeKind::TextArea {
            len, caret, anchor, ..
        } = &mut self.node_mut(id)?.kind
        {
            (*len, *caret, *anchor) = (required, required, required);
        }
        self.keep_caret_in_view(id);
        self.dirty.add(rect);
        Ok(())
    }

    /// Types `key`, Shift held if `shift`, into text area `id`: as into a
    /// text input, and Enter starts a line, Up and Down move a line - with
    /// Shift, selecting - and Home and End go to the ends of the caret's
    /// line.
    pub(crate) fn type_area(&mut self, id: NodeId, key: KeyCode, shift: bool) -> Option<UiEvent> {
        let ctrl = self.ctrl_keys.contains(&true);
        let metrics = self.metrics?;
        let node = self.get(id)?;
        let (rect, width) = (node.rect, node.content().size.width);
        let metrics = metrics.for_style(node.style.text);
        let NodeKind::TextArea {
            field,
            start,
            capacity,
            len,
            caret,
            anchor,
            ..
        } = node.kind
        else {
            return None;
        };
        let text = self.texts.get(start, len);
        let (at, from) = (
            boundary(text, usize::try_from(caret).unwrap_or(0)),
            boundary(text, usize::try_from(anchor).unwrap_or(0)),
        );
        let (low, high) = selection(text, at, from);
        // Moving the caret.
        let (line, span) = line_of(text, at, width, &metrics);
        let x = metrics.text_width(text.get(span.0..at).unwrap_or(""));
        let moved = match key {
            KeyCode::LEFT if !shift && low != high => Some(low),
            KeyCode::RIGHT if !shift && low != high => Some(high),
            KeyCode::LEFT if ctrl => Some(step(text, at, Step::WordLeft)),
            KeyCode::LEFT => Some(step(text, at, Step::Left)),
            KeyCode::RIGHT if ctrl => Some(step(text, at, Step::WordRight)),
            KeyCode::RIGHT => Some(step(text, at, Step::Right)),
            KeyCode::HOME => Some(span.0),
            KeyCode::END => Some(span.1),
            KeyCode::UP | KeyCode::DOWN => {
                let target = if key == KeyCode::UP {
                    line.checked_sub(1)
                } else {
                    line.checked_add(1)
                };
                let span = target.and_then(|target| line_spans(text, width, &metrics).nth(target));
                Some(
                    span.map_or(if key == KeyCode::UP { 0 } else { text.len() }, |span| {
                        nearest(text, span, x, &metrics)
                    }),
                )
            }
            _ => None,
        };
        if let Some(moved) = moved {
            self.set_area_caret(id, moved, !shift);
            return None;
        }
        // Editing.
        let mut typed = [0_u8; 4];
        let (range, insert) = match key {
            KeyCode::BACKSPACE | KeyCode::DELETE if low != high => ((low, high), ""),
            KeyCode::BACKSPACE => ((step(text, at, Step::Left), at), ""),
            KeyCode::DELETE => ((at, step(text, at, Step::Right)), ""),
            KeyCode::ENTER => ((low, high), "\n"),
            _ => {
                let ch = keymap::us_char(key, shift)?;
                ((low, high), &*ch.encode_utf8(&mut typed))
            }
        };
        if range.0 == range.1 && insert.is_empty() {
            return None;
        }
        // Spliced in place: the store's bytes past the new end are zeroed.
        let event = match self
            .texts
            .splice((start, capacity, len), range, insert.as_bytes())
        {
            Some(new_len) => {
                let caret = range.0.saturating_add(insert.len());
                if let Some(NodeKind::TextArea { len, .. }) =
                    self.node_mut(id).ok().map(|node| &mut node.kind)
                {
                    *len = new_len;
                }
                self.set_area_caret(id, caret, true);
                Some(UiEvent::ValueChanged(field))
            }
            None => Some(UiEvent::InputFull(field)),
        };
        typed.wipe();
        self.dirty.add(rect);
        event
    }

    /// Puts text area `id`'s caret at `at`, its anchor too unless keeping
    /// a selection, scrolls to show it and marks the area.
    fn set_area_caret(&mut self, id: NodeId, at: usize, collapse: bool) {
        let Ok(node) = self.node_mut(id) else {
            return;
        };
        let rect = node.rect;
        if let NodeKind::TextArea { caret, anchor, .. } = &mut node.kind {
            *caret = u32::try_from(at).unwrap_or(0);
            if collapse {
                *anchor = *caret;
            }
        }
        self.keep_caret_in_view(id);
        self.dirty.add(rect);
    }

    /// Scrolls text area `id` the least that shows its caret's line.
    fn keep_caret_in_view(&mut self, id: NodeId) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let Some(node) = self.get(id) else {
            return;
        };
        let width = node.content().size.width;
        let metrics = metrics.for_style(node.style.text);
        let NodeKind::TextArea {
            start,
            len,
            caret,
            rows,
            top,
            ..
        } = node.kind
        else {
            return;
        };
        let text = self.texts.get(start, len);
        let (line, _) = line_of(text, usize::try_from(caret).unwrap_or(0), width, &metrics);
        let line = u32::try_from(line).unwrap_or(u32::MAX);
        let rows = u32::from(rows);
        let top = if line < top {
            line
        } else if line >= top.saturating_add(rows) {
            line.saturating_sub(rows).saturating_add(1)
        } else {
            top
        };
        if let Ok(node) = self.node_mut(id)
            && let NodeKind::TextArea { top: kept, .. } = &mut node.kind
        {
            *kept = top;
        }
    }

    /// Text area `id`'s text - for drawing it.
    #[must_use]
    pub fn area_text(&self, id: NodeId) -> Option<&str> {
        match self.get(id)?.kind {
            NodeKind::TextArea { start, len, .. } => Some(self.texts.get(start, len)),
            _ => None,
        }
    }
}
