// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layout: a measure pass (natural sizes, bottom-up) then an arrange pass
//! (positions, top-down). Both recurse at most [`MAX_DEPTH`] deep and use
//! saturating arithmetic throughout, so no tree can make layout panic or
//! wrap.
//!
//! The measure pass also hands each node the width it may take - the
//! viewport's, less each container's padding, or a fixed width - so a text
//! set on several lines is measured at its width first, then given
//! the height of the lines its text takes there ([`crate::text::wrap`]). In
//! a row, children that wrap share the width the others leave.

use core::fmt;
use core::hash::{Hash, Hasher};

use nowayos_display::{DeviceScale, Rect, Size};
use nowayos_glyphs::{Atlas, Face};

use crate::node::INPUT_WIDTH_CHARS;
use crate::tree::MAX_DEPTH;
use crate::{
    Align, Justify, Length, Node, NodeId, NodeKind, SpaceTokens, Spacing, Style, TextStyle, Tree,
    UiError,
};

/// The largest viewport width or height [`Tree::layout`] accepts, in
/// pixels: comfortably more than an 8K display.
pub const MAX_VIEWPORT_SIZE: u32 = 16_384;

/// How far from the origin, in pixels, the edges of a viewport may lie.
/// Keeps every coordinate a renderer draws at far from the ends of the
/// `i32` range that `embedded-graphics` computes in.
pub const MAX_VIEWPORT_OFFSET: i32 = 1 << 24;

/// Glyph advances and kerning come in 64ths of a pixel.
const UNITS_PER_PIXEL: u32 = 64;

/// How big text is, and how big a control must be: what layout needs from
/// a theme.
///
/// Text is measured from a face's outlines at a size in 1/64 pixel per em
/// ([`TextMetrics::from_face`], or [`TextMetrics::from_atlas`] for the size
/// an atlas was drawn at): each character's own advance and the font's pair
/// kerning, scaled to 1/64 pixel as the atlases were, and the face's line
/// height. Or, the special case, from a built-in monospace
/// font ([`TextMetrics::new`]): every character the same advance and no
/// kerning. The same code measures both - [`text_width`](Self::text_width),
/// [`text::fit_start`](crate::text::fit_start) and
/// [`text::fit_end`](crate::text::fit_end) - so layout and drawing measure
/// alike when both use the metrics the theme gives.
///
/// ```
/// use nowayos_glyphs::REGULAR_24;
/// use nowayos_ui::TextMetrics;
///
/// let atlas = TextMetrics::from_atlas(&REGULAR_24);
/// // T advances 857/64 pixel and o 842/64, and the pair is kerned by
/// // -123/64: 1,576/64 = 24.6 pixels, rounded up.
/// assert_eq!(atlas.text_width("To"), 25);
/// assert_eq!(atlas.line_height(), 32);
///
/// let mono = TextMetrics::new(6, 10);
/// assert_eq!(mono.text_width("To"), 12);
/// ```
///
/// Provisional: this API may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextMetrics {
    char_width: u32,
    line_height: u32,
    min_target: u32,
    /// The least height of a button, in pixels: a theme's token. 0
    /// leaves buttons at the minimum target size.
    button_height: u32,
    scale: DeviceScale,
    face: Option<FaceRef>,
    /// The face and size of each text style, by [`TextStyle::index`]: what
    /// text in that role and weight is measured with. A style with none is
    /// measured with `face`, the body text's.
    faces: [Option<FaceRef>; TextStyle::COUNT],
    /// The line height of each style, in pixels, where the theme sets one;
    /// 0 for its atlas's own.
    lines: [u32; TextStyle::COUNT],
    /// What each spacing token measures, in layout units: the theme's.
    spacing: Spacing,
}

impl TextMetrics {
    /// Metrics for a built-in monospace font: every character advances
    /// `char_width` pixels, nothing is kerned, and a line is `line_height`
    /// pixels high. [`MIN_TARGET`](crate::MIN_TARGET) is the minimum
    /// target size.
    #[must_use]
    pub const fn new(char_width: u32, line_height: u32) -> Self {
        Self {
            char_width,
            line_height,
            min_target: crate::MIN_TARGET,
            button_height: 0,
            scale: DeviceScale::ONE,
            face: None,
            faces: [None; TextStyle::COUNT],
            lines: [0; TextStyle::COUNT],
            spacing: Spacing::STANDARD,
        }
    }

    /// Metrics measured from `face` at `size64` 1/64 pixel per em: each
    /// character's advance and the font's pair kerning, and the face's line
    /// height - its ascent, descent and line gap together, rounded up
    /// ([`nowayos_glyphs::LineMetrics::line_height`]). A character's width
    /// ([`char_width`](Self::char_width)) is the advance of the digit zero,
    /// rounded up. [`MIN_TARGET`](crate::MIN_TARGET) is the minimum target
    /// size. A size the face cannot be drawn at measures as nothing.
    #[must_use]
    pub const fn from_face(face: &'static Face, size64: u32) -> Self {
        let face = FaceRef { face, size64 };
        Self {
            char_width: face.zero_width(),
            line_height: face.line_height(),
            min_target: crate::MIN_TARGET,
            button_height: 0,
            scale: DeviceScale::ONE,
            face: Some(face),
            faces: [None; TextStyle::COUNT],
            lines: [0; TextStyle::COUNT],
            spacing: Spacing::STANDARD,
        }
    }

    /// Metrics measured as [`from_face`](Self::from_face) measures the face
    /// `atlas` was drawn from at the size it was drawn at: exactly as the
    /// atlas measures.
    #[must_use]
    pub const fn from_atlas(atlas: &'static Atlas) -> Self {
        Self::from_face(atlas.face(), (atlas.size() as u32).saturating_mul(64))
    }

    /// The same metrics, with text in `style` measured from `face` at
    /// `size64` 1/64 pixel per em: a theme's type scale, one role and
    /// weight at a time. Text in a style given no face is measured as body
    /// text.
    #[must_use]
    pub fn with_face(self, style: TextStyle, face: &'static Face, size64: u32) -> Self {
        let mut faces = self.faces;
        if let Some(slot) = faces.get_mut(style.index()) {
            *slot = Some(FaceRef { face, size64 });
        }
        Self { faces, ..self }
    }

    /// The same metrics, with text in `style` measured as `atlas` measures:
    /// its face at the size it was drawn at.
    #[must_use]
    pub fn with_atlas(self, style: TextStyle, atlas: &'static Atlas) -> Self {
        self.with_face(
            style,
            atlas.face(),
            u32::from(atlas.size()).saturating_mul(64),
        )
    }

    /// The same metrics, with text in `style` measured from `face` at
    /// `size64` and set `line_height` pixels a line, rather than the face's
    /// own line height: a type scale whose roles set their line height.
    #[must_use]
    pub fn with_face_line(
        self,
        style: TextStyle,
        face: &'static Face,
        size64: u32,
        line_height: u32,
    ) -> Self {
        let mut metrics = self.with_face(style, face, size64);
        if let Some(line) = metrics.lines.get_mut(style.index()) {
            *line = line_height;
        }
        metrics
    }

    /// The metrics text in `style` is measured with: its face's advances,
    /// kerning, line height and character width at its size - or the line
    /// height the theme gives the style
    /// ([`with_face_line`](Self::with_face_line)) - with the same minimum
    /// target, scale and type scale; or these metrics themselves for a style
    /// given no face ([`with_face`](Self::with_face)).
    #[must_use]
    pub fn for_style(&self, style: TextStyle) -> Self {
        match self.faces.get(style.index()).copied().flatten() {
            Some(face) => {
                let set = self.lines.get(style.index()).copied().unwrap_or(0);
                Self {
                    char_width: face.zero_width(),
                    line_height: if set > 0 { set } else { face.line_height() },
                    face: Some(face),
                    ..*self
                }
            }
            None => *self,
        }
    }

    /// The same metrics with `min_target` as the minimum target size - a
    /// theme's token, in pixels.
    #[must_use]
    pub const fn with_min_target(self, min_target: u32) -> Self {
        Self { min_target, ..self }
    }

    /// The same metrics with `height` as a button's least height, in
    /// pixels: a theme's token. A button is never lower than the
    /// minimum target size either.
    #[must_use]
    pub const fn with_button_height(self, height: u32) -> Self {
        Self {
            button_height: height,
            ..self
        }
    }

    /// The same metrics with `spacing` as what each spacing token measures:
    /// a theme's, in layout units, which layout gives the device
    /// scale's pixels as it does every length.
    #[must_use]
    pub const fn with_spacing(self, spacing: Spacing) -> Self {
        Self { spacing, ..self }
    }

    /// What each spacing token measures, in layout units:
    /// [`Spacing::STANDARD`] unless a theme gives others.
    #[must_use]
    pub const fn spacing(&self) -> Spacing {
        self.spacing
    }

    /// A button's least height, in pixels: 0 unless a theme gives
    /// one.
    #[must_use]
    pub const fn button_height(&self) -> u32 {
        self.button_height
    }

