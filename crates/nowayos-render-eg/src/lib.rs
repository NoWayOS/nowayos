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
//! Both themes draw text from the glyph outlines of the signed-off font
//! (`nowayos-glyphs`): proportional, kerned and anti-aliased,
//! each node's text in its own role and weight from the theme's type scale
//! ([`TypeScale`]) - body text 18 pixels per em in the light theme,
//! 24 in the large-text theme. A line is
//! placed exactly as layout measures it ([`Theme::metrics`]: the same
//! face's advances and kerning, in 1/64 pixel), each glyph at the nearest
//! whole pixel. Glyphs are drawn from the faces' outlines at the size and
//! scale the text needs, and kept, by a [`Glyphs`] the caller owns.
//!
//! # Device scale
//!
//! A theme's sizes are in layout units. A tree laid out at a device scale
//! ([`Theme::metrics_at`]), from 1 to 4 in quarter steps, is drawn at that
//! many pixels a unit: its text drawn from the outlines at the size times
//! the scale (36 px for the light theme's 18 px body text at 2, 27 at 1.5)
//! and every line, ring and border as many pixels wide, each to the
//! nearest whole pixel, so a screen is crisp at the monitor's own pixels at
//! any size, never a smaller frame enlarged. [`render`] takes the scale
//! from the tree ([`Tree::scale`]). Each pixel's coverage, from 0 to 255, blends the text's
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

// So the unit tests can name the crate as its users do, for
// `nowayos_testkit::glyph_store!`.
#[cfg(test)]
extern crate self as nowayos_render_eg;

mod compose;
pub mod contrast;
mod framebuffer;
mod glyphs;
mod picture;
mod pointer;
mod shape;
mod text;

pub use embedded_graphics::pixelcolor::Rgb888;
pub use framebuffer::{RawFramebuffer, RawFramebufferError};
pub use glyphs::{DEFAULT_BYTES, DEFAULT_SLOTS, GlyphRasteriser, Glyphs, Slot};
pub use pointer::{POINTER_HEIGHT, POINTER_WIDTH, pointer_area, render_pointer};

use core::fmt;

use embedded_graphics::Drawable;
use embedded_graphics::draw_target::{DrawTarget, DrawTargetExt};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::{PixelColor, RgbColor};
use embedded_graphics::prelude::{Pixel, Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text};
use nowayos_display::DeviceScale;
use nowayos_display::{Insets, Rect};
use nowayos_glyphs::{BOLD, DISPLAY, Face, REGULAR};
use nowayos_ui::edit;
use nowayos_ui::keyboard::KeyAction;
use nowayos_ui::motion::Visual;
use nowayos_ui::pictures::{Fit, Picture};
use nowayos_ui::rich::Run;
use nowayos_ui::text::{fit_start, wrap};
use nowayos_ui::{
    ButtonVariant, Colour, Elevation, GradientKind, InputMode, LiveId, Look, MIN_TARGET, Node,
    NodeId, NodeKind, Spacing, Surface, TextAlign, TextMetrics, TextRole, TextStyle, Tone, Tree,
    Weight,
};

use crate::shape::{Inside, Paint, Radii, Rounded, fill_polygon};

/// What a theme draws text in.
#[derive(Clone, Copy)]
pub enum Font {
    /// One face of the signed-off font at one size of the type scale, in
    /// layout units per em: drawn from the face's outlines
    /// (`nowayos-glyphs`) at that size times the device scale -
    /// proportional, kerned, anti-aliased text, as every product theme
    /// draws it, crisp at every scale.
    Outline(&'static Face, u16),
    /// One of `embedded-graphics`' built-in monospace bitmap fonts, for
    /// bring-up and debug screens only - signed off as interim for them,
    /// never for a product UI. Each pixel of a glyph is either the
    /// text's colour or left as it was. The same size at every scale.
    Mono(&'static MonoFont<'static>),
}

impl Font {
    /// The font's face and size in 1/64 pixel per em at device scale
    /// `scale`; `None` for a bitmap font.
    #[must_use]
    pub const fn face_at(&self, scale: DeviceScale) -> Option<(&'static Face, u32)> {
        match self {
            Self::Outline(face, size) => Some((face, scale.size64(*size as u32))),
            Self::Mono(_) => None,
        }
    }

    /// The metrics layout measures text in this font with, so that layout
    /// and drawing measure alike: the face's advances, kerning and line
    /// height at its size, or the monospace special case - every character
    /// the same advance, nothing kerned ([`metrics`]). [`MIN_TARGET`] is
    /// the minimum target size ([`Theme::metrics`] gives a theme's own). At
    /// scale 1; [`metrics_at`](Self::metrics_at) for another.
    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.metrics_at(DeviceScale::ONE)
    }

    /// The metrics at device scale `scale`: the face's at its size times
    /// the scale, which layout lays the tree out in pixels at
    /// ([`TextMetrics::with_scale`]).
    #[must_use]
    pub fn metrics_at(&self, scale: DeviceScale) -> TextMetrics {
        let metrics = match self {
            Self::Outline(face, size) => {
                TextMetrics::from_face(face, scale.size64(u32::from(*size)))
            }
            Self::Mono(font) => metrics(font),
        };
        metrics.with_scale(scale)
    }
}

impl fmt::Debug for Font {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Outline(face, size) => write!(f, "Outline(face {}, {size} px)", face.id()),
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
/// its face and size ([`Font::Outline`]); sizes are in layout units, drawn
/// at any device scale.
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
    /// A scrollbar's thumb, over what a scroll view shows - on a header
    /// bar, the bar's text colour ([`Theme::scrollbar_on`]).
    pub scrollbar: C,
    /// What is selected in a text input, behind its text (#10075).
    pub selection: C,
    /// A link in a rich text, on the background and on a card (#10078); on
    /// any other surface, links take its text's colour, underlined as
    /// always ([`Theme::link_on`]).
    pub link: C,
    /// Code in a rich text: the tint behind it, which its text is blended
    /// over (#10078).
    pub code: C,
    /// The shadows a node casts, by its elevation (#10079), drawn toward
    /// the [`veil`](Self::veil) colour.
    pub shadows: Shadows,
    /// What an open dialog veils the screen beneath it with: every colour
    /// there is drawn half way to this ([`Theme::veiled`], #10076).
    pub veil: C,
    /// What each spacing token measures, in layout units: the
    /// gaps, padding and spacers a screen names by size. Layout gives them
    /// through [`Theme::metrics`].
    pub spacing: Spacing,
}

/// A shadow (#10079): moved down `dy` and grown by `spread` layout units,
/// blurred over `blur` more, at most `alpha` out of 255 of the way to the
/// theme's veil colour. `dy`, `spread` and `blur` together stay within
/// [`nowayos_ui::layer::SHADOW_REACH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shadow {
    /// How far down it falls, in layout units.
    pub dy: u32,
    /// How wide its edge blurs, in layout units.
    pub blur: u32,
    /// How far it grows past the shape, in layout units.
    pub spread: u32,
    /// How dark it is where it is darkest, out of 255.
    pub alpha: u8,
}

/// A theme's shadows, one for each elevation (#10079).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shadows {
    /// A card resting on the screen.
    pub low: Shadow,
    /// A menu or popover.
    pub medium: Shadow,
    /// A dialog.
    pub high: Shadow,
}

impl Shadows {
    /// Soft shadows, as the website's cards cast.
    pub const SOFT: Self = Self {
        low: Shadow {
            dy: 1,
            blur: 4,
            spread: 0,
            alpha: 46,
        },
        medium: Shadow {
            dy: 4,
            blur: 14,
            spread: 0,
            alpha: 60,
        },
        high: Shadow {
            dy: 12,
            blur: 32,
            spread: 0,
            alpha: 80,
        },
    };

