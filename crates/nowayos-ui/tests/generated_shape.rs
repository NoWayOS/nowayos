// SPDX-License-Identifier: MIT OR Apache-2.0
//! Code shaped like Screen Compiler's output (builder contract v0, and v1's slots
//! and groups) must compile against `nowayos_ui::builder` and build a working
//! screen. If this file stops compiling, the contract and the generator have
//! drifted apart.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic_in_result_fn,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::{TextMetrics, Tree, UiError, UiEvent};

// ---- the shape of a generated file (synthetic screen) -----------------------

use nowayos_ui::builder as ui;

/// Design tokens, in logical pixels.
pub mod tokens {
    /// Space between items.
    pub const SPACE_MD: u16 = 8;
}

/// Screen `SignIn`.
pub mod sign_in {
    use super::ui;

    /// Actions the screen emits, numbered from 1 in document order.
    pub mod actions {
        use super::ui;

        /// Callback prop `onBack`.
        pub const BACK: ui::ActionId = ui::ActionId::new(1);
        /// Callback prop `onContinue`.
        pub const CONTINUE: ui::ActionId = ui::ActionId::new(2);
    }

    /// Handles to the root node and to each element with an `id`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Nodes<N> {
        /// The root node.
        pub root: N,
        /// `id="code"`.
        pub code: N,
    }

    /// Builds the screen with `b`, children before their parents.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when its node arena is full.
    pub fn build<B: ui::UiBuilder>(b: &mut B) -> Result<Nodes<B::Node>, B::Error> {
        let n1 = b.text("Enter your code")?;
        let n2 = b.text_input(Some("Code"))?;
        let n3 = b.spacer(None)?;
        let n5 = b.button("Back", actions::BACK)?;
        let n6 = b.spacer(Some(super::tokens::SPACE_MD))?;
        let n7 = b.button("Continue", actions::CONTINUE)?;
        let n4 = b.row(ui::Layout::new(), &[n5, n6, n7])?;
        let n0 = b.column(
            ui::Layout::new()
                .gap(super::tokens::SPACE_MD)
                .padding(super::tokens::SPACE_MD),
            &[n1, n2, n3, n4],
        )?;
        Ok(Nodes { root: n0, code: n2 })
    }
}

/// Screen `ParcelStatus`: contract v1's slots and groups, as
/// Screen Compiler writes them for Screen Compiler's `parcel-status.tsx` fixture.
pub mod parcel_status {
    use super::ui;

    /// Actions the screen emits, numbered from 1 in document order.
    pub mod actions {
        use super::ui;

        /// Callback prop `onOpen`.
        pub const OPEN: ui::ActionId = ui::ActionId::new(1);
    }

    /// Groups of content the application shows or hides, one per prop, numbered from 1 in document order.
    pub mod groups {
        use super::ui;

        /// Prop `ready`.
        pub const READY: ui::GroupId = ui::GroupId::new(1);
        /// Prop `late`.
        pub const LATE: ui::GroupId = ui::GroupId::new(2);
    }

    /// Slots for text the application sets, one per prop, numbered from 1 in document order.
    pub mod slots {
        use super::ui;

        /// Prop `locker`.
        pub const LOCKER: ui::SlotId = ui::SlotId::new(1);
        /// Prop `lateNotice`.
        pub const LATE_NOTICE: ui::SlotId = ui::SlotId::new(2);
    }

    /// Handles to the root node and to each element with an `id`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Nodes<N> {
        /// The root node.
        pub root: N,
        /// `id="open"`.
        pub open: N,
    }

    /// Builds the screen with `b`, children before their parents.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when its node arena is full.
    pub fn build<B: ui::UiBuilder>(b: &mut B) -> Result<Nodes<B::Node>, B::Error> {
        let n2 = b.text("It is ready to collect.")?;
        let n3 = b.text("It is on its way to this locker.")?;
        let n1 = b.either(groups::READY, n2, n3)?;
        let n5 = b.text("Locker:")?;
        let n6 = b.slot(slots::LOCKER)?;
        let n4 = b.row(ui::Layout::new().gap(8), &[n5, n6])?;
        let n8 = b.slot(slots::LATE_NOTICE)?;
        let n7 = b.group(groups::LATE, n8)?;
        let n10 = b.button("Open the locker", actions::OPEN)?;
        let n9 = b.group(groups::READY, n10)?;
        let n0 = b.column(ui::Layout::new().gap(4), &[n1, n4, n7, n9])?;
        Ok(Nodes {
            root: n0,
            open: n10,
        })
    }
}

/// Screen `VisitBooking`: contract v1's value controls, as Screen Compiler
/// writes them for Screen Compiler's `visit-booking.tsx` fixture.
pub mod visit_booking {
    use super::ui;

    /// Fields of the controls that hold a value, named after their ids, numbered from 1 in document order.
    pub mod fields {
        use super::ui;

        /// `TextInput` `id="phone"`.
        pub const PHONE: ui::FieldId = ui::FieldId::new(1);
        /// `RadioGroup` `id="when"`.
        pub const WHEN: ui::FieldId = ui::FieldId::new(2);
        /// `Checkbox` `id="remind"`.
        pub const REMIND: ui::FieldId = ui::FieldId::new(3);
    }