    /// The same metrics at device scale `scale`: layout gives every
    /// length a style names - padding, gaps, fixed widths and heights,
    /// which a screen states in layout units - `scale` pixels for each
    /// unit, so a screen written for a 960 x 540 layout fills a 1920 x 1080
    /// viewport at scale 2. Text is measured in pixels from the atlas the
    /// metrics were made from, which must be the one drawn at that scale (a
    /// theme's metrics at a scale give it: `nowayos-render-eg`'s
    /// `Theme::metrics_at`), and so is the minimum target size. A scale of
    /// 0 is taken as 1.
    #[must_use]
    pub const fn with_scale(self, scale: DeviceScale) -> Self {
        Self { scale, ..self }
    }

    /// The device scale: pixels each way for each layout unit a style
    /// names. 1 unless [`with_scale`](Self::with_scale) set another.
    #[must_use]
    pub const fn scale(&self) -> DeviceScale {
        self.scale
    }

    /// The width of one character, in pixels: a monospace font's advance;
    /// with an atlas, the digit zero's advance, rounded up (CSS's `ch`). A
    /// text input's natural width is counted in it
    /// ([`INPUT_WIDTH_CHARS`](crate::INPUT_WIDTH_CHARS)), and a mark is
    /// followed by one of it ([`marked_width`](Self::marked_width)).
    #[must_use]
    pub const fn char_width(&self) -> u32 {
        self.char_width
    }

    /// The height of one line, in pixels.
    #[must_use]
    pub const fn line_height(&self) -> u32 {
        self.line_height
    }

    /// The least width and height of every focusable control's target, in
    /// pixels: the theme's minimum target size, never below
    /// [`MIN_TARGET`](crate::MIN_TARGET) in a NoWayOS theme. Layout
    /// gives a button, a text input and a checkbox at least this much each
    /// way, and each option of a radio group a row at least this high and
    /// wide - unless a style fixes a smaller size or a parent is too small.
    #[must_use]
    pub const fn min_target(&self) -> u32 {
        self.min_target
    }

    /// The face text is measured from, and its size in 1/64 pixel per
    /// em; `None` for a monospace font.
    #[must_use]
    pub const fn face(&self) -> Option<(&'static Face, u32)> {
        match self.face {
            Some(FaceRef { face, size64 }) => Some((face, size64)),
            None => None,
        }
    }

    /// How far the pen moves for `ch`, in 1/64 pixel: its glyph's advance,
    /// or a monospace font's. `None` for a character the atlas has no glyph
    /// for, which takes no width and is kerned with neither neighbour (text
    /// a tree holds never has one: it is refused when it is set, see
    /// [`text::check_printable`](crate::text::check_printable)). A
    /// monospace font draws every character, an unknown one as its
    /// replacement glyph.
    pub(crate) fn advance(&self, ch: char) -> Option<i64> {
        match self.face {
            Some(face) => face.advance(ch),
            None => Some(i64::from(self.char_width).saturating_mul(i64::from(UNITS_PER_PIXEL))),
        }
    }

    /// The adjustment to the pen between `left` and `right`, in 1/64 pixel:
    /// the atlas's pair kerning; 0 for a monospace font.
    pub(crate) fn kerning(&self, left: char, right: char) -> i64 {
        self.face.map_or(0, |face| face.kerning(left, right))
    }

    /// Width of `text` on one line, in pixels: the sum of its characters'
    /// advances and its pairs' kerning, in 1/64 pixel with checked
    /// arithmetic, rounded up (and `u32::MAX` if it comes to more).
    #[must_use]
    pub fn text_width(&self, text: &str) -> u32 {
        let width = crate::text::starts(text, self)
            .last()
            .map_or(Some(0), |(_, width)| width);
        whole_pixels(width)
    }

    /// Width of `count` characters, in pixels (saturating): `count` times
    /// [`char_width`](Self::char_width).
    #[must_use]
    pub const fn chars_width(&self, count: u32) -> u32 {
        count.saturating_mul(self.char_width)
    }

    /// Width of a line led by a mark - a field error's marker, a checkbox's
    /// box, a radio option's ring: the mark's place, a square one line
    /// high, then one character's space ([`char_width`](Self::char_width)),
    /// then `text` (saturating). A renderer may draw the mark smaller
    /// inside its place.
    #[must_use]
    pub fn marked_width(&self, text: &str) -> u32 {
        self.line_height
            .saturating_add(self.char_width)
            .saturating_add(self.text_width(text))
    }

    /// Height of a checkbox's content and of each radio option: a line, but
    /// at least the minimum target size.
    #[must_use]
    pub fn choice_height(&self) -> u32 {
        self.line_height.max(self.min_target)
    }

    /// Height of a key of the on-screen keyboard: a line and
    /// [`KEY_SPACE`](crate::KEY_SPACE) at the scale, but at least the
    /// minimum target size.
    #[must_use]
    pub fn key_height(&self) -> u32 {
        self.line_height
            .saturating_add(self.scale.px(crate::KEY_SPACE))
            .max(self.min_target)
    }

    /// The least natural size, padding included, of a node of `kind`: the
    /// minimum target size each way for a button or a text input, whose
    /// whole rectangle is the target, and a button at least the theme's
    /// button height. A checkbox's and a radio option's
    /// content is already at least that size (see `measure`); other kinds
    /// have none.
    fn least_size(&self, kind: &NodeKind) -> Size {
        match kind {
            NodeKind::Button { .. } => {
                Size::new(self.min_target, self.min_target.max(self.button_height))
            }
            NodeKind::TextInput { .. } => Size::new(self.min_target, self.min_target),
            // An icon's size, at the scale.
            NodeKind::Icon(_) => {
                let side = self.scale.px(nowayos_icons::SIZE);
                Size::new(side, side)
            }
            // A progress bar's height, at the scale.
            NodeKind::Progress { .. } => Size::new(0, self.scale.px(crate::PROGRESS_HEIGHT)),
            _ => Size::ZERO,
        }
    }
}

/// A run's width in 1/64 pixel as whole pixels, rounded up: 0 for nothing
/// (or less), `u32::MAX` for more than that or a run too wide to count
/// (`None`).
pub(crate) fn whole_pixels(width: Option<i64>) -> u32 {
    match width {
        Some(width) if width <= 0 => 0,
        Some(width) => u32::try_from(width.unsigned_abs().div_ceil(u64::from(UNITS_PER_PIXEL)))
            .unwrap_or(u32::MAX),
        None => u32::MAX,
    }
}

/// `width` pixels in 1/64 pixel: what a run's width must not exceed to fit.
pub(crate) fn pixels_in_units(width: u32) -> i64 {
    i64::from(width).saturating_mul(i64::from(UNITS_PER_PIXEL))
}

/// A face at a size in 1/64 pixel per em: what text is measured with,
/// compared and hashed by the face's identity and the size - every face is
/// a `static`, so two metrics measure alike when they name the same one at
/// the same size.
#[derive(Clone, Copy)]
struct FaceRef {
    face: &'static Face,
    size64: u32,
}

impl FaceRef {
    /// `ch`'s advance, in 1/64 pixel; `None` for a character the face
    /// lacks or a size it cannot be scaled to.
    fn advance(&self, ch: char) -> Option<i64> {
        let scale = self.face.scale(self.size64)?;
        let outline = self.face.glyph(ch).ok()?;
        Some(i64::from(scale.to_64(outline.advance())))
    }

    /// The pair's kerning, in 1/64 pixel.
    fn kerning(&self, left: char, right: char) -> i64 {
        self.face.scale(self.size64).map_or(0, |scale| {
            i64::from(scale.to_64(self.face.kerning_units(left, right)))
        })
    }

    /// A line's height in whole pixels; 0 at a size it cannot be scaled to.
    const fn line_height(&self) -> u32 {
        match self.face.scale(self.size64) {
            Some(scale) => self.face.line_metrics(scale).line_height(),
            None => 0,
        }
    }

    /// The advance of the digit zero in whole pixels, rounded up; 0 if it
    /// has none.
    const fn zero_width(&self) -> u32 {
        let (Some(scale), Some(units)) = (self.face.scale(self.size64), self.face.advance_of('0'))
        else {
            return 0;
        };
        let advance = scale.to_64(units);
        if advance <= 0 {
            0
        } else {
            advance.unsigned_abs().div_ceil(64)
        }
    }
}

impl PartialEq for FaceRef {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.face, other.face) && self.size64 == other.size64
    }
}

impl Eq for FaceRef {}

impl Hash for FaceRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::ptr::hash(self.face, state);
        self.size64.hash(state);
    }
}

impl fmt::Debug for FaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.size64.is_multiple_of(64) {
            write!(f, "Face({}, {} px)", self.face.id(), self.size64 / 64)
        } else {
            write!(f, "Face({}, {}/64 px)", self.face.id(), self.size64)
        }
    }
}

