// SPDX-License-Identifier: MIT OR Apache-2.0
//! Copy and paste inside the application (#10080): a clipboard the tree -
//! the UI PD - holds alone.
//!
//! Ctrl+C and Ctrl+X copy or cut the selection in a text input or area, or
//! a text marked selectable ([`TextStyle::selectable`](crate::TextStyle))
//! that a press has selected; Ctrl+V pastes into the focused input, if it
//! fits and every character is one the input takes. What is copied is
//! wiped in place - at a session wipe, when the tree is cleared, and once
//! it is older than the life a product sets ([`Tree::set_clipboard_life`]);
//! its `Debug` never shows it, so it never reaches a log. A masked field
//! (#8356) will copy nothing.

use core::fmt;

use nowayos_bounded::{BoundedStr, Wipe};

use crate::{NodeId, NodeKind, SHOWN_CAPACITY, Tree};

/// The most bytes the clipboard holds: a copy longer is refused.
pub const CLIPBOARD: usize = 1024;

/// The clipboard: what was copied, how long ago, and how long it may live.
#[derive(Clone)]
pub(crate) struct Clipboard {
    text: BoundedStr<CLIPBOARD>,
    age: u32,
    life: Option<u32>,
}

impl fmt::Debug for Clipboard {
    /// How much it holds, never what.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clipboard")
            .field("len", &self.text.len())
            .field("age", &self.age)
            .field("life", &self.life)
            .finish()
    }
}

impl Clipboard {
    pub(crate) const fn new() -> Self {
        Self {
            text: BoundedStr::new(),
            age: 0,
            life: None,
        }
    }

    /// Holds `text` from now; `false`, keeping what it held, if it is too
    /// long.
    pub(crate) fn set(&mut self, text: &str) -> bool {
        if text.len() > CLIPBOARD {
            return false;
        }
        self.text.wipe();
        self.age = 0;
        self.text.push_str(text).is_ok()
    }

    /// What it holds.
    pub(crate) fn text(&self) -> &str {
        self.text.as_str()
    }

    /// Wipes what it holds, in place.
    pub(crate) fn wipe(&mut self) {
        self.text.wipe();
        self.age = 0;
    }
}

impl<const N: usize, const T: usize> Tree<N, T> {
    /// How long, in milliseconds, what is copied may stay before it is
    /// wiped; `None` until the session ends.
    pub fn set_clipboard_life(&mut self, life: Option<u32>) {
        self.clipboard.life = life;
        self.pass_time(0);
    }

    /// Lets `elapsed` milliseconds pass for the clipboard: past its life,
    /// what it holds is wiped. [`Tree::advance`] calls it; a product with a
    /// clock of its own may too.
    pub fn pass_time(&mut self, elapsed: u32) {
        let clipboard = &mut self.clipboard;
        if clipboard.text.is_empty() {
            return;
        }
        clipboard.age = clipboard.age.saturating_add(elapsed);
        if clipboard.life.is_some_and(|life| clipboard.age >= life) {
            clipboard.wipe();
        }
    }

    /// Whether the clipboard holds anything.
    #[must_use]
    pub fn has_clipboard(&self) -> bool {
        !self.clipboard.text.is_empty()
    }

    /// Whether the clipboard's bytes are all zeros: for tests.
    #[must_use]
    pub fn clipboard_is_wiped(&self) -> bool {
        self.clipboard.text.is_zeroed()
    }

    /// The selectable text a press has selected, if any.
    #[must_use]
    pub const fn selected_text(&self) -> Option<NodeId> {
        self.selected
    }

    /// Copies selectable text `id`, as it shows: Ctrl+C on it.
    pub(crate) fn copy_text(&mut self, id: NodeId) {
        let mut shown: BoundedStr<SHOWN_CAPACITY> = match self.get(id).map(|node| &node.kind) {
            Some(NodeKind::Text(text)) => {
                self.clipboard.set(text);
                return;
            }
            Some(NodeKind::Slot {
                text,
                template: Some(template),
                ..
            }) => template.around(text),
            Some(NodeKind::Slot { text, .. }) => {
                let mut shown = BoundedStr::new();
                // Never full: a slot holds fewer bytes than a shown text.
                if shown.push_str(text).is_err() {
                    return;
                }
                shown
            }
            _ => return,
        };
        self.clipboard.set(shown.as_str());
        shown.wipe();
    }

    /// Selects selectable text `id`, or none, marking what changes.
    pub(crate) fn select_text(&mut self, id: Option<NodeId>) {
        if self.selected == id {
            return;
        }
        for marked in [self.selected, id].into_iter().flatten() {
            self.mark_dirty(marked);
        }
        self.selected = id;
    }
}
