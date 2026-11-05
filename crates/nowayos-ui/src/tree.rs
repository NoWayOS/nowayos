// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Tree`]: the bounded node arena and the operations that build it.

use nowayos_bounded::{BoundedStr, BoundedVec, Wipe};
use nowayos_display::{DirtyRegion, Point, Rect};

use crate::node::{BUTTON_PADDING, INPUT_PADDING};
use crate::text::check_printable;
use crate::{
    ContainerKind, FieldId, GroupId, INPUT_CAPACITY, Input, Length, Node, NodeId, NodeKind, Style,
    SwitchId, TextMetrics, UiError,
};

/// The deepest nodes may be nested: a root plus up to 16 levels below it.
/// Keeps every recursive pass over the tree bounded.
pub const MAX_DEPTH: usize = 16;

/// Rectangles the dirty region tracks before it starts merging them.
const DIRTY_RECTS: usize = 8;

/// A screen's UI: up to `N` nodes (`N` at most 65 536), their layout and
/// interaction state.
///
/// Build it by creating nodes ([`column`](Self::column),
/// [`text`](Self::text), [`button`](Self::button)…), linking them with
/// [`append`](Self::append) and choosing a [`root`](Self::set_root). Nodes
/// can be created in any order and subtrees built bottom-up or top-down.
/// Then call [`layout`](Self::layout), feed input to
/// [`handle`](Self::handle), and render.
///
/// Provisional: this API may change.
#[derive(Debug, Clone)]
pub struct Tree<const N: usize, const T: usize = 0> {
    pub(crate) nodes: BoundedVec<Node, N>,
    pub(crate) root: Option<NodeId>,
    pub(crate) viewport: Rect,
    /// The device scale of the last layout.
    pub(crate) scale: nowayos_display::DeviceScale,
    pub(crate) focus: Option<NodeId>,
    pub(crate) pressed: Option<NodeId>,
    /// The key held down, from 0, while [`pressed`](Self::pressed) is the
    /// on-screen keyboard.
    pub(crate) pressed_key: Option<u8>,
    pub(crate) pointer: Point,
    /// Whether the focused control shows its focus ring: since the
    /// last key, yes; since the last press of the pointer or a touch, no.
    pub(crate) focus_visible: bool,
    /// Whether the pointer was last moved by a mouse or a tablet,
    /// rather than a touch: then a pointer is drawn where it is.
    pub(crate) pointer_shown: bool,
    /// Whether each Shift key is held: left, right.
    pub(crate) shift_keys: [bool; 2],
    /// Whether each Ctrl key is held: left, right.
    pub(crate) ctrl_keys: [bool; 2],
    /// The text input last pressed and where its caret went: a second
    /// press there, with nothing between, is a double click.
    pub(crate) last_click: Option<(NodeId, u8)>,
    pub(crate) dirty: DirtyRegion<DIRTY_RECTS>,
    /// The metrics of the last layout: what a slot's new text is
    /// measured with, so that only the part that changed is repainted.
    pub(crate) metrics: Option<TextMetrics>,
    /// A touch on the scroll view: where it started, the view's
    /// offset then, and whether it has moved far enough to scroll.
    pub(crate) drag: Option<Drag>,
    /// The long texts' bytes: `T` of them (builder contract v16).
    pub(crate) texts: crate::long_text::TextStore<T>,
    /// What scrolls moved of what is drawn, since the last repaint.
    pub(crate) moves: crate::scroll::Moves,
    /// A view flinging on after a touch lifted.
    pub(crate) fling: Option<crate::scroll::Fling>,
    /// A scrollbar's thumb held by the pointer or a finger.
    pub(crate) thumb: Option<crate::scroll::ThumbHold>,
    /// A virtual list whose window of items moved, not yet reported.
    pub(crate) list_moved: Option<crate::ListId>,
    /// The layers open over the screen, and the tooltip showing (#10076).
    pub(crate) layers: crate::layer::Layers,
    /// Transitions and animations running, and how moving nodes are drawn
    /// (#10086).
    pub(crate) motion: crate::motion::Motion,
}

/// A touch that may scroll a scroll view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drag {
    /// Where the touch started.
    pub(crate) from: Point,
    /// The view it scrolls - the innermost under it that can - if any.
    pub(crate) view: Option<NodeId>,
    /// That view's offsets then: right and down.
    pub(crate) offset: (u32, u32),
    /// Whether it has moved past [`DRAG_SLOP`](crate::interact::DRAG_SLOP)
    /// and now scrolls, its press cancelled.
    pub(crate) scrolling: bool,
    /// The list picker it started on, and its first option showing then:
    /// the drag scrolls that list, not the scroll view.
    pub(crate) list: Option<(NodeId, u8)>,
    /// Where it was at the last tick of the runtime's clock
    /// ([`Tree::advance`]).
    pub(crate) last: (i64, i64),
    /// How fast it moved then, in 1/256 pixel a second: right and down.
    pub(crate) speed: (i64, i64),
}