/// A container's main axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    const fn of(kind: &NodeKind) -> Option<Self> {
        match kind {
            NodeKind::Row => Some(Self::Horizontal),
            NodeKind::Column => Some(Self::Vertical),
            _ => None,
        }
    }

    const fn main(self, size: Size) -> u32 {
        match self {
            Self::Horizontal => size.width,
            Self::Vertical => size.height,
        }
    }

    const fn cross(self, size: Size) -> u32 {
        match self {
            Self::Horizontal => size.height,
            Self::Vertical => size.width,
        }
    }

    const fn main_length(self, style: &Style) -> Length {
        match self {
            Self::Horizontal => style.width,
            Self::Vertical => style.height,
        }
    }

    const fn cross_length(self, style: &Style) -> Length {
        match self {
            Self::Horizontal => style.height,
            Self::Vertical => style.width,
        }
    }

    const fn size(self, main: u32, cross: u32) -> Size {
        match self {
            Self::Horizontal => Size::new(main, cross),
            Self::Vertical => Size::new(cross, main),
        }
    }

    /// Start of `rect` along (main, cross).
    const fn start(self, rect: &Rect) -> (i64, i64) {
        match self {
            Self::Horizontal => (rect.left(), rect.top()),
            Self::Vertical => (rect.top(), rect.left()),
        }
    }

    fn rect(self, main_at: i64, main: u32, cross_at: i64, cross: u32) -> Rect {
        let (left, top) = match self {
            Self::Horizontal => (main_at, cross_at),
            Self::Vertical => (cross_at, main_at),
        };
        let size = self.size(main, cross);
        Rect::from_edges(
            left,
            top,
            left.saturating_add(i64::from(size.width)),
            top.saturating_add(i64::from(size.height)),
        )
    }
}

