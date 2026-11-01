// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every node draws only inside its own rectangle, so repainting the dirty
//! areas gives exactly the pixels of a full repaint — whatever the sizes,
//! including nodes squeezed smaller than their content, in both themes'
//! atlas text — and nothing a tree can lay out makes drawing overflow.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Dimensions, OriginDimensions, Point as EgPoint, Size};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::primitives::Rectangle;
use nowayos_display::{Point, Rect};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_render_eg::{Theme, render, render_dirty};
use nowayos_testkit::Framebuffer;
use nowayos_ui::{
    ActionId, Align, FieldId, INPUT_PADDING, Length, MAX_VIEWPORT_OFFSET, MAX_VIEWPORT_SIZE,
    NodeId, NodeKind, SlotId, Style, Tree,
};
use proptest::prelude::*;

const CAP: usize = 24;

fn full<const N: usize>(tree: &Tree<N>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    render(tree, &mut fb, theme).unwrap();
    fb
}

/// Pixels of `fb` (whose origin is the viewport's) that differ from the
/// background and lie outside every rectangle in `allowed`.
fn stray_pixels(fb: &Framebuffer, background: Rgb888, allowed: &[Rect]) -> Vec<Point> {
    let mut stray = Vec::new();
    let size = fb.size();
    for y in 0..size.height {
        for x in 0..size.width {
            let p = Point::new(x as i32, y as i32);
            if fb.pixel(x, y) != Some(background) && !allowed.iter().any(|r| r.contains(p)) {
                stray.push(p);
            }
        }
    }
    stray
}

/// The rectangles of the nodes that draw anything (text, controls, error
/// lines).
fn drawing_rects<const N: usize>(tree: &Tree<N>) -> Vec<Rect> {
    tree.paint_order()
        .filter(|(_, node)| {
            matches!(
                node.kind(),
                NodeKind::Text(_)
                    | NodeKind::Button { .. }
                    | NodeKind::TextInput { .. }
                    | NodeKind::Slot { .. }
                    | NodeKind::FieldError { .. }
                    | NodeKind::Checkbox { .. }
                    | NodeKind::RadioGroup { .. }
            )
        })
        .map(|(_, node)| node.rect())
        .collect()
}

#[test]
fn a_label_shorter_than_a_line_draws_only_inside_its_rect() {
    // Regression (review 2026-10-01): a label squeezed below one line height
    // drew its whole glyph cell, over the node below it, and only a full
    // repaint showed that overflow. At 24 px, H's ink starts 7 pixels below
    // its line's top: a label 12 high shows its top 5 rows.
    let theme = Theme::light();
    let mut tree: Tree<16> = Tree::new();
    let root = tree.column(Style::new()).unwrap();
    tree.set_root(root).unwrap();
    let squashed = tree
        .add_child(
            root,
            NodeKind::Text("HHHH"),
            Style::new().height(Length::Fixed(12)),
        )
        .unwrap();
    let below = tree.text("b").unwrap();
    tree.append(root, below).unwrap();
    tree.layout(Rect::new(0, 0, 80, 60), &theme.metrics())
        .unwrap();
    assert_eq!(tree.get(squashed).unwrap().rect(), Rect::new(0, 0, 80, 12));

    let mut incremental = full(&tree, &theme);
    assert_eq!(
        stray_pixels(&incremental, theme.background, &drawing_rects(&tree)),
        []
    );
    // Only part of the label's glyphs is visible, but that part is drawn.
    let label_area = Rect::new(0, 0, 80, 12);
    assert!(
        stray_pixels(&incremental, theme.background, &[])
            .iter()
            .any(|p| label_area.contains(*p))
    );

    tree.clear_dirty();
    tree.set_text(below, "c").unwrap();
    render_dirty(&tree, &mut incremental, &theme).unwrap();
    assert_eq!(incremental.diff(&full(&tree, &theme)), None);
}

#[test]
fn the_caret_of_a_narrow_input_stays_inside_the_input() {
    // Regression (review 2026-10-01): an input narrower than its padding
    // drew its caret one pixel to the right of its rectangle.
    let theme = Theme::light();
    let mut tree: Tree<16> = Tree::new();
    let root = tree.row(Style::new().align(Align::Start)).unwrap();
    tree.set_root(root).unwrap();
    let input = tree.text_input(FieldId(1), "").unwrap();
    tree.append(root, input).unwrap();
    tree.set_style(
        input,
        Style::new().insets(INPUT_PADDING).width(Length::Fixed(3)),
    )
    .unwrap();
    tree.layout(Rect::new(0, 0, 40, 30), &theme.metrics())
        .unwrap();
    tree.set_focus(Some(input)).unwrap();
    let fb = full(&tree, &theme);
    assert_eq!(
        stray_pixels(&fb, theme.background, &drawing_rects(&tree)),
        []
    );
}