    /// The shadow for `elevation`.
    #[must_use]
    pub const fn of(&self, elevation: Elevation) -> Shadow {
        match elevation {
            Elevation::Low => self.low,
            Elevation::Medium => self.medium,
            Elevation::High => self.high,
        }
    }
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
    /// A text input's corner radius.
    pub field_radius: u32,
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
        field_radius: 8,
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
            scrollbar: Rgb888::new(0x6B, 0x6B, 0x6B).into(),
            selection: Rgb888::new(0xB9, 0xD4, 0xF5).into(),
            link: Rgb888::new(0x1F, 0x5F, 0xBF).into(),
            code: Rgb888::new(0xE9, 0xE5, 0xDC).into(),
            shadows: Shadows::SOFT,
            veil: Rgb888::new(0x16, 0x19, 0x1D).into(),
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
            scrollbar: Rgb888::new(0xC8, 0xC8, 0xC8).into(),
            selection: Rgb888::new(0x1F, 0x4E, 0x8C).into(),
            link: Rgb888::new(0x8A, 0xB4, 0xFF).into(),
            code: Rgb888::new(0x2A, 0x2A, 0x2A).into(),
            shadows: Shadows::SOFT,
            veil: Rgb888::new(0, 0, 0).into(),
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

/// The light theme's type scale: each role's face and size, drawn at any
/// device scale.
const LIGHT_TYPE: TypeScale = TypeScale {
    display: Font::Outline(&DISPLAY, 96),
    title: Font::Outline(&BOLD, 28),
    heading: Font::Outline(&BOLD, 20),
    body: Font::Outline(&REGULAR, 18),
    body_bold: Font::Outline(&BOLD, 18),
    label: Font::Outline(&REGULAR, 16),
    label_bold: Font::Outline(&BOLD, 16),
    caption: Font::Outline(&REGULAR, 14),
    caption_bold: Font::Outline(&BOLD, 14),
    line_heights: LineHeights::SHOWCASE,
};

const LARGE_TYPE: TypeScale = TypeScale {
    display: Font::Outline(&DISPLAY, 96),
    title: Font::Outline(&BOLD, 36),
    heading: Font::Outline(&BOLD, 28),
    body: Font::Outline(&REGULAR, 24),
    body_bold: Font::Outline(&BOLD, 24),
    label: Font::Outline(&REGULAR, 24),
    label_bold: Font::Outline(&BOLD, 24),
    caption: Font::Outline(&REGULAR, 24),
    caption_bold: Font::Outline(&BOLD, 24),
    line_heights: LineHeights::SHOWCASE,
};

impl<C: PixelColor> Theme<C> {
    /// The metrics to lay a tree out with, so layout and drawing agree: the
    /// font's ([`Font::metrics`]) with the theme's minimum target size. At
    /// device scale 1; [`metrics_at`](Self::metrics_at) for another.
    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.metrics_at(DeviceScale::ONE)
    }

    /// The metrics to lay a tree out with at device scale `scale`: each
    /// role's face at its size times the scale ([`Font::metrics_at`]), the
    /// minimum target size and button height in pixels at it, and the
    /// scale, at which layout gives every length a style names. A tree
    /// laid out with them is drawn at the same scale ([`render`] takes it
    /// from the tree).
    #[must_use]
    pub fn metrics_at(&self, scale: DeviceScale) -> TextMetrics {
        let mut metrics = self
            .type_scale
            .body
            .metrics_at(scale)
            .with_min_target(scale.px(self.min_target))
            .with_button_height(scale.px(self.shape.button_height))
            .with_spacing(self.spacing);
        for (style, font) in self.type_scale.styles() {
            if let Some((face, size64)) = font.face_at(scale) {
                metrics = match self.type_scale.line_heights.of(style.role) {
                    // The role's size in pixels at this scale: the line
                    // that percentage of it, to the nearest pixel.
                    Some(percent) => {
                        let line = size64.saturating_mul(percent).saturating_add(3200) / 6400;
                        metrics.with_face_line(style, face, size64, line)
                    }
                    None => metrics.with_face(style, face, size64),
                };
            }
        }
        metrics
    }
}

impl<C: RgbColor + From<Rgb888>> Theme<C> {
    /// This theme beneath an open dialog (#10076): every colour drawn
    /// half way to the [`veil`](Self::veil), so the screen shows, dimmed,
    /// out of reach behind it.
    #[must_use]
    pub fn veiled(&self) -> Self {
        let v = |colour: C| text::blend(self.veil, colour, VEIL);
        let s = |colours: SurfaceColours<C>| SurfaceColours {
            fill: v(colours.fill),
            border: v(colours.border),
            text: v(colours.text),
        };
        Self {
            background: v(self.background),
            text: v(self.text),
            text_muted: v(self.text_muted),
            placeholder: v(self.placeholder),
            button: v(self.button),
            button_pressed: v(self.button_pressed),
            button_text: v(self.button_text),
            input: v(self.input),
            input_border: v(self.input_border),
            focus: v(self.focus),
            error: v(self.error),
            button_secondary: v(self.button_secondary),
            surfaces: Surfaces {
                card: s(self.surfaces.card),
                header: s(self.surfaces.header),
                success: s(self.surfaces.success),
                caution: s(self.surfaces.caution),
                warning: s(self.surfaces.warning),
            },
            progress_track: v(self.progress_track),
            scrollbar: v(self.scrollbar),
            selection: v(self.selection),
            link: v(self.link),
            code: v(self.code),
            ..*self
        }
    }

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

    /// A link's colour standing on `on` (#10078): the theme's on the
    /// background or a card, the surface's text colour on any other.
    #[must_use]
    pub const fn link_on(&self, on: Option<Surface>) -> C {
        match on {
            None | Some(Surface::Card) => self.link,
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
                hover: text::blend(pressed, brand, 128),
            },
            ButtonVariant::Secondary if on == Some(Surface::Header) => ButtonColours {
                fill: None,
                outline: Some(text::blend(brand, on_brand, 179)),
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
                hover: hover_tint(brand, on_brand, brand),
            },
            ButtonVariant::Secondary => ButtonColours {
                fill: Some(self.button_secondary),
                outline: Some(brand),
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
                hover: hover_tint(brand, self.button_secondary, brand),
            },
            ButtonVariant::Quiet => ButtonColours {
                fill: None,
                outline: None,
                label: brand,
                pressed: brand,
                pressed_label: on_brand,
                hover: hover_tint(brand, self.ground(on), brand),
            },
        }
    }

    /// What a hovered option or list row stands on, over `ground`
    /// (#10082): the button colour's tint.
    #[must_use]
    pub fn hover_over(&self, ground: C) -> C {
        text::blend(self.button, ground, HOVER_TINT)
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

    /// A scrollbar's thumb standing on `on`: on a header bar, the bar's
    /// text colour; elsewhere the theme's.
    #[must_use]
    pub const fn scrollbar_on(&self, on: Option<Surface>) -> C {
        match on {
            Some(Surface::Header) => self.surfaces.header.text,
            _ => self.scrollbar,
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

/// How far beneath an open dialog each colour is drawn toward the theme's
/// veil, out of 255: half way.
const VEIL: u8 = 128;

/// The pixels of pictures decoded at run time (#10084), lent to each frame
/// by the program that holds them, by their [`LiveId`]: copied out of the
/// region the picture decoder PD wrote, into the program's own memory,
/// before they are lent.
pub trait Live {
    /// Picture `id`'s pixels, if there are any.
    fn view(&self, id: LiveId) -> Option<nowayos_ui::pictures::View<'_>>;
}

/// No picture decoded at run time: what [`render`] and its kin lend.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLive;

impl Live for NoLive {
    fn view(&self, _: LiveId) -> Option<nowayos_ui::pictures::View<'_>> {
        None
    }
}

/// [`render_dirty`], the pixels of pictures decoded at run time lent by
/// `live` (#10084). Call [`Tree::clear_dirty`] afterwards.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_dirty_live<D, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut D,
    theme: &Theme<D::Color>,
    glyphs: &mut Glyphs<'_>,
    live: &dyn Live,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    for &area in tree.dirty_rects() {
        render_area_live(tree, target, theme, area, glyphs, live)?;
    }
    Ok(())
}

/// Paints the tree's whole viewport.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render<D, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut D,
    theme: &Theme<D::Color>,
    glyphs: &mut Glyphs<'_>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    render_area(tree, target, theme, tree.viewport(), glyphs)
}

/// Paints only the areas the tree has marked dirty. Call
/// [`Tree::clear_dirty`] afterwards.
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_dirty<D, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut D,
    theme: &Theme<D::Color>,
    glyphs: &mut Glyphs<'_>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    for &area in tree.dirty_rects() {
        render_area(tree, target, theme, area, glyphs)?;
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
pub fn render_area<D, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut D,
    theme: &Theme<D::Color>,
    area: Rect,
    glyphs: &mut Glyphs<'_>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    render_area_live(tree, target, theme, area, glyphs, &NoLive)
}

/// [`render_area`], the pixels of pictures decoded at run time lent by
/// `live` (#10084).
///
/// # Errors
///
/// Whatever the draw target reports.
pub fn render_area_live<D, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut D,
    theme: &Theme<D::Color>,
    area: Rect,
    glyphs: &mut Glyphs<'_>,
    live: &dyn Live,
) -> Result<(), D::Error>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    let Some(area) = area.intersection(&tree.viewport()) else {
        return Ok(());
    };
    // With a shadow or a see-through node in it, composed tile by tile in
    // a buffer that can be read back (#10079, ADR-0027); else straight.
    if !has_effects(tree, area) {
        return paint(
            tree,
            &mut compose::Straight(target),
            theme,
            area,
            glyphs,
            live,
        );
    }
    for part in compose::tiles(area) {
        let mut tile = compose::Tile::new(part, theme.background);
        let Ok(()) = paint(tree, &mut tile, theme, part, glyphs, live);
        target.fill_contiguous(&to_eg(part), tile.pixels())?;
    }
    Ok(())
}

