// SPDX-License-Identifier: MIT OR Apache-2.0
//! Hit-testing, focus and input handling.

use nowayos_display::Point;
use nowayos_input::keymap::{self, Layout, Typed};
use nowayos_input::{InputEvent, KeyCode, TouchPhase};

use crate::keyboard::KeyAction;
use crate::tree::MAX_DEPTH;
use crate::{ActionId, FieldId, InputMode, NodeId, NodeKind, Tree, UiError};

/// A step of zoom (#10083): the whole interface larger, smaller or as it
/// was, in proportion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zoom {
    /// A quarter of a pixel a unit larger: Ctrl and plus.
    In,
    /// A quarter smaller: Ctrl and minus.
    Out,
    /// As the display has it: Ctrl and zero.
    Reset,
}

impl Zoom {
    /// The device scale after this step from `now`, the display's own
    /// being `base`: a quarter up or down, within 1 to 4, or back to
    /// `base`.
    #[must_use]
    pub const fn apply(
        self,
        now: nowayos_display::DeviceScale,
        base: nowayos_display::DeviceScale,
    ) -> nowayos_display::DeviceScale {
        let quarters = match self {
            Self::In => now.quarters().saturating_add(1),
            Self::Out => now.quarters().saturating_sub(1),
            Self::Reset => return base,
        };
        match nowayos_display::DeviceScale::from_quarters(quarters) {
            Some(scale) => scale,
            None => now,
        }
    }

    /// The step `key` asks for with Ctrl held: plus (`=` or the keypad's),
    /// minus or zero.
    #[must_use]
    pub const fn of_key(key: KeyCode) -> Option<Self> {
        match key.usage() {
            0x2E | 0x57 => Some(Self::In),
            0x2D | 0x56 => Some(Self::Out),
            0x27 | 0x62 => Some(Self::Reset),
            _ => None,
        }
    }
}

/// What the UI reports back to the application after an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiEvent {
    /// A button was activated: a press and release over it (pointer or first
    /// touch contact), or Enter / Space while it has focus.
    Action(ActionId),
    /// Keyboard focus moved (`None`: nothing is focused).
    FocusChanged(Option<NodeId>),
    /// A text input's value changed; read it with
    /// [`Tree::value`](crate::Tree::value).
    ValueChanged(FieldId),
    /// A character was refused because the text input is full: it holds
    /// its `max_len` bytes.
    InputFull(FieldId),
    /// A checkbox was ticked or unticked; `checked` is its new value.
    Toggled {
        /// The checkbox's field.
        field: FieldId,
        /// Whether it is now ticked.
        checked: bool,
    },
    /// A radio group's choice changed; `option` is the one now chosen, from
    /// 0.
    Selected {
        /// The radio group's field.
        field: FieldId,
        /// The option now chosen.
        option: u8,
    },
    /// A virtual list's window of items moved (builder contract v18): the
    /// application binds each row that now shows another item
    /// ([`Tree::list_row`](crate::Tree::list_row)).
    ListMoved(crate::ListId),
    /// The runtime closed a menu or popover (builder contract v20): on
    /// Escape, a press outside it or, for a menu, a choice made in it.
    Closed(crate::LayerId),
    /// Escape was pressed in an open dialog (builder contract v20): the
    /// application may close it ([`Tree::close`](crate::Tree::close)).
    Dismissed(crate::LayerId),
    /// Ctrl and plus, minus or zero (#10083): the person asks for the
    /// whole interface larger, smaller or as it was. The loop steps its
    /// scale ([`Zoom::apply`]) and the screen is laid out again.
    Zoom(Zoom),
}

/// How far, in layout units, a touch on the scroll view must move before it
/// scrolls instead of pressing.
pub(crate) const DRAG_SLOP: u32 = 8;

/// How many lines a notch of a mouse's wheel scrolls the scroll view.
pub(crate) const WHEEL_LINES: u32 = 3;

impl<const N: usize, const T: usize> Tree<N, T> {
    /// The deepest node whose area contains `point`, preferring later
    /// siblings (drawn on top). Parts of a child outside its parent's area
    /// cannot be hit.
    #[must_use]
    pub fn hit_test(&self, point: Point) -> Option<NodeId> {
        // An open layer first: under a dialog, menu or popover nothing
        // beneath can be hit (#10076).
        if let Some(hit) = self.hit_layers(point) {
            return hit;
        }
        self.hit(self.root?, point, 0)
    }

    fn hit(&self, id: NodeId, point: Point, level: usize) -> Option<NodeId> {
        let node = self.get(id)?;
        if level > MAX_DEPTH || !node.rect.contains(point) {
            return None;
        }
        if node.kind.is_conditional() {
            // Only the child a group or switch shows can be hit; never it.
            return self.hit(self.group_branch(id)?, point, level.saturating_add(1));
        }
        let mut found = None;
        for child in self.children(id) {
            if let Some(hit) = self.hit(child, point, level.saturating_add(1)) {
                found = Some(hit);
            }
        }
        found.or(Some(id))
    }

    /// The button or text input at `point`, if any.
    #[must_use]
    pub fn control_at(&self, point: Point) -> Option<NodeId> {
        self.hit_test(point)
            .filter(|&id| self.get(id).is_some_and(|node| node.is_focusable()))
    }

    /// The node with keyboard focus.
    #[must_use]
    pub const fn focused(&self) -> Option<NodeId> {
        self.focus
    }

    /// The control currently held down by the pointer or a touch.
    #[must_use]
    pub const fn pressed(&self) -> Option<NodeId> {
        self.pressed
    }

    /// The key of the on-screen keyboard held down, by its index in the
    /// keyboard's layout ([`keyboard`](crate::keyboard)), while the keyboard
    /// is [`pressed`](Self::pressed).
    #[must_use]
    pub fn pressed_key(&self) -> Option<usize> {
        self.pressed.and(self.pressed_key).map(usize::from)
    }

    /// The on-screen keyboard, if the tree has one.
    #[must_use]
    pub fn keyboard_node(&self) -> Option<NodeId> {
        self.paint_order()
            .find(|(_, node)| matches!(node.kind, NodeKind::Keyboard { .. }))
            .map(|(id, _)| id)
    }

    /// The text input the on-screen keyboard types into: the focused
    /// control, if it is a text input. Without one, its keys type
    /// nothing.
    #[must_use]
    pub fn typing_target(&self) -> Option<NodeId> {
        self.focus.filter(|&id| self.input_mode(Some(id)).is_some())
    }

    /// Moves focus to `id` (or clears it with `None`).
    ///
    /// # Errors
    ///
    /// [`UiError::InvalidNode`], or [`UiError::NotFocusable`] for a node
    /// that cannot take focus or that a group hides.
    pub fn set_focus(&mut self, id: Option<NodeId>) -> Result<(), UiError> {
        if let Some(id) = id
            && (!self.node(id)?.is_focusable() || !self.is_shown(id))
        {
            return Err(UiError::NotFocusable(id));
        }
        self.move_focus(id);
        Ok(())
    }

    /// Moves focus to the next button or text input in tree order, wrapping
    /// round; returns the newly focused node.
    pub fn focus_next(&mut self) -> Option<NodeId> {
        let next = match self.focus {
            None => self.focus_order().next(),
            Some(current) => {
                let mut after = self.focus_order().skip_while(|&id| id != current);
                after.next();
                after.next().or_else(|| self.focus_order().next())
            }
        };
        self.move_focus(next);
        next
    }

    /// Moves focus to the previous button or text input in tree order,
    /// wrapping round; returns the newly focused node.
    pub fn focus_prev(&mut self) -> Option<NodeId> {
        let current = self.focus;
        let mut before = None;
        let mut last = None;
        let mut seen_current = false;
        for id in self.focus_order() {
            if Some(id) == current {
                seen_current = true;
            }
            if !seen_current {
                before = Some(id);
            }
            last = Some(id);
        }
        let prev = if seen_current { before.or(last) } else { last };
        self.move_focus(prev);
        prev
    }

    /// Moves focus to the first control, in focus order, whose field has an
    /// error showing ([`set_field_error`](Self::set_field_error)), and
    /// returns it - for when the person tries to move on from a step with
    /// errors. Returns `None`, leaving focus alone, if no field with a
    /// control has an error.
    pub fn focus_first_error(&mut self) -> Option<NodeId> {
        let first = self.focus_order().find(|&id| {
            self.get(id)
                .and_then(|node| node.kind().field())
                .and_then(|field| self.field_error_message(field))
                .is_some()
        })?;
        self.move_focus(Some(first));
        Some(first)
    }

    /// Applies one canonical input event and reports what it meant, if
    /// anything.
    ///
    /// * Pointer button 0 and the first touch contact press and release
    ///   controls; a press focuses the control, and releasing over the same
    ///   control activates it: a button reports its action, a checkbox
    ///   toggles, and a radio group chooses the option under the pointer.
    /// * A key shows the focus ring, and a press of the pointer or a touch
    ///   hides it ([`focus_visible`](Self::focus_visible)); a move
    ///   of a mouse or a tablet shows the pointer, and a touch hides it
    ///   ([`pointer_shown`](Self::pointer_shown)).
    /// * A mouse's wheel scrolls a list picker under the pointer a row a
    ///   notch, and otherwise the scroll view, wherever the pointer is,
    ///   three lines a notch; away from the user scrolls up.
    /// * Tab / Shift+Tab and the Down / Up arrows move focus - except in a
    ///   focused radio group, where the arrow keys choose the next or
    ///   previous option, wrapping round.
    /// * Enter or Space activates a focused button; Space toggles a focused
    ///   checkbox, and chooses the first option of a focused radio group
    ///   with nothing chosen.
    /// * In a focused text input, printable keys (US layout, see
    ///   [`keymap`]) append, up to its `max_len` and only characters its
    ///   [`InputMode`](crate::InputMode) takes; Backspace deletes the last
    ///   character, and Enter moves to the next control.
    /// * A key of the on-screen keyboard, pressed and released
    ///   over it, sends its key event to the focused text input, as above,
    ///   without taking focus from it; its Shift makes the next letter a
    ///   capital. While no text input has focus its keys do nothing.
    ///
    /// Events are expected to be validated already (`nowayos-input`'s
    /// `InputLimits`); anything unexpected is ignored, never a panic.
    pub fn handle(&mut self, event: InputEvent) -> Option<UiEvent> {
        let reported = self.respond(event);
        // The tooltip of what the pointer rests on, or what has focus.
        self.update_tooltip();
        // A virtual list whose window moved, if nothing else is reported.
        reported.or_else(|| self.list_moved.take().map(UiEvent::ListMoved))
    }

