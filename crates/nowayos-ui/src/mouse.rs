// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mouse (#10082): the control the pointer hovers, drawn so; the
//! pointer's shape, saying what a press there will do; and a context
//! action - a right click, or a finger resting on a control - that a menu
//! the application then opens for that control shows at.
//!
//! The hovered control - a button, an input, a checkbox, one option of a
//! radio group or a list picker's row, one link of a rich text - is marked
//! for repainting only when it changes, and only while the pointer shows: a
//! touch hides both. The pointer is an arrow, a hand over what a press
//! activates, an I-beam over text that can be typed or selected, and
//! not-allowed over the screen beneath a dialog; a node's own
//! ([`Tree::set_cursor`], CSS's `cursor`) wins over its ancestors' and
//! these. A control given a context action ([`Tree::set_context_action`],
//! React's `onContextMenu`) reports it on a right click, or when a finger
//! has rested on it for [`LONG_PRESS`] milliseconds and lifts; a menu or
//! popover the application opens for that control in answer opens at the
//! point pressed, as a desktop's context menu does.

use nowayos_bounded::BoundedVec;
use nowayos_display::{Point, Rect};

use crate::{ActionId, LayerKind, NodeId, NodeKind, Tree, UiError, UiEvent};

/// The most nodes with a context action, and with a cursor of their own,
/// in a tree.
pub const MAX_MOUSE_NODES: usize = 32;

/// How long a finger rests on a control, in milliseconds, before lifting it
/// reports the control's context action.
pub const LONG_PRESS: u32 = 500;

/// How far a finger may move, in pixels each way, and still rest.
pub const LONG_PRESS_SLOP: i32 = 12;

/// The pointer's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Cursor {
    /// An arrow: nothing in particular (CSS's `default`).
    #[default]
    Arrow,
    /// A hand: a press activates what is under it (CSS's `pointer`).
    Hand,
    /// An I-beam: text to type or select (CSS's `text`).
    Text,
    /// Not allowed: nothing here can be pressed (CSS's `not-allowed`).
    NotAllowed,
}

impl Cursor {
    /// Every shape.
    pub const ALL: [Self; 4] = [Self::Arrow, Self::Hand, Self::Text, Self::NotAllowed];

    /// CSS's name for it.
    #[must_use]
    pub const fn css(self) -> &'static str {
        match self {
            Self::Arrow => "default",
            Self::Hand => "pointer",
            Self::Text => "text",
            Self::NotAllowed => "not-allowed",
        }
    }
}

/// What the pointer hovers: a control, and the part of it - a radio
/// group's option, a list picker's row, a rich text's link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hover {
    /// The control.
    pub node: Option<NodeId>,
    /// Its part, if it has parts.
    pub part: Option<u16>,
}

/// A finger resting on a control with a context action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rest {
    node: NodeId,
    action: ActionId,
    from: Point,
    held: u32,
}

/// The mouse's state in a tree.
#[derive(Debug, Clone)]
pub(crate) struct Mouse {
    pub(crate) hover: Hover,
    /// Each node's context action, by its number.
    context: BoundedVec<(NodeId, u16), MAX_MOUSE_NODES>,
    cursors: BoundedVec<(NodeId, Cursor), MAX_MOUSE_NODES>,
    rest: Option<Rest>,
    /// The control a context action was last reported on, and where: a
    /// menu or popover opened for it opens there.
    pub(crate) context_at: Option<(NodeId, Point)>,
}

impl Mouse {
    pub(crate) fn new() -> Self {
        Self {
            hover: Hover {
                node: None,
                part: None,
            },
            context: BoundedVec::new(),
            cursors: BoundedVec::new(),
            rest: None,
            context_at: None,
        }
    }

    /// Forgets what the pointer hovered and pressed: the tree's nodes are
    /// gone or a session ends. What nodes were given stays only for a
    /// session end; [`Tree::clear`] empties those too.
    pub(crate) const fn forget(&mut self) {
        self.hover = Hover {
            node: None,
            part: None,
        };
        self.rest = None;
        self.context_at = None;
    }

