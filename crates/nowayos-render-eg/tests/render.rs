// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rendering tests against the in-memory framebuffer from `nowayos-testkit`:
//! golden-image snapshots of synthetic screens - in both themes, with text
//! drawn from the glyph atlases, as `Rgb888` and as `Rgb565`, and
//! once in the bring-up bitmap font - and the guarantee that repainting
//! only the dirty areas gives exactly the same pixels as a full repaint.
//!
//! Regenerate the golden images after an intended change with
//! `NOWAYOS_UPDATE_SNAPSHOTS=1 make test`, and review them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::PathBuf;

use embedded_graphics::mono_font::iso_8859_1::FONT_6X10;
use embedded_graphics::pixelcolor::{Rgb565, Rgb888, RgbColor};
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, Size};
use nowayos_display::{FramebufferLayout, Insets, PixelFormat, Rect};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_render_eg::{
    Font, RawFramebuffer, RawFramebufferError, Theme, TypeScale, render, render_area, render_dirty,
};
use nowayos_testkit::{Framebuffer, check_snapshot};
use nowayos_ui::{
    ActionId, Align, FieldId, Length, Navigator, NodeId, NodeKind, ScreenId, SlotId, Style,
    TextRole, TextStyle, Tree, UiError, UiEvent,
};

const CONTINUE: ActionId = ActionId(1);
const HELP: ActionId = ActionId(2);
const NAME: FieldId = FieldId(1);
/// The light theme's screens: room for its body text, 18 px.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
/// The large-text theme's screens: half as big again, for its 24 px body
/// text and larger titles.
const LARGE_WIDTH: u32 = 480;
const LARGE_HEIGHT: u32 = 360;

struct Welcome {
    tree: Tree<16>,
    name: NodeId,
    help: NodeId,
    continue_button: NodeId,
}

/// A synthetic, generic welcome screen: title, hint, one text input and a
/// row of two buttons pinned to the bottom.
fn welcome(theme: &Theme<Rgb888>, width: u32, height: u32) -> Welcome {
    welcome_at(theme, width, height, 1)
}