    /// What `event` does, as [`handle`](Self::handle) says.
    fn respond(&mut self, event: InputEvent) -> Option<UiEvent> {
        match event {
            InputEvent::PointerMove { x, y } => {
                let moved = self.pointer != Point::new(x, y);
                self.pointer = Point::new(x, y);
                self.pointer_shown = true;
                self.update_hover();
                self.thumb_to();
                // A text input held by the mouse selects as it moves.
                if moved
                    && let Some(held) = self.pressed
                    && matches!(
                        self.get(held).map(|node| &node.kind),
                        Some(NodeKind::TextInput { .. })
                    )
                {
                    self.place_caret(held, false);
                }
                None
            }
            InputEvent::PointerButton { button: 0, down } => {
                self.fling = None;
                if down {
                    // A scrollbar's thumb is held, not pressed.
                    if let Some((view, vertical)) = self.thumb_at(self.pointer) {
                        self.hold_thumb(view, vertical);
                        return None;
                    }
                    self.press()
                } else if self.thumb.take().is_some() {
                    None
                } else {
                    self.release()
                }
            }
            // A right click: a context action (#10082).
            InputEvent::PointerButton {
                button: 1,
                down: true,
            } => self.context_click(),
            InputEvent::Touch { id: 0, x, y, phase } => {
                self.pointer = Point::new(x, y);
                // A finger needs no pointer drawn under it, and hovers
                // nothing.
                self.pointer_shown = false;
                self.update_hover();
                match phase {
                    TouchPhase::Start => {
                        self.fling = None;
                        if let Some((view, vertical)) = self.thumb_at(self.pointer) {
                            self.hold_thumb(view, vertical);
                            return None;
                        }
                        let event = self.press();
                        self.start_drag();
                        // It may rest, for a context action.
                        self.touch_started();
                        event
                    }
                    TouchPhase::End => {
                        if self.thumb.take().is_some() {
                            return None;
                        }
                        if let Some(context) = self.touch_ended() {
                            return Some(context);
                        }
                        self.fling_on();
                        self.drag = None;
                        self.release()
                    }
                    TouchPhase::Move => {
                        self.touch_moved();
                        if self.thumb.is_some() {
                            self.thumb_to();
                        } else {
                            self.drag_to();
                        }
                        None
                    }
                    TouchPhase::Cancel => {
                        self.mouse.forget();
                        self.drag = None;
                        self.thumb = None;
                        self.set_pressed(None);
                        None
                    }
                }
            }
            InputEvent::PointerButton { .. } | InputEvent::Touch { .. } => None,
            InputEvent::Wheel { notches } => {
                self.fling = None;
                self.wheel(notches);
                None
            }
            InputEvent::KeyDown(key)
                if key.is_shift() || matches!(key, KeyCode::LEFT_CTRL | KeyCode::RIGHT_CTRL) =>
            {
                self.keymap.key_down(key);
                self.set_shift_key(key, true);
                None
            }
            InputEvent::KeyUp(key) => {
                self.keymap.key_up(key);
                self.set_shift_key(key, false);
                None
            }
            InputEvent::KeyDown(key) => {
                // What it types on the keyboard's layout (#10081).
                let typed = self.keymap.key_down(key);
                // A key shows the focus ring, and ends any double click.
                self.set_focus_visible(true);
                self.last_click = None;
                self.key_down(key, typed)
            }
        }
    }

    /// Records a Shift key going down or up. Each Shift key is tracked on
    /// its own, so releasing one while the other is held keeps Shift held.
    /// (A Shift release lost upstream — a rate limiter may drop it — leaves
    /// Shift held until that key is pressed and released again.)
    fn set_shift_key(&mut self, key: KeyCode, down: bool) {
        let (keys, index) = match key {
            KeyCode::LEFT_SHIFT => (&mut self.shift_keys, 0),
            KeyCode::RIGHT_SHIFT => (&mut self.shift_keys, 1),
            KeyCode::LEFT_CTRL => (&mut self.ctrl_keys, 0),
            KeyCode::RIGHT_CTRL => (&mut self.ctrl_keys, 1),
            _ => return,
        };
        if let Some(held) = keys.get_mut(index) {
            *held = down;
        }
    }

    /// Whether either Shift key is held.
    fn shift(&self) -> bool {
        self.shift_keys.contains(&true)
    }

    /// Whether either Ctrl key is held.
    fn ctrl(&self) -> bool {
        self.ctrl_keys.contains(&true)
    }

    fn press(&mut self) -> Option<UiEvent> {
        // A press of the pointer or a touch hides the focus ring.
        self.set_focus_visible(false);
        // With nothing open over the screen, a menu opened later opens
        // beside its control, not where a context action was (#10082).
        if self.layers.open.is_empty() {
            self.mouse.context_at = None;
        }
        // A key of the on-screen keyboard is held without taking focus
        // from the input it types into.
        if let Some((board, key)) = self.key_under_pointer() {
            self.set_pressed(None);
            self.pressed = Some(board);
            self.pressed_key = Some(key);
            self.mark_key(board, key);
            return None;
        }
        // Outside the layer holding focus: a menu or popover closes, and
        // nothing beneath is pressed (#10076).
        if let Some(outcome) = self.press_outside(self.pointer) {
            self.set_pressed(None);
            return outcome;
        }
        // Selectable text under the pointer is selected, or a selection
        // cleared (#10080).
        let under = self.hit_test(self.pointer).filter(|&id| {
            self.get(id).is_some_and(|node| {
                node.style.text.selectable
                    && matches!(node.kind, NodeKind::Text(_) | NodeKind::Slot { .. })
            })
        });
        self.select_text(under);
        // On a rich text, only its links can be pressed (#10078).
        let target =
            self.control_at(self.pointer)
                .filter(|&id| match self.get(id).map(|node| &node.kind) {
                    Some(NodeKind::Rich { .. }) => match self.link_at(id, self.pointer) {
                        Some(link) => {
                            self.set_link(id, link);
                            true
                        }
                        None => false,
                    },
                    _ => true,
                });
        self.set_pressed(target);
        if let Some(id) = target {
            self.place_caret(id, true);
        }
        match target {
            Some(id) if self.focus != Some(id) => {
                self.move_focus(Some(id));
                Some(UiEvent::FocusChanged(Some(id)))
            }
            // Text selected takes focus from an input, so Ctrl+C copies it.
            None if under.is_some() && self.focus.is_some() => {
                self.move_focus(None);
                Some(UiEvent::FocusChanged(None))
            }
            _ => None,
        }
    }

    /// Puts text input `id`'s caret where the pointer is: at a press,
    /// `pressed`, selecting from where it was if Shift is held and the
    /// word there at a second press in the same place; as the pointer
    /// moves with its button down, the selection follows it.
    fn place_caret(&mut self, id: NodeId, pressed: bool) {
        let Some(at) = self.caret_at_pointer(id) else {
            return;
        };
        let again = pressed && self.last_click == Some((id, at));
        let shift = self.shift();
        let Ok(node) = self.node_mut(id) else {
            return;
        };
        let rect = node.rect;
        let NodeKind::TextInput {
            value,
            caret,
            anchor,
            ..
        } = &mut node.kind
        else {
            return;
        };
        if again {
            let (start, end) = crate::edit::word_at(value.as_str(), usize::from(at));
            *anchor = u8::try_from(start).unwrap_or(0);
            *caret = u8::try_from(end).unwrap_or(at);
        } else {
            *caret = at;
            if pressed && !shift {
                *anchor = at;
            }
        }
        if pressed {
            self.last_click = (!again).then_some((id, at));
        }
        self.dirty.add(rect);
    }

    /// The caret offset in text input `id` nearest the pointer, as the
    /// input is drawn ([`crate::edit::window`]).
    fn caret_at_pointer(&self, id: NodeId) -> Option<u8> {
        let node = self.get(id)?;
        let NodeKind::TextInput { value, caret, .. } = &node.kind else {
            return None;
        };
        let metrics = self.metrics?.for_style(node.style.text);
        let content = node.content();
        let text = value.as_str();
        let to_caret = metrics.text_width(text.get(..usize::from(*caret)).unwrap_or(text));
        let shift = crate::edit::window(
            to_caret,
            content.size.width,
            self.scale.px(crate::edit::CARET_ROOM),
        );
        let x = i64::from(self.pointer.x)
            .saturating_sub(content.left())
            .saturating_add(i64::from(shift));
        let best = text
            .char_indices()
            .map(|(index, _)| index)
            .chain(core::iter::once(text.len()))
            .min_by_key(|&index| {
                let width = i64::from(metrics.text_width(text.get(..index).unwrap_or("")));
                width.saturating_sub(x).unsigned_abs()
            })?;
        u8::try_from(best).ok()
    }

