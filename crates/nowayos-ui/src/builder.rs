// SPDX-License-Identifier: MIT OR Apache-2.0
//! Children-first building: the API that Screen Compiler-generated screens target.
//!
//! Screen Compiler converts a supported subset of React/TSX into Rust that builds a
//! screen through the [`UiBuilder`] trait, importing this module as `ui`
//! (Screen Compiler's builder contract v1; v0's six calls are unchanged, so screens
//! generated for v0 still build). Construction is **children first**: a
//! container is created from the handles of children that already exist, so
//! every node is complete when it is created. Every call is fallible — a full
//! arena is an error propagated with `?`, never a panic — and text is
//! `&'static str` literals, so nothing is copied or allocated.
//!
//! [`Tree`] implements [`UiBuilder`] with [`NodeId`] handles and [`UiError`]
//! errors. A generated `build` function returns the root handle; install it
//! with [`Tree::set_root`], then lay out and render as usual:
//!
//! ```
//! use nowayos_ui::builder::{self as ui, UiBuilder};
//! use nowayos_ui::{Tree, UiError};
//!
//! // Shaped like Screen Compiler's output for a two-element column.
//! const GO: ui::ActionId = ui::ActionId::new(1);
//! fn build<B: ui::UiBuilder>(b: &mut B) -> Result<B::Node, B::Error> {
//!     let title = b.text("Hello")?;
//!     let go = b.button("Go", GO)?;
//!     b.column(ui::Layout::new().gap(8).padding(16), &[title, go])
//! }
//!
//! let mut tree: Tree<8> = Tree::new();
//! let root = build(&mut tree)?;
//! tree.set_root(root)?;
//! assert_eq!(tree.children(root).count(), 2);
//! # Ok::<(), UiError>(())
//! ```
//!
//! `Tree` also has inherent methods named `column`, `text` and so on, with
//! different arguments; the inherent ones are for hand-written code, and the
//! trait methods are what generated code calls through `B: UiBuilder`.
//!
//! Contract v1 adds what form screens need:
//! application text in a [`UiBuilder::slot`];
//! content shown or hidden by the application, [`UiBuilder::group`] and
//! [`UiBuilder::either`]; and the value-holding controls, each bound to a
//! field number - [`UiBuilder::input`] (a text input with its own `max_len`
//! and [`InputMode`]), [`UiBuilder::checkbox`] and
//! [`UiBuilder::radio_group`]. A generated screen numbers its fields, slots
//! and groups from 1 in document order, as it numbers actions. [`Tree`]'s
//! implementation refuses a field or slot number used twice in one tree
//! ([`UiError::DuplicateField`], [`UiError::DuplicateSlot`]); several groups
//! may share a number, the one prop that shows them all
//! ([`Tree::set_visible`]).
//!
//! Contract v2 adds text in a role of the type scale and a weight:
//! [`UiBuilder::text_styled`] and [`UiBuilder::slot_styled`], with
//! [`TextStyle`], [`TextRole`] and [`Weight`]. Text in the display role may
//! use only what its atlases draw ([`crate::text::is_display`]); the tree
//! refuses any other with [`UiError::NoGlyph`].
//!
//! Contract v3 adds the look of buttons and surfaces:
//! [`UiBuilder::button_with`], a button in a [`ButtonVariant`] - primary,
//! secondary or quiet - and [`UiBuilder::column_on`] and
//! [`UiBuilder::row_on`], a container drawn on a [`Surface`]: a card, a
//! header bar or a notice in a [`Tone`]. Every button's label is Bold.
//!
//! Contract v4 adds text set on several lines: a [`TextStyle`]'s
//! [`lines`](TextStyle::lines), up to [`MAX_LINES`], on `text_styled` or
//! `slot_styled`.
//!
//! Contract v5 adds [`UiBuilder::progress`], a progress bar the
//! application fills, bound to a slot number of its own.
//!
//! Contract v6 adds [`UiBuilder::icon`], one of Lucide's icons
//! from [`icons`], tinted by the theme.
//!
//! Contract v7 adds a text's alignment, [`TextStyle::align`] with a
//! [`TextAlign`], and the theme's spacing tokens ([`Space`]): a gap or
//! padding named by size ([`Layout::gap_space`], [`Layout::padding_space`])
//! and [`UiBuilder::spacer_space`], each measured against the theme's
//! spacing when the tree is laid out, so the large-text theme spaces half
//! again as much.
//!
//! Contract v8 adds what a screen's `style={{...}}` declares: on a
//! `Layout`, a fixed [`width`](Layout::width) and [`height`](Layout::height),
//! a least and greatest width, children's [`Align`] across and [`Justify`]
//! along the main axis, and a [`Look`] - a fill, a border and its width,
//! rounded corners - in the theme's [`Colour`]s; a text's colour,
//! [`TextStyle::colour`]; and [`UiBuilder::button_look`], a button with a
//! fill or label colour of its own.
//!
//! Contract v10 adds [`UiBuilder::list_picker`]: one choice of up
//! to 64 options, in a list a [`Picker`] shapes - its placeholder and how
//! many rows show - that scrolls inside itself.
//!
//! Contract v9 adds [`UiBuilder::scroll_view`]: one child - a
//! column - shown through a clipped view that scrolls, at most one in a
//! screen and never nested.
//!
//! Contract v11 adds [`UiBuilder::keyboard`]: the on-screen
//! keyboard, its keys typing into the focused text input in a layout for
//! its mode; at most one in a screen.
//!
//! Contract v12 adds [`UiBuilder::field_error`]: the line where the
//! errors the application sets for a field show, one per field.
//!
//! Contract v13 adds conditions beyond one prop's truth:
//! [`UiBuilder::unless`], a child shown while a group is hidden (React's
//! `!cond && <X/>`), and switches - [`UiBuilder::case`] and
//! [`UiBuilder::case_or`] - a child shown while a switch the application
//! sets is on one case (`step === "review" && <X/>`), every switch of one
//! number moving together.
//!
//! Contract v14 adds [`UiBuilder::slot_template`] and
//! [`UiBuilder::slot_template_styled`]: a slot shown between literal text
//! compiled in (React's `<Text>Hello, {props.name}!</Text>`), the
//! application setting only its part.
//!
//! Contract v15 adds [`UiBuilder::image`]: a picture converted at build
//! time, by its static from the product's generated module, fitted to its
//! box by a [`Fit`] and with words saying what it shows (React's
//! `<img src alt style={{ objectFit }} />`).
//!
//! Contract v16 adds [`UiBuilder::long_text`]: paragraphs the application
//! sets at run time, up to a capacity the screen gives, kept in the tree's
//! text store - `T` bytes of a [`Tree<N, T>`](Tree) - so nothing allocates
//! (React's `<Text maxLength={10240}>{props.article}</Text>`).
//!
//! Contract v17 adds [`UiBuilder::scroll_view_axes`]: scroll views that
//! scroll down, across or both ways ([`ScrollAxes`]), any number of them,
//! one inside another (CSS's `overflow`, `overflowX`, `overflowY`).
//!
//! Contract v18 adds [`UiBuilder::virtual_list`]: a list of as many items
//! as the application counts, shown through a pool of rows built once and
//! rebound as it scrolls ([`ListId`]).
//!
//! Contract v19 adds [`UiBuilder::text_area`]: several lines a person
//! types, kept in the tree's text store (React's `<textarea rows
//! maxLength>`).
//!
//! Contract v20 adds layers over the screen ([`LayerId`]):
//! [`UiBuilder::dialog`], [`UiBuilder::menu`], [`UiBuilder::popover`] and
//! [`UiBuilder::tooltip`] (HTML's `<dialog>`, and menus, popovers and
//! tooltips beside a control).
//!
//! Contract v21 adds [`UiBuilder::rich_text`]: one paragraph of runs, each
//! with its own weight, italics, colour, code or link ([`Run`]; React's
//! `<strong>`, `<em>`, `<code>`, `<a onClick>` and `<span>` inside a text).
//!
//! Contract v22 adds to [`Look`] a theme's shadow ([`Elevation`]), a
//! [`Gradient`] between the theme's colours and an opacity (CSS's
//! `boxShadow`, `linear-gradient`, `radial-gradient` and `opacity`).
//!
//! Contract v23 adds to [`Look`] a [`Transition`] - how a node comes into
//! view when a condition shows it - and an [`Animation`] of its own (CSS's
//! `transition` and `animation`).
//!
//! Contract v24 adds [`TextStyle::selectable`](crate::TextStyle::selectable):
//! text a press selects, for Ctrl+C to copy (CSS's `userSelect: "text"`).
//!
//! Contract v25 adds [`UiBuilder::context_action`] - an action a right
//! click or a long press reports (React's `onContextMenu`) - and
//! [`UiBuilder::cursor`], the pointer's shape over a node (CSS's `cursor`).
//!
//! Provisional: tracks Screen Compiler's contract; both sides change together.

