// SPDX-License-Identifier: MIT OR Apache-2.0
//! Draws a laid-out `nowayos-ui` [`Tree`] to any `embedded-graphics`
//! [`DrawTarget`].
//!
//! This is NoWayOS's software rendering path —
//! UI tree → layout → `embedded-graphics` primitives → draw target →
//! framebuffer — with no GPU, window system or C library involved. The same
//! code draws into an in-memory framebuffer in CI (see `nowayos-testkit`)
//! and, later, into a display PD's framebuffer.
//!
//! * [`render`] repaints the whole viewport;
//! * [`render_dirty`] repaints only the areas the tree has marked dirty, so
//!   a key press redraws one text field rather than the screen;
//! * [`RawFramebuffer`] is a draw target over raw framebuffer memory laid
//!   out as a validated `FramebufferLayout` says — how a UI PD draws into
//!   the framebuffer it shares with the display PD, with exactly the pixels
//!   the golden images in CI have, stored in the layout's pixel format;
//! * [`render_pointer`] draws the mouse pointer over a frame: the
//!   UI loop's, never a tree's, so no golden image holds it;
//! * [`Theme`] holds the colours, the type scale and the minimum target size —
//!   the first cut of design tokens, with a [`Theme::light`] default and a
//!   large-text [`Theme::high_contrast`] variant for accessibility. In every
//!   theme ([`Theme::all`]) text meets WCAG 2's contrast minimum of 4.5:1,
//!   control boundaries and the focus ring 3:1, and every focusable control
//!   is at least 24 pixels each way, which the tests check.
//!   A product's own themes - a brand and its high-contrast version - go
//!   through the same contrast check in the product's tests ([`contrast`]),
//!   and a UI PD switches between them at run time
//!   (`nowayos-ui-loop`).
//!
//! Every node is drawn clipped to its own rectangle, so text, a caret or a
//! focus ring that does not fit is cut at the node's edge rather than drawn
//! over its neighbours — which is also what lets [`render_dirty`] repaint
//! only the dirty areas and still give exactly the pixels of a full repaint.
//!
//! # Text
//!
//! Both themes draw text from the glyph atlases of the signed-off font
//! (`nowayos-glyphs`): proportional, kerned and anti-aliased,
//! each node's text in its own role and weight from the theme's type scale
//! ([`TypeScale`]) - body text 18 pixels per em in the light theme,
//! 24 in the large-text theme. A line is
//! placed exactly as layout measures it ([`Theme::metrics`]: the same
//! atlas's advances and kerning, in 1/64 pixel), each glyph at the nearest
//! whole pixel.
//!
//! # Device scale
//!
//! A theme's sizes are in layout units. A tree laid out at device scale 2
//! ([`Theme::metrics_at`]) is drawn at 2 pixels a unit: its text
//! from the atlas rasterised at twice the size - 36 px for the light
//! theme's 18 px body text - and every line, ring and border twice as
//! wide, so a screen drawn for the Pi 5's 1920 x 1080 window is crisp at
//! the monitor's own pixels, never a smaller frame doubled. [`render`]
//! takes the scale from the tree ([`Tree::scale`]). Each pixel's coverage, from 0 to 255, blends the text's
//! colour over the colour the node is known to draw beneath it - the
//! background, a button's fill, a text input's fill, the error marker -
//! per channel in the colour type's own depth, in integer arithmetic:
//! `(text × c + background × (255 − c) + 127) / 255`, the nearest whole
//! value. Nothing is read back from the framebuffer, so the same pixels
//! come out whatever the draw target, and a dirty repaint equals a full one.
//! Where two glyphs share a pixel, their coverage adds up, capped at full.
//! Drawing needs a colour type with RGB channels that converts from
//! [`Rgb888`] - every RGB colour of `embedded-graphics`, such as `Rgb888`
//! and `Rgb565`.
//!
//! `embedded-graphics`' built-in ISO 8859-1 monospace bitmap fonts remain
//! for bring-up and debug screens ([`Font::Mono`]), never a product UI:
//! they ship inside `embedded-graphics` (MIT OR Apache-2.0) but are
//! generated from X.Org bitmap fonts marked public domain, which is outside
//! the house licence list - signed off as interim for that use.
//!
//! Provisional: this API may change.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

pub mod contrast;
mod framebuffer;
mod pointer;
mod shape;
mod text;

pub use embedded_graphics::pixelcolor::Rgb888;
pub use framebuffer::{RawFramebuffer, RawFramebufferError};
pub use pointer::{POINTER_HEIGHT, POINTER_WIDTH, pointer_area, render_pointer};

use core::fmt;

use embedded_graphics::Drawable;
use embedded_graphics::draw_target::{DrawTarget, DrawTargetExt};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::{PixelColor, RgbColor};
use embedded_graphics::prelude::{Pixel, Point, Size};
use embedded_graphics::primitives::{
    Circle, Polyline, Primitive, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, StrokeAlignment,
};
use embedded_graphics::text::{Baseline, Text};
use nowayos_display::{Insets, Rect};
use nowayos_glyphs::{
    Atlas, BOLD_14, BOLD_16, BOLD_18, BOLD_20, BOLD_24, BOLD_28, BOLD_32, BOLD_36, BOLD_40,
    BOLD_48, BOLD_56, BOLD_72, DISPLAY_96, DISPLAY_192, REGULAR_14, REGULAR_16, REGULAR_18,
    REGULAR_24, REGULAR_28, REGULAR_32, REGULAR_36, REGULAR_48,
};
use nowayos_ui::keyboard::KeyAction;
use nowayos_ui::text::{fit_end, fit_start, wrap};
use nowayos_ui::{
    ButtonVariant, Colour, InputMode, Look, MIN_TARGET, Node, NodeId, NodeKind, Spacing, Surface,
    TextAlign, TextMetrics, TextRole, TextStyle, Tone, Tree, Weight,
};

use crate::shape::{Paint, Rounded};

/// What a theme draws text in.
#[derive(Clone, Copy)]
pub enum Font {
    /// One size of the signed-off font's type scale, as glyph atlases
    /// (`nowayos-glyphs`): proportional, kerned, anti-aliased text,
    /// as every product theme draws it. One atlas for each device
    /// scale the size is drawn at, from 1: `[&REGULAR_24,
    /// &REGULAR_48]` is 24 px text, drawn from the 48 px atlas at scale 2.
    /// Only the atlases a theme names are linked into a program.
    Atlas(&'static [&'static Atlas]),
    /// One of `embedded-graphics`' built-in monospace bitmap fonts, for
    /// bring-up and debug screens only - signed off as interim for them,
    /// never for a product UI. Each pixel of a glyph is either the
    /// text's colour or left as it was. The same size at every scale.
    Mono(&'static MonoFont<'static>),
}

impl Font {
    /// The atlas drawn at device scale `scale`: the font's for it; `None`
    /// for a scale it names none for - never a smaller atlas in its place,
    /// which would shrink the text below its size - for a bitmap font, or
    /// for scale 0. Every theme names an atlas at every scale the display
    /// contract allows (`nowayos_display::MAX_SCALE`), as its tests check.
    #[must_use]
    pub fn atlas(&self, scale: u32) -> Option<&'static Atlas> {
        let Self::Atlas(atlases) = self else {
            return None;
        };
        let index = usize::try_from(scale.checked_sub(1)?).ok()?;
        atlases.get(index).copied()
    }

    /// Whether the font can be drawn at device scale `scale`: a bitmap font
    /// at any, an atlas font where it names an atlas.
    #[must_use]
    pub fn draws_at(&self, scale: u32) -> bool {
        matches!(self, Self::Mono(_)) || self.atlas(scale).is_some()
    }

    /// The metrics layout measures text in this font with, so that layout
    /// and drawing measure alike: an atlas's advances, kerning and line
    /// height, or the monospace special case - every character the same
    /// advance, nothing kerned ([`metrics`]). [`MIN_TARGET`] is the minimum
    /// target size ([`Theme::metrics`] gives a theme's own). At scale 1;
    /// [`metrics_at`](Self::metrics_at) for another.
    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.metrics_at(1)
    }

    /// The metrics at device scale `scale`: [`atlas`](Self::atlas)'s
    /// for that scale, which layout lays the tree out in pixels at
    /// ([`TextMetrics::with_scale`]).
    #[must_use]
    pub fn metrics_at(&self, scale: u32) -> TextMetrics {
        let metrics = match (self, self.atlas(scale)) {
            (_, Some(atlas)) => TextMetrics::from_atlas(atlas),
            (Self::Mono(font), None) => metrics(font),
            (Self::Atlas(_), None) => TextMetrics::new(0, 0),
        };
        metrics.with_scale(scale)
    }
}

