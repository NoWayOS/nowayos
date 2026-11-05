// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layout with the glyph atlases' real metrics: a line's width is the sum of its advances and of
//! its pairs' kerning, in 1/64 pixel, rounded up to whole pixels, and its
//! height the face's line height; text too wide for its node is cut on a
//! character boundary - a label keeping its start, a text input its end -
//! and never exceeds the width. The built-in monospace fonts are the
//! special case: equal advances, no kerning.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_glyphs::{ATLASES, REGULAR_18, REGULAR_24, REGULAR_36, REGULAR_48};
use nowayos_ui::text::{fit_end, fit_start, is_printable};
use nowayos_ui::{ActionId, Align, FieldId, Length, NodeId, SlotId, Style, TextMetrics, Tree};
use proptest::prelude::*;

/// The 24 px atlas's metrics: the large-text theme's body size,
/// the size every width and height here was worked by hand from.
const BODY: TextMetrics = TextMetrics::from_atlas(&REGULAR_24);

fn rect(tree: &Tree<16>, id: NodeId) -> Rect {
    tree.get(id).unwrap().rect()
}

/// Each kind of node that shows text, laid out at its natural size with the
/// 24 px atlas: every width and height worked by hand from the atlas's
/// tables (advances and kerning in 1/64 pixel; a line 1,511 + 485 + 0 =
/// 1,996/64 = 31.2, so 32 pixels high; the digit zero 995/64, so a
/// character 16 wide).
#[test]
fn layout_measures_every_kind_of_text_with_the_atlas() {
    assert_eq!((BODY.line_height(), BODY.char_width()), (32, 16));
    let mut t: Tree<16> = Tree::new();
    let column = t.column(Style::new().align(Align::Start)).unwrap();
    t.set_root(column).unwrap();
    let add = |t: &mut Tree<16>, id: NodeId| {
        t.append(column, id).unwrap();
        id
    };
    let welcome = t.text("Welcome").unwrap();
    let welcome = add(&mut t, welcome);
    let to = t.text("To").unwrap();
    let to = add(&mut t, to);
    let av = t.text("AV").unwrap();
    let av = add(&mut t, av);
    let button = t.button("Continue", ActionId(1)).unwrap();
    let button = add(&mut t, button);
    let input = t.text_input(FieldId(1), "Your name").unwrap();
    let input = add(&mut t, input);
    let checkbox = t.checkbox(FieldId(2), "I agree").unwrap();
    let checkbox = add(&mut t, checkbox);
    let radio = t.radio_group(FieldId(3), &["Yes", "No"]).unwrap();
    let radio = add(&mut t, radio);
    let error = t.field_error(FieldId(1)).unwrap();
    let error = add(&mut t, error);
    let slot = t.slot(SlotId(1)).unwrap();
    let slot = add(&mut t, slot);
    t.layout(Rect::new(0, 0, 800, 480), &BODY).unwrap();

    // W 1315, e 826, l 402, c 745, o 842, m 1298, e 826, and the pairs
    // W-e -34, l-c -6, c-o -15: 6,199/64 = 96.9.
    assert_eq!(rect(&t, welcome), Rect::new(0, 0, 97, 32));
    // T 857 and o 842, kerned by -123: 1,576/64 = 24.6.
    assert_eq!(rect(&t, to), Rect::new(0, 32, 25, 32));
    // A 962 and V 911, kerned by -61: 1,812/64 = 28.3.
    assert_eq!(rect(&t, av), Rect::new(0, 64, 29, 32));
    // "Continue": 6,052/64 = 94.6, so 95 - measured in the body atlas, as
    // these metrics name no Bold one - its padding 24 each side and 8
    // above and below.
    assert_eq!(rect(&t, button), Rect::new(0, 96, 95 + 48, 32 + 16));
    // Sixteen characters of 16 pixels, wider than its placeholder ("Your
    // name": 7,172/64 = 112.1, so 113); padding 6 and 5.
    assert_eq!(rect(&t, input), Rect::new(0, 144, 256 + 12, 32 + 10));
    // The box one line high, one character's space, then "I agree"
    // (4,863/64 = 76.0, so 76); padding 6 and 3.
    assert_eq!(
        rect(&t, checkbox),
        Rect::new(0, 186, 32 + 16 + 76 + 12, 32 + 6)
    );
    // The wider option, "Yes" (2,340/64 = 36.6, so 37) beside "No" (30);
    // a row a line high for each.
    assert_eq!(
        rect(&t, radio),
        Rect::new(0, 224, 32 + 16 + 37 + 12, 2 * 32 + 6)
    );
    // An error line and a slot fill the width; each one line high.
    assert_eq!(rect(&t, error), Rect::new(0, 294, 800, 32));
    assert_eq!(rect(&t, slot), Rect::new(0, 326, 800, 32));
}

