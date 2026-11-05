// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`UiError`]: everything that can go wrong building or laying out a tree.

use core::fmt;

use nowayos_bounded::CapacityError;
use nowayos_display::Rect;

use crate::{FieldId, GroupId, NodeId, SlotId, SwitchId};

/// Why a tree operation failed. The tree is left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UiError {
    /// The tree already holds its maximum number of nodes.
    TreeFull {
        /// The tree's node capacity.
        capacity: usize,
    },
    /// The id does not name a node in this tree.
    InvalidNode(NodeId),
    /// Only `Column`, `Row` and `Stack` nodes can have children.
    NotAContainer(NodeId),
    /// The node already has a parent, or is the root.
    AlreadyAttached(NodeId),
    /// The operation would make a node its own ancestor.
    WouldCycle,
    /// The operation would nest nodes deeper than
    /// [`MAX_DEPTH`](crate::MAX_DEPTH).
    TooDeep,
    /// No root has been set.
    NoRoot,
    /// The node cannot take keyboard focus.
    NotFocusable(NodeId),
    /// No node is bound to this field id: no text input, or for an error,
    /// no error line ([`NodeKind::FieldError`](crate::NodeKind::FieldError)).
    UnknownField(FieldId),
    /// The node is not of the kind the operation needs (for example
    /// `set_text` on a `Column`).
    WrongKind(NodeId),
    /// A value is longer than a text input can hold.
    ValueTooLong(CapacityError),
    /// Text is longer than its slot can hold
    /// ([`SLOT_CAPACITY`](crate::SLOT_CAPACITY)).
    TextTooLong(CapacityError),
    /// No slot has this id.
    UnknownSlot(SlotId),
    /// No group has this id.
    UnknownGroup(GroupId),
    /// No switch is bound to this number.
    UnknownSwitch(SwitchId),
    /// A text input's `max_len` is 0 or more than it can hold.
    MaxLenOutOfRange {
        /// The most it may be ([`INPUT_CAPACITY`](crate::INPUT_CAPACITY)).
        max: usize,
    },
    /// A value holds a character the text input's mode refuses
    /// ([`InputMode`](crate::InputMode)). Like the errors below, it never
    /// says which character.
    NotInInputMode,
    /// A value-holding control - a text input, checkbox or radio group -
    /// is already bound to this field: the builder gives each of a screen's
    /// fields its own number ([`builder`](crate::builder)).
    DuplicateField(FieldId),
    /// A slot with this id is already in the tree: the builder gives each
    /// slot of a screen its own number ([`builder`](crate::builder)).
    DuplicateSlot(SlotId),

    /// Text for a node contains a control character, such as a line break
    /// or a tab. A node shows exactly one line of text, laid out and drawn
    /// as that one line. Like the two below, it never says which character.
    ControlCharacter,
    /// Text for a node contains a character that would not be seen: a
    /// zero-width space or joiner, a soft hyphen, a direction mark or
    /// override, a variation selector - Unicode's default-ignorable code
    /// points ([`crate::text::is_invisible`]).
    InvisibleCharacter,
    /// Text for a node contains a character outside the glyph atlases'
    /// set, which no font of the UI draws: today anything outside printable
    /// Latin-1 ([`crate::text::is_printable`]).
    NoGlyph,
    /// A text input, a field's error, a checkbox or a radio group was given
    /// the display role: only a text, a slot or a button may have
    /// it, as their text alone is checked against its 46 characters.
    RoleNotAllowed,
    /// A radio group was given more options than it may offer
    /// ([`MAX_OPTIONS`](crate::MAX_OPTIONS)).
    TooManyOptions {
        /// The most it may offer.
        max: usize,
    },
    /// A radio group was given no options.
    NoOptions,
    /// The radio group has no option with that number.
    NoSuchOption,
    /// The back stack already holds as many screens as it can
    /// ([`Navigator`](crate::Navigator)); the current screen stays.
    BackStackFull {
        /// How many screens it holds.
        capacity: usize,
    },
    /// The viewport is outside the range layout and drawing support: its
    /// width and height must be at most
    /// [`MAX_VIEWPORT_SIZE`](crate::MAX_VIEWPORT_SIZE), and every edge
    /// within [`MAX_VIEWPORT_OFFSET`](crate::MAX_VIEWPORT_OFFSET) of the
    /// origin.
    ViewportOutOfRange(Rect),

    /// The tree's text store has fewer bytes left than a long text asks
    /// for: a `Tree<N, T>` holds `T`.
    TextStoreFull {
        /// The store's size, in bytes.
        capacity: usize,
    },
    /// No virtual list of this number in the tree.
    UnknownList(crate::ListId),
    /// No layer of this number in the tree (#10076).
    UnknownLayer(crate::LayerId),
    /// The layer is inside a hidden group or a closed layer, so cannot
    /// open.
    LayerHidden(crate::LayerId),
    /// [`MAX_OPEN`](crate::layer::MAX_OPEN) layers are open already.
    TooManyLayers,
    /// A layer of this number is in the tree already.
    DuplicateLayer(crate::LayerId),
    /// [`MAX_RUNNING`](crate::motion::MAX_RUNNING) animations are running
    /// already (#10086).
    TooManyAnimations,
    /// A long text holds from 1 to
    /// [`MAX_LONG_TEXT`](crate::long_text::MAX_LONG_TEXT) bytes.
    LongTextOutOfRange,
    /// A list picker shows from 3 to 8 rows at once.
    RowsOutOfRange,
    /// A tree holds at most one on-screen keyboard.
    SecondKeyboard,
    /// The field already has an error line.
    SecondFieldError(FieldId),
}

impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TreeFull { capacity } => write!(f, "UI tree is full ({capacity} nodes)"),
            Self::InvalidNode(id) => write!(f, "no node {id:?} in this tree"),
            Self::NotAContainer(id) => write!(f, "node {id:?} cannot have children"),
            Self::AlreadyAttached(id) => write!(f, "node {id:?} already has a parent"),
            Self::WouldCycle => f.write_str("a node cannot become its own ancestor"),
            Self::TooDeep => f.write_str("UI tree would nest too deeply"),
            Self::NoRoot => f.write_str("UI tree has no root"),
            Self::NotFocusable(id) => write!(f, "node {id:?} cannot take focus"),
            Self::UnknownField(field) => write!(f, "no node bound to {field:?}"),
            Self::WrongKind(id) => write!(f, "node {id:?} is the wrong kind for this operation"),
            Self::ValueTooLong(err) => write!(f, "value too long: {err}"),
            Self::TextTooLong(err) => write!(f, "text too long for its slot: {err}"),
            Self::UnknownSlot(slot) => write!(f, "no slot {slot:?}"),
            Self::UnknownGroup(group) => write!(f, "no group {group:?}"),
            Self::UnknownSwitch(switch) => write!(f, "no switch {switch:?}"),
            Self::MaxLenOutOfRange { max } => {
                write!(f, "a text input takes from 1 to {max} bytes")
            }
            Self::NotInInputMode => {
                f.write_str("text contains a character the input's mode does not take")
            }
            Self::DuplicateField(field) => write!(f, "{field:?} is already bound in the tree"),
            Self::DuplicateSlot(slot) => write!(f, "{slot:?} is already in the tree"),
            Self::ControlCharacter => {
                f.write_str("text contains a control character; a node shows one line")
            }
            Self::InvisibleCharacter => f.write_str("text contains an invisible character"),
            Self::NoGlyph => f.write_str("text contains a character the fonts cannot draw"),
            Self::TooManyOptions { max } => {
                write!(
                    f,
                    "a radio group or list picker offers at most {max} options"
                )
            }
            Self::NoOptions => {
                f.write_str("a radio group or list picker needs at least one option")
            }
            Self::RoleNotAllowed => {
                f.write_str("only a text, a slot or a button may be in the display role")
            }
            Self::NoSuchOption => f.write_str("the radio group has no such option"),
            Self::BackStackFull { capacity } => {
                write!(f, "the back stack is full ({capacity} screens)")
            }
            Self::ViewportOutOfRange(rect) => write!(f, "viewport {rect:?} is out of range"),

            Self::TextStoreFull { capacity } => {
                write!(f, "the tree's text store ({capacity} bytes) is full")
            }
            Self::UnknownList(list) => write!(f, "no list {list:?}"),
            Self::UnknownLayer(layer) => write!(f, "no layer {layer:?}"),
            Self::LayerHidden(layer) => write!(f, "{layer:?} is inside what does not show"),
            Self::DuplicateLayer(layer) => write!(f, "{layer:?} is already in the tree"),
            Self::TooManyAnimations => write!(
                f,
                "at most {} animations run at once",
                crate::motion::MAX_RUNNING
            ),
            Self::TooManyLayers => write!(
                f,
                "at most {} layers are open at once",
                crate::layer::MAX_OPEN
            ),
            Self::LongTextOutOfRange => write!(
                f,
                "a long text holds from 1 to {} bytes",
                crate::long_text::MAX_LONG_TEXT
            ),
            Self::RowsOutOfRange => f.write_str("a list picker shows from 3 to 8 rows"),
            Self::SecondKeyboard => f.write_str("a screen has at most one keyboard"),
            Self::SecondFieldError(field) => write!(f, "{field:?} already has an error line"),
        }
    }
}

impl core::error::Error for UiError {}