/// The welcome screen at device scale `scale`, in a viewport `scale` times
/// `width` x `height` layout units.
fn welcome_at(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> Welcome {
    let mut tree: Tree<16> = Tree::new();
    let screen = tree.column(Style::new().padding(8).gap(6)).unwrap();
    tree.set_root(screen).unwrap();
    for text in ["Welcome", "Please enter your name"] {
        let line = tree.text(text).unwrap();
        tree.append(screen, line).unwrap();
    }
    let name = tree.text_input(NAME, "Your name").unwrap();
    tree.append(screen, name).unwrap();
    let push_down = tree.spacer().unwrap();
    tree.append(screen, push_down).unwrap();
    let buttons = tree.row(Style::new().gap(6).align(Align::Center)).unwrap();
    tree.append(screen, buttons).unwrap();
    let help = tree.button("Help", HELP).unwrap();
    tree.append(buttons, help).unwrap();
    let push_right = tree.spacer().unwrap();
    tree.append(buttons, push_right).unwrap();
    let continue_button = tree.button("Continue", CONTINUE).unwrap();
    tree.append(buttons, continue_button).unwrap();
    tree.layout(
        Rect::new(0, 0, width * scale, height * scale),
        &theme.metrics_at(nowayos_display::DeviceScale::whole(scale).unwrap()),
    )
    .unwrap();
    Welcome {
        tree,
        name,
        help,
        continue_button,
    }
}

fn full_frame(tree: &Tree<16>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

fn key(tree: &mut Tree<16>, key: KeyCode) -> Option<UiEvent> {
    tree.handle(InputEvent::KeyDown(key))
}

fn type_text(tree: &mut Tree<16>, text: &str) {
    for ch in text.chars() {
        let upper = ch.is_ascii_uppercase();
        if upper {
            key(tree, KeyCode::LEFT_SHIFT);
        }
        key(tree, KeyCode::letter(ch).expect("letters only"));
        if upper {
            tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
        }
    }
}

#[test]
fn welcome_screen_matches_golden_image() {
    let theme = Theme::light();
    let screen = welcome(&theme, WIDTH, HEIGHT);
    let fb = full_frame(&screen.tree, &theme);
    check_snapshot(&fb, &golden("welcome.png")).unwrap();
}

/// The welcome screen with "Ada" typed into its focused input, in `theme`
/// at `width` x `height`.
fn welcome_typing(theme: &Theme<Rgb888>, width: u32, height: u32) -> Welcome {
    welcome_typing_at(theme, width, height, 1)
}

/// As `welcome_typing`, at device scale `scale`.
fn welcome_typing_at(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> Welcome {
    let mut screen = welcome_at(theme, width, height, scale);
    screen.tree.set_focus(Some(screen.name)).unwrap();
    type_text(&mut screen.tree, "Ada");
    assert_eq!(screen.tree.value(NAME), Some("Ada"));
    screen
}

/// The themes, each with the size its screens are drawn at and the name
/// its golden images carry.
fn themes() -> [(Theme<Rgb888>, u32, u32, &'static str); 2] {
    [
        (Theme::light(), WIDTH, HEIGHT, ""),
        (
            Theme::high_contrast(),
            LARGE_WIDTH,
            LARGE_HEIGHT,
            "-high-contrast",
        ),
    ]
}

/// Lines of text, typed text with its caret, labels on buttons and a focus
/// ring, in each theme's atlas text.
#[test]
fn focused_input_with_text_matches_golden_images_in_every_theme() {
    for (theme, width, height, suffix) in themes() {
        let screen = welcome_typing(&theme, width, height);
        let fb = full_frame(&screen.tree, &theme);
        check_snapshot(&fb, &golden(&format!("welcome-typing{suffix}.png"))).unwrap();
    }
}

/// The bitmap fonts stay for bring-up and debug screens: the light
/// theme's colours with the 6x10 font draw exactly what the first cut drew
/// before the atlases, at its size.
#[test]
fn the_bring_up_font_draws_as_before() {
    let theme = Theme {
        type_scale: TypeScale::uniform(Font::Mono(&FONT_6X10)),
        ..Theme::light()
    };
    assert_eq!(
        (theme.metrics().char_width(), theme.metrics().line_height()),
        (6, 10)
    );
    let screen = welcome(&theme, 160, 120);
    let fb = full_frame(&screen.tree, &theme);
    check_snapshot(&fb, &golden("welcome-bring-up.png")).unwrap();
}

/// One interaction applied to the tree.
type Step = Box<dyn Fn(&mut Tree<16>)>;

/// The dirty-rectangle promise: after any interaction, repainting only
/// the dirty areas over the previous frame gives exactly the pixels a full
/// repaint would.
#[test]
fn dirty_repaint_equals_full_repaint() {
    for (theme, width, height, _) in themes() {
        for scale in [1, 2] {
            dirty_repaint_equals_full_repaint_in(&theme, width, height, scale);
        }
    }
}

fn dirty_repaint_equals_full_repaint_in(
    theme: &Theme<Rgb888>,
    width: u32,
    height: u32,
    scale: u32,
) {
    let theme = *theme;
    let mut screen = welcome_at(&theme, width, height, scale);
    let (width, height) = (width * scale, height * scale);
    let mut incremental = full_frame(&screen.tree, &theme);
    screen.tree.clear_dirty();

    let continue_centre = screen
        .tree
        .get(screen.continue_button)
        .unwrap()
        .rect()
        .center();
    let steps: Vec<Step> = vec![
        Box::new(|t| {
            key(t, KeyCode::TAB);
        }),
        Box::new(|t| type_text(t, "Sam")),
        Box::new(|t| {
            key(t, KeyCode::BACKSPACE);
        }),
        Box::new(|t| {
            key(t, KeyCode::TAB);
        }),
        Box::new(move |t| {
            t.handle(InputEvent::PointerMove {
                x: continue_centre.x,
                y: continue_centre.y,
            });
            t.handle(InputEvent::PointerButton {
                button: 0,
                down: true,
            });
        }),
        Box::new(|t| {
            t.handle(InputEvent::PointerButton {
                button: 0,
                down: false,
            });
        }),
        Box::new(|t| t.clear_values()),
    ];
    for (index, step) in steps.iter().enumerate() {
        step(&mut screen.tree);
        let dirty_area: u64 = screen.tree.dirty_rects().iter().map(Rect::area).sum();
        assert!(
            dirty_area < u64::from(width * height),
            "step {index} repainted the whole screen"
        );
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&screen.tree, &mut incremental, &theme, &mut glyphs__)
        }
        .unwrap();
        screen.tree.clear_dirty();
        let full = full_frame(&screen.tree, &theme);
        assert_eq!(
            incremental.diff(&full),
            None,
            "after step {index} at scale {scale}"
        );
    }
}

/// At device scale 2 a screen is drawn at its viewport's own pixels -
/// text from the atlas twice the size, rings and borders twice as wide -
/// never a scale-1 frame doubled. The focused welcome screen at 2x matches
/// its approved image; it differs from the 1x frame with each pixel
/// doubled; and its text has detail a doubled frame cannot have: 2 x 2
/// blocks whose pixels differ.
#[test]
fn at_scale_2_a_screen_is_drawn_at_its_own_pixels() {
    let theme = Theme::light();
    let large = welcome_typing_at(&theme, WIDTH, HEIGHT, 2);
    let crisp = full_frame(&large.tree, &theme);
    assert_eq!(crisp.size(), Size::new(2 * WIDTH, 2 * HEIGHT));
    check_snapshot(&crisp, &golden("welcome-typing-2x.png")).unwrap();

    let small = welcome_typing(&theme, WIDTH, HEIGHT);
    let one = full_frame(&small.tree, &theme);
    let pixels = (0..2 * HEIGHT)
        .flat_map(|y| (0..2 * WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| one.pixel(x / 2, y / 2).unwrap())
        .collect();
    let doubled = Framebuffer::from_pixels(2 * WIDTH, 2 * HEIGHT, pixels).unwrap();
    assert!(
        crisp.diff(&doubled).is_some(),
        "the 2x frame is not the 1x frame doubled"
    );

    // Every block of a doubled frame is one colour; the crisp frame's text
    // has many that are not.
    let mixed = |frame: &Framebuffer, area: Rect| {
        let mut count = 0;
        for by in (area.top()..area.bottom()).step_by(2) {
            for bx in (area.left()..area.right()).step_by(2) {
                let (x, y) = (bx as u32, by as u32);
                let first = frame.pixel(x, y).unwrap();
                if [(1, 0), (0, 1), (1, 1)]
                    .iter()
                    .any(|&(dx, dy)| frame.pixel(x + dx, y + dy).unwrap() != first)
                {
                    count += 1;
                }
            }
        }
        count
    };
    let title = large.tree.paint_order().nth(1).unwrap().1.rect();
    assert_eq!(mixed(&doubled, title), 0);
    assert!(
        mixed(&crisp, title) > 50,
        "{} mixed blocks",
        mixed(&crisp, title)
    );
    // The ring and borders are whole layout units: twice as many pixels.
    let ring = large.tree.get(large.name).unwrap().rect();
    let edge = crisp.pixel(ring.origin.x as u32 + 1, ring.center().y as u32);
    assert_eq!(edge, Some(theme.focus), "the ring's second pixel at 2x");
}

#[test]
fn painting_an_area_touches_nothing_outside_it() {
    let theme = Theme::light();
    let screen = welcome(&theme, WIDTH, HEIGHT);
    let marker = Rgb888::new(0xFF, 0x00, 0xFF);
    let mut fb = Framebuffer::filled(WIDTH, HEIGHT, marker).unwrap();
    let area = Rect::new(20, 30, 50, 25);
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_area(&screen.tree, &mut fb, &theme, area, &mut glyphs__)
    }
    .unwrap();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let inside = area.contains(nowayos_display::Point::new(x as i32, y as i32));
            let untouched = fb.pixel(x, y) == Some(marker);
            assert_eq!(untouched, !inside, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn focus_and_press_are_visible() {
    let theme = Theme::light();
    let mut screen = welcome(&theme, WIDTH, HEIGHT);
    let before = full_frame(&screen.tree, &theme);
    assert_eq!(before.count(theme.focus), 0);
    // Focus alone shows no ring until a key.
    screen.tree.set_focus(Some(screen.help)).unwrap();
    assert_eq!(full_frame(&screen.tree, &theme).count(theme.focus), 0);
    // Tab shows it.
    screen.tree.set_focus(None).unwrap();
    key(&mut screen.tree, KeyCode::TAB);
    assert_eq!(screen.tree.focused(), Some(screen.name));
    let focused = full_frame(&screen.tree, &theme);
    assert!(focused.count(theme.focus) > 0);
    screen.tree.set_focus(Some(screen.help)).unwrap();
    let centre = screen.tree.get(screen.help).unwrap().rect().center();
    screen.tree.handle(InputEvent::PointerMove {
        x: centre.x,
        y: centre.y,
    });
    screen.tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
    let pressed = full_frame(&screen.tree, &theme);
    assert!(pressed.count(theme.button_pressed) > 0);
    // A click hides the ring.
    assert!(!screen.tree.focus_visible());
    assert_eq!(pressed.count(theme.focus), 0);
    assert_eq!(
        screen.tree.handle(InputEvent::PointerButton {
            button: 0,
            down: false
        }),
        Some(UiEvent::Action(HELP))
    );
}

/// The pointer is the UI loop's, drawn over a frame, never the
/// tree's: a frame of the tree - every golden image - never holds it.
#[test]
fn a_tree_s_frame_never_holds_the_pointer() {
    let theme = Theme::light();
    let mut screen = welcome(&theme, WIDTH, HEIGHT);
    let before = full_frame(&screen.tree, &theme);
    screen.tree.handle(InputEvent::PointerMove { x: 40, y: 30 });
    assert!(screen.tree.pointer_shown().is_some());
    assert_eq!(full_frame(&screen.tree, &theme), before);
}

#[test]
fn high_contrast_theme_uses_the_large_text() {
    // Body text: the 24 px atlas in the large-text theme - a
    // character 16 wide, a line 32 high - and the 18 px atlas in the light
    // theme - 12 wide, 24 high.
    let theme = Theme::high_contrast();
    assert_eq!(theme.metrics().char_width(), 16);
    assert_eq!(theme.metrics().line_height(), 32);
    let light: Theme<Rgb888> = Theme::light();
    assert_eq!(
        (light.metrics().char_width(), light.metrics().line_height()),
        (12, 24)
    );
    let mut screen = welcome(&theme, LARGE_WIDTH, LARGE_HEIGHT);
    screen.tree.set_focus(Some(screen.name)).unwrap();
    screen.tree.set_focus_visible(true);
    let fb = full_frame(&screen.tree, &theme);
    assert!(fb.count(Rgb888::BLACK) > 0);
    assert!(fb.count(theme.focus) > 0);
    // Taller text makes taller controls.
    let light = welcome(&light, LARGE_WIDTH, LARGE_HEIGHT);
    let tall = screen.tree.get(screen.name).unwrap().rect().size.height;
    let short = light.tree.get(light.name).unwrap().rect().size.height;
    assert!(tall > short);
}

const NAME_SLOT: SlotId = SlotId(1);
const REFERENCE_SLOT: SlotId = SlotId(2);
const SUBMIT: ActionId = ActionId(3);

/// A synthetic review screen: a title, two rows of a label and a slot the
/// application fills while the screen shows, and a Submit button.
fn review(theme: &Theme<Rgb888>) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let screen = tree.column(Style::new().padding(8).gap(6)).unwrap();
    tree.set_root(screen).unwrap();
    let title = tree.text("Check your answers").unwrap();
    tree.append(screen, title).unwrap();
    for (label, slot) in [("Name:", NAME_SLOT), ("Ref.:", REFERENCE_SLOT)] {
        let row = tree.row(Style::new().gap(6)).unwrap();
        tree.append(screen, row).unwrap();
        let label = tree.text(label).unwrap();
        tree.append(row, label).unwrap();
        let shown = tree.slot(slot).unwrap();
        tree.append(row, shown).unwrap();
    }
    let push_down = tree.spacer().unwrap();
    tree.append(screen, push_down).unwrap();
    let submit = tree.button("Submit", SUBMIT).unwrap();
    tree.append(screen, submit).unwrap();
    tree.layout(Rect::new(0, 0, WIDTH, HEIGHT), &theme.metrics())
        .unwrap();
    tree
}

/// Application text set in slots is drawn where the slots are.
#[test]
fn slot_text_matches_golden_image() {
    let theme = Theme::light();
    let mut tree = review(&theme);
    tree.set_slot(NAME_SLOT, "Sam Sample").unwrap();
    tree.set_slot(REFERENCE_SLOT, "TS-0042-K").unwrap();
    check_snapshot(&full_frame(&tree, &theme), &golden("review-slots.png")).unwrap();
}

/// Setting a slot's text repaints only within the slot - only the part
/// that changes - and that repaint gives exactly the pixels of a
/// full one, for longer, shorter, over-long and empty text.
#[test]
fn slot_text_repaints_only_its_area() {
    let theme = Theme::light();
    let mut tree = review(&theme);
    let mut incremental = full_frame(&tree, &theme);
    tree.clear_dirty();
    let slot = tree.find_slot(NAME_SLOT).unwrap();
    let slot_rect = tree.get(slot).unwrap().rect();
    for text in [
        "Sam Sample",
        "Sam",
        "A name far too long to fit in the slot it is shown in",
        "",
    ] {
        tree.set_slot(NAME_SLOT, text).unwrap();
        let dirty = tree.dirty_rects();
        assert!(
            !dirty.is_empty() && dirty.iter().all(|rect| slot_rect.contains_rect(rect)),
            "{text:?}: {dirty:?} outside {slot_rect:?}"
        );
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
        }
        .unwrap();
        tree.clear_dirty();
        assert_eq!(
            incremental.diff(&full_frame(&tree, &theme)),
            None,
            "{text:?}"
        );
    }
    tree.set_slot(NAME_SLOT, "Sam").unwrap();
    tree.clear_slots();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
    }
    .unwrap();
    assert_eq!(
        incremental.diff(&full_frame(&tree, &theme)),
        None,
        "cleared"
    );
}

/// A live figure - pounds counting up, as a pump's do - repaints
/// only the digits that change, and each repaint gives exactly the pixels
/// of a full one, in each theme and at device scale 2: when only the last
/// digit changes, a strip two digits and a line high wide - the digit
/// before it, which kerning and overhang can reach, and half a line's
/// margin each side - not the slot's whole line.
#[test]
fn a_live_figure_repaints_only_the_digits_that_change() {
    for (theme, scale) in [
        (Theme::light(), 1),
        (Theme::light(), 2),
        (Theme::high_contrast(), 1),
    ] {
        let mut tree = review(&theme);
        let metrics = theme.metrics_at(nowayos_display::DeviceScale::whole(scale).unwrap());
        tree.layout(Rect::new(0, 0, WIDTH * scale, HEIGHT * scale), &metrics)
            .unwrap();
        let slot = tree.find_slot(NAME_SLOT).unwrap();
        let slot_rect = tree.get(slot).unwrap().rect();
        let body = metrics.for_style(TextStyle::BODY);
        let strip = body.text_width("00") + 2 * (body.line_height() / 2);
        tree.set_slot(NAME_SLOT, "£0.00").unwrap();
        let mut incremental = full_frame(&tree, &theme);
        tree.clear_dirty();
        for pence in 1..=150_u32 {
            let text = format!("£{}.{:02}", pence / 100, pence % 100);
            tree.set_slot(NAME_SLOT, &text).unwrap();
            for rect in tree.dirty_rects() {
                assert!(slot_rect.contains_rect(rect), "{text}: {rect:?}");
                if pence % 10 != 0 {
                    assert!(rect.size.width <= strip, "{text}: {rect:?} at {scale}x");
                }
            }
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
            }
            .unwrap();
            tree.clear_dirty();
            assert_eq!(
                incremental.diff(&full_frame(&tree, &theme)),
                None,
                "{text} at {scale}x"
            );
        }
    }
}

