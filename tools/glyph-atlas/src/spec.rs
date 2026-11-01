// SPDX-License-Identifier: MIT OR Apache-2.0
//! `glyphs.toml`, the atlases' input, and the pin check: every file it
//! lists matches its sha256, and every file in `fonts/` is listed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::Error;

/// The input file, in the atlas crate's folder.
pub const SPEC_FILE: &str = "glyphs.toml";
/// The folder of font files in the atlas crate: every file in it must be
/// pinned in [`SPEC_FILE`].
pub const FONTS_DIR: &str = "fonts";
/// The largest `glyphs.toml` or `Cargo.toml` read, in bytes.
const MAX_SPEC_BYTES: u64 = 64 * 1024;
/// The largest pinned file read, in bytes: the signed-off font's Regular
/// file is 65,068.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The most characters a set may hold: kerning shapes every ordered pair.
pub const MAX_CHARS: usize = 1024;
/// The largest size, in pixels per em.
pub const MAX_SIZE: u16 = 256;
/// How deep `fonts/` may nest, and how many entries it may hold.
const MAX_DEPTH: usize = 4;
const MAX_ENTRIES: usize = 256;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecFile {
    family: Family,
    #[serde(rename = "file")]
    files: Vec<PinnedFile>,
    #[serde(rename = "face")]
    faces: Vec<Face>,
    characters: Characters,
    #[serde(rename = "alias", default)]
    aliases: Vec<Alias>,
}

/// The font family: its name, licence and where it comes from.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Family {
    /// The family's name, for the notice and the manifest only: an atlas is
    /// named for its job, never for its font.
    pub name: String,
    /// Its licence, an SPDX identifier, which the atlas crate must declare.
    pub licence: String,
    /// The pinned file holding the licence and the copyright line.
    pub licence_file: String,
    /// The copyright line, as the licence file gives it.
    pub copyright: String,
    /// The repository the files come from.
    pub repository: String,
    /// The commit they come from.
    pub commit: String,
    /// The decision that signed the family off.
    pub decision: String,
}

/// A file of the family, pinned.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedFile {
    /// Where it is in the atlas crate.
    pub path: String,
    /// Where it is in the repository at the commit.
    pub upstream: String,
    /// The family's version it belongs to.
    pub version: String,
    /// Its sha256, in lowercase hexadecimal.
    pub sha256: String,
}