#[test]
fn text_cut_at_a_dirty_edge_matches_a_full_repaint() {
    // Dirty areas that cut through the middle of a long label and a value,
    // across glyphs and between them, in strips both ways: drawing only
    // what an area can show must give the same pixels as drawing it all.
    for theme in [Theme::light(), Theme::high_contrast()] {
        let (width, height) = (300, 100);
        let mut tree: Tree<16> = Tree::new();
        let root = tree.column(Style::new().padding(3).gap(2)).unwrap();
        tree.set_root(root).unwrap();
        let label = tree.text("The quick brown fox jumps over").unwrap();
        tree.append(root, label).unwrap();
        let input = tree.text_input(FieldId(1), "placeholder").unwrap();
        tree.append(root, input).unwrap();
        tree.layout(Rect::new(0, 0, width, height), &theme.metrics())
            .unwrap();
        tree.set_value(FieldId(1), "synthetic value, jumbled")
            .unwrap();
        let expected = full(&tree, &theme);
        let columns = (0..100).map(|x| Rect::new(x * 3 + 1, 0, 7, height));
        let rows = (0..33).map(|y| Rect::new(0, y * 3 + 1, width, 5));
        for area in columns.chain(rows) {
            let mut fb = Framebuffer::filled(width, height, Rgb888::new(1, 2, 3)).unwrap();
            nowayos_render_eg::render_area(&tree, &mut fb, &theme, area).unwrap();
            for y in 0..height {
                for x in 0..width {
                    let inside = area.contains(Point::new(x as i32, y as i32));
                    let want = if inside {
                        expected.pixel(x, y)
                    } else {
                        Some(Rgb888::new(1, 2, 3))
                    };
                    assert_eq!(fb.pixel(x, y), want, "({x}, {y}) painting {area:?}");
                }
            }
        }
    }
}

/// A draw target that keeps nothing, covering every coordinate a viewport
/// may use (and more): for drawing at extreme coordinates without
/// allocating a framebuffer to match. Its size is as large as
/// `embedded-graphics` allows (it must fit an `i32`).
struct Sink {
    pixels: u64,
}

impl Dimensions for Sink {
    fn bounding_box(&self) -> Rectangle {
        let half = 1 << 30;
        Rectangle::new(
            EgPoint::new(-half, -half),
            Size::new(i32::MAX as u32, i32::MAX as u32),
        )
    }
}

impl DrawTarget for Sink {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for _ in pixels {
            self.pixels += 1;
        }
        Ok(())
    }

    fn fill_solid(&mut self, _area: &Rectangle, _color: Rgb888) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn extreme_layouts_render_without_overflow() {
    // Regression (review 2026-10-01): coordinates near the ends of the i32
    // range overflowed inside embedded-graphics, a panic with overflow
    // checks on (and an abort in a PD). Layout now refuses such viewports;
    // within the range it accepts, absurd styles must not overflow either.
    // Regression: a checkbox's tick was drawn at the viewport's own
    // coordinates, where embedded-graphics' thick-line joins multiply a
    // coordinate by the line's direction twice - past i32 at 2^24 for a
    // mark 32 pixels high, and for the large-text theme's 36. The light
    // theme's marks, 24 pixels, stay below it: the large-text run is the
    // one that catches the bug.
    for theme in [Theme::light(), Theme::high_contrast()] {
        extreme_layouts_render_without_overflow_in(&theme);
    }
}