const FORM: ScreenId = ScreenId(1);
const CHECK: ScreenId = ScreenId(2);

/// The two screens of a synthetic flow, for the navigator: a form asking
/// for a name and offering a copy, and a review showing the name back.
fn flow_screen(screen: ScreenId, tree: &mut Tree<16>) -> Result<NodeId, UiError> {
    let column = tree.column(Style::new().padding(8).gap(6))?;
    let title = tree.text(if screen == FORM {
        "Welcome"
    } else {
        "Check your answers"
    })?;
    tree.append(column, title)?;
    if screen == FORM {
        let name = tree.text_input(NAME, "Your name")?;
        tree.append(column, name)?;
        let copy = tree.checkbox(COPY, "Send me a copy")?;
        tree.append(column, copy)?;
    } else {
        let shown = tree.slot(NAME_SLOT)?;
        tree.append(column, shown)?;
    }
    let push_down = tree.spacer()?;
    tree.append(column, push_down)?;
    let next = tree.button(if screen == FORM { "Continue" } else { "Submit" }, CONTINUE)?;
    tree.append(column, next)?;
    Ok(column)
}

/// A full repaint of `screen`, freshly built and shown by a navigator.
fn fresh_frame(screen: ScreenId, theme: &Theme<Rgb888>, keyed: bool) -> Framebuffer {
    let mut nav: Navigator<2> =
        Navigator::new(screen, Rect::new(0, 0, WIDTH, HEIGHT), theme.metrics());
    let mut tree: Tree<16> = Tree::new();
    nav.show_start(&mut tree, flow_screen).unwrap();
    // After keys, the focus ring shows.
    tree.set_focus_visible(keyed);
    full_frame(&tree, theme)
}

