// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rich text (#10078, builder contract v21): one paragraph of runs, each
//! with its own weight, italics, theme colour, code or link, wrapped as one
//! text - React's `<Text>Press <strong>Save</strong>, or read
//! <a onClick={…}>the guide</a>.</Text>`.
//!
//! Lines break after spaces, never inside a word that runs across runs
//! (`<strong>bold</strong>,`); a word wider than the line is cut where it
//! no longer fits. Italics are drawn in the italic of the run's weight and
//! code in the monospace face, on a tint (`nowayos_glyphs::styled`), each
//! measured in its own face. A run of a link - or consecutive runs of the same
//! link - is one link: each is a tab stop of the text, and Enter, Space or
//! a press on it reports its action.

use crate::{ActionId, Colour, TextMetrics, TextStyle, Weight};

/// One run of a rich text: its words and how they are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Its text: printable, one line.
    pub text: &'static str,
    /// Its weight; `None` for the text's own.
    pub weight: Option<Weight>,
    /// Whether it is set in italics.
    pub italic: bool,
    /// Whether it is code.
    pub code: bool,
    /// Its colour, one of the theme's; `None` for the text's own, or a
    /// link's.
    pub colour: Option<Colour>,
    /// The action it reports, if it is a link.
    pub link: Option<ActionId>,
}

impl Run {
    /// `text`, set as the text it is in is.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        Self {
            text,
            weight: None,
            italic: false,
            code: false,
            colour: None,
            link: None,
        }
    }

    /// In Bold.
    #[must_use]
    pub const fn bold(self) -> Self {
        Self {
            weight: Some(Weight::Bold),
            ..self
        }
    }

    /// In italics.
    #[must_use]
    pub const fn italic(self) -> Self {
        Self {
            italic: true,
            ..self
        }
    }

    /// As code.
    #[must_use]
    pub const fn code(self) -> Self {
        Self { code: true, ..self }
    }

    /// In the theme's `colour`.
    #[must_use]
    pub const fn colour(self, colour: Colour) -> Self {
        Self {
            colour: Some(colour),
            ..self
        }
    }

    /// A link reporting `action`.
    #[must_use]
    pub const fn link(self, action: ActionId) -> Self {
        Self {
            link: Some(action),
            ..self
        }
    }

    /// The style its text is measured and drawn in, within a text of
    /// `style`.
    #[must_use]
    pub const fn style_in(&self, style: TextStyle) -> TextStyle {
        match self.weight {
            Some(weight) => TextStyle { weight, ..style },
            None => style,
        }
    }

    /// The metrics its text is measured with, within a text of `style`
    /// measured by `metrics`: its weight's face, or that face's italic or
    /// monospace sibling.
    #[must_use]
    pub fn metrics_in(&self, style: TextStyle, metrics: &TextMetrics) -> TextMetrics {
        metrics
            .for_style(self.style_in(style))
            .styled(self.italic, self.code)
    }
}

/// How many links `runs` hold: consecutive runs of one action are one.
#[must_use]
pub fn links(runs: &[Run]) -> u8 {
    let mut count = 0_u8;
    let mut previous: Option<ActionId> = None;
    for run in runs {
        if run.link.is_some() && run.link != previous {
            count = count.saturating_add(1);
        }
        previous = run.link;
    }
    count
}

/// Which link run `index` is part of, counting from 0; `None` for a run
/// that is no link.
#[must_use]
pub fn link_of(runs: &[Run], index: usize) -> Option<u8> {
    let mut count: Option<u8> = None;
    let mut previous: Option<ActionId> = None;
    for (at, run) in runs.iter().enumerate() {
        if run.link.is_some() && run.link != previous {
            count = Some(count.map_or(0, |n| n.saturating_add(1)));
        }
        previous = run.link;
        if at == index {
            return run.link.and(count);
        }
    }
    None
}

/// The action of link `link`, counting from 0.
#[must_use]
pub fn link_action(runs: &[Run], link: u8) -> Option<ActionId> {
    (0..runs.len())
        .find(|&index| link_of(runs, index) == Some(link))
        .and_then(|index| runs.get(index))
        .and_then(|run| run.link)
}

/// A place in a rich text: a run, and a byte offset in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct At {
    /// The run.
    pub run: usize,
    /// The byte in its text.
    pub byte: usize,
}