impl<const N: usize, const T: usize> Default for Tree<N, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: BoundedVec::new(),
            root: None,
            viewport: Rect::EMPTY,
            scale: nowayos_display::DeviceScale::ONE,
            focus: None,
            pressed: None,
            pressed_key: None,
            pointer: Point::ORIGIN,
            focus_visible: false,
            pointer_shown: false,
            shift_keys: [false; 2],
            ctrl_keys: [false; 2],
            last_click: None,
            dirty: DirtyRegion::new(Rect::EMPTY),
            metrics: None,
            drag: None,
            texts: crate::long_text::TextStore::new(),
            moves: nowayos_bounded::BoundedVec::new(),
            fling: None,
            thumb: None,
            list_moved: None,
            layers: crate::layer::Layers::new(),
            motion: crate::motion::Motion::new(),
        }
    }

    /// Removes every node and all interaction state. Text-input values and
    /// slot texts are wiped in place first ([`Wipe`]), so what was typed or
    /// shown does not linger in the tree's storage. Existing [`NodeId`]s
    /// become meaningless. How the person last gave input - keys or the
    /// pointer, and so whether a focus ring shows - is kept, as a new screen
    /// is the same person's next.
    pub fn clear(&mut self) {
        self.nodes.wipe();
        self.texts.clear();
        self.moves.clear();
        self.fling = None;
        self.thumb = None;
        self.root = None;
        self.focus = None;
        self.pressed = None;
        self.pressed_key = None;
        self.drag = None;
        self.shift_keys = [false; 2];
        self.ctrl_keys = [false; 2];
        self.layers.open.clear();
        self.layers.tooltip = None;
        self.motion.clear();
        self.dirty.mark_all();
    }

    // ----- creating nodes ---------------------------------------------------

    /// Creates a detached node. Every text it shows is checked first
    /// ([`NodeKind::check_text`]): one line, nothing invisible, every
    /// character one the glyph atlases draw.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`] if the tree already holds `N` nodes (or
    /// 65 536); [`UiError::ControlCharacter`], [`UiError::InvisibleCharacter`]
    /// or [`UiError::NoGlyph`] if a text it shows cannot be shown
    /// ([`crate::text::check_printable`]), or another error of
    /// [`NodeKind::check`]. Nothing is created on error.
    pub fn add(&mut self, kind: NodeKind, style: Style) -> Result<NodeId, UiError> {
        kind.check()?;
        kind.check_text_in(&style.text)?;
        let full = UiError::TreeFull { capacity: N };
        let id = NodeId(u16::try_from(self.nodes.len()).map_err(|_| full)?);
        self.nodes.push(Node::new(kind, style)).map_err(|_| full)?;
        Ok(id)
    }

    /// Creates a `Column` container.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn column(&mut self, style: Style) -> Result<NodeId, UiError> {
        self.add(NodeKind::Column, style)
    }

    /// Creates a `Row` container.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn row(&mut self, style: Style) -> Result<NodeId, UiError> {
        self.add(NodeKind::Row, style)
    }

    /// Creates a `Stack` container.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn stack(&mut self, style: Style) -> Result<NodeId, UiError> {
        self.add(NodeKind::Stack, style)
    }

    /// Creates a line of text with the default style.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`], or the error of
    /// [`check_printable`](crate::text::check_printable) if `text` cannot be
    /// shown: a control character, an invisible one, or one outside the
    /// glyph atlases' set.
    pub fn text(&mut self, text: &'static str) -> Result<NodeId, UiError> {
        self.add(NodeKind::Text(text), Style::DEFAULT)
    }

    /// Creates a primary button with [`BUTTON_PADDING`](crate::BUTTON_PADDING),
    /// its label in Bold body text. It is at least the minimum
    /// target size each way
    /// ([`TextMetrics::min_target`](crate::TextMetrics::min_target)), and
    /// at least the theme's button height
    /// ([`TextMetrics::button_height`](crate::TextMetrics::button_height)).
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`], or the error of
    /// [`check_printable`](crate::text::check_printable) if `label` cannot
    /// be shown.
    pub fn button(
        &mut self,
        label: &'static str,
        action: crate::ActionId,
    ) -> Result<NodeId, UiError> {
        self.button_with(label, action, crate::ButtonVariant::Primary)
    }

    /// Creates a button in `variant`, as [`button`](Self::button)
    /// does a primary one.
    ///
    /// # Errors
    ///
    /// As [`button`](Self::button).
    pub fn button_with(
        &mut self,
        label: &'static str,
        action: crate::ActionId,
        variant: crate::ButtonVariant,
    ) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::Button { label, action },
            Style::new()
                .insets(BUTTON_PADDING)
                .text(crate::TextStyle::BODY.bold())
                .variant(variant),
        )
    }

    /// Creates an empty text input with
    /// [`INPUT_PADDING`](crate::INPUT_PADDING). It is at least the minimum
    /// target size each way
    /// ([`TextMetrics::min_target`](crate::TextMetrics::min_target)).
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`], or the error of
    /// [`check_printable`](crate::text::check_printable) if `placeholder`
    /// cannot be shown.
    pub fn text_input(
        &mut self,
        field: FieldId,
        placeholder: &'static str,
    ) -> Result<NodeId, UiError> {
        self.input(field, Input::new().placeholder(placeholder))
    }

    /// Creates an empty text input bound to `field` as `input` says: its
    /// placeholder, the most bytes it takes and which characters
    /// ([`InputMode`](crate::InputMode)), with
    /// [`INPUT_PADDING`](crate::INPUT_PADDING). It is at least the minimum
    /// target size each way.
    ///
    /// # Errors
    ///
    /// [`UiError::MaxLenOutOfRange`] unless `input`'s `max_len` is 1 to
    /// [`INPUT_CAPACITY`]; [`UiError::TreeFull`]; or the error of
    /// [`check_printable`](crate::text::check_printable) if the placeholder
    /// cannot be shown.
    pub fn input(&mut self, field: FieldId, input: Input) -> Result<NodeId, UiError> {
        let max_len = u8::try_from(input.max_len_bytes())
            .ok()
            .filter(|&max| max > 0 && usize::from(max) <= INPUT_CAPACITY)
            .ok_or(UiError::MaxLenOutOfRange {
                max: INPUT_CAPACITY,
            })?;
        self.add(
            NodeKind::TextInput {
                field,
                placeholder: input.placeholder_text(),
                value: BoundedStr::new(),
                max_len,
                mode: input.input_mode(),
                caret: 0,
                anchor: 0,
            },
            Style::new().insets(INPUT_PADDING),
        )
    }

    /// Creates an empty slot for application text bound to `slot`: one line
    /// of text the application sets while the screen shows
    /// ([`set_slot`](Self::set_slot)), such as an answer to review or a
    /// reference number. Its width is [`Length::Fill`], so it takes the room
    /// left in a `Row` and the whole width of a `Column`: new text then needs
    /// no new layout, and text wider than the slot is cut at its edge.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn slot(&mut self, slot: crate::SlotId) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::Slot {
                slot,
                text: BoundedStr::new(),
                template: None,
            },
            Style::new().width(Length::Fill),
        )
    }

    /// Creates a slot ([`slot`](Self::slot)) shown between `template`'s
    /// literals (contract v14), which show from the first frame: the
    /// application sets only its part, and the whole is measured, wrapped
    /// and aligned as one text.
    ///
    /// # Errors
    ///
    /// [`UiError::TextTooLong`] for literals of more than
    /// [`TEMPLATE_CAPACITY`](crate::node::TEMPLATE_CAPACITY) bytes
    /// together; as [`check_text`](NodeKind::check_text) for a literal it
    /// cannot show; [`UiError::TreeFull`].
    pub fn slot_template(
        &mut self,
        slot: crate::SlotId,
        template: &'static crate::Template,
    ) -> Result<NodeId, UiError> {
        let literals = template.before.len().saturating_add(template.after.len());
        if literals > crate::node::TEMPLATE_CAPACITY {
            return Err(UiError::TextTooLong(nowayos_bounded::CapacityError {
                capacity: crate::node::TEMPLATE_CAPACITY,
                required: literals,
            }));
        }
        self.add(
            NodeKind::Slot {
                slot,
                text: BoundedStr::new(),
                template: Some(template),
            },
            Style::new().width(Length::Fill),
        )
    }

    /// Creates `icon`, drawn in the colour of text where it stands.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn icon(&mut self, icon: &'static nowayos_icons::Icon) -> Result<NodeId, UiError> {
        self.add(NodeKind::Icon(icon), Style::new())
    }

    /// Creates `picture`, fitted to its box by `fit`, with `alt` saying
    /// what it shows (empty for decoration): at its own size until a style
    /// gives it another.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`]; for `alt`, as [`Tree::text`].
    pub fn image(
        &mut self,
        picture: &'static nowayos_pictures::Picture,
        fit: nowayos_pictures::Fit,
        alt: &'static str,
    ) -> Result<NodeId, UiError> {
        self.add(NodeKind::Image { picture, fit, alt }, Style::new())
    }

    /// Creates a long text bound to `slot` (builder contract v16), taking
    /// `capacity` bytes of the tree's text store: empty until the
    /// application sets it ([`set_slot`](Self::set_slot)). Its width is
    /// [`Length::Fill`]; its height is the lines its text wraps onto there.
    /// When a new text takes another number of lines the tree is laid out
    /// again.
    ///
    /// # Errors
    ///
    /// [`UiError::LongTextOutOfRange`] for 0 bytes or more than
    /// [`MAX_LONG_TEXT`](crate::long_text::MAX_LONG_TEXT);
    /// [`UiError::TextStoreFull`] when the store has fewer left;
    /// [`UiError::TreeFull`].
    pub fn long_text(&mut self, slot: crate::SlotId, capacity: u32) -> Result<NodeId, UiError> {
        if capacity == 0 || capacity > crate::long_text::MAX_LONG_TEXT {
            return Err(UiError::LongTextOutOfRange);
        }
        let start = self
            .texts
            .take(capacity)
            .ok_or(UiError::TextStoreFull { capacity: T })?;
        let kind = NodeKind::LongText {
            slot,
            start,
            capacity,
            len: 0,
        };
        self.add(kind, Style::new().width(Length::Fill))
            .inspect_err(|_| self.texts.give_back(start))
    }

    /// Creates an empty progress bar bound to `slot`, as wide as
    /// its container allows ([`Length::Fill`]).
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn progress(&mut self, slot: crate::SlotId) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::Progress { slot, permille: 0 },
            Style::new().width(Length::Fill),
        )
    }

    /// Creates an unticked checkbox labelled `label`, bound to `field`, with
    /// [`CHOICE_PADDING`](crate::CHOICE_PADDING). Inside its padding it is
    /// at least the minimum target size each way
    /// ([`TextMetrics::min_target`](crate::TextMetrics::min_target)).
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`], or the error of
    /// [`check_printable`](crate::text::check_printable) if `label` cannot
    /// be shown.
    pub fn checkbox(&mut self, field: FieldId, label: &'static str) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::Checkbox {
                field,
                label,
                checked: false,
            },
            Style::new().insets(crate::CHOICE_PADDING),
        )
    }

    /// Creates a radio group offering `options`, bound to `field`, with
    /// nothing chosen and [`CHOICE_PADDING`](crate::CHOICE_PADDING). Each
    /// option's row is at least the minimum target size each way
    /// ([`TextMetrics::min_target`](crate::TextMetrics::min_target)).
    ///
    /// # Errors
    ///
    /// [`UiError::TooManyOptions`] beyond
    /// [`MAX_OPTIONS`](crate::MAX_OPTIONS), [`UiError::NoOptions`] for none,
    /// the error of [`check_printable`](crate::text::check_printable) if an
    /// option cannot be shown, or [`UiError::TreeFull`].
    pub fn radio_group(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
    ) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::RadioGroup {
                field,
                options,
                selected: None,
            },
            Style::new().insets(crate::CHOICE_PADDING),
        )
    }

    /// Creates a list picker bound to `field`: `options`, 1 to
    /// [`MAX_LIST_OPTIONS`](crate::MAX_LIST_OPTIONS), `rows` of them showing
    /// at once (within [`LIST_ROWS`](crate::LIST_ROWS)), under a header
    /// showing `placeholder` until one is chosen.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`], [`UiError::TooManyOptions`],
    /// [`UiError::NoOptions`], [`UiError::RowsOutOfRange`], or a text that
    /// cannot be shown.
    pub fn list_picker(
        &mut self,
        field: FieldId,
        options: &'static [&'static str],
        placeholder: &'static str,
        rows: u8,
    ) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::ListPicker {
                field,
                options,
                placeholder,
                selected: None,
                rows,
                top: 0,
                cursor: 0,
            },
            Style::new().insets(crate::CHOICE_PADDING),
        )
    }

    /// Creates the on-screen keyboard, as wide as its container
    /// allows ([`Length::Fill`]), with
    /// [`KEYBOARD_PADDING`](crate::KEYBOARD_PADDING), in the text layout
    /// until a text input has focus.
    ///
    /// # Errors
    ///
    /// [`UiError::SecondKeyboard`] if the tree already has one, or
    /// [`UiError::TreeFull`].
    pub fn keyboard(&mut self) -> Result<NodeId, UiError> {
        if self
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Keyboard { .. }))
        {
            return Err(UiError::SecondKeyboard);
        }
        self.add(
            NodeKind::Keyboard {
                mode: crate::InputMode::Text,
                shift: false,
            },
            Style::new()
                .width(Length::Fill)
                .insets(crate::KEYBOARD_PADDING),
        )
    }

    /// Creates the line where errors for `field` show, empty until one is
    /// set ([`set_field_error`](Self::set_field_error)): put it beside the
    /// field. It is one line high even while empty, and its width is
    /// [`Length::Fill`], so an error can appear and go without a new layout.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn field_error(&mut self, field: FieldId) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::FieldError {
                field,
                message: None,
            },
            Style::new().width(Length::Fill),
        )
    }

    /// Creates a spacer that fills the remaining space in both directions.
    ///
    /// # Errors
    ///
    /// [`UiError::TreeFull`].
    pub fn spacer(&mut self) -> Result<NodeId, UiError> {
        self.add(
            NodeKind::Spacer,
            Style::new().width(Length::Fill).height(Length::Fill),
        )
    }

    // ----- linking ----------------------------------------------------------

    /// Makes `child` the last child of `parent`.
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`], [`UiError::NotAContainer`] if `parent`
    /// cannot have children, [`UiError::AlreadyAttached`] if `child` has a
    /// parent or is the root, [`UiError::WouldCycle`] if `parent` is inside
    /// `child`'s subtree, or [`UiError::TooDeep`]. Nothing changes on error.
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> Result<(), UiError> {
        let parent_node = self.node(parent)?;
        if !parent_node.kind.is_container() {
            return Err(UiError::NotAContainer(parent));
        }
        let child_node = self.node(child)?;
        if child_node.parent.is_some() || self.root == Some(child) {
            return Err(UiError::AlreadyAttached(child));
        }
        if parent == child || self.is_ancestor(child, parent)? {
            return Err(UiError::WouldCycle);
        }
        let depth = self.depth(parent)?;
        let height = self.height(child, 0)?;
        if depth.saturating_add(1).saturating_add(height) > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        self.link(parent, child)
    }

    /// Creates a node and appends it to `parent` in one step.
    ///
    /// # Errors
    ///
    /// As for [`add`](Self::add) and [`append`](Self::append). The parent is
    /// checked first, so a failure does not use up a node.
    pub fn add_child(
        &mut self,
        parent: NodeId,
        kind: NodeKind,
        style: Style,
    ) -> Result<NodeId, UiError> {
        if !self.node(parent)?.kind.is_container() {
            return Err(UiError::NotAContainer(parent));
        }
        if self.depth(parent)?.saturating_add(1) > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let child = self.add(kind, style)?;
        self.link(parent, child)?;
        Ok(child)
    }

    /// Creates a container with `children`, which must all exist and be
    /// detached — children-first construction, as the [`builder`] API uses.
    /// Everything is checked before anything is created, so on error the
    /// tree is unchanged.
    ///
    /// [`builder`]: crate::builder
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`], [`UiError::AlreadyAttached`] (including a
    /// child listed twice), [`UiError::TooDeep`] or [`UiError::TreeFull`].
    pub fn add_with_children(
        &mut self,
        container: ContainerKind,
        style: Style,
        children: &[NodeId],
    ) -> Result<NodeId, UiError> {
        self.add_parent(container.into(), style, children)
    }

    /// Creates a group bound to `group` holding `child`, **hidden**: the
    /// child shows only while the application makes the group visible
    /// ([`set_visible`](Self::set_visible)) - React's `cond && <X/>`. The
    /// group takes no space of its own: shown, the child is laid out in its
    /// place with its own style; hidden, the group takes no space and no
    /// gap, and the child is neither drawn, focusable nor hit, keeping its
    /// values ([`NodeKind::Group`]). `child` must exist and be detached.
    ///
    /// # Errors
    ///
    /// As [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn group(&mut self, group: GroupId, child: NodeId) -> Result<NodeId, UiError> {
        self.add_parent(
            NodeKind::Group {
                group,
                visible: false,
                when: true,
            },
            Style::DEFAULT,
            &[child],
        )
    }

    /// Creates a group bound to `group` that shows `child` only while the
    /// application does not make it visible - React's `!cond && <X/>`
    /// (contract v13). It starts hidden, so `child` shows at first; made
    /// visible, it shows nothing and takes no space. `child` must exist and
    /// be detached.
    ///
    /// # Errors
    ///
    /// As [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn unless(&mut self, group: GroupId, child: NodeId) -> Result<NodeId, UiError> {
        self.add_parent(
            NodeKind::Group {
                group,
                visible: false,
                when: false,
            },
            Style::DEFAULT,
            &[child],
        )
    }

    /// Creates a switch bound to `switch` that shows `child` only while
    /// the application sets its case to `when` - React's
    /// `step === "review" && <X/>` (contract v13). It starts on no case,
    /// showing nothing and taking no space. `child` must exist and be
    /// detached.
    ///
    /// # Errors
    ///
    /// As [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn case(&mut self, switch: SwitchId, when: u8, child: NodeId) -> Result<NodeId, UiError> {
        self.add_parent(
            NodeKind::Switch {
                switch,
                case: None,
                when,
            },
            Style::DEFAULT,
            &[child],
        )
    }

    /// Creates a switch bound to `switch` that shows `when_matched` while
    /// the application sets its case to `when`, and `otherwise` on any
    /// other case or none, as at first - React's
    /// `step === "review" ? <X/> : <Y/>` (contract v13). Both must exist and
    /// be detached.
    ///
    /// # Errors
    ///
    /// As [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn case_or(
        &mut self,
        switch: SwitchId,
        when: u8,
        when_matched: NodeId,
        otherwise: NodeId,
    ) -> Result<NodeId, UiError> {
        self.add_parent(
            NodeKind::Switch {
                switch,
                case: None,
                when,
            },
            Style::DEFAULT,
            &[when_matched, otherwise],
        )
    }

    /// Creates a group bound to `group` that shows `when_visible` while the
    /// application makes it visible ([`set_visible`](Self::set_visible))
    /// and `otherwise` while it does not - React's `cond ? <X/> : <Y/>`. It
    /// starts hidden, showing `otherwise`. Each child is laid out in the
    /// group's place while it shows, and the other is neither drawn,
    /// focusable nor hit, keeping its values ([`NodeKind::Group`]). Both
    /// must exist and be detached.
    ///
    /// # Errors
    ///
    /// As [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn either(
        &mut self,
        group: GroupId,
        when_visible: NodeId,
        otherwise: NodeId,
    ) -> Result<NodeId, UiError> {
        self.add_parent(
            NodeKind::Group {
                group,
                visible: false,
                when: true,
            },
            Style::DEFAULT,
            &[when_visible, otherwise],
        )
    }

    /// Creates a node of `kind` with `children`, children first: every
    /// check comes before anything is created.
    pub(crate) fn add_parent(
        &mut self,
        kind: NodeKind,
        style: Style,
        children: &[NodeId],
    ) -> Result<NodeId, UiError> {
        for (position, &child) in children.iter().enumerate() {
            let listed_before = children
                .get(..position)
                .is_some_and(|earlier| earlier.contains(&child));
            if self.node(child)?.parent.is_some() || self.root == Some(child) || listed_before {
                return Err(UiError::AlreadyAttached(child));
            }
            if self.height(child, 0)?.saturating_add(1) > MAX_DEPTH {
                return Err(UiError::TooDeep);
            }
        }
        let id = self.add(kind, style)?;
        for &child in children {
            self.link(id, child)?;
        }
        Ok(id)
    }

    /// Makes `root` the root: the node laid out to fill the viewport.
    /// Clears focus and any pressed control.
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`], or [`UiError::AlreadyAttached`] if `root`
    /// has a parent.
    pub fn set_root(&mut self, root: NodeId) -> Result<(), UiError> {
        if self.node(root)?.parent.is_some() {
            return Err(UiError::AlreadyAttached(root));
        }
        self.root = Some(root);
        self.focus = None;
        self.pressed = None;
        self.dirty.mark_all();
        // Each node's own animation, if motion is on (#10087).
        self.start_declared();
        Ok(())
    }

    // ----- changing nodes ---------------------------------------------------

    /// Replaces a node's style. Takes effect at the next
    /// [`layout`](Self::layout).
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`].
    pub fn set_style(&mut self, id: NodeId, style: Style) -> Result<(), UiError> {
        let node = self.node_mut(id)?;
        node.kind.check_text_in(&style.text)?;
        node.style = style;
        Ok(())
    }

    /// Replaces the text of a `Text` node or the label of a `Button`, and
    /// marks it for repainting. Re-run [`layout`](Self::layout) if the new
    /// text changes an `Auto` size.
    ///
    /// # Errors
    ///
    /// The error of [`check_printable`](crate::text::check_printable) if
    /// `text` cannot be shown, [`UiError::InvalidNode`], or
    /// [`UiError::WrongKind`] for other kinds. Nothing changes on error: the
    /// previous text stays.
    pub fn set_text(&mut self, id: NodeId, text: &'static str) -> Result<(), UiError> {
        check_printable(text)?;
        let node = self.node_mut(id)?;
        if node.style.text.role == crate::TextRole::Display {
            crate::text::check_display(text)?;
        }
        match &mut node.kind {
            NodeKind::Text(current) => *current = text,
            NodeKind::Button { label, .. } => *label = text,
            _ => return Err(UiError::WrongKind(id)),
        }
        let rect = node.rect;
        self.dirty.add(rect);
        Ok(())
    }

    /// The value of the text input bound to `field`. Field ids are expected
    /// to be unique in a tree; if two inputs share one, the first created
    /// wins here and in [`set_value`](Self::set_value).
    #[must_use]
    pub fn value(&self, field: FieldId) -> Option<&str> {
        self.nodes.iter().find_map(|node| match &node.kind {
            NodeKind::TextInput {
                field: f, value, ..
            } if *f == field => Some(value.as_str()),
            NodeKind::TextArea {
                field: f,
                start,
                len,
                ..
            } if *f == field => Some(self.texts.get(*start, *len)),
            _ => None,
        })
    }

    /// The value of text input `id` — for example a handle returned by a
    /// generated screen's `build` function.
    #[must_use]
    pub fn node_value(&self, id: NodeId) -> Option<&str> {
        match &self.get(id)?.kind {
            NodeKind::TextInput { value, .. } => Some(value.as_str()),
            _ => None,
        }
    }

    /// The field control `id` is bound to: a text input, checkbox or radio
    /// group.
    #[must_use]
    pub fn field_of(&self, id: NodeId) -> Option<FieldId> {
        self.get(id)?.kind.field()
    }

    /// Whether the checkbox bound to `field` is ticked; `None` if no
    /// checkbox is bound to it.
    #[must_use]
    pub fn checked(&self, field: FieldId) -> Option<bool> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::Checkbox {
                field: f, checked, ..
            } if f == field => Some(checked),
            _ => None,
        })
    }

    /// Ticks or unticks the checkbox bound to `field`, marking it for
    /// repainting if that changes it.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownField`] if no checkbox is bound to `field`.
    pub fn set_checked(&mut self, field: FieldId, to: bool) -> Result<(), UiError> {
        for node in self.nodes.iter_mut() {
            if let NodeKind::Checkbox {
                field: f, checked, ..
            } = &mut node.kind
                && *f == field
            {
                if *checked != to {
                    *checked = to;
                    self.dirty.add(node.rect);
                }
                return Ok(());
            }
        }
        Err(UiError::UnknownField(field))
    }

    /// The option chosen in the radio group bound to `field`, from 0;
    /// `None` if nothing is chosen, or no radio group is bound to it.
    #[must_use]
    pub fn selected(&self, field: FieldId) -> Option<u8> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::RadioGroup {
                field: f, selected, ..
            }
            | NodeKind::ListPicker {
                field: f, selected, ..
            } if f == field => selected,
            _ => None,
        })
    }

    /// Chooses `option` (from 0) in the radio group bound to `field`, or
    /// clears its choice with `None`, marking it for repainting if that
    /// changes it.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownField`] if no radio group is bound to `field`;
    /// [`UiError::NoSuchOption`] if it has no such option. Nothing changes
    /// on error.
    pub fn set_selected(&mut self, field: FieldId, option: Option<u8>) -> Result<(), UiError> {
        for node in self.nodes.iter_mut() {
            if let NodeKind::RadioGroup {
                field: f,
                options,
                selected,
            } = &mut node.kind
                && *f == field
            {
                if option.is_some_and(|option| usize::from(option) >= options.len()) {
                    return Err(UiError::NoSuchOption);
                }
                if *selected != option {
                    *selected = option;
                    self.dirty.add(node.rect);
                }
                return Ok(());
            }
            // A list picker: the choice highlighted and in view.
            if let NodeKind::ListPicker {
                field: f,
                options,
                selected,
                rows,
                top,
                cursor,
                ..
            } = &mut node.kind
                && *f == field
            {
                if option.is_some_and(|option| usize::from(option) >= options.len()) {
                    return Err(UiError::NoSuchOption);
                }
                *selected = option;
                *cursor = option.unwrap_or(0);
                *top = shown_top(*top, *cursor, *rows, options.len());
                self.dirty.add(node.rect);
                return Ok(());
            }
        }
        Err(UiError::UnknownField(field))
    }

    /// Replaces the value of the text input bound to `field`.
    ///
    /// # Errors
    ///
    /// For a character that is a control character, invisible or outside
    /// the glyph atlases' set, [`UiError::ControlCharacter`],
    /// [`UiError::InvisibleCharacter`] or [`UiError::NoGlyph`]
    /// ([`check_printable`](crate::text::check_printable));
    /// [`UiError::UnknownField`]; [`UiError::ValueTooLong`] beyond the
    /// input's `max_len`; [`UiError::NotInInputMode`] for a character its
    /// mode refuses. The value is unchanged on error.
    pub fn set_value(&mut self, field: FieldId, text: &str) -> Result<(), UiError> {
        let id = self.find_field(field).ok_or(UiError::UnknownField(field))?;
        if matches!(self.node(id)?.kind, NodeKind::TextArea { .. }) {
            return self.set_area_value(id, text);
        }
        check_printable(text)?;
        let node = self.node_mut(id)?;
        let NodeKind::TextInput {
            value,
            max_len,
            mode,
            caret,
            anchor,
            ..
        } = &mut node.kind
        else {
            return Err(UiError::WrongKind(id));
        };
        // The input's own length first: the error names its capacity.
        let max = usize::from(*max_len);
        if text.len() > max {
            return Err(UiError::ValueTooLong(nowayos_bounded::CapacityError {
                capacity: max,
                required: text.len(),
            }));
        }
        if !text.chars().all(|ch| mode.accepts(ch)) {
            return Err(UiError::NotInInputMode);
        }
        *value = BoundedStr::try_from_str(text).map_err(UiError::ValueTooLong)?;
        // The caret after the new text, nothing selected.
        let end = u8::try_from(value.len()).unwrap_or(u8::MAX);
        (*caret, *anchor) = (end, end);
        let rect = node.rect;
        self.dirty.add(rect);
        Ok(())
    }

    /// The text of the slot bound to `slot` (the first created, if several
    /// share it).
    #[must_use]
    pub fn slot_text(&self, slot: crate::SlotId) -> Option<&str> {
        self.nodes.iter().find_map(|node| match &node.kind {
            NodeKind::Slot { slot: s, text, .. } if *s == slot => Some(text.as_str()),
            NodeKind::LongText {
                slot: s,
                start,
                len,
                ..
            } if *s == slot => Some(self.texts.get(*start, *len)),
            _ => None,
        })
    }

    /// The text a long text node shows (builder contract v16): `None` for
    /// any other node.
    #[must_use]
    pub fn long_text_of(&self, node: &Node) -> Option<&str> {
        match node.kind {
            NodeKind::LongText { start, len, .. } => Some(self.texts.get(start, len)),
            _ => None,
        }
    }

    /// The slot node bound to `slot`, if any (the first created, if several
    /// share it).
    #[must_use]
    pub fn find_slot(&self, slot: crate::SlotId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(&node.kind, NodeKind::Slot { slot: s, .. } | NodeKind::LongText { slot: s, .. } if *s == slot)
                .then(|| u16::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }

    /// Shows `text` in the slot bound to `slot`, replacing its text in place
    /// (the old bytes are zeroed first), and marks only the part of the
    /// slot that changes for repainting: a live figure whose last
    /// digits change repaints those digits, not its whole line
    /// ([`crate::text::changed_area`]). The slot keeps the rectangle the last
    /// layout gave it.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownSlot`]; [`UiError::TextTooLong`] beyond
    /// [`SLOT_CAPACITY`](crate::SLOT_CAPACITY) bytes; or, for a character
    /// that is a control character, invisible or not in the fonts,
    /// [`UiError::ControlCharacter`], [`UiError::InvisibleCharacter`] or
    /// [`UiError::NoGlyph`] ([`crate::text::check_printable`]). On error
    /// the previous text stays.
    pub fn set_slot(&mut self, slot: crate::SlotId, text: &str) -> Result<(), UiError> {
        let id = self.find_slot(slot).ok_or(UiError::UnknownSlot(slot))?;
        if matches!(self.node(id)?.kind, NodeKind::LongText { .. }) {
            return self.set_long_text(id, text);
        }
        check_printable(text)?;
        let metrics = self.metrics;
        let node = self.node_mut(id)?;
        // A slot in the display role takes only what its atlases draw.
        if node.style.text.role == crate::TextRole::Display {
            crate::text::check_display(text)?;
        }
        let (rect, content, style) = (node.rect, node.content(), node.style.text);
        let NodeKind::Slot {
            text: shown,
            template,
            ..
        } = &mut node.kind
        else {
            return Err(UiError::WrongKind(id));
        };
        if text.len() > shown.capacity() {
            return Err(UiError::TextTooLong(nowayos_bounded::CapacityError {
                capacity: shown.capacity(),
                required: text.len(),
            }));
        }
        let area = match (metrics, template) {
            (Some(metrics), None) if !style.wraps() => crate::text::changed_area(
                rect,
                content,
                shown.as_str(),
                text,
                &metrics.for_style(style),
            ),
            // The whole text, literals and all, as it shows.
            (Some(metrics), Some(template)) if !style.wraps() => crate::text::changed_area(
                rect,
                content,
                &template.around(shown),
                &template.around(text),
                &metrics.for_style(style),
            ),
            // Not laid out yet, or wrapped: every line may move.
            _ => rect,
        };
        shown.clear();
        shown.push_str(text).map_err(UiError::TextTooLong)?;
        self.dirty.add(area);
        Ok(())
    }

    /// Shows `text` in the long text `id`: its paragraphs checked as any
    /// text is, its bytes replacing the old in place, and only the lines
    /// that differ marked for repainting - or, if it now takes another
    /// number of lines, the tree laid out again as last time and repainted
    /// whole.
    fn set_long_text(&mut self, id: NodeId, text: &str) -> Result<(), UiError> {
        crate::long_text::check_paragraphs(text)?;
        let node = self.node(id)?;
        let (rect, content, style) = (node.rect, node.content(), node.style.text);
        let NodeKind::LongText {
            start,
            capacity,
            len,
            ..
        } = node.kind
        else {
            return Err(UiError::WrongKind(id));
        };
        let required = u32::try_from(text.len()).unwrap_or(u32::MAX);
        if required > capacity {
            return Err(UiError::TextTooLong(nowayos_bounded::CapacityError {
                capacity: usize::try_from(capacity).unwrap_or(usize::MAX),
                required: text.len(),
            }));
        }
        let mut relayout = false;
        match self.metrics {
            Some(metrics) => {
                let metrics = metrics.for_style(style);
                let old = self.texts.get(start, len);
                let changed = crate::long_text::changed_lines(old, text, content, &metrics);
                match changed {
                    Some(lines) => lines.for_each(|rect| self.dirty.add(rect)),
                    None => relayout = true,
                }
            }
            None => self.dirty.add(rect),
        }
        self.texts
            .put(start, capacity, text)
            .ok_or(UiError::WrongKind(id))?;
        if let NodeKind::LongText { len, .. } = &mut self.node_mut(id)?.kind {
            *len = required;
        }
        // Laid out again only once the screen is whole: a long text set as
        // its screen is built is laid out with the rest.
        if relayout
            && self.root.is_some()
            && let Some(metrics) = self.metrics
        {
            // Inside a scroll view, only that view is laid out again: its
            // child grows or shrinks, the view does not.
            let around = self.views_around(id).next();
            match around {
                Some(view) => self.relayout_view(view)?,
                None => {
                    let viewport = self.viewport;
                    self.layout(viewport, &metrics)?;
                }
            }
        }
        Ok(())
    }

    /// Empties every slot, zeroing its bytes in place, and every progress
    /// bar, and marks the ones that showed anything for
    /// repainting.
    pub fn clear_slots(&mut self) {
        for node in self.nodes.iter_mut() {
            match &mut node.kind {
                NodeKind::Slot { text, .. } => {
                    if !text.is_empty() {
                        self.dirty.add(node.rect);
                    }
                    text.wipe();
                }
                NodeKind::LongText {
                    start,
                    capacity,
                    len,
                    ..
                } => {
                    if *len != 0 {
                        self.dirty.add(node.rect);
                    }
                    let _ = self.texts.put(*start, *capacity, "");
                    *len = 0;
                }
                NodeKind::Progress { permille, .. } => {
                    if *permille != 0 {
                        self.dirty.add(node.rect);
                    }
                    *permille = 0;
                }
                _ => {}
            }
        }
    }

    /// The progress bar bound to `slot`, if the tree has one.
    #[must_use]
    pub fn find_progress(&self, slot: crate::SlotId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(&node.kind, NodeKind::Progress { slot: s, .. } if *s == slot)
                .then(|| u16::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }

    /// How far the progress bar bound to `slot` shows, in thousandths.
    #[must_use]
    pub fn progress_of(&self, slot: crate::SlotId) -> Option<u16> {
        let node = self.get(self.find_progress(slot)?)?;
        match node.kind {
            NodeKind::Progress { permille, .. } => Some(permille),
            _ => None,
        }
    }

    /// Shows `done` of `whole` on the progress bar bound to `slot`,
    /// to the nearest thousandth - all of it for `done` at or past `whole`,
    /// none for a `whole` of 0 - and marks only the part of the bar that
    /// changes for repainting: between the old and the new end of its
    /// fill, and a rounded end's width - the bar's height - each side.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownSlot`] if no progress bar is bound to `slot`.
    pub fn set_progress(
        &mut self,
        slot: crate::SlotId,
        done: u32,
        whole: u32,
    ) -> Result<(), UiError> {
        let id = self.find_progress(slot).ok_or(UiError::UnknownSlot(slot))?;
        let permille = progress_permille(done, whole);
        let node = self.node_mut(id)?;
        let rect = node.rect;
        let content = node.content();
        let NodeKind::Progress {
            permille: shown, ..
        } = &mut node.kind
        else {
            return Err(UiError::WrongKind(id));
        };
        let before = core::mem::replace(shown, permille);
        if before == permille {
            return Ok(());
        }
        let width = i64::from(content.size.width);
        let end = |permille: u16| {
            width
                .saturating_mul(i64::from(permille))
                .checked_div(i64::from(crate::PROGRESS_WHOLE))
                .unwrap_or(0)
        };
        let (from, to) = (end(before.min(permille)), end(before.max(permille)));
        let round = i64::from(content.size.height);
        let area = Rect::from_edges(
            content.left().saturating_add(from).saturating_sub(round),
            rect.top(),
            content.left().saturating_add(to).saturating_add(round),
            rect.bottom(),
        );
        if let Some(area) = area.intersection(&rect) {
            self.dirty.add(area);
        }
        Ok(())
    }

    /// Shows or hides every group bound to `group` ([`group`](Self::group),
    /// [`either`](Self::either)): visible, each shows its first child;
    /// hidden, its second if it has one, else nothing. A control that stops showing
    /// loses focus, and a press on it is cancelled; it keeps its value. Lay
    /// the tree out again before the next frame ([`layout`](Self::layout)):
    /// what a group shows takes its space only then. The whole viewport is
    /// marked for repainting if anything changed.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownGroup`] if no group is bound to `group`. Nothing
    /// changes on error.
    pub fn set_visible(&mut self, group: GroupId, visible: bool) -> Result<(), UiError> {
        let mut found = false;
        let mut changed = false;
        for node in self.nodes.iter_mut() {
            if let NodeKind::Group {
                group: g,
                visible: shown,
                ..
            } = &mut node.kind
                && *g == group
            {
                found = true;
                if *shown != visible {
                    *shown = visible;
                    changed = true;
                }
            }
        }
        if !found {
            return Err(UiError::UnknownGroup(group));
        }
        if changed {
            self.drop_hidden_focus();
            self.dirty.mark_all();
            // What came into view, transitions in (#10087).
            self.enter_shown(
                |kind| matches!(kind, NodeKind::Group { group: g, .. } if *g == group),
            );
        }
        Ok(())
    }

    /// Sets the case of every switch bound to `switch` ([`case`](Self::case),
    /// [`case_or`](Self::case_or)): each shows its first child while the case
    /// is its own, else its second if it has one, else nothing; `None` is no
    /// case, as at first. A control that stops showing loses focus, and a
    /// press on it is cancelled; it keeps its value. Lay the tree out again
    /// before the next frame ([`layout`](Self::layout)). The whole viewport
    /// is marked for repainting if anything changed.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownSwitch`] if no switch is bound to `switch`. Nothing
    /// changes on error.
    pub fn set_case(&mut self, switch: SwitchId, case: Option<u8>) -> Result<(), UiError> {
        let mut found = false;
        let mut changed = false;
        for node in self.nodes.iter_mut() {
            if let NodeKind::Switch {
                switch: s,
                case: current,
                ..
            } = &mut node.kind
                && *s == switch
            {
                found = true;
                if *current != case {
                    *current = case;
                    changed = true;
                }
            }
        }
        if !found {
            return Err(UiError::UnknownSwitch(switch));
        }
        if changed {
            self.drop_hidden_focus();
            self.dirty.mark_all();
            // What came into view, transitions in (#10087).
            self.enter_shown(
                |kind| matches!(kind, NodeKind::Switch { switch: s, .. } if *s == switch),
            );
        }
        Ok(())
    }

    /// The case of the switch bound to `switch` - `Some(None)` for none -
    /// or `None` if no switch is bound to it.
    #[must_use]
    pub fn case_of(&self, switch: SwitchId) -> Option<Option<u8>> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::Switch {
                switch: s, case, ..
            } if s == switch => Some(case),
            _ => None,
        })
    }

    /// Whether the group bound to `group` is visible; `None` if no group is
    /// bound to it.
    #[must_use]
    pub fn visible(&self, group: GroupId) -> Option<bool> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::Group {
                group: g, visible, ..
            } if g == group => Some(visible),
            _ => None,
        })
    }

    /// The group node bound to `group`, if any (the first created, if
    /// several share it).
    #[must_use]
    pub fn find_group(&self, group: GroupId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(&node.kind, NodeKind::Group { group: g, .. } if *g == group)
                .then(|| u16::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }

    /// Whether node `id` shows: no group above it hides the child it is
    /// in. Says nothing of whether `id` is attached to the root.
    #[must_use]
    pub fn is_shown(&self, id: NodeId) -> bool {
        let mut current = id;
        for _ in 0..=MAX_DEPTH.saturating_add(1) {
            let Some(parent) = self.get(current).and_then(|node| node.parent) else {
                return self.get(current).is_some();
            };
            let parent_node = self.get(parent);
            // Inside a layer: shown while it is.
            if parent_node.is_some_and(|node| matches!(node.kind, NodeKind::Layer { .. })) {
                if !self.shown_layers().any(|shown| shown == parent) {
                    return false;
                }
                current = parent;
                continue;
            }
            let in_group = parent_node.is_some_and(|node| node.kind.is_conditional());
            if in_group && self.group_branch(parent) != Some(current) {
                return false;
            }
            current = parent;
        }
        false
    }

    /// For a group or a switch, the child it shows: its first while its
    /// group is as it shows it for (visible, or for [`unless`](Self::unless)
    /// hidden) or its switch is on its case, else its second (if any).
    /// `None` for one showing nothing, or any other kind.
    pub(crate) fn group_branch(&self, id: NodeId) -> Option<NodeId> {
        let node = self.get(id)?;
        let first = match node.kind {
            NodeKind::Group { visible, when, .. } => visible == when,
            NodeKind::Switch { case, when, .. } => case == Some(when),
            _ => return None,
        };
        if first {
            node.first_child
        } else {
            self.get(node.first_child?)?.next_sibling
        }
    }

    /// The children a pass over what shows walks: a group's shown child
    /// alone, or every child of any other node.
    pub(crate) fn first_shown_child(&self, id: NodeId) -> Option<NodeId> {
        let node = self.get(id)?;
        // A layer is walked only as a plane's root: its child shows there.
        if let NodeKind::Layer { .. } = node.kind {
            return node.first_child;
        }
        if node.kind.is_conditional() {
            self.group_branch(id)
        } else {
            node.first_child
        }
    }

    /// The error showing for `field`, if any.
    #[must_use]
    pub fn field_error_message(&self, field: FieldId) -> Option<&'static str> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::FieldError { field: f, message } if f == field => message,
            _ => None,
        })
    }

    /// Shows `message` as the error for `field`, in words, on its error
    /// line ([`field_error`](Self::field_error)), replacing any error it
    /// showed; marks only that line for repainting. Validation stays in the
    /// application: the UI shows what it is told. After setting the errors
    /// of a step, [`focus_first_error`](Self::focus_first_error) takes the
    /// person to the first.
    ///
    /// # Errors
    ///
    /// The error of [`check_printable`](crate::text::check_printable) if
    /// `message` cannot be shown, or [`UiError::UnknownField`] if no error
    /// line is bound to `field`. Nothing changes on error: the previous
    /// error, if any, stays.
    pub fn set_field_error(
        &mut self,
        field: FieldId,
        message: &'static str,
    ) -> Result<(), UiError> {
        check_printable(message)?;
        self.update_field_error(field, Some(message))
    }

    /// Removes the error for `field`, marking its line for repainting if it
    /// showed one.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownField`] if no error line is bound to `field`.
    pub fn clear_field_error(&mut self, field: FieldId) -> Result<(), UiError> {
        self.update_field_error(field, None)
    }

    /// Removes every field's error.
    pub fn clear_field_errors(&mut self) {
        for node in self.nodes.iter_mut() {
            if let NodeKind::FieldError { message, .. } = &mut node.kind
                && message.take().is_some()
            {
                self.dirty.add(node.rect);
            }
        }
    }

    /// Sets the message of every error line bound to `field`, marking the
    /// ones that change.
    fn update_field_error(
        &mut self,
        field: FieldId,
        to: Option<&'static str>,
    ) -> Result<(), UiError> {
        let mut found = false;
        for node in self.nodes.iter_mut() {
            if let NodeKind::FieldError { field: f, message } = &mut node.kind
                && *f == field
            {
                found = true;
                if *message != to {
                    *message = to;
                    self.dirty.add(node.rect);
                }
            }
        }
        if found {
            Ok(())
        } else {
            Err(UiError::UnknownField(field))
        }
    }

    /// Empties every text input, zeroing the stored bytes, unticks every
    /// checkbox and clears every radio group's choice — for example when a
    /// kiosk session times out. Marks the controls that held a value for
    /// repainting.
    pub fn clear_values(&mut self) {
        for node in self.nodes.iter_mut() {
            let held = match &mut node.kind {
                NodeKind::TextInput {
                    value,
                    caret,
                    anchor,
                    ..
                } => {
                    let held = !value.is_empty();
                    value.clear();
                    (*caret, *anchor) = (0, 0);
                    held
                }
                NodeKind::TextArea {
                    start,
                    capacity,
                    len,
                    caret,
                    anchor,
                    top,
                    ..
                } => {
                    let held = *len != 0;
                    let _ = self.texts.put(*start, *capacity, "");
                    (*len, *caret, *anchor, *top) = (0, 0, 0, 0);
                    held
                }
                NodeKind::Checkbox { checked, .. } => core::mem::take(checked),
                NodeKind::RadioGroup { selected, .. } | NodeKind::ListPicker { selected, .. } => {
                    selected.take().is_some()
                }
                _ => false,
            };
            if held {
                self.dirty.add(node.rect);
            }
        }
    }

    /// Wipes everything a session put in the tree, in place: every text
    /// input's value and every slot's text zeroed, every checkbox unticked,
    /// every radio group's choice and every field's error cleared, every
    /// group hidden again. The
    /// nodes stay, so [`is_wiped`](Self::is_wiped) can check every byte;
    /// the whole viewport is marked for repainting. A session reset
    /// ([`Navigator::reset`](crate::Navigator::reset)) starts with this.
    pub fn wipe_session(&mut self) {
        for node in self.nodes.iter_mut() {
            // A long text's or a text area's bytes, in the text store.
            if let NodeKind::LongText {
                start, capacity, ..
            }
            | NodeKind::TextArea {
                start, capacity, ..
            } = node.kind
            {
                let _ = self.texts.put(start, capacity, "");
            }
            node.wipe();
        }
        self.pressed = None;
        self.pressed_key = None;
        // The next person has given no input yet: no focus ring until a key.
        self.focus_visible = false;
        // Every layer closed: the next person starts on the screen.
        self.close_all();
        self.motion.clear();
        self.drop_hidden_focus();
        self.dirty.mark_all();
    }

    /// Whether the tree holds nothing a person entered or the application
    /// showed for them: every text input's value and every slot's text all
    /// zeros - every byte of storage, not merely empty
    /// (`BoundedStr::is_zeroed`) - every checkbox unticked, no radio
    /// option chosen and every group hidden, as built. Field errors are the
    /// application's fixed messages, not data, and are not counted.
    ///
    /// For tests, and safe to ship: it only reads, and tells nothing beyond
    /// one bit.
    #[must_use]
    pub fn is_wiped(&self) -> bool {
        self.nodes.iter().all(|node| match &node.kind {
            NodeKind::TextInput { value, .. } => value.is_zeroed(),
            NodeKind::TextArea {
                start,
                capacity,
                len,
                ..
            } => *len == 0 && self.texts.is_zeroed(*start, *capacity),
            NodeKind::Slot { text, .. } => text.is_zeroed(),
            NodeKind::LongText {
                start,
                capacity,
                len,
                ..
            } => *len == 0 && self.texts.is_zeroed(*start, *capacity),
            NodeKind::Progress { permille, .. } => *permille == 0,
            NodeKind::Checkbox { checked, .. } => !checked,
            NodeKind::RadioGroup { selected, .. } => selected.is_none(),
            NodeKind::ListPicker {
                selected,
                top,
                cursor,
                ..
            } => selected.is_none() && *top == 0 && *cursor == 0,
            NodeKind::Keyboard { shift, .. } => !shift,
            NodeKind::Group { visible, .. } => !visible,
            NodeKind::Switch { case, .. } => case.is_none(),
            _ => true,
        })
    }

    // ----- queries ----------------------------------------------------------

    /// The root, if set.
    #[must_use]
    pub const fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// Number of nodes created.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no nodes have been created.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node `id` names, if any.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    /// The children of `id`, first to last (empty for an invalid id).
    #[must_use]
    pub fn children(&self, id: NodeId) -> Children<'_, N, T> {
        Children {
            tree: self,
            next: self.get(id).and_then(|n| n.first_child),
            budget: N,
        }
    }

    /// Every node that shows, from the root: parents before children and
    /// siblings in order — the order to paint in, and the focus order. A
    /// group's hidden child and everything in it are left out.
    #[must_use]
    pub fn paint_order(&self) -> PaintOrder<'_, N, T> {
        PaintOrder {
            tree: self,
            root: self.root,
            next: self.root,
            budget: N,
            plane: 0,
            onwards: true,
        }
    }

    /// Every node that shows in plane `index` alone - 0 the root's, then
    /// each layer shown ([`plane`](Self::plane)) - in paint order.
    #[must_use]
    pub fn plane_order(&self, index: usize) -> PaintOrder<'_, N, T> {
        let root = self.plane(index);
        PaintOrder {
            tree: self,
            root,
            next: root,
            budget: N,
            plane: index,
            onwards: false,
        }
    }

    /// Whether a value-holding control - a text input, checkbox or radio
    /// group - is bound to `field`.
    #[must_use]
    pub fn has_field(&self, field: FieldId) -> bool {
        self.nodes
            .iter()
            .any(|node| node.kind.field() == Some(field))
    }

    /// The text input bound to `field`, if any (the first created, if
    /// several share it).
    #[must_use]
    pub fn find_field(&self, field: FieldId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(&node.kind, NodeKind::TextInput { field: f, .. } | NodeKind::TextArea { field: f, .. } if *f == field)
                .then(|| u16::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }

    /// The area laid out by the last [`layout`](Self::layout).
    #[must_use]
    pub const fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The device scale the tree was last laid out at
    /// ([`TextMetrics::scale`](crate::TextMetrics::scale)): pixels each way
    /// for each layout unit. A renderer draws at it - lines, rings and text
    /// from the theme's atlas for it - so drawing matches layout.
    /// 1 before the first layout.
    #[must_use]
    pub const fn scale(&self) -> nowayos_display::DeviceScale {
        self.scale
    }

    /// The areas that need repainting since the last
    /// [`clear_dirty`](Self::clear_dirty). Everything after a layout.
    #[must_use]
    pub fn dirty_rects(&self) -> &[Rect] {
        self.dirty.rects()
    }

    /// Records that the renderer has repainted the dirty areas.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
        self.moves.clear();
    }

    /// Marks a node's area for repainting.
    pub fn mark_dirty(&mut self, id: NodeId) {
        if let Some(rect) = self.get(id).map(Node::rect) {
            self.dirty.add(rect);
        }
    }

    /// Marks `area` for repainting, clipped to the viewport: for what is
    /// drawn over the tree, such as the pointer.
    pub fn mark_area_dirty(&mut self, area: Rect) {
        if let Some(area) = area.intersection(&self.viewport) {
            self.dirty.add(area);
        }
    }

    /// Whether the focused control shows its focus ring: only after
    /// keyboard use. A key sets it; a press of the pointer or a touch clears
    /// it, as does the end of a session. Focus itself, and a text input's
    /// caret, are unchanged by it.
    #[must_use]
    pub const fn focus_visible(&self) -> bool {
        self.focus_visible
    }

    /// Shows or hides the focus ring, repainting the focused control if it
    /// changes - for an application that moves focus for the person, or a
    /// test.
    pub fn set_focus_visible(&mut self, visible: bool) {
        if visible != self.focus_visible {
            self.focus_visible = visible;
            if let Some(id) = self.focus {
                self.mark_dirty(id);
            }
        }
    }

    /// Where the pointer is, if a mouse or a tablet last moved it:
    /// a pointer is drawn there. `None` until the first move, and after a
    /// touch.
    #[must_use]
    pub const fn pointer_shown(&self) -> Option<Point> {
        if self.pointer_shown {
            Some(self.pointer)
        } else {
            None
        }
    }

    // ----- internals --------------------------------------------------------

    pub(crate) fn node(&self, id: NodeId) -> Result<&Node, UiError> {
        self.get(id).ok_or(UiError::InvalidNode(id))
    }

    pub(crate) fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, UiError> {
        self.nodes
            .get_mut(id.index())
            .ok_or(UiError::InvalidNode(id))
    }

    /// Takes focus, and a press, off a control a group now hides: neither
    /// may rest on what is neither drawn nor hit.
    fn drop_hidden_focus(&mut self) {
        if let Some(id) = self.focus
            && !self.is_shown(id)
        {
            self.focus = None;
            self.keyboard_follows(Some(id));
        }
        if self.pressed.is_some_and(|id| !self.is_shown(id)) {
            self.pressed = None;
        }
    }

    /// Links a detached `child` as `parent`'s last child, then takes focus
    /// and a press off anything the link puts in a group's hidden child.
    /// Callers have already checked every rule.
    fn link(&mut self, parent: NodeId, child: NodeId) -> Result<(), UiError> {
        let previous_last = self.node(parent)?.last_child;
        {
            let child_node = self.node_mut(child)?;
            child_node.parent = Some(parent);
            child_node.next_sibling = None;
        }
        match previous_last {
            Some(last) => self.node_mut(last)?.next_sibling = Some(child),
            None => self.node_mut(parent)?.first_child = Some(child),
        }
        self.node_mut(parent)?.last_child = Some(child);
        self.drop_hidden_focus();
        Ok(())
    }

    /// Levels above `id` (0 for a node with no parent).
    fn depth(&self, id: NodeId) -> Result<usize, UiError> {
        let mut depth = 0usize;
        let mut current = self.node(id)?.parent;
        while let Some(parent) = current {
            depth = depth.saturating_add(1);
            if depth > MAX_DEPTH {
                return Err(UiError::TooDeep);
            }
            current = self.node(parent)?.parent;
        }
        Ok(depth)
    }

    /// Whether `ancestor` is `id` or above it.
    fn is_ancestor(&self, ancestor: NodeId, id: NodeId) -> Result<bool, UiError> {
        let mut current = Some(id);
        let mut steps = 0usize;
        while let Some(node) = current {
            if node == ancestor {
                return Ok(true);
            }
            steps = steps.saturating_add(1);
            if steps > MAX_DEPTH.saturating_add(1) {
                return Err(UiError::TooDeep);
            }
            current = self.node(node)?.parent;
        }
        Ok(false)
    }

    /// Levels below `id` (0 for a leaf). `level` bounds the recursion.
    fn height(&self, id: NodeId, level: usize) -> Result<usize, UiError> {
        if level > MAX_DEPTH {
            return Err(UiError::TooDeep);
        }
        let mut height = 0usize;
        for child in self.children(id) {
            height = height.max(
                self.height(child, level.saturating_add(1))?
                    .saturating_add(1),
            );
        }
        Ok(height)
    }
}