/// Size and offset across an axis for a child of natural size `natural`
/// in `available` pixels.
fn place_across(length: Length, natural: u32, available: u32, align: Align) -> (u32, u32) {
    let size = match length {
        Length::Fixed(value) => value.min(available),
        Length::Fill => available,
        Length::Auto if align == Align::Stretch => available,
        Length::Auto => natural.min(available),
    };
    let slack = available.saturating_sub(size);
    let offset = match align {
        Align::Start | Align::Stretch => 0,
        Align::Center => slack / 2,
        Align::End => slack,
    };
    (offset, size)
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// Lays the tree out in `viewport`, using `metrics` to size text.
    ///
    /// The root is placed at the viewport's corner and, unless its style
    /// fixes a dimension, fills the viewport. Afterwards every node's
    /// [`rect`](crate::Node::rect) is set and the whole viewport is marked
    /// dirty. Content that does not fit overflows its container along the
    /// main axis; renderers clip to the viewport.
    ///
    /// # Errors
    ///
    /// [`UiError::ViewportOutOfRange`] unless the viewport is at most
    /// [`MAX_VIEWPORT_SIZE`] each way with every edge within
    /// [`MAX_VIEWPORT_OFFSET`] of the origin; [`UiError::NoRoot`]; or
    /// [`UiError::TooDeep`] if the structure is deeper than [`MAX_DEPTH`]
    /// (which the building API already prevents). Nothing changes on error.
    pub fn layout(&mut self, viewport: Rect, metrics: &TextMetrics) -> Result<(), UiError> {
        check_viewport(viewport)?;
        let root = self.root.ok_or(UiError::NoRoot)?;
        let units = Units::of(metrics);
        self.measure(root, metrics, 0, viewport.size.width)?;
        // A group at the root is sized by the child it shows.
        let style = match self.present(root)? {
            Some(shown) => style_at(&self.node(shown)?.style, units),
            None => Style::DEFAULT,
        };
        let width = match style.width {
            Length::Fixed(width) => width,
            Length::Auto | Length::Fill => viewport.size.width,
        };
        let height = match style.height {
            Length::Fixed(height) => height,
            Length::Auto | Length::Fill => viewport.size.height,
        };
        let root_rect = Rect {
            origin: viewport.origin,
            size: Size::new(width, height),
        };
        self.arrange(root, root_rect, units, 0)?;
        self.viewport = viewport;
        self.metrics = Some(*metrics);
        self.scale = units.scale;
        self.dirty.reset(viewport);
        self.moves.clear();
        Ok(())
    }

    /// [`measure`](Self::measure) of `id`'s subtree, `level` deep, in
    /// `room` pixels: for laying one part of the tree out again.
    pub(crate) fn measure_at(
        &mut self,
        id: NodeId,
        metrics: &TextMetrics,
        level: usize,
        room: u32,
    ) -> Result<Size, UiError> {
        self.measure(id, metrics, level, room)
    }

    /// [`arrange`](Self::arrange) of `id`'s subtree at `rect`, `level`
    /// deep, at `metrics`' scale: for laying one part out again.
    pub(crate) fn arrange_at(
        &mut self,
        id: NodeId,
        rect: Rect,
        metrics: &TextMetrics,
        level: usize,
    ) -> Result<(), UiError> {
        self.arrange(id, rect, Units::of(metrics), level)
    }

    /// Computes and stores the natural size of `id` and its subtree, `room`
    /// pixels being the width it may take.
    fn measure(
        &mut self,
        id: NodeId,
        metrics: &TextMetrics,
        level: usize,
        room: u32,
    ) -> Result<Size, UiError> {
        if level > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let node = self.node(id)?;
        let style = style_at(&node.style, Units::of(metrics));
        // Its text measured in its own role and weight.
        let metrics = &metrics.for_style(style.text);
        let least = metrics.least_size(&node.kind);
        if node.kind.is_conditional() {
            // The child it shows, measured as if it stood in its place.
            let natural = match self.group_branch(id) {
                Some(shown) => self.measure(shown, metrics, level.saturating_add(1), room)?,
                None => Size::ZERO,
            };
            self.node_mut(id)?.intrinsic = natural;
            return Ok(natural);
        }
        // The width its content may take: its own, if fixed, less its
        // padding.
        let inner = width_within(&style, fixed_or(style.width, room))
            .saturating_sub(style.padding.left.saturating_add(style.padding.right));
        let content = match &node.kind {
            NodeKind::Text(text) => text_size(text, style.text, inner, metrics),
            NodeKind::Slot {
                text,
                template: Some(template),
                ..
            } => text_size(&template.around(text), style.text, inner, metrics),
            NodeKind::Slot { text, .. } => text_size(text, style.text, inner, metrics),
            // Every line it wraps onto at the width it may take.
            NodeKind::LongText { start, len, .. } => {
                let text = self.texts.get(*start, *len);
                let (mut width, mut lines) = (0_u32, 0_u32);
                for line in crate::long_text::lines(text, inner, metrics) {
                    width = width.max(metrics.text_width(line));
                    lines = lines.saturating_add(1);
                }
                Size::new(width, metrics.line_height.saturating_mul(lines.max(1)))
            }
            NodeKind::FieldError { message, .. } => Size::new(
                message.map_or(0, |message| metrics.marked_width(message)),
                metrics.line_height,
            ),
            NodeKind::Checkbox { label, .. } => Size::new(
                metrics.marked_width(label).max(metrics.min_target),
                metrics.choice_height(),
            ),
            NodeKind::RadioGroup { options, .. } => {
                let widest = options
                    .iter()
                    .map(|option| metrics.marked_width(option))
                    .max()
                    .unwrap_or(0);
                let rows = u32::try_from(options.len()).unwrap_or(u32::MAX);
                Size::new(
                    widest.max(metrics.min_target),
                    metrics.choice_height().saturating_mul(rows),
                )
            }
            // A header line and its rows, each a choice high; as
            // wide as its widest option, marked, or its placeholder.
            NodeKind::ListPicker {
                options,
                placeholder,
                rows,
                ..
            } => {
                let widest = options
                    .iter()
                    .map(|option| metrics.marked_width(option))
                    .max()
                    .unwrap_or(0)
                    .max(metrics.text_width(placeholder));
                Size::new(
                    widest.max(metrics.min_target),
                    metrics
                        .choice_height()
                        .saturating_mul(u32::from(*rows).saturating_add(1)),
                )
            }
            // Its rows of keys, each key standing in its padding
            // again; at least ten keys of the minimum target size across.
            NodeKind::Keyboard { .. } => {
                let pad = &style.padding;
                let across = metrics
                    .min_target
                    .saturating_add(pad.left.saturating_add(pad.right));
                let down = metrics
                    .key_height()
                    .saturating_add(pad.top.saturating_add(pad.bottom));
                Size::new(
                    across.saturating_mul(10),
                    down.saturating_mul(u32::from(crate::keyboard::KEY_ROWS)),
                )
            }
            NodeKind::Button { label, .. } => {
                Size::new(metrics.text_width(label), metrics.line_height)
            }
            NodeKind::TextInput { placeholder, .. } => Size::new(
                metrics
                    .chars_width(INPUT_WIDTH_CHARS)
                    .max(metrics.text_width(placeholder)),
                metrics.line_height,
            ),
            NodeKind::Spacer
            | NodeKind::Group { .. }
            | NodeKind::Switch { .. }
            | NodeKind::Progress { .. }
            | NodeKind::Icon(_) => Size::ZERO,
            // A picture's own size, at the scale.
            NodeKind::Image { picture, .. } => Size::new(
                metrics.scale.px(picture.width()),
                metrics.scale.px(picture.height()),
            ),
            // A scroll view is measured as a stack of its one child.
            NodeKind::Column | NodeKind::Row | NodeKind::Stack | NodeKind::Scroll { .. } => {
                let axis = Axis::of(&node.kind);
                let first_child = node.first_child;
                // A view that scrolls across gives its child all the width
                // it wants.
                let inner = match node.kind {
                    NodeKind::Scroll { axes, .. } if axes.horizontal() => MAX_VIEWPORT_SIZE,
                    // Beside the gutter its scrollbar runs in.
                    NodeKind::Scroll { .. } => {
                        inner.saturating_sub(metrics.scale.px(crate::scroll::SCROLLBAR_WIDTH))
                    }
                    _ => inner,
                };
                // In a row, children that wrap share what the others leave.
                let shared = match axis {
                    Some(Axis::Horizontal) => {
                        Some(self.share_row(id, metrics, level, inner, style.gap)?)
                    }
                    _ => None,
                };
                let mut next = first_child;
                let mut main = 0u32;
                let mut cross = 0u32;
                let mut count = 0u32;
                let mut visited = 0usize;
                while let Some(child) = next {
                    let size = match shared {
                        // Measured already, at the row's whole width.
                        Some(_) if !self.wraps(child, level.saturating_add(1))? => {
                            self.node(child)?.intrinsic
                        }
                        Some(share) => {
                            self.measure(child, metrics, level.saturating_add(1), share)?
                        }
                        None => self.measure(child, metrics, level.saturating_add(1), inner)?,
                    };
                    next = self.node(child)?.next_sibling;
                    visited = visited.saturating_add(1);
                    if visited > N {
                        // More siblings than nodes: the links are corrupt.
                        return Err(UiError::InvalidNode(child));
                    }
                    // A group showing nothing takes no space and no gap.
                    let Some(shown) = self.present(child)? else {
                        continue;
                    };
                    // A spacer takes space along the main axis only.
                    let is_spacer = matches!(self.node(shown)?.kind, NodeKind::Spacer);
                    count = count.saturating_add(1);
                    match axis {
                        Some(axis) => {
                            main = main.saturating_add(axis.main(size));
                            if !is_spacer {
                                cross = cross.max(axis.cross(size));
                            }
                        }
                        None => {
                            // Stack: the largest child in each direction.
                            main = main.max(size.width);
                            cross = cross.max(size.height);
                        }
                    }
                }
                match axis {
                    Some(axis) => {
                        let gaps = count.saturating_sub(1).saturating_mul(style.gap);
                        axis.size(main.saturating_add(gaps), cross)
                    }
                    None => Size::new(main, cross),
                }
            }
        };
        let padded = content
            .saturating_add(style.padding.size())
            .component_max(least);
        let natural = Size::new(
            width_within(&style, fixed_or(style.width, padded.width)),
            fixed_or(style.height, padded.height),
        );
        self.node_mut(id)?.intrinsic = natural;
        Ok(natural)
    }

    /// Measures every child of row `id` that does not wrap, at the row's
    /// whole content width `inner`, and returns the width each child that
    /// does is to be measured at: what the others and the gaps
    /// leave, shared equally - the whole width if none wraps.
    fn share_row(
        &mut self,
        id: NodeId,
        metrics: &TextMetrics,
        level: usize,
        inner: u32,
        gap: u32,
    ) -> Result<u32, UiError> {
        let mut next = self.node(id)?.first_child;
        let mut used = 0u32;
        let mut count = 0u32;
        let mut wrapping = 0u32;
        let mut budget = N;
        while let Some(child) = next {
            budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(child))?;
            next = self.node(child)?.next_sibling;
            if self.wraps(child, level.saturating_add(1))? {
                wrapping = wrapping.saturating_add(1);
                count = count.saturating_add(u32::from(self.present(child)?.is_some()));
                continue;
            }
            let size = self.measure(child, metrics, level.saturating_add(1), inner)?;
            if self.present(child)?.is_some() {
                count = count.saturating_add(1);
                used = used.saturating_add(size.width);
            }
        }
        let gaps = count.saturating_sub(1).saturating_mul(gap);
        let left = inner.saturating_sub(used).saturating_sub(gaps);
        Ok(left.checked_div(wrapping).unwrap_or(inner))
    }

    /// Whether `id`, or anything below it, is a text or slot that wraps.
    fn wraps(&self, id: NodeId, level: usize) -> Result<bool, UiError> {
        if level > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let node = self.node(id)?;
        if matches!(node.kind, NodeKind::Text(_) | NodeKind::Slot { .. }) && node.style.text.wraps()
            || matches!(node.kind, NodeKind::LongText { .. })
        {
            return Ok(true);
        }
        let mut next = node.first_child;
        let mut budget = N;
        while let Some(child) = next {
            budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(child))?;
            if self.wraps(child, level.saturating_add(1))? {
                return Ok(true);
            }
            next = self.node(child)?.next_sibling;
        }
        Ok(false)
    }

    /// Places `id` at `rect` and lays out its children inside it, every
    /// length its style names - or the spacing token standing for it - in
    /// pixels at `units`' device scale.
    fn arrange(
        &mut self,
        id: NodeId,
        rect: Rect,
        units: Units,
        level: usize,
    ) -> Result<(), UiError> {
        if level > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let node = self.node_mut(id)?;
        node.rect = rect;
        node.padding = style_at(&node.style, units).padding;
        if let NodeKind::Scroll { .. } = node.kind {
            return self.arrange_scroll(id, rect, units, level);
        }
        if node.kind.is_conditional() {
            // The child it shows takes the group's place; the other, none.
            let shown = self.group_branch(id);
            let mut next = self.node(id)?.first_child;
            let mut budget = N;
            while let Some(child) = next {
                budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(child))?;
                next = self.node(child)?.next_sibling;
                if Some(child) == shown {
                    self.arrange(child, rect, units, level.saturating_add(1))?;
                } else {
                    self.hide(child, level.saturating_add(1))?;
                }
            }
            return Ok(());
        }
        if !node.kind.is_container() {
            return Ok(());
        }
        let style = style_at(&node.style, units);
        let axis = Axis::of(&node.kind);
        let content = rect.inset(style.padding);
        let mut placer = match axis {
            None => Placer::Stack {
                content,
                align: style.align,
            },
            Some(axis) => self.plan_line(id, axis, content, &style, units)?,
        };
        // Walk the sibling links, reading the next link before recursing, so
        // no list of children is held on the stack.
        let mut next = self.node(id)?.first_child;
        let mut budget = N;
        while let Some(child) = next {
            budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(child))?;
            next = self.node(child)?.next_sibling;
            // A group is placed as the child it shows; one showing nothing
            // takes no place, and no gap.
            let Some(shown) = self.present(child)? else {
                self.hide(child, level.saturating_add(1))?;
                continue;
            };
            let shown_node = self.node(shown)?;
            let child_rect =
                placer.place(&style_at(&shown_node.style, units), shown_node.intrinsic);
            self.arrange(child, child_rect, units, level.saturating_add(1))?;
        }
        Ok(())
    }

    /// Places scroll view `id`'s one child: along each axis it scrolls at
    /// its natural size but never smaller than the view, across one it does
    /// not as large as the view; moved left by its `x` and up by its `y`,
    /// each kept between 0 and how much larger the child is than the view,
    /// and stored so.
    fn arrange_scroll(
        &mut self,
        id: NodeId,
        rect: Rect,
        units: Units,
        level: usize,
    ) -> Result<(), UiError> {
        let node = self.node(id)?;
        let NodeKind::Scroll { x, y, axes, rows } = node.kind else {
            return Ok(());
        };
        let content = port(
            rect.inset(style_at(&node.style, units).padding),
            axes,
            units.scale.px(crate::scroll::SCROLLBAR_WIDTH),
        );
        let Some(child) = node.first_child else {
            return Ok(());
        };
        let natural = self.node(child)?.intrinsic;
        let (view_width, view_height) = (content.size.width, content.size.height);
        // A virtual list's child is as tall as all its items.
        let row_px = rows.map(|rows| units.scale.px(rows.height).max(1));
        let natural_height = match (rows, row_px) {
            (Some(rows), Some(row)) => {
                u32::try_from(u64::from(rows.count).saturating_mul(u64::from(row)))
                    .unwrap_or(u32::MAX)
            }
            _ => natural.height,
        };
        let width = if axes.horizontal() {
            natural.width.max(view_width)
        } else {
            view_width
        };
        let height = if axes.vertical() {
            natural_height.max(view_height)
        } else {
            view_height
        };
        let x = x.min(width.saturating_sub(view_width));
        let y = y.min(height.saturating_sub(view_height));
        if let NodeKind::Scroll {
            x: kept_x,
            y: kept_y,
            ..
        } = &mut self.node_mut(id)?.kind
        {
            (*kept_x, *kept_y) = (x, y);
        }
        let left = content.left().saturating_sub(i64::from(x));
        let top = content.top().saturating_sub(i64::from(y));
        let placed = Rect::from_edges(
            left,
            top,
            left.saturating_add(i64::from(width)),
            top.saturating_add(i64::from(height)),
        );
        match (rows, row_px) {
            (Some(rows), Some(row)) => {
                self.arrange_rows(id, child, placed, rows, row, y, view_height, units, level)
            }
            _ => self.arrange(child, placed, units, level.saturating_add(1)),
        }
    }

    /// Places a virtual list's pool of rows - `child`'s children - in
    /// `placed`, `row` pixels each: the window of items starts a margin of
    /// rows above the `y` scrolled to, item `i` shown by pool row `i` mod
    /// the pool's size at `i` rows down; a row with no item is hidden. A
    /// window that moved is reported ([`UiEvent::ListMoved`](crate::UiEvent::ListMoved)).
    #[allow(
        clippy::too_many_arguments,
        reason = "the scroll view's placement, worked out by its caller, passed whole"
    )]
    fn arrange_rows(
        &mut self,
        id: NodeId,
        child: NodeId,
        placed: Rect,
        rows: crate::Rows,
        row: u32,
        y: u32,
        view_height: u32,
        units: Units,
        level: usize,
    ) -> Result<(), UiError> {
        let node = self.node_mut(child)?;
        node.rect = placed;
        node.padding = nowayos_display::Insets::ZERO;
        let pool = u32::try_from(self.children(child).count()).unwrap_or(u32::MAX);
        let shown = view_height.div_ceil(row).saturating_add(1);
        let margin = pool.saturating_sub(shown) / 2;
        let first = y
            .checked_div(row)
            .unwrap_or(0)
            .saturating_sub(margin)
            .min(rows.count.saturating_sub(pool));
        let mut moved = false;
        if let NodeKind::Scroll {
            rows: Some(kept), ..
        } = &mut self.node_mut(id)?.kind
        {
            moved = kept.first != first;
            kept.first = first;
        }
        if moved {
            self.list_moved = Some(rows.list);
        }
        let mut next = self.node(child)?.first_child;
        let mut index = 0_u32;
        let mut budget = N;
        while let Some(at) = next {
            budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(at))?;
            next = self.node(at)?.next_sibling;
            // The item this row shows: the one in the window that is
            // `index` mod the pool's size.
            let offset = index
                .saturating_add(pool)
                .saturating_sub(first.checked_rem(pool).unwrap_or(0))
                .checked_rem(pool)
                .unwrap_or(0);
            let item = first.saturating_add(offset);
            index = index.saturating_add(1);
            if item >= rows.count {
                self.hide(at, level.saturating_add(2))?;
                continue;
            }
            let top = placed
                .top()
                .saturating_add(i64::from(item).saturating_mul(i64::from(row)));
            let rect = Rect::from_edges(
                placed.left(),
                top,
                placed.right(),
                top.saturating_add(i64::from(row)),
            );
            self.arrange(at, rect, units, level.saturating_add(2))?;
        }
        Ok(())
    }

    /// Every scroll view, in paint order: each before any inside it.
    pub fn scroll_views(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.paint_order()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Scroll { .. }))
            .map(|(id, _)| id)
    }

    /// The first scroll view, if the tree has one.
    #[must_use]
    pub fn scroll_node(&self) -> Option<NodeId> {
        self.scroll_views().next()
    }

    /// How far the first scroll view is scrolled down, in pixels, if the
    /// tree has one ([`scroll_position`](Self::scroll_position) for any).
    #[must_use]
    pub fn scroll_offset(&self) -> Option<u32> {
        self.scroll_position(self.scroll_node()?).map(|(_, y)| y)
    }

    /// How far scroll view `id` is scrolled: right and down, in pixels.
    #[must_use]
    pub fn scroll_position(&self, id: NodeId) -> Option<(u32, u32)> {
        match self.get(id)?.kind {
            NodeKind::Scroll { x, y, .. } => Some((x, y)),
            _ => None,
        }
    }

    /// What scroll view `id` shows its child through: its content less a
    /// gutter for its scrollbar along each axis it scrolls - down its right
    /// side if it scrolls down, along its bottom if it scrolls across - so
    /// nothing it shows is ever drawn under a scrollbar.
    #[must_use]
    pub fn scroll_port(&self, id: NodeId) -> Option<Rect> {
        let view = self.get(id)?;
        let NodeKind::Scroll { axes, .. } = view.kind else {
            return None;
        };
        Some(port(
            view.content(),
            axes,
            self.scale.px(crate::scroll::SCROLLBAR_WIDTH),
        ))
    }

    /// How far scroll view `id` can scroll: right and down, in pixels, as
    /// last laid out - how much larger its child is than what it shows it
    /// through ([`scroll_port`](Self::scroll_port)).
    #[must_use]
    pub fn scroll_range(&self, id: NodeId) -> Option<(u32, u32)> {
        let view = self.get(id)?;
        let content = self.scroll_port(id)?;
        let child = view.first_child.and_then(|child| self.get(child))?.rect;
        Some((
            child.size.width.saturating_sub(content.size.width),
            child.size.height.saturating_sub(content.size.height),
        ))
    }

    /// What `node` is seen through: what every scroll view it is inside
    /// shows its child through ([`scroll_port`](Self::scroll_port)),
    /// together, outside which nothing of it is drawn or hit; `None` for a
    /// node outside every scroll view. Inside a view scrolled wholly out of
    /// an outer one, an empty rectangle.
    #[must_use]
    pub fn scroll_clip(&self, node: &Node) -> Option<Rect> {
        let mut seen: Option<Rect> = None;
        let mut next = node.parent;
        for _ in 0..=MAX_DEPTH {
            let Some(id) = next else {
                break;
            };
            let Some(parent) = self.get(id) else {
                break;
            };
            if let Some(view) = self.scroll_port(id) {
                seen = Some(match seen {
                    Some(inner) => inner.intersection(&view).unwrap_or(Rect::EMPTY),
                    None => view,
                });
            }
            next = parent.parent;
        }
        seen
    }

    /// Scrolls the first scroll view to `offset` pixels down, kept within
    /// its range ([`scroll_view_to`](Self::scroll_view_to) for any).
    ///
    /// # Errors
    ///
    /// As [`scroll_view_to`](Self::scroll_view_to).
    pub fn scroll_to(&mut self, offset: u32) -> Result<(), UiError> {
        let Some(id) = self.scroll_node() else {
            return Ok(());
        };
        let x = self.scroll_position(id).map_or(0, |(x, _)| x);
        self.scroll_view_to(id, x, offset).map(|_| ())
    }

    /// Scrolls view `id` to `x` pixels right and `y` down, each kept within
    /// its range: its child is placed again, what was already drawn of it
    /// moves with it ([`moves`](Self::moves)) and only what that exposes -
    /// and its scrollbars - is marked for repainting. Whether it moved.
    /// Nothing happens before the first layout.
    ///
    /// # Errors
    ///
    /// [`UiError::WrongKind`] if `id` is not a scroll view;
    /// [`UiError::TooDeep`] if the tree's links are corrupt.
    pub fn scroll_view_to(&mut self, id: NodeId, x: u32, y: u32) -> Result<bool, UiError> {
        let Some(metrics) = self.metrics else {
            return Ok(false);
        };
        let Some(was) = self.scroll_position(id) else {
            return Err(UiError::WrongKind(id));
        };
        let (most_x, most_y) = self.scroll_range(id).unwrap_or((0, 0));
        let now = (x.min(most_x), y.min(most_y));
        if now == was {
            return Ok(false);
        }
        let mut depth = 0usize;
        let mut next = self.node(id)?.parent;
        while let Some(parent) = next {
            depth = depth.saturating_add(1);
            if depth > MAX_DEPTH {
                return Err(UiError::TooDeep);
            }
            next = self.node(parent)?.parent;
        }
        let rect = self.node(id)?.rect;
        if let NodeKind::Scroll { x, y, .. } = &mut self.node_mut(id)?.kind {
            (*x, *y) = now;
        }
        self.arrange(id, rect, Units::of(&metrics), depth)?;
        let moved_x = i64::from(was.0).saturating_sub(i64::from(now.0));
        let moved_y = i64::from(was.1).saturating_sub(i64::from(now.1));
        self.scrolled(id, moved_x, moved_y);
        Ok(true)
    }

    /// Scrolls every scroll view `id` is inside, innermost first, until
    /// `id` is in view along each axis it scrolls: the least scroll that
    /// shows it whole, or its start if it is larger than the view.
    pub(crate) fn reveal(&mut self, id: NodeId) {
        let mut next = self.get(id).and_then(|node| node.parent);
        for _ in 0..=MAX_DEPTH {
            let Some(view_id) = next else {
                return;
            };
            let Some(view) = self.get(view_id) else {
                return;
            };
            next = view.parent;
            let NodeKind::Scroll { x, y, axes, .. } = view.kind else {
                continue;
            };
            let (Some(seen), Some(rect)) =
                (self.scroll_port(view_id), self.get(id).map(Node::rect))
            else {
                return;
            };
            let shift = |start: i64, end: i64, view_start: i64, view_end: i64| {
                let before = view_start.saturating_sub(start);
                let after = end.saturating_sub(view_end);
                if before > 0 {
                    before.saturating_neg()
                } else if after > 0 {
                    after.min(start.saturating_sub(view_start))
                } else {
                    0
                }
            };
            let target = |offset: u32, by: i64| {
                u32::try_from(i64::from(offset).saturating_add(by).max(0)).unwrap_or(u32::MAX)
            };
            let to_x = if axes.horizontal() {
                target(
                    x,
                    shift(rect.left(), rect.right(), seen.left(), seen.right()),
                )
            } else {
                x
            };
            let to_y = if axes.vertical() {
                target(
                    y,
                    shift(rect.top(), rect.bottom(), seen.top(), seen.bottom()),
                )
            } else {
                y
            };
            let _ = self.scroll_view_to(view_id, to_x, to_y);
        }
    }

    /// Gives `id` and everything below it an empty rectangle: what a group
    /// does not show takes no place on screen.
    fn hide(&mut self, id: NodeId, level: usize) -> Result<(), UiError> {
        if level > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let node = self.node_mut(id)?;
        node.rect = Rect::EMPTY;
        node.padding = nowayos_display::Insets::ZERO;
        let mut next = self.node(id)?.first_child;
        let mut budget = N;
        while let Some(child) = next {
            budget = budget.checked_sub(1).ok_or(UiError::InvalidNode(child))?;
            next = self.node(child)?.next_sibling;
            self.hide(child, level.saturating_add(1))?;
        }
        Ok(())
    }

    /// The node that stands in `id`'s place in its parent's layout: `id`
    /// itself, or for a group the child it shows (a group in turn resolved
    /// the same way); `None` for a group that shows nothing.
    fn present(&self, id: NodeId) -> Result<Option<NodeId>, UiError> {
        let mut current = id;
        for _ in 0..=MAX_DEPTH {
            if !self.node(current)?.kind.is_conditional() {
                return Ok(Some(current));
            }
            match self.group_branch(current) {
                Some(shown) => current = shown,
                None => return Ok(None),
            }
        }
        Err(UiError::TooDeep)
    }

    /// Works out how the children of a `Row` or `Column` share its main
    /// axis: the space left for `Fill` children and where the first one
    /// starts.
    fn plan_line(
        &self,
        id: NodeId,
        axis: Axis,
        content: Rect,
        style: &Style,
        units: Units,
    ) -> Result<Placer, UiError> {
        let mut used = 0u32;
        let mut fills = 0u32;
        let mut count = 0u32;
        for child in self.children(id) {
            let Some(shown) = self.present(child)? else {
                continue;
            };
            let child_node = self.node(shown)?;
            count = count.saturating_add(1);
            match axis.main_length(&style_at(&child_node.style, units)) {
                Length::Fill => fills = fills.saturating_add(1),
                Length::Fixed(value) => used = used.saturating_add(value),
                Length::Auto => used = used.saturating_add(axis.main(child_node.intrinsic)),
            }
        }
        let gaps = count.saturating_sub(1).saturating_mul(style.gap);
        let free = axis
            .main(content.size)
            .saturating_sub(used)
            .saturating_sub(gaps);
        let (mut cursor, cross_start) = axis.start(&content);
        // The room left over, where `justify` puts it: none when a
        // child fills it.
        let (mut spread, mut spread_remainder) = (0, 0);
        if fills == 0 {
            match style.justify {
                Justify::Start => {}
                Justify::Center => cursor = cursor.saturating_add(i64::from(free / 2)),
                Justify::End => cursor = cursor.saturating_add(i64::from(free)),
                Justify::SpaceBetween => {
                    let between = count.saturating_sub(1);
                    spread = free.checked_div(between).unwrap_or(0);
                    spread_remainder = free.checked_rem(between).unwrap_or(0);
                }
            }
        }
        Ok(Placer::Line {
            axis,
            cursor,
            cross_start,
            available_cross: axis.cross(content.size),
            gap: style.gap,
            align: style.align,
            share: free.checked_div(fills).unwrap_or(0),
            remainder: free.checked_rem(fills).unwrap_or(0),
            spread,
            spread_remainder,
        })
    }
}