impl fmt::Debug for Font {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.atlas(1) {
            Some(atlas) => return write!(f, "Atlas({} px)", atlas.size()),
            None if matches!(self, Self::Atlas(_)) => return f.write_str("Atlas(none)"),
            None => {}
        }
        match self {
            Self::Atlas(_) => Ok(()),
            Self::Mono(font) => write!(
                f,
                "Mono({}x{})",
                font.character_size.width, font.character_size.height
            ),
        }
    }
}

/// A theme's type scale: the font of each text role, in Regular
/// and in Bold - display, title and heading only in Bold. Each font names
/// its atlas at each device scale ([`Font::Atlas`]); sizes are in layout
/// units.
#[derive(Clone, Copy, Debug)]
pub struct TypeScale {
    /// A number or reference to read across a room: digits, capitals and a
    /// few signs only. Bold.
    pub display: Font,
    /// A screen's title. Bold.
    pub title: Font,
    /// A heading: a header bar's or a section's. Bold.
    pub heading: Font,
    /// Text, fields and buttons.
    pub body: Font,
    /// Body text in Bold.
    pub body_bold: Font,
    /// A field's or a tile's label.
    pub label: Font,
    /// A label in Bold.
    pub label_bold: Font,
    /// Small print and footnotes.
    pub caption: Font,
    /// Small print in Bold: a pill's.
    pub caption_bold: Font,
    /// Each role's line height, where the type scale sets one rather than
    /// the font's own.
    pub line_heights: LineHeights,
}

/// Each role's line height, as a percentage of its size; `None`
/// for the font's own - its ascent, descent and line gap, about 130% of
/// the size for the signed-off family. A line set tighter centres the
/// font's ascent and descent in it, as CSS sets a line height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineHeights {
    /// The display role's.
    pub display: Option<u32>,
    /// The title role's.
    pub title: Option<u32>,
    /// The heading role's.
    pub heading: Option<u32>,
    /// Body text's.
    pub body: Option<u32>,
    /// The label role's.
    pub label: Option<u32>,
    /// The caption role's.
    pub caption: Option<u32>,
}

impl LineHeights {
    /// Every role in the font's own line height.
    pub const NATURAL: Self = Self {
        display: None,
        title: None,
        heading: None,
        body: None,
        label: None,
        caption: None,
    };

    /// The showcase's (Showcase → Showcase Look): display set solid,
    /// at 100%, and titles at 120%; the rest in the font's own, about 130%.
    pub const SHOWCASE: Self = Self {
        display: Some(100),
        title: Some(120),
        ..Self::NATURAL
    };

    /// The percentage `role` is set in, if the type scale sets one.
    #[must_use]
    pub const fn of(&self, role: TextRole) -> Option<u32> {
        match role {
            TextRole::Display => self.display,
            TextRole::Title => self.title,
            TextRole::Heading => self.heading,
            TextRole::Body => self.body,
            TextRole::Label => self.label,
            TextRole::Caption => self.caption,
        }
    }
}

impl TypeScale {
    /// One font for every role: a bring-up theme's bitmap font.
    #[must_use]
    pub const fn uniform(font: Font) -> Self {
        Self {
            display: font,
            title: font,
            heading: font,
            body: font,
            body_bold: font,
            label: font,
            label_bold: font,
            caption: font,
            caption_bold: font,
            line_heights: LineHeights::NATURAL,
        }
    }

    /// The font text in `style` is drawn in: display, title and heading in
    /// their one, Bold, weight whatever the style says.
    #[must_use]
    pub const fn font(&self, style: TextStyle) -> Font {
        match (style.role, style.weight) {
            (TextRole::Display, _) => self.display,
            (TextRole::Title, _) => self.title,
            (TextRole::Heading, _) => self.heading,
            (TextRole::Body, Weight::Regular) => self.body,
            (TextRole::Body, Weight::Bold) => self.body_bold,
            (TextRole::Label, Weight::Regular) => self.label,
            (TextRole::Label, Weight::Bold) => self.label_bold,
            (TextRole::Caption, Weight::Regular) => self.caption,
            (TextRole::Caption, Weight::Bold) => self.caption_bold,
        }
    }

    /// Every style, with its font: each role in both weights.
    pub fn styles(&self) -> impl Iterator<Item = (TextStyle, Font)> + '_ {
        TextRole::ALL.into_iter().flat_map(move |role| {
            [TextStyle::new(role).regular(), TextStyle::new(role).bold()]
                .into_iter()
                .map(move |style| (style, self.font(style)))
        })
    }
}

/// Colours, the font, line widths and the minimum target size used to lay
/// out and draw a tree — the first cut of design tokens. Every size is in
/// layout units: at device scale 2 each is drawn twice as large
/// ([`Theme::metrics_at`]).
///
/// The contrast each pair of colours drawn together must have, in every
/// theme, is WCAG 2's: 4.5:1 for text, 3:1 for a control's boundary, the
/// marks of the choice controls and the focus ring. Each colour
/// below says what it is drawn against: for text, that is also the colour
/// its edges are blended with.
///
/// Provisional: this API may change.
#[derive(Debug, Clone, Copy)]
pub struct Theme<C: PixelColor> {
    /// The type scale: each text role's font, in each weight. A
    /// node's text is drawn in its style's ([`nowayos_ui::Style::text`]):
    /// body text, Regular, unless it names another role or weight.
    pub type_scale: TypeScale,
    /// Screen background.
    pub background: C,
    /// Ordinary text, on the background and on a text input's fill; also
    /// the box and tick of a checkbox and the ring and dot of a radio
    /// option, on the background.
    pub text: C,
    /// Muted text: the caption role's, on the background.
    pub text_muted: C,
    /// Placeholder text in empty inputs, on the input's fill.
    pub placeholder: C,
    /// Button fill: the button's boundary, against the background.
    pub button: C,
    /// Button fill while pressed, against the background.
    pub button_pressed: C,
    /// Button label, on both button fills.
    pub button_text: C,
    /// Text-input fill.
    pub input: C,
    /// Text-input border: the input's boundary, against the background
    /// outside it and the input's fill inside.
    pub input_border: C,
    /// Focus ring and text caret. The ring is drawn just inside the
    /// focused control's edge, so it lies against the background outside
    /// it; inside, against the background too on a checkbox or radio group,
    /// and on a button, through a one-pixel gap in the background colour
    /// between the ring and the fill; against the fill on a text input,
    /// where the caret is too.
    pub focus: C,
    /// Focus ring width, in layout units.
    pub focus_width: u32,
    /// A field's error: its message and its marker, on the background,
    /// with the background showing through the marker's `!`.
    pub error: C,
    /// The least width and height of every focusable control, in layout
    /// units: never below [`MIN_TARGET`], WCAG 2.2's minimum target size.
    /// Layout honours it through [`Theme::metrics`].
    pub min_target: u32,
    /// A secondary button's fill, inside its outline in the
    /// [`button`](Self::button) colour, its label in that colour too.
    pub button_secondary: C,
    /// The surfaces containers are drawn on.
    pub surfaces: Surfaces<C>,
    /// Corner radii, heights and border widths.
    pub shape: Shape,
    /// A progress bar's track: the part not yet filled. Its fill is
    /// the [`button`](Self::button) colour - on a header bar, the bar's text
    /// colour over the bar's own fill ([`Theme::progress_on`]).
    pub progress_track: C,
    /// What each spacing token measures, in layout units: the
    /// gaps, padding and spacers a screen names by size. Layout gives them
    /// through [`Theme::metrics`].
    pub spacing: Spacing,
}

/// The colours of one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceColours<C> {
    /// Its fill, which text on it is blended over.
    pub fill: C,
    /// Its border, [`Shape::surface_border`] wide.
    pub border: C,
    /// Every text on it, of every role, and the marks of choice controls.
    pub text: C,
}

/// Every surface's colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surfaces<C> {
    /// A card.
    pub card: SurfaceColours<C>,
    /// A header bar. Buttons on it are drawn the other way round: a
    /// primary button filled with its text colour, a secondary one outlined
    /// in its text colour at 70%, a quiet one in its text colour.
    pub header: SurfaceColours<C>,
    /// A notice in the success tone.
    pub success: SurfaceColours<C>,
    /// A notice in the caution tone.
    pub caution: SurfaceColours<C>,
    /// A notice in the warning tone.
    pub warning: SurfaceColours<C>,
}