/// One line's part of one run, placed: its text and where it starts, in
/// pixels from the line's start, and how wide it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// The run it is part of.
    pub run: usize,
    /// Its text.
    pub text: &'static str,
    /// Where it starts along the line.
    pub x: u32,
    /// Its width.
    pub width: u32,
}

/// Each line `runs` wrap onto at `width` pixels, as where it starts and
/// ends; `metrics` are the text's, measuring each run in its own style
/// within `style`.
pub fn lines<'r>(
    runs: &'r [Run],
    width: u32,
    style: TextStyle,
    metrics: &'r TextMetrics,
) -> impl Iterator<Item = (At, At)> + 'r {
    let end = At {
        run: runs.len(),
        byte: 0,
    };
    let mut start = Some(normal(runs, At { run: 0, byte: 0 }));
    core::iter::from_fn(move || {
        let from = start?;
        if from >= end {
            return None;
        }
        let to = line_end(runs, from, width, style, metrics);
        // A line always takes something, so the walk ends.
        let to = if to <= from { step(runs, from) } else { to };
        start = (to < end).then_some(to);
        Some((from, to))
    })
}

/// The place after the character at `at`, a run's end being the next
/// run's start.
fn step(runs: &[Run], at: At) -> At {
    let text = runs.get(at.run).map_or("", |run| run.text);
    let next = match text.get(at.byte..).and_then(|rest| rest.chars().next()) {
        Some(ch) => At {
            run: at.run,
            byte: at.byte.saturating_add(ch.len_utf8()),
        },
        None => At {
            run: at.run.saturating_add(1),
            byte: 0,
        },
    };
    normal(runs, next)
}

/// `at`, at the end of a run's text, as the next run's start - past any
/// empty runs.
fn normal(runs: &[Run], mut at: At) -> At {
    while let Some(run) = runs.get(at.run)
        && at.byte >= run.text.len()
    {
        at = At {
            run: at.run.saturating_add(1),
            byte: 0,
        };
    }
    at
}

/// Where the line starting at `from` ends: after the last space before it
/// is wider than `width`, or where a word with no space before it stops
/// fitting, or at the end. Widths are measured as [`pieces`] measures
/// them - each run's part whole, kerned - so a line is as wide as drawn.
fn line_end(runs: &[Run], from: At, width: u32, style: TextStyle, metrics: &TextMetrics) -> At {
    // The width of the runs before this one on the line.
    let mut before = 0_u32;
    let mut last_break: Option<At> = None;
    let mut at = from;
    let mut ink = false;
    for (index, run) in runs.iter().enumerate().skip(from.run) {
        let run_metrics = run.metrics_in(style, metrics);
        let begin = if index == from.run { from.byte } else { 0 };
        let rest = run.text.get(begin..).unwrap_or("");
        for (offset, ch) in rest.char_indices() {
            let here = At {
                run: index,
                byte: begin.saturating_add(offset),
            };
            let through = rest
                .get(..offset.saturating_add(ch.len_utf8()))
                .unwrap_or(rest);
            let used = before.saturating_add(run_metrics.text_width(through));
            if ch != ' ' && ink && used > width {
                return last_break.unwrap_or(here);
            }
            if ch == ' ' {
                last_break = Some(step(runs, here));
            } else {
                ink = true;
            }
            at = step(runs, here);
        }
        before = before.saturating_add(run_metrics.text_width(rest));
    }
    at
}

/// The pieces of the line from `from` to `to`, placed along it.
pub fn pieces<'r>(
    runs: &'r [Run],
    (from, to): (At, At),
    style: TextStyle,
    metrics: &'r TextMetrics,
) -> impl Iterator<Item = Piece> + 'r {
    let mut x = 0_u32;
    runs.iter()
        .enumerate()
        .skip(from.run)
        .take_while(move |(index, _)| *index < to.run || (*index == to.run && to.byte > 0))
        .filter_map(move |(index, run)| {
            let begin = if index == from.run { from.byte } else { 0 };
            let end = if index == to.run {
                to.byte
            } else {
                run.text.len()
            };
            let text = run.text.get(begin..end)?;
            if text.is_empty() {
                return None;
            }
            let width = run.metrics_in(style, metrics).text_width(text);
            let piece = Piece {
                run: index,
                text,
                x,
                width,
            };
            x = x.saturating_add(width);
            Some(piece)
        })
}

