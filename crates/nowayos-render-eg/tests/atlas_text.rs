// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text drawn from the glyph outlines: every pixel a line's glyphs
//! cover is its coverage blended over the colour the node is known to draw
//! beneath the text - worked out here independently of the renderer, from
//! the face's own tables and a rasteriser of the test's own, in both
//! themes and in `Rgb888` and `Rgb565` -
//! and any text, in any node, at any size, repaints exactly in any area
//! and never draws outside its node.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::HashMap;

use embedded_graphics::pixelcolor::{Rgb565, Rgb888, RgbColor};
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, Size};
use nowayos_display::{Insets, Point, Rect};
use nowayos_glyphs::Face;
use nowayos_input::InputEvent;
use nowayos_raster::Rasteriser;
use nowayos_render_eg::{Font, Theme, render, render_area};
use nowayos_testkit::Framebuffer;
use nowayos_ui::text::{fit_end, fit_start, is_printable};
use nowayos_ui::{
    ActionId, Align, BUTTON_PADDING, CHOICE_PADDING, FieldId, INPUT_PADDING, Length, NodeId,
    SlotId, Style, TextMetrics, TextStyle, Tree,
};
use proptest::prelude::*;

/// A frame of any colour type, read pixel by pixel.
trait Frame<C> {
    fn at(&self, x: i64, y: i64) -> C;
}

impl Frame<Rgb888> for Framebuffer {
    fn at(&self, x: i64, y: i64) -> Rgb888 {
        self.pixel(x as u32, y as u32).unwrap()
    }
}

/// An RGB565 frame in memory.
struct Narrow {
    size: Size,
    pixels: Vec<Rgb565>,
}

impl Frame<Rgb565> for Narrow {
    fn at(&self, x: i64, y: i64) -> Rgb565 {
        self.pixels[(y * i64::from(self.size.width) + x) as usize]
    }
}

impl OriginDimensions for Narrow {
    fn size(&self) -> Size {
        self.size
    }
}

impl DrawTarget for Narrow {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if p.x >= 0
                && p.y >= 0
                && (p.x as u32) < self.size.width
                && (p.y as u32) < self.size.height
            {
                let index = p.y as usize * self.size.width as usize + p.x as usize;
                self.pixels[index] = c;
            }
        }
        Ok(())
    }
}