impl<C: Copy> Surfaces<C> {
    /// The colours of `surface`.
    #[must_use]
    pub const fn of(&self, surface: Surface) -> SurfaceColours<C> {
        match surface {
            Surface::Card => self.card,
            Surface::Header => self.header,
            Surface::Notice(Tone::Success) => self.success,
            Surface::Notice(Tone::Caution) => self.caution,
            Surface::Notice(Tone::Warning) => self.warning,
        }
    }

    /// Every surface with its colours, for the contrast tests.
    #[must_use]
    pub const fn all(&self) -> [(Surface, SurfaceColours<C>); 5] {
        [
            (Surface::Card, self.card),
            (Surface::Header, self.header),
            (Surface::Notice(Tone::Success), self.success),
            (Surface::Notice(Tone::Caution), self.caution),
            (Surface::Notice(Tone::Warning), self.warning),
        ]
    }
}

/// A theme's shapes, in layout units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// A button's least height: at least 48 in every theme.
    pub button_height: u32,
    /// A button's corner radius.
    pub button_radius: u32,
    /// A secondary button's outline width.
    pub outline_width: u32,
    /// A card's corner radius.
    pub card_radius: u32,
    /// A notice's corner radius.
    pub notice_radius: u32,
    /// Every surface's border width.
    pub surface_border: u32,
}

impl Shape {
    /// The showcase's shapes (Showcase → Showcase Look): buttons 56
    /// high with corners of 12, an outline of 2; cards' corners 16 and
    /// notices' 12, with a border of 1.
    pub const SHOWCASE: Self = Self {
        button_height: 56,
        button_radius: 12,
        outline_width: 2,
        card_radius: 16,
        notice_radius: 12,
        surface_border: 1,
    };

    /// The corner radius of `surface`: a header bar's corners are square.
    #[must_use]
    pub const fn radius_of(&self, surface: Surface) -> u32 {
        match surface {
            Surface::Card => self.card_radius,
            Surface::Header => 0,
            Surface::Notice(_) => self.notice_radius,
        }
    }
}

impl<C: PixelColor + From<Rgb888>> Theme<C> {
    /// Every theme, with its name: the themes the contrast and target-size
    /// tests check, so a theme added here is checked too.
    #[must_use]
    pub fn all() -> [(&'static str, Self); 2] {
        [
            ("light", Self::light()),
            ("high contrast", Self::high_contrast()),
        ]
    }

    /// Dark ink on a warm light ground, in the showcase's type scale
    /// (Showcase → Showcase Look, approved 2026-10-08): display 96
    /// Bold, title 28 Bold, heading 20 Bold, body 18, label 16 and caption
    /// 14 - each in Regular and Bold - twice as many pixels at device scale
    /// 2.
    #[must_use]
    pub fn light() -> Self {
        Self {
            type_scale: LIGHT_TYPE,
            background: Rgb888::new(0xF6, 0xF4, 0xEF).into(),
            text: Rgb888::new(0x16, 0x19, 0x1D).into(),
            text_muted: Rgb888::new(0x5A, 0x62, 0x70).into(),
            placeholder: Rgb888::new(0x5A, 0x62, 0x70).into(),
            button: Rgb888::new(0x1F, 0x5F, 0xBF).into(),
            button_pressed: Rgb888::new(0x15, 0x42, 0x86).into(),
            button_text: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            input: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            input_border: Rgb888::new(0x80, 0x87, 0x8F).into(),
            focus: Rgb888::new(0xC2, 0x6A, 0x00).into(),
            focus_width: 2,
            error: Rgb888::new(0xA4, 0x16, 0x1A).into(),
            min_target: MIN_TARGET,
            button_secondary: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            surfaces: Surfaces {
                card: surface(0xFF_FF_FF, 0xE3_DE_D4, 0x16_19_1D),
                header: surface(0x1F_5F_BF, 0x1F_5F_BF, 0xFF_FF_FF),
                success: surface(0xE3_F1_EA, 0xE3_F1_EA, 0x16_3F_31),
                caution: surface(0xFB_EB_DD, 0xFB_EB_DD, 0x5A_2B_04),
                warning: surface(0xFF_F4_D6, 0xFF_F4_D6, 0x5C_3B_00),
            },
            shape: Shape::SHOWCASE,
            progress_track: Rgb888::new(0xE6, 0xE0, 0xD5).into(),
            spacing: Spacing::STANDARD,
        }
    }

    /// White and yellow on black, in the large-text type scale - no text
    /// below 24 px: body, label and caption 24, heading 28 Bold, title 36
    /// Bold, display 96 Bold - for people with low vision.
    #[must_use]
    pub fn high_contrast() -> Self {
        Self {
            type_scale: LARGE_TYPE,
            background: Rgb888::new(0, 0, 0).into(),
            text: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            text_muted: Rgb888::new(0xC0, 0xC0, 0xC0).into(),
            placeholder: Rgb888::new(0xC0, 0xC0, 0xC0).into(),
            button: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            button_pressed: Rgb888::new(0xFF, 0xD7, 0x00).into(),
            button_text: Rgb888::new(0, 0, 0).into(),
            input: Rgb888::new(0, 0, 0).into(),
            input_border: Rgb888::new(0xFF, 0xFF, 0xFF).into(),
            focus: Rgb888::new(0xFF, 0xD7, 0x00).into(),
            focus_width: 3,
            error: Rgb888::new(0xFF, 0x8A, 0x80).into(),
            min_target: MIN_TARGET,
            button_secondary: Rgb888::new(0, 0, 0).into(),
            // Every surface black with a white border and white text: the
            // tone of a notice is in its words.
            surfaces: Surfaces {
                card: surface(0x00_00_00, 0xFF_FF_FF, 0xFF_FF_FF),
                header: surface(0x00_00_00, 0xFF_FF_FF, 0xFF_FF_FF),
                success: surface(0x00_00_00, 0xFF_FF_FF, 0xFF_FF_FF),
                caution: surface(0x00_00_00, 0xFF_FF_FF, 0xFF_FF_FF),
                warning: surface(0x00_00_00, 0xFF_FF_FF, 0xFF_FF_FF),
            },
            shape: Shape {
                surface_border: 2,
                ..Shape::SHOWCASE
            },
            progress_track: Rgb888::new(0x59, 0x59, 0x59).into(),
            spacing: Spacing::LARGE,
        }
    }
}

/// A surface's colours from `0xRRGGBB` values: fill, border and text.
fn surface<C: From<Rgb888>>(fill: u32, border: u32, text: u32) -> SurfaceColours<C> {
    SurfaceColours {
        fill: rgb(fill),
        border: rgb(border),
        text: rgb(text),
    }
}

/// A colour from `0xRRGGBB`.
fn rgb<C: From<Rgb888>>(value: u32) -> C {
    let [_, r, g, b] = value.to_be_bytes();
    Rgb888::new(r, g, b).into()
}

/// The light theme's type scale: each role's atlas at device scales 1 and
/// 2.
const LIGHT_TYPE: TypeScale = TypeScale {
    display: Font::Atlas(&[&DISPLAY_96, &DISPLAY_192]),
    title: Font::Atlas(&[&BOLD_28, &BOLD_56]),
    heading: Font::Atlas(&[&BOLD_20, &BOLD_40]),
    body: Font::Atlas(&[&REGULAR_18, &REGULAR_36]),
    body_bold: Font::Atlas(&[&BOLD_18, &BOLD_36]),
    label: Font::Atlas(&[&REGULAR_16, &REGULAR_32]),
    label_bold: Font::Atlas(&[&BOLD_16, &BOLD_32]),
    caption: Font::Atlas(&[&REGULAR_14, &REGULAR_28]),
    caption_bold: Font::Atlas(&[&BOLD_14, &BOLD_28]),
    line_heights: LineHeights::SHOWCASE,
};

/// The large-text theme's type scale: no text below 24 px.
const LARGE_TYPE: TypeScale = TypeScale {
    display: Font::Atlas(&[&DISPLAY_96, &DISPLAY_192]),
    title: Font::Atlas(&[&BOLD_36, &BOLD_72]),
    heading: Font::Atlas(&[&BOLD_28, &BOLD_56]),
    body: Font::Atlas(&[&REGULAR_24, &REGULAR_48]),
    body_bold: Font::Atlas(&[&BOLD_24, &BOLD_48]),
    label: Font::Atlas(&[&REGULAR_24, &REGULAR_48]),
    label_bold: Font::Atlas(&[&BOLD_24, &BOLD_48]),
    caption: Font::Atlas(&[&REGULAR_24, &REGULAR_48]),
    caption_bold: Font::Atlas(&[&BOLD_24, &BOLD_48]),
    line_heights: LineHeights::SHOWCASE,
};

impl<C: PixelColor> Theme<C> {
    /// Whether the theme can be drawn at device scale `scale`: every font of
    /// its type scale names an atlas for it. A UI refuses a
    /// display whose scale its theme cannot draw (`nowayos-ui-loop`), rather
    /// than draw text smaller.
    #[must_use]
    pub fn draws_at(&self, scale: u32) -> bool {
        self.type_scale
            .styles()
            .all(|(_, font)| font.draws_at(scale))
    }

