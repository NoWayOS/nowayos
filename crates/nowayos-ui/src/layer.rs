// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layers over the screen (#10076): dialogs, menus, popovers and tooltips.
//!
//! A layer is built with the screen, where it is written, and takes no
//! place there: it is laid out against the viewport - a dialog in the
//! middle, a menu, popover or tooltip beside the control it belongs to, on
//! whichever side has room - and drawn over everything else. The
//! application opens and closes it by its [`LayerId`] ([`Tree::open`],
//! [`Tree::close`]): built with the screen, a layer is whole before it can
//! show, and one that cannot be built fails the screen's build.
//!
//! While a dialog, menu or popover is open, focus is inside it and Tab
//! stays there; what lies beneath cannot be pressed. A dialog veils the
//! screen beneath it until the application closes it; Escape asks it to
//! ([`UiEvent::Dismissed`]). A menu or popover closes itself on Escape or a
//! press outside it, and a menu when one of its buttons is chosen
//! ([`UiEvent::Closed`]). Focus then returns to the control that had it
//! when the layer opened. A tooltip shows while the pointer rests on its
//! control, or the control has focus after a key; it is never focused or
//! pressed.

use nowayos_bounded::BoundedVec;
use nowayos_display::{Point, Rect, Size};

use crate::layout::{Units, style_at};
use crate::{MAX_DEPTH, NodeId, NodeKind, Style, Surface, TextMetrics, Tree, UiError, UiEvent};

/// The most layers open at once: a menu in a dialog, say, and one more.
pub const MAX_OPEN: usize = 4;

/// The room kept between a layer and the viewport's edges, in layout
/// units.
pub const LAYER_MARGIN: u32 = 16;

/// The room between a menu, popover or tooltip and its control, in layout
/// units.
pub const POPUP_GAP: u32 = 4;

/// Which layer, as a number the application defines (builder contract
/// v20): the handle it opens and closes the layer by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u16);

impl LayerId {
    /// The layer numbered `value`.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The layer's number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// What a layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerKind {
    /// A modal dialog, in the middle of the viewport: the screen beneath is
    /// veiled and out of reach until the application closes it.
    Dialog,
    /// Choices beside their control: closed by Escape, a press outside or
    /// choosing one of its buttons; Up and Down move between them.
    Menu,
    /// Content beside its control: closed by Escape or a press outside.
    Popover,
    /// A line of text beside its control, shown while the pointer rests
    /// on the control or it has focus after a key; never focused or
    /// pressed.
    Tooltip,
}

impl LayerKind {
    /// Whether focus moves into it, stays there while it is open and
    /// nothing beneath can be pressed: all but a tooltip.
    #[must_use]
    pub const fn traps(self) -> bool {
        !matches!(self, Self::Tooltip)
    }

    /// Whether it closes itself on Escape or a press outside: a menu or a
    /// popover.
    #[must_use]
    pub const fn light(self) -> bool {
        matches!(self, Self::Menu | Self::Popover)
    }
}

/// The open layers, bottom to top, each with the control that had focus
/// when it opened; and the tooltip showing.
#[derive(Debug, Clone)]
pub(crate) struct Layers {
    pub(crate) open: BoundedVec<(NodeId, Option<NodeId>), MAX_OPEN>,
    pub(crate) tooltip: Option<NodeId>,
}