/// Iterator over a node's children. See [`Tree::children`].
#[derive(Debug, Clone)]
pub struct Children<'a, const N: usize, const T: usize = 0> {
    tree: &'a Tree<N, T>,
    next: Option<NodeId>,
    /// Steps left; bounds the walk even if the links were ever corrupt.
    budget: usize,
}

impl<const N: usize, const T: usize> Iterator for Children<'_, N, T> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        self.budget = self.budget.checked_sub(1)?;
        self.next = self.tree.get(id).and_then(|n| n.next_sibling);
        Some(id)
    }
}

/// Pre-order iterator over the tree from its root, then over each layer
/// shown, bottom to top. See [`Tree::paint_order`]. Uses the parent and
/// sibling links, so it needs no stack.
#[derive(Debug, Clone)]
pub struct PaintOrder<'a, const N: usize, const T: usize = 0> {
    tree: &'a Tree<N, T>,
    root: Option<NodeId>,
    next: Option<NodeId>,
    budget: usize,
    /// The plane being walked: 0 the root's, then each layer shown.
    plane: usize,
    /// Whether to go on to the planes above when this one ends.
    onwards: bool,
}

impl<'a, const N: usize, const T: usize> Iterator for PaintOrder<'a, N, T> {
    type Item = (NodeId, &'a Node);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next.is_none() && self.onwards {
            // The plane above, if there is one.
            self.plane = self.plane.saturating_add(1);
            self.root = self.tree.plane(self.plane);
            self.next = self.root;
        }
        let id = self.next?;
        self.budget = self.budget.checked_sub(1)?;
        let node = self.tree.get(id)?;
        let mut next = match self.tree.first_shown_child(id) {
            Some(child) => Some(child),
            None => self.successor(id),
        };
        // A layer shows in a plane of its own, never where it is written.
        for _ in 0..N {
            match next {
                Some(layer) if self.tree.layer_kind(layer).is_some() => {
                    next = self.successor(layer);
                }
                _ => break,
            }
        }
        self.next = next;
        Some((id, node))
    }
}