/// Hands out child rectangles, one child at a time, in order.
enum Placer {
    /// A `Stack`: every child aligned within the same content area.
    Stack { content: Rect, align: Align },
    /// A `Row` or `Column`: children one after another along `axis`.
    Line {
        axis: Axis,
        /// Where the next child starts along the main axis.
        cursor: i64,
        cross_start: i64,
        available_cross: u32,
        gap: u32,
        align: Align,
        /// Main-axis size of each `Fill` child…
        share: u32,
        /// …plus one pixel for each of the first `remainder` of them.
        remainder: u32,
        /// Room `justify` spreads after each child…
        spread: u32,
        /// …plus one pixel after each of the first `spread_remainder`.
        spread_remainder: u32,
    },
}

impl Placer {
    fn place(&mut self, style: &Style, natural: Size) -> Rect {
        match self {
            Self::Stack { content, align } => {
                let (dx, width) = place_across_within(
                    style,
                    style.width,
                    natural.width,
                    content.size.width,
                    *align,
                );
                let (dy, height) =
                    place_across(style.height, natural.height, content.size.height, *align);
                let left = content.left().saturating_add(i64::from(dx));
                let top = content.top().saturating_add(i64::from(dy));
                Rect::from_edges(
                    left,
                    top,
                    left.saturating_add(i64::from(width)),
                    top.saturating_add(i64::from(height)),
                )
            }
            Self::Line {
                axis,
                cursor,
                cross_start,
                available_cross,
                gap,
                align,
                share,
                remainder,
                spread,
                spread_remainder,
            } => {
                let mut main = match axis.main_length(style) {
                    Length::Fixed(value) => value,
                    Length::Auto => axis.main(natural),
                    Length::Fill => {
                        let extra = u32::from(*remainder > 0);
                        *remainder = remainder.saturating_sub(extra);
                        share.saturating_add(extra)
                    }
                };
                // A row's child: its width is its main length.
                if *axis == Axis::Horizontal {
                    main = width_within(style, main);
                }
                let (offset, cross) = match axis {
                    Axis::Vertical => place_across_within(
                        style,
                        axis.cross_length(style),
                        axis.cross(natural),
                        *available_cross,
                        *align,
                    ),
                    Axis::Horizontal => place_across(
                        axis.cross_length(style),
                        axis.cross(natural),
                        *available_cross,
                        *align,
                    ),
                };
                let rect = axis.rect(
                    *cursor,
                    main,
                    cross_start.saturating_add(i64::from(offset)),
                    cross,
                );
                // What `justify` spreads between the children.
                let extra = u32::from(*spread_remainder > 0);
                *spread_remainder = spread_remainder.saturating_sub(extra);
                *cursor = cursor
                    .saturating_add(i64::from(main))
                    .saturating_add(i64::from(*gap))
                    .saturating_add(i64::from(spread.saturating_add(extra)));
                rect
            }
        }
    }
}