/// Whether drawing `area` meets a shadow or a see-through node.
fn has_effects<const N: usize, const T: usize>(tree: &Tree<N, T>, area: Rect) -> bool {
    let reach = i64::from(tree.scale().px(nowayos_ui::layer::SHADOW_REACH));
    tree.paint_order().any(|(_, node)| {
        let look = node.style().look;
        let rect = node.rect();
        (look.shadow.is_some()
            && Rect::from_edges(
                rect.left().saturating_sub(reach),
                rect.top().saturating_sub(reach),
                rect.right().saturating_add(reach),
                rect.bottom().saturating_add(reach),
            )
            .intersects(&area))
            || (look.is_translucent() && rect.intersects(&area))
    }) || tree.paint_order().any(|(id, node)| {
        // A node moving see-through, scaled or shimmering (#10086).
        let visual = tree.visual(id);
        visual.composes()
            && nowayos_ui::motion::drawn_area(node.rect(), visual, tree.scale(), 0)
                .intersects(&area)
    })
}

/// A see-through or shimmering group open while its subtree is drawn
/// (#10079, #10086).
#[derive(Clone, Copy)]
struct Group<C> {
    node: NodeId,
    opacity: u8,
    /// Where its shimmer's band is, and over what.
    shimmer: Option<(u16, Rect)>,
    /// The band's colour.
    light: C,
}

/// Finishes `group`: its shimmer's band over what it drew, then all of it
/// blended at its opacity.
fn finish<K: compose::Canvas>(target: &mut K, area: Rect, group: Group<K::Color>)
where
    K::Color: RgbColor + From<Rgb888>,
{
    if let Some((band, rect)) = group.shimmer {
        // A soft band a third of the box wide, crossing from before its
        // start to past its end.
        let width = i64::from(rect.size.width).max(1);
        let half = width / 6;
        let centre = rect.left().saturating_sub(half).saturating_add(
            (width.saturating_add(half.saturating_mul(2))).saturating_mul(i64::from(band)) / 1000,
        );
        target.shade(rect, group.light, |x, _| {
            let away = x.saturating_sub(centre).abs();
            if away >= half || half == 0 {
                return 0;
            }
            u8::try_from(
                half.saturating_sub(away)
                    .saturating_mul(160)
                    .checked_div(half)
                    .unwrap_or(0),
            )
            .unwrap_or(0)
        });
    }
    if group.opacity < u8::MAX {
        target.end(area, group.opacity);
    } else {
        target.end(area, u8::MAX);
    }
}

/// The sine and cosine of `tenths` of a degree, in 4096ths: a quarter turn
/// at a time exactly, and between them from a table every 15 degrees.
fn turn_sin_cos(tenths: i32) -> (i64, i64) {
    const SIN: [i64; 7] = [0, 1060, 2048, 2896, 3547, 3956, 4096];
    let degrees = i64::from(tenths / 10).rem_euclid(360);
    let sin_of = |d: i64| -> i64 {
        let d = d.rem_euclid(360);
        let (quarter, within) = (d / 90, d % 90);
        let first = |w: i64| {
            let at = usize::try_from(w / 15).unwrap_or(0);
            let (a, b) = (
                SIN.get(at).copied().unwrap_or(0),
                SIN.get(at.saturating_add(1)).copied().unwrap_or(4096),
            );
            a.saturating_add(b.saturating_sub(a).saturating_mul(w % 15) / 15)
        };
        match quarter {
            0 => first(within),
            1 => first(90_i64.saturating_sub(within)),
            2 => first(within).saturating_neg(),
            _ => first(90_i64.saturating_sub(within)).saturating_neg(),
        }
    };
    (sin_of(degrees), sin_of(degrees.saturating_add(90)))
}

/// `rect` scaled by `scale` thousandths about its centre.
fn scaled(rect: Rect, scale: u16) -> Rect {
    if scale == 1000 {
        return rect;
    }
    let grow = |length: u32| {
        (i64::from(length).saturating_mul(i64::from(scale)) / 1000)
            .saturating_sub(i64::from(length))
            / 2
    };
    let (gx, gy) = (grow(rect.size.width), grow(rect.size.height));
    Rect::from_edges(
        rect.left().saturating_sub(gx),
        rect.top().saturating_sub(gy),
        rect.right().saturating_add(gx),
        rect.bottom().saturating_add(gy),
    )
}

/// Where node `id` is moved to while it, or anything around it up to
/// `root`, moves (#10086): how far right and down, in pixels at `scale`,
/// and what it is cut to - each box around it scaled or partly revealed -
/// in its own coordinates.
fn moved<const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    id: NodeId,
    root: NodeId,
    scale: DeviceScale,
) -> ((i64, i64), Option<Rect>) {
    let px = |units: i32| scale.px_signed(i64::from(units));
    // The node and those around it, inner first.
    let mut path: [Option<NodeId>; nowayos_ui::MAX_DEPTH + 2] = [None; nowayos_ui::MAX_DEPTH + 2];
    let mut current = Some(id);
    for slot in &mut path {
        let Some(node) = current else {
            break;
        };
        *slot = Some(node);
        if node == root {
            break;
        }
        current = tree.get(node).and_then(Node::parent);
    }
    let total = path.iter().flatten().fold((0_i64, 0_i64), |sum, &node| {
        let (dx, dy) = tree.visual(node).offset;
        (sum.0.saturating_add(px(dx)), sum.1.saturating_add(px(dy)))
    });
    // Each box around it in its coordinates: moved by what lies between.
    let mut between = (0_i64, 0_i64);
    let mut clip: Option<Rect> = None;
    for &node in path.iter().flatten() {
        let visual = tree.visual(node);
        let Some(rect) = tree.get(node).map(Node::rect) else {
            continue;
        };
        let mut cut = None;
        if visual.reveal < 1000 {
            let height = u32::try_from(
                u64::from(rect.size.height).saturating_mul(u64::from(visual.reveal)) / 1000,
            )
            .unwrap_or(0);
            cut = Some(Rect::new(
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                height,
            ));
        }
        if visual.scale != 1000 && node != id {
            let box_ = scaled(rect, visual.scale);
            cut = Some(cut.map_or(box_, |c| c.intersection(&box_).unwrap_or(Rect::EMPTY)));
        }
        if let Some(cut) = cut {
            let cut = Rect::from_edges(
                cut.left().saturating_sub(between.0),
                cut.top().saturating_sub(between.1),
                cut.right().saturating_sub(between.0),
                cut.bottom().saturating_sub(between.1),
            );
            clip = Some(clip.map_or(cut, |c| c.intersection(&cut).unwrap_or(Rect::EMPTY)));
        }
        let (dx, dy) = visual.offset;
        between = (
            between.0.saturating_add(px(dx)),
            between.1.saturating_add(px(dy)),
        );
    }
    let _ = total;
    (between, clip)
}

/// Whether `id` is `ancestor` or inside it.
fn within<const N: usize, const T: usize>(tree: &Tree<N, T>, id: NodeId, ancestor: NodeId) -> bool {
    let mut current = Some(id);
    for _ in 0..=nowayos_ui::MAX_DEPTH.saturating_add(1) {
        match current {
            Some(node) if node == ancestor => return true,
            Some(node) => current = tree.get(node).and_then(Node::parent),
            None => return false,
        }
    }
    false
}

