// SPDX-License-Identifier: MIT OR Apache-2.0
//! The glyph atlas tool on a copy of the real atlas crate's inputs: the
//! signed-off font, its pins and its settings. A font that differs
//! from its pin, an unpinned file in fonts/, or a character the font lacks
//! fails naming the file or the character, and writes nothing; two runs
//! give the same bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics"
)]

use std::fs;
use std::path::{Path, PathBuf};

use nowayos_glyph_atlas::{Error, generate, run};

const REGULAR: &str = "fonts/AtkinsonHyperlegibleNext-Regular.ttf";
const BOLD: &str = "fonts/AtkinsonHyperlegibleNext-Bold.ttf";

/// A copy of the atlas crate's inputs in a fresh folder named `name`.
fn inputs(name: &str) -> PathBuf {
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/nowayos-glyphs");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(dir.join("fonts")).unwrap();
    for file in ["Cargo.toml", "glyphs.toml", "OFL.txt", REGULAR, BOLD] {
        fs::copy(real.join(file), dir.join(file)).unwrap();
    }
    dir
}

/// Replaces `old` by `new` in the copy's `file`, which must hold it.
fn edit(dir: &Path, file: &str, old: &str, new: &str) {
    let text = fs::read_to_string(dir.join(file)).unwrap();
    assert!(text.contains(old), "{file} has no {old:?}");
    fs::write(dir.join(file), text.replacen(old, new, 1)).unwrap();
}

fn run_on(dir: &Path, out: &Path) -> Result<String, Error> {
    run(&[
        "--out".into(),
        out.display().to_string(),
        dir.display().to_string(),
    ])
}

#[test]
fn two_runs_give_the_same_bytes_naming_no_path() {
    let dir = inputs("same-bytes");
    let first = generate(&dir).unwrap();
    let second = generate(&dir).unwrap();
    assert_eq!(first.files, second.files);
    let names: Vec<&str> = first.files.iter().map(|(name, _)| name.as_str()).collect();
    let mut expected = vec![String::from("manifest.toml"), String::from("mod.rs")];
    for (face, sizes) in [
        ("regular", &[14, 16, 18, 24, 28, 32, 36, 48][..]),
        (
            "bold",
            &[14, 16, 18, 20, 24, 28, 32, 36, 40, 48, 56, 72][..],
        ),
        ("display", &[96, 192][..]),
    ] {
        expected.push(format!("{face}_kerning.rs"));
        expected.push(format!("{face}_outline.rs"));
        for size in sizes {
            expected.push(format!("{face}_{size}.coverage"));
            expected.push(format!("{face}_{size}.rs"));
        }
    }
    expected.sort();
    assert_eq!(names, expected);
    let folder = dir.display().to_string();
    for (name, contents) in &first.files {
        if !name.ends_with(".coverage") {
            let text = std::str::from_utf8(contents).unwrap();
            assert!(text.is_ascii(), "{name}");
            assert!(!text.contains(&folder), "{name} names the folder");
            assert!(
                text.contains("Copyright 2020-2024 The Atkinson Hyperlegible Next Project Authors")
            );
        }
    }
    assert!(first.summary.starts_with(
        "glyph-atlas: PASS - 22 atlases: regular Regular at 14, 16, 18, 24, 28, 32, 36, 48 px"
    ));
}

#[test]
fn a_font_that_differs_from_its_pin_fails_naming_it() {
    let dir = inputs("differs");
    let mut font = fs::read(dir.join(REGULAR)).unwrap();
    let last = font.len() - 1;
    font[last] ^= 1;
    fs::write(dir.join(REGULAR), font).unwrap();
    let out = dir.join("out");
    let error = run_on(&dir, &out).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, Error::Pins(_)), "{message}");
    assert!(
        message.contains(&format!("{REGULAR}: its sha256 is")),
        "{message}"
    );
    assert!(!out.exists(), "nothing is written");
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn a_file_in_fonts_that_is_not_pinned_fails_naming_it() {
    let dir = inputs("unlisted");
    fs::copy(dir.join(REGULAR), dir.join("fonts/Another-Bold.ttf")).unwrap();
    fs::create_dir(dir.join("fonts/more")).unwrap();
    fs::write(dir.join("fonts/more/notes.txt"), "a note").unwrap();
    let message = generate(&dir).err().unwrap().to_string();
    assert!(
        message.contains("fonts/Another-Bold.ttf is in fonts/ but not pinned"),
        "{message}"
    );
    assert!(
        message.contains("fonts/more/notes.txt is in fonts/ but not pinned"),
        "{message}"
    );
}

#[test]
fn a_pinned_file_that_is_missing_fails_naming_it() {
    let dir = inputs("missing-file");
    fs::remove_file(dir.join("OFL.txt")).unwrap();
    let message = generate(&dir).err().unwrap().to_string();
    assert!(
        message.contains("OFL.txt is pinned in glyphs.toml but missing"),
        "{message}"
    );
}