    /// Handles to the root node and to each element with an `id`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Nodes<N> {
        /// The root node.
        pub root: N,
        /// `id="phone"`.
        pub phone: N,
    }

    /// Builds the screen with `b`, children before their parents.
    ///
    /// # Errors
    ///
    /// The builder's error, for example when its node arena is full.
    pub fn build<B: ui::UiBuilder>(b: &mut B) -> Result<Nodes<B::Node>, B::Error> {
        let n1 = b.input(
            fields::PHONE,
            ui::Input::new()
                .placeholder("Phone, digits only")
                .max_len(super::tokens::SPACE_MD)
                .mode(ui::InputMode::Digits),
        )?;
        let n2 = b.radio_group(fields::WHEN, &["Morning", "Afternoon", "Evening"])?;
        let n3 = b.checkbox(fields::REMIND, "Remind me the day before")?;
        let n0 = b.column(ui::Layout::new().gap(4), &[n1, n2, n3])?;
        Ok(Nodes {
            root: n0,
            phone: n1,
        })
    }
}

// ---- an application using it -------------------------------------------------

#[test]
fn generated_screen_builds_lays_out_and_responds() -> Result<(), UiError> {
    let mut tree: Tree<16> = Tree::new();
    let nodes = sign_in::build(&mut tree)?;
    tree.set_root(nodes.root)?;
    tree.layout(Rect::new(0, 0, 240, 160), &TextMetrics::new(6, 10))?;

    // Tab into the code field and type.
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    assert_eq!(tree.focused(), Some(nodes.code));
    for key in [KeyCode::digit(4), KeyCode::digit(2)] {
        tree.handle(InputEvent::KeyDown(key.unwrap()));
    }
    assert_eq!(tree.node_value(nodes.code), Some("42"));
    assert_eq!(tree.field_of(nodes.code), Some(nowayos_ui::FieldId::new(1)));

    // Tab past Back to Continue and press Enter.
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    assert_eq!(
        tree.handle(InputEvent::KeyDown(KeyCode::ENTER)),
        Some(UiEvent::Action(sign_in::actions::CONTINUE))
    );
    Ok(())
}

#[test]
fn a_v1_screen_shows_application_text_and_what_its_props_say() -> Result<(), UiError> {
    use parcel_status::{groups, slots};
    let viewport = Rect::new(0, 0, 240, 160);
    let metrics = TextMetrics::new(6, 10);
    let mut tree: Tree<16> = Tree::new();
    let nodes = parcel_status::build(&mut tree)?;
    tree.set_root(nodes.root)?;
    tree.layout(viewport, &metrics)?;
    // Hidden at first: no Open button to tab to, nothing to act on.
    assert_eq!(tree.handle(InputEvent::KeyDown(KeyCode::TAB)), None);
    // The application fills the slots and shows what its props say.
    tree.set_slot(slots::LOCKER, "B-14")?;
    tree.set_visible(groups::READY, true)?;
    tree.layout(viewport, &metrics)?;
    assert_eq!(tree.slot_text(slots::LOCKER), Some("B-14"));
    assert_eq!(
        tree.handle(InputEvent::KeyDown(KeyCode::TAB)),
        Some(UiEvent::FocusChanged(Some(nodes.open)))
    );
    assert_eq!(
        tree.handle(InputEvent::KeyDown(KeyCode::ENTER)),
        Some(UiEvent::Action(parcel_status::actions::OPEN))
    );
    // A slot number used twice is a typed error when the screen is built.
    assert_eq!(
        ui::UiBuilder::slot(&mut tree, slots::LOCKER),
        Err(UiError::DuplicateSlot(slots::LOCKER))
    );
    Ok(())
}

#[test]
fn a_v1_screens_fields_take_what_their_controls_allow() -> Result<(), UiError> {
    use visit_booking::fields;
    let mut tree: Tree<8> = Tree::new();
    let nodes = visit_booking::build(&mut tree)?;
    tree.set_root(nodes.root)?;
    tree.layout(Rect::new(0, 0, 240, 200), &TextMetrics::new(6, 10))?;
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    assert_eq!(tree.focused(), Some(nodes.phone));
    // Digits only, at most SPACE_MD (8) of them.
    tree.handle(InputEvent::KeyDown(KeyCode::letter('x').unwrap()));
    for _ in 0..9 {
        tree.handle(InputEvent::KeyDown(KeyCode::digit(7).unwrap()));
    }
    assert_eq!(tree.value(fields::PHONE), Some("77777777"));
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    tree.handle(InputEvent::KeyDown(KeyCode::DOWN));
    assert_eq!(tree.selected(fields::WHEN), Some(0));
    tree.handle(InputEvent::KeyDown(KeyCode::TAB));
    tree.handle(InputEvent::KeyDown(KeyCode::SPACE));
    assert_eq!(tree.checked(fields::REMIND), Some(true));
    // A field number used twice is a typed error when the screen is built.
    assert_eq!(
        ui::UiBuilder::checkbox(&mut tree, fields::WHEN, "Again"),
        Err(UiError::DuplicateField(fields::WHEN))
    );
    Ok(())
}

#[test]
fn a_full_arena_is_an_error_not_a_panic() {
    let mut tree: Tree<4> = Tree::new();
    assert_eq!(
        sign_in::build(&mut tree),
        Err(UiError::TreeFull { capacity: 4 })
    );
}