    fn release(&mut self) -> Option<UiEvent> {
        let pressed = self.pressed;
        let key = self.pressed_key;
        self.set_pressed(None);
        let pressed = pressed?;
        if let Some(key) = key {
            // Released over the key it pressed: typed.
            return (self.key_under_pointer() == Some((pressed, key)))
                .then(|| self.type_on_screen(pressed, usize::from(key)))
                .flatten();
        }
        if self.control_at(self.pointer) != Some(pressed) {
            return None; // dragged off: no activation
        }
        let node = self.get(pressed)?;
        match node.kind {
            NodeKind::Button { action, .. } => {
                self.chosen_in_menu(pressed);
                Some(UiEvent::Action(action))
            }
            // Released over the link it pressed.
            NodeKind::Rich { runs, link, .. } => (self.link_at(pressed, self.pointer)
                == Some(link))
            .then(|| crate::rich::link_action(runs, link))
            .flatten()
            .map(UiEvent::Action),
            NodeKind::Checkbox { .. } => self.toggle(pressed),
            NodeKind::RadioGroup { .. } => {
                let option = node.option_at(self.pointer)?;
                self.choose(pressed, option)
            }
            NodeKind::ListPicker { .. } => {
                let option = node.list_option_at(self.pointer)?;
                self.choose_listed(pressed, option)
            }
            _ => None,
        }
    }

    fn key_down(&mut self, key: KeyCode, typed: Typed) -> Option<UiEvent> {
        // Ctrl and plus, minus or zero: zoom, wherever focus is (#10083).
        if self.ctrl()
            && let Some(zoom) = Zoom::of_key(key)
        {
            return Some(UiEvent::Zoom(zoom));
        }
        // Escape in a layer holding focus: a menu or popover closes, a
        // dialog asks the application to close it (#10076).
        if key == KeyCode::ESCAPE
            && let Some(top) = self.trapping()
        {
            return self.dismiss(top);
        }
        // In a focused radio group, the arrow keys choose: it is one tab
        // stop, so they do not move focus there.
        if let Some(focused) = self.focus
            && matches!(
                key,
                KeyCode::UP | KeyCode::DOWN | KeyCode::LEFT | KeyCode::RIGHT
            )
            && matches!(
                self.get(focused).map(|node| &node.kind),
                Some(NodeKind::RadioGroup { .. })
            )
        {
            return self.step_choice(focused, matches!(key, KeyCode::DOWN | KeyCode::RIGHT));
        }
        // In a focused list picker, Up and Down move its highlight
        // and Enter or Space chooses it: one tab stop too.
        if let Some(focused) = self.focus
            && let Some(NodeKind::ListPicker { cursor, .. }) =
                self.get(focused).map(|node| &node.kind)
        {
            let cursor = usize::from(*cursor);
            match key {
                KeyCode::UP | KeyCode::DOWN => {
                    self.move_highlight(focused, key == KeyCode::DOWN);
                    return None;
                }
                KeyCode::ENTER | KeyCode::SPACE => return self.choose_listed(focused, cursor),
                _ => {}
            }
        }
        // In a focused text area, the arrows, Home, End and Enter edit:
        // Up and Down move between its lines, not between controls.
        if let Some(focused) = self.focus
            && matches!(
                self.get(focused).map(|node| &node.kind),
                Some(NodeKind::TextArea { .. })
            )
            && key != KeyCode::TAB
            && !matches!(key, KeyCode::PAGE_UP | KeyCode::PAGE_DOWN)
        {
            let shift = self.shift();
            return self.type_area(focused, key, shift, typed);
        }
        // Page Up and Page Down scroll the scroll view a page, less a line.
        if matches!(key, KeyCode::PAGE_UP | KeyCode::PAGE_DOWN) {
            self.scroll_page(key == KeyCode::PAGE_DOWN);
            return None;
        }
        // In a focused rich text, Tab and Shift-Tab go through its links
        // before leaving it (#10078).
        if key == KeyCode::TAB
            && let Some(focused) = self.focus
            && let Some(NodeKind::Rich { links, link, .. }) = self.get(focused).map(|n| &n.kind)
        {
            let (links, link) = (*links, *link);
            let next = if self.shift() {
                link.checked_sub(1)
            } else {
                link.checked_add(1).filter(|&next| next < links)
            };
            if let Some(next) = next {
                self.set_link(focused, next);
                return None;
            }
        }
        match key {
            KeyCode::TAB if self.shift() => {
                let event = self.focus_event(Self::focus_prev);
                self.enter_links(false);
                return event;
            }
            KeyCode::TAB | KeyCode::DOWN => {
                let event = self.focus_event(Self::focus_next);
                self.enter_links(true);
                return event;
            }
            KeyCode::UP => {
                let event = self.focus_event(Self::focus_prev);
                self.enter_links(false);
                return event;
            }
            _ => {}
        }
        // Ctrl+C with selectable text selected copies it (#10080), unless
        // an input holds the keys.
        if self.ctrl()
            && KeyCode::letter('c') == Some(key)
            && let Some(selected) = self.selected
            && self
                .focus
                .and_then(|id| self.input_mode(Some(id)))
                .is_none()
        {
            self.copy_text(selected);
            return None;
        }
        let focused = self.focus?;
        let shift = self.shift();
        match &self.get(focused)?.kind {
            NodeKind::Button { action, .. } => {
                let action = *action;
                let chosen = matches!(key, KeyCode::ENTER | KeyCode::SPACE);
                if chosen {
                    self.chosen_in_menu(focused);
                }
                chosen.then_some(UiEvent::Action(action))
            }
            NodeKind::Checkbox { .. } => {
                if key == KeyCode::SPACE {
                    self.toggle(focused)
                } else {
                    None
                }
            }
            NodeKind::Rich { runs, link, .. } => matches!(key, KeyCode::ENTER | KeyCode::SPACE)
                .then(|| crate::rich::link_action(runs, *link))
                .flatten()
                .map(UiEvent::Action),
            NodeKind::RadioGroup { selected, .. } => {
                if key == KeyCode::SPACE && selected.is_none() {
                    self.choose(focused, 0)
                } else {
                    None
                }
            }
            NodeKind::TextInput { .. } => self.type_key(focused, key, shift, typed),
            _ => None,
        }
    }

    /// Types `key`, Shift held if `shift`, into text input `id`: from a
    /// keyboard, or a key of the on-screen keyboard (#10075). Left and
    /// Right move the caret a character - a word with Ctrl - and Home and
    /// End to an end; with Shift they select. Ctrl and A select it all. A
    /// character replaces what is selected or goes in at the caret;
    /// Backspace and Delete remove what is selected, or the character
    /// before or after the caret. Enter moves to the next control.
    fn type_key(&mut self, id: NodeId, key: KeyCode, shift: bool, typed: Typed) -> Option<UiEvent> {
        use crate::edit::{Step, selection, step};
        use nowayos_bounded::Wipe;
        if key == KeyCode::ENTER {
            return self.focus_event(Self::focus_next);
        }
        let ctrl = self.ctrl();
        // Copy, cut and paste (#10080).
        if ctrl && is_clip_key(key) {
            return self.clip_input(id, key);
        }
        let node = self.node_mut(id).ok()?;
        let rect = node.rect;
        let NodeKind::TextInput {
            value,
            mode,
            caret,
            anchor,
            ..
        } = &mut node.kind
        else {
            return None;
        };
        let text = value.as_str();
        let (at, from) = (usize::from(*caret), usize::from(*anchor));
        let (low, high) = selection(text, at, from);
        let to_u8 = |offset: usize| u8::try_from(offset).unwrap_or(u8::MAX);
        let moving = match key {
            KeyCode::LEFT if ctrl => Some(Step::WordLeft),
            KeyCode::LEFT => Some(Step::Left),
            KeyCode::RIGHT if ctrl => Some(Step::WordRight),
            KeyCode::RIGHT => Some(Step::Right),
            KeyCode::HOME => Some(Step::Home),
            KeyCode::END => Some(Step::End),
            _ => None,
        };
        if let Some(movement) = moving {
            // Without Shift, Left and Right first fold a selection to its
            // near end.
            let moved = match (shift, movement) {
                (false, Step::Left) if low != high => low,
                (false, Step::Right) if low != high => high,
                _ => step(text, at, movement),
            };
            *caret = to_u8(moved);
            if !shift {
                *anchor = *caret;
            }
            self.dirty.add(rect);
            return None;
        }
        if ctrl && Some(key) == KeyCode::letter('a') {
            (*anchor, *caret) = (0, to_u8(text.len()));
            self.dirty.add(rect);
            return None;
        }
        let mut bytes = [0_u8; TYPED_BYTES];
        let (range, insert) = match key {
            KeyCode::BACKSPACE | KeyCode::DELETE if low != high => ((low, high), ""),
            KeyCode::BACKSPACE => ((step(text, at, Step::Left), at), ""),
            KeyCode::DELETE => ((at, step(text, at, Step::Right)), ""),
            _ => {
                // A key types only characters the glyph atlases draw, as
                // text set any other way must be (the layouts type no
                // other), and only ones the input's mode takes: a key that
                // types none is refused, reporting nothing.
                let len = encode_typed(typed, |ch| mode.accepts(ch), &mut bytes);
                if len == 0 {
                    return None;
                }
                ((low, high), core::str::from_utf8(bytes.get(..len)?).ok()?)
            }
        };
        if range.0 == range.1 && insert.is_empty() {
            return None;
        }
        let event = self.put_input(id, range, insert);
        // What was typed leaves no copy behind.
        bytes.wipe();
        event
    }

