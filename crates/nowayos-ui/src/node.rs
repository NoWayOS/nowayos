// SPDX-License-Identifier: MIT OR Apache-2.0
//! Node identities, kinds and styles.

use nowayos_bounded::{BoundedStr, Wipe};
use nowayos_display::{Insets, Point, Rect, Size};

use crate::{TextMetrics, UiError};

/// The most bytes a [`NodeKind::TextInput`] value can hold: its storage, and
/// the largest `max_len` an input may have ([`Input::max_len`]).
pub const INPUT_CAPACITY: usize = 64;

/// The most bytes the text of a [`NodeKind::Slot`] can hold: as many as a
/// text input, so a review screen can show any answer.
///
/// Provisional: a per-slot capacity may replace this constant.
pub const SLOT_CAPACITY: usize = 64;

/// The most options a [`NodeKind::RadioGroup`] may offer.
pub const MAX_OPTIONS: usize = 8;

/// The most options a [`NodeKind::ListPicker`] may offer.
pub const MAX_LIST_OPTIONS: usize = 64;

/// The fewest and most rows a [`NodeKind::ListPicker`] shows at once,
/// and how many unless it is told.
pub const LIST_ROWS: core::ops::RangeInclusive<u8> = 3..=8;

/// How many rows a [`NodeKind::ListPicker`] shows unless it is told.
pub const LIST_ROWS_DEFAULT: u8 = 5;

/// WCAG 2.2's minimum target size, in pixels (success criterion 2.5.8, 24 by
/// 24 CSS pixels), counting one pixel as one CSS pixel until displays are
/// scaled: the least minimum target size a NoWayOS theme may set,
/// and the one [`TextMetrics::new`](crate::TextMetrics::new) starts with.
/// Layout gives every focusable control at least the theme's minimum each
/// way ([`TextMetrics::min_target`](crate::TextMetrics::min_target)), and
/// each option of a radio group a row that size. A style that fixes a
/// smaller size, or a parent too small, still makes a control smaller.
pub const MIN_TARGET: u32 = 24;

/// Padding given to checkboxes and radio groups created with
/// [`Tree::checkbox`](crate::Tree::checkbox) and
/// [`Tree::radio_group`](crate::Tree::radio_group): room for the focus ring
/// clear of the marks.
pub const CHOICE_PADDING: Insets = Insets::symmetric(6, 3);

/// How many characters wide a text input is when its width is
/// [`Length::Auto`] (unless its placeholder is wider).
pub const INPUT_WIDTH_CHARS: u32 = 16;

/// Padding given to buttons created with [`Tree::button`](crate::Tree::button):
/// 24 each side, as the showcase's buttons have, and 8 above and
/// below - a theme's button height ([`TextMetrics::button_height`](crate::TextMetrics::button_height))
/// usually makes them taller.
pub const BUTTON_PADDING: Insets = Insets::symmetric(24, 8);

/// Padding given to text inputs created with
/// [`Tree::text_input`](crate::Tree::text_input).
pub const INPUT_PADDING: Insets = Insets::symmetric(6, 5);

/// The on-screen keyboard's padding, given by
/// [`Tree::keyboard`](crate::Tree::keyboard): each key is inset as much
/// again from its place on the grid, so keys stand twice this apart and
/// from the keyboard's edge.
pub const KEYBOARD_PADDING: Insets = Insets::uniform(3);

/// How much taller than a line of its text a key of the on-screen keyboard
/// is, in layout units, and never below the minimum target size.
pub const KEY_SPACE: u32 = 12;

/// Names a node in one [`Tree`](crate::Tree). Ids are handed out in creation
/// order and are only meaningful in the tree that created them, until that
/// tree is cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NodeId(pub(crate) u16);

impl NodeId {
    /// The node's position in creation order, from 0.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a button does, as a number the application defines. Returned in
/// [`UiEvent::Action`](crate::UiEvent::Action) when the button is activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionId(pub u16);

impl ActionId {
    /// The action numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The action's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Which application value a text input edits, as a number the application
/// defines. Read the value back with [`Tree::value`](crate::Tree::value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldId(pub u16);

impl FieldId {
    /// The field numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The field's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Which application-supplied text a [`NodeKind::Slot`] shows, as a number
/// the application defines. Set it with
/// [`Tree::set_slot`](crate::Tree::set_slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId(pub u16);

impl SlotId {
    /// The slot numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The slot's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Which group a [`NodeKind::Group`] is, as a number the application
/// defines: the handle it shows or hides the group by
/// ([`Tree::set_visible`](crate::Tree::set_visible)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub u16);

/// Which switch a [`NodeKind::Switch`] is, as a number the application
/// defines: the handle it sets the switch's case by
/// ([`Tree::set_case`](crate::Tree::set_case)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SwitchId(pub u16);

impl SwitchId {
    /// The switch numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The switch's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl GroupId {
    /// The group numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The group's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Which characters a text input takes, and so which layout of the on-screen
/// keyboard it shows (builder contract v1): a key for any other character is
/// refused, and so is a value holding one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InputMode {
    /// Any character the glyph atlases draw ([`text::is_printable`]).
    ///
    /// [`text::is_printable`]: crate::text::is_printable
    #[default]
    Text,
    /// The digits `0` to `9` only: a number, a PIN, a telephone number.
    Digits,
    /// An address such as an email address: printable ASCII without the
    /// space.
    Email,
}

impl InputMode {
    /// Whether an input in this mode takes `ch`.
    #[must_use]
    pub fn accepts(self, ch: char) -> bool {
        match self {
            Self::Text => crate::text::is_printable(ch),
            Self::Digits => ch.is_ascii_digit(),
            Self::Email => ch.is_ascii_graphic(),
        }
    }
}

/// How a text input is made (builder contract v1): its placeholder, the most
/// bytes it takes - 1 to [`INPUT_CAPACITY`] - and its [`InputMode`]. Build it
/// with the `const` methods:
///
/// ```
/// use nowayos_ui::{Input, InputMode};
///
/// const PHONE: Input = Input::new().placeholder("Digits only").max_len(15).mode(InputMode::Digits);
/// assert_eq!(PHONE.max_len_bytes(), 15);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Input {
    placeholder: &'static str,
    max_len: u16,
    mode: InputMode,
}

impl Input {
    /// No placeholder, [`INPUT_CAPACITY`] bytes, [`InputMode::Text`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            placeholder: "",
            // 64: a u16 holds it (a const fn cannot use `TryFrom`).
            max_len: INPUT_CAPACITY as u16,
            mode: InputMode::Text,
        }
    }

    /// With `placeholder` shown, dimmed, while the value is empty.
    #[must_use]
    pub const fn placeholder(self, placeholder: &'static str) -> Self {
        Self {
            placeholder,
            ..self
        }
    }