/// A face at a size in 1/64 pixel per em.
type Sized = (&'static Face, u32);

/// The face and size a theme draws body text in, at scale 1.
fn body<C: RgbColor>(theme: &Theme<C>) -> Sized {
    match theme.type_scale.body {
        Font::Outline(face, size) => (face, u32::from(size) * 64),
        Font::Mono(_) => panic!("a product theme draws from outlines"),
    }
}

/// The metrics, face and size `line` is drawn with in `theme`: Bold body
/// text's for a button's label, body text's for the rest.
fn face<C: RgbColor, D>(
    theme: &Theme<C>,
    metrics: &TextMetrics,
    line: &Expected<D>,
) -> (TextMetrics, Sized) {
    if !line.bold {
        return (*metrics, body(theme));
    }
    match theme.type_scale.body_bold {
        Font::Outline(face, size) => (
            metrics.for_style(TextStyle::BODY.bold()),
            (face, u32::from(size) * 64),
        ),
        Font::Mono(_) => panic!("a product theme draws Bold from outlines"),
    }
}

/// What a line of `text` covers, worked out from the face's tables and a
/// rasteriser of its own: the pen starts at `left`; each glyph sits at the
/// sum of the advances before it and the pairs' kerning, in 1/64 pixel,
/// rounded to the nearest pixel (a half rounding right), then its ink box's
/// left; its top that many pixels above the baseline, which is the ascent,
/// rounded, below the line's `top`. Where glyphs share a pixel their
/// coverage adds up, capped at 255.
fn covered((face, size64): Sized, text: &str, left: i64, top: i64) -> HashMap<(i64, i64), u8> {
    let metrics = face.scale(size64).unwrap();
    let outlines = face.outline_scale(size64).unwrap();
    let ascent = i64::from(face.line_metrics(metrics).ascent());
    let baseline = top + (ascent + 32).div_euclid(64);
    let mut rasteriser: Box<Rasteriser<4096, 16_384>> = Box::default();
    let mut sums: HashMap<(i64, i64), u32> = HashMap::new();
    let (mut pen, mut previous) = (0_i64, None);
    for ch in text.chars() {
        let outline = face.glyph(ch).unwrap();
        if let Some(before) = previous {
            pen += i64::from(metrics.to_64(face.kerning_units(before, ch)));
        }
        let placement = outline.placement(outlines);
        let mut coverage = vec![0; placement.len()];
        rasteriser
            .render(outline.path(), outlines, placement, &mut coverage)
            .unwrap();
        let x = left + (pen + 32).div_euclid(64) + i64::from(placement.left());
        let y = baseline - i64::from(placement.top());
        let width = placement.width() as usize;
        for (index, &coverage) in coverage.iter().enumerate() {
            if coverage > 0 {
                let (row, column) = (index / width, index % width);
                *sums.entry((x + column as i64, y + row as i64)).or_insert(0) +=
                    u32::from(coverage);
            }
        }
        pen += i64::from(metrics.to_64(outline.advance()));
        previous = Some(ch);
    }
    sums.into_iter()
        .map(|(at, sum)| (at, sum.min(255) as u8))
        .collect()
}

/// The exact blend of `fore` over `back` at `coverage`, rounded to the
/// nearest whole value, per channel in the colour's own depth.
fn exact<C: RgbColor>(fore: C, back: C, coverage: u8) -> (u8, u8, u8) {
    let one = |f: u8, b: u8| {
        let c = f64::from(coverage) / 255.0;
        (f64::from(f) * c + f64::from(b) * (1.0 - c)).round() as u8
    };
    (
        one(fore.r(), back.r()),
        one(fore.g(), back.g()),
        one(fore.b(), back.b()),
    )
}

/// One line of text as the renderer must draw it: in `color` over
/// `background`, placed in `area` (the node's content, or the marker) and
/// drawn only inside `clip` (the node, or the marker).
struct Expected<C> {
    what: &'static str,
    text: String,
    area: Rect,
    clip: Rect,
    centred: bool,
    /// Whether it is Bold body text: a button's label.
    bold: bool,
    color: C,
    background: C,
}

/// Every pixel the text covers inside its clip is its coverage blended
/// over `background`; every other pixel of `area` is `background`.
fn check_line<C, F>(frame: &F, metrics: &TextMetrics, font: Sized, line: &Expected<C>)
where
    C: RgbColor + std::fmt::Debug,
    F: Frame<C>,
{
    let width = metrics.text_width(&line.text);
    let dx = if line.centred {
        line.area.size.width.saturating_sub(width) / 2
    } else {
        0
    };
    let left = line.area.left() + i64::from(dx);
    let top = line.area.top()
        + i64::from(line.area.size.height.saturating_sub(metrics.line_height()) / 2);
    let cover = covered(font, &line.text, left, top);
    assert!(!cover.is_empty(), "{}: no ink", line.what);
    let inside = |x: i64, y: i64| {
        x >= line.clip.left()
            && x < line.clip.right()
            && y >= line.clip.top()
            && y < line.clip.bottom()
    };
    let mut inked = 0;
    for (&(x, y), &coverage) in &cover {
        if inside(x, y) {
            inked += 1;
            let pixel = frame.at(x, y);
            assert_eq!(
                (pixel.r(), pixel.g(), pixel.b()),
                exact(line.color, line.background, coverage),
                "{}: ({x}, {y}), coverage {coverage}",
                line.what
            );
        }
    }
    assert!(inked > 0, "{}: no ink inside its clip", line.what);
    for y in line.area.top()..line.area.bottom() {
        for x in line.area.left()..line.area.right() {
            if !cover.contains_key(&(x, y)) {
                let pixel = frame.at(x, y);
                assert_eq!(
                    (pixel.r(), pixel.g(), pixel.b()),
                    exact(line.color, line.background, 0),
                    "{}: ({x}, {y}), uncovered",
                    line.what
                );
            }
        }
    }
}

const NAME: FieldId = FieldId(1);
const EMPTY: FieldId = FieldId(2);
const TICK: FieldId = FieldId(3);

/// A screen with text on every background the renderer knows: a label, a
/// button and a pressed button, a value and a placeholder in text inputs,
/// an error and its marker, a checkbox's label. The text has kerned pairs
/// (To, AV, Wa), glyphs that share pixels (two i's with diaereses) and a
/// glyph reaching back over the pen (j).
fn screen(metrics: &TextMetrics, width: u32, height: u32) -> (Tree<16>, [NodeId; 7]) {
    let mut tree: Tree<16> = Tree::new();
    let root = tree
        .column(Style::new().padding(8).gap(8).align(Align::Start))
        .unwrap();
    tree.set_root(root).unwrap();
    let label = tree.text("To AV, Wa\u{EF}\u{EF}ve: jij").unwrap();
    let button = tree.button("Away", ActionId(1)).unwrap();
    let pressed = tree.button("Fjord", ActionId(2)).unwrap();
    let value = tree.text_input(NAME, "Unused").unwrap();
    let placeholder = tree.text_input(EMPTY, "Your name").unwrap();
    let error = tree.field_error(NAME).unwrap();
    let tick = tree.checkbox(TICK, "Tick \u{E5} box").unwrap();
    let nodes = [label, button, pressed, value, placeholder, error, tick];
    for node in nodes {
        tree.append(root, node).unwrap();
    }
    tree.layout(Rect::new(0, 0, width, height), metrics)
        .unwrap();
    tree.set_value(NAME, "jij Typed \u{D8}").unwrap();
    tree.set_field_error(NAME, "Use digits only").unwrap();
    let centre = tree.get(pressed).unwrap().rect().center();
    tree.handle(InputEvent::PointerMove {
        x: centre.x,
        y: centre.y,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    assert_eq!(tree.pressed(), Some(pressed));
    (tree, nodes)
}

/// The lines the screen shows, each with its known background.
fn lines<C: RgbColor>(
    tree: &Tree<16>,
    nodes: [NodeId; 7],
    theme: &Theme<C>,
    metrics: &TextMetrics,
) -> Vec<Expected<C>> {
    let [label, button, pressed, value, placeholder, error, tick] = nodes;
    let rect = |id: NodeId| tree.get(id).unwrap().rect();
    let line = metrics.line_height();
    let indent = i64::from(line + metrics.char_width());
    let words = |area: Rect| {
        Rect::from_edges(
            area.left() + indent,
            area.top(),
            area.right(),
            area.bottom(),
        )
    };
    let marker = {
        let area = rect(error);
        let side = line.min(area.size.height);
        Rect::new(
            area.origin.x,
            area.origin.y + ((area.size.height - side) / 2) as i32,
            side,
            side,
        )
    };
    let input_room = |id: NodeId| rect(id).inset(INPUT_PADDING);
    vec![
        Expected {
            what: "a label on the background",
            text: fit_start(
                "To AV, Wa\u{EF}\u{EF}ve: jij",
                rect(label).size.width,
                metrics,
            )
            .into(),
            area: rect(label),
            clip: rect(label),
            centred: false,
            bold: false,
            color: theme.text,
            background: theme.background,
        },
        Expected {
            what: "a button's label on its fill",
            text: "Away".into(),
            area: rect(button).inset(BUTTON_PADDING),
            clip: rect(button),
            centred: true,
            bold: true,
            color: theme.button_text,
            background: theme.button,
        },
        Expected {
            what: "a pressed button's label on its fill",
            text: "Fjord".into(),
            area: rect(pressed).inset(BUTTON_PADDING),
            clip: rect(pressed),
            centred: true,
            bold: true,
            color: theme.button_text,
            background: theme.button_pressed,
        },
        Expected {
            what: "a value on the text input",
            text: fit_end(
                "jij Typed \u{D8}",
                input_room(value).size.width - 2,
                metrics,
            )
            .into(),
            area: input_room(value),
            clip: rect(value).inset(Insets::uniform(1)),
            centred: false,
            bold: false,
            color: theme.text,
            background: theme.input,
        },
        Expected {
            what: "a placeholder on the text input",
            text: "Your name".into(),
            area: input_room(placeholder),
            clip: rect(placeholder).inset(Insets::uniform(1)),
            centred: false,
            bold: false,
            color: theme.placeholder,
            background: theme.input,
        },
        Expected {
            what: "the error marker's `!` on the marker",
            text: "!".into(),
            area: marker,
            clip: marker,
            centred: true,
            bold: false,
            color: theme.background,
            background: theme.error,
        },
        Expected {
            what: "an error message on the background",
            text: "Use digits only".into(),
            area: words(rect(error)),
            clip: Rect::from_edges(
                marker.right(),
                rect(error).top(),
                rect(error).right(),
                rect(error).bottom(),
            ),
            centred: false,
            bold: false,
            color: theme.error,
            background: theme.background,
        },
        Expected {
            what: "a checkbox's label on the background",
            text: "Tick \u{E5} box".into(),
            area: words(rect(tick).inset(CHOICE_PADDING)),
            clip: Rect::from_edges(
                rect(tick).left() + i64::from(CHOICE_PADDING.left) + i64::from(line),
                rect(tick).top(),
                rect(tick).right(),
                rect(tick).bottom(),
            ),
            centred: false,
            bold: false,
            color: theme.text,
            background: theme.background,
        },
    ]
}

/// Coverage is blended with each node's known background, exactly, in
/// both themes, as `Rgb888` and as `Rgb565`.
#[test]
fn text_is_its_coverage_blended_over_its_nodes_known_background() {
    for (large, width, height) in [(false, 400, 400), (true, 600, 560)] {
        let wide: Theme<Rgb888> = if large {
            Theme::high_contrast()
        } else {
            Theme::light()
        };
        let narrow: Theme<Rgb565> = if large {
            Theme::high_contrast()
        } else {
            Theme::light()
        };
        let metrics = wide.metrics();
        let (tree, nodes) = screen(&metrics, width, height);
        let mut frame = Framebuffer::new(width, height).unwrap();
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render(&tree, &mut frame, &wide, &mut glyphs__)
        }
        .unwrap();
        for line in lines(&tree, nodes, &wide, &metrics) {
            let (metrics, font) = face(&wide, &metrics, &line);
            check_line(&frame, &metrics, font, &line);
        }
        let mut frame = Narrow {
            size: Size::new(width, height),
            pixels: vec![Rgb565::BLACK; (width * height) as usize],
        };
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render(&tree, &mut frame, &narrow, &mut glyphs__)
        }
        .unwrap();
        for line in lines(&tree, nodes, &narrow, &metrics) {
            let (metrics, font) = face(&narrow, &metrics, &line);
            check_line(&frame, &metrics, font, &line);
        }
    }
}

// ---- Any text, anywhere ----------------------------------------------------

/// Any character of the atlases' set.
fn printable() -> impl Strategy<Value = char> {
    prop_oneof![
        3 => (0x20_u32..=0x7E).prop_map(|code| char::from_u32(code).unwrap()),
        1 => (0xA0_u32..=0xFF).prop_map(|code| char::from_u32(code).unwrap()),
    ]
    .prop_filter("the soft hyphen is invisible", |&ch| is_printable(ch))
}

/// Text that lives as long as the tree needs it, for the nodes that keep a
/// `&'static str` (tests only).
fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

/// A tree holding one node of `kind` showing `text`, of a fixed size, at
/// `(x, y)` in a 300 x 140 viewport: everything else is background.
fn one_node(
    kind: u8,
    text: &str,
    at: (u32, u32),
    size: (u32, u32),
    padding: u32,
    theme: &Theme<Rgb888>,
) -> (Tree<4>, NodeId) {
    let mut tree: Tree<4> = Tree::new();
    let root = tree
        .column(
            Style::new()
                .insets(Insets {
                    left: at.0,
                    top: at.1,
                    right: 0,
                    bottom: 0,
                })
                .align(Align::Start),
        )
        .unwrap();
    tree.set_root(root).unwrap();
    let node = match kind {
        0 => tree.text(leak(text)),
        1 => tree.button(leak(text), ActionId(1)),
        2 => tree.text_input(FieldId(1), leak(text)),
        3 => tree.slot(SlotId(1)),
        4 => tree.field_error(FieldId(1)),
        5 => tree.checkbox(FieldId(2), leak(text)),
        _ => {
            let options: &'static [&'static str] = Box::leak(Box::new([leak(text), "Two"]));
            tree.radio_group(FieldId(3), options)
        }
    }
    .unwrap();
    tree.append(root, node).unwrap();
    tree.set_style(
        node,
        Style::new()
            .padding(padding)
            .width(Length::Fixed(size.0))
            .height(Length::Fixed(size.1)),
    )
    .unwrap();
    tree.layout(Rect::new(0, 0, 300, 140), &theme.metrics())
        .unwrap();
    match kind {
        2 => tree.set_value(FieldId(1), text).unwrap(),
        3 => tree.set_slot(SlotId(1), text).unwrap(),
        4 => tree.set_field_error(FieldId(1), leak(text)).unwrap(),
        _ => {}
    }
    (tree, node)
}