/// One face: a font file, at each of its sizes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Face {
    /// The face's name in the atlases' module and static names: `regular`
    /// makes `regular_24` and `REGULAR_24`.
    pub name: String,
    /// Its style, as the family names it (Regular, Bold).
    pub style: String,
    /// Its font file, a pinned file in `fonts/`.
    pub file: String,
    /// Its sizes, in pixels per em, in increasing order.
    pub sizes: Vec<u16>,
    /// Its own character set, as ranges, if it draws only part of the
    /// whole set - a display face's digits and capitals: every
    /// character must be one of `[characters]`. The whole set if absent.
    #[serde(default)]
    pub ranges: Option<Vec<[u32; 2]>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Characters {
    ranges: Vec<[u32; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Alias {
    char: u32,
    glyph_of: u32,
}

/// `glyphs.toml`, checked, with the atlas crate's name.
pub struct Spec {
    /// The atlas crate's name, from its `Cargo.toml`.
    pub crate_name: String,
    /// The font family.
    pub family: Family,
    /// Every pinned file.
    pub files: Vec<PinnedFile>,
    /// The faces.
    pub faces: Vec<Face>,
    /// The character set, in increasing order.
    pub chars: Vec<char>,
    /// The same set, as inclusive ranges.
    pub ranges: Vec<(char, char)>,
    /// Each aliased character, and the character whose glyph draws it.
    pub aliases: BTreeMap<char, char>,
    /// Each face's character set, in the order of `faces`: its own
    /// ranges', or the whole set's; in increasing order, with its ranges.
    pub face_sets: Vec<CharacterSet>,
}

impl Spec {
    /// Reads and checks `crate_dir/glyphs.toml` and the crate's
    /// `Cargo.toml`, which must declare the family's licence.
    pub fn load(crate_dir: &Path) -> Result<Self, Error> {
        let spec_text = read_text(crate_dir, SPEC_FILE)?;
        let file: SpecFile = toml::from_str(&spec_text)
            .map_err(|error| Error::Spec(format!("{SPEC_FILE} is not valid: {error}")))?;
        let manifest = read_text(crate_dir, "Cargo.toml")?;
        let manifest: toml::Table = toml::from_str(&manifest)
            .map_err(|error| Error::Spec(format!("Cargo.toml is not valid: {error}")))?;
        let package = manifest.get("package").and_then(toml::Value::as_table);
        let field = |key: &str| {
            package
                .and_then(|p| p.get(key))
                .and_then(toml::Value::as_str)
        };
        let crate_name = field("name")
            .ok_or_else(|| Error::Spec("Cargo.toml names no package".into()))?
            .to_owned();
        if field("license") != Some(file.family.licence.as_str()) {
            return Err(Error::Spec(format!(
                "the atlases are under {}, as {SPEC_FILE} says, so Cargo.toml must declare \
                 license = \"{}\" (it declares {:?})",
                file.family.licence,
                file.family.licence,
                field("license")
            )));
        }
        check_family(&file.family)?;
        check_files(&file.files, &file.family)?;
        check_faces(&file.faces, &file.files)?;
        let (chars, ranges) = character_set(&file.characters)?;
        let aliases = aliases(&file.aliases, &chars)?;
        let mut face_sets = Vec::with_capacity(file.faces.len());
        for face in &file.faces {
            face_sets.push(match &face.ranges {
                None => (chars.clone(), ranges.clone()),
                Some(own) => {
                    let what = format!("face {}'s ranges", face.name);
                    let (subset, subset_ranges) = character_set(&Characters {
                        ranges: own.clone(),
                    })
                    .map_err(|_| {
                        Error::Spec(format!(
                            "{SPEC_FILE}: {what} must be valid, increasing ranges"
                        ))
                    })?;
                    if let Some(outside) = subset.iter().find(|ch| chars.binary_search(ch).is_err())
                    {
                        return Err(Error::Spec(format!(
                            "{SPEC_FILE}: {what} hold U+{:04X}, which is not in [characters]",
                            u32::from(*outside)
                        )));
                    }
                    (subset, subset_ranges)
                }
            });
        }
        Ok(Self {
            crate_name,
            family: file.family,
            files: file.files,
            faces: file.faces,
            chars,
            ranges,
            aliases,
            face_sets,
        })
    }

    /// The pinned file at `path`.
    pub fn file(&self, path: &str) -> Result<&PinnedFile, Error> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .ok_or_else(|| Error::Spec(format!("{path} is not pinned in {SPEC_FILE}")))
    }
}

/// Checks every pinned file against its sha256, and that every file in
/// `fonts/` is pinned. Returns each pinned file's contents, by path.
pub fn check_pins(crate_dir: &Path, spec: &Spec) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut problems = Vec::new();
    let mut contents = BTreeMap::new();
    for file in &spec.files {
        match read_capped(&crate_dir.join(&file.path), MAX_FILE_BYTES, &file.path) {
            Ok(Some(bytes)) => {
                let actual = sha256_hex(&bytes);
                if actual == file.sha256 {
                    contents.insert(file.path.clone(), bytes);
                } else {
                    problems.push(format!(
                        "{}: its sha256 is {actual}, but {SPEC_FILE} pins {}",
                        file.path, file.sha256
                    ));
                }
            }
            Ok(None) => problems.push(format!(
                "{} is pinned in {SPEC_FILE} but missing",
                file.path
            )),
            Err(error) => return Err(error),
        }
    }
    let pinned: BTreeSet<&str> = spec.files.iter().map(|file| file.path.as_str()).collect();
    let mut found = Vec::new();
    list_files(&crate_dir.join(FONTS_DIR), FONTS_DIR, 0, &mut found)?;
    for path in found {
        if !pinned.contains(path.as_str()) {
            problems.push(format!(
                "{path} is in {FONTS_DIR}/ but not pinned in {SPEC_FILE}: every font file there \
                 is listed with its version and sha256, once it is signed off"
            ));
        }
    }
    if problems.is_empty() {
        Ok(contents)
    } else {
        Err(Error::Pins(problems))
    }
}

/// The sha256 of `bytes`, in lowercase hexadecimal.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Every file under `dir` (shown as `shown`), recursively, in sorted order.
fn list_files(dir: &Path, shown: &str, depth: usize, found: &mut Vec<String>) -> Result<(), Error> {
    if depth > MAX_DEPTH {
        return Err(Error::Spec(format!(
            "{shown}/ nests deeper than {MAX_DEPTH} folders"
        )));
    }
    let io = |error| Error::Io {
        what: format!("read {shown}/"),
        error,
    };
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let kind = entry.file_type().map_err(io)?;
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            kind.is_dir(),
        ));
    }
    entries.sort();
    for (name, is_dir) in entries {
        if found.len() >= MAX_ENTRIES {
            return Err(Error::Spec(format!(
                "{FONTS_DIR}/ holds more than {MAX_ENTRIES} files"
            )));
        }
        let path = format!("{shown}/{name}");
        if is_dir {
            list_files(&dir.join(&name), &path, depth.saturating_add(1), found)?;
        } else {
            found.push(path);
        }
    }
    Ok(())
}

