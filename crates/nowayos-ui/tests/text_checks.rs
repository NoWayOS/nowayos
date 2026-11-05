// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every text a node shows is checked against the glyph atlases' set:
//! a character outside
//! it, an invisible format character such as U+202E (a right-to-left
//! override) or a control character is refused with a typed error that
//! never names it - fixed text when its node is added, text set later when
//! it is set, the previous text staying.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_bounded::BoundedStr;
use nowayos_display::Rect;
use nowayos_input::{KeyCode, keymap};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::text::is_printable;
use nowayos_ui::{ActionId, FieldId, NodeKind, SlotId, Style, TextMetrics, Tree, UiError, UiEvent};

/// Text no node may show, each with the error it gets: the first character
/// that fails decides.
const REFUSED: [(&str, UiError); 12] = [
    // A right-to-left override would show "Pay 1000" reversed as typed.
    ("Pay \u{202E}0001", UiError::InvisibleCharacter),
    ("zero\u{200B}width", UiError::InvisibleCharacter),
    ("soft\u{AD}hyphen", UiError::InvisibleCharacter),
    ("joined\u{2060}word", UiError::InvisibleCharacter),
    ("two\nlines", UiError::ControlCharacter),
    ("tab\there", UiError::ControlCharacter),
    ("c1\u{85}control", UiError::ControlCharacter),
    ("em \u{2014} dash", UiError::NoGlyph),
    ("\u{20AC}5", UiError::NoGlyph),
    // Decomposed: e and a combining acute accent; only precomposed text.
    ("cafe\u{301}", UiError::NoGlyph),
    ("\u{6771}\u{4EAC}", UiError::NoGlyph),
    ("\u{1F600}", UiError::NoGlyph),
];

/// `text` as `&'static str`, as fixed text is.
fn fixed(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

#[test]
fn fixed_text_that_cannot_be_shown_is_refused_when_its_node_is_added() {
    for (text, error) in REFUSED {
        let text = fixed(text);
        let options: &'static [&'static str] = Box::leak(Box::new(["Fine", text]));
        let mut value = BoundedStr::new();
        value.push_str(text).unwrap();
        let mut t: Tree<16> = Tree::new();
        let refused = Err(error);
        assert_eq!(t.text(text), refused, "{text:?}");
        assert_eq!(t.button(text, ActionId(1)), refused, "{text:?}");
        assert_eq!(t.text_input(FieldId(1), text), refused, "{text:?}");
        assert_eq!(t.checkbox(FieldId(2), text), refused, "{text:?}");
        assert_eq!(t.radio_group(FieldId(3), options), refused, "{text:?}");
        for kind in [
            NodeKind::FieldError {
                field: FieldId(4),
                message: Some(text),
            },
            NodeKind::TextInput {
                field: FieldId(5),
                placeholder: "",
                value: value.clone(),
                max_len: 64,
                mode: nowayos_ui::InputMode::Text,
                caret: 0,
                anchor: 0,
            },
            NodeKind::Slot {
                slot: SlotId(1),
                text: value.clone(),
                template: None,
            },
        ] {
            assert_eq!(t.add(kind, Style::new()), refused, "{text:?}");
        }
        // Screens Screen Compiler generates build through the same checks.
        assert_eq!(UiBuilder::text(&mut t, text), refused, "{text:?}");
        assert_eq!(
            UiBuilder::button(&mut t, text, ui::ActionId::new(1)),
            refused
        );
        assert_eq!(UiBuilder::text_input(&mut t, Some(text)), refused);
        assert!(t.is_empty(), "{text:?}: nothing was created");
    }
}

