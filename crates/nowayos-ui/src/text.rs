// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measuring and fitting text into a width, and checking text a node shows.
//!
//! A line's width is the sum of its characters' advances and of its pairs'
//! kerning, in 1/64 pixel with checked arithmetic, rounded up to whole
//! pixels - from a glyph atlas, or from a built-in monospace font, where
//! every advance is the same and nothing is kerned ([`TextMetrics`]).
//! Fitting keeps as many whole characters as fit: a label its start
//! ([`fit_start`]), a text input the end being typed ([`fit_end`]). Cuts
//! always fall on character boundaries, and the kept part is measured as a
//! line of its own, so it never exceeds the width. A text set on several
//! lines breaks after spaces ([`wrap`]).
//!
//! Every text a node shows must be printable ([`check_printable`]): every
//! character one the glyph atlases draw and a person can see. Fixed text is
//! checked when its node is added, and text set later - a slot's, a text
//! input's value, a field's error - when it is set, keeping the previous
//! text on error.

use nowayos_display::Rect;

use crate::layout::pixels_in_units;
use crate::{TextMetrics, UiError};

/// Unicode's default-ignorable code points (the `Default_Ignorable_Code_Point`
/// property, `DerivedCoreProperties.txt`): characters a renderer shows as
/// nothing at all. Inclusive ranges.
const INVISIBLE: [(u32, u32); 17] = [
    (0x00AD, 0x00AD),   // soft hyphen
    (0x034F, 0x034F),   // combining grapheme joiner
    (0x061C, 0x061C),   // Arabic letter mark
    (0x115F, 0x1160),   // Hangul fillers
    (0x17B4, 0x17B5),   // Khmer inherent vowels
    (0x180B, 0x180F),   // Mongolian variation selectors and vowel separator
    (0x200B, 0x200F),   // zero-width space, joiners, direction marks
    (0x202A, 0x202E),   // direction embeddings and overrides
    (0x2060, 0x206F),   // word joiner, invisible operators, isolates
    (0x3164, 0x3164),   // Hangul filler
    (0xFE00, 0xFE0F),   // variation selectors
    (0xFEFF, 0xFEFF),   // zero-width no-break space (byte-order mark)
    (0xFFA0, 0xFFA0),   // halfwidth Hangul filler
    (0xFFF0, 0xFFF8),   // unassigned, reserved as ignorable
    (0x1BCA0, 0x1BCA3), // shorthand format controls
    (0x1D173, 0x1D17A), // musical symbol format controls
    (0xE0000, 0xE0FFF), // tags and variation selectors supplement
];

/// Whether `ch` would be invisible: one of Unicode's default-ignorable
/// code points, such as a zero-width space, a soft hyphen or a direction
/// override.
#[must_use]
pub fn is_invisible(ch: char) -> bool {
    let code = u32::from(ch);
    INVISIBLE
        .iter()
        .any(|&(first, last)| (first..=last).contains(&code))
}

/// Whether the UI's fonts can draw `ch`: the glyph atlases' set, printable
/// Latin-1 - the space to `~` and the no-break space to `ÿ`, less the soft
/// hyphen - which is also the repertoire of the built-in ISO 8859-1 fonts.
/// Every atlas holds exactly these characters, and a test keeps the two the
/// same. Provisional, until W4 decides the supported scripts.
#[must_use]
pub const fn is_printable(ch: char) -> bool {
    matches!(ch, ' '..='~' | '\u{A0}'..='\u{FF}') && ch != '\u{AD}'
}

/// Whether the display role's atlases draw `ch`: the digits, the
/// capitals, the space and `# % + , - . / : £` - what a queue number, an
/// amount, a time or a reference needs - and nothing else, since the
/// display role's large sizes would cost megabytes for the whole set. The
/// display face's ranges in `nowayos-glyphs`' `glyphs.toml`; a test keeps
/// the two the same.
#[must_use]
pub const fn is_display(ch: char) -> bool {
    matches!(ch, ' ' | '#' | '%' | '+'..=':' | 'A'..='Z' | '\u{A3}')
}