    /// Replaces bytes `range` of text input `id`'s value with `insert`,
    /// the caret after it: `ValueChanged`, or `InputFull` if it would not
    /// fit.
    fn put_input(&mut self, id: NodeId, range: (usize, usize), insert: &str) -> Option<UiEvent> {
        use crate::edit::replaced;
        use nowayos_bounded::Wipe;
        let node = self.node_mut(id).ok()?;
        let rect = node.rect;
        let NodeKind::TextInput {
            field,
            value,
            max_len,
            caret,
            anchor,
            ..
        } = &mut node.kind
        else {
            return None;
        };
        let field = *field;
        let text = value.as_str();
        let to_u8 = |offset: usize| u8::try_from(offset).unwrap_or(u8::MAX);
        let mut made = [0_u8; crate::INPUT_CAPACITY];
        let event = match replaced(text, range, insert, usize::from(*max_len), &mut made) {
            Some(len) => {
                let ok = made
                    .get(..len)
                    .and_then(|bytes| core::str::from_utf8(bytes).ok())
                    .is_some_and(|new| {
                        value.clear();
                        value.push_str(new).is_ok()
                    });
                let end = to_u8(range.0.saturating_add(insert.len()));
                (*caret, *anchor) = (end, end);
                ok.then_some(UiEvent::ValueChanged(field))
            }
            None => Some(UiEvent::InputFull(field)),
        };
        // What was typed leaves no copy behind.
        made.wipe();
        if matches!(event, Some(UiEvent::ValueChanged(_))) {
            self.dirty.add(rect);
        }
        event
    }

    /// Ctrl+C, Ctrl+X or Ctrl+V in text input `id` (#10080): the
    /// selection copied, or cut, or the clipboard pasted over it - if it
    /// fits and the input takes every character.
    fn clip_input(&mut self, id: NodeId, key: KeyCode) -> Option<UiEvent> {
        use nowayos_bounded::Wipe;
        let NodeKind::TextInput {
            value,
            mode,
            caret,
            anchor,
            ..
        } = &self.nodes.get(id.index())?.kind
        else {
            return None;
        };
        let (low, high) =
            crate::edit::selection(value.as_str(), usize::from(*caret), usize::from(*anchor));
        let mode = *mode;
        if KeyCode::letter('v') != Some(key) {
            if low == high {
                return None;
            }
            if let Some(part) = value.as_str().get(low..high) {
                self.clipboard.set(part);
            }
            return if KeyCode::letter('x') == Some(key) {
                self.put_input(id, (low, high), "")
            } else {
                None
            };
        }
        // Pasted: one line, every character one the input takes.
        let mut paste = [0_u8; crate::clipboard::CLIPBOARD];
        let clip = self.clipboard.text();
        let len = clip.len();
        if let (Some(into), Some(from)) = (paste.get_mut(..len), clip.as_bytes().get(..len)) {
            into.copy_from_slice(from);
        }
        let event = paste
            .get(..len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .filter(|text| !text.is_empty() && text.chars().all(|ch| mode.accepts(ch)))
            .and_then(|text| self.put_input(id, (low, high), text));
        paste.wipe();
        event
    }

    /// A key of the on-screen keyboard typed into `target`: a text input or
    /// a text area.
    /// Its keys are the US layout's, as their labels show.
    fn type_into(&mut self, target: NodeId, key: KeyCode, shift: bool) -> Option<UiEvent> {
        let typed = Typed::one(keymap::us_char(key, shift));
        if matches!(
            self.get(target).map(|node| &node.kind),
            Some(NodeKind::TextArea { .. })
        ) {
            self.type_area(target, key, shift, typed)
        } else {
            self.type_key(target, key, shift, typed)
        }
    }

    /// Sets the keyboard layout keys type on (#10081): US until a product
    /// names another. A dead key waiting is dropped.
    pub fn set_keyboard_layout(&mut self, layout: Layout) {
        self.keymap.set_layout(layout);
    }

    /// The keyboard layout keys type on.
    #[must_use]
    pub const fn keyboard_layout(&self) -> Layout {
        self.keymap.layout()
    }

    /// The on-screen keyboard's key under the pointer, if a text input has
    /// focus for it to type into.
    pub(crate) fn key_under_pointer(&self) -> Option<(NodeId, u8)> {
        self.typing_target()?;
        let board = self.hit_test(self.pointer)?;
        let key = self.get(board)?.key_at(self.pointer)?;
        Some((board, u8::try_from(key).ok()?))
    }

    /// Key `index` of keyboard `board`, released over it: Shift
    /// turns its Shift on or off; any other key sends its key event to the
    /// focused text input - a letter in capitals while Shift is on, which
    /// then goes off.
    fn type_on_screen(&mut self, board: NodeId, index: usize) -> Option<UiEvent> {
        let target = self.typing_target()?;
        let NodeKind::Keyboard { mode, shift } = self.get(board)?.kind else {
            return None;
        };
        match crate::keyboard::layout(mode).key(index)?.action {
            KeyAction::Shift => {
                self.set_keyboard_shift(board, !shift);
                None
            }
            KeyAction::Letter(key) => {
                if shift {
                    self.set_keyboard_shift(board, false);
                }
                self.type_into(target, key, shift)
            }
            KeyAction::Type { key, shift } => self.type_into(target, key, shift),
        }
    }

    /// Turns keyboard `board`'s Shift on or off, repainting it: its letters
    /// change case.
    fn set_keyboard_shift(&mut self, board: NodeId, on: bool) {
        if let Ok(node) = self.node_mut(board)
            && let NodeKind::Keyboard { shift, .. } = &mut node.kind
        {
            *shift = on;
            self.mark_dirty(board);
        }
    }

    /// The mode of `id`, if it is a text input.
    fn input_mode(&self, id: Option<NodeId>) -> Option<InputMode> {
        match self.get(id?)?.kind {
            NodeKind::TextInput { mode, .. } => Some(mode),
            NodeKind::TextArea { .. } => Some(InputMode::Text),
            _ => None,
        }
    }

    /// Keeps the on-screen keyboard on the layout of the text input focus
    /// moved to, from `before`, and repaints it when its keys
    /// change: to another layout, or between typing and not as a text
    /// input gains or loses focus.
    pub(crate) fn keyboard_follows(&mut self, before: Option<NodeId>) {
        let Some(board) = self.keyboard_node() else {
            return;
        };
        let now = self.input_mode(self.focus);
        if self.input_mode(before) == now {
            return;
        }
        if let Some(now) = now
            && let Ok(node) = self.node_mut(board)
            && let NodeKind::Keyboard { mode, .. } = &mut node.kind
        {
            *mode = now;
        }
        self.mark_dirty(board);
    }

    /// Marks key `key` of keyboard `board` for repainting.
    fn mark_key(&mut self, board: NodeId, key: u8) {
        if let Some(rect) = self
            .get(board)
            .and_then(|node| node.key_rect(usize::from(key)))
        {
            self.dirty.add(rect);
        }
    }

    /// Toggles checkbox `id`, marks it for repainting and reports its new
    /// value.
    fn toggle(&mut self, id: NodeId) -> Option<UiEvent> {
        let node = self.node_mut(id).ok()?;
        let NodeKind::Checkbox { field, checked, .. } = &mut node.kind else {
            return None;
        };
        *checked = !*checked;
        let event = UiEvent::Toggled {
            field: *field,
            checked: *checked,
        };
        let rect = node.rect;
        self.dirty.add(rect);
        Some(event)
    }

    /// Chooses `option` in radio group `id`, clearing any other choice;
    /// reports it, and marks the group for repainting, if that changes it.
    fn choose(&mut self, id: NodeId, option: usize) -> Option<UiEvent> {
        let node = self.node_mut(id).ok()?;
        let NodeKind::RadioGroup {
            field,
            options,
            selected,
        } = &mut node.kind
        else {
            return None;
        };
        if option >= options.len() {
            return None;
        }
        let option = u8::try_from(option).ok()?;
        if *selected == Some(option) {
            return None;
        }
        *selected = Some(option);
        let event = UiEvent::Selected {
            field: *field,
            option,
        };
        let rect = node.rect;
        self.dirty.add(rect);
        Some(event)
    }

    /// Chooses the next option in radio group `id` (or the previous one),
    /// wrapping round; with nothing chosen, the first (or the last).
    /// Chooses `option` in list picker `id`, highlighting it too;
    /// an event if the choice changed.
    fn choose_listed(&mut self, id: NodeId, option: usize) -> Option<UiEvent> {
        let node = self.node_mut(id).ok()?;
        let NodeKind::ListPicker {
            field,
            options,
            selected,
            rows,
            top,
            cursor,
            ..
        } = &mut node.kind
        else {
            return None;
        };
        if option >= options.len() {
            return None;
        }
        let option = u8::try_from(option).ok()?;
        *cursor = option;
        *top = crate::tree::shown_top(*top, option, *rows, options.len());
        let changed = *selected != Some(option);
        *selected = Some(option);
        let field = *field;
        self.mark_dirty(id);
        changed.then_some(UiEvent::Selected { field, option })
    }

    /// Moves list picker `id`'s highlight one option down or up, scrolling
    /// the list to keep it in view; it stops at either end.
    fn move_highlight(&mut self, id: NodeId, down: bool) {
        let Ok(node) = self.node_mut(id) else {
            return;
        };
        let NodeKind::ListPicker {
            options,
            rows,
            top,
            cursor,
            ..
        } = &mut node.kind
        else {
            return;
        };
        let last = u8::try_from(options.len().saturating_sub(1)).unwrap_or(u8::MAX);
        *cursor = if down {
            cursor.saturating_add(1).min(last)
        } else {
            cursor.saturating_sub(1)
        };
        *top = crate::tree::shown_top(*top, *cursor, *rows, options.len());
        self.mark_dirty(id);
    }

    fn step_choice(&mut self, id: NodeId, forward: bool) -> Option<UiEvent> {
        let NodeKind::RadioGroup {
            options, selected, ..
        } = &self.get(id)?.kind
        else {
            return None;
        };
        let count = options.len();
        let last = count.checked_sub(1)?;
        let next = match (selected.map(usize::from), forward) {
            (None, true) => 0,
            (None, false) => last,
            (Some(current), true) if current >= last => 0,
            (Some(current), true) => current.saturating_add(1),
            (Some(0), false) => last,
            (Some(current), false) => current.saturating_sub(1).min(last),
        };
        self.choose(id, next)
    }

    /// Runs a focus move and reports it if focus actually changed.
    fn focus_event(&mut self, step: fn(&mut Self) -> Option<NodeId>) -> Option<UiEvent> {
        let before = self.focus;
        let after = step(self);
        (after != before).then_some(UiEvent::FocusChanged(after))
    }

    /// Puts focus within rich text `id` on its link `link`, and marks it.
    fn set_link(&mut self, id: NodeId, to: u8) {
        if let Ok(node) = self.node_mut(id)
            && let NodeKind::Rich { link, .. } = &mut node.kind
        {
            *link = to;
            let rect = node.rect;
            self.dirty.add(rect);
        }
    }

    /// After focus moved onto a rich text: its first link, going forward,
    /// or its last, going back.
    fn enter_links(&mut self, forward: bool) {
        if let Some(focused) = self.focus
            && let Some(NodeKind::Rich { links, .. }) = self.get(focused).map(|n| &n.kind)
        {
            let to = if forward { 0 } else { links.saturating_sub(1) };
            self.set_link(focused, to);
        }
    }

    /// The link of rich text `id` at `point`, if any.
    pub(crate) fn link_at(&self, id: NodeId, point: Point) -> Option<u8> {
        let metrics = self.metrics?;
        let node = self.get(id)?;
        let NodeKind::Rich { runs, .. } = node.kind else {
            return None;
        };
        let style = node.style.text;
        let metrics = metrics.for_style(style);
        let content = node.content();
        let lines = crate::rich::lines(runs, content.size.width, style, &metrics);
        for (index, line) in lines.enumerate() {
            let top = content.top().saturating_add(
                i64::from(metrics.line_height()).saturating_mul(i64::try_from(index).ok()?),
            );
            let bottom = top.saturating_add(i64::from(metrics.line_height()));
            if !(top..bottom).contains(&i64::from(point.y)) {
                continue;
            }
            return crate::rich::pieces(runs, line, style, &metrics)
                .find(|piece| {
                    let left = content.left().saturating_add(i64::from(piece.x));
                    (left..left.saturating_add(i64::from(piece.width)))
                        .contains(&i64::from(point.x))
                })
                .and_then(|piece| crate::rich::link_of(runs, piece.run));
        }
        None
    }

    /// Focusable nodes in tree order.
    pub(crate) fn focus_order(&self) -> impl Iterator<Item = NodeId> + '_ {
        // While a layer holds focus, only its controls (#10076).
        let plane = self
            .trapping()
            .and_then(|top| self.shown_layers().position(|id| id == top))
            .map_or(0, |at| at.saturating_add(1));
        self.plane_order(plane)
            .filter(|(_, node)| node.is_focusable())
            .map(|(id, _)| id)
    }