    /// Taking at most `bytes` bytes: 1 to [`INPUT_CAPACITY`], or the input
    /// is refused when it is made ([`UiError::MaxLenOutOfRange`]).
    #[must_use]
    pub const fn max_len(self, bytes: u16) -> Self {
        Self {
            max_len: bytes,
            ..self
        }
    }

    /// In `mode`.
    #[must_use]
    pub const fn mode(self, mode: InputMode) -> Self {
        Self { mode, ..self }
    }

    /// The placeholder.
    #[must_use]
    pub const fn placeholder_text(self) -> &'static str {
        self.placeholder
    }

    /// The most bytes it takes.
    #[must_use]
    pub const fn max_len_bytes(self) -> u16 {
        self.max_len
    }

    /// Its mode.
    #[must_use]
    pub const fn input_mode(self) -> InputMode {
        self.mode
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

/// A size along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Length {
    /// The content's natural size (text width, children plus gaps…).
    #[default]
    Auto,
    /// Exactly this many pixels. Along a container's cross axis it is capped
    /// at the space available.
    Fixed(u32),
    /// Along the parent's main axis: an equal share of the space left after
    /// the other children. Along the cross axis: all of it.
    Fill,
}

/// How a container places children across its main axis (horizontally in
/// a `Column`, vertically in a `Row`, both ways in a `Stack`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    /// At the start (left or top), at natural size.
    Start,
    /// Centred, at natural size.
    Center,
    /// At the end (right or bottom), at natural size.
    End,
    /// Stretched across the whole container — flexbox's default.
    #[default]
    Stretch,
}

/// Where a `Row`'s or `Column`'s children sit along its main axis when they
/// leave room over - CSS's `justify-content`. A child that fills
/// ([`Length::Fill`]) takes the room, and then this changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Justify {
    /// From the start, the room left at the end. The default.
    #[default]
    Start,
    /// The room shared before the first child and after the last.
    Center,
    /// The room before the first child.
    End,
    /// The room shared between the children, none before the first or
    /// after the last; a lone child sits at the start.
    SpaceBetween,
}

/// One of the theme's colours, by its role,
/// which a screen names instead of a colour value: a theme
/// switched to another - the large-text, high-contrast one - recolours the
/// screen, and only the theme's colours, whose contrast its tests check,
/// are ever drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colour {
    /// The screen's background.
    Background,
    /// Ordinary text.
    Text,
    /// Text that matters less: hints and small print.
    TextMuted,
    /// A text input's placeholder.
    Placeholder,
    /// A primary button's fill: the brand's colour.
    Button,
    /// A primary button's label, on its fill.
    ButtonText,
    /// A text input's fill.
    Input,
    /// A text input's border.
    InputBorder,
    /// The focus ring.
    Focus,
    /// An error.
    Error,
}

impl Colour {
    /// Every colour, in a fixed order.
    pub const ALL: [Self; 10] = [
        Self::Background,
        Self::Text,
        Self::TextMuted,
        Self::Placeholder,
        Self::Button,
        Self::ButtonText,
        Self::Input,
        Self::InputBorder,
        Self::Focus,
        Self::Error,
    ];
}

/// How a container or a button is drawn beyond its kind's own look:
/// a fill, a border and how wide it is, the radius of its corners - widths
/// in layout units - and a button's label colour. Each colour is one of the
/// theme's ([`Colour`]); what is not named is the kind's own (a surface's,
/// a button's variant's, or none). Text inside a filled container is drawn
/// over the fill, in the colour its own style names, or ordinary text's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Look {
    /// Its fill.
    pub fill: Option<Colour>,
    /// Its border's colour: with no width named, 1 unit wide.
    pub border: Option<Colour>,
    /// Its border's width, in layout units: with no colour named, a text
    /// input's border colour.
    pub border_width: Option<u32>,
    /// The radius of its corners, in layout units.
    pub radius: Option<u32>,
    /// A button's label colour.
    pub text: Option<Colour>,
}

impl Look {
    /// Nothing named: the kind's own look.
    pub const NONE: Self = Self {
        fill: None,
        border: None,
        border_width: None,
        radius: None,
        text: None,
    };

    /// With `colour` as its fill.
    #[must_use]
    pub const fn fill(self, colour: Colour) -> Self {
        Self {
            fill: Some(colour),
            ..self
        }
    }

    /// With a border in `colour`.
    #[must_use]
    pub const fn border(self, colour: Colour) -> Self {
        Self {
            border: Some(colour),
            ..self
        }
    }

    /// With a border `units` wide.
    #[must_use]
    pub const fn border_width(self, units: u32) -> Self {
        Self {
            border_width: Some(units),
            ..self
        }
    }

    /// With corners of radius `units`.
    #[must_use]
    pub const fn radius(self, units: u32) -> Self {
        Self {
            radius: Some(units),
            ..self
        }
    }

    /// A button's label in `colour`.
    #[must_use]
    pub const fn text(self, colour: Colour) -> Self {
        Self {
            text: Some(colour),
            ..self
        }
    }

    /// Whether it draws a border: a colour or a width named.
    #[must_use]
    pub const fn has_border(&self) -> bool {
        self.border.is_some() || self.border_width.is_some()
    }

    /// Whether it changes how a container is drawn: a fill, a border or
    /// corners named.
    #[must_use]
    pub const fn draws(&self) -> bool {
        self.fill.is_some() || self.has_border() || self.radius.is_some()
    }
}

/// What a line of text is for: its size in the theme's type scale,
/// from the largest to the smallest. The theme gives each role its size, an
/// atlas at each device scale, and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextRole {
    /// A number or a short reference to read across a room - a queue
    /// number, an amount - in digits, capitals and a few signs only
    /// ([`text::is_display`](crate::text::is_display)). Bold.
    Display,
    /// A screen's title. Bold.
    Title,
    /// A heading: a header bar's or a section's. Bold.
    Heading,
    /// Text, fields and buttons: the default.
    #[default]
    Body,
    /// A field's or a tile's label, smaller than body.
    Label,
    /// Small print and footnotes.
    Caption,
}

impl TextRole {
    /// Every role, largest first.
    pub const ALL: [Self; 6] = [
        Self::Display,
        Self::Title,
        Self::Heading,
        Self::Body,
        Self::Label,
        Self::Caption,
    ];

    /// The weight the role takes unless a style names another: Bold for
    /// display, title and heading, Regular for the rest.
    #[must_use]
    pub const fn default_weight(self) -> Weight {
        match self {
            Self::Display | Self::Title | Self::Heading => Weight::Bold,
            Self::Body | Self::Label | Self::Caption => Weight::Regular,
        }
    }
}

/// A text's weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Weight {
    /// Regular, 400.
    #[default]
    Regular,
    /// Bold, 700.
    Bold,
}