/// Checks text for a node in the display role: [`check_printable`]'s
/// checks, and every character one the display role's atlases draw
/// ([`is_display`]).
///
/// # Errors
///
/// As [`check_printable`]; [`UiError::NoGlyph`] for a printable character
/// outside the display set. Neither names the character.
pub fn check_display(text: &str) -> Result<(), UiError> {
    check_printable(text)?;
    if text.chars().all(is_display) {
        Ok(())
    } else {
        Err(UiError::NoGlyph)
    }
}

/// Checks text for a node: one line, nothing invisible, and every character
/// one the glyph atlases draw. [`Tree::add`](crate::Tree::add) checks every
/// fixed text a node shows this way, and every call that sets text checks it
/// first.
///
/// # Errors
///
/// For the first character that fails, in this order:
/// [`UiError::ControlCharacter`] for a control character (a line break, a
/// tab, NUL…), [`UiError::InvisibleCharacter`] for one that would not be
/// seen ([`is_invisible`]: a zero-width space, a direction override…),
/// [`UiError::NoGlyph`] for one outside the atlases' set
/// ([`is_printable`]). The error never says which character, so text a
/// person typed cannot leak through it.
///
/// ```
/// use nowayos_ui::{UiError, text::check_printable};
///
/// assert_eq!(check_printable("Ref. 4F-2207, café"), Ok(()));
/// assert_eq!(check_printable("two\nlines"), Err(UiError::ControlCharacter));
/// assert_eq!(check_printable("zero\u{200B}width"), Err(UiError::InvisibleCharacter));
/// assert_eq!(check_printable("abc\u{202E}def"), Err(UiError::InvisibleCharacter));
/// assert_eq!(check_printable("東京"), Err(UiError::NoGlyph));
/// ```
pub fn check_printable(text: &str) -> Result<(), UiError> {
    for ch in text.chars() {
        if ch.is_control() {
            return Err(UiError::ControlCharacter);
        }
        if is_invisible(ch) {
            return Err(UiError::InvisibleCharacter);
        }
        if !is_printable(ch) {
            return Err(UiError::NoGlyph);
        }
    }
    Ok(())
}

/// Number of characters (Unicode scalar values) in `text`, saturating at
/// `u32::MAX`.
#[must_use]
pub fn char_count(text: &str) -> u32 {
    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
}

/// Each start of `text`, measured: for each character, its byte index and
/// the width of the line up to and including it, in 1/64 pixel - `None`
/// once the sum is too large to count. A character with no glyph in the
/// atlas adds nothing and is kerned with neither neighbour.
pub(crate) fn starts<'a>(
    text: &'a str,
    metrics: &'a TextMetrics,
) -> impl Iterator<Item = (usize, Option<i64>)> + 'a {
    text.char_indices().scan(
        (Some(0_i64), None::<char>),
        move |(width, previous), (index, ch)| {
            let advance = metrics.advance(ch);
            let kerning = match (*previous, advance) {
                (Some(left), Some(_)) => metrics.kerning(left, ch),
                _ => 0,
            };
            *width = width
                .and_then(|width| width.checked_add(advance.unwrap_or(0)))
                .and_then(|width| width.checked_add(kerning));
            *previous = advance.map(|_| ch);
            Some((index, *width))
        },
    )
}

/// Each end of `text`, measured, from the last character back: for each
/// character, its byte index and the width of the line from it to the end,
/// in 1/64 pixel - `None` once the sum is too large to count. The same sum
/// as [`starts`] gives the whole line.
fn ends<'a>(
    text: &'a str,
    metrics: &'a TextMetrics,
) -> impl Iterator<Item = (usize, Option<i64>)> + 'a {
    text.char_indices().rev().scan(
        (Some(0_i64), None::<char>),
        move |(width, next), (index, ch)| {
            let advance = metrics.advance(ch);
            let kerning = match (advance, *next) {
                (Some(_), Some(right)) => metrics.kerning(ch, right),
                _ => 0,
            };
            *width = width
                .and_then(|width| width.checked_add(advance.unwrap_or(0)))
                .and_then(|width| width.checked_add(kerning));
            *next = advance.map(|_| ch);
            Some((index, *width))
        },
    )
}