/// Draws `area` onto `canvas`, as [`render_area`] says: each node, its
/// shadow beneath it and a see-through node's subtree blended, where the
/// canvas can.
fn paint<K, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    target: &mut K,
    theme: &Theme<K::Color>,
    area: Rect,
    glyphs: &mut Glyphs<'_>,
    live: &dyn Live,
) -> Result<(), K::Error>
where
    K: compose::Canvas,
    K::Color: RgbColor + From<Rgb888>,
{
    let scale = tree.scale();
    let base = theme.metrics_at(scale);
    // Beneath an open dialog, every colour veiled (#10076).
    let veiled = theme.veiled();
    let theme_of = |plane: usize| {
        if tree.is_veiled(plane) {
            (&veiled, Some(theme.veil))
        } else {
            (theme, None)
        }
    };
    let (ground_theme, veil) = theme_of(0);
    Painter {
        target: &mut target.clipped(&to_eg(area)),
        theme: ground_theme,
        glyphs: &mut *glyphs,
        metrics: base,
        scale,
        font: ground_theme.type_scale.body,
        clip: area,
        ground: ground_theme.background,
        surface: None,
        veil,
        gradient: None,
        visual: Visual::REST,
        live,
    }
    .fill(area, ground_theme.background)?;
    // The screen, then each layer shown over it, bottom to top.
    for plane in 0..=nowayos_ui::layer::MAX_OPEN.saturating_add(1) {
        let Some(plane_root) = tree.plane(plane) else {
            break;
        };
        let (theme, veil) = theme_of(plane);
        // The see-through groups open around the node being drawn.
        let mut groups: [Option<Group<K::Color>>; compose::GROUPS] = [None; compose::GROUPS];
        for (id, node) in tree.plane_order(plane) {
            // Each group the node is outside of is finished.
            for slot in groups.iter_mut().rev() {
                if let Some(group) = *slot
                    && !within(tree, id, group.node)
                {
                    finish(target, area, group);
                    *slot = None;
                }
            }
            let look = node.style().look;
            // How it, and those around it, are drawn while they move.
            let visual = tree.visual(id);
            let (offset, moved) = moved(tree, id, plane_root, scale);
            let opacity = u8::try_from(
                u32::from(look.opacity.unwrap_or(u8::MAX))
                    .saturating_mul(u32::from(visual.opacity))
                    / 255,
            )
            .unwrap_or(u8::MAX);
            if (opacity < u8::MAX || visual.shimmer.is_some())
                && let Some(slot) = groups.iter_mut().find(|slot| slot.is_none())
                && target.begin()
            {
                let shifted = node.rect().checked_translate(
                    i32::try_from(offset.0).unwrap_or(0),
                    i32::try_from(offset.1).unwrap_or(0),
                );
                *slot = Some(Group {
                    node: id,
                    opacity,
                    shimmer: visual.shimmer.zip(shifted),
                    light: theme.progress_track,
                });
            }
            // Its shadow inside its group, so it fades with it.
            if let Some(elevation) = look.shadow {
                let clip = match tree.scroll_clip(node) {
                    Some(view) => area.intersection(&view),
                    None => Some(area),
                };
                if let Some(clip) = clip {
                    shadow(
                        target,
                        theme,
                        node,
                        theme.shadows.of(elevation),
                        clip,
                        tree.scale(),
                    );
                }
            }
            // The part of the node inside the area - and, inside a scroll
            // view, inside the view: everything it draws is clipped to
            // this, in NoWayOS's overflow-safe geometry first and by
            // `embedded-graphics` as well.
            // In the node's own coordinates, before it is moved.
            let local = Rect::from_edges(
                area.left().saturating_sub(offset.0),
                area.top().saturating_sub(offset.1),
                area.right().saturating_sub(offset.0),
                area.bottom().saturating_sub(offset.1),
            );
            let seen = match tree.scroll_clip(node) {
                Some(view) => local.intersection(&view),
                None => Some(local),
            };
            let seen = match moved {
                Some(moved) => seen.and_then(|seen| seen.intersection(&moved)),
                None => seen,
            };
            let own = scaled(node.rect(), visual.scale);
            if let Some(clip) = seen.and_then(|seen| own.intersection(&seen)) {
                // Its text in its own role and weight, measured as
                // layout measured it.
                let style = node.style().text;
                // What it stands on: the nearest container
                // drawn on a surface or with a fill of its own, or the
                // background - for a layer, the background.
                let (surface, fill) = if id == plane_root && plane > 0 {
                    (None, None)
                } else {
                    under(tree, node)
                };
                let shift = embedded_graphics::prelude::Point::new(
                    i32::try_from(offset.0).unwrap_or(0),
                    i32::try_from(offset.1).unwrap_or(0),
                );
                Painter {
                    target: &mut target.translated(shift).clipped(&to_eg(clip)),
                    theme,
                    glyphs: &mut *glyphs,
                    metrics: base.for_style(style),
                    scale,
                    font: theme.type_scale.font(style),
                    clip,
                    ground: fill.map_or_else(|| theme.ground(surface), |fill| theme.colour(fill)),
                    surface,
                    veil,
                    gradient: if id == plane_root && plane > 0 {
                        None
                    } else {
                        gradient_under(tree, node, theme)
                    },
                    visual,
                    live,
                }
                .node(tree, id, node)?;
            }
        }
        for group in groups.iter().rev().flatten() {
            finish(target, area, *group);
        }
        // Each scroll view's thumbs in the plane, over what it shows.
        let in_plane = |id: NodeId| match tree.layer_of(id) {
            Some(layer) => plane > 0 && layer == plane_root,
            None => plane == 0,
        };
        for id in tree.scroll_views().filter(|&id| in_plane(id)) {
            let Some(view) = tree.get(id) else {
                continue;
            };
            let seen = match tree.scroll_clip(view) {
                Some(outer) => view.content().intersection(&outer),
                None => Some(view.content()),
            };
            let Some(clip) = seen.and_then(|seen| seen.intersection(&area)) else {
                continue;
            };
            let bars = tree.scrollbars(id);
            let style = view.style();
            let (surface, fill) = if style.surface.is_some() || style.look.fill.is_some() {
                (style.surface, style.look.fill)
            } else {
                under(tree, view)
            };
            let mut painter = Painter {
                target: &mut target.clipped(&to_eg(clip)),
                theme,
                glyphs: &mut *glyphs,
                metrics: base,
                scale,
                font: theme.type_scale.body,
                clip,
                ground: fill.map_or_else(|| theme.ground(surface), |fill| theme.colour(fill)),
                surface,
                veil,
                gradient: None,
                visual: Visual::REST,
                live,
            };
            for bar in [bars.vertical, bars.horizontal].into_iter().flatten() {
                painter.thumb(bar.thumb)?;
            }
        }
    }
    Ok(())
}

/// The gradient `node` stands on (#10079): its nearest ancestor drawn on a
/// surface, a fill or a gradient, if that is a gradient.
pub(crate) fn gradient_under<C, const N: usize, const T: usize>(
    tree: &Tree<N, T>,
    node: &Node,
    theme: &Theme<C>,
) -> Option<shape::GradientFill<C>>
where
    C: RgbColor + From<Rgb888>,
{
    let mut next = node.parent();
    for _ in 0..=nowayos_ui::MAX_DEPTH {
        let parent = next.and_then(|id| tree.get(id))?;
        let style = parent.style();
        if let Some(gradient) = style.look.gradient {
            return Some(shape::GradientFill {
                from: theme.colour(gradient.from),
                to: theme.colour(gradient.to),
                area: parent.rect(),
                angle: match gradient.kind {
                    GradientKind::Linear { angle } => Some(angle),
                    GradientKind::Radial => None,
                },
            });
        }
        if style.surface.is_some() || style.look.fill.is_some() {
            return None;
        }
        next = parent.parent();
    }
    None
}

/// Draws `node`'s `shadow` beneath it, inside `clip`, at `scale`: toward
/// the theme's veil colour, falling smoothly from its rounded edge.
fn shadow<K>(
    target: &mut K,
    theme: &Theme<K::Color>,
    node: &Node,
    shadow: Shadow,
    clip: Rect,
    scale: DeviceScale,
) where
    K: compose::Canvas,
    K::Color: RgbColor + From<Rgb888>,
{
    let rect = node.rect();
    let (dy, spread, blur) = (
        i64::from(scale.px(shadow.dy)),
        i64::from(scale.px(shadow.spread)),
        scale.px(shadow.blur),
    );
    let shape = Rect::from_edges(
        rect.left().saturating_sub(spread),
        rect.top().saturating_add(dy).saturating_sub(spread),
        rect.right().saturating_add(spread),
        rect.bottom().saturating_add(dy).saturating_add(spread),
    );
    let style = node.style();
    let radius = scale
        .px(style.look.radius.unwrap_or_else(|| {
            style
                .surface
                .map_or(0, |surface| theme.shape.radius_of(surface))
        }))
        .saturating_add(u32::try_from(spread).unwrap_or(0));
    let reach = i64::from(blur);
    let extent = Rect::from_edges(
        shape.left().saturating_sub(reach),
        shape.top().saturating_sub(reach),
        shape.right().saturating_add(reach),
        shape.bottom().saturating_add(reach),
    );
    let Some(part) = extent.intersection(&clip) else {
        return;
    };
    let own = radius.saturating_sub(u32::try_from(spread).unwrap_or(0));
    target.shade(part, theme.veil, |x, y| {
        // None under the node's own rounded shape, which is drawn over it.
        if compose::outside(rect, own, x, y) <= 0 {
            return 0;
        }
        compose::falloff(compose::outside(shape, radius, x, y), blur, shadow.alpha)
    });
}