/// The size of `text` set in `style`, at most `room` pixels wide if it
/// wraps: one line, as wide as the text; or the lines it takes
/// there, up to the style's most, as wide as the widest.
fn text_size(text: &str, style: TextStyle, room: u32, metrics: &TextMetrics) -> Size {
    if !style.wraps() {
        return Size::new(metrics.text_width(text), metrics.line_height);
    }
    let mut width = 0u32;
    let mut lines = 0u32;
    for line in crate::text::wrap(text, room, metrics).take(usize::from(style.lines)) {
        width = width.max(metrics.text_width(line));
        lines = lines.saturating_add(1);
    }
    Size::new(width, metrics.line_height.saturating_mul(lines.max(1)))
}

/// `content` less a gutter `bar` pixels wide for each scrollbar of a view
/// scrolling along `axes`: down its right side if it scrolls down, along
/// its bottom if it scrolls across.
pub(crate) fn port(content: Rect, axes: crate::ScrollAxes, bar: u32) -> Rect {
    let bar = i64::from(bar);
    let right = if axes.vertical() {
        content.right().saturating_sub(bar).max(content.left())
    } else {
        content.right()
    };
    let bottom = if axes.horizontal() {
        content.bottom().saturating_sub(bar).max(content.top())
    } else {
        content.bottom()
    };
    Rect::from_edges(content.left(), content.top(), right, bottom)
}

/// Accepts a viewport only inside the range layout and drawing support.
fn check_viewport(viewport: Rect) -> Result<(), UiError> {
    let limit = i64::from(MAX_VIEWPORT_OFFSET);
    let in_range = viewport.size.width <= MAX_VIEWPORT_SIZE
        && viewport.size.height <= MAX_VIEWPORT_SIZE
        && viewport.left() >= limit.saturating_neg()
        && viewport.top() >= limit.saturating_neg()
        && viewport.right() <= limit
        && viewport.bottom() <= limit;
    if in_range {
        Ok(())
    } else {
        Err(UiError::ViewportOutOfRange(viewport))
    }
}

/// `Fixed(v)` → `v`; otherwise the natural size.
const fn fixed_or(length: Length, natural: u32) -> u32 {
    match length {
        Length::Fixed(value) => value,
        Length::Auto | Length::Fill => natural,
    }
}

/// What layout measures a style's lengths with: the device
/// scale, pixels for each layout unit, and what each spacing token
/// measures in the theme.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Units {
    scale: DeviceScale,
    spacing: Spacing,
}

impl Units {
    /// The units `metrics` lays out with.
    pub(crate) const fn of(metrics: &TextMetrics) -> Self {
        Self {
            scale: metrics.scale(),
            spacing: metrics.spacing(),
        }
    }
}