/// Switching screens repaints the whole viewport: drawing only the dirty
/// areas over the old screen's frame gives exactly the new screen, with no
/// pixel of the old one left.
#[test]
fn switching_screens_leaves_no_pixel_of_the_old_one() {
    let theme = Theme::light();
    let mut nav: Navigator<2> =
        Navigator::new(FORM, Rect::new(0, 0, WIDTH, HEIGHT), theme.metrics());
    let mut tree: Tree<16> = Tree::new();
    nav.show_start(&mut tree, flow_screen).unwrap();
    type_text(&mut tree, "Sam");
    let mut screen = full_frame(&tree, &theme);
    tree.clear_dirty();

    nav.go(CHECK, &mut tree, flow_screen).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
    assert_eq!(
        screen.diff(&fresh_frame(CHECK, &theme, true)),
        None,
        "to the review"
    );

    tree.set_slot(NAME_SLOT, "Sam").unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
    assert!(nav.back(&mut tree, flow_screen).unwrap());
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
    }
    .unwrap();
    assert_eq!(
        screen.diff(&fresh_frame(FORM, &theme, true)),
        None,
        "back to the form"
    );
    assert_eq!(
        tree.value(NAME),
        Some(""),
        "what was typed is not shown again"
    );
}

const CODE: FieldId = FieldId(2);

/// A synthetic step with two fields, each followed by its error line.
fn details(theme: &Theme<Rgb888>, width: u32, height: u32) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let screen = tree.column(Style::new().padding(8).gap(4)).unwrap();
    tree.set_root(screen).unwrap();
    let title = tree.text("Your details").unwrap();
    tree.append(screen, title).unwrap();
    for (field, placeholder) in [(NAME, "Your name"), (CODE, "Code")] {
        let input = tree.text_input(field, placeholder).unwrap();
        tree.append(screen, input).unwrap();
        let error = tree.field_error(field).unwrap();
        tree.append(screen, error).unwrap();
    }
    tree.layout(Rect::new(0, 0, width, height), &theme.metrics())
        .unwrap();
    tree
}

/// A field's error shows in words beside it, marked by a shape as well as
/// the error colour, in both themes.
#[test]
fn field_errors_match_golden_images_in_every_theme() {
    for (theme, width, height, image) in [
        (Theme::light(), WIDTH, HEIGHT, "field-error.png"),
        (
            Theme::high_contrast(),
            LARGE_WIDTH,
            LARGE_HEIGHT,
            "field-error-high-contrast.png",
        ),
    ] {
        let mut tree = details(&theme, width, height);
        tree.set_field_error(CODE, "Use digits only").unwrap();
        let frame = full_frame(&tree, &theme);
        check_snapshot(&frame, &golden(image)).unwrap();
        // The marker: error-coloured, with the background showing through
        // its `!` - wholly where the glyph covers a pixel fully - so the
        // error is not told by colour alone.
        let line = tree
            .get(tree.paint_order().map(|(id, _)| id).last().unwrap())
            .unwrap()
            .rect();
        let side = theme.metrics().line_height();
        let mut marker_error = 0;
        let mut marker_background = 0;
        for y in line.top()..line.top() + i64::from(side) {
            for x in line.left()..line.left() + i64::from(side) {
                let pixel = frame.pixel(x as u32, y as u32).unwrap();
                marker_error += u32::from(pixel == theme.error);
                marker_background += u32::from(pixel == theme.background);
            }
        }
        assert!(marker_error > marker_background, "{image}: a filled marker");
        assert!(marker_background > 0, "{image}: with a mark cut out of it");
    }
}

/// Clearing an error removes it, repainting only its line, with exactly the
/// pixels of a full repaint.
#[test]
fn clearing_a_field_error_repaints_only_its_line() {
    let theme = Theme::light();
    let mut tree = details(&theme, WIDTH, HEIGHT);
    let mut incremental = full_frame(&tree, &theme);
    let before = incremental.clone();
    tree.clear_dirty();
    tree.set_field_error(NAME, "Enter your name").unwrap();
    tree.set_field_error(CODE, "Use digits only").unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
    assert_eq!(incremental.diff(&full_frame(&tree, &theme)), None);
    tree.clear_field_error(CODE).unwrap();
    let line = tree
        .get(tree.paint_order().map(|(id, _)| id).last().unwrap())
        .unwrap()
        .rect();
    assert_eq!(tree.dirty_rects(), &[line], "one line, not the screen");
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
    }
    .unwrap();
    tree.clear_dirty();
    assert_eq!(incremental.diff(&full_frame(&tree, &theme)), None);
    tree.clear_field_error(NAME).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
    }
    .unwrap();
    assert_eq!(incremental.diff(&before), None, "gone without a trace");
}

const COPY: FieldId = FieldId(3);
const CALL: FieldId = FieldId(4);
const TIME: FieldId = FieldId(5);
const TIMES: &[&str] = &["Morning", "Afternoon", "Evening"];

/// A synthetic step of choices: two checkboxes and a radio group.
fn choices(theme: &Theme<Rgb888>, width: u32, height: u32) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let screen = tree.column(Style::new().padding(6).gap(4)).unwrap();
    tree.set_root(screen).unwrap();
    let title = tree.text("How should we reply?").unwrap();
    tree.append(screen, title).unwrap();
    let copy = tree.checkbox(COPY, "Send me a copy").unwrap();
    tree.append(screen, copy).unwrap();
    let call = tree.checkbox(CALL, "Call me back").unwrap();
    tree.append(screen, call).unwrap();
    let time = tree.radio_group(TIME, TIMES).unwrap();
    tree.append(screen, time).unwrap();
    tree.layout(Rect::new(0, 0, width, height), &theme.metrics())
        .unwrap();
    tree
}