/// What `node` stands on: the surface and the fill of its
/// nearest ancestor drawn on a surface or with a fill of its own - a fill,
/// if it names one, is the colour beneath, and the surface still sets the
/// colours of text and controls - or neither, for the background.
pub(crate) fn under<const N: usize, const T: usize>(
    tree: &Tree<N, T>,
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
        // A layer stands on its own surface, over the screen (#10076).
        if let NodeKind::Layer { .. } = parent.kind() {
            return (style.surface, None);
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
    /// Its fill while the pointer hovers it (#10082), its label as at
    /// rest: a primary button half way to pressed, the others tinted with
    /// their label's colour.
    pub hover: C,
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
struct Painter<'t, 'th, 'g, 'c, D: DrawTarget> {
    target: &'t mut D,
    theme: &'th Theme<D::Color>,
    /// Where text's glyphs come from: drawn from the outlines, and kept.
    glyphs: &'g mut Glyphs<'c>,
    metrics: TextMetrics,
    /// The device scale: pixels each way for each layout unit.
    scale: DeviceScale,
    /// The font the node's text is drawn in: its style's, from the type
    /// scale. At an outline font, `metrics` holds its face and size at the
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
    /// The veil over the plane being drawn, beneath an open dialog: a
    /// picture's pixels are drawn half way to it, as the theme's colours
    /// are ([`Theme::veiled`]).
    veil: Option<D::Color>,
    /// The gradient the node stands on, if it does (#10079): its text is
    /// blended over the gradient's colour at each pixel.
    gradient: Option<shape::GradientFill<D::Color>>,
    /// How the node is drawn while it moves (#10086): its colours, its
    /// box's scale, an icon's turn, a progress bar's sweep.
    visual: Visual,
    /// The pixels of pictures decoded at run time (#10084).
    live: &'th dyn Live,
}

impl<D> Painter<'_, '_, '_, '_, D>
where
    D: DrawTarget,
    D::Color: RgbColor + From<Rgb888>,
{
    fn node<const N: usize, const T: usize>(
        &mut self,
        tree: &Tree<N, T>,
        id: NodeId,
        node: &Node,
    ) -> Result<(), D::Error> {
        // Its box scaled about its centre while it moves (#10086).
        let rect = scaled(node.rect(), self.visual.scale);
        let focused = tree.focused() == Some(id);
        // What the pointer hovers (#10082): this node, and which part.
        let hover = tree.hover();
        let hovered = hover.node == Some(id);
        let hover_part = hover.part.filter(|_| hovered).map(usize::from);
        // The focus ring only after keyboard use; a text input's
        // caret whenever it has focus.
        let ring = focused && tree.focus_visible();
        let content = node.content();
        match node.kind() {
            NodeKind::Column | NodeKind::Row | NodeKind::Stack | NodeKind::Scroll { .. } => {
                let (surface, look) = (node.style().surface, node.style().look);
                if look.draws() || self.visual.fill.is_some() {
                    self.looked(rect, surface, look)
                } else if let Some(surface) = surface {
                    self.surface(rect, surface)
                } else {
                    Ok(())
                }
            }
            NodeKind::Group { .. } | NodeKind::Switch { .. } | NodeKind::Spacer => Ok(()),
            // A layer over the screen, on its surface (#10076).
            NodeKind::Layer { .. } => match node.style().surface {
                Some(surface) => self.surface(rect, surface),
                None => Ok(()),
            },
            NodeKind::Progress { permille, .. } => self.progress(content, *permille),
            NodeKind::Icon(icon) => self.icon(content, icon),
            NodeKind::Image { picture, fit, .. } => self.picture(content, picture, *fit),
            // A picture decoded at run time, if its pixels are lent
            // (#10084): at its own pixels' size until fitted.
            NodeKind::Live { id, fit, .. } => match self.live.view(*id) {
                Some(view) => self.pixels(content, (view.width(), view.height()), view, *fit),
                None => Ok(()),
            },
            NodeKind::Text(text) => {
                self.selected(tree, id, content)?;
                self.text_lines(text, node, content)
            }
            NodeKind::Rich { runs, link, .. } => {
                let hovered_link = hover_part.and_then(|part| u8::try_from(part).ok());
                self.rich(runs, (*link, hovered_link), node, content, ring)
            }
            NodeKind::Slot { text, template, .. } => {
                self.selected(tree, id, content)?;
                match template {
                    Some(template) => self.text_lines(&template.around(text), node, content),
                    None => self.text_lines(text, node, content),
                }
            }
            NodeKind::TextArea { .. } => {
                self.text_area(tree, id, node, rect, content, focused, ring)
            }
            NodeKind::LongText { .. } => {
                self.long_text(tree.long_text_of(node).unwrap_or(""), node, content)
            }
            NodeKind::FieldError { message, .. } => match message {
                Some(message) => self.field_error(message, content),
                None => Ok(()),
            },
            NodeKind::Checkbox { label, checked, .. } => {
                if hovered {
                    self.hovered(rect)?;
                }
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
                    let ground = self.ground;
                    if hover_part == Some(index) {
                        self.hovered(row)?;
                    }
                    if let Some(mark) = self.choice_mark(row) {
                        let chosen = selected.is_some_and(|chosen| usize::from(chosen) == index);
                        self.radio(mark, chosen)?;
                    }
                    self.marked_text(option, row, self.ink())?;
                    self.ground = ground;
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
                    if hover_part == Some(row) {
                        self.hovered(area)?;
                    }
                    if let Some(mark) = self.choice_mark(area) {
                        let chosen = selected.is_some_and(|chosen| usize::from(chosen) == option);
                        self.radio(mark, chosen)?;
                    }
                    self.marked_text(label, area, self.theme.text)?;
                    self.ground = fill;
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
                    radius: Radii::all(radius),
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
                        radius: Radii::all(radius),
                        border: width,
                    };
                    let paint = Paint {
                        fill: self.ground,
                        border: self.focus_colour(),
                        ground: self.ground,
                    };
                    ring.draw(
                        &mut *self.target,
                        self.clip,
                        paint,
                        self.glyphs.rasteriser(),
                    )?;
                    let inset = width.saturating_add(self.px(FOCUS_GAP));
                    body = Rounded {
                        rect: rect.inset(Insets::uniform(inset)),
                        radius: Radii::all(radius.saturating_sub(inset)),
                        border: 0,
                    };
                }
                let (fill, label_colour, outline) = if tree.pressed() == Some(id) {
                    (colours.pressed, colours.pressed_label, None)
                } else if hovered && look.fill.is_none() && look.text.is_none() {
                    (colours.hover, colours.label, colours.outline)
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
                body.draw(
                    &mut *self.target,
                    self.clip,
                    paint,
                    self.glyphs.rasteriser(),
                )?;
                let shown = fit_start(label, content.size.width, &self.metrics);
                self.text(shown, content, TextAlign::Center, label_colour, fill)
            }
            NodeKind::TextInput {
                value,
                placeholder,
                caret,
                anchor,
                ..
            } => {
                let fill = self.theme.input;
                // A rounded body, its border drawn after the text.
                let radius = Radii::all(self.px(self.theme.shape.field_radius));
                let ground = self.ground;
                Rounded {
                    rect,
                    radius,
                    border: 0,
                }
                .draw(
                    &mut *self.target,
                    self.clip,
                    Paint {
                        fill,
                        border: fill,
                        ground,
                    },
                    self.glyphs.rasteriser(),
                )?;
                if value.is_empty() {
                    let shown = fit_start(placeholder, content.size.width, &self.metrics);
                    let hint = self.theme.placeholder;
                    self.text(shown, content, TextAlign::Start, hint, fill)?;
                }
                // The line drawn from as far left as keeps the caret in
                // view (`edit::window`), cut at the content's edges; what
                // is selected on the selection colour, its glyphs blended
                // over it.
                let text = value.as_str();
                let (low, high) = edit::selection(text, usize::from(*caret), usize::from(*anchor));
                let width_to = |at: usize| self.metrics.text_width(text.get(..at).unwrap_or(text));
                let to_caret = width_to(edit::boundary(text, usize::from(*caret)));
                let shift = edit::window(to_caret, content.size.width, self.px(edit::CARET_ROOM));
                let left = content.left().saturating_sub(i64::from(shift));
                let line = Rect::from_edges(left, content.top(), content.right(), content.bottom());
                let (from, to) = (
                    left.saturating_add(i64::from(width_to(low))),
                    left.saturating_add(i64::from(width_to(high))),
                );
                let selected = focused && high > low;
                let saved = self.clip;
                // Cut at the content's left edge only when the line starts
                // before it: unshifted, a glyph's overhang is kept.
                let edge = if shift > 0 {
                    content.left()
                } else {
                    saved.left()
                };
                // Unselected, the line is drawn in one part.
                let parts = if selected {
                    [
                        (edge, from, fill),
                        (from, to, self.theme.selection),
                        (to, content.right(), fill),
                    ]
                } else {
                    [(edge, content.right(), fill); 3]
                };
                let count = if selected { 3 } else { 1 };
                for &(start, end, ground) in parts.iter().take(count) {
                    let part = Rect::from_edges(
                        start.max(edge),
                        content.top(),
                        end.min(content.right()),
                        content.bottom(),
                    );
                    let Some(clip) = part.intersection(&saved) else {
                        continue;
                    };
                    if !selected && ground != fill {
                        continue;
                    }
                    self.clip = clip;
                    if ground != fill {
                        self.fill(part, ground)?;
                    }
                    self.text(
                        text,
                        line,
                        TextAlign::Start,
                        self.theme.text,
                        if selected { ground } else { fill },
                    )?;
                }
                self.clip = saved;
                let shown = text;
                // The border or focus ring after the text, over any edge of
                // a glyph that reaches it: text is blended with the fill
                // and is only right where the fill is.
                let (border, colour) = if ring {
                    (self.px(self.theme.focus_width), self.theme.focus)
                } else if hovered {
                    (
                        self.px(INPUT_BORDER.saturating_mul(2)),
                        self.theme.input_border,
                    )
                } else {
                    (self.px(INPUT_BORDER), self.theme.input_border)
                };
                Rounded {
                    rect,
                    radius,
                    border,
                }
                .draw_inside(
                    &mut *self.target,
                    self.clip,
                    Paint {
                        fill,
                        border: colour,
                        ground,
                    },
                    Inside::Keep,
                    self.glyphs.rasteriser(),
                )?;
                if !focused {
                    return Ok(());
                }
                let _ = shown;
                let advance = to_caret.saturating_sub(shift);
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
    fn keyboard<const N: usize, const T: usize>(
        &mut self,
        tree: &Tree<N, T>,
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
                radius: Radii::all(radius),
                border,
            }
            .draw(
                &mut *self.target,
                self.clip,
                paint,
                self.glyphs.rasteriser(),
            )?;
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
                    glyphs: &mut *self.glyphs,
                    metrics: self.metrics,
                    scale: self.scale,
                    font: self.font,
                    clip: inside,
                    ground: self.ground,
                    surface: self.surface,
                    veil: self.veil,
                    gradient: self.gradient,
                    visual: Visual::REST,
                    live: self.live,
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
    /// square as high as the text's em - its size, 18 pixels in the light
    /// theme and 24 in the large-text theme at scale 1 - centred in the mark's
    /// place ([`Self::mark`]), a line high. So the background shows all
    /// round it even where a row is only a line high, as with the atlases:
    /// between one option's ring and the next, and between the mark and a
    /// focus ring. In a bitmap font, which has no em, it fills its
    /// place, as it always has.
    fn choice_mark(&self, area: Rect) -> Option<Rect> {
        let place = self.mark(area)?;
        let Some((_, size64)) = self.metrics.face() else {
            return Some(place);
        };
        let side = size64
            .saturating_add(32)
            .checked_div(64)
            .unwrap_or(0)
            .min(place.size.height);
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
        let (ink, ground) = (self.ink(), self.ground);
        // A box with softly rounded corners, its inside left as it is.
        Rounded {
            rect: mark,
            radius: Radii::all(side / 6),
            border: line_weight(side),
        }
        .draw_inside(
            &mut *self.target,
            self.clip,
            Paint {
                fill: ground,
                border: ink,
                ground,
            },
            Inside::Keep,
            self.glyphs.rasteriser(),
        )?;
        if !checked {
            return Ok(());
        }
        // The tick: two strokes a fifth of the side wide, from 2/10 across
        // and 5/10 down to 4/10 and 7/10, then up to 8/10 and 3/10; each
        // runs on half its width past the corner, so the corner is full.
        let at = |tenths_x: i64, tenths_y: i64| {
            let side = i64::from(side).saturating_mul(256);
            (
                mark.left()
                    .saturating_mul(256)
                    .saturating_add(side.saturating_mul(tenths_x) / 10),
                mark.top()
                    .saturating_mul(256)
                    .saturating_add(side.saturating_mul(tenths_y) / 10),
            )
        };
        let width = i64::from((side / 5).max(1)).saturating_mul(256);
        let (start, corner, end) = (at(2, 5), at(4, 7), at(8, 3));
        let [a, b, c, d] = stroke(start, corner, width, false, true);
        let [e, f, g, h] = stroke(corner, end, width, true, false);
        let points = [
            Some(a),
            Some(b),
            Some(c),
            Some(d),
            None,
            Some(e),
            Some(f),
            Some(g),
            Some(h),
            None,
        ];
        fill_polygon(
            &mut *self.target,
            self.clip,
            &points,
            ink,
            ground,
            self.glyphs.rasteriser(),
        )
    }

    /// A radio option's ring in `mark`, with a dot in it if `chosen`: the
    /// choice is shown by the dot, not by colour.
    fn radio(&mut self, mark: Rect, chosen: bool) -> Result<(), D::Error> {
        let side = mark.size.width;
        let (ink, ground) = (self.ink(), self.ground);
        Rounded {
            rect: mark,
            radius: Radii::all(side / 2),
            border: line_weight(side),
        }
        .draw_inside(
            &mut *self.target,
            self.clip,
            Paint {
                fill: ground,
                border: ink,
                ground,
            },
            Inside::Keep,
            self.glyphs.rasteriser(),
        )?;
        if chosen {
            // Half the ring's size, with an even margin all round.
            let mut dot = side / 2;
            if !side.saturating_sub(dot).is_multiple_of(2) {
                dot = dot.saturating_sub(1);
            }
            let inset = i32::try_from(side.saturating_sub(dot) / 2).unwrap_or(0);
            if let Some(rect) =
                Rect::new(mark.origin.x, mark.origin.y, dot, dot).checked_translate(inset, inset)
            {
                Rounded {
                    rect,
                    radius: Radii::all(dot / 2),
                    border: 0,
                }
                .draw(
                    &mut *self.target,
                    self.clip,
                    Paint {
                        fill: ink,
                        border: ink,
                        ground,
                    },
                    self.glyphs.rasteriser(),
                )?;
            }
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
    /// Selectable text a press has selected (#10080): its box on the
    /// selection colour, its text blended over it.
    fn selected<const N: usize, const T: usize>(
        &mut self,
        tree: &Tree<N, T>,
        id: NodeId,
        content: Rect,
    ) -> Result<(), D::Error> {
        if tree.selected_text() != Some(id) {
            return Ok(());
        }
        self.ground = self.theme.selection;
        self.gradient = None;
        self.fill(content, self.theme.selection)
    }

    /// A hovered option or row (#10082): `area` on the hover tint, what
    /// is drawn on it next blended over that. The caller puts the ground
    /// back.
    fn hovered(&mut self, area: Rect) -> Result<(), D::Error> {
        let tint = self.theme.hover_over(self.ground);
        self.fill(area, tint)?;
        self.ground = tint;
        Ok(())
    }

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

    /// A text area: a text input's body, its lines from the first that
    /// shows, `rows` of them, each a line high; what is selected on the
    /// selection colour; its caret, while focused; its border or focus ring
    /// last.
    #[allow(
        clippy::too_many_arguments,
        reason = "the node's geometry and focus, worked out by the caller, passed whole"
    )]
    fn text_area<const N: usize, const T: usize>(
        &mut self,
        tree: &Tree<N, T>,
        id: NodeId,
        node: &Node,
        rect: Rect,
        content: Rect,
        focused: bool,
        ring: bool,
    ) -> Result<(), D::Error> {
        let NodeKind::TextArea {
            placeholder,
            caret,
            anchor,
            rows,
            top,
            ..
        } = *node.kind()
        else {
            return Ok(());
        };
        let fill = self.theme.input;
        let radius = Radii::all(self.px(self.theme.shape.field_radius));
        let ground = self.ground;
        Rounded {
            rect,
            radius,
            border: 0,
        }
        .draw(
            &mut *self.target,
            self.clip,
            Paint {
                fill,
                border: fill,
                ground,
            },
            self.glyphs.rasteriser(),
        )?;
        let text = tree.area_text(id).unwrap_or("");
        let metrics = self.metrics;
        let height = i64::from(metrics.line_height());
        if text.is_empty() {
            let line = Rect::from_edges(
                content.left(),
                content.top(),
                content.right(),
                content.top().saturating_add(height),
            );
            let shown = fit_start(placeholder, content.size.width, &metrics);
            let hint = self.theme.placeholder;
            self.text(shown, line, TextAlign::Start, hint, fill)?;
        }
        let at = |offset: u32| edit::boundary(text, usize::try_from(offset).unwrap_or(0));
        let (low, high) = edit::selection(text, at(caret), at(anchor));
        let caret_at = at(caret);
        let saved = self.clip;
        let width = content.size.width;
        let lines = nowayos_ui::area::line_spans(text, width, &metrics)
            .skip(usize::try_from(top).unwrap_or(0))
            .take(usize::from(rows));
        let mut caret_rect = None;
        for (index, (start, end)) in lines.enumerate() {
            let y = content
                .top()
                .saturating_add(i64::try_from(index).unwrap_or(0).saturating_mul(height));
            let line_rect =
                Rect::from_edges(content.left(), y, content.right(), y.saturating_add(height));
            let line = text.get(start..end).unwrap_or("");
            let x_of = |offset: usize| {
                content.left().saturating_add(i64::from(
                    metrics.text_width(text.get(start..offset.clamp(start, end)).unwrap_or("")),
                ))
            };
            // The part of this line that is selected.
            let (from, to) = (low.max(start), high.min(end));
            let selected = focused && from < to;
            let (sx, ex) = (x_of(from), x_of(to));
            let parts = if selected {
                [
                    (content.left(), sx, fill),
                    (sx, ex, self.theme.selection),
                    (ex, content.right(), fill),
                ]
            } else {
                [(content.left(), content.right(), fill); 3]
            };
            let count = if selected { 3 } else { 1 };
            for &(left, right, under) in parts.iter().take(count) {
                if !selected && under != fill {
                    continue;
                }
                let part = Rect::from_edges(left, y, right, y.saturating_add(height));
                let Some(clip) = part.intersection(&saved) else {
                    continue;
                };
                self.clip = clip;
                if under != fill {
                    self.fill(part, under)?;
                }
                let behind = if selected { under } else { fill };
                self.text(line, line_rect, TextAlign::Start, self.theme.text, behind)?;
            }
            self.clip = saved;
            if (start..=end).contains(&caret_at) && caret_rect.is_none() {
                let x = x_of(caret_at);
                caret_rect = Some(Rect::from_edges(
                    x,
                    y,
                    x.saturating_add(i64::from(self.px(CARET_WIDTH))),
                    y.saturating_add(height),
                ));
            }
        }
        let (border, colour) = if ring {
            (self.px(self.theme.focus_width), self.theme.focus)
        } else if tree.hover().node == Some(id) {
            (
                self.px(INPUT_BORDER.saturating_mul(2)),
                self.theme.input_border,
            )
        } else {
            (self.px(INPUT_BORDER), self.theme.input_border)
        };
        Rounded {
            rect,
            radius,
            border,
        }
        .draw_inside(
            &mut *self.target,
            self.clip,
            Paint {
                fill,
                border: colour,
                ground,
            },
            Inside::Keep,
            self.glyphs.rasteriser(),
        )?;
        match caret_rect {
            Some(caret) if focused => self.fill(caret, self.theme.focus),
            _ => Ok(()),
        }
    }

    /// A long text's paragraphs in `content`, wrapped at its width, each
    /// line a line high from its top and set across it as its style aligns
    /// it; only the lines that meet the area being drawn are drawn.
    fn long_text(&mut self, text: &str, node: &Node, content: Rect) -> Result<(), D::Error> {
        let (on, colour) = (self.ground, self.text_colour(node));
        let align = node.style().text.align;
        let metrics = self.metrics;
        let width = content.size.width;
        let height = i64::from(metrics.line_height());
        let (from, to) = (self.clip.top(), self.clip.bottom());
        let mut top = content.top();
        for line in nowayos_ui::long_text::lines(text, width, &metrics) {
            let bottom = top.saturating_add(height);
            if top >= to {
                break;
            }
            if bottom > from {
                let rect = Rect::from_edges(content.left(), top, content.right(), bottom);
                let shown = fit_start(line, width, &metrics);
                self.text(shown, rect, aligned(shown, line, align), colour, on)?;
            }
            top = bottom;
        }
        Ok(())
    }

    fn text_colour(&self, node: &Node) -> D::Color {
        // A text changing colour (#10086).
        if let Some((from, to, t)) = self.visual.text {
            return self.mixed(from, to, t);
        }
        let style = node.style().text;
        style.colour.map_or_else(
            || self.theme.text_on(self.surface, style.role),
            |colour| self.theme.colour(colour),
        )
    }

    /// `t` thousandths of the way from the theme's `from` to its `to`.
    fn mixed(&self, from: Colour, to: Colour, t: u16) -> D::Color {
        let coverage = u8::try_from(u32::from(t.min(1000)) * 255 / 1000).unwrap_or(u8::MAX);
        text::blend(self.theme.colour(to), self.theme.colour(from), coverage)
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
            radius: Radii::all(self.px(self.theme.shape.radius_of(surface))),
            border: self.px(self.theme.shape.surface_border),
        };
        let paint = Paint {
            fill: colours.fill,
            border: colours.border,
            ground: self.ground,
        };
        shape.draw(
            &mut *self.target,
            self.clip,
            paint,
            self.glyphs.rasteriser(),
        )
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
        // A fill changing colour (#10086).
        let fill = match self.visual.fill {
            Some((from, to, t)) => self.mixed(from, to, t),
            None => fill,
        };
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
            radius: Radii::all(self.px(look.radius.unwrap_or(radius))),
            border: self.px(width),
        };
        let paint = Paint {
            fill,
            border,
            ground: self.ground,
        };
        // A gradient in place of the fill (#10079).
        if let Some(gradient) = look.gradient {
            let fill = shape::GradientFill {
                from: self.theme.colour(gradient.from),
                to: self.theme.colour(gradient.to),
                area: rect,
                angle: match gradient.kind {
                    GradientKind::Linear { angle } => Some(angle),
                    GradientKind::Radial => None,
                },
            };
            return shape.draw_gradient(
                &mut *self.target,
                self.clip,
                paint,
                self.glyphs.rasteriser(),
                fill,
            );
        }
        shape.draw(
            &mut *self.target,
            self.clip,
            paint,
            self.glyphs.rasteriser(),
        )
    }

    /// An icon in `rect`: its mask at the device scale, centred,
    /// each pixel's coverage blending the colour of text where it stands
    /// over what lies beneath, clipped.
    fn icon(&mut self, rect: Rect, icon: &nowayos_ui::icons::Icon) -> Result<(), D::Error> {
        let Some(mask) = icon.mask(self.scale.quarters()) else {
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
        // Turned about its centre while it spins (#10086): each pixel
        // takes the mask's nearest under it, turned back.
        let (sin, cos) = turn_sin_cos(self.visual.turn);
        let half = i64::from(size) / 2;
        let pixels = (0..size).flat_map(move |y| {
            (0..size).filter_map(move |x| {
                let (sx, sy) = if sin == 0 && cos == 4096 {
                    (x, y)
                } else {
                    let (dx, dy) = (
                        i64::from(x).saturating_sub(half),
                        i64::from(y).saturating_sub(half),
                    );
                    let rx = (dx
                        .saturating_mul(cos)
                        .saturating_add(dy.saturating_mul(sin)))
                        / 4096;
                    let ry = (dy
                        .saturating_mul(cos)
                        .saturating_sub(dx.saturating_mul(sin)))
                        / 4096;
                    (
                        u32::try_from(rx.saturating_add(half)).ok()?,
                        u32::try_from(ry.saturating_add(half)).ok()?,
                    )
                };
                let coverage = mask.at(sx, sy).filter(|&c| c > 0)?;
                let px = left.saturating_add(i64::from(x));
                let py = top.saturating_add(i64::from(y));
                let point = Point::new(i32::try_from(px).ok()?, i32::try_from(py).ok()?);
                clip.contains(nowayos_display::Point::new(point.x, point.y))
                    .then(|| Pixel(point, text::blend(ink, ground, coverage)))
            })
        });
        self.target.draw_iter(pixels)
    }

    /// An indeterminate progress bar: its track, and a segment a third of
    /// its width `along` thousandths of the way along.
    fn sweep(&mut self, rect: Rect, along: u16) -> Result<(), D::Error> {
        let (fill, track) = self.theme.progress_on(self.surface);
        let radius = rect.size.height / 2;
        let paint = |fill: D::Color, ground: D::Color| Paint {
            fill,
            border: fill,
            ground,
        };
        Rounded {
            rect,
            radius: Radii::all(radius),
            border: 0,
        }
        .draw(
            &mut *self.target,
            self.clip,
            paint(track, self.ground),
            self.glyphs.rasteriser(),
        )?;
        let width = rect.size.width / 3;
        let room = rect.size.width.saturating_sub(width);
        let left = rect.left().saturating_add(
            (i64::from(room).saturating_mul(i64::from(along.min(1000))) / 1000)
                .min(i64::from(room)),
        );
        Rounded {
            rect: Rect::from_edges(
                left,
                rect.top(),
                left.saturating_add(i64::from(width)),
                rect.bottom(),
            ),
            radius: Radii::all(radius),
            border: 0,
        }
        .draw(
            &mut *self.target,
            self.clip,
            paint(fill, track),
            self.glyphs.rasteriser(),
        )
    }

    /// A picture in `rect`: fitted by `fit`, sampled from the pixels it was
    /// made with nearest its drawn size ([`Picture::best`]), each blended
    /// by its alpha over what lies beneath, clipped to `rect`.
    fn picture(&mut self, rect: Rect, picture: &Picture, fit: Fit) -> Result<(), D::Error> {
        let natural = (self.px(picture.width()), self.px(picture.height()));
        let placed = fit.place(natural, (rect.size.width, rect.size.height));
        let pixels = picture.best(placed.width, placed.height).view();
        self.pixels(rect, natural, pixels, fit)
    }

    /// `pixels`, whose own size at the scale is `natural`, fitted into
    /// `rect` by `fit`, blended over the ground: a picture, or one decoded
    /// at run time (#10084).
    fn pixels(
        &mut self,
        rect: Rect,
        natural: (u32, u32),
        pixels: nowayos_ui::pictures::View<'_>,
        fit: Fit,
    ) -> Result<(), D::Error> {
        let placed = fit.place(natural, (rect.size.width, rect.size.height));
        let left = rect.left().saturating_add(placed.left);
        let top = rect.top().saturating_add(placed.top);
        let drawn = Rect::from_edges(
            left,
            top,
            left.saturating_add(i64::from(placed.width)),
            top.saturating_add(i64::from(placed.height)),
        );
        let Some(visible) = drawn
            .intersection(&rect)
            .and_then(|drawn| drawn.intersection(&self.clip))
        else {
            return Ok(());
        };
        let (ground, veil) = (self.ground, self.veil);
        let offset = |from: i64, to: i64| u32::try_from(from.saturating_sub(to)).unwrap_or(0);
        let (dx, dy) = (offset(visible.left(), left), offset(visible.top(), top));
        let (width, height) = (visible.size.width, visible.size.height);
        let colours = (0..height).flat_map(move |y| {
            (0..width).map(move |x| {
                let rgba = pixels.sample(
                    dx.saturating_add(x),
                    dy.saturating_add(y),
                    placed.width,
                    placed.height,
                );
                let colour = picture::over(rgba, ground);
                veil.map_or(colour, |veil| text::blend(veil, colour, VEIL))
            })
        });
        self.target.fill_contiguous(&to_eg(visible), colours)
    }

    /// A scrollbar's thumb in `rect`: rounded at each end, in the
    /// scrollbar's colour where its view stands.
    fn thumb(&mut self, rect: Rect) -> Result<(), D::Error> {
        let colour = self.theme.scrollbar_on(self.surface);
        let radius = rect.size.width.min(rect.size.height) / 2;
        Rounded {
            rect,
            radius: Radii::all(radius),
            border: 0,
        }
        .draw(
            &mut *self.target,
            self.clip,
            Paint {
                fill: colour,
                border: colour,
                ground: self.ground,
            },
            self.glyphs.rasteriser(),
        )
    }

    /// A progress bar in `rect`: a rounded track, and over it, from
    /// the start, a rounded fill `permille` thousandths of its width.
    fn progress(&mut self, rect: Rect, permille: u16) -> Result<(), D::Error> {
        // Indeterminate (#10086): a third of the bar sweeping along.
        if let Some(along) = self.visual.sweep {
            return self.sweep(rect, along);
        }
        let (fill, track) = self.theme.progress_on(self.surface);
        let radius = rect.size.height / 2;
        let bar = Rounded {
            rect,
            radius: Radii::all(radius),
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
            self.glyphs.rasteriser(),
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
            radius: Radii::all(radius),
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
                    self.glyphs.rasteriser(),
                )?;
            }
        }
        Ok(())
    }

    /// `units` layout units in pixels at the device scale, to the nearest
    /// whole pixel.
    const fn px(&self, units: u32) -> u32 {
        self.scale.px(units)
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

    /// A rich text's runs (#10078), line by line inside `content`: each
    /// in its own weight and colour, italics slanted, code on its tint,
    /// links underlined, and the focused link ringed after a key.
    fn rich(
        &mut self,
        runs: &[Run],
        (link, hovered): (u8, Option<u8>),
        node: &Node,
        content: Rect,
        ring: bool,
    ) -> Result<(), D::Error> {
        let style = node.style().text;
        let (base, font, ground) = (self.metrics, self.font, self.ground);
        let height = i64::from(base.line_height());
        let theme = self.theme;
        let mut top = content.top();
        for line in nowayos_ui::rich::lines(runs, content.size.width, style, &base) {
            let row_top = top;
            top = top.saturating_add(height);
            if row_top >= self.clip.bottom() {
                break;
            }
            if top <= self.clip.top() {
                continue;
            }
            for piece in nowayos_ui::rich::pieces(runs, line, style, &base) {
                let Some(run) = runs.get(piece.run) else {
                    continue;
                };
                let rect = Rect::from_edges(
                    content.left().saturating_add(i64::from(piece.x)),
                    row_top,
                    content
                        .left()
                        .saturating_add(i64::from(piece.x))
                        .saturating_add(i64::from(piece.width)),
                    top,
                );
                let on = if run.code {
                    self.fill(rect, theme.code)?;
                    theme.code
                } else {
                    ground
                };
                let linked = nowayos_ui::rich::link_of(runs, piece.run);
                let colour = match (run.colour, linked) {
                    (Some(colour), _) => theme.colour(colour),
                    (None, Some(_)) => theme.link_on(self.surface),
                    (None, None) => style.colour.map_or_else(
                        || theme.text_on(self.surface, style.role),
                        |colour| theme.colour(colour),
                    ),
                };
                let run_style = run.style_in(style);
                self.metrics = base.for_style(run_style);
                self.font = theme.type_scale.font(run_style);
                self.text_slanted(piece.text, rect, TextAlign::Start, colour, on, run.italic)?;
                if linked.is_some() {
                    // Underlined, a little below the baseline; twice as
                    // thick while the pointer hovers it (#10082).
                    let thick = if linked == hovered {
                        self.px(2).max(2)
                    } else {
                        self.px(1).max(1)
                    };
                    let below = rect.bottom().saturating_sub(i64::from(self.px(3)));
                    let underline = Rect::from_edges(
                        rect.left(),
                        below,
                        rect.left().saturating_add(i64::from(
                            self.metrics.text_width(piece.text.trim_end_matches(' ')),
                        )),
                        below.saturating_add(i64::from(thick)),
                    );
                    self.fill(underline, colour)?;
                }
                if ring && linked == Some(link) {
                    self.frame(
                        rect,
                        self.px(self.theme.focus_width),
                        self.theme.focus_on(self.surface),
                    )?;
                }
            }
        }
        self.metrics = base;
        self.font = font;
        Ok(())
    }

    /// Draws one line of text in `rect`, vertically centred, clipped, in
    /// `color` - from the face's outlines blended over `background`, the colour
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
        self.text_slanted(text, rect, align, color, background, false)
    }

    /// As [`text`](Self::text), slanted as an oblique if `slanted`.
    fn text_slanted(
        &mut self,
        text: &str,
        rect: Rect,
        align: TextAlign,
        color: D::Color,
        background: D::Color,
        slanted: bool,
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
        match (self.metrics.face(), self.font) {
            (Some((face, size64)), _) => {
                let line = text::Line {
                    face,
                    size64,
                    text,
                    left,
                    baseline: top.saturating_add(text::baseline_in(
                        face,
                        size64,
                        self.metrics.line_height(),
                    )),
                    slanted,
                };
                text::draw(
                    &mut *self.target,
                    line,
                    self.clip,
                    color,
                    background,
                    self.glyphs,
                    self.gradient,
                )
            }
            (None, Font::Mono(font)) => self.mono_text(font, text, left, top, width, color),
            (None, Font::Outline(..)) => Ok(()),
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

/// How much of the button colour tints what the pointer hovers (#10082),
/// in 255ths: an option, a list row; a secondary or quiet button as much as
/// its label's contrast allows ([`hover_tint`]).
const HOVER_TINT: u8 = 24;

/// A secondary or quiet button's hover fill: `brand` over `base` as
/// strongly as keeps `label` at 4.5:1 on it - as specified, and as a 16-bit
/// screen shows it - [`HOVER_TINT`], 16 or 8 255ths; `base` itself if none
/// does, for a theme whose label only just meets the minimum there.
fn hover_tint<C: RgbColor + From<Rgb888>>(brand: C, base: C, label: C) -> C {
    let label = eight_bit(label);
    for tint in [HOVER_TINT, 16, 8] {
        let fill = text::blend(brand, base, tint);
        let shown = eight_bit(fill);
        if contrast::ratio(label, shown) >= 4.5
            && contrast::ratio(contrast::as_shown(label), contrast::as_shown(shown)) >= 4.5
        {
            return fill;
        }
    }
    base
}

/// `colour` at 8 bits a channel, to weigh its contrast.
fn eight_bit<C: RgbColor>(colour: C) -> Rgb888 {
    let widen = |value: u8, max: u8| {
        u16::from(value)
            .saturating_mul(255)
            .checked_div(u16::from(max))
            .and_then(|wide| u8::try_from(wide).ok())
            .unwrap_or(u8::MAX)
    };
    Rgb888::new(
        widen(colour.r(), C::MAX_R),
        widen(colour.g(), C::MAX_G),
        widen(colour.b(), C::MAX_B),
    )
}

/// The corner radius, in layout units, of a key of the on-screen keyboard.
const KEY_RADIUS: u32 = 6;

/// Width, in layout units, of a text input's caret.
const CARET_WIDTH: u32 = 1;

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

/// The four corners of a stroke `width` wide (1/256 pixel) from `from` to
/// `to`, in order round it, run on half its width past `from` or `to` where
/// asked, so two strokes meeting there leave no notch at the corner.
fn stroke(
    from: (i64, i64),
    to: (i64, i64),
    width: i64,
    extend_from: bool,
    extend_to: bool,
) -> [(i64, i64); 4] {
    let (dx, dy) = (to.0.saturating_sub(from.0), to.1.saturating_sub(from.1));
    let length = i64::try_from(
        u64::try_from(dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)))
            .unwrap_or(0)
            .isqrt(),
    )
    .unwrap_or(0)
    .max(1);
    let half = width / 2;
    // Half the width across the stroke, and along it.
    let per_length = |value: i64| value.checked_div(length).unwrap_or(0);
    let (nx, ny) = (
        per_length(dy.saturating_neg().saturating_mul(half)),
        per_length(dx.saturating_mul(half)),
    );
    let (ax, ay) = (
        per_length(dx.saturating_mul(half)),
        per_length(dy.saturating_mul(half)),
    );
    let start = if extend_from {
        (from.0.saturating_sub(ax), from.1.saturating_sub(ay))
    } else {
        from
    };
    let end = if extend_to {
        (to.0.saturating_add(ax), to.1.saturating_add(ay))
    } else {
        to
    };
    [
        (start.0.saturating_add(nx), start.1.saturating_add(ny)),
        (end.0.saturating_add(nx), end.1.saturating_add(ny)),
        (end.0.saturating_sub(nx), end.1.saturating_sub(ny)),
        (start.0.saturating_sub(nx), start.1.saturating_sub(ny)),
    ]
}

/// The width of the lines of a mark `side` pixels across: a tenth of it, at
/// least one pixel.
fn line_weight(side: u32) -> u32 {
    (side / 10).max(1)
}

/// Offset that centres a line of `line_height` in `rect`.
fn vertical_offset(rect: Rect, line_height: u32) -> i32 {
    i32::try_from(rect.size.height.saturating_sub(line_height) / 2).unwrap_or(0)
}