    /// The metrics to lay a tree out with, so layout and drawing agree: the
    /// font's ([`Font::metrics`]) with the theme's minimum target size. At
    /// device scale 1; [`metrics_at`](Self::metrics_at) for another.
    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.metrics_at(1)
    }

    /// The metrics to lay a tree out with at device scale `scale`:
    /// the font's atlas for that scale ([`Font::metrics_at`]), the minimum
    /// target size in pixels at it, and the scale, at which layout gives
    /// every length a style names. A tree laid out with them is drawn at
    /// the same scale ([`render`] takes it from the tree).
    #[must_use]
    pub fn metrics_at(&self, scale: u32) -> TextMetrics {
        let mut metrics = self.type_scale.body.metrics_at(scale);
        metrics = metrics
            .with_min_target(self.min_target.saturating_mul(metrics.scale()))
            .with_button_height(self.shape.button_height.saturating_mul(metrics.scale()))
            .with_spacing(self.spacing);
        for (style, font) in self.type_scale.styles() {
            if let Some(atlas) = font.atlas(scale) {
                metrics = match self.type_scale.line_heights.of(style.role) {
                    // The role's size in pixels at this scale is its
                    // atlas's: the line that percentage of it, rounded.
                    Some(percent) => {
                        let line = u32::from(atlas.size())
                            .saturating_mul(percent)
                            .saturating_add(50)
                            / 100;
                        metrics.with_face_line(style, atlas, line)
                    }
                    None => metrics.with_face(style, atlas),
                };
            }
        }
        metrics
    }
}

impl<C: RgbColor + From<Rgb888>> Theme<C> {
    /// One of the theme's colours, by its role: what a screen's
    /// style names - a fill, a border, a text's colour - is drawn in the
    /// theme's own, so a theme switched recolours it.
    #[must_use]
    pub const fn colour(&self, colour: Colour) -> C {
        match colour {
            Colour::Background => self.background,
            Colour::Text => self.text,
            Colour::TextMuted => self.text_muted,
            Colour::Placeholder => self.placeholder,
            Colour::Button => self.button,
            Colour::ButtonText => self.button_text,
            Colour::Input => self.input,
            Colour::InputBorder => self.input_border,
            Colour::Focus => self.focus,
            Colour::Error => self.error,
        }
    }

    /// The colour beneath what stands on `on`: the surface's fill,
    /// or the background.
    #[must_use]
    pub const fn ground(&self, on: Option<Surface>) -> C {
        match on {
            Some(surface) => self.surfaces.of(surface).fill,
            None => self.background,
        }
    }

    /// The colour of text in `role` standing on `on`: on the
    /// background, the caption role's muted colour or the theme's text
    /// colour; on a card the same, as its fill is as light as the
    /// background; on any other surface, the surface's text colour.
    #[must_use]
    pub const fn text_on(&self, on: Option<Surface>, role: TextRole) -> C {
        let muted = matches!(role, TextRole::Caption);
        match on {
            None | Some(Surface::Card) if muted => self.text_muted,
            None => self.text,
            Some(surface) => self.surfaces.of(surface).text,
        }
    }

    /// The colour of text and marks with no role of their own - a
    /// checkbox's box and label, a radio group's rings and options -
    /// standing on `on`.
    #[must_use]
    pub const fn ink_on(&self, on: Option<Surface>) -> C {
        match on {
            Some(surface) => self.surfaces.of(surface).text,
            None => self.text,
        }
    }

    /// The focus ring's colour standing on `on`: on a header bar,
    /// the bar's text colour, as the theme's focus colour need not stand
    /// out from the brand colour; elsewhere the theme's.
    #[must_use]
    pub const fn focus_on(&self, on: Option<Surface>) -> C {
        match on {
            Some(Surface::Header) => self.surfaces.header.text,
            _ => self.focus,
        }
    }

    /// A button's colours in `variant` standing on `on`: primary
    /// filled with [`button`](Self::button), its label in
    /// [`button_text`](Self::button_text); secondary filled with
    /// [`button_secondary`](Self::button_secondary) inside an outline in
    /// `button`, its label in `button`; quiet its label alone, in
    /// `button`. Pressed, a primary button is
    /// [`button_pressed`](Self::button_pressed); a secondary or quiet one
    /// becomes a primary one. On a header bar they are turned round: the
    /// bar's text colour fills a primary button, its label in the bar's
    /// colour - 90% of the way to the text colour when pressed - outlines a
    /// secondary one at 70% and colours a secondary or quiet one's label,
    /// which pressed is filled with it.
    #[must_use]
    pub fn button_on(&self, on: Option<Surface>, variant: ButtonVariant) -> ButtonColours<C> {
        let (brand, on_brand, pressed) = if on == Some(Surface::Header) {
            let bar = self.surfaces.header;
            (bar.text, bar.fill, text::blend(bar.text, bar.fill, 230))
        } else {
            (self.button, self.button_text, self.button_pressed)
        };
        match variant {
            ButtonVariant::Primary => ButtonColours {
                fill: Some(brand),
                outline: None,
                label: on_brand,
                pressed,
                pressed_label: on_brand,
            },
            ButtonVariant::Secondary if on == Some(Surface::Header) => ButtonColours {
                fill: None,
                outline: Some(text::blend(brand, on_brand, 179)),
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
            },
            ButtonVariant::Secondary => ButtonColours {
                fill: Some(self.button_secondary),
                outline: Some(brand),
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
            },
            ButtonVariant::Quiet => ButtonColours {
                fill: None,
                outline: None,
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
            },
        }
    }

    /// A progress bar's colours standing on `on`: its fill and its
    /// track. On a header bar, the bar's text colour fills it and the bar
    /// itself is its track; elsewhere the theme's button colour and track.
    #[must_use]
    pub const fn progress_on(&self, on: Option<Surface>) -> (C, C) {
        match on {
            Some(Surface::Header) => (self.surfaces.header.text, self.surfaces.header.fill),
            _ => (self.button, self.progress_track),
        }
    }

    /// A field's error colour standing on `on`: on a header bar,
    /// the bar's text colour; elsewhere the theme's.
    #[must_use]
    pub const fn error_on(&self, on: Option<Surface>) -> C {
        match on {
            Some(Surface::Header) => self.surfaces.header.text,
            _ => self.error,
        }
    }
}

/// Layout metrics for a monospace `embedded-graphics` font, with
/// [`MIN_TARGET`] as the minimum target size.
#[must_use]
pub fn metrics(font: &MonoFont<'_>) -> TextMetrics {
    TextMetrics::new(
        font.character_size
            .width
            .saturating_add(font.character_spacing),
        font.character_size.height,
    )
}

/// Paints the tree's whole viewport.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render<D, const N: usize>(
    tree: &Tree<N>,
    target: &mut D,
    theme: &Theme<D::Color>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    render_area(tree, target, theme, tree.viewport())
}

/// Paints only the areas the tree has marked dirty. Call
/// [`Tree::clear_dirty`] afterwards.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_dirty<D, const N: usize>(
    tree: &Tree<N>,
    target: &mut D,
    theme: &Theme<D::Color>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    for &area in tree.dirty_rects() {
        render_area(tree, target, theme, area)?;
    }
    Ok(())
}