/// A progress bar's height, in layout units: Showcase → Showcase
/// Look's 12.
pub const PROGRESS_HEIGHT: u32 = 12;

/// A progress bar's whole, in its own units: it shows thousandths.
pub const PROGRESS_WHOLE: u16 = 1000;

/// The most lines a text may wrap onto.
pub const MAX_LINES: u8 = 8;

/// Where a text's lines sit across its node.
/// Each line of a wrapped text is placed on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    /// Against the node's start edge: its left, in the scripts NoWayOS
    /// draws. The default.
    #[default]
    Start,
    /// Centred, any odd pixel of space left on the end side.
    Center,
    /// Against the node's end edge: its right.
    End,
}

/// A theme's spacing token: a
/// gap, padding or spacer named by its size instead of a number of layout
/// units, so it grows with a theme that spaces more - the large-text theme
/// by half again ([`Spacing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    /// The smallest: 4 units in the light theme.
    Xs,
    /// 8 units in the light theme.
    Sm,
    /// 16 units in the light theme.
    Md,
    /// 24 units in the light theme.
    Lg,
    /// The largest: 32 units in the light theme.
    Xl,
}

/// What each [`Space`] measures in a theme, in layout units, which
/// layout gives the device scale's pixels as it does every length.
///
/// ```
/// use nowayos_ui::{Space, Spacing};
///
/// assert_eq!(Spacing::STANDARD.of(Space::Md), 16);
/// assert_eq!(Spacing::LARGE.of(Space::Md), 24);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Spacing {
    /// [`Space::Xs`].
    pub xs: u32,
    /// [`Space::Sm`].
    pub sm: u32,
    /// [`Space::Md`].
    pub md: u32,
    /// [`Space::Lg`].
    pub lg: u32,
    /// [`Space::Xl`].
    pub xl: u32,
}

impl Spacing {
    /// The light theme's: 4, 8, 16, 24 and 32.
    /// What [`crate::TextMetrics`] lays out with unless a theme
    /// gives others.
    pub const STANDARD: Self = Self {
        xs: 4,
        sm: 8,
        md: 16,
        lg: 24,
        xl: 32,
    };

    /// The large-text theme's: half again - 6, 12, 24, 36 and 48.
    pub const LARGE: Self = Self {
        xs: 6,
        sm: 12,
        md: 24,
        lg: 36,
        xl: 48,
    };

    /// What `space` measures, in layout units.
    #[must_use]
    pub const fn of(&self, space: Space) -> u32 {
        match space {
            Space::Xs => self.xs,
            Space::Sm => self.sm,
            Space::Md => self.md,
            Space::Lg => self.lg,
            Space::Xl => self.xl,
        }
    }
}

/// The spacing tokens a [`Style`] names in place of numbers, which
/// layout measures against the theme's [`Spacing`] each time it runs: a
/// theme switched to one that spaces more lays the same tree out wider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SpaceTokens {
    /// The gap between a container's children, in place of [`Style::gap`].
    pub gap: Option<Space>,
    /// The padding on every side, in place of [`Style::padding`].
    pub padding: Option<Space>,
    /// A fixed width and height - a spacer's size - in place of
    /// [`Style::width`] and [`Style::height`].
    pub size: Option<Space>,
}

impl SpaceTokens {
    /// None: the style's own numbers stand.
    pub const NONE: Self = Self {
        gap: None,
        padding: None,
        size: None,
    };
}

/// How a node's text is set: its role in the type scale, its
/// weight, the most lines it may wrap onto and where its lines sit
/// across the node. The default is body text, Regular, on one line,
/// against the start - what every node had before.
///
/// ```
/// use nowayos_ui::{TextAlign, TextRole, TextStyle, Weight};
///
/// assert_eq!(TextStyle::new(TextRole::Title).weight, Weight::Bold);
/// assert_eq!(TextStyle::BODY.bold().weight, Weight::Bold);
/// assert_eq!(TextStyle::default(), TextStyle::BODY);
/// assert_eq!(TextStyle::BODY.lines(3).lines, 3);
/// assert_eq!(TextStyle::BODY.lines(20).lines, 8);
/// assert_eq!(TextStyle::BODY.align, TextAlign::Start);
/// assert_eq!(TextStyle::BODY.align(TextAlign::Center).align, TextAlign::Center);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextStyle {
    /// Its role in the type scale.
    pub role: TextRole,
    /// Its weight.
    pub weight: Weight,
    /// The most lines its text may wrap onto, from 1 - one line, cut if it
    /// does not fit - to [`MAX_LINES`]. Only a text and a slot
    /// wrap: breaks fall after spaces, as [`crate::text::wrap`] says, and
    /// layout gives the node its width first, then the height of the lines
    /// its text takes there.
    pub lines: u8,
    /// Where each of its lines sits across the node: against the
    /// start, centred or against the end. A line too wide for the node is
    /// cut and starts at the start, whatever the alignment.
    pub align: TextAlign,
    /// Its colour, one of the theme's; `None` for the colour of
    /// its role where it stands.
    pub colour: Option<Colour>,
}

impl TextStyle {
    /// Body text, Regular.
    pub const BODY: Self = Self::new(TextRole::Body);

    /// The number of distinct styles: every role in both weights.
    pub const COUNT: usize = 12;

    /// `role`, in its own weight ([`TextRole::default_weight`]).
    #[must_use]
    pub const fn new(role: TextRole) -> Self {
        Self {
            role,
            weight: role.default_weight(),
            lines: 1,
            align: TextAlign::Start,
            colour: None,
        }
    }

    /// The same, wrapping onto at most `lines` lines: from 1 to
    /// [`MAX_LINES`], a number outside taken as the nearer of the two.
    #[must_use]
    pub const fn lines(self, lines: u8) -> Self {
        let lines = if lines < 1 {
            1
        } else if lines > MAX_LINES {
            MAX_LINES
        } else {
            lines
        };
        Self { lines, ..self }
    }

    /// The same, each line set `align` across the node.
    #[must_use]
    pub const fn align(self, align: TextAlign) -> Self {
        Self { align, ..self }
    }

    /// The same, in the theme's `colour`.
    #[must_use]
    pub const fn colour(self, colour: Colour) -> Self {
        Self {
            colour: Some(colour),
            ..self
        }
    }

    /// Whether its text wraps: onto more than one line.
    #[must_use]
    pub const fn wraps(self) -> bool {
        self.lines > 1
    }

    /// The same, in Bold.
    #[must_use]
    pub const fn bold(self) -> Self {
        Self {
            weight: Weight::Bold,
            ..self
        }
    }

    /// The same, in Regular.
    #[must_use]
    pub const fn regular(self) -> Self {
        Self {
            weight: Weight::Regular,
            ..self
        }
    }

