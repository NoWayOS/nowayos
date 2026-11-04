// SPDX-License-Identifier: MIT OR Apache-2.0
//! Long text (builder contract v16): a 10 KB article the application sets
//! at run time, wrapped into paragraphs in a scroll view, as the golden
//! images show, at the top and scrolled; and a new text repainted from the
//! lines the tree marks gives exactly the pixels of a full repaint, for any
//! texts, line breaks and all.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Theme, render, render_dirty};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{SlotId, TextRole, TextStyle, Tree};
use proptest::prelude::*;

const ARTICLE: &str = include_str!("fixtures/article.txt");
const BODY: SlotId = SlotId::new(1);

/// Room for the article and more.
type Screen = Tree<16, 16_384>;

/// A heading over the article, in a scroll view filling a `width` x
/// `height` screen.
fn screen(theme: &Theme<Rgb888>, width: u32, height: u32) -> Box<Screen> {
    let mut tree: Box<Screen> = Box::default();
    let heading = UiBuilder::text_styled(
        &mut *tree,
        "The river path",
        TextStyle::new(TextRole::Heading),
    )
    .unwrap();
    let body = UiBuilder::long_text(&mut *tree, BODY, 12_288, TextStyle::default()).unwrap();
    let column = UiBuilder::column(&mut *tree, Layout::new().gap(8), &[heading, body]).unwrap();
    let view = UiBuilder::scroll_view(&mut *tree, Layout::new(), column).unwrap();
    let root = UiBuilder::column(&mut *tree, Layout::new().padding(16), &[view]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, width, height), &theme.metrics())
        .unwrap();
    tree
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn full_frame(tree: &Screen, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

/// Repaints only what `tree` marked, onto `fb`.
fn repaint(tree: &mut Screen, fb: &mut Framebuffer, theme: &Theme<Rgb888>) {
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(tree, fb, theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
}

#[test]
fn a_ten_kilobyte_article_wraps_into_its_paragraphs_and_scrolls() {
    assert!(ARTICLE.len() > 10_000);
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme, 480, 320);
    tree.set_slot(BODY, ARTICLE).unwrap();
    assert_eq!(tree.slot_text(BODY), Some(ARTICLE));
    // Taller than the view: laid out again to its new height.
    let body = tree.find_slot(BODY).unwrap();
    let lines = tree.get(body).unwrap().rect().size.height / theme.metrics().line_height();
    assert!(lines > 100, "{lines} lines");
    check_snapshot(&full_frame(&tree, &theme), &golden("long-text.png")).unwrap();
    tree.scroll_to(2_000).unwrap();
    check_snapshot(
        &full_frame(&tree, &theme),
        &golden("long-text-scrolled.png"),
    )
    .unwrap();
}

#[test]
fn one_word_changed_repaints_only_its_lines_and_matches_a_full_repaint() {
    let theme = Theme::<Rgb888>::light();
    let mut tree = screen(&theme, 480, 320);
    tree.set_slot(BODY, ARTICLE).unwrap();
    let mut fb = full_frame(&tree, &theme);
    tree.clear_dirty();
    let changed = ARTICLE.replacen("eleven kilometres", "twelve kilometres", 1);
    tree.set_slot(BODY, &changed).unwrap();
    // One line of the first paragraph, not the whole article.
    let marked: u32 = tree.dirty_rects().iter().map(|rect| rect.size.height).sum();
    assert_eq!(marked, theme.metrics().line_height());
    repaint(&mut tree, &mut fb, &theme);
    assert!(full_frame(&tree, &theme).diff(&fb).is_none());
}

/// A text of words from a small set, with line breaks and blank lines.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(
            &[
                "path",
                "river",
                "the",
                "heron",
                "a",
                "kingfisher",
                "footbridge",
                "\n",
                "\n\n",
                "mill",
                "estuary,",
                "walk.",
            ][..],
        ),
        0..80,
    )
    .prop_map(|words| words.join(" "))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn any_new_text_repainted_from_its_marks_equals_a_full_repaint(old in text(), new in text()) {
        let theme = Theme::<Rgb888>::light();
        let mut tree = screen(&theme, 240, 200);
        tree.set_slot(BODY, &old).unwrap();
        let mut fb = full_frame(&tree, &theme);
        tree.clear_dirty();
        tree.set_slot(BODY, &new).unwrap();
        repaint(&mut tree, &mut fb, &theme);
        prop_assert!(full_frame(&tree, &theme).diff(&fb).is_none());
    }
}
