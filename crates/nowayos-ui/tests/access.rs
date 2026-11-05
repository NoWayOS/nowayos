// SPDX-License-Identifier: MIT OR Apache-2.0
//! The accessibility tree (#10085, a spike): a fixture screen told as a
//! screen reader would be told it - each thing's role, name, state and
//! value, at its depth - and kept in step: what changes in the tree
//! changes in it. Its `Debug` never shows a value.

#![allow(clippy::unwrap_used, reason = "test code: a failed check panics")]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode};
use nowayos_ui::builder::{Layout, Run, UiBuilder};
use nowayos_ui::{ActionId, FieldId, Input, LayerId, TextMetrics, TextRole, TextStyle, Tree};

const RUNS: &[Run] = &[
    Run::new("Read "),
    Run::new("the site rules").link(ActionId(3)),
    Run::new(" first."),
];

fn screen() -> Tree<32> {
    let mut tree: Tree<32> = Tree::new();
    let t = &mut tree;
    let title = UiBuilder::text_styled(t, "Book a visit", TextStyle::new(TextRole::Title)).unwrap();
    let rules = UiBuilder::rich_text(t, RUNS, TextStyle::default()).unwrap();
    let name = UiBuilder::input(t, FieldId(1), Input::new().placeholder("Your name")).unwrap();
    let day = UiBuilder::radio_group(t, FieldId(2), &["Monday", "Tuesday"]).unwrap();
    let agree = UiBuilder::checkbox(t, FieldId(3), "I have read them").unwrap();
    let go = UiBuilder::button(t, "Continue", ActionId(1)).unwrap();
    let ok = UiBuilder::button(t, "OK", ActionId(2)).unwrap();
    let dialog = UiBuilder::dialog(t, LayerId(1), ok).unwrap();
    let form = UiBuilder::column(
        t,
        Layout::new().gap(8),
        &[title, rules, name, day, agree, go],
    )
    .unwrap();
    let root = UiBuilder::column(t, Layout::new(), &[form, dialog]).unwrap();
    tree.set_root(root).unwrap();
    tree.layout(Rect::new(0, 0, 480, 400), &TextMetrics::new(8, 16))
        .unwrap();
    tree
}

fn told(tree: &Tree<32>) -> String {
    let mut out = String::new();
    tree.access(|thing| {
        out.push_str(&"  ".repeat(usize::from(thing.depth)));
        out.push_str(&thing.to_string());
        out.push('\n');
    });
    out
}

#[test]
fn a_screen_is_told_with_each_role_name_state_and_value_in_step() {
    let mut tree = screen();
    let name = tree.find_field(FieldId(1)).unwrap();
    tree.set_focus(Some(name)).unwrap();
    for ch in ['z', 'o', 'e'] {
        tree.handle(InputEvent::KeyDown(KeyCode::letter(ch).unwrap()));
    }
    tree.set_selected(FieldId(2), Some(1)).unwrap();
    let shown = told(&tree);
    println!("{shown}");
    assert_eq!(
        shown,
        "heading \"Book a visit\"\n\
         text \"Read the site rules first.\"\n\
         \x20 link \"the site rules\"\n\
         textbox \"Your name\" value \"zoe\" focused\n\
         radiogroup \"\"\n\
         \x20 radio \"Monday\"\n\
         \x20 radio \"Tuesday\" selected\n\
         checkbox \"I have read them\" not checked\n\
         button \"Continue\"\n"
    );
    // In step: the box ticked, the dialog opened over the screen.
    tree.set_checked(FieldId(3), true).unwrap();
    tree.open(LayerId(1)).unwrap();
    let shown = told(&tree);
    assert!(
        shown.contains("checkbox \"I have read them\" checked\n"),
        "{shown}"
    );
    assert!(
        shown.ends_with("dialog \"\"\n  button \"OK\" focused\n"),
        "{shown}"
    );
}

#[test]
fn debug_never_shows_a_value() {
    let mut tree = screen();
    tree.set_value(FieldId(1), "secret").unwrap();
    let mut debug = String::new();
    tree.access(|thing| debug.push_str(&format!("{thing:?}\n")));
    assert!(debug.contains("TextBox"));
    assert!(!debug.contains("secret"), "{debug}");
}