proptest! {
    /// Any text of the set, in any kind of node that shows text, at any
    /// size and place, in either theme: painting any area gives exactly
    /// the pixels of a full repaint there and touches nothing outside it,
    /// and no glyph pixel lands outside its node.
    #[test]
    fn any_text_repaints_exactly_and_stays_inside_its_node(
        large in any::<bool>(),
        kind in 0_u8..7,
        chars in proptest::collection::vec(printable(), 0..24),
        at in (0_u32..80, 0_u32..60),
        size in (0_u32..260, 0_u32..80),
        padding in 0_u32..8,
        focused in any::<bool>(),
        areas in proptest::collection::vec((0_i32..300, 0_i32..140, 1_u32..120, 1_u32..60), 1..5),
    ) {
        let theme = if large { Theme::high_contrast() } else { Theme::light() };
        let text: String = chars.into_iter().collect();
        let (mut tree, node) = one_node(kind, &text, at, size, padding, &theme);
        if focused && tree.get(node).unwrap().is_focusable() {
            tree.set_focus(Some(node)).unwrap();
        }
        let rect = tree.get(node).unwrap().rect();
        let mut full = Framebuffer::filled(300, 140, Rgb888::new(1, 2, 3)).unwrap();
        { nowayos_testkit::glyph_store!(glyphs__); render(&tree, &mut full, &theme, &mut glyphs__) }.unwrap();
        for y in 0..140_u32 {
            for x in 0..300_u32 {
                let inside = rect.contains(Point::new(x as i32, y as i32));
                if !inside {
                    prop_assert_eq!(full.pixel(x, y), Some(theme.background), "({}, {}) outside {:?}", x, y, rect);
                }
            }
        }
        for (x, y, w, h) in areas {
            let area = Rect::new(x, y, w, h);
            let mut part = Framebuffer::filled(300, 140, Rgb888::new(1, 2, 3)).unwrap();
            { nowayos_testkit::glyph_store!(glyphs__); render_area(&tree, &mut part, &theme, area, &mut glyphs__) }.unwrap();
            for py in 0..140_u32 {
                for px in 0..300_u32 {
                    let want = if area.contains(Point::new(px as i32, py as i32)) {
                        full.pixel(px, py)
                    } else {
                        Some(Rgb888::new(1, 2, 3))
                    };
                    prop_assert_eq!(part.pixel(px, py), want, "({}, {}) painting {:?}", px, py, area);
                }
            }
        }
    }
}