    /// Its place among the [`COUNT`](Self::COUNT) styles: role by role,
    /// Regular before Bold.
    #[must_use]
    pub const fn index(self) -> usize {
        let regular = match self.role {
            TextRole::Display => 0,
            TextRole::Title => 2,
            TextRole::Heading => 4,
            TextRole::Body => 6,
            TextRole::Label => 8,
            TextRole::Caption => 10,
        };
        match self.weight {
            Weight::Regular => regular,
            Weight::Bold => regular | 1,
        }
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::BODY
    }
}

/// How a button is drawn: its weight among the buttons of a
/// screen, so that Cancel never looks like Check in. The theme gives each
/// its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ButtonVariant {
    /// The one main action: filled with the brand colour.
    #[default]
    Primary,
    /// Another action: outlined in the brand colour.
    Secondary,
    /// A minor action, such as Cancel: its label alone, in the brand
    /// colour.
    Quiet,
}

/// What a notice tells, which picks its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Something done or ready: green.
    Success,
    /// Something to note: amber.
    Caution,
    /// Something that needs care: a warning banner, its text in Bold.
    Warning,
}

/// A surface a container is drawn on: filled, with rounded
/// corners and a border as the theme gives them, behind its children. Text
/// on it is drawn in the surface's text colour, blended over its fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// A card: content grouped on a lighter fill.
    Card,
    /// A header bar: the brand colour across the top of a screen, its text
    /// and buttons in the colour drawn on the brand.
    Header,
    /// A notice in a tone.
    Notice(Tone),
}

/// Layout properties of a node. Build with the `const` methods:
///
/// ```
/// use nowayos_ui::{Align, Length, Style};
///
/// let style = Style::new().padding(16).gap(8).align(Align::Center).height(Length::Fill);
/// assert_eq!(style.gap, 8);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    /// Width.
    pub width: Length,
    /// Height.
    pub height: Length,
    /// Space inside the node's edges.
    pub padding: Insets,
    /// Space between a container's children along its main axis.
    pub gap: u32,
    /// How a container aligns its children across its main axis.
    pub align: Align,
    /// How the node's text is set: its role and weight. Body text,
    /// Regular, by default.
    pub text: TextStyle,
    /// A button's variant: primary by default. Other kinds ignore
    /// it.
    pub variant: ButtonVariant,
    /// The surface a container is drawn on, if any. Other kinds
    /// ignore it.
    pub surface: Option<Surface>,
    /// Spacing tokens standing for its gap, padding or size: each
    /// one named takes the place of that number when the tree is laid out.
    pub tokens: SpaceTokens,
    /// Where a container's children sit along its main axis when they
    /// leave room over.
    pub justify: Justify,
    /// Its least width, in layout units: 0 for none.
    pub min_width: u32,
    /// Its greatest width, in layout units, if any. A least width
    /// above it wins, as in CSS.
    pub max_width: Option<u32>,
    /// How a container or a button is drawn beyond its kind's own look.
    pub look: Look,
}

impl Style {
    /// Auto size, no padding, no gap, stretch alignment, body text.
    pub const DEFAULT: Self = Self {
        width: Length::Auto,
        height: Length::Auto,
        padding: Insets::ZERO,
        gap: 0,
        align: Align::Stretch,
        text: TextStyle::BODY,
        variant: ButtonVariant::Primary,
        surface: None,
        tokens: SpaceTokens::NONE,
        justify: Justify::Start,
        min_width: 0,
        max_width: None,
        look: Look::NONE,
    };

    /// The default style.
    #[must_use]
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// With this width.
    #[must_use]
    pub const fn width(self, width: Length) -> Self {
        Self { width, ..self }
    }

    /// With this height.
    #[must_use]
    pub const fn height(self, height: Length) -> Self {
        Self { height, ..self }
    }

    /// With a fixed width and height.
    #[must_use]
    pub const fn size(self, width: u32, height: u32) -> Self {
        Self {
            width: Length::Fixed(width),
            height: Length::Fixed(height),
            ..self
        }
    }

    /// With the same padding on all sides.
    #[must_use]
    pub const fn padding(self, all: u32) -> Self {
        Self {
            padding: Insets::uniform(all),
            ..self
        }
    }

    /// With `horizontal` padding left and right, `vertical` top and bottom.
    #[must_use]
    pub const fn padding_xy(self, horizontal: u32, vertical: u32) -> Self {
        Self {
            padding: Insets::symmetric(horizontal, vertical),
            ..self
        }
    }

    /// With these insets as padding.
    #[must_use]
    pub const fn insets(self, padding: Insets) -> Self {
        Self { padding, ..self }
    }

    /// With this gap between children.
    #[must_use]
    pub const fn gap(self, gap: u32) -> Self {
        Self { gap, ..self }
    }

    /// With the theme's `space` between children, in place of
    /// [`gap`](Self::gap).
    #[must_use]
    pub const fn gap_space(self, space: Space) -> Self {
        Self {
            tokens: SpaceTokens {
                gap: Some(space),
                ..self.tokens
            },
            ..self
        }
    }

    /// With the theme's `space` as padding on every side, in place
    /// of [`padding`](Self::padding).
    #[must_use]
    pub const fn padding_space(self, space: Space) -> Self {
        Self {
            tokens: SpaceTokens {
                padding: Some(space),
                ..self.tokens
            },
            ..self
        }
    }

    /// With the theme's `space` as a fixed width and height: a
    /// spacer's size, in place of [`size`](Self::size).
    #[must_use]
    pub const fn size_space(self, space: Space) -> Self {
        Self {
            tokens: SpaceTokens {
                size: Some(space),
                ..self.tokens
            },
            ..self
        }
    }

    /// With children placed along its main axis as `justify` says.
    #[must_use]
    pub const fn justify(self, justify: Justify) -> Self {
        Self { justify, ..self }
    }

    /// At least `units` wide.
    #[must_use]
    pub const fn min_width(self, units: u32) -> Self {
        Self {
            min_width: units,
            ..self
        }
    }

    /// At most `units` wide.
    #[must_use]
    pub const fn max_width(self, units: u32) -> Self {
        Self {
            max_width: Some(units),
            ..self
        }
    }

    /// Drawn with `look` beyond its kind's own.
    #[must_use]
    pub const fn look(self, look: Look) -> Self {
        Self { look, ..self }
    }

    /// With this cross-axis alignment for children.
    #[must_use]
    pub const fn align(self, align: Align) -> Self {
        Self { align, ..self }
    }

    /// With its text set in `text`'s role and weight.
    #[must_use]
    pub const fn text(self, text: TextStyle) -> Self {
        Self { text, ..self }
    }

    /// A button in `variant`.
    #[must_use]
    pub const fn variant(self, variant: ButtonVariant) -> Self {
        Self { variant, ..self }
    }