/// The themes and sizes the choice controls are drawn in.
fn choice_themes() -> [(Theme<Rgb888>, u32, u32, &'static str); 2] {
    [
        (Theme::light(), WIDTH, HEIGHT, "choices.png"),
        (
            Theme::high_contrast(),
            LARGE_WIDTH,
            LARGE_HEIGHT,
            "choices-high-contrast.png",
        ),
    ]
}

/// A ticked and an unticked checkbox and a radio group with one option
/// chosen and focus on it, in both themes.
#[test]
fn choice_controls_match_golden_images_in_every_theme() {
    for (theme, width, height, image) in choice_themes() {
        let mut tree = choices(&theme, width, height);
        tree.set_checked(COPY, true).unwrap();
        tree.set_selected(TIME, Some(1)).unwrap();
        let group = tree.paint_order().map(|(id, _)| id).last().unwrap();
        tree.set_focus(Some(group)).unwrap();
        tree.set_focus_visible(true);
        check_snapshot(&full_frame(&tree, &theme), &golden(image)).unwrap();
    }
}

/// The colours inside `area` of `frame`, and how many pixels of each.
fn colours(frame: &Framebuffer, area: Rect) -> Vec<(Rgb888, u32)> {
    let mut found: Vec<(Rgb888, u32)> = Vec::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let pixel = frame.pixel(x as u32, y as u32).unwrap();
            match found.iter_mut().find(|(colour, _)| *colour == pixel) {
                Some((_, count)) => *count += 1,
                None => found.push((pixel, 1)),
            }
        }
    }
    found.sort_by_key(|(colour, _)| (colour.r(), colour.g(), colour.b()));
    found
}

/// Ticked and unticked, chosen and not, are told apart by a mark - more
/// ink - never by a colour of their own: every colour either way is the
/// ink, the ground or a blend of the two along an anti-aliased edge.
#[test]
fn choice_state_is_shown_by_a_mark_not_colour_alone() {
    for (theme, width, height, _) in choice_themes() {
        let mut tree = choices(&theme, width, height);
        tree.set_checked(COPY, true).unwrap();
        tree.set_selected(TIME, Some(1)).unwrap();
        let frame = full_frame(&tree, &theme);
        let side = theme.metrics().line_height();
        let mark_in = |area: Rect| {
            let top = area.top() + i64::from(area.size.height.saturating_sub(side) / 2);
            Rect::new(area.origin.x, top as i32, side, side)
        };
        let ids: Vec<_> = tree.paint_order().map(|(id, _)| id).collect();
        let (copy, call, time) = (ids[2], ids[3], ids[4]);
        let content = |id| {
            tree.get(id)
                .unwrap()
                .rect()
                .inset(nowayos_ui::CHOICE_PADDING)
        };
        let ticked = colours(&frame, mark_in(content(copy)));
        let unticked = colours(&frame, mark_in(content(call)));
        let group = tree.get(time).unwrap();
        let chosen = colours(&frame, mark_in(group.option_rect(1).unwrap()));
        let not_chosen = colours(&frame, mark_in(group.option_rect(0).unwrap()));
        for (on, off) in [(&ticked, &unticked), (&chosen, &not_chosen)] {
            let palette = |found: &Vec<(Rgb888, u32)>| -> Vec<Rgb888> {
                found.iter().map(|(colour, _)| *colour).collect()
            };
            // On the line from the ground to the ink, to within rounding.
            let ground = theme.background;
            let blend_of_ink = |colour: &Rgb888| {
                let share = |c: u8, g: u8, t: u8| {
                    (f64::from(c) - f64::from(g)) / (f64::from(t) - f64::from(g))
                };
                let shares = [
                    share(colour.r(), ground.r(), theme.text.r()),
                    share(colour.g(), ground.g(), theme.text.g()),
                    share(colour.b(), ground.b(), theme.text.b()),
                ];
                let (low, high) = shares
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(l, h), &s| (l.min(s), h.max(s)));
                high - low < 0.06 && low > -0.03 && high < 1.03
            };
            for colour in palette(on).iter().chain(&palette(off)) {
                assert!(blend_of_ink(colour), "{colour:?} is a colour of its own");
            }
            let ink = |found: &Vec<(Rgb888, u32)>| {
                found
                    .iter()
                    .find(|(colour, _)| *colour == theme.text)
                    .map_or(0, |(_, count)| *count)
            };
            assert!(ink(on) > ink(off), "a mark: more ink when on");
        }
    }
}

/// The marks of the choice controls keep clear of the focus ring and of
/// each other, in every theme: just inside a focused checkbox's or
/// radio group's ring there is only background - what the ring's contrast
/// is checked against - and background separates each radio option's ring
/// from the next one's. Marks a whole line high, as tall as the rows, met
/// both: consecutive rings merged, and in the large-text theme the yellow
/// focus ring touched the white rings, at 1.4:1.
#[test]
fn choice_marks_keep_clear_of_the_focus_ring_and_of_each_other() {
    for (theme, width, height, image) in choice_themes() {
        let mut tree = choices(&theme, width, height);
        tree.set_checked(COPY, true).unwrap();
        tree.set_selected(TIME, Some(1)).unwrap();
        let ids: Vec<_> = tree.paint_order().map(|(id, _)| id).collect();
        let (copy, time) = (ids[2], ids[4]);
        for focused in [copy, time] {
            tree.set_focus(Some(focused)).unwrap();
            let frame = full_frame(&tree, &theme);
            // The band one pixel wide just inside the ring.
            let band = tree
                .get(focused)
                .unwrap()
                .rect()
                .inset(Insets::uniform(theme.focus_width));
            for y in band.top()..band.bottom() {
                for x in band.left()..band.right() {
                    let edge = x == band.left()
                        || x == band.right() - 1
                        || y == band.top()
                        || y == band.bottom() - 1;
                    if edge {
                        assert_eq!(
                            frame.pixel(x as u32, y as u32),
                            Some(theme.background),
                            "{image}: ({x}, {y}), just inside the focus ring"
                        );
                    }
                }
            }
        }
        tree.set_focus(None).unwrap();
        let frame = full_frame(&tree, &theme);
        let group = tree.get(time).unwrap();
        // The rows of an option's mark - in the square one line high that
        // leads the row - that hold anything but background.
        let side = i64::from(theme.metrics().line_height());
        let inked_rows = |row: Rect| -> Vec<i64> {
            (row.top()..row.bottom())
                .filter(|&y| {
                    (row.left()..row.left() + side)
                        .any(|x| frame.pixel(x as u32, y as u32) != Some(theme.background))
                })
                .collect()
        };
        for index in 1..TIMES.len() {
            let above = inked_rows(group.option_rect(index - 1).unwrap());
            let below = inked_rows(group.option_rect(index).unwrap());
            let (last, first) = (*above.last().unwrap(), *below.first().unwrap());
            assert!(
                first - last > 1,
                "{image}: option {index}'s ring touches the one above it (rows {last} and {first})"
            );
        }
    }
}