#[test]
fn text_set_later_is_refused_and_the_previous_text_stays() {
    const NAME: FieldId = FieldId(1);
    const ANSWER: SlotId = SlotId(1);
    let mut t: Tree<16> = Tree::new();
    let column = t.column(Style::new()).unwrap();
    t.set_root(column).unwrap();
    let label = t.text("Your answer").unwrap();
    let button = t.button("Continue", ActionId(1)).unwrap();
    let input = t.text_input(NAME, "Name").unwrap();
    let slot = t.slot(ANSWER).unwrap();
    let error = t.field_error(NAME).unwrap();
    for id in [label, button, input, slot, error] {
        t.append(column, id).unwrap();
    }
    t.layout(Rect::new(0, 0, 320, 240), &TextMetrics::new(6, 10))
        .unwrap();
    t.set_value(NAME, "Ada").unwrap();
    t.set_slot(ANSWER, "Ref. 4F-2207").unwrap();
    t.set_field_error(NAME, "Enter your name").unwrap();
    let before = |t: &Tree<16>| {
        (
            t.get(label).unwrap().kind().clone(),
            t.get(button).unwrap().kind().clone(),
            t.value(NAME).map(str::to_owned),
            t.slot_text(ANSWER).map(str::to_owned),
            t.field_error_message(NAME),
        )
    };
    let shown = before(&t);
    t.clear_dirty();
    for (text, error) in REFUSED {
        let fixed = fixed(text);
        assert_eq!(t.set_text(label, fixed), Err(error), "{text:?}");
        assert_eq!(t.set_text(button, fixed), Err(error), "{text:?}");
        assert_eq!(t.set_value(NAME, text), Err(error), "{text:?}");
        assert_eq!(t.set_slot(ANSWER, text), Err(error), "{text:?}");
        assert_eq!(t.set_field_error(NAME, fixed), Err(error), "{text:?}");
        assert_eq!(before(&t), shown, "{text:?}: the previous text stays");
        assert!(t.dirty_rects().is_empty(), "{text:?}: nothing repaints");
    }
}

/// The errors say what is wrong, never which character: text a person
/// typed or the application set cannot leak through them.
#[test]
fn the_errors_never_name_the_character() {
    for (text, error) in REFUSED {
        for shown in [std::format!("{error}"), std::format!("{error:?}")] {
            for ch in text
                .chars()
                .filter(|&ch| !ch.is_ascii_alphabetic() && ch != ' ')
            {
                assert!(!shown.contains(ch), "{shown:?} names {ch:?}");
            }
            assert!(!shown.contains(text), "{shown:?} holds {text:?}");
        }
    }
}

/// Typed text is always in the set too: every character the keyboard map
/// can type is one the atlases draw, so typing never needs the error.
#[test]
fn every_character_the_keyboard_types_is_in_the_set() {
    let mut typed = 0;
    for usage in 0..=u16::MAX {
        let Some(key) = KeyCode::new(usage) else {
            continue;
        };
        for shift in [false, true] {
            if let Some(ch) = keymap::us_char(key, shift) {
                assert!(is_printable(ch), "{key:?} with shift {shift} types {ch:?}");
                typed += 1;
            }
        }
    }
    // Letters, digits, space and punctuation, each with and without Shift.
    assert_eq!(typed, 2 * (26 + 10 + 1 + 11));
    // And a typed character still lands in the focused input.
    let mut t: Tree<4> = Tree::new();
    let input = t.text_input(FieldId(1), "").unwrap();
    t.set_root(input).unwrap();
    t.layout(Rect::new(0, 0, 100, 40), &TextMetrics::new(6, 10))
        .unwrap();
    t.set_focus(Some(input)).unwrap();
    assert_eq!(
        t.handle(nowayos_ui::InputEvent::KeyDown(KeyCode::new(0x2D).unwrap())),
        Some(UiEvent::ValueChanged(FieldId(1)))
    );
    assert_eq!(t.value(FieldId(1)), Some("-"));
}

/// Every printable character is accepted everywhere, so the first cut's
/// screens, all in printable Latin-1, build and look as before.
#[test]
fn every_printable_character_is_accepted() {
    let all: String = (0..=0xFF_u32)
        .filter_map(char::from_u32)
        .filter(|&ch| is_printable(ch))
        .collect();
    assert_eq!(all.chars().count(), 190);
    let all = fixed(&all[..]);
    let mut t: Tree<16> = Tree::new();
    t.text(all).unwrap();
    t.button(all, ActionId(1)).unwrap();
    t.text_input(FieldId(1), all).unwrap();
    t.checkbox(FieldId(2), all).unwrap();
    let slot = t.slot(SlotId(1)).unwrap();
    let error = t.field_error(FieldId(1)).unwrap();
    assert!(t.get(slot).is_some() && t.get(error).is_some());
    t.set_slot(SlotId(1), "Zoë, café, 3 × 4 ÷ 2, ½ ¿ ÿ µ")
        .unwrap();
    t.set_value(FieldId(1), "naïve façade").unwrap();
    t.set_field_error(FieldId(1), all).unwrap();
}