    /// A container drawn on `surface`.
    #[must_use]
    pub const fn surface(self, surface: Surface) -> Self {
        Self {
            surface: Some(surface),
            ..self
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a node is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// Children top to bottom.
    Column,
    /// Children left to right.
    Row,
    /// Children on top of each other; later children are drawn on top.
    Stack,
    /// A scroll view: one child - a column - laid out at its
    /// natural height and shown through the view's own rectangle, moved up
    /// by `offset` pixels; nothing inside is drawn or hit outside the view.
    /// At most one in a tree, never inside another
    /// ([`UiBuilder::scroll_view`](crate::builder::UiBuilder::scroll_view)).
    /// Page Up and Page Down scroll it a page at a time, a mouse's wheel
    /// three lines a notch, a touch dragged on it scrolls it, and focus
    /// moving onto a control it hides scrolls that control into view.
    Scroll {
        /// How far its child is moved up, in pixels: kept by layout
        /// between 0 and how much taller the child is than the view.
        offset: u32,
    },
    /// A single line of fixed text, every character one the glyph atlases
    /// draw and a person can see ([`NodeKind::check_text`]).
    Text(&'static str),
    /// A button that reports `action` when activated.
    Button {
        /// Its label, one line.
        label: &'static str,
        /// What activating it reports.
        action: ActionId,
    },
    /// A single-line text field bound to `field`, taking at most `max_len`
    /// bytes of the characters its `mode` accepts: a key for any other
    /// character, or one more than fits, is refused.
    TextInput {
        /// Which application value it edits.
        field: FieldId,
        /// Shown, dimmed, while the value is empty. One line.
        placeholder: &'static str,
        /// The current value. One line, at most `max_len` bytes.
        value: BoundedStr<INPUT_CAPACITY>,
        /// The most bytes it takes: 1 to [`INPUT_CAPACITY`].
        max_len: u8,
        /// Which characters it takes.
        mode: InputMode,
    },
    /// A single line of text the application sets while the screen shows -
    /// an answer to review, a reference number - held in the tree's own
    /// bounded storage, bound to `slot`. It starts empty. Its text, like
    /// every text a node shows, must be printable
    /// ([`crate::text::check_printable`]).
    Slot {
        /// Which application text it shows.
        slot: SlotId,
        /// The text. One line, at most [`SLOT_CAPACITY`] bytes.
        text: BoundedStr<SLOT_CAPACITY>,
    },
    /// A box the person ticks or unticks, its label beside it, bound to
    /// `field`. Tapping or clicking anywhere on it - the box or the label -
    /// or pressing Space while it has focus toggles it. A tick shows it is
    /// ticked, not colour alone.
    Checkbox {
        /// Which application value it sets.
        field: FieldId,
        /// Its label, one line.
        label: &'static str,
        /// Whether it is ticked.
        checked: bool,
    },
    /// Options, one line each, of which the person chooses one, bound to
    /// `field`: at most [`MAX_OPTIONS`], stacked one per row, and one tab
    /// stop. Nothing is chosen at first. Tapping or clicking an option
    /// chooses it; while the group has focus, the arrow keys choose the next
    /// or previous option, and Space the first if none is chosen. A dot
    /// shows the choice, not colour alone.
    RadioGroup {
        /// Which application value it sets.
        field: FieldId,
        /// The options' labels, one line each.
        options: &'static [&'static str],
        /// The chosen option, from 0, if any.
        selected: Option<u8>,
    },
    /// One choice of up to [`MAX_LIST_OPTIONS`] options:
    /// a header line showing the choice, or the
    /// placeholder until one is made, over `rows` rows of a list that
    /// scrolls inside itself. One tab stop: the arrows move a highlight -
    /// the list scrolling to keep it in view - and Enter or Space chooses
    /// the highlighted option; a tap on a row chooses it, and a touch
    /// dragged on the list scrolls it.
    ListPicker {
        /// Which application value it sets.
        field: FieldId,
        /// The options' labels, one line each.
        options: &'static [&'static str],
        /// Shown in the header until an option is chosen.
        placeholder: &'static str,
        /// The chosen option, from 0, if any.
        selected: Option<u8>,
        /// How many rows of options show at once, within [`LIST_ROWS`].
        rows: u8,
        /// The first option showing, from 0.
        top: u8,
        /// The highlighted option, from 0: what Enter or Space chooses.
        cursor: u8,
    },
    /// The on-screen keyboard:
    /// [`KEY_ROWS`](crate::keyboard::KEY_ROWS) rows of keys, drawn as
    /// buttons, laid out for the focused text input's mode
    /// ([`keyboard::layout`](crate::keyboard::layout)). A key pressed and
    /// released sends its canonical key event to that input, as a key of a
    /// keyboard would, without taking focus from it; while no text input
    /// has focus its keys type nothing. It is not a tab stop. It holds no
    /// copy of what is typed, and offers no suggestions: only its layout
    /// and its Shift. At most one in a tree.
    Keyboard {
        /// The mode of the text input it last typed for, or would: its
        /// layout's.
        mode: InputMode,
        /// Whether the next letter is a capital: Shift pressed on the
        /// keyboard, which goes off once a letter is typed.
        shift: bool,
    },
    /// Where the error the application reports for `field` shows, in words
    /// ([`Tree::set_field_error`](crate::Tree::set_field_error)): put it
    /// beside the field. Validation stays in the application; this only
    /// shows what it is told. Empty until an error is set, but it always
    /// takes one line's height, so an error can appear and go without a
    /// new layout. Drawn with a marker shape as well as in the error
    /// colour.
    FieldError {
        /// The field whose errors it shows.
        field: FieldId,
        /// The error, if there is one. One line.
        message: Option<&'static str>,
    },
    /// Content the application shows or hides, bound to `group`: one child,
    /// shown only while the group is visible - React's `cond && <X/>` - or
    /// two, the first shown while it is visible and the second while it is
    /// not - `cond ? <X/> : <Y/>`; or one shown only while the group is
    /// hidden - `!cond && <X/>` (contract v13). Created hidden, with its
    /// children, by [`Tree::group`](crate::Tree::group),
    /// [`Tree::either`](crate::Tree::either) or
    /// [`Tree::unless`](crate::Tree::unless); nothing can be appended to
    /// it. A group takes no space of its own: in a `Row`, `Column` or
    /// `Stack` the child it shows is laid out as if it stood in the group's
    /// place, with its own style, and a group showing nothing takes no
    /// space and no gap. The child it does not show is neither drawn,
    /// focusable nor hit, and keeps its values.
    Group {
        /// Which group it is.
        group: GroupId,
        /// Whether the group is visible.
        visible: bool,
        /// While the group is this, its first child shows; otherwise its
        /// second, if any. `true` but for [`Tree::unless`](crate::Tree::unless).
        when: bool,
    },
    /// Content shown while a switch the application sets is on one case,
    /// bound to `switch`: one child, shown only while the switch's case is
    /// `when` - React's `step === "review" && <X/>` - or two, the first
    /// shown while it is and the second while it is not -
    /// `step === "review" ? <X/> : <Y/>` (contract v13). Every switch bound
    /// to one number moves together, each showing its child for its own
    /// case, so siblings over one prop show one case at a time. Created on
    /// no case, with its children, by [`Tree::case`](crate::Tree::case) or
    /// [`Tree::case_or`](crate::Tree::case_or); laid out, drawn and hit as
    /// a group is.
    Switch {
        /// Which switch it is.
        switch: SwitchId,
        /// The switch's case, or `None` for none: at first, and after a wipe.
        case: Option<u8>,
        /// The case its first child shows for.
        when: u8,
    },
    /// An icon: one of Lucide's, drawn from its alpha mask at the
    /// device scale and tinted with the colour of text where it stands,
    /// [`nowayos_icons::SIZE`] units square.
    Icon(&'static nowayos_icons::Icon),
    /// A bar showing how far something has gone - a pump's
    /// fuelling, say - as a fraction in thousandths the application sets,
    /// bound to `slot` like a slot's text ([`Tree::set_progress`](crate::Tree::set_progress)).
    /// [`PROGRESS_HEIGHT`] high; it takes its container's width. It starts
    /// empty.
    Progress {
        /// Which application value it shows.
        slot: SlotId,
        /// How far, in thousandths: 0 to 1000.
        permille: u16,
    },
    /// Empty space: with `Fill` lengths it takes whatever room is left;
    /// with `Fixed` lengths it is a fixed gap. Inside a `Row` or `Column` it
    /// only takes space along the main axis — it never widens its parent
    /// across it.
    Spacer,
}

impl NodeKind {
    /// Whether this kind shows at most one of its children, by a value the
    /// application sets: a group or a switch.
    #[must_use]
    pub const fn is_conditional(&self) -> bool {
        matches!(self, Self::Group { .. } | Self::Switch { .. })
    }

    /// Whether this kind can have children.
    #[must_use]
    pub const fn is_container(&self) -> bool {
        matches!(
            self,
            Self::Column | Self::Row | Self::Stack | Self::Scroll { .. }
        )
    }

    /// Whether this kind can take keyboard focus.
    #[must_use]
    pub const fn is_focusable(&self) -> bool {
        matches!(
            self,
            Self::Button { .. }
                | Self::TextInput { .. }
                | Self::Checkbox { .. }
                | Self::RadioGroup { .. }
                | Self::ListPicker { .. }
        )
    }

    /// The field this control edits, if it is one that does.
    #[must_use]
    pub const fn field(&self) -> Option<FieldId> {
        match self {
            Self::TextInput { field, .. }
            | Self::Checkbox { field, .. }
            | Self::RadioGroup { field, .. }
            | Self::ListPicker { field, .. } => Some(*field),
            _ => None,
        }
    }

    /// Checks everything [`Tree::add`](crate::Tree::add) checks: the texts
    /// ([`check_text`](Self::check_text)), a radio group's bounds and a text
    /// input's.
    ///
    /// # Errors
    ///
    /// As [`check_text`](Self::check_text); for a radio group,
    /// [`UiError::TooManyOptions`] beyond [`MAX_OPTIONS`],
    /// [`UiError::NoOptions`] for none, and [`UiError::NoSuchOption`] if
    /// the chosen option is not one of them; for a text input,
    /// [`UiError::MaxLenOutOfRange`] for a `max_len` of 0 or above
    /// [`INPUT_CAPACITY`], [`UiError::ValueTooLong`] for a value longer
    /// than it, and [`UiError::NotInInputMode`] for a value holding a
    /// character its mode refuses.
    pub fn check(&self) -> Result<(), UiError> {
        if let Self::TextInput {
            value,
            max_len,
            mode,
            ..
        } = self
        {
            let max = usize::from(*max_len);
            if max == 0 || max > INPUT_CAPACITY {
                return Err(UiError::MaxLenOutOfRange {
                    max: INPUT_CAPACITY,
                });
            }
            if value.len() > max {
                return Err(UiError::ValueTooLong(nowayos_bounded::CapacityError {
                    capacity: max,
                    required: value.len(),
                }));
            }
            self.check_text()?;
            if !value.chars().all(|ch| mode.accepts(ch)) {
                return Err(UiError::NotInInputMode);
            }
        }
        if let Self::RadioGroup {
            options, selected, ..
        } = self
        {
            if options.len() > MAX_OPTIONS {
                return Err(UiError::TooManyOptions { max: MAX_OPTIONS });
            }
            if options.is_empty() {
                return Err(UiError::NoOptions);
            }
            if selected.is_some_and(|option| usize::from(option) >= options.len()) {
                return Err(UiError::NoSuchOption);
            }
        }
        if let Self::ListPicker {
            options,
            selected,
            rows,
            top,
            cursor,
            ..
        } = self
        {
            if options.len() > MAX_LIST_OPTIONS {
                return Err(UiError::TooManyOptions {
                    max: MAX_LIST_OPTIONS,
                });
            }
            if options.is_empty() {
                return Err(UiError::NoOptions);
            }
            if !LIST_ROWS.contains(rows) {
                return Err(UiError::RowsOutOfRange);
            }
            let beyond = |option: u8| usize::from(option) >= options.len();
            if selected.is_some_and(beyond) || beyond(*top) || beyond(*cursor) {
                return Err(UiError::NoSuchOption);
            }
        }
        self.check_text()
    }

    /// [`check_text`](Self::check_text), and, in the display role, every
    /// character one the display role's atlases draw
    /// ([`text::check_display`](crate::text::check_display)). Only a
    /// text, a slot and a button may be in the display role: a text input,
    /// a field's error, a checkbox and a radio group show text a person
    /// types or the application sets through calls that do not know the
    /// role, so none of them may be.
    ///
    /// # Errors
    ///
    /// As [`check_text`](Self::check_text); [`UiError::NoGlyph`];
    /// [`UiError::RoleNotAllowed`] for a kind the display role is not for.
    pub fn check_text_in(&self, style: &TextStyle) -> Result<(), UiError> {
        self.check_text()?;
        if style.role != TextRole::Display {
            return Ok(());
        }
        use crate::text::check_display;
        match self {
            Self::Text(text) | Self::Button { label: text, .. } => check_display(text),
            Self::Slot { text, .. } => check_display(text),
            Self::TextInput { .. }
            | Self::Checkbox { .. }
            | Self::RadioGroup { .. }
            | Self::ListPicker { .. }
            | Self::Keyboard { .. }
            | Self::FieldError { .. } => Err(UiError::RoleNotAllowed),
            Self::Column
            | Self::Row
            | Self::Stack
            | Self::Scroll { .. }
            | Self::Group { .. }
            | Self::Switch { .. }
            | Self::Spacer
            | Self::Progress { .. }
            | Self::Icon(_) => Ok(()),
        }
    }

    /// Checks that every text this kind shows can be shown: one line,
    /// nothing invisible, and every character one the glyph atlases draw
    /// ([`crate::text::check_printable`]) - a text, a label, a placeholder,
    /// an option, an error message, a value or a slot's text.
    ///
    /// # Errors
    ///
    /// For the first character that fails, [`UiError::ControlCharacter`]
    /// (a line break, a tab, NUL, DEL or a C1 control: layout measures one
    /// line, and `embedded-graphics` would start a new one at a `'\n'`,
    /// drawing outside the node), [`UiError::InvisibleCharacter`] (a
    /// zero-width space, a direction override…) or [`UiError::NoGlyph`]
    /// (outside the atlases' set). None of them names the character.
    pub fn check_text(&self) -> Result<(), UiError> {
        use crate::text::check_printable;
        match self {
            Self::Text(text) | Self::Button { label: text, .. } => check_printable(text),
            Self::TextInput {
                placeholder, value, ..
            } => check_printable(placeholder).and_then(|()| check_printable(value)),
            Self::Slot { text, .. } => check_printable(text),
            Self::Checkbox { label, .. } => check_printable(label),
            Self::RadioGroup { options, .. } => options
                .iter()
                .try_for_each(|option| check_printable(option)),
            Self::ListPicker {
                options,
                placeholder,
                ..
            } => check_printable(placeholder).and_then(|()| {
                options
                    .iter()
                    .try_for_each(|option| check_printable(option))
            }),
            Self::FieldError { message, .. } => message.map_or(Ok(()), check_printable),
            // Its labels are fixed and printable (`keyboard`'s tests).
            Self::Column
            | Self::Row
            | Self::Stack
            | Self::Scroll { .. }
            | Self::Group { .. }
            | Self::Switch { .. }
            | Self::Spacer
            | Self::Progress { .. }
            | Self::Keyboard { .. }
            | Self::Icon(_) => Ok(()),
        }
    }
}

/// The node kinds that can have children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    /// Children top to bottom.
    Column,
    /// Children left to right.
    Row,
    /// Children on top of each other.
    Stack,
}

impl From<ContainerKind> for NodeKind {
    fn from(kind: ContainerKind) -> Self {
        match kind {
            ContainerKind::Column => Self::Column,
            ContainerKind::Row => Self::Row,
            ContainerKind::Stack => Self::Stack,
        }
    }
}

/// One node of a [`Tree`](crate::Tree): its kind, style, links and, after
/// [`Tree::layout`](crate::Tree::layout), its position on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) style: Style,
    pub(crate) parent: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,
    /// Natural size, computed by the measure pass of layout.
    pub(crate) intrinsic: Size,
    /// Position, computed by the arrange pass of layout.
    pub(crate) rect: Rect,
    /// The style's padding in pixels at the device scale it was laid out
    /// at, set by the arrange pass of layout.
    pub(crate) padding: Insets,
}