/// Every size of every theme's type scale, and the monospace special case
/// with the numbers the first cut has always had.
#[test]
fn each_atlas_gives_its_own_size_and_monospace_keeps_its_own() {
    for (atlas, welcome, line, ch) in [
        // 4,649/64 = 72.6; 1,498/64 = 23.4; 746/64 = 11.7.
        (&REGULAR_18, 73, 24, 12),
        (&REGULAR_24, 97, 32, 16),
        // 9,300/64 = 145.3; 2,995/64 = 46.8; 1,493/64 = 23.3.
        (&REGULAR_36, 146, 47, 24),
        (&REGULAR_48, 194, 63, 32),
    ] {
        let metrics = TextMetrics::from_atlas(atlas);
        assert_eq!(
            metrics.text_width("Welcome"),
            welcome,
            "{} px",
            atlas.size()
        );
        assert_eq!(metrics.line_height(), line, "{} px", atlas.size());
        assert_eq!(metrics.char_width(), ch, "{} px", atlas.size());
        assert_eq!(
            metrics.face().map(|(face, size64)| (face.id(), size64)),
            Some((atlas.face().id(), u32::from(atlas.size()) * 64))
        );
    }
    for (mono, welcome) in [
        (TextMetrics::new(6, 10), 42),
        (TextMetrics::new(10, 20), 70),
    ] {
        assert_eq!(mono.text_width("Welcome"), welcome);
        assert!(mono.face().is_none());
    }
    // Metrics name their face by identity and size, and a theme's minimum
    // target size is kept apart from it.
    assert_eq!(TextMetrics::from_atlas(&REGULAR_24), BODY);
    assert_ne!(TextMetrics::from_atlas(&REGULAR_36), BODY);
    assert_ne!(BODY.with_min_target(40), BODY);
    assert_eq!(BODY.with_min_target(40).min_target(), 40);
    assert!(std::format!("{BODY:?}").contains(", 24 px)"));
}

/// A label too wide for its node is cut on a character boundary, keeping
/// its start; what is kept fits the node's width.
#[test]
fn a_label_wider_than_its_node_keeps_its_start() {
    let mut t: Tree<16> = Tree::new();
    let column = t.column(Style::new().align(Align::Start)).unwrap();
    t.set_root(column).unwrap();
    let label = t
        .add(
            nowayos_ui::NodeKind::Text("Continue"),
            Style::new().width(Length::Fixed(55)),
        )
        .unwrap();
    t.append(column, label).unwrap();
    t.layout(Rect::new(0, 0, 800, 480), &BODY).unwrap();
    let width = rect(&t, label).size.width;
    assert_eq!(width, 55);
    // "Cont" is 3,156/64 = 49.3, so 50; "Conti" 3,552/64 = 55.5, so 56.
    assert_eq!(fit_start("Continue", width, &BODY), "Cont");
    assert_eq!(fit_start("Continue", 56, &BODY), "Conti");
    // An input keeps the end being typed: "inue" is 2,887/64 = 45.1.
    assert_eq!(fit_end("Continue", 46, &BODY), "inue");
    assert_eq!(fit_end("Continue", 45, &BODY), "nue");
}

/// The atlases with each of the first cut's monospace fonts: every metric
/// layout may be given.
fn any_metrics() -> impl Strategy<Value = TextMetrics> {
    prop_oneof![
        Just(TextMetrics::from_atlas(&REGULAR_18)),
        Just(TextMetrics::from_atlas(&REGULAR_24)),
        Just(TextMetrics::from_atlas(&REGULAR_36)),
        Just(TextMetrics::from_atlas(&REGULAR_48)),
        Just(TextMetrics::new(6, 10)),
        Just(TextMetrics::new(10, 20)),
    ]
}

/// Mostly text from the atlases' set; sometimes anything at all, including
/// characters with no glyph, which a tree refuses but measuring must not
/// panic on.
fn any_text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[ -~\u{A0}-\u{AC}\u{AE}-\u{FF}]{0,40}",
        1 => any::<String>(),
    ]
}