    pub(crate) fn move_focus(&mut self, to: Option<NodeId>) {
        if to != self.focus {
            self.mark_optional(self.focus);
            // Onto a control the scroll view hides: scrolled into view
            // first, so it is marked where it is drawn.
            if let Some(id) = to {
                self.reveal(id);
            }
            self.mark_optional(to);
            let before = self.focus;
            self.focus = to;
            // An accent waiting for its letter is not carried to another
            // control (#10081).
            self.keymap.cancel();
            self.keyboard_follows(before);
        }
    }

    /// Scrolls the view holding focus - or, failing that, the first - a
    /// page down or up: its height less a line, at least one pixel.
    fn scroll_page(&mut self, down: bool) {
        let view = self
            .focus
            .and_then(|id| self.views_around(id).next())
            .or_else(|| self.scroll_node());
        let (Some(view), Some(metrics)) = (view, self.metrics) else {
            return;
        };
        let Some(seen) = self.scroll_port(view) else {
            return;
        };
        let page = i64::from(
            seen.size
                .height
                .saturating_sub(metrics.line_height())
                .max(1),
        );
        let by = if down { page } else { page.saturating_neg() };
        self.scroll_by(view, 0, by);
    }

    /// A mouse's wheel turned `notches` (positive away from the user): a
    /// list picker under the pointer shows earlier or later options, a row
    /// a notch; otherwise the innermost view under the pointer that can
    /// still move goes [`WHEEL_LINES`] lines a notch - across, with Shift
    /// held.
    fn wheel(&mut self, notches: i8) {
        let up = i64::from(notches);
        if let Some(id) = self.hit_test(self.pointer)
            && let Some(&NodeKind::ListPicker { top, .. }) = self.get(id).map(|node| &node.kind)
        {
            let row = self
                .get(id)
                .and_then(|node| node.list_row(1))
                .map_or(1, |row| i64::from(row.size.height.max(1)));
            self.drag_list(id, top, up.saturating_neg().saturating_mul(row));
            return;
        }
        let Some(metrics) = self.metrics else {
            return;
        };
        let lines = i64::from(metrics.line_height().saturating_mul(WHEEL_LINES));
        let by = up.saturating_neg().saturating_mul(lines);
        if self.shift_keys.contains(&true) {
            self.scroll_under_pointer(by, 0);
        } else {
            self.scroll_under_pointer(0, by);
        }
    }

    /// A touch starting where the pointer is: on a list picker it may
    /// become a drag of the list, and elsewhere a drag of the innermost
    /// scroll view under it that can scroll.
    fn start_drag(&mut self) {
        self.drag = None;
        let from = self.pointer;
        let last = (i64::from(from.x), i64::from(from.y));
        if let Some(id) = self.pressed
            && let Some(NodeKind::ListPicker { top, .. }) = self.get(id).map(|node| &node.kind)
        {
            self.drag = Some(crate::tree::Drag {
                from,
                view: None,
                offset: (0, 0),
                scrolling: false,
                list: Some((id, *top)),
                last,
                speed: (0, 0),
            });
            return;
        }
        let view = self.hit_test(from).and_then(|hit| {
            self.views_around(hit)
                .find(|&view| self.scroll_range(view).is_some_and(|range| range != (0, 0)))
        });
        let (Some(view), Some(offset)) = (view, view.and_then(|view| self.scroll_position(view)))
        else {
            return;
        };
        self.drag = Some(crate::tree::Drag {
            from,
            view: Some(view),
            offset,
            scrolling: false,
            list: None,
            last,
            speed: (0, 0),
        });
    }

    /// A touch moved to where the pointer is: once it has moved past
    /// [`DRAG_SLOP`], it cancels its press, so lifting it presses nothing,
    /// and scrolls its view with the finger, along each axis the view
    /// scrolls.
    fn drag_to(&mut self) {
        let Some(mut drag) = self.drag else {
            return;
        };
        let moved_x = i64::from(drag.from.x).saturating_sub(i64::from(self.pointer.x));
        let moved_y = i64::from(drag.from.y).saturating_sub(i64::from(self.pointer.y));
        let slop = u64::from(self.scale.px(DRAG_SLOP));
        if !drag.scrolling && moved_x.unsigned_abs().max(moved_y.unsigned_abs()) <= slop {
            return;
        }
        if !drag.scrolling {
            drag.scrolling = true;
            self.set_pressed(None);
        }
        self.drag = Some(drag);
        if let Some((id, from_top)) = drag.list {
            self.drag_list(id, from_top, moved_y);
            return;
        }
        let Some(view) = drag.view else {
            return;
        };
        let to = |offset: u32, by: i64| {
            u32::try_from(i64::from(offset).saturating_add(by).max(0)).unwrap_or(u32::MAX)
        };
        let _ = self.scroll_view_to(view, to(drag.offset.0, moved_x), to(drag.offset.1, moved_y));
    }

    /// Holds scroll view `view`'s thumb - its vertical one, or else its
    /// horizontal one - where the pointer is.
    fn hold_thumb(&mut self, view: NodeId, vertical: bool) {
        let Some((x, y)) = self.scroll_position(view) else {
            return;
        };
        let (from, offset) = if vertical {
            (i64::from(self.pointer.y), y)
        } else {
            (i64::from(self.pointer.x), x)
        };
        self.set_pressed(None);
        self.thumb = Some(crate::scroll::ThumbHold {
            view,
            vertical,
            from,
            offset,
        });
    }