use crate::{ContainerKind, LayerKind, Length, NodeId, NodeKind, Style, Tree, UiError};

/// The icons generated screens name (contract v6).
pub use crate::icons;
/// Motion a screen declares (contract v23): how a node comes into view,
/// and an animation of its own.
pub use crate::motion::{Animation, Easing, Property, Repeat, Transition};
/// The pointer's shape over a node (contract v25).
pub use crate::mouse::Cursor;
/// How a picture fills its box (contract v15).
pub use crate::pictures::{Fit, Picture};
/// A run of a rich text (contract v21).
pub use crate::rich::Run;
pub use crate::{
    ActionId, Align, ButtonVariant, Colour, Elevation, FieldId, Gradient, GroupId, Input,
    InputMode, Justify, LayerId, ListId, Look, MAX_LINES, SLOT_CAPACITY, ScrollAxes, SlotId, Space,
    Surface, SwitchId, TEMPLATE_CAPACITY, Template, TextAlign, TextRole, TextStyle, Tone, Weight,
};

/// How a list picker shows (contract v10): the placeholder its
/// header shows until an option is chosen, and how many rows of options
/// show at once - [`LIST_ROWS_DEFAULT`](crate::LIST_ROWS_DEFAULT) unless
/// told, within [`LIST_ROWS`](crate::LIST_ROWS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Picker {
    placeholder: &'static str,
    rows: u8,
}

impl Picker {
    /// No placeholder, the default number of rows.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            placeholder: "",
            rows: crate::LIST_ROWS_DEFAULT,
        }
    }

    /// With `text` in the header until an option is chosen.
    #[must_use]
    pub const fn placeholder(self, text: &'static str) -> Self {
        Self {
            placeholder: text,
            ..self
        }
    }

    /// With `rows` rows of options showing at once.
    #[must_use]
    pub const fn rows(self, rows: u8) -> Self {
        Self { rows, ..self }
    }

    /// Its placeholder.
    #[must_use]
    pub const fn placeholder_text(self) -> &'static str {
        self.placeholder
    }

    /// How many rows show.
    #[must_use]
    pub const fn row_count(self) -> u8 {
        self.rows
    }
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

/// Gap and padding of a `Column` or `Row`: each in layout units, or a
/// spacing token the theme measures (contract v7), which stands in
/// place of the number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Layout {
    gap: u16,
    padding: u16,
    gap_space: Option<Space>,
    padding_space: Option<Space>,
    width: Option<u16>,
    height: Option<u16>,
    min_width: u16,
    max_width: Option<u16>,
    align: Align,
    justify: Justify,
    look: Look,
}