/// The UTF-8 text of `crate_dir/name`.
fn read_text(crate_dir: &Path, name: &str) -> Result<String, Error> {
    let bytes = read_capped(&crate_dir.join(name), MAX_SPEC_BYTES, name)?
        .ok_or_else(|| Error::Spec(format!("{} has no {name}", crate_dir.display())))?;
    String::from_utf8(bytes).map_err(|_| Error::Spec(format!("{name} is not UTF-8")))
}

/// The contents of `path`, at most `cap` bytes, or `None` if it does not
/// exist; refused, before it is read, if it is larger.
fn read_capped(path: &Path, cap: u64, shown: &str) -> Result<Option<Vec<u8>>, Error> {
    let io = |error| Error::Io {
        what: format!("read {shown}"),
        error,
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let too_big = || {
        Error::Spec(format!(
            "{shown} is larger than the {cap} bytes the tool reads"
        ))
    };
    if file.metadata().map_err(io)?.len() > cap {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > cap) {
        return Err(too_big());
    }
    Ok(Some(bytes))
}

/// One line of printable text: no control character.
fn check_line(what: &str, value: &str) -> Result<(), Error> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: {what} must be one line of text, not {value:?}"
        )));
    }
    Ok(())
}

/// A relative path inside the crate: `/`-separated names of letters,
/// digits, `.`, `-` and `_`, none of them `.` or `..`.
fn check_path(what: &str, path: &str) -> Result<(), Error> {
    let good = !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        });
    if good {
        Ok(())
    } else {
        Err(Error::Spec(format!(
            "{SPEC_FILE}: {what} {path:?} is not a relative path inside the crate"
        )))
    }
}

fn check_family(family: &Family) -> Result<(), Error> {
    for (what, value) in [
        ("[family] name", &family.name),
        ("[family] licence", &family.licence),
        ("[family] copyright", &family.copyright),
        ("[family] repository", &family.repository),
        ("[family] commit", &family.commit),
        ("[family] decision", &family.decision),
    ] {
        check_line(what, value)?;
    }
    check_path("[family] licence-file", &family.licence_file)
}

fn check_files(files: &[PinnedFile], family: &Family) -> Result<(), Error> {
    let mut paths = BTreeSet::new();
    for file in files {
        check_path("a [[file]] path", &file.path)?;
        check_path("a [[file]] upstream path", &file.upstream)?;
        check_line("a [[file]] version", &file.version)?;
        if file.sha256.len() != 64
            || !file
                .sha256
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: {}'s sha256 must be 64 lowercase hexadecimal digits",
                file.path
            )));
        }
        if !paths.insert(file.path.as_str()) {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: {} is pinned twice",
                file.path
            )));
        }
    }
    if !paths.contains(family.licence_file.as_str()) {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: the licence file {} must be pinned, like the fonts",
            family.licence_file
        )));
    }
    Ok(())
}

fn check_faces(faces: &[Face], files: &[PinnedFile]) -> Result<(), Error> {
    if faces.is_empty() {
        return Err(Error::Spec(format!("{SPEC_FILE} names no [[face]]")));
    }
    let mut names = BTreeSet::new();
    for face in faces {
        let mut chars = face.name.chars();
        let good = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !good || !names.insert(face.name.as_str()) {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: face {:?} needs a name of its own, of lowercase letters and digits",
                face.name
            )));
        }
        check_line("a [[face]] style", &face.style)?;
        let in_fonts = face
            .file
            .strip_prefix(FONTS_DIR)
            .is_some_and(|rest| rest.starts_with('/'));
        if !in_fonts || !files.iter().any(|file| file.path == face.file) {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: face {}'s file {} must be a pinned file in {FONTS_DIR}/",
                face.name, face.file
            )));
        }
        let increasing = face
            .sizes
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a < b));
        let in_range = face.sizes.iter().all(|size| (1..=MAX_SIZE).contains(size));
        if face.sizes.is_empty() || !increasing || !in_range {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: face {}'s sizes must be 1 to {MAX_SIZE} pixels per em, in \
                 increasing order",
                face.name
            )));
        }
    }
    Ok(())
}

/// A character set: its characters in increasing order, and its ranges.
pub type CharacterSet = (Vec<char>, Vec<(char, char)>);