/// Whether a line `width` 1/64 pixel wide fits in `limit` (from
/// `pixels_in_units`): rounded up to whole pixels, it is no wider.
fn fits(width: Option<i64>, limit: i64) -> bool {
    width.is_some_and(|width| width <= limit)
}

/// The longest start of `text` that fits in `width` pixels, cut on a
/// character boundary — for labels, where the beginning matters most.
/// Measured as a line of its own, it is never wider than `width`
/// ([`TextMetrics::text_width`]).
///
/// ```
/// use nowayos_glyphs::REGULAR_24;
/// use nowayos_ui::{TextMetrics, text::fit_start};
///
/// let m = TextMetrics::new(6, 10);
/// assert_eq!(fit_start("Continue", 30, &m), "Conti");
/// // C, o, n and t advance 991, 842, 845 and 501 sixty-fourths of a pixel,
/// // and the font kerns C-o by +3 and n-t by -26: 3,156/64 = 49.3 pixels,
/// // so "Cont" takes 50, and "Conti" would take 56.
/// let atlas = TextMetrics::from_atlas(&REGULAR_24);
/// assert_eq!(fit_start("Continue", 50, &atlas), "Cont");
/// assert_eq!(fit_start("Continue", 55, &atlas), "Cont");
/// ```
#[must_use]
pub fn fit_start<'a>(text: &'a str, width: u32, metrics: &TextMetrics) -> &'a str {
    let limit = pixels_in_units(width);
    match starts(text, metrics).find(|&(_, line)| !fits(line, limit)) {
        Some((cut, _)) => text.get(..cut).unwrap_or(""),
        None => text,
    }
}

/// The longest end of `text` that fits in `width` pixels, cut on a
/// character boundary — for text inputs, where the most recently typed
/// characters matter most. Measured as a line of its own, it is never
/// wider than `width` ([`TextMetrics::text_width`]).
///
/// ```
/// use nowayos_ui::{TextMetrics, text::fit_end};
///
/// let m = TextMetrics::new(6, 10);
/// assert_eq!(fit_end("someone@example.org", 42, &m), "ple.org");
/// ```
#[must_use]
pub fn fit_end<'a>(text: &'a str, width: u32, metrics: &TextMetrics) -> &'a str {
    let limit = pixels_in_units(width);
    let mut kept = text.len();
    for (index, line) in ends(text, metrics) {
        if !fits(line, limit) {
            break;
        }
        kept = index;
    }
    text.get(kept..).unwrap_or("")
}

/// The lines `text` is set in at `width` pixels: breaks fall after
/// a space, as many words on each line as fit - first fit - and the spaces
/// at a break belong to neither line. A word wider than the line is cut on
/// a character boundary, as many characters as fit ([`fit_start`]) and at
/// least one, so every line moves on; the next line starts where it was
/// cut. No hyphens, and a no-break space never breaks. Each line is
/// measured as a line of its own, so kerning never crosses a break and
/// every line is at most `width` wide - unless not even one character fits.
///
/// Layout and drawing both break text this way, so a wrapped text is drawn
/// on exactly the lines layout measured.
///
/// ```
/// use nowayos_ui::{TextMetrics, text::wrap};
///
/// let mono = TextMetrics::new(10, 20);
/// let lines: Vec<&str> = wrap("Sign in so reception knows", 120, &mono).collect();
/// assert_eq!(lines, ["Sign in so", "reception", "knows"]);
/// // A word wider than the line is cut where it no longer fits.
/// let lines: Vec<&str> = wrap("Unbreakable", 50, &mono).collect();
/// assert_eq!(lines, ["Unbre", "akabl", "e"]);
/// ```
#[must_use]
pub fn wrap<'t, 'm>(text: &'t str, width: u32, metrics: &'m TextMetrics) -> Wrap<'t, 'm> {
    Wrap {
        rest: text,
        width,
        metrics,
    }
}

/// The lines of a text, one at a time ([`wrap`]).
#[derive(Debug, Clone)]
pub struct Wrap<'t, 'm> {
    rest: &'t str,
    width: u32,
    metrics: &'m TextMetrics,
}

