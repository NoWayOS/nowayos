// SPDX-License-Identifier: MIT OR Apache-2.0
//! A text's alignment, drawn: in a column that stretches its
//! children, a line set against the start, centred or against the end sits
//! there across its node - each line of a wrapped text on its own - and a
//! line too wide for its node is cut and starts at the start, whatever its
//! alignment. In each theme.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::{NodeId, NodeKind, Style, TextAlign, TextStyle, Tree};

/// The leftmost and rightmost columns, relative to `rect`'s left, in which
/// any of `rect`'s pixels differs from the background - where its text's
/// ink is - or `None` if it has none.
fn ink(fb: &Framebuffer, rect: Rect, ground: Rgb888) -> Option<(u32, u32)> {
    let (left, top) = (
        u32::try_from(rect.left()).unwrap(),
        u32::try_from(rect.top()).unwrap(),
    );
    let mut found: Option<(u32, u32)> = None;
    for x in 0..rect.size.width {
        for y in 0..rect.size.height {
            if fb.pixel(left + x, top + y).unwrap() != ground {
                found = Some(found.map_or((x, x), |(first, _)| (first, x)));
                break;
            }
        }
    }
    found
}

/// One text per alignment, and a centred text that wraps onto two lines,
/// in a column `width` wide, laid out with `theme`'s metrics.
fn screen(theme: &Theme<Rgb888>, width: u32) -> (Tree<16>, [NodeId; 4]) {
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new().padding(10).gap(6)).unwrap();
    tree.set_root(root).unwrap();
    let mut ids = [root; 4];
    for (at, (text, style)) in [
        ("Pay here", TextStyle::BODY),
        ("Pay here", TextStyle::BODY.align(TextAlign::Center)),
        ("Pay here", TextStyle::BODY.align(TextAlign::End)),
        (
            "Ask a member of staff at the desk if you need any help with this screen",
            TextStyle::BODY.lines(2).align(TextAlign::Center),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = tree
            .add(NodeKind::Text(text), Style::new().text(style))
            .unwrap();
        tree.append(root, id).unwrap();
        ids[at] = id;
    }
    tree.layout(Rect::new(0, 0, width, 240), &theme.metrics())
        .unwrap();
    (tree, ids)
}

fn frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    render(tree, &mut fb, theme).unwrap();
    fb
}

#[test]
fn each_line_sits_where_its_style_aligns_it_in_each_theme() {
    for (name, theme) in Theme::<Rgb888>::all() {
        let width = 360;
        let (tree, [start, centre, end, wrapped]) = screen(&theme, width);
        let fb = frame(&tree, &theme);
        let ground = theme.background;
        let content = |id| tree.get(id).unwrap().content();
        // The three one-line texts: the same width of ink, in three places.
        let (s0, s1) = ink(&fb, content(start), ground).unwrap();
        let (c0, c1) = ink(&fb, content(centre), ground).unwrap();
        let (e0, e1) = ink(&fb, content(end), ground).unwrap();
        let across = content(start).size.width;
        assert_eq!(s1 - s0, c1 - c0, "{name}");
        assert_eq!(s1 - s0, e1 - e0, "{name}");
        // Against the start: no more space before the ink than a glyph's
        // own bearing; against the end, likewise after it.
        assert!(s0 <= 3, "{name}: start at {s0}");
        assert!(
            across - 1 - e1 <= 3,
            "{name}: end leaves {}",
            across - 1 - e1
        );
        // Centred: as much space either side, to a pixel or two of the
        // glyphs' bearings.
        let (before, after) = (c0, across - 1 - c1);
        assert!(
            before.abs_diff(after) <= 3,
            "{name}: centred {before} | {after}"
        );
        assert!(
            before > 20 && after > 20,
            "{name}: not centred: {before} | {after}"
        );
        // The wrapped text: each of its two lines centred on its own.
        let block = content(wrapped);
        let line = theme.metrics().line_height();
        assert!(block.size.height >= 2 * line, "{name}: {block:?}");
        let top = block.top() + i64::from((block.size.height - 2 * line) / 2);
        let mut lines = Vec::new();
        for row in 0..2 {
            let rect = Rect::new(
                i32::try_from(block.left()).unwrap(),
                i32::try_from(top + i64::from(row * line)).unwrap(),
                block.size.width,
                line,
            );
            lines.push(ink(&fb, rect, ground).unwrap());
        }
        assert_ne!(lines[0], lines[1], "{name}: two lines of different widths");
        for (first, last) in lines {
            let (before, after) = (first, block.size.width - 1 - last);
            assert!(
                before.abs_diff(after) <= 3,
                "{name}: a wrapped line {before} | {after}"
            );
        }
    }
}

#[test]
fn a_line_too_wide_for_its_node_is_cut_and_starts_at_the_start() {
    let theme = Theme::<Rgb888>::light();
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new().padding(4)).unwrap();
    tree.set_root(root).unwrap();
    let mut ids = Vec::new();
    for align in [TextAlign::Start, TextAlign::Center, TextAlign::End] {
        let id = tree
            .add(
                NodeKind::Text("Every visitor signs in at the front desk before going any further"),
                Style::new().text(TextStyle::BODY.align(align)),
            )
            .unwrap();
        tree.append(root, id).unwrap();
        ids.push(id);
    }
    tree.layout(Rect::new(0, 0, 160, 120), &theme.metrics())
        .unwrap();
    let fb = frame(&tree, &theme);
    let inks: Vec<_> = ids
        .iter()
        .map(|&id| ink(&fb, tree.get(id).unwrap().content(), theme.background).unwrap())
        .collect();
    assert_eq!(inks[0], inks[1]);
    assert_eq!(inks[0], inks[2]);
    assert!(inks[0].0 <= 3);
}