/// `style` with every length it names - padding, gap, fixed width and
/// height, in layout units, or the spacing token standing for one -
/// in pixels at `units`' device scale (saturating). Alignment and `Auto`
/// and `Fill` lengths are the same at every scale; the result names no
/// token.
pub(crate) const fn style_at(style: &Style, units: Units) -> Style {
    let scale = units.scale;
    let tokens = style.tokens;
    let padding = match tokens.padding {
        Some(space) => nowayos_display::Insets::uniform(units.spacing.of(space)),
        None => style.padding,
    };
    let gap = match tokens.gap {
        Some(space) => units.spacing.of(space),
        None => style.gap,
    };
    let (width, height) = match tokens.size {
        Some(space) => {
            let side = Length::Fixed(units.spacing.of(space));
            (side, side)
        }
        None => (style.width, style.height),
    };
    Style {
        width: length_at(width, scale),
        height: length_at(height, scale),
        padding: nowayos_display::Insets {
            left: scale.px(padding.left),
            top: scale.px(padding.top),
            right: scale.px(padding.right),
            bottom: scale.px(padding.bottom),
        },
        gap: scale.px(gap),
        align: style.align,
        text: style.text,
        variant: style.variant,
        surface: style.surface,
        tokens: SpaceTokens::NONE,
        justify: style.justify,
        min_width: scale.px(style.min_width),
        max_width: match style.max_width {
            Some(units) => Some(scale.px(units)),
            None => None,
        },
        // Drawn in layout units: the renderer scales it.
        look: style.look,
    }
}

/// `width` within `style`'s least and greatest width: the least
/// wins over the greatest, as in CSS.
const fn width_within(style: &Style, width: u32) -> u32 {
    let capped = match style.max_width {
        Some(most) if width > most => most,
        _ => width,
    };
    if capped < style.min_width {
        style.min_width
    } else {
        capped
    }
}

/// [`place_across`] for a width: the size kept within `style`'s least and
/// greatest width and, unless that least is wider, the space available;
/// then placed by `align`.
fn place_across_within(
    style: &Style,
    length: Length,
    natural: u32,
    available: u32,
    align: Align,
) -> (u32, u32) {
    let (_, size) = place_across(length, natural, available, align);
    let size = width_within(style, size).min(available.max(style.min_width));
    let slack = available.saturating_sub(size);
    let offset = match align {
        Align::Start | Align::Stretch => 0,
        Align::Center => slack / 2,
        Align::End => slack,
    };
    (offset, size)
}