/// Paints `area` (clipped to the viewport): background first, then every
/// node that overlaps it, parents before children. Nothing outside `area`
/// is touched.
///
/// Each node draws only inside its own rectangle: text, carets and focus
/// rings that do not fit are cut at the node's edge. So a node's pixels
/// change only where the node is, which is what makes repainting just the
/// dirty areas ([`render_dirty`]) give exactly the pixels of a full repaint.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_area<D, const N: usize>(
    tree: &Tree<N>,
    target: &mut D,
    theme: &Theme<D::Color>,
    area: Rect,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    let Some(area) = area.intersection(&tree.viewport()) else {
        return Ok(());
    };
    let scale = tree.scale();
    let base = theme.metrics_at(scale);
    Painter {
        target: &mut target.clipped(&to_eg(area)),
        theme,
        metrics: base,
        scale,
        font: theme.type_scale.body,
        clip: area,
        ground: theme.background,
        surface: None,
    }
    .fill(area, theme.background)?;
    for (id, node) in tree.paint_order() {
        // The part of the node inside the area - and, inside the scroll
        // view, inside the view: everything it draws is clipped to
        // this, in NoWayOS's overflow-safe geometry first and by
        // `embedded-graphics` as well.
        let seen = match tree.scroll_clip(node) {
            Some(view) => area.intersection(&view),
            None => Some(area),
        };
        if let Some(clip) = seen.and_then(|seen| node.rect().intersection(&seen)) {
            // Its text in its own role and weight, measured as
            // layout measured it.
            let style = node.style().text;
            // What it stands on: the nearest container
            // drawn on a surface or with a fill of its own, or the
            // background.
            let (surface, fill) = under(tree, node);
            Painter {
                target: &mut target.clipped(&to_eg(clip)),
                theme,
                metrics: base.for_style(style),
                scale,
                font: theme.type_scale.font(style),
                clip,
                ground: fill.map_or_else(|| theme.ground(surface), |fill| theme.colour(fill)),
                surface,
            }
            .node(tree, id, node)?;
        }
    }
    Ok(())
}

/// What `node` stands on: the surface and the fill of its
/// nearest ancestor drawn on a surface or with a fill of its own - a fill,
/// if it names one, is the colour beneath, and the surface still sets the
/// colours of text and controls - or neither, for the background.
pub(crate) fn under<const N: usize>(
    tree: &Tree<N>,
    node: &Node,
) -> (Option<Surface>, Option<Colour>) {
    let mut next = node.parent();
    // Each step goes one level up; a tree is at most MAX_DEPTH deep, so
    // the bound only guards against corrupt links.
    for _ in 0..=nowayos_ui::MAX_DEPTH {
        let Some(parent) = next.and_then(|id| tree.get(id)) else {
            break;
        };
        let style = parent.style();
        if parent.kind().is_container() && (style.surface.is_some() || style.look.fill.is_some()) {
            return (style.surface, style.look.fill);
        }
        next = parent.parent();
    }
    (None, None)
}

/// A button's colours where it stands ([`Theme::button_on`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonColours<C> {
    /// Its fill, if it has one of its own; a quiet button shows what lies
    /// beneath it.
    pub fill: Option<C>,
    /// Its outline, if any: its boundary.
    pub outline: Option<C>,
    /// Its label, on its fill or on what lies beneath it.
    pub label: C,
    /// Its fill while pressed: always a fill of its own, so the press shows
    /// on every variant.
    pub pressed: C,
    /// Its label while pressed, on that fill.
    pub pressed_label: C,
}

/// Converts a NoWayOS rectangle to an `embedded-graphics` one.
#[must_use]
pub const fn to_eg(rect: Rect) -> Rectangle {
    Rectangle::new(
        Point::new(rect.origin.x, rect.origin.y),
        Size::new(rect.size.width, rect.size.height),
    )
}

/// Draws into one clipped rectangle: the area being painted, or the part of
/// one node inside it.
struct Painter<'t, 'th, D: DrawTarget> {
    target: &'t mut D,
    theme: &'th Theme<D::Color>,
    metrics: TextMetrics,
    /// The device scale: pixels each way for each layout unit.
    scale: u32,
    /// The font the node's text is drawn in: its style's, from the type
    /// scale. At an atlas font, `metrics` holds the atlas for the
    /// scale.
    font: Font,
    /// Everything drawn is first intersected with this, using NoWayOS's
    /// overflow-safe geometry, so `embedded-graphics` only ever sees
    /// coordinates inside, or within one character of, the viewport — which
    /// [`Tree::layout`] keeps well inside the `i32` range.
    clip: Rect,
    /// The colour beneath the node: its surface's fill, or the
    /// background. Its text and its shapes' edges are blended over it.
    ground: D::Color,
    /// The surface the node stands on, if any.
    surface: Option<Surface>,
}