impl<'t> Iterator for Wrap<'t, '_> {
    type Item = &'t str;

    fn next(&mut self) -> Option<&'t str> {
        let rest = self.rest.trim_start_matches(' ');
        if rest.is_empty() {
            self.rest = rest;
            return None;
        }
        // The longest run of whole words that fits: each word's end is a
        // place to break, its width that of the line up to it.
        let limit = pixels_in_units(self.width);
        let mut end = 0;
        let mut chars = starts(rest, self.metrics).zip(rest.chars()).peekable();
        while let Some(((at, line), ch)) = chars.next() {
            let word_ends = ch != ' ' && chars.peek().is_none_or(|&(_, next)| next == ' ');
            if word_ends {
                if !fits(line, limit) {
                    break;
                }
                end = at.saturating_add(ch.len_utf8());
            }
        }
        if end == 0 {
            // The first word alone is too wide: as much of it as fits, at
            // least one character.
            let word = rest.split(' ').next().unwrap_or(rest);
            end = match fit_start(word, self.width, self.metrics).len() {
                0 => word.chars().next().map_or(word.len(), char::len_utf8),
                kept => kept,
            };
        }
        let (line, after) = rest.split_at_checked(end).unwrap_or((rest, ""));
        self.rest = after;
        Some(line)
    }
}