impl Node {
    pub(crate) const fn new(kind: NodeKind, style: Style) -> Self {
        Self {
            kind,
            style,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            intrinsic: Size::ZERO,
            rect: Rect::EMPTY,
            padding: Insets::ZERO,
        }
    }

    /// What the node is.
    #[must_use]
    pub const fn kind(&self) -> &NodeKind {
        &self.kind
    }

    /// Its style.
    #[must_use]
    pub const fn style(&self) -> &Style {
        &self.style
    }

    /// Where it was placed by the last layout (empty before the first).
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// Where its content goes after the last layout: its rectangle less its
    /// style's padding, in pixels at the device scale it was laid out at.
    /// A renderer draws a node's text, and a radio group its
    /// options, here.
    #[must_use]
    pub fn content(&self) -> Rect {
        self.rect.inset(self.padding)
    }

    /// Its parent, if attached.
    #[must_use]
    pub const fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// Whether it can take keyboard focus.
    #[must_use]
    pub const fn is_focusable(&self) -> bool {
        self.kind.is_focusable()
    }

    /// For a radio group, where option `index` is: the rows of its padded
    /// area, shared evenly (each at least the minimum target size high,
    /// after layout: [`MIN_TARGET`] or the theme's larger one). `None` for
    /// another kind or an index beyond its options.
    #[must_use]
    pub fn option_rect(&self, index: usize) -> Option<Rect> {
        let NodeKind::RadioGroup { options, .. } = &self.kind else {
            return None;
        };
        if index >= options.len() {
            return None;
        }
        let area = self.content();
        let count = u64::try_from(options.len()).ok()?;
        let at = u64::try_from(index).ok()?;
        let height = u64::from(area.size.height);
        let top = area.top();
        let row_top = i64::try_from(height.checked_mul(at)?.checked_div(count)?).ok()?;
        let row_bottom =
            i64::try_from(height.checked_mul(at.checked_add(1)?)?.checked_div(count)?).ok()?;
        Some(Rect::from_edges(
            area.left(),
            top.saturating_add(row_top),
            area.right(),
            top.saturating_add(row_bottom),
        ))
    }