    pub(crate) fn clear(&mut self) {
        self.forget();
        self.context.clear();
        self.cursors.clear();
    }

    /// Whether a finger rests on a control with a context action, so the
    /// frame clock should run to time it.
    pub(crate) const fn resting(&self) -> bool {
        matches!(self.rest, Some(rest) if rest.held < LONG_PRESS)
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// Gives node `id` a context action (builder contract v25; React's
    /// `onContextMenu`): reported on a right click on it, or when a finger
    /// has rested on it for [`LONG_PRESS`] milliseconds and lifts. Giving
    /// one again replaces it.
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`]; [`UiError::TooManyMouseNodes`] past
    /// [`MAX_MOUSE_NODES`].
    pub fn set_context_action(&mut self, id: NodeId, action: ActionId) -> Result<(), UiError> {
        self.node(id)?;
        set(&mut self.mouse.context, id, action.0)
    }

    /// Gives node `id` a pointer shape of its own (builder contract v25;
    /// CSS's `cursor`), shown over it and what it holds unless a node
    /// inside has one.
    ///
    /// # Errors
    ///
    /// As [`set_context_action`](Self::set_context_action).
    pub fn set_cursor(&mut self, id: NodeId, cursor: Cursor) -> Result<(), UiError> {
        self.node(id)?;
        set(&mut self.mouse.cursors, id, cursor)
    }

    /// What the pointer hovers, while it shows.
    #[must_use]
    pub const fn hover(&self) -> Hover {
        self.mouse.hover
    }

    /// The pointer's shape where it is.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        let at = self.pointer;
        if self.beneath_dialog(at) {
            return Cursor::NotAllowed;
        }
        let Some(hit) = self.hit_test(at) else {
            return Cursor::Arrow;
        };
        // A node's own, or its nearest ancestor's.
        let mut node = Some(hit);
        for _ in 0..=crate::MAX_DEPTH {
            let Some(id) = node else { break };
            if let Some(&(_, cursor)) = self.mouse.cursors.iter().find(|(with, _)| *with == id) {
                return cursor;
            }
            node = self.get(id).and_then(crate::Node::parent);
        }
        let Some(node) = self.get(hit) else {
            return Cursor::Arrow;
        };
        match &node.kind {
            NodeKind::TextInput { .. } | NodeKind::TextArea { .. } => Cursor::Text,
            NodeKind::Text(_) | NodeKind::Slot { .. } if node.style.text.selectable => Cursor::Text,
            NodeKind::Rich { .. } => {
                if self.link_at(hit, at).is_some() {
                    Cursor::Hand
                } else {
                    Cursor::Arrow
                }
            }
            _ if node.is_focusable() || self.key_under_pointer().is_some() => Cursor::Hand,
            _ => Cursor::Arrow,
        }
    }

    /// Whether `at` is on the screen beneath an open dialog, outside every
    /// layer over it: nothing there can be pressed.
    fn beneath_dialog(&self, at: Point) -> bool {
        for &(id, _) in self.layers.open.as_slice().iter().rev() {
            if self.get(id).is_some_and(|layer| layer.rect.contains(at)) {
                return false;
            }
            if self.layer_kind(id) == Some(LayerKind::Dialog) {
                return true;
            }
        }
        false
    }

    /// Updates what the pointer hovers, marking what changes.
    pub(crate) fn update_hover(&mut self) {
        let hover = if self.pointer_shown {
            self.hover_at(self.pointer)
        } else {
            Hover::default()
        };
        if hover == self.mouse.hover {
            return;
        }
        for node in [self.mouse.hover.node, hover.node].into_iter().flatten() {
            self.mark_dirty(node);
        }
        self.mouse.hover = hover;
    }

    /// What a pointer at `at` hovers.
    fn hover_at(&self, at: Point) -> Hover {
        let Some(id) = self.control_at(at) else {
            return Hover::default();
        };
        let Some(node) = self.get(id) else {
            return Hover::default();
        };
        let part = match &node.kind {
            NodeKind::Rich { .. } => match self.link_at(id, at) {
                Some(link) => Some(u16::from(link)),
                // A rich text is hovered only on a link.
                None => return Hover::default(),
            },
            NodeKind::RadioGroup { .. } => node.option_at(at).and_then(|at| u16::try_from(at).ok()),
            NodeKind::ListPicker { rows, .. } => (1..=usize::from(*rows))
                .find(|&row| node.list_row(row).is_some_and(|rect| rect.contains(at)))
                .and_then(|row| u16::try_from(row).ok()),
            _ => None,
        };
        Hover {
            node: Some(id),
            part,
        }
    }

    /// The context action of the node under `at`, or of its nearest
    /// ancestor with one.
    fn context_action_at(&self, at: Point) -> Option<(NodeId, ActionId)> {
        let mut node = self.hit_test(at);
        for _ in 0..=crate::MAX_DEPTH {
            let id = node?;
            if let Some(&(with, action)) = self.mouse.context.iter().find(|(with, _)| *with == id) {
                return Some((with, ActionId(action)));
            }
            node = self.get(id).and_then(crate::Node::parent);
        }
        None
    }

    /// A right click: the context action under the pointer, if any.
    pub(crate) fn context_click(&mut self) -> Option<UiEvent> {
        let (node, action) = self.context_action_at(self.pointer)?;
        self.mouse.context_at = Some((node, self.pointer));
        Some(UiEvent::Action(action))
    }

    /// A finger touching: it may rest on a control with a context action.
    pub(crate) fn touch_started(&mut self) {
        self.mouse.rest = self
            .context_action_at(self.pointer)
            .map(|(node, action)| Rest {
                node,
                action,
                from: self.pointer,
                held: 0,
            });
    }

    /// A finger moving: past [`LONG_PRESS_SLOP`] it no longer rests.
    pub(crate) fn touch_moved(&mut self) {
        let at = self.pointer;
        if let Some(rest) = self.mouse.rest
            && (at.x.saturating_sub(rest.from.x).saturating_abs() > LONG_PRESS_SLOP
                || at.y.saturating_sub(rest.from.y).saturating_abs() > LONG_PRESS_SLOP)
        {
            self.mouse.rest = None;
        }
    }

    /// A finger lifting: after a long rest, the press it began is let go
    /// and the context action reported in its place.
    pub(crate) fn touch_ended(&mut self) -> Option<UiEvent> {
        let rest = self.mouse.rest.take()?;
        if rest.held < LONG_PRESS {
            return None;
        }
        self.set_pressed(None);
        self.drag = None;
        self.mouse.context_at = Some((rest.node, rest.from));
        Some(UiEvent::Action(rest.action))
    }

    /// Lets `elapsed` milliseconds pass for a resting finger.
    pub(crate) const fn rest_for(&mut self, elapsed: u32) {
        if let Some(rest) = &mut self.mouse.rest {
            rest.held = rest.held.saturating_add(elapsed);
        }
    }

    /// Where a layer anchored to `id` is placed beside: the point a
    /// context action was reported at on it, if one was.
    pub(crate) fn context_anchor(&self, id: NodeId) -> Option<Rect> {
        match self.mouse.context_at {
            Some((node, at)) if node == id => Some(Rect::new(at.x, at.y, 1, 1)),
            _ => None,
        }
    }
}

/// Sets `id`'s entry in `table` to `value`.
fn set<V: Copy + Default>(
    table: &mut BoundedVec<(NodeId, V), MAX_MOUSE_NODES>,
    id: NodeId,
    value: V,
) -> Result<(), UiError> {
    if let Some(entry) = table.iter_mut().find(|(with, _)| *with == id) {
        entry.1 = value;
        return Ok(());
    }
    table
        .push((id, value))
        .map_err(|_| UiError::TooManyMouseNodes)
}
