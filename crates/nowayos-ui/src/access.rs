// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a screen reader would be told (#10085, a spike; provisional): each
//! thing on the screen a person can perceive or use, with its role, its
//! name, its state and its value - the accessibility tree.
//!
//! It is derived from the tree whenever it is asked for
//! ([`Tree::access`]), so it is always in step with it: nothing is kept
//! twice. What a reader announces as it changes is what the tree already
//! reports - focus moving ([`UiEvent::FocusChanged`](crate::UiEvent)), a
//! value changing, a choice made - and what it marks for repainting.
//! Layout alone - a column, a row, a spacer, a group or switch - has no
//! role: what it holds is told at its own depth. A radio group's options
//! and a list picker's are told one by one, as parts of their control.
//!
//! An entry's [`Display`](core::fmt::Display) is what would be spoken,
//! values included - which is what a reader is for; its `Debug` never
//! shows a value, so one never reaches a log.

use core::fmt;

use nowayos_bounded::BoundedStr;

use crate::{LayerKind, NodeId, NodeKind, SHOWN_CAPACITY, TextRole, Tree};

/// What a thing on the screen is, as WAI-ARIA names its roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Text to read.
    Text,
    /// A title or heading.
    Heading,
    /// A button.
    Button,
    /// A link in a rich text.
    Link,
    /// A one-line text input.
    TextBox,
    /// A text area of several lines.
    TextArea,
    /// A checkbox.
    CheckBox,
    /// A radio group; its options are [`Role::Radio`].
    RadioGroup,
    /// One option of a radio group.
    Radio,
    /// A list picker; its options are [`Role::Option`].
    ListBox,
    /// One option of a list picker.
    Option,
    /// A picture, named by its text alternative.
    Image,
    /// A progress bar.
    ProgressBar,
    /// A part of the screen that scrolls.
    ScrollArea,
    /// A modal dialog.
    Dialog,
    /// A menu.
    Menu,
    /// A popover.
    Popover,
    /// A tooltip.
    Tooltip,
    /// An on-screen keyboard.
    Keyboard,
    /// A field's error: announced when it appears.
    Alert,
}

impl Role {
    /// Its WAI-ARIA name.
    #[must_use]
    pub const fn aria(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Heading => "heading",
            Self::Button => "button",
            Self::Link => "link",
            Self::TextBox | Self::TextArea => "textbox",
            Self::CheckBox => "checkbox",
            Self::RadioGroup => "radiogroup",
            Self::Radio => "radio",
            Self::ListBox => "listbox",
            Self::Option => "option",
            Self::Image => "img",
            Self::ProgressBar => "progressbar",
            Self::ScrollArea => "region",
            Self::Dialog => "dialog",
            Self::Menu => "menu",
            Self::Popover => "group",
            Self::Tooltip => "tooltip",
            Self::Keyboard => "group",
            Self::Alert => "alert",
        }
    }
}

/// A thing's name or value: words the tree holds, or words it shows made
/// from a slot's template.
#[derive(Clone)]
pub enum Words<'t> {
    /// Words the tree holds.
    Held(&'t str),
    /// Words made to be shown.
    Shown(BoundedStr<SHOWN_CAPACITY>),
}

impl Words<'_> {
    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Held(words) => words,
            Self::Shown(words) => words.as_str(),
        }
    }
}

/// A thing's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// It has keyboard focus.
    pub focused: bool,
    /// A checkbox ticked, or not; `None` for anything else.
    pub checked: Option<bool>,
    /// An option chosen.
    pub selected: bool,
    /// A field that holds what the person types; its value is entered
    /// text.
    pub editable: bool,
}

/// One thing a screen reader would be told of.
#[derive(Clone)]
pub struct Access<'t> {
    /// The node.
    pub id: NodeId,
    /// Its option or link, for a part of a control.
    pub part: Option<u8>,
    /// How many things told of hold it.
    pub depth: u8,
    /// What it is.
    pub role: Role,
    /// Its name: its words, label, placeholder or text alternative.
    pub name: Words<'t>,
    /// Its state.
    pub state: State,
    /// Its value: what an input holds, how far a progress bar is.
    pub value: Option<Words<'t>>,
}