impl Layers {
    pub(crate) fn new() -> Self {
        Self {
            open: BoundedVec::new(),
            tooltip: None,
        }
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// The layer node bound to `layer`, if any.
    #[must_use]
    pub fn find_layer(&self, layer: LayerId) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(index, node)| {
            matches!(node.kind, NodeKind::Layer { layer: Some(l), .. } if l == layer)
                .then(|| u16::try_from(index).ok().map(NodeId))
                .flatten()
        })
    }

    /// Whether `layer` is open.
    #[must_use]
    pub fn is_open(&self, layer: LayerId) -> bool {
        self.find_layer(layer)
            .is_some_and(|id| self.layers.open.iter().any(|&(open, _)| open == id))
    }

    /// Opens `layer` over everything open: placed, drawn and, unless it is
    /// a tooltip, holding focus - on its first control - until it closes.
    /// Opening one already open does nothing.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownLayer`]; [`UiError::LayerHidden`] for one inside
    /// a hidden group or a closed layer; [`UiError::TooManyLayers`] with
    /// [`MAX_OPEN`] open.
    pub fn open(&mut self, layer: LayerId) -> Result<(), UiError> {
        let id = self.find_layer(layer).ok_or(UiError::UnknownLayer(layer))?;
        if self.layers.open.iter().any(|&(open, _)| open == id) {
            return Ok(());
        }
        if !self.is_shown(id) {
            return Err(UiError::LayerHidden(layer));
        }
        self.layers
            .open
            .push((id, self.focus))
            .map_err(|_| UiError::TooManyLayers)?;
        self.hide_tooltip();
        if let Some(metrics) = self.metrics {
            self.place_layer(id, &metrics)?;
        }
        self.mark_layer(id);
        let first = self.focus_order().next();
        self.move_focus(first);
        Ok(())
    }

    /// Closes `layer`, and every layer opened over it, top first; focus
    /// returns to the control that had it when each opened. Closing one
    /// that is not open does nothing.
    ///
    /// # Errors
    ///
    /// [`UiError::UnknownLayer`].
    pub fn close(&mut self, layer: LayerId) -> Result<(), UiError> {
        let id = self.find_layer(layer).ok_or(UiError::UnknownLayer(layer))?;
        if let Some(index) = self.layers.open.iter().position(|&(open, _)| open == id) {
            self.close_from(index);
        }
        Ok(())
    }

    /// Closes the open layers from `index` up, top first.
    pub(crate) fn close_from(&mut self, index: usize) {
        while self.layers.open.len() > index {
            let Some((id, opener)) = self.layers.open.pop() else {
                break;
            };
            self.mark_layer(id);
            let back = opener.filter(|&opener| {
                self.is_shown(opener) && self.get(opener).is_some_and(crate::Node::is_focusable)
            });
            self.move_focus(back);
        }
    }

    /// Closes every layer, without moving focus back: for a session wipe.
    pub(crate) fn close_all(&mut self) {
        if !self.layers.open.is_empty() || self.layers.tooltip.is_some() {
            self.dirty.mark_all();
        }
        self.layers.open.clear();
        self.layers.tooltip = None;
    }

    /// The layers the screen shows over its root, bottom to top: each open
    /// layer, then the tooltip showing. A renderer draws the root's nodes,
    /// then each of these ([`Tree::plane_order`]).
    pub fn shown_layers(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.layers
            .open
            .iter()
            .map(|&(id, _)| id)
            .chain(self.layers.tooltip)
    }

    /// Plane `index` of the screen: 0 the root, then each layer shown,
    /// bottom to top ([`shown_layers`](Self::shown_layers)).
    #[must_use]
    pub fn plane(&self, index: usize) -> Option<NodeId> {
        match index.checked_sub(1) {
            None => self.root,
            Some(layer) => self.shown_layers().nth(layer),
        }
    }

    /// Whether plane `index` is veiled: a dialog is open above it.
    #[must_use]
    pub fn is_veiled(&self, index: usize) -> bool {
        self.shown_layers()
            .enumerate()
            .any(|(at, id)| at >= index && self.layer_kind(id) == Some(LayerKind::Dialog))
    }

    /// What layer node `id` is.
    #[must_use]
    pub fn layer_kind(&self, id: NodeId) -> Option<LayerKind> {
        match self.get(id)?.kind {
            NodeKind::Layer { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// The plane node `id` is drawn in: the nearest layer it is inside, or
    /// the root's (`None`).
    #[must_use]
    pub fn layer_of(&self, id: NodeId) -> Option<NodeId> {
        let mut current = Some(id);
        for _ in 0..=MAX_DEPTH.saturating_add(1) {
            let node = self.get(current?)?;
            if matches!(node.kind, NodeKind::Layer { .. }) {
                return current;
            }
            current = node.parent;
        }
        None
    }

    /// The open layer on top that holds focus, if any.
    pub(crate) fn trapping(&self) -> Option<NodeId> {
        self.layers
            .open
            .iter()
            .rev()
            .map(|&(id, _)| id)
            .find(|&id| self.layer_kind(id).is_some_and(LayerKind::traps))
    }

    /// The deepest node of the layers at `point`, from the top: `Some`
    /// with a node of the top layer holding focus or of one above it, or
    /// with `None` when that layer holds focus and `point` is outside it -
    /// nothing beneath may be hit; `None` when no layer is open.
    pub(crate) fn hit_layers(&self, point: Point) -> Option<Option<NodeId>> {
        for &(id, _) in self.layers.open.as_slice().iter().rev() {
            if let Some(hit) = self.hit_in(id, point, 0) {
                return Some(Some(hit));
            }
            if self.layer_kind(id).is_some_and(LayerKind::traps) {
                return Some(None);
            }
        }
        None
    }

    /// The deepest node under `point` in layer `id`'s subtree.
    fn hit_in(&self, id: NodeId, point: Point, level: usize) -> Option<NodeId> {
        let node = self.get(id)?;
        if level > MAX_DEPTH || !node.rect.contains(point) {
            return None;
        }
        if node.kind.is_conditional() && !matches!(node.kind, NodeKind::Layer { .. }) {
            return self.hit_in(self.group_branch(id)?, point, level.saturating_add(1));
        }
        let mut found = None;
        for child in self.children(id) {
            if matches!(self.get(child)?.kind, NodeKind::Layer { .. }) {
                continue;
            }
            if let Some(hit) = self.hit_in(child, point, level.saturating_add(1)) {
                found = Some(hit);
            }
        }
        found.or(Some(id))
    }

    /// A press at `point` while a layer is open: outside a menu or
    /// popover on top, it closes that layer, and the press goes no
    /// further (`Some`); outside a dialog it goes nowhere.
    pub(crate) fn press_outside(&mut self, point: Point) -> Option<Option<UiEvent>> {
        let top = self.trapping()?;
        if self.get(top)?.rect.contains(point) {
            return None;
        }
        if self.layer_kind(top).is_some_and(LayerKind::light) {
            Some(self.dismiss(top))
        } else {
            Some(None)
        }
    }

    /// Escape, or a press outside, in layer `id`: a menu or popover closes
    /// (and everything over it), a dialog asks the application to.
    pub(crate) fn dismiss(&mut self, id: NodeId) -> Option<UiEvent> {
        let NodeKind::Layer {
            layer: Some(layer),
            kind,
            ..
        } = self.get(id)?.kind
        else {
            return None;
        };
        if kind.light() {
            let index = self.layers.open.iter().position(|&(open, _)| open == id)?;
            self.close_from(index);
            Some(UiEvent::Closed(layer))
        } else {
            Some(UiEvent::Dismissed(layer))
        }
    }

    /// After a button in a menu was chosen: the menu it is in closes.
    pub(crate) fn chosen_in_menu(&mut self, button: NodeId) {
        if let Some(menu) = self.layer_of(button)
            && self.layer_kind(menu) == Some(LayerKind::Menu)
            && let Some(index) = self.layers.open.iter().position(|&(open, _)| open == menu)
        {
            self.close_from(index);
        }
    }

    /// Shows the tooltip of the control the pointer rests on, or that has
    /// focus after a key, and hides any other.
    pub(crate) fn update_tooltip(&mut self) {
        let over = if self.pointer_shown {
            self.hit_test(self.pointer)
        } else {
            None
        };
        let focused = self.focus.filter(|_| self.focus_visible);
        // Under a layer holding focus, only what is in it can be hit or
        // focused, so only its controls' tooltips show.
        let wanted = self.nodes.iter().enumerate().find_map(|(index, node)| {
            let NodeKind::Layer {
                kind: LayerKind::Tooltip,
                anchor: Some(anchor),
                ..
            } = node.kind
            else {
                return None;
            };
            let id = NodeId(u16::try_from(index).ok()?);
            let near =
                over.is_some_and(|over| self.is_within(over, anchor)) || focused == Some(anchor);
            (near && self.is_shown(id)).then_some(id)
        });
        if wanted == self.layers.tooltip {
            return;
        }
        self.hide_tooltip();
        if let Some(id) = wanted {
            if let Some(metrics) = self.metrics {
                let _ = self.place_layer(id, &metrics);
            }
            self.layers.tooltip = Some(id);
            self.mark_layer(id);
        }
    }

    /// Hides the tooltip showing, if any.
    fn hide_tooltip(&mut self) {
        if let Some(id) = self.layers.tooltip.take() {
            self.mark_layer(id);
        }
    }

    /// Whether `id` is `ancestor` or inside it.
    pub(crate) fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut current = Some(id);
        for _ in 0..=MAX_DEPTH.saturating_add(1) {
            match current {
                Some(node) if node == ancestor => return true,
                Some(node) => current = self.get(node).and_then(|n| n.parent),
                None => return false,
            }
        }
        false
    }

    /// Marks what layer `id` changes when it opens or closes: the whole
    /// viewport for a dialog, which veils the screen; else its area.
    fn mark_layer(&mut self, id: NodeId) {
        if self.layer_kind(id) == Some(LayerKind::Dialog) {
            self.dirty.mark_all();
        } else {
            self.mark_dirty(id);
        }
    }

    /// Creates a layer of `kind` holding `child`, bound to `layer` and
    /// beside `anchor` if given: closed, taking no place where it is
    /// written. Drawn on a card - a tooltip on a header bar's colours -
    /// with room inside it for its kind.
    ///
    /// # Errors
    ///
    /// [`UiError::DuplicateLayer`]; [`UiError::InvalidNode`] for an
    /// `anchor` not in the tree; as
    /// [`add_with_children`](Self::add_with_children). Nothing is created
    /// on error.
    pub fn add_layer(
        &mut self,
        kind: LayerKind,
        layer: Option<LayerId>,
        anchor: Option<NodeId>,
        child: NodeId,
    ) -> Result<NodeId, UiError> {
        if let Some(layer) = layer
            && self.find_layer(layer).is_some()
        {
            return Err(UiError::DuplicateLayer(layer));
        }
        if let Some(anchor) = anchor {
            self.node(anchor)?;
        }
        let style = match kind {
            LayerKind::Dialog => Style::new().padding(24).surface(Surface::Card),
            LayerKind::Menu => Style::new().padding(4).surface(Surface::Card),
            LayerKind::Popover => Style::new().padding(12).surface(Surface::Card),
            LayerKind::Tooltip => Style::new().padding_xy(8, 4).surface(Surface::Header),
        };
        self.add_parent(
            NodeKind::Layer {
                layer,
                kind,
                anchor,
            },
            style,
            &[child],
        )
    }

    /// Lays out every layer against the viewport, outer layers before the
    /// layers inside them: for [`Tree::layout`].
    pub(crate) fn place_layers(&mut self, metrics: &TextMetrics) -> Result<(), UiError> {
        for depth in 0..=MAX_DEPTH {
            let mut found = false;
            for index in 0..self.nodes.len() {
                let Ok(index) = u16::try_from(index) else {
                    break;
                };
                let id = NodeId(index);
                if self.layer_kind(id).is_none() {
                    continue;
                }
                found = true;
                if self.layer_depth(id) == depth {
                    self.place_layer(id, metrics)?;
                }
            }
            if !found {
                break;
            }
        }
        Ok(())
    }

    /// How many layers `id` is inside, itself not counted.
    fn layer_depth(&self, id: NodeId) -> usize {
        let mut depth = 0_usize;
        let mut current = self.get(id).and_then(|node| node.parent);
        for _ in 0..=MAX_DEPTH {
            let Some(node) = current.and_then(|id| self.get(id)) else {
                break;
            };
            if matches!(node.kind, NodeKind::Layer { .. }) {
                depth = depth.saturating_add(1);
            }
            current = node.parent;
        }
        depth
    }

    /// How deep `id` is: 0 for a node without a parent.
    fn level_of(&self, id: NodeId) -> usize {
        let mut level = 0_usize;
        let mut current = self.get(id).and_then(|node| node.parent);
        for _ in 0..=MAX_DEPTH {
            let Some(node) = current.and_then(|id| self.get(id)) else {
                break;
            };
            level = level.saturating_add(1);
            current = node.parent;
        }
        level
    }

    /// Lays layer `id` out: its child measured in the viewport less the
    /// margins, then the layer placed - a dialog in the middle, any other
    /// beside its control - and its child arranged inside its padding.
    pub(crate) fn place_layer(&mut self, id: NodeId, metrics: &TextMetrics) -> Result<(), UiError> {
        let node = self.node(id)?;
        let NodeKind::Layer { kind, anchor, .. } = node.kind else {
            return Ok(());
        };
        let units = Units::of(metrics);
        let style = style_at(&node.style, units);
        let Some(child) = node.first_child else {
            return Ok(());
        };
        let viewport = self.viewport;
        let margin = units.px(LAYER_MARGIN);
        let area = viewport.inset(nowayos_display::Insets::uniform(margin));
        let padding = style.padding;
        let level = self.level_of(id).saturating_add(1);
        let room = area.size.width.saturating_sub(padding.horizontal());
        let natural = self.measure_at(child, metrics, level, room)?;
        let size = Size::new(
            natural
                .width
                .saturating_add(padding.horizontal())
                .min(area.size.width),
            natural
                .height
                .saturating_add(padding.vertical())
                .min(area.size.height),
        );
        let origin = match (kind, anchor.and_then(|anchor| self.anchor_rect(anchor))) {
            (LayerKind::Dialog, _) | (_, None) => centred(area, size),
            (_, Some(control)) => beside(control, size, area, units.px(POPUP_GAP)),
        };
        let rect = Rect { origin, size };
        let node = self.node_mut(id)?;
        node.rect = rect;
        node.padding = padding;
        self.arrange_at(child, rect.inset(padding), metrics, level)
    }

    /// Where control `id` shows: its area, within every scroll view around
    /// it.
    fn anchor_rect(&self, id: NodeId) -> Option<Rect> {
        let node = self.get(id)?;
        match self.scroll_clip(node) {
            Some(seen) => node.rect.intersection(&seen).or(Some(node.rect)),
            None => Some(node.rect),
        }
    }
}

/// `size` in the middle of `area`.
fn centred(area: Rect, size: Size) -> Point {
    let x = area
        .left()
        .saturating_add(i64::from(area.size.width.saturating_sub(size.width)) / 2);
    let y = area
        .top()
        .saturating_add(i64::from(area.size.height.saturating_sub(size.height)) / 2);
    point(x, y)
}

/// Where `size` goes beside `control`, `gap` from it, inside `area`:
/// below it if it fits there, else above, else to its right, else to its
/// left, else below, pushed inside.
fn beside(control: Rect, size: Size, area: Rect, gap: u32) -> Point {
    let (width, height) = (i64::from(size.width), i64::from(size.height));
    let gap = i64::from(gap);
    let along = |start: i64, low: i64, high: i64, length: i64| {
        start.min(high.saturating_sub(length)).max(low)
    };
    let x = along(control.left(), area.left(), area.right(), width);
    let y = along(control.top(), area.top(), area.bottom(), height);
    let below = control.bottom().saturating_add(gap);
    let above = control.top().saturating_sub(gap).saturating_sub(height);
    let right = control.right().saturating_add(gap);
    let left = control.left().saturating_sub(gap).saturating_sub(width);
    if below.saturating_add(height) <= area.bottom() {
        point(x, below)
    } else if above >= area.top() {
        point(x, above)
    } else if right.saturating_add(width) <= area.right() {
        point(right, y)
    } else if left >= area.left() {
        point(left, y)
    } else {
        point(x, along(below, area.top(), area.bottom(), height))
    }
}

/// A point at `x`, `y`, within `i32`'s range.
fn point(x: i64, y: i64) -> Point {
    let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    Point::new(clamp(x), clamp(y))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failure fails the test")]
mod tests {
    use super::*;

    #[test]
    fn a_popup_goes_below_its_control_else_above_else_beside() {
        let area = Rect::new(0, 0, 400, 300);
        let size = Size::new(100, 80);
        // Room below.
        assert_eq!(
            beside(Rect::new(10, 10, 60, 20), size, area, 4),
            Point::new(10, 34)
        );
        // None below: above.
        assert_eq!(
            beside(Rect::new(10, 250, 60, 20), size, area, 4),
            Point::new(10, 166)
        );
        // At the right edge: pushed left to fit.
        assert_eq!(
            beside(Rect::new(380, 10, 20, 20), size, area, 4),
            Point::new(300, 34)
        );
        // Neither below nor above: to the right.
        let tall = Size::new(100, 280);
        assert_eq!(
            beside(Rect::new(10, 100, 60, 20), tall, area, 4),
            Point::new(74, 20)
        );
        assert_eq!(centred(area, size), Point::new(150, 110));
    }
}