/// The width of the line from `from` to `to`, its trailing spaces left out.
#[must_use]
pub fn line_width(runs: &[Run], line: (At, At), style: TextStyle, metrics: &TextMetrics) -> u32 {
    pieces(runs, line, style, metrics)
        .map(|piece| {
            let trimmed = piece.text.trim_end_matches(' ');
            let metrics = runs
                .get(piece.run)
                .map(|run| run.metrics_in(style, metrics));
            if trimmed.len() == piece.text.len() {
                piece.x.saturating_add(piece.width)
            } else {
                piece
                    .x
                    .saturating_add(metrics.map_or(0, |m| m.text_width(trimmed)))
            }
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure fails the test"
)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    const RUNS: &[Run] = &[
        Run::new("Press "),
        Run::new("Save").bold(),
        Run::new(", or read "),
        Run::new("the guide").link(ActionId(7)),
        Run::new(" first."),
    ];

    fn metrics() -> TextMetrics {
        TextMetrics::new(8, 16)
    }

    fn texts(width: u32) -> Vec<std::string::String> {
        let m = metrics();
        lines(RUNS, width, TextStyle::default(), &m)
            .map(|line| {
                pieces(RUNS, line, TextStyle::default(), &m)
                    .map(|piece| piece.text)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn runs_wrap_as_one_text_after_spaces() {
        assert_eq!(texts(1000), ["Press Save, or read the guide first."]);
        // 12 characters a line.
        assert_eq!(texts(96), ["Press Save, ", "or read the ", "guide first."]);
        // A word across two runs never breaks between them.
        assert_eq!(texts(80)[0], "Press ");
        assert_eq!(texts(80)[1], "Save, or ");
    }

    #[test]
    fn italics_and_code_are_measured_in_their_own_faces() {
        use nowayos_glyphs::{BOLD, CODE, CODEBOLD, ITALIC, REGULAR};
        let style = TextStyle::default();
        let with = |face| TextMetrics::new(8, 16).with_face(style, face, 18 * 64);
        let at = |face| with(face).for_style(style);
        let m = with(&REGULAR).with_face(style.bold(), &BOLD, 18 * 64);
        let text = "illicit";
        let width = |run: Run| run.metrics_in(style, &m).text_width(run.text);
        assert_eq!(width(Run::new(text)), at(&REGULAR).text_width(text));
        assert_eq!(width(Run::new(text).italic()), at(&ITALIC).text_width(text));
        assert_eq!(width(Run::new(text).code()), at(&CODE).text_width(text));
        assert_eq!(
            width(Run::new(text).bold().code()),
            at(&CODEBOLD).text_width(text)
        );
        // Code sets each letter in a cell of one width: narrow letters widen.
        assert!(width(Run::new(text).code()) > width(Run::new(text)));
        // A line of runs is as wide as its runs, each in its face.
        let runs = [Run::new("a "), Run::new(text).code()];
        let line = lines(&runs, 1000, style, &m).next().unwrap();
        assert_eq!(
            line_width(&runs, line, style, &m),
            width(Run::new("a ")) + width(Run::new(text).code())
        );
    }

    #[test]
    fn links_count_and_report_their_actions() {
        assert_eq!(links(RUNS), 1);
        assert_eq!(link_of(RUNS, 3), Some(0));
        assert_eq!(link_of(RUNS, 2), None);
        assert_eq!(link_action(RUNS, 0), Some(ActionId(7)));
        let two = [
            Run::new("a").link(ActionId(1)),
            Run::new("b").link(ActionId(1)).bold(),
            Run::new(" "),
            Run::new("c").link(ActionId(2)),
        ];
        assert_eq!(links(&two), 2);
        assert_eq!(link_of(&two, 1), Some(0));
        assert_eq!(link_action(&two, 1), Some(ActionId(2)));
    }

    #[test]
    fn a_word_wider_than_the_line_is_cut() {
        let runs = [Run::new("abcdefghij")];
        let m = metrics();
        let cut: Vec<_> = lines(&runs, 32, TextStyle::default(), &m).collect();
        assert_eq!(cut.len(), 3);
        assert_eq!(line_width(&runs, cut[0], TextStyle::default(), &m), 32);
    }
}
