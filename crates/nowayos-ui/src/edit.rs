// SPDX-License-Identifier: MIT OR Apache-2.0
//! Editing text: where a caret moves and what a selection's replacement
//! leaves, on UTF-8 boundaries, in bytes (#10075).
//!
//! A text input keeps a caret and an anchor, byte offsets into its value
//! on character boundaries; the selection is the text between them. These
//! functions move a caret - a character, a word or to an end - and build
//! the value a replacement gives into a buffer the caller wipes.

/// The room kept for the caret at the end of a field's text, in layout
/// units.
pub const CARET_ROOM: u32 = 2;

/// How far left a field's one line of text is drawn, in pixels, so that
/// its caret shows: nothing while the text up to the caret - `to_caret`
/// pixels - and the caret's room fit in the field's `width`; else as far as
/// puts the caret at its right edge. The same rule draws the field and
/// places a caret where it is pressed.
#[must_use]
pub const fn window(to_caret: u32, width: u32, caret_room: u32) -> u32 {
    to_caret.saturating_sub(width.saturating_sub(caret_room))
}

/// Where a caret at `at` goes, in `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A character left.
    Left,
    /// A character right.
    Right,
    /// To the start of the word it is in, or of the word before.
    WordLeft,
    /// To the end of the word it is in, or of the word after.
    WordRight,
    /// To the start.
    Home,
    /// To the end.
    End,
}

/// `at`, moved by `step` in `text`: always on a character boundary within
/// it.
#[must_use]
pub fn step(text: &str, at: usize, step: Step) -> usize {
    let at = boundary(text, at);
    match step {
        Step::Left => text
            .get(..at)
            .and_then(|before| before.char_indices().next_back())
            .map_or(0, |(index, _)| index),
        Step::Right => text
            .get(at..)
            .and_then(|after| after.chars().next())
            .map_or(at, |ch| at.saturating_add(ch.len_utf8())),
        Step::WordLeft => {
            let before = text.get(..at).unwrap_or("");
            // Back over spaces, then over the word.
            let trimmed = before.trim_end_matches(|ch: char| !is_word(ch));
            trimmed
                .char_indices()
                .rev()
                .find(|&(_, ch)| !is_word(ch))
                .map_or(0, |(index, ch)| index.saturating_add(ch.len_utf8()))
        }
        Step::WordRight => {
            let after = text.get(at..).unwrap_or("");
            let start = after
                .len()
                .saturating_sub(after.trim_start_matches(|ch: char| !is_word(ch)).len());
            let word = after.get(start..).unwrap_or("");
            let end = word.find(|ch: char| !is_word(ch)).unwrap_or(word.len());
            at.saturating_add(start).saturating_add(end)
        }
        Step::Home => 0,
        Step::End => text.len(),
    }
}

/// The word around `at` in `text`, as a range of bytes: for a double
/// click. Empty where there is none.
#[must_use]
pub fn word_at(text: &str, at: usize) -> (usize, usize) {
    let at = boundary(text, at);
    let before = text.get(..at).unwrap_or("");
    let start = before
        .char_indices()
        .rev()
        .find(|&(_, ch)| !is_word(ch))
        .map_or(0, |(index, ch)| index.saturating_add(ch.len_utf8()));
    let after = text.get(at..).unwrap_or("");
    let end = at.saturating_add(after.find(|ch: char| !is_word(ch)).unwrap_or(after.len()));
    (start, end)
}

/// Whether `ch` is part of a word: a letter or a digit.
fn is_word(ch: char) -> bool {
    ch.is_alphanumeric()
}

/// `at`, moved back to the character boundary at or before it, within
/// `text`.
#[must_use]
pub fn boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at = at.saturating_sub(1);
    }
    at
}

/// The selection between `caret` and `anchor` in `text`, as an ordered
/// range of bytes on character boundaries.
#[must_use]
pub fn selection(text: &str, caret: usize, anchor: usize) -> (usize, usize) {
    let (caret, anchor) = (boundary(text, caret), boundary(text, anchor));
    (caret.min(anchor), caret.max(anchor))
}

/// `text` with bytes `from..to` replaced by `insert`, written into `out`:
/// its length, or `None` if it would not fit in `out` or `most` bytes.
/// The caller wipes `out` when done with it.
#[must_use]
pub fn replaced(
    text: &str,
    (from, to): (usize, usize),
    insert: &str,
    most: usize,
    out: &mut [u8],
) -> Option<usize> {
    let (before, after) = (text.get(..from)?, text.get(to..)?);
    let len = before
        .len()
        .checked_add(insert.len())?
        .checked_add(after.len())?;
    if len > most || len > out.len() {
        return None;
    }
    let mut at = 0_usize;
    for part in [before, insert, after] {
        let end = at.checked_add(part.len())?;
        out.get_mut(at..end)?.copy_from_slice(part.as_bytes());
        at = end;
    }
    Some(len)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure fails the test"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn steps_move_by_characters_words_and_ends() {
        let text = "café au lait";
        assert_eq!(step(text, 5, Step::Left), 3);
        assert_eq!(step(text, 3, Step::Right), 5);
        assert_eq!(step(text, 0, Step::Left), 0);
        assert_eq!(step(text, text.len(), Step::Right), text.len());
        // From inside "au": back to its start, then to "café"'s.
        assert_eq!(step(text, 7, Step::WordLeft), 6);
        assert_eq!(step(text, 6, Step::WordLeft), 0);
        assert_eq!(step(text, 0, Step::WordRight), 5);
        assert_eq!(step(text, 5, Step::WordRight), 8);
        assert_eq!(step(text, 4, Step::Home), 0);
        assert_eq!(step(text, 4, Step::End), text.len());
        assert_eq!(word_at(text, 7), (6, 8));
    }

    #[test]
    fn a_replacement_writes_the_new_value_or_refuses_it_whole() {
        let mut out = [0_u8; 16];
        let len = replaced("hello world", (6, 11), "there", 64, &mut out).unwrap();
        assert_eq!(&out[..len], b"hello there");
        assert_eq!(replaced("hello", (5, 5), "!!", 6, &mut out), None);
    }

    proptest! {
        #[test]
        fn every_step_and_replacement_stays_on_character_boundaries(
            text in "[a-z é€😀 ]{0,20}",
            at in 0usize..80,
            anchor in 0usize..80,
            insert in "[a-zé€😀 ]{0,4}",
        ) {
            for kind in [Step::Left, Step::Right, Step::WordLeft, Step::WordRight, Step::Home, Step::End] {
                let moved = step(&text, at, kind);
                prop_assert!(moved <= text.len() && text.is_char_boundary(moved));
            }
            let (from, to) = selection(&text, at, anchor);
            prop_assert!(text.is_char_boundary(from) && text.is_char_boundary(to) && from <= to);
            let mut out = [0_u8; 128];
            if let Some(len) = replaced(&text, (from, to), &insert, 128, &mut out) {
                let made = core::str::from_utf8(&out[..len]).unwrap();
                prop_assert_eq!(made, format!("{}{}{}", &text[..from], insert, &text[to..]));
            }
        }
    }
}