/// A length in layout units, in pixels at device scale `scale`, to the
/// nearest whole pixel.
const fn length_at(length: Length, scale: DeviceScale) -> Length {
    match length {
        Length::Fixed(value) => Length::Fixed(scale.px(value)),
        Length::Auto | Length::Fill => length,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::ActionId;
    use nowayos_display::Insets;

    const METRICS: TextMetrics = TextMetrics::new(6, 10);

    fn rect_of(tree: &Tree<32>, id: NodeId) -> Rect {
        tree.get(id).unwrap().rect()
    }

    #[test]
    fn column_stacks_children_with_padding_and_gap() {
        let mut t: Tree<32> = Tree::new();
        let root = t
            .column(Style::new().padding(10).gap(4).align(Align::Start))
            .unwrap();
        t.set_root(root).unwrap();
        let a = t
            .add_child(root, NodeKind::Text("abc"), Style::new())
            .unwrap();
        let b = t
            .add_child(root, NodeKind::Text("hello"), Style::new())
            .unwrap();
        t.layout(Rect::new(0, 0, 200, 100), &METRICS).unwrap();
        assert_eq!(rect_of(&t, root), Rect::new(0, 0, 200, 100));
        assert_eq!(rect_of(&t, a), Rect::new(10, 10, 18, 10));
        assert_eq!(rect_of(&t, b), Rect::new(10, 24, 30, 10));
    }

    #[test]
    fn stretch_fills_the_cross_axis() {
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new().padding(5)).unwrap();
        t.set_root(root).unwrap();
        let a = t
            .add_child(root, NodeKind::Text("x"), Style::new())
            .unwrap();
        t.layout(Rect::new(0, 0, 100, 50), &METRICS).unwrap();
        assert_eq!(rect_of(&t, a), Rect::new(5, 5, 90, 10));
    }

    #[test]
    fn center_and_end_alignment() {
        let mut t: Tree<32> = Tree::new();
        let root = t.row(Style::new().align(Align::Center)).unwrap();
        t.set_root(root).unwrap();
        let a = t
            .add_child(root, NodeKind::Text("ab"), Style::new())
            .unwrap();
        t.layout(Rect::new(0, 0, 100, 50), &METRICS).unwrap();
        // 12x10 text centred vertically in a 50-high row.
        assert_eq!(rect_of(&t, a), Rect::new(0, 20, 12, 10));
        t.set_style(root, Style::new().align(Align::End)).unwrap();
        t.layout(Rect::new(0, 0, 100, 50), &METRICS).unwrap();
        assert_eq!(rect_of(&t, a), Rect::new(0, 40, 12, 10));
    }

    #[test]
    fn fill_children_share_leftover_space_exactly() {
        let mut t: Tree<32> = Tree::new();
        let root = t.row(Style::new().gap(1)).unwrap();
        t.set_root(root).unwrap();
        let fixed = t
            .add_child(
                root,
                NodeKind::Spacer,
                Style::new().width(Length::Fixed(10)),
            )
            .unwrap();
        let f1 = t
            .add_child(root, NodeKind::Spacer, Style::new().width(Length::Fill))
            .unwrap();
        let f2 = t
            .add_child(root, NodeKind::Spacer, Style::new().width(Length::Fill))
            .unwrap();
        let f3 = t
            .add_child(root, NodeKind::Spacer, Style::new().width(Length::Fill))
            .unwrap();
        t.layout(Rect::new(0, 0, 44, 10), &METRICS).unwrap();
        // 44 - 10 fixed - 3 gaps = 31 = 11 + 10 + 10.
        assert_eq!(rect_of(&t, fixed), Rect::new(0, 0, 10, 10));
        assert_eq!(rect_of(&t, f1), Rect::new(11, 0, 11, 10));
        assert_eq!(rect_of(&t, f2), Rect::new(23, 0, 10, 10));
        assert_eq!(rect_of(&t, f3), Rect::new(34, 0, 10, 10));
    }

    #[test]
    fn spacer_pushes_content_to_the_end() {
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new()).unwrap();
        t.set_root(root).unwrap();
        let top = t.text("top").unwrap();
        t.append(root, top).unwrap();
        let spacer = t.spacer().unwrap();
        t.append(root, spacer).unwrap();
        let bottom = t.button("Go", ActionId(1)).unwrap();
        t.append(root, bottom).unwrap();
        t.layout(Rect::new(0, 0, 80, 100), &METRICS).unwrap();
        // Button: 10 text + 8 + 8 padding = 26 high, at the very
        // bottom.
        assert_eq!(rect_of(&t, bottom), Rect::new(0, 74, 80, 26));
        assert_eq!(rect_of(&t, spacer), Rect::new(0, 10, 80, 64));
    }

    #[test]
    fn intrinsic_sizes_of_controls() {
        let mut t: Tree<32> = Tree::new();
        let root = t.row(Style::new().align(Align::Start)).unwrap();
        t.set_root(root).unwrap();
        let button = t.button("OK", ActionId(1)).unwrap();
        t.append(root, button).unwrap();
        let input = t.text_input(crate::FieldId(1), "Name").unwrap();
        t.append(root, input).unwrap();
        t.layout(Rect::new(0, 0, 300, 50), &METRICS).unwrap();
        // "OK": 12 wide + 48 padding; 10 high + 16 padding = 26.
        assert_eq!(rect_of(&t, button), Rect::new(0, 0, 60, 26));
        // 16 chars * 6 = 96 + 12 padding; 10 + 10 padding = 20, raised to 24.
        assert_eq!(rect_of(&t, input), Rect::new(60, 0, 108, 24));
    }

    #[test]
    fn controls_are_at_least_the_metrics_minimum_target_size() {
        // A theme's minimum target size, not a constant: layout honours
        // whatever the metrics say.
        for min_target in [24, 40] {
            let metrics = METRICS.with_min_target(min_target);
            let mut t: Tree<32> = Tree::new();
            let root = t.row(Style::new().align(Align::Start)).unwrap();
            t.set_root(root).unwrap();
            // The smallest each kind can be: no label, no placeholder.
            let button = t.button("", ActionId(1)).unwrap();
            t.append(root, button).unwrap();
            let input = t.text_input(crate::FieldId(1), "").unwrap();
            t.append(root, input).unwrap();
            let checkbox = t.checkbox(crate::FieldId(2), "").unwrap();
            t.append(root, checkbox).unwrap();
            let group = t.radio_group(crate::FieldId(3), &["", ""]).unwrap();
            t.append(root, group).unwrap();
            t.layout(Rect::new(0, 0, 400, 200), &metrics).unwrap();
            for id in [button, input, checkbox, group] {
                let size = rect_of(&t, id).size;
                assert!(size.width >= min_target, "{id:?}: {size:?}");
                assert!(size.height >= min_target, "{id:?}: {size:?}");
            }
            let group = t.get(group).unwrap();
            for index in 0..2 {
                let row = group.option_rect(index).unwrap().size;
                assert!(row.width >= min_target, "option {index}: {row:?}");
                assert!(row.height >= min_target, "option {index}: {row:?}");
            }
            // Text that is not a control is not raised.
            assert_eq!(metrics.least_size(&NodeKind::Text("")), Size::ZERO);
        }
    }

    #[test]
    fn fixed_sizes_and_cross_axis_cap() {
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new().align(Align::Start)).unwrap();
        t.set_root(root).unwrap();
        let wide = t
            .add_child(root, NodeKind::Spacer, Style::new().size(500, 7))
            .unwrap();
        t.layout(Rect::new(0, 0, 100, 100), &METRICS).unwrap();
        assert_eq!(rect_of(&t, wide), Rect::new(0, 0, 100, 7));
    }

    #[test]
    fn stack_overlays_children() {
        let mut t: Tree<32> = Tree::new();
        let root = t
            .stack(Style::new().padding(2).align(Align::Center))
            .unwrap();
        t.set_root(root).unwrap();
        let back = t
            .add_child(
                root,
                NodeKind::Spacer,
                Style::new().width(Length::Fill).height(Length::Fill),
            )
            .unwrap();
        let front = t
            .add_child(root, NodeKind::Text("hi"), Style::new())
            .unwrap();
        t.layout(Rect::new(0, 0, 40, 30), &METRICS).unwrap();
        assert_eq!(rect_of(&t, back), Rect::new(2, 2, 36, 26));
        assert_eq!(rect_of(&t, front), Rect::new(14, 10, 12, 10));
    }

    #[test]
    fn root_fixed_size_and_offset_viewport() {
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new().size(50, 20)).unwrap();
        t.set_root(root).unwrap();
        t.layout(Rect::new(7, 9, 300, 300), &METRICS).unwrap();
        assert_eq!(rect_of(&t, root), Rect::new(7, 9, 50, 20));
        assert_eq!(t.viewport(), Rect::new(7, 9, 300, 300));
        assert_eq!(t.dirty_rects(), &[Rect::new(7, 9, 300, 300)]);
    }

    #[test]
    fn overflow_is_allowed_but_never_wraps() {
        let mut t: Tree<32> = Tree::new();
        let root = t
            .row(Style::new().gap(u32::MAX).insets(Insets::uniform(u32::MAX)))
            .unwrap();
        t.set_root(root).unwrap();
        let a = t
            .add_child(
                root,
                NodeKind::Spacer,
                Style::new().size(u32::MAX, u32::MAX),
            )
            .unwrap();
        let b = t
            .add_child(
                root,
                NodeKind::Spacer,
                Style::new().size(u32::MAX, u32::MAX),
            )
            .unwrap();
        // The largest viewport at the far corner of the allowed range.
        let far = MAX_VIEWPORT_OFFSET - MAX_VIEWPORT_SIZE as i32;
        let viewport = Rect::new(far, far, MAX_VIEWPORT_SIZE, MAX_VIEWPORT_SIZE);
        t.layout(viewport, &METRICS).unwrap();
        // Absurd sizes saturate; nothing panics and positions stay ordered.
        assert!(rect_of(&t, b).left() >= rect_of(&t, a).left());
    }

    #[test]
    fn viewports_outside_the_supported_range_are_refused() {
        // Regression (review 2026-10-01): viewports near the edge of the
        // i32 range were accepted, and drawing them overflowed inside
        // embedded-graphics (a panic, so an abort in a PD).
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new()).unwrap();
        t.set_root(root).unwrap();
        let max = MAX_VIEWPORT_OFFSET;
        let size = MAX_VIEWPORT_SIZE;
        for viewport in [
            Rect::new(i32::MAX - 50, 0, 800, 480),
            Rect::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX),
            Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX),
            Rect::new(0, 0, size + 1, 480),
            Rect::new(0, 0, 800, size + 1),
            Rect::new(max - 799, 0, 800, 480),
            Rect::new(0, -max - 1, 800, 480),
        ] {
            assert_eq!(
                t.layout(viewport, &METRICS),
                Err(UiError::ViewportOutOfRange(viewport)),
                "{viewport:?}"
            );
        }
        // The edges of the range itself are fine, and nothing above changed
        // the tree.
        assert_eq!(t.viewport(), Rect::EMPTY);
        for viewport in [
            Rect::new(max - 800, -max, 800, 480),
            Rect::new(-max, max - size as i32, size, size),
            Rect::new(0, 0, 0, 0),
        ] {
            assert_eq!(t.layout(viewport, &METRICS), Ok(()), "{viewport:?}");
        }
    }

    /// A rectangle `scale` times the size, from `scale` times the corner.
    fn times(rect: Rect, scale: u32) -> Rect {
        let s = i32::try_from(scale).unwrap();
        Rect::new(
            rect.origin.x.checked_mul(s).unwrap(),
            rect.origin.y.checked_mul(s).unwrap(),
            rect.size.width.checked_mul(scale).unwrap(),
            rect.size.height.checked_mul(scale).unwrap(),
        )
    }

    /// At device scale 2 every length a style names - padding, gaps,
    /// fixed sizes - takes twice the pixels, so a screen with no text lays
    /// out in a viewport twice the size exactly as twice its scale-1
    /// layout; and each node's content is its rectangle less its padding at
    /// that scale.
    #[test]
    fn at_scale_2_every_style_length_takes_twice_the_pixels() {
        let mut t: Tree<32> = Tree::new();
        let root = t
            .column(
                Style::new()
                    .insets(Insets {
                        left: 3,
                        top: 5,
                        right: 7,
                        bottom: 11,
                    })
                    .gap(4),
            )
            .unwrap();
        t.set_root(root).unwrap();
        let fixed = t
            .add_child(root, NodeKind::Spacer, Style::new().size(30, 9))
            .unwrap();
        let row = t
            .add_child(
                root,
                NodeKind::Row,
                Style::new().padding(2).gap(6).height(Length::Fixed(20)),
            )
            .unwrap();
        let a = t
            .add_child(row, NodeKind::Spacer, Style::new().width(Length::Fill))
            .unwrap();
        let b = t
            .add_child(row, NodeKind::Spacer, Style::new().width(Length::Fixed(17)))
            .unwrap();
        let fill = t
            .add_child(root, NodeKind::Spacer, Style::new().height(Length::Fill))
            .unwrap();
        t.layout(Rect::new(0, 0, 200, 120), &METRICS).unwrap();
        let one: Vec<Rect> = [root, fixed, row, a, b, fill]
            .iter()
            .map(|&id| rect_of(&t, id))
            .collect();
        let contents: Vec<Rect> = [root, row]
            .iter()
            .map(|&id| t.get(id).unwrap().content())
            .collect();
        assert_eq!(t.scale(), DeviceScale::ONE);
        t.layout(
            Rect::new(0, 0, 400, 240),
            &METRICS.with_scale(DeviceScale::TWO),
        )
        .unwrap();
        assert_eq!(t.scale(), DeviceScale::TWO);
        for (id, rect) in [root, fixed, row, a, b, fill].iter().zip(one) {
            assert_eq!(rect_of(&t, *id), times(rect, 2), "{id:?}");
        }
        for (id, content) in [root, row].iter().zip(contents) {
            assert_eq!(t.get(*id).unwrap().content(), times(content, 2), "{id:?}");
        }
        assert_eq!(t.get(root).unwrap().content(), Rect::new(6, 10, 380, 208));
        // At 1.5 each length is turned into whole pixels once, to the
        // nearest (a half rounds up): padding 3, 5, 7 and 11 are 5, 8, 11
        // and 17 pixels, the 30 x 9 spacer 45 x 14.
        let one_and_a_half = DeviceScale::from_quarters(6).unwrap();
        t.layout(
            Rect::new(0, 0, 300, 180),
            &METRICS.with_scale(one_and_a_half),
        )
        .unwrap();
        assert_eq!(t.get(root).unwrap().content(), Rect::new(5, 8, 284, 155));
        assert_eq!(rect_of(&t, fixed).size, Size::new(45, 14));
    }

    /// Text is measured in the pixels of the metrics' own atlas - the one
    /// drawn at that scale - and controls get the minimum target size the
    /// metrics give, in pixels: neither is scaled again.
    #[test]
    fn at_scale_2_text_and_targets_are_the_metrics_own_pixels() {
        let mut t: Tree<32> = Tree::new();
        let root = t.row(Style::new().align(Align::Start)).unwrap();
        t.set_root(root).unwrap();
        let text = t.text("abc").unwrap();
        t.append(root, text).unwrap();
        let button = t.button("", ActionId(1)).unwrap();
        t.append(root, button).unwrap();
        let metrics = TextMetrics::new(12, 20)
            .with_min_target(48)
            .with_scale(DeviceScale::TWO);
        t.layout(Rect::new(0, 0, 400, 200), &metrics).unwrap();
        assert_eq!(rect_of(&t, text), Rect::new(0, 0, 36, 20));
        // The button's padding, 24 by 8 units, is 48 by 16 pixels;
        // its content a line, 20: 52 high, above the 48-pixel target.
        assert_eq!(rect_of(&t, button), Rect::new(36, 0, 96, 52));
        assert_eq!(t.get(button).unwrap().content(), Rect::new(84, 16, 0, 20));
        // A theme's button height raises it further, in pixels too.
        let tall = metrics.with_button_height(112);
        t.layout(Rect::new(0, 0, 400, 200), &tall).unwrap();
        assert_eq!(rect_of(&t, button), Rect::new(36, 0, 96, 112));
    }

    #[test]
    fn layout_without_root_is_an_error() {
        let mut t: Tree<32> = Tree::new();
        assert_eq!(
            t.layout(Rect::new(0, 0, 10, 10), &METRICS),
            Err(UiError::NoRoot)
        );
    }
}