/// The part of a one-line text node at `rect`, its text drawn from the
/// start of `content`, that changes when its text goes from `old` to `new`:
/// from the start of the last character the two share - kerning,
/// and a glyph drawn past its advance, reach a neighbour - to the end of
/// the longer, with half a line's margin each side for such glyphs, the
/// node's whole height, within the node. Nothing when the text is the same.
/// Repainting this alone gives the pixels of repainting the node.
#[must_use]
pub fn changed_area(
    rect: Rect,
    content: Rect,
    old: &str,
    new: &str,
    metrics: &TextMetrics,
) -> Rect {
    if old == new {
        return Rect::EMPTY;
    }
    let mut shared_from = 0;
    for ((at, a), b) in old.char_indices().zip(new.chars()) {
        if a != b {
            break;
        }
        shared_from = at;
    }
    let from = metrics.text_width(new.get(..shared_from).unwrap_or(""));
    let to = metrics.text_width(old).max(metrics.text_width(new));
    let margin = i64::from(metrics.line_height() / 2);
    let left = content
        .left()
        .saturating_add(i64::from(from))
        .saturating_sub(margin);
    let right = content
        .left()
        .saturating_add(i64::from(to))
        .saturating_add(margin);
    Rect::from_edges(left, rect.top(), right, rect.bottom())
        .intersection(&rect)
        .unwrap_or(Rect::EMPTY)
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
    use crate::layout::whole_pixels;
    use nowayos_glyphs::{REGULAR_18, REGULAR_24};

    const M: TextMetrics = TextMetrics::new(6, 10);

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(fit_start("hello", 30, &M), "hello");
        assert_eq!(fit_start("hello", 29, &M), "hell");
        assert_eq!(fit_start("hello", 0, &M), "");
        // Multi-byte characters are never split.
        assert_eq!(fit_start("héllo", 12, &M), "hé");
        assert_eq!(fit_end("héllo", 24, &M), "éllo");
        assert_eq!(fit_end("héllo", 5, &M), "");
        assert_eq!(fit_end("hi", 600, &M), "hi");
        assert_eq!(fit_start("", 0, &M), "");
        assert_eq!(fit_end("", 0, &M), "");
    }

    #[test]
    fn zero_width_characters_always_fit() {
        let free = TextMetrics::new(0, 10);
        assert_eq!(fit_start("anything", 0, &free), "anything");
        assert_eq!(fit_end("anything", 0, &free), "anything");
    }

    /// A line's width is its advances and its pairs' kerning, in 1/64
    /// pixel, rounded up - worked by hand from the 24 px atlas's tables.
    #[test]
    fn atlas_widths_are_advances_plus_kerning_rounded_up() {
        let m = TextMetrics::from_atlas(&REGULAR_24);
        // A 962, V 911, the pair kerned by -61: 1,812/64 = 28.3, so 29.
        assert_eq!(REGULAR_24.kerning('A', 'V'), -61);
        assert_eq!(m.text_width("AV"), 29);
        // Apart, each is kerned with the space between them instead (-15
        // and -31): 962 - 15 + 430 - 31 + 911 = 2,257/64 = 35.3, so 36.
        assert_eq!(m.text_width("A V"), 36);
        // One character is its advance: i 387/64 = 6.05, so 7.
        assert_eq!(m.text_width("i"), 7);
        assert_eq!(m.text_width(""), 0);
        // A line is rounded up once, as a whole, never character by
        // character: four i's are 1,548/64 = 24.2, so 25 - not 28.
        assert_eq!(m.text_width("iiii"), 25);
        // The 18 px atlas: T 643, o 631, kerned by -92: 1,182/64 = 18.5.
        let small = TextMetrics::from_atlas(&REGULAR_18);
        assert_eq!(small.text_width("To"), 19);
        // 1,134 + 364 + 0 = 1,498/64 = 23.4 pixels a line; 0 is 746/64.
        assert_eq!((small.line_height(), small.char_width()), (24, 12));
    }

    #[test]
    fn a_character_with_no_glyph_takes_no_width_and_breaks_kerning() {
        let m = TextMetrics::from_atlas(&REGULAR_24);
        // A, then a character outside the set, then V: nothing is kerned,
        // 962 + 911 = 1,873/64 = 29.3, so 30.
        assert_eq!(m.text_width("\u{2014}"), 0);
        assert_eq!(m.text_width("A\u{2014}V"), 30);
        // Fitting from either end agrees with the width.
        assert_eq!(fit_start("A\u{2014}V", 29, &m), "A\u{2014}");
        assert_eq!(fit_end("A\u{2014}V", 29, &m), "\u{2014}V");
        assert_eq!(fit_start("A\u{2014}V", 30, &m), "A\u{2014}V");
        assert_eq!(fit_end("A\u{2014}V", 30, &m), "A\u{2014}V");
        // A monospace font draws every character, an unknown one as its
        // replacement glyph, so each takes an advance.
        assert_eq!(M.text_width("A\u{2014}V"), 18);
    }

    #[test]
    fn widths_beyond_the_largest_saturate() {
        let huge = TextMetrics::new(u32::MAX, 10);
        assert_eq!(huge.text_width("ab"), u32::MAX);
        assert_eq!(fit_start("ab", u32::MAX, &huge), "a");
        assert_eq!(fit_end("ab", u32::MAX, &huge), "b");
        assert_eq!(whole_pixels(None), u32::MAX);
        assert_eq!(whole_pixels(Some(-5)), 0);
        assert_eq!(whole_pixels(Some(i64::MAX)), u32::MAX);
    }

    #[test]
    fn printable_latin1_only_and_nothing_invisible() {
        // Every printable Latin-1 character passes, the soft hyphen aside.
        for code in (0x20..=0x7E).chain(0xA0..=0xFF) {
            let ch = char::from_u32(code).unwrap();
            let expected = if code == 0xAD {
                Err(UiError::InvisibleCharacter)
            } else {
                Ok(())
            };
            assert_eq!(check_printable(ch.encode_utf8(&mut [0; 4])), expected);
        }
        for (text, error) in [
            ("tab\there", UiError::ControlCharacter),
            ("del\u{7F}", UiError::ControlCharacter),
            ("c1\u{85}", UiError::ControlCharacter),
            ("\u{200B}", UiError::InvisibleCharacter),
            ("rtl\u{202E}override", UiError::InvisibleCharacter),
            ("bom\u{FEFF}", UiError::InvisibleCharacter),
            ("tag\u{E0041}", UiError::InvisibleCharacter),
            ("em \u{2014} dash", UiError::NoGlyph),
            ("\u{1F600}", UiError::NoGlyph),
            ("Œuvre", UiError::NoGlyph),
            ("e\u{301}", UiError::NoGlyph),
        ] {
            assert_eq!(check_printable(text), Err(error), "{text:?}");
        }
        // The first failing character decides.
        assert_eq!(check_printable("\u{2014}\u{200B}\n"), Err(UiError::NoGlyph));
        assert_eq!(check_printable(""), Ok(()));
    }

    #[test]
    fn counting() {
        assert_eq!(char_count(""), 0);
        assert_eq!(char_count("aé€"), 3);
        assert_eq!(M.text_width("aé€"), 18);
    }
}