/// Toggling and choosing repaint only the control, with exactly the pixels
/// of a full repaint.
#[test]
fn choices_repaint_only_the_control() {
    let theme = Theme::light();
    let mut tree = choices(&theme, WIDTH, HEIGHT);
    let mut incremental = full_frame(&tree, &theme);
    tree.clear_dirty();
    let ids: Vec<_> = tree.paint_order().map(|(id, _)| id).collect();
    let (copy, time) = (ids[2], ids[4]);
    let copy_centre = tree.get(copy).unwrap().rect().center();
    let steps: Vec<Step> = vec![
        Box::new(move |t| {
            t.handle(InputEvent::PointerMove {
                x: copy_centre.x,
                y: copy_centre.y,
            });
            t.handle(InputEvent::PointerButton {
                button: 0,
                down: true,
            });
            t.handle(InputEvent::PointerButton {
                button: 0,
                down: false,
            });
        }),
        Box::new(move |t| {
            t.set_focus(Some(time)).unwrap();
        }),
        Box::new(|t| {
            key(t, KeyCode::DOWN);
        }),
        Box::new(|t| {
            key(t, KeyCode::UP);
        }),
        Box::new(|t| t.clear_values()),
    ];
    let controls = [
        tree.get(copy).unwrap().rect(),
        tree.get(time).unwrap().rect(),
    ];
    for (index, step) in steps.iter().enumerate() {
        step(&mut tree);
        assert!(
            !tree.dirty_rects().is_empty(),
            "step {index} repainted nothing"
        );
        for dirty in tree.dirty_rects() {
            assert!(controls.contains(dirty), "step {index} repainted {dirty:?}");
        }
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut incremental, &theme, &mut glyphs__)
        }
        .unwrap();
        tree.clear_dirty();
        assert_eq!(
            incremental.diff(&full_frame(&tree, &theme)),
            None,
            "after step {index}"
        );
    }
}

/// Ending a session: everything typed, ticked and shown is wiped in place,
/// and the next frame - drawn over the session's last one, repainting only
/// what is dirty - is the start screen, freshly built, matching its golden
/// image, with no pixel of the session left.
#[test]
fn a_reset_draws_the_start_screen_with_no_pixel_of_the_session() {
    let theme = Theme::light();
    let viewport = Rect::new(0, 0, WIDTH, HEIGHT);
    for reset_on in [FORM, CHECK] {
        let mut nav: Navigator<2> = Navigator::new(FORM, viewport, theme.metrics());
        let mut tree: Tree<16> = Tree::new();
        nav.show_start(&mut tree, flow_screen).unwrap();
        let mut screen = full_frame(&tree, &theme);
        tree.clear_dirty();
        type_text(&mut tree, "Sam");
        tree.set_checked(COPY, true).unwrap();
        if reset_on == CHECK {
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
            }
            .unwrap();
            tree.clear_dirty();
            nav.go(CHECK, &mut tree, flow_screen).unwrap();
            tree.set_slot(NAME_SLOT, "Sam").unwrap();
        }
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
        }
        .unwrap();
        tree.clear_dirty();
        assert!(!tree.is_wiped());

        nav.reset(&mut tree, flow_screen).unwrap();
        assert!(tree.is_wiped(), "every value's storage zeroed");
        assert_eq!(tree.dirty_rects(), &[viewport], "a full repaint");
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut screen, &theme, &mut glyphs__)
        }
        .unwrap();
        check_snapshot(&screen, &golden("reset-start.png")).unwrap();
        assert_eq!(
            screen.diff(&fresh_frame(FORM, &theme, false)),
            None,
            "reset on {reset_on:?}"
        );
    }
}

/// A minimal RGB565 draw target, to show the renderer is not tied to one
/// colour type.
struct Rgb565Buffer {
    size: Size,
    pixels: Vec<Rgb565>,
}

impl Rgb565Buffer {
    fn new(width: u32, height: u32) -> Self {
        Self {
            size: Size::new(width, height),
            pixels: vec![Rgb565::BLACK; (width * height) as usize],
        }
    }

    /// The frame as `Rgb888`, each channel widened by `embedded-graphics`'
    /// conversion, to compare with a golden image.
    fn widened(&self) -> Framebuffer {
        Framebuffer::from_pixels(
            self.size.width,
            self.size.height,
            self.pixels
                .iter()
                .map(|&pixel| Rgb888::from(pixel))
                .collect(),
        )
        .unwrap()
    }
}

impl OriginDimensions for Rgb565Buffer {
    fn size(&self) -> Size {
        self.size
    }
}

impl DrawTarget for Rgb565Buffer {
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

/// The golden image `name`, read back.
fn read_golden(name: &str) -> Framebuffer {
    Framebuffer::from_png(&std::fs::read(golden(name)).unwrap()).unwrap()
}

#[test]
fn raw_framebuffers_hold_the_golden_pixels_in_their_format() {
    let theme = Theme::light();
    let mut typing = welcome(&theme, WIDTH, HEIGHT);
    typing.tree.set_focus(Some(typing.name)).unwrap();
    type_text(&mut typing.tree, "Ada");
    let plain = welcome(&theme, WIDTH, HEIGHT);
    for (tree, image) in [
        (&plain.tree, "welcome.png"),
        (&typing.tree, "welcome-typing.png"),
    ] {
        let expected = read_golden(image);
        for format in [PixelFormat::Xrgb8888, PixelFormat::Rgb565] {
            let row = WIDTH * u32::from(format.bytes_per_pixel());
            for stride in [row, row + 12] {
                let layout = FramebufferLayout::new(WIDTH, HEIGHT, stride, format).unwrap();
                let mut memory = vec![0xA5_u8; layout.byte_len() + 16];
                {
                    nowayos_testkit::glyph_store!(glyphs__);
                    render(
                        tree,
                        &mut RawFramebuffer::new(layout, &mut memory[..]).unwrap(),
                        &theme,
                        &mut glyphs__,
                    )
                }
                .unwrap();
                let bpp = usize::from(format.bytes_per_pixel());
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        let pixel = expected.pixel(x, y).unwrap();
                        let mut want = [0; 4];
                        format
                            .encode(pixel.r(), pixel.g(), pixel.b(), &mut want)
                            .unwrap();
                        let at = layout.pixel_offset(x, y).unwrap();
                        assert_eq!(
                            &memory[at..at + bpp],
                            &want[..bpp],
                            "{image} {format:?} stride {stride} ({x}, {y})"
                        );
                    }
                    let padding = (y * stride + row) as usize..((y + 1) * stride) as usize;
                    assert!(
                        memory[padding].iter().all(|&b| b == 0xA5),
                        "row padding untouched"
                    );
                }
                assert!(
                    memory[layout.byte_len()..].iter().all(|&b| b == 0xA5),
                    "tail untouched"
                );
            }
        }
    }
}