impl<const N: usize, const T: usize> PaintOrder<'_, N, T> {
    /// The next node after `id`'s subtree: its next sibling, or the next
    /// sibling of the nearest ancestor that has one, stopping at the root.
    /// A group's child has no next sibling here: a group shows one child.
    fn successor(&self, mut id: NodeId) -> Option<NodeId> {
        for _ in 0..=MAX_DEPTH {
            if Some(id) == self.root {
                return None;
            }
            let node = self.tree.get(id)?;
            let parent = node.parent?;
            let in_group = self
                .tree
                .get(parent)
                .is_some_and(|parent| parent.kind.is_conditional());
            if !in_group && let Some(sibling) = node.next_sibling {
                return Some(sibling);
            }
            id = parent;
        }
        None
    }
}

/// `done` of `whole` in thousandths, to the nearest: all of it for `done`
/// at or past `whole`, none for a `whole` of 0.
fn progress_permille(done: u32, whole: u32) -> u16 {
    if whole == 0 {
        return 0;
    }
    let whole = u64::from(whole);
    let done = u64::from(done).min(whole);
    let permille = done
        .saturating_mul(u64::from(crate::PROGRESS_WHOLE))
        .saturating_add(whole / 2)
        .checked_div(whole)
        .unwrap_or(0);
    u16::try_from(permille).unwrap_or(crate::PROGRESS_WHOLE)
}