    /// For a list picker, its row `index`: row 0 its header, rows 1
    /// to its `rows` the options showing - its padded area shared evenly.
    /// `None` for another kind or a row beyond them.
    #[must_use]
    pub fn list_row(&self, index: usize) -> Option<Rect> {
        let NodeKind::ListPicker { rows, .. } = &self.kind else {
            return None;
        };
        let count = u64::from(*rows).checked_add(1)?;
        let at = u64::try_from(index).ok()?;
        if at >= count {
            return None;
        }
        let area = self.content();
        let height = u64::from(area.size.height);
        let top = area.top();
        let row_top = i64::try_from(height.checked_mul(at)?.checked_div(count)?).ok()?;
        let row_bottom =
            i64::try_from(height.checked_mul(at.checked_add(1)?)?.checked_div(count)?).ok()?;
        Some(Rect::from_edges(
            area.left(),
            top.saturating_add(row_top),
            area.right(),
            top.saturating_add(row_bottom),
        ))
    }

    /// For a list picker, the option whose row holds `point`, if any:
    /// not its header, nor a row past its last option.
    #[must_use]
    pub fn list_option_at(&self, point: Point) -> Option<usize> {
        let NodeKind::ListPicker {
            options, rows, top, ..
        } = &self.kind
        else {
            return None;
        };
        let row = (1..=usize::from(*rows))
            .find(|&row| self.list_row(row).is_some_and(|rect| rect.contains(point)))?;
        let option = usize::from(*top).checked_add(row)?.checked_sub(1)?;
        (option < options.len()).then_some(option)
    }