impl Layout {
    /// No gap and no padding.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gap: 0,
            padding: 0,
            gap_space: None,
            padding_space: None,
            width: None,
            height: None,
            min_width: 0,
            max_width: None,
            align: Align::Stretch,
            justify: Justify::Start,
            look: Look::NONE,
        }
    }

    /// `units` wide (contract v8).
    #[must_use]
    pub const fn width(self, units: u16) -> Self {
        Self {
            width: Some(units),
            ..self
        }
    }

    /// `units` high (contract v8).
    #[must_use]
    pub const fn height(self, units: u16) -> Self {
        Self {
            height: Some(units),
            ..self
        }
    }

    /// At least `units` wide (contract v8).
    #[must_use]
    pub const fn min_width(self, units: u16) -> Self {
        Self {
            min_width: units,
            ..self
        }
    }

    /// At most `units` wide (contract v8).
    #[must_use]
    pub const fn max_width(self, units: u16) -> Self {
        Self {
            max_width: Some(units),
            ..self
        }
    }

    /// Its children placed across its main axis as `align` says (contract
    /// v8): stretched, unless it names another.
    #[must_use]
    pub const fn align(self, align: Align) -> Self {
        Self { align, ..self }
    }

    /// Its children placed along its main axis as `justify` says (contract
    /// v8).
    #[must_use]
    pub const fn justify(self, justify: Justify) -> Self {
        Self { justify, ..self }
    }

    /// Drawn with `look` (contract v8): a fill, a border, rounded
    /// corners, in the theme's colours.
    #[must_use]
    pub const fn look(self, look: Look) -> Self {
        Self { look, ..self }
    }

    /// Its look (contract v8).
    #[must_use]
    pub const fn look_of(self) -> Look {
        self.look
    }

    /// With the theme's `space` between children (contract v7).
    #[must_use]
    pub const fn gap_space(self, space: Space) -> Self {
        Self {
            gap_space: Some(space),
            ..self
        }
    }

    /// With the theme's `space` as padding on every side (contract v7).
    #[must_use]
    pub const fn padding_space(self, space: Space) -> Self {
        Self {
            padding_space: Some(space),
            ..self
        }
    }

    /// The spacing token standing for the gap, if one does.
    #[must_use]
    pub const fn gap_token(self) -> Option<Space> {
        self.gap_space
    }

    /// The spacing token standing for the padding, if one does.
    #[must_use]
    pub const fn padding_token(self) -> Option<Space> {
        self.padding_space
    }

    /// With `px` between children.
    #[must_use]
    pub const fn gap(self, px: u16) -> Self {
        Self { gap: px, ..self }
    }

    /// With `px` of padding on every side.
    #[must_use]
    pub const fn padding(self, px: u16) -> Self {
        Self {
            padding: px,
            ..self
        }
    }

    /// The space between children.
    #[must_use]
    pub const fn gap_px(self) -> u16 {
        self.gap
    }

    /// The padding on every side.
    #[must_use]
    pub const fn padding_px(self) -> u16 {
        self.padding
    }

    /// The equivalent [`Style`]: children stretch across the container, as
    /// flexbox's default `align-items` does. A spacing token stands in
    /// place of its number.
    #[must_use]
    pub const fn style(self) -> Style {
        let mut style = Style::new()
            .gap(self.gap as u32)
            .padding(self.padding as u32)
            .align(self.align)
            .justify(self.justify)
            .min_width(self.min_width as u32)
            .look(self.look);
        if let Some(space) = self.gap_space {
            style = style.gap_space(space);
        }
        if let Some(space) = self.padding_space {
            style = style.padding_space(space);
        }
        if let Some(units) = self.width {
            style = style.width(Length::Fixed(units as u32));
        }
        if let Some(units) = self.height {
            style = style.height(Length::Fixed(units as u32));
        }
        if let Some(units) = self.max_width {
            style = style.max_width(units as u32);
        }
        style
    }
}

/// Builds a UI tree, children before parents.
///
/// Each method creates one node and returns its handle, or the builder's
/// error (for example when a bounded arena is full). A container takes the
/// handles of its children, which must not already have a parent.
pub trait UiBuilder {
    /// A handle to a node in the tree being built.
    type Node: Copy;
    /// Why a node could not be created.
    type Error;

