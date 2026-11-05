// SPDX-License-Identifier: MIT OR Apache-2.0
//! Frame times (#10077): how long NoWayOS's renderer takes to draw two
//! screens - a launcher of cards, and a text-heavy article - at 1920 x
//! 1080, 2560 x 1440 and 3840 x 2160, in 16 and 32 bits a pixel: a full
//! redraw, a scroll step, a press and a slot change. The same code runs on
//! the host (`cargo run --release -p nowayos-frame-times --example host`)
//! and in the `frame-bench` protection domain, in QEMU and on the Pi, each
//! timing with its own clock.
//!
//! Each screen is laid out at the device scale that makes it 960 x 540
//! layout units, as the showcase is: 2 at 1080p, 2.5 at 1440p, 4 at 4K.
//! The first full redraw draws every glyph from its outline into the glyph
//! store, so it is timed apart from a second, which draws from the store.
//!
//! Everything here is synthetic and generic. Provisional.

#![no_std]

use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use nowayos_display::{DeviceScale, FrameMemory, PixelFormat, Rect};
use nowayos_input::InputEvent;
use nowayos_render_eg::{Glyphs, RawFramebuffer, Theme, render, render_dirty, to_eg};
use nowayos_ui::builder::{Layout, UiBuilder};
use nowayos_ui::{ActionId, SlotId, Surface, TextRole, TextStyle, Tree, UiError};

/// The display sizes measured: width, height, name and the device scale,
/// in quarters, that makes each 960 x 540 layout units.
pub const SIZES: [(u32, u32, &str, u32); 3] = [
    (1920, 1080, "1080p", 8),
    (2560, 1440, "1440p", 10),
    (3840, 2160, "4K", 16),
];

/// The pixel formats measured, and their names.
pub const FORMATS: [(PixelFormat, &str); 2] = [
    (PixelFormat::Rgb565, "16-bit"),
    (PixelFormat::Xrgb8888, "32-bit"),
];

/// The most bytes a frame takes: 3840 x 2160 at 4 bytes a pixel.
pub const MOST_FRAME: usize = 3840 * 2160 * 4;

/// Nodes a screen's tree holds.
pub const NODES: usize = 96;

/// Bytes of long text a screen's tree holds.
pub const TEXT: usize = 8192;

/// The tree a screen is built into.
pub type Screen = Tree<NODES, TEXT>;

/// Which screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// A launcher: a header bar over a grid of cards, each a title, a line
    /// and a button, that scrolls.
    Launcher,
    /// A text-heavy screen: a header bar over an article of several
    /// paragraphs that scrolls.
    Article,
}

impl Which {
    /// Both screens, in the order measured.
    pub const ALL: [Self; 2] = [Self::Launcher, Self::Article];

    /// Its name, as printed.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Article => "article",
        }
    }
}

/// The slot the header bar's clock shows: what a slot change sets.
const CLOCK: SlotId = SlotId(1);

/// The article's long text.
const BODY: SlotId = SlotId(2);

/// What every card's button reports.
const OPEN: ActionId = ActionId(1);

/// A synthetic article: paragraphs of plain words.
const ARTICLE: &str = "A station with nothing else to do draws its screen from the same tree every time it changes. Each paragraph here is set by the application at run time, wrapped at the width of the column, and scrolled through a view that moves what is already drawn and paints only the strip it exposes.\n\
Large displays carry four times the pixels of a small one, and every one of them is written when the whole screen is drawn. What a person notices is not the first frame but the frames after it: a list moving under a finger, a button answering a press, a clock ticking in the corner.\n\
So these measurements time each of those apart. A full redraw paints every pixel. A scroll step moves the pixels already drawn and paints a strip. A press repaints one button twice. A slot change repaints a line of text.\n\
On a board with four cores, drawing could be split into tiles, one a core. Whether that is worth its cost is what the numbers from the board decide, together with the time the display takes to copy a frame to the screen.\n\
Synthetic text, written for this bench: no product, place or person.";