fn extreme_layouts_render_without_overflow_in(theme: &Theme<Rgb888>) {
    let theme = *theme;
    let far = MAX_VIEWPORT_OFFSET - MAX_VIEWPORT_SIZE as i32;
    for viewport in [
        Rect::new(far, far, MAX_VIEWPORT_SIZE, MAX_VIEWPORT_SIZE),
        Rect::new(-MAX_VIEWPORT_OFFSET, -MAX_VIEWPORT_OFFSET, 64, 48),
        Rect::new(far, -MAX_VIEWPORT_OFFSET, 64, MAX_VIEWPORT_SIZE),
    ] {
        for huge in [0, 1 << 20, u32::MAX / 2, u32::MAX] {
            let mut tree: Tree<16> = Tree::new();
            let root = tree
                .row(
                    Style::new()
                        .gap(huge)
                        .padding(huge / 3)
                        .align(Align::Center),
                )
                .unwrap();
            tree.set_root(root).unwrap();
            let label = tree
                .add_child(
                    root,
                    NodeKind::Text("Hello there"),
                    Style::new().width(Length::Fixed(huge)),
                )
                .unwrap();
            let input = tree.text_input(FieldId(1), "Name").unwrap();
            tree.append(root, input).unwrap();
            let button = tree.button("Next", ActionId(1)).unwrap();
            tree.append(root, button).unwrap();
            tree.set_style(
                button,
                Style::new().padding(huge).height(Length::Fixed(huge)),
            )
            .unwrap();
            // Marks drawn by embedded-graphics' own primitives, too.
            let tick = tree.checkbox(FieldId(2), "Tick").unwrap();
            tree.append(root, tick).unwrap();
            let choice = tree.radio_group(FieldId(3), &["One", "Two"]).unwrap();
            tree.append(root, choice).unwrap();
            tree.set_style(choice, Style::new().width(Length::Fixed(huge)))
                .unwrap();
            let error = tree.field_error(FieldId(1)).unwrap();
            tree.append(root, error).unwrap();
            tree.layout(viewport, &theme.metrics()).unwrap();
            tree.set_focus(Some(input)).unwrap();
            tree.set_value(FieldId(1), "synthetic").unwrap();
            tree.set_checked(FieldId(2), true).unwrap();
            tree.set_selected(FieldId(3), Some(1)).unwrap();
            tree.set_field_error(FieldId(1), "Wrong").unwrap();
            let mut sink = Sink { pixels: 0 };
            render(&tree, &mut sink, &theme).unwrap();
            render_dirty(&tree, &mut sink, &theme).unwrap();
            assert!(tree.get(label).is_some());
            // With ordinary styles the controls are on screen and drawn, so
            // the drawing code is really exercised at these coordinates;
            // absurd styles may push everything out of view.
            if huge == 0 {
                assert!(sink.pixels > 0, "{viewport:?}");
            }
        }
    }
}

// ---- Random trees -----------------------------------------------------------

#[derive(Debug, Clone)]
enum Build {
    Add(u8, Style),
    Append(usize, usize),
    SetRoot(usize),
}

fn length() -> impl Strategy<Value = Length> {
    prop_oneof![
        3 => Just(Length::Auto),
        1 => Just(Length::Fill),
        // Includes sizes below a line of text and below the padding.
        2 => (0u32..40).prop_map(Length::Fixed),
        1 => (0u32..300).prop_map(Length::Fixed),
    ]
}

fn align() -> impl Strategy<Value = Align> {
    prop_oneof![
        Just(Align::Start),
        Just(Align::Center),
        Just(Align::End),
        Just(Align::Stretch)
    ]
}

fn style() -> impl Strategy<Value = Style> {
    (length(), length(), 0u32..14, 0u32..8, align()).prop_map(|(w, h, pad, gap, align)| {
        Style::new()
            .width(w)
            .height(h)
            .padding(pad)
            .gap(gap)
            .align(align)
    })
}

fn build() -> impl Strategy<Value = Build> {
    prop_oneof![
        3 => (0u8..11, style()).prop_map(|(k, s)| Build::Add(k, s)),
        4 => (0usize..CAP, 0usize..CAP).prop_map(|(p, c)| Build::Append(p, c)),
        1 => (0usize..CAP).prop_map(Build::SetRoot),
    ]
}

fn kind(k: u8, n: usize) -> NodeKind {
    match k {
        0 => NodeKind::Column,
        1 => NodeKind::Row,
        2 => NodeKind::Stack,
        3 => NodeKind::Text("Label text"),
        4 => NodeKind::Button {
            label: "Go on",
            action: ActionId(n as u16),
        },
        5 => NodeKind::TextInput {
            field: FieldId(n as u16),
            placeholder: "type here",
            value: Default::default(),
            max_len: 64,
            mode: nowayos_ui::InputMode::Text,
        },
        7 => NodeKind::Slot {
            slot: SlotId(n as u16),
            text: Default::default(),
        },
        8 => NodeKind::FieldError {
            field: FieldId(n as u16),
            message: Some("Fix this field"),
        },
        9 => NodeKind::Checkbox {
            field: FieldId(n as u16),
            label: "Tick me",
            checked: n.is_multiple_of(2),
        },
        10 => NodeKind::RadioGroup {
            field: FieldId(n as u16),
            options: &["One", "Two", "Three"],
            selected: None,
        },
        _ => NodeKind::Spacer,
    }
}