#[test]
fn a_character_the_font_lacks_fails_naming_it_and_writes_nothing() {
    // Without its alias: the font has no micro sign.
    let dir = inputs("lacks-micro");
    edit(
        &dir,
        "glyphs.toml",
        "[[alias]]\nchar = 0xB5\nglyph-of = 0x3BC\n",
        "",
    );
    let out = dir.join("out");
    let error = run_on(&dir, &out).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, Error::MissingGlyphs { .. }), "{message}");
    assert!(message.contains(REGULAR), "{message}");
    assert!(message.contains("U+00B5 '\u{B5}'"), "{message}");
    assert!(!out.exists(), "nothing is written");

    // A Cyrillic letter, which the family does not have; the folder the tool
    // wrote before is left as it was.
    let dir = inputs("lacks-cyrillic");
    let out = dir.join("out");
    run_on(&dir, &out).unwrap();
    let before = fs::read(out.join("regular_24.rs")).unwrap();
    edit(
        &dir,
        "glyphs.toml",
        "[0xAE, 0xFF]]",
        "[0xAE, 0xFF], [0x416, 0x416]]",
    );
    let message = run_on(&dir, &out).unwrap_err().to_string();
    assert!(
        message.contains("has no glyph for 1 character(s)"),
        "{message}"
    );
    assert!(message.contains("U+0416 '\u{416}'"), "{message}");
    assert_eq!(fs::read(out.join("regular_24.rs")).unwrap(), before);
}

#[test]
fn an_alias_the_font_does_not_need_is_refused() {
    let dir = inputs("needless-alias");
    edit(&dir, "glyphs.toml", "char = 0xB5\n", "char = 0x41\n");
    let message = generate(&dir).err().unwrap().to_string();
    assert!(
        message.contains("has a glyph for U+0041 'A' itself"),
        "{message}"
    );
}

#[test]
fn the_fonts_own_version_must_be_the_pinned_one() {
    let dir = inputs("version");
    edit(
        &dir,
        "glyphs.toml",
        "version = \"2.001\"\nsha256 = \"88ed",
        "version = \"2.002\"\nsha256 = \"88ed",
    );
    let message = generate(&dir).err().unwrap().to_string();
    assert!(message.starts_with(REGULAR), "{message}");
    assert!(message.contains("pins 2.002"), "{message}");
}

#[test]
fn the_crate_must_declare_the_fonts_licence() {
    let dir = inputs("licence");
    edit(
        &dir,
        "Cargo.toml",
        "license = \"OFL-1.1\"",
        "license = \"MIT\"",
    );
    let message = generate(&dir).err().unwrap().to_string();
    assert!(
        message.contains("must declare license = \"OFL-1.1\""),
        "{message}"
    );
}

#[test]
fn a_malformed_spec_is_refused() {
    for (old, new, needle) in [
        ("[family]", "[family]\nextra = 1", "unknown field"),
        (
            "sizes = [14, 16, 18, 24, 28, 32, 36, 48]",
            "sizes = [24, 18]",
            "increasing order",
        ),
        (
            "[0xA0, 0xAC]",
            "[0x70, 0xAC]",
            "must start after the one before",
        ),
        (
            "path = \"OFL.txt\"",
            "path = \"../OFL.txt\"",
            "not a relative path",
        ),
        (
            "file = \"fonts/",
            "file = \"fonts/Other-",
            "must be a pinned file in fonts/",
        ),
        // A face's own set holds only characters of the whole set.
        (
            "[0xA3, 0xA3]]",
            "[0x2014, 0x2014]]",
            "hold U+2014, which is not in [characters]",
        ),
    ] {
        let dir = inputs("malformed");
        edit(&dir, "glyphs.toml", old, new);
        let message = generate(&dir).err().unwrap().to_string();
        assert!(message.contains(needle), "{needle}: {message}");
    }
}

#[test]
fn the_output_folder_holds_only_what_the_tool_writes() {
    let dir = inputs("out-folder");
    let out = dir.join("out");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("notes.txt"), "not ours").unwrap();
    let message = run_on(&dir, &out).unwrap_err().to_string();
    assert!(
        message.contains("holds notes.txt, which the glyph atlas tool did not write"),
        "{message}"
    );
    assert!(!out.join("mod.rs").exists(), "nothing is written");

    // What the tool wrote before and no longer makes goes.
    fs::remove_file(out.join("notes.txt")).unwrap();
    fs::write(out.join("regular_99.rs"), "// an atlas no longer made").unwrap();
    let summary = run_on(&dir, &out).unwrap();
    assert!(summary.contains("written to"), "{summary}");
    assert!(!out.join("regular_99.rs").exists());
    assert_eq!(fs::read_dir(&out).unwrap().count(), 52);
}

#[test]
fn the_command_line_is_checked() {
    for (args, needle) in [
        (vec![], "name the atlas crate's folder"),
        (vec!["--out"], "--out needs a folder"),
        (vec!["--bogus", "x"], "unknown option --bogus"),
        (vec!["a", "b"], "one crate only"),
    ] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        let error = run(&args).unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{error}");
        assert!(error.to_string().contains(needle), "{error}");
        assert_eq!(error.exit_code(), 2);
    }
    assert!(
        run(&["--help".into()])
            .unwrap()
            .starts_with("usage: glyph-atlas")
    );
}