impl<D> Painter<'_, '_, D>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    fn node<const N: usize>(
        &mut self,
        tree: &Tree<N>,
        id: NodeId,
        node: &Node,
    ) -> Result<(), D::Error> {
        let rect = node.rect();
        let focused = tree.focused() == Some(id);
        // The focus ring only after keyboard use; a text input's
        // caret whenever it has focus.
        let ring = focused && tree.focus_visible();
        let content = node.content();
        match node.kind() {
            NodeKind::Column | NodeKind::Row | NodeKind::Stack | NodeKind::Scroll { .. } => {
                let (surface, look) = (node.style().surface, node.style().look);
                if look.draws() {
                    self.looked(rect, surface, look)
                } else if let Some(surface) = surface {
                    self.surface(rect, surface)
                } else {
                    Ok(())
                }
            }
            NodeKind::Group { .. } | NodeKind::Spacer => Ok(()),
            NodeKind::Progress { permille, .. } => self.progress(content, *permille),
            NodeKind::Icon(icon) => self.icon(content, icon),
            NodeKind::Text(text) => self.text_lines(text, node, content),
            NodeKind::Slot { text, .. } => self.text_lines(text, node, content),
            NodeKind::FieldError { message, .. } => match message {
                Some(message) => self.field_error(message, content),
                None => Ok(()),
            },
            NodeKind::Checkbox { label, checked, .. } => {
                if let Some(mark) = self.choice_mark(content) {
                    self.check_box(mark, *checked)?;
                }
                self.marked_text(label, content, self.ink())?;
                if ring {
                    self.frame(rect, self.px(self.theme.focus_width), self.theme.focus)?;
                }
                Ok(())
            }
            NodeKind::RadioGroup {
                options, selected, ..
            } => {
                for (index, option) in options.iter().enumerate() {
                    let Some(row) = node.option_rect(index) else {
                        continue;
                    };
                    if let Some(mark) = self.choice_mark(row) {
                        let chosen = selected.is_some_and(|chosen| usize::from(chosen) == index);
                        self.radio(mark, chosen)?;
                    }
                    self.marked_text(option, row, self.ink())?;
                }
                if ring {
                    self.frame(rect, self.px(self.theme.focus_width), self.theme.focus)?;
                }
                Ok(())
            }
            NodeKind::ListPicker {
                options,
                placeholder,
                selected,
                rows,
                top,
                cursor,
                ..
            } => {
                // On a text input's fill and border: its text the
                // theme's, wherever the picker stands.
                let fill = self.theme.input;
                self.fill(rect, fill)?;
                self.ground = fill;
                self.surface = None;
                let rule = self.px(1);
                self.frame(rect, rule, self.theme.input_border)?;
                // The header: the choice, or the placeholder until one is
                // made, over a rule.
                if let Some(header) = node.list_row(0) {
                    let chosen = selected.and_then(|chosen| options.get(usize::from(chosen)));
                    let (words, colour) = match chosen {
                        Some(option) => (*option, self.theme.text),
                        None => (*placeholder, self.theme.placeholder),
                    };
                    let shown = fit_start(words, header.size.width, &self.metrics);
                    self.text(shown, header, TextAlign::Start, colour, fill)?;
                    let under = Rect::from_edges(
                        rect.left(),
                        header.bottom().saturating_sub(i64::from(rule)),
                        rect.right(),
                        header.bottom(),
                    );
                    self.fill(under, self.theme.input_border)?;
                }
                // The options showing, the chosen one marked, the
                // highlighted one ringed after a key.
                for row in 1..=usize::from(*rows) {
                    let option = usize::from(*top).saturating_add(row).saturating_sub(1);
                    let (Some(area), Some(label)) = (node.list_row(row), options.get(option))
                    else {
                        continue;
                    };
                    if let Some(mark) = self.choice_mark(area) {
                        let chosen = selected.is_some_and(|chosen| usize::from(chosen) == option);
                        self.radio(mark, chosen)?;
                    }
                    self.marked_text(label, area, self.theme.text)?;
                    if ring && option == usize::from(*cursor) {
                        self.frame(area, self.px(self.theme.focus_width), self.theme.focus)?;
                    }
                }
                // Where the list is, when it holds more than it shows: a
                // thin bar down its right edge.
                let count = u32::try_from(options.len()).unwrap_or(u32::MAX);
                if count > u32::from(*rows)
                    && let (Some(first), Some(last)) =
                        (node.list_row(1), node.list_row(usize::from(*rows)))
                {
                    let track =
                        u32::try_from(last.bottom().saturating_sub(first.top())).unwrap_or(0);
                    let thumb = track
                        .saturating_mul(u32::from(*rows))
                        .checked_div(count)
                        .unwrap_or(0)
                        .max(self.px(8));
                    let at = track
                        .saturating_mul(u32::from(*top))
                        .checked_div(count)
                        .unwrap_or(0);
                    let width = self.px(3);
                    let left = first.right().saturating_sub(i64::from(width));
                    let bar_top = first.top().saturating_add(i64::from(at));
                    let bar = Rect::from_edges(
                        left,
                        bar_top,
                        left.saturating_add(i64::from(width)),
                        bar_top.saturating_add(i64::from(thumb)).min(last.bottom()),
                    );
                    self.fill(bar, self.theme.text_muted)?;
                }
                Ok(())
            }
            NodeKind::Keyboard { mode, shift } => self.keyboard(tree, id, node, *mode, *shift),
            NodeKind::Button { label, .. } => {
                let colours = self.button_colours(node.style().variant);
                // Its look's fill, label colour and corners in place of its
                // variant's; pressed, the theme's own still.
                let look = node.style().look;
                let radius = self.px(look.radius.unwrap_or(self.theme.shape.button_radius));
                let mut body = Rounded {
                    rect,
                    radius,
                    border: 0,
                };
                if ring {
                    // The ring at the button's edge, then a gap in the
                    // colour beneath, then the button: so the ring lies
                    // against that colour on both sides, as a ring on a
                    // control's edge must contrast 3:1 with both colours
                    // beside it (WCAG 2, non-text contrast), and no colour
                    // does that with both the background and a button's
                    // fill.
                    let width = self.px(self.theme.focus_width);
                    let ring = Rounded {
                        rect,
                        radius,
                        border: width,
                    };
                    let paint = Paint {
                        fill: self.ground,
                        border: self.focus_colour(),
                        ground: self.ground,
                    };
                    ring.draw(&mut *self.target, self.clip, paint)?;
                    let inset = width.saturating_add(self.px(FOCUS_GAP));
                    body = Rounded {
                        rect: rect.inset(Insets::uniform(inset)),
                        radius: radius.saturating_sub(inset),
                        border: 0,
                    };
                }
                let (fill, label_colour, outline) = if tree.pressed() == Some(id) {
                    (colours.pressed, colours.pressed_label, None)
                } else {
                    let fill = look
                        .fill
                        .map_or(colours.fill.unwrap_or(self.ground), |colour| {
                            self.theme.colour(colour)
                        });
                    let label = look
                        .text
                        .map_or(colours.label, |colour| self.theme.colour(colour));
                    (fill, label, colours.outline)
                };
                let paint = Paint {
                    fill,
                    border: outline.unwrap_or(fill),
                    ground: self.ground,
                };
                if outline.is_some() {
                    body.border = self.px(self.theme.shape.outline_width);
                }
                body.draw(&mut *self.target, self.clip, paint)?;
                let shown = fit_start(label, content.size.width, &self.metrics);
                self.text(shown, content, TextAlign::Center, label_colour, fill)
            }
            NodeKind::TextInput {
                value, placeholder, ..
            } => {
                let fill = self.theme.input;
                self.fill(rect, fill)?;
                if value.is_empty() {
                    let shown = fit_start(placeholder, content.size.width, &self.metrics);
                    let hint = self.theme.placeholder;
                    self.text(shown, content, TextAlign::Start, hint, fill)?;
                }
                // Keep room for the caret; show the end of long values.
                let room = content.size.width.saturating_sub(self.px(CARET_ROOM));
                let shown = fit_end(value, room, &self.metrics);
                self.text(shown, content, TextAlign::Start, self.theme.text, fill)?;
                // The border or focus ring after the text, over any edge of
                // a glyph that reaches it: text is blended with the fill
                // and is only right where the fill is.
                if ring {
                    self.frame(rect, self.px(self.theme.focus_width), self.theme.focus)?;
                } else {
                    self.frame(rect, self.px(INPUT_BORDER), self.theme.input_border)?;
                }
                if !focused {
                    return Ok(());
                }
                let advance = self.metrics.text_width(shown);
                let caret = Rect::new(
                    content.origin.x,
                    content.origin.y,
                    self.px(CARET_WIDTH),
                    self.metrics.line_height().min(content.size.height),
                );
                match caret.checked_translate(
                    i32::try_from(advance).unwrap_or(i32::MAX),
                    vertical_offset(content, self.metrics.line_height()),
                ) {
                    Some(caret) => self.fill(caret, self.theme.focus),
                    None => Ok(()),
                }
            }
        }
    }

    /// Draws the on-screen keyboard: each key a keycap on a text
    /// input's fill and border, its label in the middle - the key held down
    /// in a pressed button's colours, and Shift, while it is on, in a
    /// button's. While no text input has focus its keys type nothing and
    /// their labels are muted.
    fn keyboard<const N: usize>(
        &mut self,
        tree: &Tree<N>,
        id: NodeId,
        node: &Node,
        mode: InputMode,
        shift: bool,
    ) -> Result<(), D::Error> {
        let live = tree.typing_target().is_some();
        let held = if tree.pressed() == Some(id) {
            tree.pressed_key()
        } else {
            None
        };
        let radius = self.px(KEY_RADIUS);
        let border = self.px(INPUT_BORDER);
        for (index, key) in nowayos_ui::keyboard::layout(mode).keys().enumerate() {
            let Some(rect) = node.key_rect(index) else {
                continue;
            };
            let latched = shift && key.action == KeyAction::Shift;
            let (fill, edge, label) = if held == Some(index) {
                let pressed = self.theme.button_pressed;
                (pressed, pressed, self.theme.button_text)
            } else if latched {
                (self.theme.button, self.theme.button, self.theme.button_text)
            } else if live {
                (self.theme.input, self.theme.input_border, self.theme.text)
            } else {
                (
                    self.theme.input,
                    self.theme.input_border,
                    self.theme.text_muted,
                )
            };
            let paint = Paint {
                fill,
                border: edge,
                ground: self.ground,
            };
            Rounded {
                rect,
                radius,
                border,
            }
            .draw(&mut *self.target, self.clip, paint)?;
            let shown = fit_start(key.label_for(shift), rect.size.width, &self.metrics);
            self.text(shown, rect, TextAlign::Center, label, fill)?;
        }
        Ok(())
    }

    /// Draws a field's error in `content`: a marker - a square one line
    /// high, in the error colour, with a `!` cut out of it - so the error
    /// is not shown by colour alone, then the message in words, in the
    /// error colour, one character's space after it.
    fn field_error(&mut self, message: &str, content: Rect) -> Result<(), D::Error> {
        let error = self.theme.error_on(self.surface);
        if let Some(marker) = self.mark(content) {
            self.fill(marker, error)?;
            // The `!` is blended with the marker's colour, so it is drawn
            // only inside the marker.
            if let Some(inside) = marker.intersection(&self.clip) {
                let (cut_out, on) = (self.ground, error);
                Painter {
                    target: &mut self.target.clipped(&to_eg(inside)),
                    theme: self.theme,
                    metrics: self.metrics,
                    scale: self.scale,
                    font: self.font,
                    clip: inside,
                    ground: self.ground,
                    surface: self.surface,
                }
                .text("!", marker, TextAlign::Center, cut_out, on)?;
            }
        }
        self.marked_text(message, content, error)
    }

    /// The place of the mark leading a line in `area`, which layout keeps
    /// for it: a square one line high (or as high as `area`, if less), at
    /// its start, centred vertically. A field's error marker fills it; a
    /// checkbox's box and a radio option's ring are drawn inside it
    /// ([`Self::choice_mark`]).
    fn mark(&self, area: Rect) -> Option<Rect> {
        let side = self.metrics.line_height().min(area.size.height);
        Rect::new(area.origin.x, area.origin.y, side, side)
            .checked_translate(0, vertical_offset(area, self.metrics.line_height()))
    }

    /// Where a checkbox's box or a radio option's ring goes in `area`: a
    /// square as high as the text's em - its atlas's size, 18 pixels in the
    /// light theme and 24 in the large-text theme - centred in the mark's
    /// place ([`Self::mark`]), a line high. So the background shows all
    /// round it even where a row is only a line high, as with the atlases:
    /// between one option's ring and the next, and between the mark and a
    /// focus ring. In a bitmap font, which has no em, it fills its
    /// place, as it always has.
    fn choice_mark(&self, area: Rect) -> Option<Rect> {
        let place = self.mark(area)?;
        let Some(atlas) = self.metrics.atlas() else {
            return Some(place);
        };
        let side = u32::from(atlas.size()).min(place.size.height);
        let inset = i32::try_from(place.size.height.saturating_sub(side) / 2).unwrap_or(0);
        Rect::new(place.origin.x, place.origin.y, side, side).checked_translate(inset, inset)
    }

    /// Draws `text` after the mark leading a line in `area`, one
    /// character's space after it, on the background.
    fn marked_text(&mut self, text: &str, area: Rect, color: D::Color) -> Result<(), D::Error> {
        let indent = self
            .metrics
            .line_height()
            .saturating_add(self.metrics.char_width());
        let words = Rect::from_edges(
            area.left().saturating_add(i64::from(indent)),
            area.top(),
            area.right(),
            area.bottom(),
        );
        let shown = fit_start(text, words.size.width, &self.metrics);
        let on = self.ground;
        self.text(shown, words, TextAlign::Start, color, on)
    }

    /// A checkbox's box in `mark`, with a tick in it if `checked`: the
    /// state is shown by the tick, not by colour.
    fn check_box(&mut self, mark: Rect, checked: bool) -> Result<(), D::Error> {
        let side = mark.size.width;
        self.frame(mark, line_weight(side), self.ink())?;
        // Only marks that reach the area being painted are handed to
        // `embedded-graphics`, so it only computes near the viewport.
        if !checked || mark.intersection(&self.clip).is_none() {
            return Ok(());
        }
        // The tick is worked out from the mark's corner and moved there as
        // it is drawn: `embedded-graphics` joins thick lines with products
        // of a point's coordinates and the lines' directions, which at the
        // viewport's own coordinates - up to 2^24 - overflow `i32` for a
        // mark as big as the large-text theme's, 36 pixels.
        let tick = [
            Point::new(tenths(0, side, 2), tenths(0, side, 5)),
            Point::new(tenths(0, side, 4), tenths(0, side, 7)),
            Point::new(tenths(0, side, 8), tenths(0, side, 3)),
        ];
        let weight = (side / 5).max(1);
        let corner = Point::new(mark.origin.x, mark.origin.y);
        Polyline::new(&tick)
            .into_styled(PrimitiveStyle::with_stroke(self.ink(), weight))
            .draw(&mut self.target.translated(corner))
    }

    /// A radio option's ring in `mark`, with a dot in it if `chosen`: the
    /// choice is shown by the dot, not by colour.
    fn radio(&mut self, mark: Rect, chosen: bool) -> Result<(), D::Error> {
        if mark.intersection(&self.clip).is_none() {
            return Ok(());
        }
        let side = mark.size.width;
        let corner = Point::new(mark.origin.x, mark.origin.y);
        let ring = PrimitiveStyleBuilder::new()
            .stroke_color(self.ink())
            .stroke_width(line_weight(side))
            .stroke_alignment(StrokeAlignment::Inside)
            .build();
        Circle::new(corner, side)
            .into_styled(ring)
            .draw(&mut *self.target)?;
        if chosen {
            // Half the ring's size, with an even margin all round.
            let mut dot = side / 2;
            if !side.saturating_sub(dot).is_multiple_of(2) {
                dot = dot.saturating_sub(1);
            }
            let inset = i32::try_from(side.saturating_sub(dot) / 2).unwrap_or(0);
            Circle::new(
                Point::new(
                    corner.x.saturating_add(inset),
                    corner.y.saturating_add(inset),
                ),
                dot,
            )
            .into_styled(PrimitiveStyle::with_fill(self.ink()))
            .draw(&mut *self.target)?;
        }
        Ok(())
    }

    /// The colour of a text or slot node's text where it stands
    /// ([`Theme::text_on`]).
    /// A text's or a slot's text in `content`: on one line, cut to fit; or,
    /// if its style wraps it, on the lines layout measured it on -
    /// broken after spaces at the content's width, at most the style's
    /// lines - each a line high, the block centred in the content's height.
    /// Each line sits across the content as its style aligns it.
    fn text_lines(&mut self, text: &str, node: &Node, content: Rect) -> Result<(), D::Error> {
        let (on, colour) = (self.ground, self.text_colour(node));
        let style = node.style().text;
        let metrics = self.metrics;
        let width = content.size.width;
        if !style.wraps() {
            let shown = fit_start(text, width, &metrics);
            return self.text(
                shown,
                content,
                aligned(shown, text, style.align),
                colour,
                on,
            );
        }
        let most = usize::from(style.lines);
        let line_height = metrics.line_height();
        let count = u32::try_from(wrap(text, width, &metrics).take(most).count()).unwrap_or(0);
        let block = line_height.saturating_mul(count);
        let mut top = content
            .top()
            .saturating_add(i64::from(content.size.height.saturating_sub(block) / 2));
        for line in wrap(text, width, &metrics).take(most) {
            let bottom = top.saturating_add(i64::from(line_height));
            let rect = Rect::from_edges(content.left(), top, content.right(), bottom);
            let shown = fit_start(line, width, &metrics);
            self.text(shown, rect, aligned(shown, line, style.align), colour, on)?;
            top = bottom;
        }
        Ok(())
    }

    fn text_colour(&self, node: &Node) -> D::Color {
        let style = node.style().text;
        style.colour.map_or_else(
            || self.theme.text_on(self.surface, style.role),
            |colour| self.theme.colour(colour),
        )
    }

    /// The colour of text and marks with no role of their own where the
    /// node stands ([`Theme::ink_on`]).
    fn ink(&self) -> D::Color {
        self.theme.ink_on(self.surface)
    }

    /// The focus ring's colour where the node stands ([`Theme::focus_on`]).
    fn focus_colour(&self) -> D::Color {
        self.theme.focus_on(self.surface)
    }

    /// A button's colours where it stands ([`Theme::button_on`]).
    fn button_colours(&self, variant: ButtonVariant) -> ButtonColours<D::Color> {
        self.theme.button_on(self.surface, variant)
    }

    /// Draws a container's `surface` over `rect`: its fill, its
    /// border and its rounded corners, over the colour beneath.
    fn surface(&mut self, rect: Rect, surface: Surface) -> Result<(), D::Error> {
        let colours = self.theme.surfaces.of(surface);
        let shape = Rounded {
            rect,
            radius: self.px(self.theme.shape.radius_of(surface)),
            border: self.px(self.theme.shape.surface_border),
        };
        let paint = Paint {
            fill: colours.fill,
            border: colours.border,
            ground: self.ground,
        };
        shape.draw(&mut *self.target, self.clip, paint)
    }

    /// Draws a container with a look of its own over `rect`: the
    /// fill, border and corners of its surface, if it is drawn on one, or
    /// none; with what its look names in their place - the theme's colours,
    /// widths in layout units. A border named by colour alone is 1 unit
    /// wide, or its surface's; by width alone, in a text input's border
    /// colour, or its surface's.
    fn looked(&mut self, rect: Rect, surface: Option<Surface>, look: Look) -> Result<(), D::Error> {
        let (fill, border, width, radius) = match surface {
            Some(surface) => {
                let colours = self.theme.surfaces.of(surface);
                (
                    colours.fill,
                    colours.border,
                    self.theme.shape.surface_border,
                    self.theme.shape.radius_of(surface),
                )
            }
            None => (self.ground, self.theme.input_border, 0, 0),
        };
        let fill = look.fill.map_or(fill, |colour| self.theme.colour(colour));
        let border = look
            .border
            .map_or(border, |colour| self.theme.colour(colour));
        let width = match look.border_width {
            Some(width) => width,
            None if look.border.is_some() => width.max(1),
            None => width,
        };
        let shape = Rounded {
            rect,
            radius: self.px(look.radius.unwrap_or(radius)),
            border: self.px(width),
        };
        let paint = Paint {
            fill,
            border,
            ground: self.ground,
        };
        shape.draw(&mut *self.target, self.clip, paint)
    }

    /// An icon in `rect`: its mask at the device scale, centred,
    /// each pixel's coverage blending the colour of text where it stands
    /// over what lies beneath, clipped.
    fn icon(&mut self, rect: Rect, icon: &nowayos_ui::icons::Icon) -> Result<(), D::Error> {
        let Some(mask) = icon.mask(self.scale) else {
            return Ok(());
        };
        let size = u32::from(mask.size());
        let left = rect
            .left()
            .saturating_add(i64::from(rect.size.width.saturating_sub(size) / 2));
        let top = rect
            .top()
            .saturating_add(i64::from(rect.size.height.saturating_sub(size) / 2));
        let (ink, ground, clip) = (self.ink(), self.ground, self.clip);
        let pixels = (0..size).flat_map(move |y| {
            (0..size).filter_map(move |x| {
                let coverage = mask.at(x, y).filter(|&c| c > 0)?;
                let px = left.saturating_add(i64::from(x));
                let py = top.saturating_add(i64::from(y));
                let point = Point::new(i32::try_from(px).ok()?, i32::try_from(py).ok()?);
                clip.contains(nowayos_display::Point::new(point.x, point.y))
                    .then(|| Pixel(point, text::blend(ink, ground, coverage)))
            })
        });
        self.target.draw_iter(pixels)
    }

    /// A progress bar in `rect`: a rounded track, and over it, from
    /// the start, a rounded fill `permille` thousandths of its width.
    fn progress(&mut self, rect: Rect, permille: u16) -> Result<(), D::Error> {
        let (fill, track) = self.theme.progress_on(self.surface);
        let radius = rect.size.height / 2;
        let bar = Rounded {
            rect,
            radius,
            border: 0,
        };
        bar.draw(
            &mut *self.target,
            self.clip,
            Paint {
                fill: track,
                border: track,
                ground: self.ground,
            },
        )?;
        let done = u64::from(rect.size.width)
            .saturating_mul(u64::from(permille.min(nowayos_ui::PROGRESS_WHOLE)))
            .checked_div(u64::from(nowayos_ui::PROGRESS_WHOLE))
            .unwrap_or(0);
        if done == 0 {
            return Ok(());
        }
        let filled = Rounded {
            rect: Rect::new(
                rect.origin.x,
                rect.origin.y,
                u32::try_from(done).unwrap_or(rect.size.width),
                rect.size.height,
            ),
            radius,
            border: 0,
        };
        // Its start's rounded end lies over the track's, so blends with
        // what lies beneath both; the rest of it over the track.
        let split = rect.left().saturating_add(i64::from(radius));
        for (part, ground) in [
            (
                Rect::from_edges(rect.left(), rect.top(), split, rect.bottom()),
                self.ground,
            ),
            (
                Rect::from_edges(split, rect.top(), rect.right(), rect.bottom()),
                track,
            ),
        ] {
            if let Some(clip) = part.intersection(&self.clip) {
                filled.draw(
                    &mut *self.target,
                    clip,
                    Paint {
                        fill,
                        border: fill,
                        ground,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// `units` layout units in pixels at the device scale.
    const fn px(&self, units: u32) -> u32 {
        units.saturating_mul(self.scale)
    }

    /// Fills `rect`, clipped.
    fn fill(&mut self, rect: Rect, color: D::Color) -> Result<(), D::Error> {
        match rect.intersection(&self.clip) {
            Some(visible) => self.target.fill_solid(&to_eg(visible), color),
            None => Ok(()),
        }
    }

    /// Draws a frame `width` pixels wide just inside `rect`.
    fn frame(&mut self, rect: Rect, width: u32, color: D::Color) -> Result<(), D::Error> {
        let inner = rect.inset(Insets::uniform(width));
        let top = Rect::from_edges(rect.left(), rect.top(), rect.right(), inner.top());
        let bottom = Rect::from_edges(rect.left(), inner.bottom(), rect.right(), rect.bottom());
        let left = Rect::from_edges(rect.left(), inner.top(), inner.left(), inner.bottom());
        let right = Rect::from_edges(inner.right(), inner.top(), rect.right(), inner.bottom());
        for band in [top, bottom, left, right] {
            self.fill(band, color)?;
        }
        Ok(())
    }

    /// Draws one line of text in `rect`, vertically centred, clipped, in
    /// `color` - from a glyph atlas blended over `background`, the colour
    /// the node is known to draw beneath it (the `text` module); in a
    /// bitmap font, over whatever is there.
    fn text(
        &mut self,
        text: &str,
        rect: Rect,
        align: TextAlign,
        color: D::Color,
        background: D::Color,
    ) -> Result<(), D::Error> {
        if text.is_empty() {
            return Ok(());
        }
        let width = self.metrics.text_width(text);
        let dx = match align {
            TextAlign::Start => 0,
            TextAlign::Center => rect.size.width.saturating_sub(width) / 2,
            TextAlign::End => rect.size.width.saturating_sub(width),
        };
        let left = rect.left().saturating_add(i64::from(dx));
        let top = rect
            .top()
            .saturating_add(i64::from(vertical_offset(rect, self.metrics.line_height())));
        match (self.metrics.atlas(), self.font) {
            (Some(atlas), _) => {
                let line = text::Line {
                    atlas,
                    text,
                    left,
                    baseline: top
                        .saturating_add(text::baseline_in(atlas, self.metrics.line_height())),
                };
                text::draw(&mut *self.target, line, self.clip, color, background)
            }
            (None, Font::Mono(font)) => self.mono_text(font, text, left, top, width, color),
            (None, Font::Atlas(_)) => Ok(()),
        }
    }

    /// Draws one line of text in a bitmap font, its cells' top-left corner
    /// at `(left, top)`, `width` pixels wide, clipped.
    ///
    /// Only the characters that can reach the clip rectangle are handed to
    /// `embedded-graphics`, starting at the first of them, so it never
    /// computes far outside the viewport however long the line is or
    /// wherever the node was laid out. With a monospace font that gives the
    /// same pixels as drawing the whole line clipped.
    fn mono_text(
        &mut self,
        font: &'static MonoFont<'static>,
        text: &str,
        left: i64,
        top: i64,
        width: u32,
        color: D::Color,
    ) -> Result<(), D::Error> {
        let advance = i64::from(self.metrics.char_width());
        if advance == 0 {
            return Ok(());
        }
        let line = Rect::from_edges(
            left,
            top,
            left.saturating_add(i64::from(width)),
            top.saturating_add(i64::from(self.metrics.line_height())),
        );
        let Some(visible) = line.intersection(&self.clip) else {
            return Ok(());
        };
        // Characters wholly left of the visible part are skipped; drawing
        // stops after the last one that starts inside it.
        let skip = visible
            .left()
            .saturating_sub(left)
            .checked_div(advance)
            .unwrap_or(0);
        let start = left.saturating_add(skip.saturating_mul(advance));
        let count = visible
            .right()
            .saturating_sub(start)
            .saturating_add(advance.saturating_sub(1))
            .checked_div(advance)
            .unwrap_or(0);
        let (Ok(skip), Ok(count)) = (usize::try_from(skip), usize::try_from(count)) else {
            return Ok(());
        };
        let shown = char_range(text, skip, count);
        let (Ok(x), Ok(y)) = (i32::try_from(start), i32::try_from(top)) else {
            return Ok(());
        };
        let style = MonoTextStyle::new(font, color);
        Text::with_baseline(shown, Point::new(x, y), style, Baseline::Top)
            .draw(&mut *self.target)
            .map(|_| ())
    }
}

/// The `count` characters of `text` after the first `skip` (fewer at the
/// end of the text), cut at character boundaries.
fn char_range(text: &str, skip: usize, count: usize) -> &str {
    let mut starts = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(core::iter::once(text.len()));
    let Some(begin) = starts.nth(skip) else {
        return "";
    };
    let end = match count.checked_sub(1) {
        Some(more) => starts.nth(more).unwrap_or(text.len()),
        None => begin,
    };
    text.get(begin..end).unwrap_or("")
}

/// Width, in layout units, of the gap in the background colour between a
/// focused button's focus ring and its fill.
const FOCUS_GAP: u32 = 1;

/// Width, in layout units, of a text input's border when it is not focused,
/// and of a key's of the on-screen keyboard.
const INPUT_BORDER: u32 = 1;

/// The corner radius, in layout units, of a key of the on-screen keyboard.
const KEY_RADIUS: u32 = 6;

/// Width, in layout units, of a text input's caret.
const CARET_WIDTH: u32 = 1;

/// Room, in layout units, a text input keeps at the end of its value for
/// the caret.
const CARET_ROOM: u32 = 2;

/// Where a line of text sits: as its style aligns it, unless it was
/// cut to fit - `shown` shorter than `whole` - when it starts at the start,
/// so what is cut off is at the end.
fn aligned(shown: &str, whole: &str, align: TextAlign) -> TextAlign {
    if shown.len() < whole.len() {
        TextAlign::Start
    } else {
        align
    }
}

/// The width of the lines of a mark `side` pixels across: a tenth of it, at
/// least one pixel.
fn line_weight(side: u32) -> u32 {
    (side / 10).max(1)
}

/// `base` plus `parts` tenths of `side`, saturating.
fn tenths(base: i32, side: u32, parts: u32) -> i32 {
    base.saturating_add(i32::try_from(side.saturating_mul(parts) / 10).unwrap_or(0))
}

/// Offset that centres a line of `line_height` in `rect`.
fn vertical_offset(rect: Rect, line_height: u32) -> i32 {
    i32::try_from(rect.size.height.saturating_sub(line_height) / 2).unwrap_or(0)
}