fn random_tree(steps: &[Build]) -> (Tree<CAP>, Vec<NodeId>) {
    let mut tree: Tree<CAP> = Tree::new();
    let mut ids = Vec::new();
    for step in steps {
        match step {
            Build::Add(k, s) => {
                if let Ok(id) = tree.add(kind(*k, ids.len()), *s) {
                    ids.push(id);
                }
            }
            Build::Append(p, c) => {
                if let (Some(&p), Some(&c)) = (ids.get(*p), ids.get(*c)) {
                    let _ = tree.append(p, c);
                }
            }
            Build::SetRoot(r) => {
                if let Some(&r) = ids.get(*r) {
                    let _ = tree.set_root(r);
                }
            }
        }
    }
    (tree, ids)
}

/// One change to a laid-out tree.
#[derive(Debug, Clone)]
enum Step {
    Input(InputEvent),
    SetText(usize, &'static str),
    SetValue(u16, &'static str),
    SetSlot(u16, &'static str),
    SetError(u16, Option<&'static str>),
    ClearValues,
}

fn step() -> impl Strategy<Value = Step> {
    let key = prop_oneof![
        Just(KeyCode::TAB),
        Just(KeyCode::ENTER),
        Just(KeyCode::SPACE),
        Just(KeyCode::BACKSPACE),
        Just(KeyCode::DOWN),
        Just(KeyCode::UP),
        Just(KeyCode::LEFT),
        Just(KeyCode::RIGHT),
        Just(KeyCode::LEFT_SHIFT),
        (0x04u16..=0x38).prop_map(|u| KeyCode::new(u).unwrap()),
    ];
    let phase = prop_oneof![
        Just(TouchPhase::Start),
        Just(TouchPhase::Move),
        Just(TouchPhase::End),
        Just(TouchPhase::Cancel)
    ];
    let words = prop_oneof![
        Just("x"),
        Just("Longer text than fits"),
        Just("Wide WWWW"),
        Just("jij fjord, \u{C5}ngstr\u{F6}m \u{B5}g"),
    ];
    let error = prop_oneof![
        Just(None),
        Just(Some("Wrong")),
        Just(Some("A much longer error"))
    ];
    prop_oneof![
        4 => key.clone().prop_map(|k| Step::Input(InputEvent::KeyDown(k))),
        1 => key.prop_map(|k| Step::Input(InputEvent::KeyUp(k))),
        2 => (-10i32..130, -10i32..100).prop_map(|(x, y)| Step::Input(InputEvent::PointerMove { x, y })),
        2 => any::<bool>().prop_map(|down| Step::Input(InputEvent::PointerButton { button: 0, down })),
        1 => (-10i32..130, -10i32..100, phase)
            .prop_map(|(x, y, phase)| Step::Input(InputEvent::Touch { id: 0, x, y, phase })),
        1 => (0usize..CAP, words.clone()).prop_map(|(n, w)| Step::SetText(n, w)),
        1 => (0u16..CAP as u16, words.clone()).prop_map(|(f, w)| Step::SetValue(f, w)),
        1 => (0u16..CAP as u16, words).prop_map(|(f, w)| Step::SetSlot(f, w)),
        1 => (0u16..CAP as u16, error).prop_map(|(f, e)| Step::SetError(f, e)),
        1 => Just(Step::ClearValues),
    ]
}

fn apply(tree: &mut Tree<CAP>, ids: &[NodeId], step: &Step) {
    match step {
        Step::Input(event) => {
            let _ = tree.handle(*event);
        }
        Step::SetText(n, text) => {
            if let Some(&id) = ids.get(*n) {
                let _ = tree.set_text(id, text);
            }
        }
        Step::SetValue(field, text) => {
            let _ = tree.set_value(FieldId(*field), text);
        }
        Step::SetSlot(slot, text) => {
            let _ = tree.set_slot(SlotId(*slot), text);
        }
        Step::SetError(field, Some(message)) => {
            let _ = tree.set_field_error(FieldId(*field), message);
        }
        Step::SetError(field, None) => {
            let _ = tree.clear_field_error(FieldId(*field));
        }
        Step::ClearValues => tree.clear_values(),
    }
}

/// A screen-shaped tree: a root container with items, each a leaf or a
/// container with leaves. Random build steps rarely make a tree this
/// meaningful; screens do every time.
#[derive(Debug, Clone)]
struct Item {
    kind: u8,
    style: Style,
    children: Vec<(u8, Style)>,
}

fn item() -> impl Strategy<Value = Item> {
    (
        0u8..11,
        style(),
        proptest::collection::vec((3u8..11, style()), 0..4),
    )
        .prop_map(|(kind, style, children)| Item {
            kind,
            style,
            children,
        })
}

fn screen_tree(root_kind: u8, root_style: Style, items: &[Item]) -> (Tree<CAP>, Vec<NodeId>) {
    let mut tree: Tree<CAP> = Tree::new();
    let mut ids = Vec::new();
    let root = tree.add(kind(root_kind % 3, 0), root_style).unwrap();
    tree.set_root(root).unwrap();
    ids.push(root);
    for item in items {
        let Ok(id) = tree.add_child(root, kind(item.kind, ids.len()), item.style) else {
            break;
        };
        ids.push(id);
        if item.kind < 3 {
            for (child_kind, child_style) in &item.children {
                if let Ok(child) = tree.add_child(id, kind(*child_kind, ids.len()), *child_style) {
                    ids.push(child);
                }
            }
        }
    }
    (tree, ids)
}

/// Lays `tree` out in `w` x `h`, then checks the dirty-rectangle promise
/// after each change: repainting only the dirty areas over the previous
/// frame gives exactly a full repaint, and nothing is ever drawn outside
/// the nodes that draw.
fn check_changes(
    mut tree: Tree<CAP>,
    ids: &[NodeId],
    changes: &[Step],
    w: u32,
    h: u32,
    theme: &Theme<Rgb888>,
) -> Result<(), TestCaseError> {
    let theme = *theme;
    if tree
        .layout(Rect::new(0, 0, w, h), &theme.metrics())
        .is_err()
    {
        return Ok(());
    }
    let mut incremental = full(&tree, &theme);
    prop_assert_eq!(
        stray_pixels(&incremental, theme.background, &drawing_rects(&tree)),
        vec![]
    );
    tree.clear_dirty();
    for (index, change) in changes.iter().enumerate() {
        apply(&mut tree, ids, change);
        render_dirty(&tree, &mut incremental, &theme).unwrap();
        tree.clear_dirty();
        let expected = full(&tree, &theme);
        prop_assert_eq!(
            incremental.diff(&expected),
            None,
            "after change {} ({:?})",
            index,
            change
        );
        prop_assert_eq!(
            stray_pixels(&expected, theme.background, &drawing_rects(&tree)),
            vec![]
        );
    }
    Ok(())
}

/// The theme a case is drawn in: the light theme or the large-text one.
fn theme(large: bool) -> Theme<Rgb888> {
    if large {
        Theme::high_contrast()
    } else {
        Theme::light()
    }
}

proptest! {
    /// Screen-shaped trees, with fixed sizes smaller than their content.
    #[test]
    fn dirty_repaint_equals_full_repaint_on_screens(
        root_kind in 0u8..3,
        root_style in style(),
        items in proptest::collection::vec(item(), 1..8),
        changes in proptest::collection::vec(step(), 1..16),
        w in 20u32..160,
        h in 20u32..120,
        large in any::<bool>(),
    ) {
        let (tree, ids) = screen_tree(root_kind, root_style, &items);
        check_changes(tree, &ids, &changes, w, h, &theme(large))?;
    }

    /// Trees from random build steps: any shape the API allows.
    #[test]
    fn dirty_repaint_equals_full_repaint_on_random_trees(
        steps in proptest::collection::vec(build(), 0..60),
        changes in proptest::collection::vec(step(), 0..16),
        w in 0u32..160,
        h in 0u32..120,
        large in any::<bool>(),
    ) {
        let (tree, ids) = random_tree(&steps);
        check_changes(tree, &ids, &changes, w, h, &theme(large))?;
    }
}
