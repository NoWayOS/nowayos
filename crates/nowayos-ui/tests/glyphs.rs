// SPDX-License-Identifier: MIT OR Apache-2.0
//! The glyph atlases and the text model agree: every atlas but the
//! display role's draws exactly the characters `text::is_printable`
//! accepts - printable Latin-1 - and the display role's atlases
//! exactly those `text::is_display` accepts, so text the UI accepts
//! in a role always has a glyph there, and a character it refuses never
//! does.

use nowayos_glyphs::{ATLASES, DISPLAY_96, DISPLAY_192};
use nowayos_ui::text::{is_display, is_printable};

fn set(accepts: fn(char) -> bool) -> Vec<char> {
    (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|&ch| accepts(ch))
        .collect()
}

#[test]
fn every_atlas_draws_exactly_its_roles_set() {
    let printable = set(is_printable);
    let display = set(is_display);
    assert_eq!(printable.len(), 190);
    assert_eq!(display.len(), 46);
    assert!(display.iter().all(|&ch| is_printable(ch)));
    for atlas in ATLASES {
        let drawn: Vec<char> = atlas.glyphs().iter().map(|glyph| glyph.ch()).collect();
        let is_display_atlas =
            core::ptr::eq(atlas, &DISPLAY_96) || core::ptr::eq(atlas, &DISPLAY_192);
        let expected = if is_display_atlas {
            &display
        } else {
            &printable
        };
        assert_eq!(&drawn, expected, "the {} px atlas", atlas.size());
    }
}