    /// A vertical stack of `children`.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn column(
        &mut self,
        layout: Layout,
        children: &[Self::Node],
    ) -> Result<Self::Node, Self::Error>;

    /// A horizontal row of `children`.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn row(&mut self, layout: Layout, children: &[Self::Node]) -> Result<Self::Node, Self::Error>;

    /// Static text.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn text(&mut self, text: &'static str) -> Result<Self::Node, Self::Error>;

    /// A button labelled `label` that emits `action` when pressed.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn button(&mut self, label: &'static str, action: ActionId) -> Result<Self::Node, Self::Error>;

    /// A single-line text field showing `placeholder` while empty.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn text_input(&mut self, placeholder: Option<&'static str>) -> Result<Self::Node, Self::Error>;

    /// Empty space of a fixed `size` along the parent's main axis, or
    /// flexible space that takes up the room left when `None`.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn spacer(&mut self, size: Option<u16>) -> Result<Self::Node, Self::Error>;

    /// Empty space the theme's `space` across, along the parent's main axis
    /// (contract v7): measured when the tree is laid out, so the
    /// large-text theme makes it half again as wide.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn spacer_space(&mut self, space: Space) -> Result<Self::Node, Self::Error>;

    /// A button in `variant` drawn with `look` (contract v8): its
    /// fill, label colour or corners the theme's, as `look` names them.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn button_look(
        &mut self,
        label: &'static str,
        action: ActionId,
        variant: ButtonVariant,
        look: Look,
    ) -> Result<Self::Node, Self::Error>;

    /// A scroll view of `child` (contract v9): `child` - a column -
    /// at its natural height, shown through the view, which takes the room
    /// its parent leaves unless `layout` gives it a height. Page Up and Page
    /// Down, a mouse's wheel, a touch dragged on it, and focus moving onto
    /// a control it hides scroll it.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full.
    fn scroll_view(&mut self, layout: Layout, child: Self::Node)
    -> Result<Self::Node, Self::Error>;

    /// A scroll view of `child` along `axes` (contract v17): CSS's
    /// `overflow`, `overflowX` or `overflowY` of `auto` or `scroll`. Along
    /// each axis it scrolls, `child` takes its natural size and the view the
    /// room its parent leaves, unless `layout` gives it a size; across one
    /// it does not, `child` is as large as the view. Views may stand side by
    /// side and one inside another.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full.
    fn scroll_view_axes(
        &mut self,
        layout: Layout,
        axes: ScrollAxes,
        child: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// A virtual list (contract v18): a scroll view down showing list
    /// `list`, whose items the application counts
    /// ([`Tree::set_list_count`]) - as many as it likes - through `rows`, a
    /// pool of rows built once, each `row_height` layout units high: item
    /// `i` is shown by row `i` mod the pool's size, and as the view scrolls
    /// the application binds each row that comes to show another item
    /// ([`Tree::list_row`], [`UiEvent::ListMoved`](crate::UiEvent::ListMoved)).
    /// The pool needs a row more than the view shows, and a few for a
    /// margin.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full.
    fn virtual_list(
        &mut self,
        layout: Layout,
        list: ListId,
        row_height: u16,
        rows: &[Self::Node],
    ) -> Result<Self::Node, Self::Error>;

    /// A text area (contract v19): several lines a person types into,
    /// bound to `field`, `rows` lines high, up to `max_len` bytes kept in
    /// the tree's text store; empty at first, `placeholder` showing.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full, its text store
    /// has too few bytes left, or `field` is already bound.
    fn text_area(
        &mut self,
        field: FieldId,
        rows: u8,
        max_len: u16,
        placeholder: &'static str,
    ) -> Result<Self::Node, Self::Error>;

    /// One paragraph of `runs`, wrapped as one text in `style` (contract
    /// v21): each run in its own weight, italics, theme colour, as code or
    /// as a link reporting its action; each link a tab stop.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full or a run's text
    /// cannot be shown.
    fn rich_text(
        &mut self,
        runs: &'static [Run],
        style: TextStyle,
    ) -> Result<Self::Node, Self::Error>;

    /// A modal dialog holding `child` (contract v20), opened and closed by
    /// the application as `layer` ([`Tree::open`], [`Tree::close`]): in
    /// the middle of the viewport, over the screen, which it veils and
    /// keeps out of reach while open. It takes no place where it is
    /// written.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full or `layer` is
    /// already used.
    fn dialog(&mut self, layer: LayerId, child: Self::Node) -> Result<Self::Node, Self::Error>;

    /// A menu holding `child` - its choices, as buttons - beside `anchor`
    /// (contract v20), opened by the application as `layer`; it closes on
    /// Escape, a press outside it or a choice made.
    ///
    /// # Errors
    ///
    /// As [`dialog`](Self::dialog).
    fn menu(
        &mut self,
        layer: LayerId,
        anchor: Self::Node,
        child: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// A popover holding `child` beside `anchor` (contract v20), opened by
    /// the application as `layer`; it closes on Escape or a press outside
    /// it.
    ///
    /// # Errors
    ///
    /// As [`dialog`](Self::dialog).
    fn popover(
        &mut self,
        layer: LayerId,
        anchor: Self::Node,
        child: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// A tooltip, `text` beside `anchor` (contract v20): shown while the
    /// pointer rests on `anchor` or it has focus after a key.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full or `text` cannot
    /// be shown.
    fn tooltip(
        &mut self,
        anchor: Self::Node,
        text: &'static str,
    ) -> Result<Self::Node, Self::Error>;

    /// Gives `node` a context action (contract v25; React's
    /// `onContextMenu`): reported on a right click on it, or a long press;
    /// a menu or popover for it opened in answer opens at the point
    /// pressed.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when too many nodes have one.
    fn context_action(&mut self, node: Self::Node, action: ActionId) -> Result<(), Self::Error>;

    /// Gives `node` the pointer's shape over it and what it holds
    /// (contract v25; CSS's `cursor`).
    ///
    /// # Errors
    ///
    /// The builder's error: for example when too many nodes have one.
    fn cursor(&mut self, node: Self::Node, cursor: Cursor) -> Result<(), Self::Error>;

    /// One choice of `options` bound to `field`, in a list `picker` shapes
    /// (contract v10).
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full, `field` is
    /// already used, or the options or rows are out of range.
    fn list_picker(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
        picker: Picker,
    ) -> Result<Self::Node, Self::Error>;

    /// The line where the errors the application sets for `field` show
    /// (contract v12), empty until it sets one
    /// ([`Tree::set_field_error`]).
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full, or `field`
    /// already has an error line.
    fn field_error(&mut self, field: FieldId) -> Result<Self::Node, Self::Error>;

    /// The on-screen keyboard (contract v11): rows of keys typing
    /// into the focused text input, laid out for its mode.
    ///
    /// # Errors
    ///
    /// The builder's error: for example when it is full, or the screen
    /// already has a keyboard.
    fn keyboard(&mut self) -> Result<Self::Node, Self::Error>;

    /// One line of text the application sets while the screen shows,
    /// bound to `slot`; empty until it does (contract v1).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full or `slot` is
    /// already used.
    fn slot(&mut self, slot: SlotId) -> Result<Self::Node, Self::Error>;

    /// One of the icons (contract v6): [`icons`]'s, by its static,
    /// so a program links only the icons its screens name.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn icon(&mut self, icon: &'static icons::Icon) -> Result<Self::Node, Self::Error>;

    /// A picture (contract v15): the product's static, so a program links
    /// only the pictures its screens name, fitted to its box by `fit`, with
    /// `alt` saying what it shows - empty for decoration. Its own size but
    /// where `layout` gives a width, a height, a least or a most width (its
    /// gap, padding, alignment and look are not used).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full or `alt` cannot be
    /// shown.
    fn image(
        &mut self,
        picture: &'static Picture,
        fit: Fit,
        alt: &'static str,
        layout: Layout,
    ) -> Result<Self::Node, Self::Error>;

    /// A progress bar bound to `slot` (contract v5): empty until the
    /// application sets how far it shows. Its slot number is distinct from
    /// every slot's.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn progress(&mut self, slot: SlotId) -> Result<Self::Node, Self::Error>;

    /// Static text set in `style`'s role and weight (contract v2):
    /// a title, a caption, a display number.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full, or text the
    /// role's atlases do not draw.
    fn text_styled(
        &mut self,
        text: &'static str,
        style: TextStyle,
    ) -> Result<Self::Node, Self::Error>;

    /// A slot ([`slot`](Self::slot)) whose text is set in `style`'s role
    /// and weight (contract v2).
    ///
    /// # Errors
    ///
    /// As [`slot`](Self::slot).
    fn slot_styled(&mut self, slot: SlotId, style: TextStyle) -> Result<Self::Node, Self::Error>;

    /// Paragraphs the application sets while the screen shows, bound to
    /// `slot` as a slot is, up to `capacity` bytes kept in the tree's text
    /// store, set in `style`'s role and weight (contract v16): wrapped onto
    /// as many lines as they need, a line break starting a new paragraph.
    ///
    /// # Errors
    ///
    /// As [`slot`](Self::slot); a capacity of 0, or more than the tree's
    /// text store has left.
    fn long_text(
        &mut self,
        slot: SlotId,
        capacity: u16,
        style: TextStyle,
    ) -> Result<Self::Node, Self::Error>;

    /// A slot ([`slot`](Self::slot)) shown between `template`'s literals
    /// (contract v14): React's `<Text>Hello, {props.name}!</Text>`. The
    /// literals show from the first frame; the application sets only its
    /// part, at most [`SLOT_CAPACITY`] bytes, and the whole is measured,
    /// wrapped and aligned as one text.
    ///
    /// # Errors
    ///
    /// As [`slot`](Self::slot); literals of more than
    /// [`TEMPLATE_CAPACITY`] bytes together, or that cannot be shown.
    fn slot_template(
        &mut self,
        slot: SlotId,
        template: &'static Template,
    ) -> Result<Self::Node, Self::Error>;

    /// A templated slot ([`slot_template`](Self::slot_template)) set in
    /// `style`'s role and weight (contract v14).
    ///
    /// # Errors
    ///
    /// As [`slot_template`](Self::slot_template), or literals the role's
    /// atlases do not draw.
    fn slot_template_styled(
        &mut self,
        slot: SlotId,
        template: &'static Template,
        style: TextStyle,
    ) -> Result<Self::Node, Self::Error>;

    /// A button in `variant` (contract v3), as [`button`](Self::button)
    /// makes a primary one.
    ///
    /// # Errors
    ///
    /// As [`button`](Self::button).
    fn button_with(
        &mut self,
        label: &'static str,
        action: ActionId,
        variant: ButtonVariant,
    ) -> Result<Self::Node, Self::Error>;

    /// A vertical stack of `children` drawn on `surface` (contract v3): a
    /// card, or a notice.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn column_on(
        &mut self,
        surface: Surface,
        layout: Layout,
        children: &[Self::Node],
    ) -> Result<Self::Node, Self::Error>;

    /// A horizontal row of `children` drawn on `surface` (contract v3): a
    /// header bar.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn row_on(
        &mut self,
        surface: Surface,
        layout: Layout,
        children: &[Self::Node],
    ) -> Result<Self::Node, Self::Error>;

    /// `child`, shown only while the application makes `group` visible:
    /// hidden at first, and then taking no space (contract v1). Several
    /// groups may share a number: they show and hide together.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn group(&mut self, group: GroupId, child: Self::Node) -> Result<Self::Node, Self::Error>;

    /// `when_visible` while the application makes `group` visible, and
    /// `otherwise` while it does not, as at first (contract v1).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn either(
        &mut self,
        group: GroupId,
        when_visible: Self::Node,
        otherwise: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// `child`, shown only while the application does not make `group`
    /// visible: shown at first (contract v13) - React's `!cond && <X/>`.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn unless(&mut self, group: GroupId, child: Self::Node) -> Result<Self::Node, Self::Error>;

    /// `child`, shown only while the application sets `switch` to the case
    /// `when`: on no case at first, and then taking no space (contract
    /// v13) - React's `step === "review" && <X/>`. Several switches may
    /// share a number: they move together, each showing its own case.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn case(
        &mut self,
        switch: SwitchId,
        when: u8,
        child: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// `when_matched` while the application sets `switch` to the case
    /// `when`, and `otherwise` on any other case or none, as at first
    /// (contract v13) - React's `step === "review" ? <X/> : <Y/>`.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full.
    fn case_or(
        &mut self,
        switch: SwitchId,
        when: u8,
        when_matched: Self::Node,
        otherwise: Self::Node,
    ) -> Result<Self::Node, Self::Error>;

    /// A single-line text field bound to `field`, made as `input` says: its
    /// placeholder, the most bytes it takes and which characters (contract
    /// v1).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full, `field` is already
    /// used, or `input`'s `max_len` is out of range.
    fn input(&mut self, field: FieldId, input: Input) -> Result<Self::Node, Self::Error>;

    /// A box the person ticks or unticks, labelled `label`, bound to
    /// `field`; unticked at first (contract v1).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full or `field` is
    /// already used.
    fn checkbox(&mut self, field: FieldId, label: &'static str) -> Result<Self::Node, Self::Error>;

    /// One choice of `options`, one per row, bound to `field`; nothing
    /// chosen at first (contract v1).
    ///
    /// # Errors
    ///
    /// The builder's error, for example when it is full, `field` is already
    /// used, or there are no options or more than the runtime offers.
    fn radio_group(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
    ) -> Result<Self::Node, Self::Error>;
}

/// Text inputs created through the builder are bound to fields numbered
/// 1, 2, 3… in creation order, which for children-first construction is
/// document order; one whose number a contract v1 control already holds is
/// refused ([`UiError::DuplicateField`]), as v1's controls are. Applications can equally read a value through the node
/// handle with [`Tree::node_value`].
impl<const N: usize, const T: usize> UiBuilder for Tree<N, T> {
    type Node = NodeId;
    type Error = UiError;

    fn column(&mut self, layout: Layout, children: &[NodeId]) -> Result<NodeId, UiError> {
        self.add_with_children(ContainerKind::Column, layout.style(), children)
    }

    fn row(&mut self, layout: Layout, children: &[NodeId]) -> Result<NodeId, UiError> {
        self.add_with_children(ContainerKind::Row, layout.style(), children)
    }

    fn text(&mut self, text: &'static str) -> Result<NodeId, UiError> {
        Tree::text(self, text)
    }

    fn button(&mut self, label: &'static str, action: ActionId) -> Result<NodeId, UiError> {
        Tree::button(self, label, action)
    }

    fn text_input(&mut self, placeholder: Option<&'static str>) -> Result<NodeId, UiError> {
        let field = self.next_field();
        self.free_field(field)?;
        Tree::text_input(self, field, placeholder.unwrap_or(""))
    }

    fn spacer(&mut self, size: Option<u16>) -> Result<NodeId, UiError> {
        match size {
            None => Tree::spacer(self),
            Some(px) => {
                let px = Length::Fixed(u32::from(px));
                self.add(NodeKind::Spacer, Style::new().width(px).height(px))
            }
        }
    }

    fn spacer_space(&mut self, space: Space) -> Result<NodeId, UiError> {
        self.add(NodeKind::Spacer, Style::new().size_space(space))
    }

    fn button_look(
        &mut self,
        label: &'static str,
        action: ActionId,
        variant: ButtonVariant,
        look: Look,
    ) -> Result<NodeId, UiError> {
        let id = Tree::button_with(self, label, action, variant)?;
        let node = self.node_mut(id)?;
        node.style = node.style.look(look);
        Ok(id)
    }

    fn scroll_view(&mut self, layout: Layout, child: NodeId) -> Result<NodeId, UiError> {
        UiBuilder::scroll_view_axes(self, layout, ScrollAxes::Vertical, child)
    }

    fn scroll_view_axes(
        &mut self,
        layout: Layout,
        axes: ScrollAxes,
        child: NodeId,
    ) -> Result<NodeId, UiError> {
        let mut style = layout.style();
        // The room its parent leaves along each axis it scrolls, unless it
        // is given a size there.
        if axes.vertical() && style.height == Length::Auto {
            style = style.height(Length::Fill);
        }
        if axes.horizontal() && style.width == Length::Auto {
            style = style.width(Length::Fill);
        }
        let id = self.add(
            NodeKind::Scroll {
                x: 0,
                y: 0,
                axes,
                rows: None,
            },
            style,
        )?;
        self.append(id, child)?;
        Ok(id)
    }

    fn virtual_list(
        &mut self,
        layout: Layout,
        list: ListId,
        row_height: u16,
        rows: &[NodeId],
    ) -> Result<NodeId, UiError> {
        let pool = UiBuilder::column(self, Layout::new(), rows)?;
        let view = UiBuilder::scroll_view(self, layout, pool)?;
        if let NodeKind::Scroll { rows, .. } = &mut self.node_mut(view)?.kind {
            *rows = Some(crate::Rows {
                list,
                count: 0,
                height: u32::from(row_height),
                first: 0,
            });
        }
        Ok(view)
    }

    fn text_area(
        &mut self,
        field: FieldId,
        rows: u8,
        max_len: u16,
        placeholder: &'static str,
    ) -> Result<NodeId, UiError> {
        if self.find_field(field).is_some() {
            return Err(UiError::DuplicateField(field));
        }
        Tree::text_area(self, field, rows, u32::from(max_len), placeholder)
    }

    fn rich_text(&mut self, runs: &'static [Run], style: TextStyle) -> Result<NodeId, UiError> {
        let links = crate::rich::links(runs);
        self.add(
            NodeKind::Rich {
                runs,
                links,
                link: 0,
            },
            Style::DEFAULT.text(style),
        )
    }

    fn dialog(&mut self, layer: LayerId, child: NodeId) -> Result<NodeId, UiError> {
        Tree::add_layer(self, LayerKind::Dialog, Some(layer), None, child)
    }

    fn menu(&mut self, layer: LayerId, anchor: NodeId, child: NodeId) -> Result<NodeId, UiError> {
        Tree::add_layer(self, LayerKind::Menu, Some(layer), Some(anchor), child)
    }

    fn popover(
        &mut self,
        layer: LayerId,
        anchor: NodeId,
        child: NodeId,
    ) -> Result<NodeId, UiError> {
        Tree::add_layer(self, LayerKind::Popover, Some(layer), Some(anchor), child)
    }

    fn tooltip(&mut self, anchor: NodeId, text: &'static str) -> Result<NodeId, UiError> {
        let line = self.add(NodeKind::Text(text), Style::DEFAULT)?;
        Tree::add_layer(self, LayerKind::Tooltip, None, Some(anchor), line)
    }

    fn context_action(&mut self, node: NodeId, action: ActionId) -> Result<(), UiError> {
        self.set_context_action(node, action)
    }

    fn cursor(&mut self, node: NodeId, cursor: Cursor) -> Result<(), UiError> {
        self.set_cursor(node, cursor)
    }

    fn slot(&mut self, slot: SlotId) -> Result<NodeId, UiError> {
        if self.find_slot(slot).is_some() || self.find_progress(slot).is_some() {
            return Err(UiError::DuplicateSlot(slot));
        }
        Tree::slot(self, slot)
    }

    fn icon(&mut self, icon: &'static icons::Icon) -> Result<NodeId, UiError> {
        Tree::icon(self, icon)
    }

    fn image(
        &mut self,
        picture: &'static Picture,
        fit: Fit,
        alt: &'static str,
        layout: Layout,
    ) -> Result<NodeId, UiError> {
        let node = Tree::image(self, picture, fit, alt)?;
        // Its size alone.
        let sized = Layout {
            width: layout.width,
            height: layout.height,
            min_width: layout.min_width,
            max_width: layout.max_width,
            ..Layout::new()
        };
        self.set_style(node, sized.style())?;
        Ok(node)
    }

    fn progress(&mut self, slot: SlotId) -> Result<NodeId, UiError> {
        if self.find_slot(slot).is_some() || self.find_progress(slot).is_some() {
            return Err(UiError::DuplicateSlot(slot));
        }
        Tree::progress(self, slot)
    }

    fn text_styled(&mut self, text: &'static str, style: TextStyle) -> Result<NodeId, UiError> {
        self.add(NodeKind::Text(text), Style::DEFAULT.text(style))
    }

    fn slot_styled(&mut self, slot: SlotId, style: TextStyle) -> Result<NodeId, UiError> {
        let id = UiBuilder::slot(self, slot)?;
        let node = self.node_mut(id)?;
        node.style = node.style.text(style);
        Ok(id)
    }

    fn long_text(
        &mut self,
        slot: SlotId,
        capacity: u16,
        style: TextStyle,
    ) -> Result<NodeId, UiError> {
        if self.find_slot(slot).is_some() || self.find_progress(slot).is_some() {
            return Err(UiError::DuplicateSlot(slot));
        }
        let id = Tree::long_text(self, slot, u32::from(capacity))?;
        let node = self.node_mut(id)?;
        node.style = node.style.text(style);
        Ok(id)
    }

    fn slot_template(
        &mut self,
        slot: SlotId,
        template: &'static Template,
    ) -> Result<NodeId, UiError> {
        if self.find_slot(slot).is_some() || self.find_progress(slot).is_some() {
            return Err(UiError::DuplicateSlot(slot));
        }
        Tree::slot_template(self, slot, template)
    }

    fn slot_template_styled(
        &mut self,
        slot: SlotId,
        template: &'static Template,
        style: TextStyle,
    ) -> Result<NodeId, UiError> {
        // The literals in the role's atlases, before the node is made.
        NodeKind::Slot {
            slot,
            text: nowayos_bounded::BoundedStr::new(),
            template: Some(template),
        }
        .check_text_in(&style)?;
        let id = UiBuilder::slot_template(self, slot, template)?;
        let node = self.node_mut(id)?;
        node.style = node.style.text(style);
        Ok(id)
    }

    fn button_with(
        &mut self,
        label: &'static str,
        action: ActionId,
        variant: ButtonVariant,
    ) -> Result<NodeId, UiError> {
        Tree::button_with(self, label, action, variant)
    }

    fn column_on(
        &mut self,
        surface: Surface,
        layout: Layout,
        children: &[NodeId],
    ) -> Result<NodeId, UiError> {
        let style = layout.style().surface(surface);
        self.add_with_children(ContainerKind::Column, style, children)
    }

    fn row_on(
        &mut self,
        surface: Surface,
        layout: Layout,
        children: &[NodeId],
    ) -> Result<NodeId, UiError> {
        let style = layout.style().surface(surface);
        self.add_with_children(ContainerKind::Row, style, children)
    }

    fn group(&mut self, group: GroupId, child: NodeId) -> Result<NodeId, UiError> {
        Tree::group(self, group, child)
    }

    fn either(
        &mut self,
        group: GroupId,
        when_visible: NodeId,
        otherwise: NodeId,
    ) -> Result<NodeId, UiError> {
        Tree::either(self, group, when_visible, otherwise)
    }

    fn unless(&mut self, group: GroupId, child: NodeId) -> Result<NodeId, UiError> {
        Tree::unless(self, group, child)
    }

    fn case(&mut self, switch: SwitchId, when: u8, child: NodeId) -> Result<NodeId, UiError> {
        Tree::case(self, switch, when, child)
    }

    fn case_or(
        &mut self,
        switch: SwitchId,
        when: u8,
        when_matched: NodeId,
        otherwise: NodeId,
    ) -> Result<NodeId, UiError> {
        Tree::case_or(self, switch, when, when_matched, otherwise)
    }

    fn input(&mut self, field: FieldId, input: Input) -> Result<NodeId, UiError> {
        self.free_field(field)?;
        Tree::input(self, field, input)
    }

    fn checkbox(&mut self, field: FieldId, label: &'static str) -> Result<NodeId, UiError> {
        self.free_field(field)?;
        Tree::checkbox(self, field, label)
    }

    fn radio_group(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
    ) -> Result<NodeId, UiError> {
        self.free_field(field)?;
        Tree::radio_group(self, field, options)
    }

    fn list_picker(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
        picker: Picker,
    ) -> Result<NodeId, UiError> {
        self.free_field(field)?;
        Tree::list_picker(self, field, options, picker.placeholder, picker.rows)
    }

    fn keyboard(&mut self) -> Result<NodeId, UiError> {
        Tree::keyboard(self)
    }

    fn field_error(&mut self, field: FieldId) -> Result<NodeId, UiError> {
        if self
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::FieldError { field: f, .. } if f == field))
        {
            return Err(UiError::SecondFieldError(field));
        }
        Tree::field_error(self, field)
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// `Ok` unless a value-holding control is already bound to `field`.
    fn free_field(&self, field: FieldId) -> Result<(), UiError> {
        if self.has_field(field) {
            Err(UiError::DuplicateField(field))
        } else {
            Ok(())
        }
    }

    /// The field number for the next text input: one more than the number
    /// of text inputs so far.
    fn next_field(&self) -> FieldId {
        let inputs = self
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::TextInput { .. }))
            .count();
        FieldId(u16::try_from(inputs.saturating_add(1)).unwrap_or(u16::MAX))
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
    use crate::{MAX_DEPTH, TextMetrics};
    use nowayos_display::Rect;

    #[test]
    fn builds_children_first_in_document_order() {
        let mut t: Tree<16> = Tree::new();
        let title = UiBuilder::text(&mut t, "Title").unwrap();
        let email = UiBuilder::text_input(&mut t, Some("Email")).unwrap();
        let phone = UiBuilder::text_input(&mut t, None).unwrap();
        let back = UiBuilder::button(&mut t, "Back", ActionId::new(1)).unwrap();
        let next = UiBuilder::button(&mut t, "Next", ActionId::new(2)).unwrap();
        let row = UiBuilder::row(&mut t, Layout::new().gap(4), &[back, next]).unwrap();
        let root = UiBuilder::column(
            &mut t,
            Layout::new().padding(8),
            &[title, email, phone, row],
        )
        .unwrap();
        t.set_root(root).unwrap();
        let order: std::vec::Vec<NodeId> = t.paint_order().map(|(id, _)| id).collect();
        assert_eq!(order, [root, title, email, phone, row, back, next]);
        // Fields are numbered in document order.
        assert_eq!(t.find_field(FieldId(1)), Some(email));
        assert_eq!(t.find_field(FieldId(2)), Some(phone));
        assert_eq!(t.get(root).unwrap().style().padding.left, 8);
        assert_eq!(t.get(row).unwrap().style().gap, 4);
    }

    #[test]
    fn refuses_reused_or_attached_children_without_changing_anything() {
        let mut t: Tree<8> = Tree::new();
        let a = UiBuilder::text(&mut t, "a").unwrap();
        assert_eq!(
            UiBuilder::column(&mut t, Layout::new(), &[a, a]),
            Err(UiError::AlreadyAttached(a))
        );
        assert_eq!(t.len(), 1);
        let col = UiBuilder::column(&mut t, Layout::new(), &[a]).unwrap();
        assert_eq!(
            UiBuilder::row(&mut t, Layout::new(), &[a]),
            Err(UiError::AlreadyAttached(a))
        );
        t.set_root(col).unwrap();
        assert_eq!(
            UiBuilder::row(&mut t, Layout::new(), &[col]),
            Err(UiError::AlreadyAttached(col))
        );
        let ghost = NodeId(40);
        assert_eq!(
            UiBuilder::row(&mut t, Layout::new(), &[ghost]),
            Err(UiError::InvalidNode(ghost))
        );
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn full_arena_is_an_error() {
        let mut t: Tree<2> = Tree::new();
        let a = UiBuilder::text(&mut t, "a").unwrap();
        let b = UiBuilder::text(&mut t, "b").unwrap();
        assert_eq!(
            UiBuilder::column(&mut t, Layout::new(), &[a, b]),
            Err(UiError::TreeFull { capacity: 2 })
        );
        assert!(t.get(a).unwrap().parent().is_none());
    }

    #[test]
    fn depth_limit_applies() {
        let mut t: Tree<32> = Tree::new();
        let mut node = UiBuilder::text(&mut t, "leaf").unwrap();
        for _ in 0..MAX_DEPTH {
            node = UiBuilder::column(&mut t, Layout::new(), &[node]).unwrap();
        }
        assert_eq!(
            UiBuilder::column(&mut t, Layout::new(), &[node]),
            Err(UiError::TooDeep)
        );
    }

    #[test]
    fn fixed_spacer_takes_space_only_along_the_main_axis() {
        let mut t: Tree<8> = Tree::new();
        let a = UiBuilder::text(&mut t, "ab").unwrap();
        let gap = UiBuilder::spacer(&mut t, Some(30)).unwrap();
        let b = UiBuilder::text(&mut t, "cd").unwrap();
        let root = UiBuilder::column(&mut t, Layout::new(), &[a, gap, b]).unwrap();
        t.set_root(root).unwrap();
        t.set_style(root, Style::new().align(crate::Align::Start))
            .unwrap();
        t.layout(Rect::new(0, 0, 100, 100), &TextMetrics::new(6, 10))
            .unwrap();
        assert_eq!(t.get(b).unwrap().rect(), Rect::new(0, 40, 12, 10));
        // The spacer does not widen the column's natural size.
        let mut u: Tree<8> = Tree::new();
        let s = UiBuilder::spacer(&mut u, Some(500)).unwrap();
        let inner = UiBuilder::column(&mut u, Layout::new(), &[s]).unwrap();
        let outer = UiBuilder::row(&mut u, Layout::new(), &[inner]).unwrap();
        u.set_root(outer).unwrap();
        u.set_style(outer, Style::new().align(crate::Align::Start))
            .unwrap();
        u.layout(Rect::new(0, 0, 100, 600), &TextMetrics::new(6, 10))
            .unwrap();
        assert_eq!(u.get(inner).unwrap().rect().size.width, 0);
        assert_eq!(u.get(inner).unwrap().rect().size.height, 500);
    }

    #[test]
    fn flexible_spacer_fills() {
        let mut t: Tree<8> = Tree::new();
        let top = UiBuilder::text(&mut t, "top").unwrap();
        let fill = UiBuilder::spacer(&mut t, None).unwrap();
        let bottom = UiBuilder::text(&mut t, "end").unwrap();
        let root = UiBuilder::column(&mut t, Layout::new(), &[top, fill, bottom]).unwrap();
        t.set_root(root).unwrap();
        t.layout(Rect::new(0, 0, 50, 100), &TextMetrics::new(6, 10))
            .unwrap();
        assert_eq!(t.get(bottom).unwrap().rect().origin.y, 90);
    }

    #[test]
    fn action_ids_are_const_constructible() {
        const SUBMIT: ActionId = ActionId::new(7);
        assert_eq!(SUBMIT.get(), 7);
        assert_eq!(SUBMIT, ActionId(7));
    }
}