/// The set from its ranges: valid, increasing, apart and not too many.
fn character_set(characters: &Characters) -> Result<CharacterSet, Error> {
    let bad = |message: String| Error::Spec(format!("{SPEC_FILE}: [characters] {message}"));
    if characters.ranges.is_empty() {
        return Err(bad("names no range".into()));
    }
    let mut chars = Vec::new();
    let mut ranges = Vec::new();
    let mut after: Option<u32> = None;
    for &[first, last] in &characters.ranges {
        let (Some(start), Some(end)) = (char::from_u32(first), char::from_u32(last)) else {
            return Err(bad(format!(
                "range {first:#X}-{last:#X} is not Unicode scalar values"
            )));
        };
        if first > last || after.is_some_and(|previous| first <= previous) {
            return Err(bad(format!(
                "range {first:#X}-{last:#X} must start after the one before and end after it starts"
            )));
        }
        for ch in start..=end {
            if ch.is_control() {
                return Err(bad(format!(
                    "holds the control character U+{:04X}",
                    u32::from(ch)
                )));
            }
            if chars.len() >= MAX_CHARS {
                return Err(bad(format!("holds more than {MAX_CHARS} characters")));
            }
            chars.push(ch);
        }
        ranges.push((start, end));
        after = Some(last);
    }
    Ok((chars, ranges))
}

fn aliases(entries: &[Alias], chars: &[char]) -> Result<BTreeMap<char, char>, Error> {
    let mut aliases = BTreeMap::new();
    for alias in entries {
        let (Some(ch), Some(of)) = (char::from_u32(alias.char), char::from_u32(alias.glyph_of))
        else {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: alias {:#X} -> {:#X} is not Unicode scalar values",
                alias.char, alias.glyph_of
            )));
        };
        if ch == of || chars.binary_search(&ch).is_err() || aliases.insert(ch, of).is_some() {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: an alias is for a character of the set, once, drawn with another's \
                 glyph: not U+{:04X} -> U+{:04X}",
                alias.char, alias.glyph_of
            )));
        }
    }
    Ok(aliases)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failed check panics")]
mod tests {
    use super::*;

    fn set(ranges: &[[u32; 2]]) -> Result<CharacterSet, Error> {
        character_set(&Characters {
            ranges: ranges.to_vec(),
        })
    }

    #[test]
    fn ranges_make_the_set() {
        let (chars, ranges) = set(&[[0x20, 0x7E], [0xA0, 0xAC], [0xAE, 0xFF]]).unwrap();
        assert_eq!(chars.len(), 190);
        assert!(chars.windows(2).all(|pair| matches!(pair, [a, b] if a < b)));
        assert_eq!(ranges.first(), Some(&(' ', '~')));
        assert!(!chars.contains(&'\u{AD}'));
    }

    #[test]
    fn a_faces_ranges_are_its_set() {
        let (chars, ranges) = set(&[[0x30, 0x39], [0x41, 0x5A]]).unwrap();
        assert_eq!(chars.len(), 36);
        assert_eq!(ranges, [('0', '9'), ('A', 'Z')]);
    }

    #[test]
    fn bad_ranges_are_refused() {
        for ranges in [
            &[][..],
            &[[0x41, 0x40]],
            &[[0x40, 0x50], [0x50, 0x60]],
            &[[0x60, 0x70], [0x40, 0x50]],
            &[[0x0A, 0x41]],
            &[[0xD800, 0xD801]],
            &[[0x110000, 0x110001]],
            &[[0x100, 0x100 + 2000]],
        ] {
            assert!(matches!(set(ranges), Err(Error::Spec(_))), "{ranges:?}");
        }
    }

    #[test]
    fn paths_stay_inside_the_crate() {
        for good in ["OFL.txt", "fonts/Some-Font_1.ttf"] {
            assert!(check_path("p", good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "fonts/../x",
            "fonts//x",
            "a\\b",
            "./x",
            "a b",
        ] {
            assert!(check_path("p", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_alias_is_for_a_character_of_the_set() {
        let chars = ['a', 'b'];
        let alias = |char, glyph_of| Alias { char, glyph_of };
        assert_eq!(
            aliases(&[alias(0x61, 0x3BC)], &chars).unwrap().get(&'a'),
            Some(&'\u{3BC}')
        );
        for bad in [
            vec![alias(0x63, 0x3BC)],
            vec![alias(0x61, 0x61)],
            vec![alias(0x61, 0x62), alias(0x61, 0x3BC)],
            vec![alias(0x61, 0xD800)],
        ] {
            assert!(aliases(&bad, &chars).is_err());
        }
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