#[test]
fn a_framebuffer_shorter_than_its_layout_is_refused() {
    let layout = FramebufferLayout::packed(WIDTH, HEIGHT, PixelFormat::Xrgb8888).unwrap();
    let mut memory = vec![0x5A_u8; layout.byte_len() - 1];
    assert_eq!(
        RawFramebuffer::new(layout, &mut memory[..]).unwrap_err(),
        RawFramebufferError::TooShort {
            len: layout.byte_len() - 1,
            needed: layout.byte_len()
        }
    );
    assert!(memory.iter().all(|&b| b == 0x5A), "nothing written");
    assert_eq!(
        RawFramebufferError::TooShort { len: 1, needed: 2 }.to_string(),
        "the framebuffer memory is 1 bytes; the layout needs 2"
    );
}

#[test]
fn renders_to_other_colour_types() {
    let theme: Theme<Rgb565> = Theme::light();
    let screen = welcome(&Theme::light(), WIDTH, HEIGHT);
    let mut target = Rgb565Buffer::new(WIDTH, HEIGHT);
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(&screen.tree, &mut target, &theme, &mut glyphs__)
    }
    .unwrap();
    assert!(target.pixels.contains(&theme.button));
    assert!(target.pixels.contains(&theme.background));
}

/// Text blended in RGB565's own depths - 5, 6 and 5 bits - matches its
/// golden images in both themes, and differs from the RGB888 frame
/// only within what 5 and 6 bits can hold.
#[test]
fn rgb565_frames_match_golden_images_in_every_theme() {
    for (theme, width, height, suffix) in themes() {
        let screen = welcome_typing(&theme, width, height);
        let narrow: Theme<Rgb565> = if suffix.is_empty() {
            Theme::light()
        } else {
            Theme::high_contrast()
        };
        let mut target = Rgb565Buffer::new(width, height);
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render(&screen.tree, &mut target, &narrow, &mut glyphs__)
        }
        .unwrap();
        let frame = target.widened();
        check_snapshot(
            &frame,
            &golden(&format!("welcome-typing{suffix}-rgb565.png")),
        )
        .unwrap();
        let wide = full_frame(&screen.tree, &theme);
        for (narrow, wide) in frame.pixels().iter().zip(wide.pixels()) {
            for (a, b, step) in [
                (narrow.r(), wide.r(), 9),
                (narrow.g(), wide.g(), 5),
                (narrow.b(), wide.b(), 9),
            ] {
                assert!(a.abs_diff(b) <= step, "{narrow:?} against {wide:?}");
            }
        }
    }
}

/// Every text role of the type scale, in each weight it has: a
/// display number, a title, a heading, body, label and caption text in
/// Regular and Bold.
fn type_scale(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> Tree<16> {
    let mut tree: Tree<16> = Tree::new();
    let root = tree
        .column(Style::new().padding(16).gap(8).align(Align::Start))
        .unwrap();
    tree.set_root(root).unwrap();
    let lines: [(&'static str, TextStyle); 9] = [
        ("A047", TextStyle::new(TextRole::Display)),
        ("About you", TextStyle::new(TextRole::Title)),
        ("Service desk", TextStyle::new(TextRole::Heading)),
        ("We use this only to reply.", TextStyle::BODY),
        ("Your name, in Bold", TextStyle::BODY.bold()),
        ("Reference (optional)", TextStyle::new(TextRole::Label)),
        ("Reference, in Bold", TextStyle::new(TextRole::Label).bold()),
        (
            "Nothing you type is kept",
            TextStyle::new(TextRole::Caption),
        ),
        ("Ready", TextStyle::new(TextRole::Caption).bold()),
    ];
    for (text, style) in lines {
        tree.add_child(root, NodeKind::Text(text), Style::new().text(style))
            .unwrap();
    }
    tree.layout(
        Rect::new(0, 0, width * scale, height * scale),
        &theme.metrics_at(nowayos_display::DeviceScale::whole(scale).unwrap()),
    )
    .unwrap();
    tree
}

/// The type scale drawn in each theme, and at device scale 2, matches its
/// approved images: each role at its size and weight, the caption muted;
/// and a dirty repaint of any line gives a full repaint's pixels.
#[test]
fn every_text_role_matches_its_approved_images() {
    for (theme, width, height, scale, name) in [
        (Theme::light(), 480, 400, 1, "type-scale"),
        (Theme::light(), 480, 400, 2, "type-scale-2x"),
        (
            Theme::high_contrast(),
            720,
            560,
            1,
            "type-scale-high-contrast",
        ),
    ] {
        let tree = type_scale(&theme, width, height, scale);
        let full = full_frame(&tree, &theme);
        check_snapshot(&full, &golden(&format!("{name}.png"))).unwrap();
        // The caption is muted, the body is ink.
        let caption = tree.paint_order().nth(8).unwrap().1.rect();
        assert!(
            colours(&full, caption)
                .iter()
                .any(|&(c, _)| c == theme.text_muted),
            "{name}: the caption in its muted colour"
        );
        // Each line repainted alone gives the full frame's pixels there.
        for (_, node) in tree.paint_order().skip(1) {
            let mut part = Framebuffer::new(width * scale, height * scale).unwrap();
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_area(&tree, &mut part, &theme, node.rect(), &mut glyphs__)
            }
            .unwrap();
            let area = node.rect();
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    let (x, y) = (x as u32, y as u32);
                    assert_eq!(part.pixel(x, y), full.pixel(x, y), "{name} ({x}, {y})");
                }
            }
        }
    }
}