    /// The held thumb follows the pointer: the view scrolls as far through
    /// its range as the thumb moves through its track.
    fn thumb_to(&mut self) {
        let Some(hold) = self.thumb else {
            return;
        };
        let bars = self.scrollbars(hold.view);
        let (Some(bar), Some((most_x, most_y)), Some((x, y))) = (
            if hold.vertical {
                bars.vertical
            } else {
                bars.horizontal
            },
            self.scroll_range(hold.view),
            self.scroll_position(hold.view),
        ) else {
            return;
        };
        let (along, free, most) = if hold.vertical {
            (
                i64::from(self.pointer.y),
                bar.track.size.height.saturating_sub(bar.thumb.size.height),
                most_y,
            )
        } else {
            (
                i64::from(self.pointer.x),
                bar.track.size.width.saturating_sub(bar.thumb.size.width),
                most_x,
            )
        };
        let moved = along
            .saturating_sub(hold.from)
            .saturating_mul(i64::from(most))
            .checked_div(i64::from(free))
            .unwrap_or(0);
        let target =
            u32::try_from(i64::from(hold.offset).saturating_add(moved).max(0)).unwrap_or(u32::MAX);
        let (to_x, to_y) = if hold.vertical {
            (x, target)
        } else {
            (target, y)
        };
        let _ = self.scroll_view_to(hold.view, to_x, to_y);
    }

    /// Scrolls list picker `id` with a finger that has moved `moved`
    /// pixels up since the list showed `from_top` first: a row's
    /// height for each option.
    fn drag_list(&mut self, id: NodeId, from_top: u8, moved: i64) {
        let Some(row) = self
            .get(id)
            .and_then(|node| node.list_row(1))
            .map(|row| i64::from(row.size.height.max(1)))
        else {
            return;
        };
        let Ok(node) = self.node_mut(id) else {
            return;
        };
        let NodeKind::ListPicker {
            options, rows, top, ..
        } = &mut node.kind
        else {
            return;
        };
        let last_top = i64::try_from(options.len().saturating_sub(usize::from(*rows))).unwrap_or(0);
        let shown = i64::from(from_top)
            .saturating_add(moved.checked_div(row).unwrap_or(0))
            .clamp(0, last_top);
        let shown = u8::try_from(shown).unwrap_or(0);
        if *top != shown {
            *top = shown;
            self.mark_dirty(id);
        }
    }

    pub(crate) fn set_pressed(&mut self, to: Option<NodeId>) {
        if to != self.pressed {
            // A key of the on-screen keyboard is repainted alone.
            match (self.pressed, self.pressed_key.take()) {
                (Some(board), Some(key)) => self.mark_key(board, key),
                (pressed, _) => self.mark_optional(pressed),
            }
            self.mark_optional(to);
            self.pressed = to;
        }
    }

    fn mark_optional(&mut self, id: Option<NodeId>) {
        if let Some(id) = id {
            self.mark_dirty(id);
        }
    }
}

/// Whether `key` is C, X or V: with Ctrl, copy, cut or paste (#10080).
/// The most bytes one key types: two characters, a dead key's and the
/// letter it did not go on.
pub(crate) const TYPED_BYTES: usize = 8;

/// The characters `typed` that `accepts` takes, encoded into `into`: how
/// many bytes.
pub(crate) fn encode_typed(
    typed: Typed,
    accepts: impl Fn(char) -> bool,
    into: &mut [u8; TYPED_BYTES],
) -> usize {
    let mut len = 0_usize;
    for ch in typed.chars().filter(|&ch| accepts(ch)) {
        let end = len.saturating_add(ch.len_utf8());
        if let Some(slot) = into.get_mut(len..end) {
            ch.encode_utf8(slot);
            len = end;
        }
    }
    len
}