/// The width a line should have, worked out here independently of
/// `nowayos-ui`, from the atlas's own lookups: every advance and every
/// kerned pair, in 1/64 pixel, rounded up once. A character with no glyph
/// adds nothing and is kerned with neither neighbour. A monospace font's
/// characters each take its advance.
fn expected_width(text: &str, metrics: &TextMetrics) -> u32 {
    // The atlas of the face and size the metrics measure with.
    let atlas = metrics.face().and_then(|(face, size64)| {
        nowayos_glyphs::ATLASES.into_iter().find(|atlas| {
            core::ptr::eq(atlas.face(), face) && u32::from(atlas.size()) * 64 == size64
        })
    });
    let Some(atlas) = atlas else {
        return u32::try_from(text.chars().count() * metrics.char_width() as usize)
            .unwrap_or(u32::MAX);
    };
    let mut total: i64 = 0;
    let mut previous: Option<char> = None;
    for ch in text.chars() {
        match atlas.glyph(ch) {
            Ok(glyph) => {
                if let Some(left) = previous {
                    total += i64::from(atlas.kerning(left, ch));
                }
                total += i64::from(glyph.advance());
                previous = Some(ch);
            }
            Err(_) => previous = None,
        }
    }
    if total <= 0 {
        0
    } else {
        u32::try_from((total + 63) / 64).unwrap()
    }
}

proptest! {
    /// A line's width is the sum of its advances and its pairs' kerning,
    /// rounded up to whole pixels - with every atlas, and with each
    /// monospace font, where it is the characters times the advance.
    #[test]
    fn width_is_advances_plus_kerning_rounded_up(metrics in any_metrics(), text in any_text()) {
        prop_assert_eq!(metrics.text_width(&text), expected_width(&text, &metrics));
    }

    /// Text wider than its node: a label keeps its start and a text input
    /// its end, cut on a character boundary; what is kept never exceeds the
    /// width, and one more character would.
    #[test]
    fn fitting_keeps_the_right_end_and_never_exceeds_the_width(
        metrics in any_metrics(),
        text in any_text(),
        width in 0u32..400,
    ) {
        let start = fit_start(&text, width, &metrics);
        prop_assert!(text.starts_with(start));
        prop_assert!(text.is_char_boundary(start.len()));
        prop_assert!(metrics.text_width(start) <= width, "{:?} in {}", start, width);
        if let Some(next) = text[start.len()..].chars().next() {
            let longer = &text[..start.len() + next.len_utf8()];
            prop_assert!(metrics.text_width(longer) > width, "{:?} would fit {}", longer, width);
        }
        let end = fit_end(&text, width, &metrics);
        let cut = text.len() - end.len();
        prop_assert!(text.ends_with(end));
        prop_assert!(text.is_char_boundary(cut));
        prop_assert!(metrics.text_width(end) <= width, "{:?} in {}", end, width);
        if let Some(before) = text[..cut].chars().next_back() {
            let longer = &text[cut - before.len_utf8()..];
            prop_assert!(metrics.text_width(longer) > width, "{:?} would fit {}", longer, width);
        }
        // Text that fits is kept whole, from either end.
        if metrics.text_width(&text) <= width {
            prop_assert_eq!(start, text.as_str());
            prop_assert_eq!(end, text.as_str());
        }
    }

    /// Laid out with an atlas, each text node is as wide as its text and
    /// one line high, until the viewport's width caps it.
    #[test]
    fn layout_gives_text_its_measured_width_and_one_line(
        texts in proptest::collection::vec("[ -~\u{A0}-\u{AC}\u{AE}-\u{FF}]{0,40}", 1..8),
        which in 0usize..4,
        viewport_width in 0u32..1200,
    ) {
        let atlas = ATLASES[which];
        let metrics = TextMetrics::from_atlas(atlas);
        let mut t: Tree<16> = Tree::new();
        let column = t.column(Style::new().align(Align::Start)).unwrap();
        t.set_root(column).unwrap();
        let mut ids = Vec::new();
        for text in &texts {
            let text: &'static str = Box::leak(text.clone().into_boxed_str());
            prop_assert!(text.chars().all(is_printable));
            let id = t.text(text).unwrap();
            t.append(column, id).unwrap();
            ids.push((id, text));
        }
        t.layout(Rect::new(0, 0, viewport_width, 600), &metrics).unwrap();
        for (id, text) in ids {
            let size = rect(&t, id).size;
            prop_assert_eq!(size.width, expected_width(text, &metrics).min(viewport_width));
            prop_assert_eq!(size.height, atlas.line_metrics().line_height());
        }
    }
}