/// The look's surfaces and buttons: a header bar with a secondary
/// button on it, a card holding a title, body text, a caption and the three
/// button variants, and a notice in each tone - laid out in `theme` in a
/// `width` x `height` unit viewport at device scale `scale`. Returns the
/// tree and the card's buttons: quiet, secondary, primary.
fn look(theme: &Theme<Rgb888>, width: u32, height: u32, scale: u32) -> (Tree<32>, [NodeId; 4]) {
    use nowayos_ui::{ButtonVariant, Surface, Tone};
    let mut tree: Tree<32> = Tree::new();
    let root = tree.column(Style::new().padding(16).gap(16)).unwrap();
    tree.set_root(root).unwrap();
    let bar = tree
        .add_child(
            root,
            NodeKind::Row,
            Style::new()
                .padding_xy(16, 8)
                .gap(16)
                .align(Align::Center)
                .surface(Surface::Header),
        )
        .unwrap();
    tree.add_child(
        bar,
        NodeKind::Text("Service desk"),
        Style::new().text(TextStyle::new(TextRole::Heading)),
    )
    .unwrap();
    tree.add_child(bar, NodeKind::Spacer, Style::new().width(Length::Fill))
        .unwrap();
    let home = tree
        .button_with("Home", HELP, ButtonVariant::Secondary)
        .unwrap();
    tree.append(bar, home).unwrap();
    let card = tree
        .add_child(
            root,
            NodeKind::Column,
            Style::new().padding(24).gap(12).surface(Surface::Card),
        )
        .unwrap();
    for (text, style) in [
        ("About you", TextStyle::new(TextRole::Title)),
        ("We use this only to reply.", TextStyle::BODY),
        (
            "Nothing you type is kept",
            TextStyle::new(TextRole::Caption),
        ),
    ] {
        tree.add_child(card, NodeKind::Text(text), Style::new().text(style))
            .unwrap();
    }
    let buttons = tree
        .add_child(
            card,
            NodeKind::Row,
            Style::new().gap(16).align(Align::Center),
        )
        .unwrap();
    let quiet = tree
        .button_with("Cancel", HELP, ButtonVariant::Quiet)
        .unwrap();
    tree.append(buttons, quiet).unwrap();
    tree.add_child(buttons, NodeKind::Spacer, Style::new().width(Length::Fill))
        .unwrap();
    let secondary = tree
        .button_with("Back", HELP, ButtonVariant::Secondary)
        .unwrap();
    tree.append(buttons, secondary).unwrap();
    let primary = tree.button("Continue", CONTINUE).unwrap();
    tree.append(buttons, primary).unwrap();
    for (tone, text) in [
        (Tone::Success, "Your visit is booked."),
        (Tone::Caution, "Please collect your card."),
        (Tone::Warning, "This kiosk is being serviced."),
    ] {
        let notice = tree
            .add_child(
                root,
                NodeKind::Column,
                Style::new()
                    .padding_xy(16, 12)
                    .surface(Surface::Notice(tone)),
            )
            .unwrap();
        let weight = if tone == Tone::Warning {
            TextStyle::BODY.bold()
        } else {
            TextStyle::BODY
        };
        tree.add_child(notice, NodeKind::Text(text), Style::new().text(weight))
            .unwrap();
    }
    tree.layout(
        Rect::new(0, 0, width * scale, height * scale),
        &theme.metrics_at(nowayos_display::DeviceScale::whole(scale).unwrap()),
    )
    .unwrap();
    (tree, [quiet, secondary, primary, home])
}

fn frame_of<const N: usize>(tree: &Tree<N>, theme: &Theme<Rgb888>) -> Framebuffer {
    let size = tree.viewport().size;
    let mut fb = Framebuffer::new(size.width, size.height).unwrap();
    {
        nowayos_testkit::glyph_store!(glyphs__);
        render(tree, &mut fb, theme, &mut glyphs__)
    }
    .unwrap();
    fb
}

/// Presses `id` with the pointer and holds it down.
fn hold<const N: usize>(tree: &mut Tree<N>, id: NodeId) {
    let centre = tree.get(id).unwrap().rect().center();
    tree.handle(InputEvent::PointerMove {
        x: centre.x,
        y: centre.y,
    });
    tree.handle(InputEvent::PointerButton {
        button: 0,
        down: true,
    });
}

/// The surfaces and the three button variants match their approved
/// images in both themes and at device scale 2 - with the primary button
/// focused and the secondary one held down - every button at least 48
/// units high; text on each surface is blended over the surface's own
/// fill, never the background; and a dirty repaint of any node gives
/// exactly a full repaint's pixels.
#[test]
fn surfaces_and_button_variants_match_their_approved_images() {
    for (theme, width, height, scale, name) in [
        (Theme::light(), 640, 560, 1, "look"),
        (Theme::light(), 640, 560, 2, "look-2x"),
        (Theme::high_contrast(), 800, 720, 1, "look-high-contrast"),
    ] {
        let (mut tree, [quiet, secondary, primary, home]) = look(&theme, width, height, scale);
        for button in [quiet, secondary, primary, home] {
            let rect = tree.get(button).unwrap().rect();
            assert!(rect.size.height >= 48 * scale, "{name}: {rect:?}");
        }
        hold(&mut tree, secondary);
        // Focus moved by keys while the press is held: the ring shows.
        tree.set_focus(Some(primary)).unwrap();
        tree.set_focus_visible(true);
        assert_eq!(tree.pressed(), Some(secondary), "{name}");
        let full = frame_of(&tree, &theme);
        check_snapshot(&full, &golden(&format!("{name}.png"))).unwrap();
        // Text on a surface never shows the background: its edges are
        // blended over the surface's fill.
        for (id, node) in tree.paint_order() {
            if !matches!(node.kind(), NodeKind::Text(_)) {
                continue;
            }
            let on = tree.get(node.parent().unwrap()).unwrap().style().surface;
            let Some(on) = on else { continue };
            let found = colours(&full, node.rect());
            assert!(
                found.iter().all(|&(c, _)| c != theme.background)
                    || theme.surfaces.of(on).fill == theme.background,
                "{name}: {id:?} shows the background"
            );
            assert!(
                found.iter().any(|&(c, _)| c == theme.surfaces.of(on).fill),
                "{name}: {id:?} on its surface's fill"
            );
        }
        // The pressed secondary button is filled with the brand colour;
        // the focused primary one has its ring.
        let pressed = colours(&full, tree.get(secondary).unwrap().rect());
        assert!(pressed.iter().any(|&(c, _)| c == theme.button), "{name}");
        let focused = colours(&full, tree.get(primary).unwrap().rect());
        assert!(focused.iter().any(|&(c, _)| c == theme.focus), "{name}");
        // Each node repainted alone gives the full frame's pixels there.
        for (_, node) in tree.paint_order() {
            let size = tree.viewport().size;
            let mut part = Framebuffer::new(size.width, size.height).unwrap();
            {
                nowayos_testkit::glyph_store!(glyphs__);
                render_area(&tree, &mut part, &theme, node.rect(), &mut glyphs__)
            }
            .unwrap();
            let area = node.rect();
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    let (x, y) = (x as u32, y as u32);
                    assert_eq!(part.pixel(x, y), full.pixel(x, y), "{name} ({x}, {y})");
                }
            }
        }
        // Releasing the button and moving focus repaints only what changed,
        // and the dirty repaint equals a full one.
        let mut dirty = full.clone();
        tree.clear_dirty();
        tree.handle(InputEvent::PointerButton {
            button: 0,
            down: false,
        });
        tree.set_focus(Some(quiet)).unwrap();
        {
            nowayos_testkit::glyph_store!(glyphs__);
            render_dirty(&tree, &mut dirty, &theme, &mut glyphs__)
        }
        .unwrap();
        assert_eq!(dirty, frame_of(&tree, &theme), "{name}");
    }
}