pub(crate) fn is_clip_key(key: KeyCode) -> bool {
    ['c', 'x', 'v']
        .iter()
        .any(|&letter| KeyCode::letter(letter) == Some(key))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::{Align, Style, TextMetrics};
    use nowayos_display::Rect;

    const OK: ActionId = ActionId(1);
    const CANCEL: ActionId = ActionId(2);
    const NAME: FieldId = FieldId(10);

    struct Screen {
        tree: Tree<16>,
        name: NodeId,
        ok: NodeId,
        cancel: NodeId,
    }

    /// Column: title, name input, row of [OK] [Cancel].
    fn screen() -> Screen {
        let mut tree: Tree<16> = Tree::new();
        let root = tree.column(Style::new().padding(4).gap(4)).unwrap();
        tree.set_root(root).unwrap();
        let title = tree.text("Title").unwrap();
        tree.append(root, title).unwrap();
        let name = tree.text_input(NAME, "Name").unwrap();
        tree.append(root, name).unwrap();
        let row = tree.row(Style::new().gap(4).align(Align::Start)).unwrap();
        tree.append(root, row).unwrap();
        let ok = tree.button("OK", OK).unwrap();
        tree.append(row, ok).unwrap();
        let cancel = tree.button("Cancel", CANCEL).unwrap();
        tree.append(row, cancel).unwrap();
        tree.layout(Rect::new(0, 0, 200, 100), &TextMetrics::new(6, 10))
            .unwrap();
        Screen {
            tree,
            name,
            ok,
            cancel,
        }
    }

    fn centre(tree: &Tree<16>, id: NodeId) -> (i32, i32) {
        let c = tree.get(id).unwrap().rect().center();
        (c.x, c.y)
    }

    fn click(tree: &mut Tree<16>, (x, y): (i32, i32)) -> [Option<UiEvent>; 2] {
        tree.handle(InputEvent::PointerMove { x, y });
        [
            tree.handle(InputEvent::PointerButton {
                button: 0,
                down: true,
            }),
            tree.handle(InputEvent::PointerButton {
                button: 0,
                down: false,
            }),
        ]
    }

    #[test]
    fn hit_test_finds_the_deepest_node() {
        let s = screen();
        let t = &s.tree;
        let (x, y) = centre(t, s.cancel);
        assert_eq!(t.hit_test(Point::new(x, y)), Some(s.cancel));
        assert_eq!(t.control_at(Point::new(x, y)), Some(s.cancel));
        // The title is not a control.
        assert_eq!(t.control_at(Point::new(6, 6)), None);
        assert_eq!(t.hit_test(Point::new(500, 500)), None);
        assert_eq!(t.hit_test(Point::new(-1, 0)), None);
    }

    #[test]
    fn click_focuses_then_activates() {
        let mut s = screen();
        let at = centre(&s.tree, s.ok);
        let events = click(&mut s.tree, at);
        assert_eq!(
            events,
            [
                Some(UiEvent::FocusChanged(Some(s.ok))),
                Some(UiEvent::Action(OK))
            ]
        );
        // Already focused: the second click only activates.
        let events = click(&mut s.tree, at);
        assert_eq!(events, [None, Some(UiEvent::Action(OK))]);
    }

    #[test]
    fn dragging_off_a_button_cancels_it() {
        let mut s = screen();
        let (x, y) = centre(&s.tree, s.ok);
        s.tree.handle(InputEvent::PointerMove { x, y });
        s.tree.handle(InputEvent::PointerButton {
            button: 0,
            down: true,
        });
        assert_eq!(s.tree.pressed(), Some(s.ok));
        let (cx, cy) = centre(&s.tree, s.cancel);
        s.tree.handle(InputEvent::PointerMove { x: cx, y: cy });
        assert_eq!(
            s.tree.handle(InputEvent::PointerButton {
                button: 0,
                down: false
            }),
            None
        );
        assert_eq!(s.tree.pressed(), None);
    }

    #[test]
    fn touch_taps_like_a_click_and_other_contacts_are_ignored() {
        let mut s = screen();
        let (x, y) = centre(&s.tree, s.cancel);
        let tap = |id, phase| InputEvent::Touch { id, x, y, phase };
        assert_eq!(s.tree.handle(tap(1, TouchPhase::Start)), None);
        s.tree.handle(tap(0, TouchPhase::Start));
        assert_eq!(
            s.tree.handle(tap(0, TouchPhase::End)),
            Some(UiEvent::Action(CANCEL))
        );
        s.tree.handle(tap(0, TouchPhase::Start));
        assert_eq!(s.tree.handle(tap(0, TouchPhase::Cancel)), None);
        assert_eq!(s.tree.handle(tap(0, TouchPhase::End)), None);
    }

    /// The focus ring only after keyboard use, and the pointer only
    /// where a mouse moved it.
    #[test]
    fn keys_show_the_ring_clicks_hide_it_and_a_mouse_shows_the_pointer() {
        let mut s = screen();
        let t = &mut s.tree;
        assert!(!t.focus_visible());
        assert_eq!(t.pointer_shown(), None);
        t.handle(InputEvent::KeyDown(KeyCode::TAB));
        assert!(t.focus_visible());
        // Moving the mouse keeps the ring; pressing hides it.
        let (x, y) = centre(t, s.ok);
        t.handle(InputEvent::PointerMove { x, y });
        assert!(t.focus_visible());
        assert_eq!(t.pointer_shown(), Some(Point::new(x, y)));
        t.handle(InputEvent::PointerButton {
            button: 0,
            down: true,
        });
        assert!(!t.focus_visible());
        t.handle(InputEvent::PointerButton {
            button: 0,
            down: false,
        });
        // A touch hides the pointer, and the ring too.
        t.handle(InputEvent::KeyDown(KeyCode::TAB));
        t.handle(InputEvent::Touch {
            id: 0,
            x,
            y,
            phase: TouchPhase::Start,
        });
        assert!(!t.focus_visible());
        assert_eq!(t.pointer_shown(), None);
        // A session's end hides the ring; a new screen keeps how the
        // person last gave input.
        t.handle(InputEvent::KeyDown(KeyCode::TAB));
        t.clear();
        assert!(t.focus_visible());
        t.wipe_session();
        assert!(!t.focus_visible());
    }

    #[test]
    fn tab_order_wraps_both_ways() {
        let mut s = screen();
        let t = &mut s.tree;
        assert_eq!(t.focus_next(), Some(s.name));
        assert_eq!(t.focus_next(), Some(s.ok));
        assert_eq!(t.focus_next(), Some(s.cancel));
        assert_eq!(t.focus_next(), Some(s.name));
        assert_eq!(t.focus_prev(), Some(s.cancel));
        t.set_focus(None).unwrap();
        assert_eq!(t.focus_prev(), Some(s.cancel));
        // Keys drive the same moves.
        t.set_focus(Some(s.name)).unwrap();
        assert_eq!(
            t.handle(InputEvent::KeyDown(KeyCode::DOWN)),
            Some(UiEvent::FocusChanged(Some(s.ok)))
        );
        t.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
        assert_eq!(
            t.handle(InputEvent::KeyDown(KeyCode::TAB)),
            Some(UiEvent::FocusChanged(Some(s.name)))
        );
        t.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
        assert_eq!(
            t.handle(InputEvent::KeyDown(KeyCode::TAB)),
            Some(UiEvent::FocusChanged(Some(s.ok)))
        );
    }

    #[test]
    fn focus_rules() {
        let mut s = screen();
        let root = s.tree.root().unwrap();
        assert_eq!(
            s.tree.set_focus(Some(root)),
            Err(UiError::NotFocusable(root))
        );
        let mut empty: Tree<4> = Tree::new();
        assert_eq!(empty.focus_next(), None);
        assert_eq!(empty.focus_prev(), None);
    }

    #[test]
    fn typing_editing_and_capacity() {
        let mut s = screen();
        s.tree.set_focus(Some(s.name)).unwrap();
        let key = |k| InputEvent::KeyDown(k);
        assert_eq!(
            s.tree.handle(key(KeyCode::letter('a').unwrap())),
            Some(UiEvent::ValueChanged(NAME))
        );
        s.tree.handle(key(KeyCode::LEFT_SHIFT));
        s.tree.handle(key(KeyCode::letter('b').unwrap()));
        s.tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
        s.tree.handle(key(KeyCode::SPACE));
        s.tree.handle(key(KeyCode::DIGIT_1));
        assert_eq!(s.tree.value(NAME), Some("aB 1"));
        assert_eq!(
            s.tree.handle(key(KeyCode::BACKSPACE)),
            Some(UiEvent::ValueChanged(NAME))
        );
        assert_eq!(s.tree.value(NAME), Some("aB "));
        // Keys that type nothing are ignored.
        assert_eq!(s.tree.handle(key(KeyCode::F1)), None);
        // Fill it up, then one more.
        for _ in 0..crate::INPUT_CAPACITY {
            s.tree.handle(key(KeyCode::letter('z').unwrap()));
        }
        assert_eq!(
            s.tree.value(NAME).map(str::len),
            Some(crate::INPUT_CAPACITY)
        );
        assert_eq!(
            s.tree.handle(key(KeyCode::letter('z').unwrap())),
            Some(UiEvent::InputFull(NAME))
        );
        // Enter moves on to the next control.
        assert_eq!(
            s.tree.handle(key(KeyCode::ENTER)),
            Some(UiEvent::FocusChanged(Some(s.ok)))
        );
        // Backspace on an empty input reports nothing.
        s.tree.set_value(NAME, "").unwrap();
        s.tree.set_focus(Some(s.name)).unwrap();
        assert_eq!(s.tree.handle(key(KeyCode::BACKSPACE)), None);
    }

    #[test]
    fn shift_stays_held_while_either_shift_key_is_down() {
        // Regression (review 2026-10-01): Shift was one bool, so releasing
        // one Shift key while the other was still held dropped it.
        let mut s = screen();
        s.tree.set_focus(Some(s.name)).unwrap();
        let a = InputEvent::KeyDown(KeyCode::letter('a').unwrap());
        s.tree.handle(InputEvent::KeyDown(KeyCode::LEFT_SHIFT));
        s.tree.handle(InputEvent::KeyDown(KeyCode::RIGHT_SHIFT));
        s.tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
        s.tree.handle(a); // right Shift still held
        s.tree.handle(InputEvent::KeyUp(KeyCode::RIGHT_SHIFT));
        s.tree.handle(a);
        // A key-up for a Shift key that was never pressed changes nothing.
        s.tree.handle(InputEvent::KeyDown(KeyCode::RIGHT_SHIFT));
        s.tree.handle(InputEvent::KeyUp(KeyCode::LEFT_SHIFT));
        s.tree.handle(a);
        assert_eq!(s.tree.value(NAME), Some("AaA"));
    }

    #[test]
    fn enter_and_space_activate_focused_buttons() {
        let mut s = screen();
        s.tree.set_focus(Some(s.cancel)).unwrap();
        assert_eq!(
            s.tree.handle(InputEvent::KeyDown(KeyCode::SPACE)),
            Some(UiEvent::Action(CANCEL))
        );
        assert_eq!(
            s.tree.handle(InputEvent::KeyDown(KeyCode::ENTER)),
            Some(UiEvent::Action(CANCEL))
        );
        assert_eq!(s.tree.handle(InputEvent::KeyDown(KeyCode::A)), None);
        // With nothing focused, keys do nothing.
        s.tree.set_focus(None).unwrap();
        assert_eq!(s.tree.handle(InputEvent::KeyDown(KeyCode::ENTER)), None);
    }

    const CODE: FieldId = FieldId(11);

    /// A step with two fields, each with its error line, then OK.
    fn step() -> (Tree<16>, [NodeId; 5]) {
        let mut tree: Tree<16> = Tree::new();
        let root = tree.column(Style::new().padding(4).gap(2)).unwrap();
        tree.set_root(root).unwrap();
        let name = tree.text_input(NAME, "Name").unwrap();
        let name_error = tree.field_error(NAME).unwrap();
        let code = tree.text_input(CODE, "Code").unwrap();
        let code_error = tree.field_error(CODE).unwrap();
        let ok = tree.button("OK", OK).unwrap();
        for id in [name, name_error, code, code_error, ok] {
            tree.append(root, id).unwrap();
        }
        tree.layout(Rect::new(0, 0, 200, 120), &TextMetrics::new(6, 10))
            .unwrap();
        tree.clear_dirty();
        (tree, [name, name_error, code, code_error, ok])
    }

    #[test]
    fn a_field_error_shows_and_goes_repainting_only_its_line() {
        let (mut tree, [_, name_error, _, code_error, _]) = step();
        // An empty error line still takes one line, so nothing moves.
        assert_eq!(tree.get(name_error).unwrap().rect().size.height, 10);
        tree.set_field_error(NAME, "Enter your name").unwrap();
        assert_eq!(tree.field_error_message(NAME), Some("Enter your name"));
        assert_eq!(tree.field_error_message(CODE), None);
        let line = tree.get(name_error).unwrap().rect();
        assert_eq!(tree.dirty_rects(), &[line]);
        tree.clear_dirty();
        // The same error again changes nothing.
        tree.set_field_error(NAME, "Enter your name").unwrap();
        assert!(tree.dirty_rects().is_empty());
        tree.clear_field_error(NAME).unwrap();
        assert_eq!(tree.field_error_message(NAME), None);
        assert_eq!(tree.dirty_rects(), &[line]);
        tree.clear_dirty();
        tree.clear_field_error(NAME).unwrap();
        assert!(tree.dirty_rects().is_empty(), "nothing showed");
        // Refusals change nothing.
        assert_eq!(
            tree.set_field_error(FieldId(99), "x"),
            Err(UiError::UnknownField(FieldId(99)))
        );
        assert_eq!(
            tree.set_field_error(CODE, "two\nlines"),
            Err(UiError::ControlCharacter)
        );
        assert_eq!(tree.field_error_message(CODE), None);
        // Clearing them all repaints the lines that showed one.
        tree.set_field_error(CODE, "Use digits only").unwrap();
        tree.clear_dirty();
        tree.clear_field_errors();
        assert_eq!(tree.dirty_rects(), &[tree.get(code_error).unwrap().rect()]);
        assert_eq!(tree.field_error_message(CODE), None);
    }

    #[test]
    fn focus_moves_to_the_first_field_in_error() {
        let (mut tree, [name, _, code, _, ok]) = step();
        assert_eq!(tree.focus_first_error(), None, "no errors: focus stays");
        tree.set_focus(Some(ok)).unwrap();
        tree.set_field_error(CODE, "Use digits only").unwrap();
        assert_eq!(tree.focus_first_error(), Some(code));
        assert_eq!(tree.focused(), Some(code));
        // With several, the first in focus order wins, wherever focus is.
        tree.set_field_error(NAME, "Enter your name").unwrap();
        tree.set_focus(Some(ok)).unwrap();
        assert_eq!(tree.focus_first_error(), Some(name));
        tree.clear_field_errors();
        assert_eq!(tree.focus_first_error(), None);
        assert_eq!(tree.focused(), Some(name));
    }

    const CONSENT: FieldId = FieldId(20);
    const CONTACT: FieldId = FieldId(21);
    const WAYS: &[&str] = &["Email", "Phone", "Post"];

    struct Choices {
        tree: Tree<16>,
        consent: NodeId,
        contact: NodeId,
        ok: NodeId,
    }

    /// A checkbox, a radio group of three, then OK.
    fn choices(metrics: TextMetrics) -> Choices {
        let mut tree: Tree<16> = Tree::new();
        let root = tree.column(Style::new().padding(4).gap(2)).unwrap();
        tree.set_root(root).unwrap();
        let consent = tree.checkbox(CONSENT, "I agree").unwrap();
        let contact = tree.radio_group(CONTACT, WAYS).unwrap();
        let ok = tree.button("OK", OK).unwrap();
        for id in [consent, contact, ok] {
            tree.append(root, id).unwrap();
        }
        tree.layout(Rect::new(0, 0, 200, 160), &metrics).unwrap();
        Choices {
            tree,
            consent,
            contact,
            ok,
        }
    }

    fn tap(tree: &mut Tree<16>, point: Point) -> [Option<UiEvent>; 2] {
        let touch = |phase| InputEvent::Touch {
            id: 0,
            x: point.x,
            y: point.y,
            phase,
        };
        [
            tree.handle(touch(TouchPhase::Start)),
            tree.handle(touch(TouchPhase::End)),
        ]
    }

    #[test]
    fn a_checkbox_toggles_by_its_box_its_label_or_space() {
        let mut c = choices(TextMetrics::new(6, 10));
        let t = &mut c.tree;
        assert_eq!(t.checked(CONSENT), Some(false));
        let rect = t.get(c.consent).unwrap().rect();
        // The box, at the left.
        let on_box = Point::new(rect.origin.x + 8, rect.center().y);
        assert_eq!(
            tap(t, on_box),
            [
                Some(UiEvent::FocusChanged(Some(c.consent))),
                Some(UiEvent::Toggled {
                    field: CONSENT,
                    checked: true
                })
            ]
        );
        // The label, further right: the same control.
        let on_label = Point::new(rect.origin.x + 40, rect.center().y);
        assert_eq!(
            click(t, (on_label.x, on_label.y))[1],
            Some(UiEvent::Toggled {
                field: CONSENT,
                checked: false
            })
        );
        // Space while focused; Enter does nothing.
        assert_eq!(
            t.handle(InputEvent::KeyDown(KeyCode::SPACE)),
            Some(UiEvent::Toggled {
                field: CONSENT,
                checked: true
            })
        );
        assert_eq!(t.handle(InputEvent::KeyDown(KeyCode::ENTER)), None);
        assert_eq!(t.checked(CONSENT), Some(true));
        // Only the checkbox repaints.
        t.clear_dirty();
        t.handle(InputEvent::KeyDown(KeyCode::SPACE));
        assert_eq!(t.dirty_rects(), &[rect]);
        // Dragged off before release: no toggle.
        t.handle(InputEvent::PointerMove {
            x: on_box.x,
            y: on_box.y,
        });
        t.handle(InputEvent::PointerButton {
            button: 0,
            down: true,
        });
        let ok = t.get(c.ok).unwrap().rect().center();
        t.handle(InputEvent::PointerMove { x: ok.x, y: ok.y });
        assert_eq!(
            t.handle(InputEvent::PointerButton {
                button: 0,
                down: false
            }),
            None
        );
        assert_eq!(t.checked(CONSENT), Some(false));
    }

    #[test]
    fn a_radio_group_is_one_tab_stop_chosen_by_touch_pointer_or_arrows() {
        let mut c = choices(TextMetrics::new(6, 10));
        let t = &mut c.tree;
        assert_eq!(t.selected(CONTACT), None, "nothing chosen at first");
        // Tab order: checkbox, the group once, OK.
        assert_eq!(t.focus_next(), Some(c.consent));
        assert_eq!(t.focus_next(), Some(c.contact));
        assert_eq!(t.focus_next(), Some(c.ok));
        t.set_focus(Some(c.contact)).unwrap();
        let key = |k| InputEvent::KeyDown(k);
        let chose = |option| {
            Some(UiEvent::Selected {
                field: CONTACT,
                option,
            })
        };
        // Arrow keys choose, wrapping round, and focus stays in the group.
        assert_eq!(t.handle(key(KeyCode::DOWN)), chose(0));
        assert_eq!(t.handle(key(KeyCode::DOWN)), chose(1));
        assert_eq!(t.handle(key(KeyCode::RIGHT)), chose(2));
        assert_eq!(t.handle(key(KeyCode::DOWN)), chose(0));
        assert_eq!(t.handle(key(KeyCode::UP)), chose(2));
        assert_eq!(t.handle(key(KeyCode::LEFT)), chose(1));
        assert_eq!(t.focused(), Some(c.contact));
        assert_eq!(t.selected(CONTACT), Some(1));
        // Space chooses nothing more once something is chosen...
        assert_eq!(t.handle(key(KeyCode::SPACE)), None);
        // ...and the first option when nothing is.
        t.set_selected(CONTACT, None).unwrap();
        assert_eq!(t.handle(key(KeyCode::SPACE)), chose(0));
        t.set_selected(CONTACT, None).unwrap();
        assert_eq!(
            t.handle(key(KeyCode::UP)),
            chose(2),
            "from nothing, Up is the last"
        );
        // Touch and pointer choose the option under them; choosing the one
        // already chosen changes nothing.
        let node = t.get(c.contact).unwrap().clone();
        let post = node.option_rect(2).unwrap().center();
        assert_eq!(tap(t, node.option_rect(0).unwrap().center())[1], chose(0));
        assert_eq!(tap(t, node.option_rect(0).unwrap().center())[1], None);
        assert_eq!(click(t, (post.x, post.y))[1], chose(2));
        // Tab leaves the group.
        assert_eq!(
            t.handle(key(KeyCode::TAB)),
            Some(UiEvent::FocusChanged(Some(c.ok)))
        );
        // Only the group repaints when its choice changes.
        t.set_focus(Some(c.contact)).unwrap();
        t.clear_dirty();
        t.handle(key(KeyCode::DOWN));
        assert_eq!(t.dirty_rects(), &[node.rect()]);
    }

    #[test]
    fn more_options_than_the_bound_is_a_typed_error() {
        const NINE: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9"];
        const EIGHT: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8"];
        let mut t: Tree<8> = Tree::new();
        assert_eq!(
            t.radio_group(CONTACT, NINE),
            Err(UiError::TooManyOptions {
                max: crate::MAX_OPTIONS
            })
        );
        assert_eq!(t.radio_group(CONTACT, &[]), Err(UiError::NoOptions));
        assert_eq!(
            t.radio_group(CONTACT, &["two\nlines"]),
            Err(UiError::ControlCharacter)
        );
        let chosen_beyond = NodeKind::RadioGroup {
            field: CONTACT,
            options: WAYS,
            selected: Some(3),
        };
        assert_eq!(
            t.add(chosen_beyond, Style::new()),
            Err(UiError::NoSuchOption)
        );
        assert!(t.is_empty(), "nothing was created");
        t.radio_group(CONTACT, EIGHT).unwrap();
        assert_eq!(t.set_selected(CONTACT, Some(8)), Err(UiError::NoSuchOption));
        assert_eq!(t.selected(CONTACT), None);
        t.set_selected(CONTACT, Some(7)).unwrap();
        assert_eq!(t.selected(CONTACT), Some(7));
        assert_eq!(
            t.set_selected(CONSENT, Some(0)),
            Err(UiError::UnknownField(CONSENT))
        );
        assert_eq!(
            t.set_checked(CONTACT, true),
            Err(UiError::UnknownField(CONTACT))
        );
    }

    #[test]
    fn choice_controls_meet_the_minimum_target_size() {
        for metrics in [TextMetrics::new(6, 10), TextMetrics::new(10, 20)] {
            let c = choices(metrics);
            let checkbox = c.tree.get(c.consent).unwrap().rect();
            assert!(checkbox.size.height >= crate::MIN_TARGET, "{checkbox:?}");
            assert!(checkbox.size.width >= crate::MIN_TARGET);
            let group = c.tree.get(c.contact).unwrap();
            for index in 0..WAYS.len() {
                let row = group.option_rect(index).unwrap();
                assert!(row.size.height >= crate::MIN_TARGET, "{row:?}");
                assert!(row.size.width >= crate::MIN_TARGET);
                assert_eq!(group.option_at(row.center()), Some(index));
            }
            assert_eq!(group.option_rect(WAYS.len()), None);
        }
    }

    #[test]
    fn clearing_values_and_wiping_clear_the_choices() {
        let mut c = choices(TextMetrics::new(6, 10));
        let t = &mut c.tree;
        t.set_checked(CONSENT, true).unwrap();
        t.set_selected(CONTACT, Some(1)).unwrap();
        let mut held = [
            t.get(c.consent).unwrap().clone(),
            t.get(c.contact).unwrap().clone(),
        ];
        t.clear_dirty();
        t.clear_values();
        assert_eq!(t.checked(CONSENT), Some(false));
        assert_eq!(t.selected(CONTACT), None);
        assert_eq!(t.dirty_rects().len(), 2);
        // What `clear` does to each node before it lets it go.
        for node in &mut held {
            nowayos_bounded::Wipe::wipe(node);
        }
        assert!(matches!(
            held[0].kind(),
            NodeKind::Checkbox { checked: false, .. }
        ));
        assert!(matches!(
            held[1].kind(),
            NodeKind::RadioGroup { selected: None, .. }
        ));
    }

    #[test]
    fn interaction_marks_only_affected_areas_dirty() {
        let mut s = screen();
        s.tree.clear_dirty();
        s.tree.set_focus(Some(s.ok)).unwrap();
        let ok_rect = s.tree.get(s.ok).unwrap().rect();
        assert_eq!(s.tree.dirty_rects(), &[ok_rect]);
        s.tree.clear_dirty();
        s.tree.set_focus(Some(s.name)).unwrap();
        s.tree.handle(InputEvent::KeyDown(KeyCode::A));
        let name_rect = s.tree.get(s.name).unwrap().rect();
        assert!(s.tree.dirty_rects().contains(&ok_rect));
        assert!(s.tree.dirty_rects().contains(&name_rect));
        assert_eq!(s.tree.dirty_rects().len(), 2);
    }
}