impl fmt::Debug for Access<'_> {
    /// Everything but the value, which may be what someone typed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Access")
            .field("id", &self.id)
            .field("part", &self.part)
            .field("depth", &self.depth)
            .field("role", &self.role)
            .field("state", &self.state)
            .field("value", &self.value.is_some())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Access<'_> {
    /// What would be spoken: `button "Continue" focused`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:?}", self.role.aria(), self.name.as_str())?;
        if let Some(value) = &self.value {
            write!(f, " value {:?}", value.as_str())?;
        }
        if let Some(checked) = self.state.checked {
            f.write_str(if checked { " checked" } else { " not checked" })?;
        }
        if self.state.selected {
            f.write_str(" selected")?;
        }
        if self.state.focused {
            f.write_str(" focused")?;
        }
        Ok(())
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// The accessibility tree (#10085, provisional): each thing showing a
    /// person can perceive or use, in reading order - the screen, then
    /// each layer open over it - with how deep it is, passed to `each`.
    pub fn access(&self, mut each: impl FnMut(&Access<'_>)) {
        for (id, node) in self.paint_order() {
            let Some((role, name)) = self.role_and_name(id) else {
                continue;
            };
            let depth = self.access_depth(id);
            let focused = self.focus == Some(id);
            let mut entry = Access {
                id,
                part: None,
                depth,
                role,
                name,
                state: State {
                    focused,
                    ..State::default()
                },
                value: None,
            };
            match &node.kind {
                NodeKind::TextInput { value, .. } => {
                    entry.state.editable = true;
                    entry.value = Some(Words::Held(value.as_str()));
                }
                NodeKind::TextArea { start, len, .. } => {
                    entry.state.editable = true;
                    entry.value = Some(Words::Held(self.texts.get(*start, *len)));
                }
                NodeKind::Checkbox { checked, .. } => entry.state.checked = Some(*checked),
                NodeKind::Progress { permille, .. } => {
                    let mut value = BoundedStr::new();
                    let _ = fmt::write(&mut value, format_args!("{}%", permille / 10));
                    entry.value = Some(Words::Shown(value));
                }
                _ => {}
            }
            each(&entry);
            // A rich text's links, each by its words, the focused one so.
            if let NodeKind::Rich { runs, link, .. } = &node.kind {
                for (index, run) in runs.iter().enumerate() {
                    let Some(number) = crate::rich::link_of(runs, index) else {
                        continue;
                    };
                    each(&Access {
                        id,
                        part: Some(number),
                        depth: depth.saturating_add(1),
                        role: Role::Link,
                        name: Words::Held(run.text),
                        state: State {
                            focused: focused && *link == number,
                            ..State::default()
                        },
                        value: None,
                    });
                }
                continue;
            }
            // A radio group's or list picker's options, one by one.
            let (options, selected, role) = match &node.kind {
                NodeKind::RadioGroup {
                    options, selected, ..
                } => (*options, *selected, Role::Radio),
                NodeKind::ListPicker {
                    options, selected, ..
                } => (*options, *selected, Role::Option),
                _ => continue,
            };
            for (index, option) in (0_u8..).zip(options.iter()) {
                each(&Access {
                    id,
                    part: Some(index),
                    depth: depth.saturating_add(1),
                    role,
                    name: Words::Held(option),
                    state: State {
                        selected: selected == Some(index),
                        ..State::default()
                    },
                    value: None,
                });
            }
        }
    }

    /// A node's role and name, if it is told of at all.
    fn role_and_name(&self, id: NodeId) -> Option<(Role, Words<'_>)> {
        let node = self.get(id)?;
        let text_role = |words| {
            let role = match node.style.text.role {
                TextRole::Display | TextRole::Title | TextRole::Heading => Role::Heading,
                _ => Role::Text,
            };
            Some((role, words))
        };
        match &node.kind {
            NodeKind::Text(text) => text_role(Words::Held(text)),
            NodeKind::Slot {
                text,
                template: Some(template),
                ..
            } => text_role(Words::Shown(template.around(text))),
            NodeKind::Slot { text, .. } => text_role(Words::Held(text.as_str())),
            NodeKind::LongText { .. } => text_role(Words::Held(self.long_text_of(node)?)),
            NodeKind::Rich { runs, .. } => {
                // Its words run together; its links are told by their own.
                let mut words = BoundedStr::new();
                for run in runs.iter() {
                    if words.push_str(run.text).is_err() {
                        break;
                    }
                }
                Some((Role::Text, Words::Shown(words)))
            }
            NodeKind::Button { label, .. } => Some((Role::Button, Words::Held(label))),
            NodeKind::TextInput { placeholder, .. } => {
                Some((Role::TextBox, Words::Held(placeholder)))
            }
            NodeKind::TextArea { placeholder, .. } => {
                Some((Role::TextArea, Words::Held(placeholder)))
            }
            NodeKind::Checkbox { label, .. } => Some((Role::CheckBox, Words::Held(label))),
            NodeKind::RadioGroup { .. } => Some((Role::RadioGroup, Words::Held(""))),
            NodeKind::ListPicker { placeholder, .. } => {
                Some((Role::ListBox, Words::Held(placeholder)))
            }
            NodeKind::Image { alt, .. } => Some((Role::Image, Words::Held(alt))),
            NodeKind::Progress { .. } => Some((Role::ProgressBar, Words::Held(""))),
            NodeKind::Scroll { .. } => Some((Role::ScrollArea, Words::Held(""))),
            NodeKind::Keyboard { .. } => Some((Role::Keyboard, Words::Held("keyboard"))),
            NodeKind::FieldError {
                message: Some(message),
                ..
            } => Some((Role::Alert, Words::Held(message))),
            NodeKind::Layer { kind, .. } => Some((
                match kind {
                    LayerKind::Dialog => Role::Dialog,
                    LayerKind::Menu => Role::Menu,
                    LayerKind::Popover => Role::Popover,
                    LayerKind::Tooltip => Role::Tooltip,
                },
                Words::Held(""),
            )),
            _ => None,
        }
    }

    /// How many of `id`'s ancestors are told of.
    fn access_depth(&self, id: NodeId) -> u8 {
        let mut depth: u8 = 0;
        let mut next = self.get(id).and_then(crate::Node::parent);
        for _ in 0..=crate::MAX_DEPTH {
            let Some(parent) = next else { break };
            if self.role_and_name(parent).is_some() {
                depth = depth.saturating_add(1);
            }
            next = self.get(parent).and_then(crate::Node::parent);
        }
        depth
    }
}