    /// For the on-screen keyboard, where key `index` of its layout is:
    /// its place on the layout's grid - the padded area shared
    /// evenly into the grid's columns and [`KEY_ROWS`] rows - inset by the
    /// keyboard's padding. `None` for another kind or an index beyond its
    /// keys.
    ///
    /// [`KEY_ROWS`]: crate::keyboard::KEY_ROWS
    #[must_use]
    pub fn key_rect(&self, index: usize) -> Option<Rect> {
        let NodeKind::Keyboard { mode, .. } = &self.kind else {
            return None;
        };
        let layout = crate::keyboard::layout(*mode);
        let key = layout.key(index)?;
        let area = self.content();
        let share = |length: u32, at: u8, of: u8| -> Option<i64> {
            let part = u64::from(length)
                .checked_mul(u64::from(at))?
                .checked_div(u64::from(of))?;
            i64::try_from(part).ok()
        };
        let (width, height) = (area.size.width, area.size.height);
        let rows = crate::keyboard::KEY_ROWS;
        let right = key.column.checked_add(key.span)?;
        let cell = Rect::from_edges(
            area.left()
                .saturating_add(share(width, key.column, layout.columns)?),
            area.top().saturating_add(share(height, key.row, rows)?),
            area.left()
                .saturating_add(share(width, right, layout.columns)?),
            area.top()
                .saturating_add(share(height, key.row.checked_add(1)?, rows)?),
        );
        Some(cell.inset(self.padding))
    }

    /// For the on-screen keyboard, the key holding `point`, if any:
    /// not the space between keys.
    #[must_use]
    pub fn key_at(&self, point: Point) -> Option<usize> {
        let NodeKind::Keyboard { mode, .. } = &self.kind else {
            return None;
        };
        (0..crate::keyboard::layout(*mode).keys().count())
            .find(|&index| self.key_rect(index).is_some_and(|key| key.contains(point)))
    }

    /// For a radio group, the option whose row holds `point`, if any.
    #[must_use]
    pub fn option_at(&self, point: Point) -> Option<usize> {
        let NodeKind::RadioGroup { options, .. } = &self.kind else {
            return None;
        };
        (0..options.len()).find(|&index| {
            self.option_rect(index)
                .is_some_and(|row| row.contains(point))
        })
    }
}

impl Node {
    /// The first text the node shows that is not shown whole at its laid-out
    /// size with `metrics` - the theme's, for the node's own style (each
    /// node's text in its role and weight) - if any: what a fit test
    /// fails on. A text, a slot, a button's label, a text input's
    /// placeholder, a checkbox's label or a radio option wider than its
    /// room; a wrapped text or slot needing more lines than its style
    /// allows, or more than its height holds. The text is the node's own:
    /// a slot's is the application's.
    #[must_use]
    pub fn cut_text(&self, metrics: &TextMetrics) -> Option<&str> {
        let metrics = metrics.for_style(self.style.text);
        let content = self.content();
        let width = content.size.width;
        let marked = width.saturating_sub(metrics.marked_width(""));
        let too_wide = |text: &str, room: u32| metrics.text_width(text) > room;
        match &self.kind {
            NodeKind::Text(text) => self.cut_wrapped(text, &metrics),
            NodeKind::Slot { text, .. } => self.cut_wrapped(text.as_str(), &metrics),
            NodeKind::Button { label: text, .. }
            | NodeKind::TextInput {
                placeholder: text, ..
            } => too_wide(text, width).then_some(*text),
            NodeKind::Checkbox { label, .. } => too_wide(label, marked).then_some(*label),
            NodeKind::RadioGroup { options, .. } => options
                .iter()
                .copied()
                .find(|option| too_wide(option, marked)),
            NodeKind::ListPicker {
                options,
                placeholder,
                ..
            } => options
                .iter()
                .copied()
                .find(|option| too_wide(option, marked))
                .or_else(|| too_wide(placeholder, width).then_some(*placeholder)),
            // A key's label wider than its key, in this layout.
            NodeKind::Keyboard { mode, shift } => crate::keyboard::layout(*mode)
                .keys()
                .enumerate()
                .find(|(index, key)| {
                    self.key_rect(*index)
                        .is_some_and(|rect| too_wide(key.label_for(*shift), rect.size.width))
                })
                .map(|(_, key)| key.label_for(*shift)),
            _ => None,
        }
    }

    /// `text`, if the node does not show it whole: on one line, wider than
    /// its content; wrapped, on more lines than its style allows or its
    /// content's height holds.
    fn cut_wrapped<'t>(&self, text: &'t str, metrics: &TextMetrics) -> Option<&'t str> {
        let content = self.content().size;
        if !self.style.text.wraps() {
            return (metrics.text_width(text) > content.width).then_some(text);
        }
        let needed = crate::text::wrap(text, content.width, metrics).count();
        let allowed = usize::from(self.style.text.lines);
        let tall = u32::try_from(needed)
            .unwrap_or(u32::MAX)
            .saturating_mul(metrics.line_height());
        let cut = needed > allowed
            || tall > content.height
            || crate::text::wrap(text, content.width, metrics)
                .any(|line| metrics.text_width(line) > content.width);
        cut.then_some(text)
    }
}

impl Default for Node {
    fn default() -> Self {
        Self::new(NodeKind::Spacer, Style::DEFAULT)
    }
}

impl Wipe for Node {
    /// Zeroes a text input's value or a slot's text in place, empties a
    /// progress bar, unticks a
    /// checkbox, clears a radio group's choice, forgets a field's error and
    /// hides a group again, as it was built. Other kinds hold no data of the
    /// session (their texts are `&'static str`).
    fn wipe(&mut self) {
        match &mut self.kind {
            NodeKind::TextInput { value, .. } => value.wipe(),
            NodeKind::Slot { text, .. } => text.wipe(),
            NodeKind::Progress { permille, .. } => *permille = 0,
            NodeKind::FieldError { message, .. } => *message = None,
            NodeKind::Checkbox { checked, .. } => *checked = false,
            NodeKind::RadioGroup { selected, .. } => *selected = None,
            NodeKind::ListPicker {
                selected,
                top,
                cursor,
                ..
            } => {
                *selected = None;
                *top = 0;
                *cursor = 0;
            }
            NodeKind::Group { visible, .. } => *visible = false,
            NodeKind::Switch { case, .. } => *case = None,
            // The next person starts at the top.
            NodeKind::Scroll { offset } => *offset = 0,
            // Its layout follows focus; only its Shift is the person's.
            NodeKind::Keyboard { shift, .. } => *shift = false,
            _ => {}
        }
    }
}
