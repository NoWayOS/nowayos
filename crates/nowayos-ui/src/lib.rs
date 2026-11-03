// SPDX-License-Identifier: MIT OR Apache-2.0
//! NoWayOS's declarative UI core — **first cut**.
//!
//! A deliberately small UI model that a secure appliance can afford to own:
//! not a DOM, not CSS. A screen is a bounded [`Tree`] of
//! nodes — layout containers ([`NodeKind::Column`], [`NodeKind::Row`],
//! [`NodeKind::Stack`]), content ([`NodeKind::Text`], and
//! [`NodeKind::Slot`] for text the application sets at run time), controls
//! ([`NodeKind::Button`], [`NodeKind::TextInput`], [`NodeKind::Checkbox`],
//! [`NodeKind::RadioGroup`]), the on-screen [`NodeKind::Keyboard`]
//! ([`keyboard`]), error lines ([`NodeKind::FieldError`]),
//! groups the application shows or hides ([`NodeKind::Group`]) and
//! [`NodeKind::Spacer`]. The tree provides:
//!
//! * **layout** — padding, gaps, cross-axis alignment, and fixed, intrinsic
//!   or fill sizes along each axis ([`Tree::layout`]);
//! * **hit-testing** — which node is under a point ([`Tree::hit_test`]);
//! * **focus** — keyboard focus order and navigation over the controls
//!   ([`Tree::focus_next`], [`Tree::focus_prev`]);
//! * **interaction** — canonical [`InputEvent`]s in, [`UiEvent`]s out
//!   (actions, focus and value changes) ([`Tree::handle`]);
//! * **dirty tracking** — the screen areas that need repainting after each
//!   change ([`Tree::dirty_rects`]);
//! * **navigation** — switching between screens built into the one tree,
//!   with a bounded back stack ([`Navigator`]);
//! * **conditional content** — a group shows its content, or chooses
//!   between two, as the application says ([`Tree::set_visible`]): hidden
//!   content takes no space and is neither drawn, focusable nor hit.
//!
//! Rendering is separate: `nowayos-render-eg` draws a laid-out tree to any
//! `embedded-graphics` draw target. Screens converted from React by
//! Screen Compiler are built through the children-first [`builder`] API.
//!
//! Everything is bounded: the tree holds at most `N` nodes, nests at most
//! [`MAX_DEPTH`] deep, a text input holds at most its own `max_len` bytes,
//! never more than [`INPUT_CAPACITY`], and a slot [`SLOT_CAPACITY`]. Nothing
//! allocates; nothing panics.
//!
//! ```
//! use nowayos_display::Rect;
//! use nowayos_ui::{ActionId, Align, FieldId, Style, TextMetrics, Tree, UiEvent};
//! use nowayos_input::{InputEvent, KeyCode};
//!
//! const CONTINUE: ActionId = ActionId(1);
//! const NAME: FieldId = FieldId(1);
//!
//! let mut tree: Tree<16> = Tree::new();
//! let screen = tree.column(Style::new().padding(8).gap(4).align(Align::Stretch))?;
//! tree.set_root(screen)?;
//! let title = tree.text("Welcome")?;
//! tree.append(screen, title)?;
//! let name = tree.text_input(NAME, "Your name")?;
//! tree.append(screen, name)?;
//! let button = tree.button("Continue", CONTINUE)?;
//! tree.append(screen, button)?;
//!
//! tree.layout(Rect::new(0, 0, 200, 120), &TextMetrics::new(6, 10))?;
//!
//! // Tab to the text input, type, Tab to the button, press Enter.
//! tree.handle(InputEvent::KeyDown(KeyCode::TAB));
//! tree.handle(InputEvent::KeyDown(KeyCode::letter('h').unwrap()));
//! tree.handle(InputEvent::KeyDown(KeyCode::letter('i').unwrap()));
//! assert_eq!(tree.value(NAME), Some("hi"));
//! tree.handle(InputEvent::KeyDown(KeyCode::TAB));
//! assert_eq!(
//!     tree.handle(InputEvent::KeyDown(KeyCode::ENTER)),
//!     Some(UiEvent::Action(CONTINUE))
//! );
//! # Ok::<(), nowayos_ui::UiError>(())
//! ```
//!
//! # Status
//!
//! Text is one line per node, and every character of every text a node
//! shows must be one the glyph atlases draw and a person can see
//! ([`text::check_printable`]): a control character such as `'\n'`, an
//! invisible one such as a direction override, or one outside the atlases'
//! set is refused with a typed error that never names it - fixed text when
//! its node is added, text set later ([`Tree::set_slot`],
//! [`Tree::set_value`], [`Tree::set_text`], [`Tree::set_field_error`]) when
//! it is set, the previous text staying. Layout measures text with the
//! metrics it is given ([`TextMetrics`]): a glyph atlas's advances, kerning
//! and line height, or a built-in monospace font's. Layout accepts
//! viewports up to [`MAX_VIEWPORT_SIZE`] each way within
//! [`MAX_VIEWPORT_OFFSET`] of the origin, which keeps every coordinate a
//! renderer draws at far from the ends of the `i32` range. [`Tree::clear`]
//! wipes typed values and slot texts in place (`nowayos_bounded::Wipe`)
//! before it lets the nodes go; [`Tree::wipe_session`] wipes them keeping
//! the nodes, so [`Tree::is_wiped`] can check every byte, and a session
//! reset ([`Navigator::reset`]) starts with it.
//!
//! Provisional: this whole API may change. Known gaps,
//! by design for a first cut: single-line text, no text cursor (inputs
//! append and delete at the end), no scrolling or clipping containers, no
//! images, no select control, and only the first touch contact interacts.
//! `nowayos-render-eg`'s themes draw text from the glyph atlases, and lay
//! it out with the same atlas's metrics.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

pub mod builder;
mod error;
mod interact;
pub mod keyboard;
mod layout;
pub mod nav;
mod node;
pub mod text;
mod tree;

pub use error::UiError;
pub use interact::UiEvent;
pub use layout::{MAX_VIEWPORT_OFFSET, MAX_VIEWPORT_SIZE, TextMetrics};
pub use nav::{Navigator, ScreenId};
pub use node::{
    ActionId, Align, BUTTON_PADDING, ButtonVariant, CHOICE_PADDING, Colour, ContainerKind, FieldId,
    GroupId, INPUT_CAPACITY, INPUT_PADDING, INPUT_WIDTH_CHARS, Input, InputMode, Justify,
    KEY_SPACE, KEYBOARD_PADDING, LIST_ROWS, LIST_ROWS_DEFAULT, Length, Look, MAX_LINES,
    MAX_LIST_OPTIONS, MAX_OPTIONS, MIN_TARGET, Node, NodeId, NodeKind, PROGRESS_HEIGHT,
    PROGRESS_WHOLE, SHOWN_CAPACITY, SLOT_CAPACITY, SlotId, Space, SpaceTokens, Spacing, Style,
    Surface, SwitchId, TEMPLATE_CAPACITY, Template, TextAlign, TextRole, TextStyle, Tone, Weight,
};
/// The icons a screen may show: Lucide's, as alpha masks.
pub use nowayos_icons as icons;
pub use nowayos_input::InputEvent;
pub use tree::{Children, MAX_DEPTH, PaintOrder, Tree};