/// Builds `which` into `tree` and lays it out in `viewport` at `scale` in
/// `theme`'s metrics, its clock and article set.
///
/// # Errors
///
/// The tree's error: for example when it is too small.
pub fn build<C: RgbColor + From<Rgb888>>(
    tree: &mut Screen,
    which: Which,
    theme: &Theme<C>,
    viewport: Rect,
    scale: DeviceScale,
) -> Result<(), UiError> {
    tree.clear();
    let t = &mut *tree;
    let title = UiBuilder::text_styled(t, "Frame bench", TextStyle::new(TextRole::Title))?;
    let clock = UiBuilder::slot(t, CLOCK)?;
    let help = UiBuilder::button(t, "Help", OPEN)?;
    let bar = UiBuilder::row_on(
        t,
        Surface::Header,
        Layout::new().gap(16).padding(16),
        &[title, clock, help],
    )?;
    let content = match which {
        Which::Launcher => {
            let mut rows = [bar; 4];
            for row in &mut rows {
                let mut cards = [bar; 3];
                for card in &mut cards {
                    let heading = UiBuilder::text_styled(
                        t,
                        "A tile of the launcher",
                        TextStyle::new(TextRole::Heading),
                    )?;
                    let line = UiBuilder::text(t, "What it opens, in a line")?;
                    let open = UiBuilder::button(t, "Open", OPEN)?;
                    *card = UiBuilder::column_on(
                        t,
                        Surface::Card,
                        Layout::new().gap(8).padding(16).width(280),
                        &[heading, line, open],
                    )?;
                }
                *row = UiBuilder::row(t, Layout::new().gap(16), &cards)?;
            }
            UiBuilder::column(t, Layout::new().gap(16).padding(24), &rows)?
        }
        Which::Article => {
            let heading =
                UiBuilder::text_styled(t, "Frames at any size", TextStyle::new(TextRole::Heading))?;
            let body = UiBuilder::long_text(t, BODY, 4096, TextStyle::default())?;
            UiBuilder::column(t, Layout::new().gap(16).padding(24), &[heading, body])?
        }
    };
    let view = UiBuilder::scroll_view(t, Layout::new(), content)?;
    let root = UiBuilder::column(t, Layout::new(), &[bar, view])?;
    t.set_root(root)?;
    t.layout(viewport, &theme.metrics_at(scale))?;
    t.set_slot(CLOCK, "12:34")?;
    if which == Which::Article {
        // Twice over, so it scrolls at every size.
        let mut both = [0_u8; 4096];
        let mut len = 0_usize;
        for _ in 0..2 {
            let end = len.saturating_add(ARTICLE.len());
            if let Some(into) = both.get_mut(len..end) {
                into.copy_from_slice(ARTICLE.as_bytes());
                len = end;
            }
        }
        let text = both
            .get(..len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or(ARTICLE);
        t.set_slot(BODY, text)?;
    }
    t.layout(viewport, &theme.metrics_at(scale))?;
    t.clear_dirty();
    Ok(())
}

/// What each step took, in the clock's ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Times {
    /// The first full redraw: every glyph drawn from its outline.
    pub first: u64,
    /// A second full redraw, its glyphs from the store.
    pub full: u64,
    /// A wheel's step down: the move and the strip it exposes.
    pub scroll: u64,
    /// A button pressed and released: its repaint each time.
    pub press: u64,
    /// The clock's slot set to another time: its repaint.
    pub slot: u64,
}

/// Times each step on `tree`, built ([`build`]), drawn into `target` in
/// `theme` with `glyphs`, `clock` giving the time in its own ticks.
///
/// # Errors
///
/// The tree's error, should a step fail.
pub fn time<M: FrameMemory, C>(
    tree: &mut Screen,
    target: &mut RawFramebuffer<M>,
    theme: &Theme<C>,
    glyphs: &mut Glyphs<'_>,
    clock: &mut impl FnMut() -> u64,
) -> Result<Times, UiError>
where
    C: RgbColor + From<Rgb888>,
    RawFramebuffer<M>: embedded_graphics::draw_target::DrawTarget<Color = C>,
{
    let mut times = Times::default();
    let timed = |clock: &mut dyn FnMut() -> u64, step: &mut dyn FnMut()| {
        let started = clock();
        step();
        clock().saturating_sub(started)
    };
    times.first = timed(clock, &mut || {
        let _ = render(tree, target, theme, glyphs);
    });
    times.full = timed(clock, &mut || {
        let _ = render(tree, target, theme, glyphs);
    });
    tree.clear_dirty();
    // The pointer over the scrolling view, then a wheel's step down.
    let view = tree
        .scroll_node()
        .and_then(|id| tree.get(id))
        .map(|node| node.rect().center());
    if let Some(at) = view {
        tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
    }
    tree.clear_dirty();
    times.scroll = timed(clock, &mut || {
        tree.handle(InputEvent::Wheel { notches: -1 });
        present(tree, target, theme, glyphs);
    });
    // The header bar's Help pressed and released.
    let button = tree
        .paint_order()
        .find(|(_, node)| node.is_focusable())
        .map(|(_, node)| node.rect().center());
    if let Some(at) = button {
        tree.handle(InputEvent::PointerMove { x: at.x, y: at.y });
        tree.clear_dirty();
    }
    times.press = timed(clock, &mut || {
        for down in [true, false] {
            tree.handle(InputEvent::PointerButton { button: 0, down });
            present(tree, target, theme, glyphs);
        }
    });
    times.slot = timed(clock, &mut || {
        let _ = tree.set_slot(CLOCK, "12:35");
        present(tree, target, theme, glyphs);
    });
    Ok(times)
}

/// What a UI loop does after an event: moves what scrolled, draws what is
/// marked.
fn present<M: FrameMemory, C>(
    tree: &mut Screen,
    target: &mut RawFramebuffer<M>,
    theme: &Theme<C>,
    glyphs: &mut Glyphs<'_>,
) where
    C: RgbColor + From<Rgb888>,
    RawFramebuffer<M>: embedded_graphics::draw_target::DrawTarget<Color = C>,
{
    let mut moved = true;
    for made in tree.moves() {
        moved &= target.move_area(&to_eg(made.area), made.dx, made.dy);
    }
    if !moved {
        tree.mark_area_dirty(tree.viewport());
    }
    let _ = render_dirty(tree, target, theme, glyphs);
    tree.clear_dirty();
}
