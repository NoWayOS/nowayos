// SPDX-License-Identifier: MIT OR Apache-2.0
//! The faces and the text model agree: every face but the display role's
//! draws exactly the characters `text::is_printable` accepts - printable
//! Latin-1 - and the display face exactly those `text::is_display`
//! accepts, so text the UI accepts in a role always has a glyph there, and
//! a character it refuses never does.

use nowayos_glyphs::{BOLD, BOLDITALIC, CODE, CODEBOLD, DISPLAY, Face, ITALIC, REGULAR};
use nowayos_ui::text::{is_display, is_printable};

fn set(accepts: fn(char) -> bool) -> Vec<char> {
    (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|&ch| accepts(ch))
        .collect()
}

#[test]
fn every_face_draws_exactly_its_roles_set() {
    let printable = set(is_printable);
    let display = set(is_display);
    assert_eq!(printable.len(), 190);
    assert_eq!(display.len(), 46);
    assert!(display.iter().all(|&ch| is_printable(ch)));
    let faces: [&Face; 7] = [
        &REGULAR,
        &BOLD,
        &DISPLAY,
        &ITALIC,
        &BOLDITALIC,
        &CODE,
        &CODEBOLD,
    ];
    for face in faces {
        let drawn: Vec<char> = (0..=u32::from(char::MAX))
            .filter_map(char::from_u32)
            .filter(|&ch| face.glyph(ch).is_ok())
            .collect();
        let expected = if core::ptr::eq(face, &DISPLAY) {
            &display
        } else {
            &printable
        };
        assert_eq!(&drawn, expected, "face {}", face.id());
    }
}