/// The first option a list picker of `rows` rows and `count` options shows
/// so that `cursor` is in view, moving `top` no more than it must.
pub(crate) fn shown_top(top: u8, cursor: u8, rows: u8, count: usize) -> u8 {
    let last_top = u8::try_from(count.saturating_sub(usize::from(rows))).unwrap_or(u8::MAX);
    let top = if cursor < top {
        cursor
    } else if cursor >= top.saturating_add(rows) {
        cursor.saturating_sub(rows.saturating_sub(1))
    } else {
        top
    };
    top.min(last_top)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::ActionId;

    fn ids(tree: &Tree<32>) -> std::vec::Vec<usize> {
        tree.paint_order().map(|(id, _)| id.index()).collect()
    }

    #[test]
    fn builds_top_down_and_bottom_up() {
        let mut t: Tree<32> = Tree::new();
        let root = t.column(Style::new()).unwrap(); // 0
        t.set_root(root).unwrap();
        let a = t
            .add_child(root, NodeKind::Text("a"), Style::new())
            .unwrap(); // 1
        // Build a row bottom-up, then attach it.
        let b = t.text("b").unwrap(); // 2
        let row = t.row(Style::new()).unwrap(); // 3
        t.append(row, b).unwrap();
        t.append(root, row).unwrap();
        let c = t.text("c").unwrap(); // 4
        t.append(root, c).unwrap();
        assert_eq!(ids(&t), [0, 1, 3, 2, 4]);
        assert_eq!(t.children(root).collect::<std::vec::Vec<_>>(), [a, row, c]);
        assert_eq!(t.get(b).unwrap().parent(), Some(row));
    }

    #[test]
    fn refuses_bad_links() {
        let mut t: Tree<8> = Tree::new();
        let col = t.column(Style::new()).unwrap();
        let text = t.text("x").unwrap();
        assert_eq!(t.append(text, col), Err(UiError::NotAContainer(text)));
        t.append(col, text).unwrap();
        assert_eq!(t.append(col, text), Err(UiError::AlreadyAttached(text)));
        assert_eq!(t.append(col, col), Err(UiError::WouldCycle));
        let inner = t.column(Style::new()).unwrap();
        t.append(col, inner).unwrap();
        // `col` is above `inner`, so it cannot go inside it.
        t.set_root(col).unwrap();
        assert_eq!(t.append(inner, col), Err(UiError::AlreadyAttached(col)));
        let ghost = NodeId(99);
        assert_eq!(t.append(col, ghost), Err(UiError::InvalidNode(ghost)));
    }

    #[test]
    fn detects_cycles_between_detached_subtrees() {
        let mut t: Tree<8> = Tree::new();
        let outer = t.column(Style::new()).unwrap();
        let inner = t.column(Style::new()).unwrap();
        t.append(outer, inner).unwrap();
        // `outer` is detached (no parent, not root) but `inner` is inside it.
        assert_eq!(t.append(inner, outer), Err(UiError::WouldCycle));
    }

    #[test]
    fn capacity_is_enforced() {
        let mut t: Tree<2> = Tree::new();
        t.text("a").unwrap();
        t.text("b").unwrap();
        assert_eq!(t.text("c"), Err(UiError::TreeFull { capacity: 2 }));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn depth_is_bounded_either_way_round() {
        // Top-down: a chain of columns MAX_DEPTH + 1 long is the limit.
        let mut t: Tree<64> = Tree::new();
        let mut parent = t.column(Style::new()).unwrap();
        for _ in 0..MAX_DEPTH {
            parent = t.add_child(parent, NodeKind::Column, Style::new()).unwrap();
        }
        assert_eq!(
            t.add_child(parent, NodeKind::Column, Style::new()),
            Err(UiError::TooDeep)
        );
        // Bottom-up: joining two chains that are fine alone but too deep together.
        let mut u: Tree<64> = Tree::new();
        let top = u.column(Style::new()).unwrap();
        let mut deep = top;
        for _ in 0..10 {
            deep = u.add_child(deep, NodeKind::Column, Style::new()).unwrap();
        }
        let other = u.column(Style::new()).unwrap();
        let mut other_leaf = other;
        for _ in 0..10 {
            other_leaf = u
                .add_child(other_leaf, NodeKind::Column, Style::new())
                .unwrap();
        }
        assert_eq!(u.append(deep, other), Err(UiError::TooDeep));
        assert!(other_leaf.index() > 0);
    }

    #[test]
    fn add_child_checks_the_parent_before_using_a_slot() {
        let mut t: Tree<4> = Tree::new();
        let text = t.text("x").unwrap();
        assert_eq!(
            t.add_child(text, NodeKind::Spacer, Style::new()),
            Err(UiError::NotAContainer(text))
        );
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn values_can_be_set_read_and_wiped() {
        let mut t: Tree<4> = Tree::new();
        let field = FieldId(7);
        t.text_input(field, "name").unwrap();
        assert_eq!(t.value(field), Some(""));
        t.set_value(field, "Ada").unwrap();
        assert_eq!(t.value(field), Some("Ada"));
        let long = "x".repeat(crate::INPUT_CAPACITY + 1);
        assert!(matches!(
            t.set_value(field, &long),
            Err(UiError::ValueTooLong(_))
        ));
        assert_eq!(t.value(field), Some("Ada"));
        t.clear_values();
        assert_eq!(t.value(field), Some(""));
        assert_eq!(
            t.set_value(FieldId(8), "x"),
            Err(UiError::UnknownField(FieldId(8)))
        );
    }

    /// The typed value's storage is zeroed in place, every byte of it - not
    /// merely emptied (`BoundedStr::is_zeroed`): by `clear_values`,
    /// and by the wipe `clear` gives each node before letting it go.
    #[test]
    fn clearing_values_zeroes_their_storage() {
        let mut t: Tree<4> = Tree::new();
        let input = t.text_input(FieldId(1), "name").unwrap();
        let zeroed = |t: &Tree<4>| match t.get(input).unwrap().kind() {
            NodeKind::TextInput { value, .. } => value.is_zeroed(),
            _ => false,
        };
        assert!(zeroed(&t));
        t.set_value(FieldId(1), "Ada Lovelace").unwrap();
        assert!(!zeroed(&t));
        let mut held = t.get(input).unwrap().clone();
        t.clear_values();
        assert!(zeroed(&t));
        // What `clear` does to each node first.
        held.wipe();
        assert!(matches!(held.kind(), NodeKind::TextInput { value, .. } if value.is_zeroed()));
    }

    const REF: crate::SlotId = crate::SlotId(1);
    const ANSWER: crate::SlotId = crate::SlotId(2);

    /// A column holding a static line and two slots, laid out.
    fn review() -> (Tree<8>, NodeId, NodeId) {
        let mut t: Tree<8> = Tree::new();
        let root = t.column(Style::new().padding(4).gap(2)).unwrap();
        t.set_root(root).unwrap();
        let title = t.text("Check your answers").unwrap();
        t.append(root, title).unwrap();
        let reference = t.slot(REF).unwrap();
        t.append(root, reference).unwrap();
        let answer = t.slot(ANSWER).unwrap();
        t.append(root, answer).unwrap();
        t.layout(Rect::new(0, 0, 160, 60), &crate::TextMetrics::new(6, 10))
            .unwrap();
        t.clear_dirty();
        (t, reference, answer)
    }

    fn slot_storage(t: &Tree<8>, id: NodeId) -> BoundedStr<{ crate::SLOT_CAPACITY }> {
        match t.get(id).unwrap().kind() {
            NodeKind::Slot { text, .. } => text.clone(),
            other => panic!("not a slot: {other:?}"),
        }
    }

    #[test]
    fn a_slot_shows_application_text_and_repaints_only_its_area() {
        let (mut t, reference, answer) = review();
        assert_eq!(t.slot_text(REF), Some(""), "a slot starts empty");
        assert_eq!(t.find_slot(ANSWER), Some(answer));
        t.set_slot(REF, "Ref. 4F-2207").unwrap();
        assert_eq!(t.slot_text(REF), Some("Ref. 4F-2207"));
        // From empty: the text's own width and half a line's margin each
        // side, within the slot - 12 characters of 6, then 5.
        assert_eq!(t.dirty_rects(), &[Rect::new(4, 16, 77, 10)]);
        t.clear_dirty();
        // One character changes: it and the one before it, with the margin.
        t.set_slot(REF, "Ref. 4F-2208").unwrap();
        assert_eq!(t.dirty_rects(), &[Rect::new(59, 16, 22, 10)]);
        t.clear_dirty();
        // The same text: nothing to repaint.
        t.set_slot(REF, "Ref. 4F-2208").unwrap();
        assert!(t.dirty_rects().is_empty());
        assert!(
            t.get(reference)
                .unwrap()
                .rect()
                .contains_rect(&Rect::new(59, 16, 22, 10))
        );
        // A slot fills its column's width, so new text needs no new layout.
        assert_eq!(t.get(answer).unwrap().rect(), Rect::new(4, 28, 152, 10));
        // Shorter text replaces longer in place: the bytes beyond are zero.
        t.set_slot(REF, "Ref. 9").unwrap();
        let stored = slot_storage(&t, reference);
        assert_eq!(stored.as_str(), "Ref. 9");
        assert!(stored.unused_is_zeroed());
    }

    #[test]
    fn slot_text_that_cannot_be_shown_is_refused_and_the_old_text_stays() {
        let (mut t, _, answer) = review();
        t.set_slot(ANSWER, "Ada").unwrap();
        t.clear_dirty();
        let too_long = "x".repeat(crate::SLOT_CAPACITY + 1);
        for (text, error) in [
            (too_long.as_str(), "TextTooLong"),
            ("two\nlines", "ControlCharacter"),
            ("zero\u{200B}width", "InvisibleCharacter"),
            ("soft\u{AD}hyphen", "InvisibleCharacter"),
            ("\u{2014}", "NoGlyph"),
            ("東京", "NoGlyph"),
        ] {
            let refused = t.set_slot(ANSWER, text).unwrap_err();
            assert!(
                std::format!("{refused:?}").starts_with(error),
                "{text:?}: {refused:?}"
            );
            assert_eq!(t.slot_text(ANSWER), Some("Ada"));
        }
        assert_eq!(
            t.set_slot(crate::SlotId(9), "x"),
            Err(UiError::UnknownSlot(crate::SlotId(9)))
        );
        assert!(t.dirty_rects().is_empty(), "nothing repaints on a refusal");
        // Exactly the capacity fits; so does all of printable Latin-1.
        let full = "y".repeat(crate::SLOT_CAPACITY);
        t.set_slot(ANSWER, &full).unwrap();
        t.set_slot(ANSWER, "Zoë, café, 3 × 4 ÷ 2, ½ ¿ ÿ").unwrap();
        assert!(
            t.slot_text(ANSWER)
                .is_some_and(|text| text.starts_with("Zoë"))
        );
        assert_eq!(t.get(answer).unwrap().kind().check_text(), Ok(()));
        // A slot built directly is held to the same rules.
        let mut bad = BoundedStr::new();
        bad.push_str("tab\t").unwrap();
        assert_eq!(
            t.add(
                NodeKind::Slot {
                    slot: REF,
                    text: bad,
                    template: None,
                },
                Style::new()
            ),
            Err(UiError::ControlCharacter)
        );
    }

    #[test]
    fn slots_are_wiped_in_place() {
        let (mut t, reference, answer) = review();
        t.set_slot(REF, "Ref. 4F-2207").unwrap();
        t.set_slot(ANSWER, "Ada").unwrap();
        let mut held = t.get(answer).unwrap().clone();
        t.clear_dirty();
        t.clear_slots();
        for id in [reference, answer] {
            assert!(slot_storage(&t, id).is_zeroed());
            assert!(t.dirty_rects().contains(&t.get(id).unwrap().rect()));
        }
        // What `clear` does to each node before it lets it go.
        held.wipe();
        assert!(matches!(held.kind(), NodeKind::Slot { text, .. } if text.is_zeroed()));
        t.clear();
        assert_eq!(t.slot_text(REF), None);
    }

    #[test]
    fn set_text_only_for_text_and_buttons() {
        let mut t: Tree<4> = Tree::new();
        let text = t.text("old").unwrap();
        let button = t.button("Old", ActionId(1)).unwrap();
        let col = t.column(Style::new()).unwrap();
        t.set_text(text, "new").unwrap();
        t.set_text(button, "New").unwrap();
        assert_eq!(t.get(text).unwrap().kind(), &NodeKind::Text("new"));
        assert!(matches!(
            t.get(button).unwrap().kind(),
            NodeKind::Button { label: "New", .. }
        ));
        assert_eq!(t.set_text(col, "x"), Err(UiError::WrongKind(col)));
    }

    #[test]
    fn single_line_text_refuses_control_characters() {
        // Regression (review 2026-10-01): a '\n' was laid out as one line
        // but drawn as two, the second outside the node.
        let mut t: Tree<8> = Tree::new();
        let refused = Err(UiError::ControlCharacter);
        assert_eq!(t.text("two\nlines"), refused);
        assert_eq!(t.button("tab\tbed", ActionId(1)), refused);
        assert_eq!(t.text_input(FieldId(1), "bell\u{7}"), refused);
        assert_eq!(t.add(NodeKind::Text("cr\r"), Style::new()), refused);
        assert_eq!(t.add(NodeKind::Text("c1\u{85}"), Style::new()), refused);
        let mut typed = BoundedStr::new();
        typed.push_str("a\nb").unwrap();
        let input = NodeKind::TextInput {
            field: FieldId(2),
            placeholder: "",
            value: typed,
            max_len: 64,
            mode: crate::InputMode::Text,
            caret: 0,
            anchor: 0,
        };
        assert_eq!(t.add(input, Style::new()), refused);
        assert!(t.is_empty(), "nothing was created");
        // Changing text later is checked too, and changes nothing.
        let label = t.text("fine").unwrap();
        assert_eq!(t.set_text(label, "no\nway"), Err(UiError::ControlCharacter));
        assert_eq!(t.get(label).unwrap().kind(), &NodeKind::Text("fine"));
        t.text_input(FieldId(1), "name").unwrap();
        t.set_value(FieldId(1), "ok").unwrap();
        assert_eq!(
            t.set_value(FieldId(1), "a\u{0}b"),
            Err(UiError::ControlCharacter)
        );
        assert_eq!(t.value(FieldId(1)), Some("ok"));
        // Printable Latin-1 is fine. Fixed text with a
        // character outside the glyph atlases' set is refused as well,
        // where the first cut drew it as the built-in font's question mark.
        assert!(t.text("Grüße, Zoë").is_ok());
        assert_eq!(t.text("Grüße, Zoë — 東京"), Err(UiError::NoGlyph));
    }

    #[test]
    fn clear_wipes_values_and_empties_the_tree() {
        let mut t: Tree<4> = Tree::new();
        let root = t.column(Style::new()).unwrap();
        t.set_root(root).unwrap();
        let input = t.text_input(FieldId(1), "name").unwrap();
        t.append(root, input).unwrap();
        t.set_value(FieldId(1), "Ada").unwrap();
        t.clear();
        assert!(t.is_empty());
        assert_eq!(t.root(), None);
        assert_eq!(t.value(FieldId(1)), None);
        // The same tree can be built again, starting empty.
        t.text_input(FieldId(1), "name").unwrap();
        assert_eq!(t.value(FieldId(1)), Some(""));
    }

    #[test]
    fn paint_order_of_empty_and_single_node_trees() {
        let mut t: Tree<32> = Tree::new();
        assert!(ids(&t).is_empty());
        let only = t.text("only").unwrap();
        t.set_root(only).unwrap();
        assert_eq!(ids(&t), [0]);
    }
}
