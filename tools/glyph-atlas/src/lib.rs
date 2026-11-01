// SPDX-License-Identifier: MIT OR Apache-2.0
//! NoWayOS's glyph atlas generator: the build-time tool behind
//! `make atlases`.
//!
//! It reads an atlas crate's `glyphs.toml` - the font files with their
//! versions and sha256 pins, the faces and their sizes, the character set
//! and its aliases - checks every file against its pin and refuses any file
//! in `fonts/` that is not pinned, then rasterises every character of the
//! set at every size of every face with `swash`: unhinted, 8-bit
//! coverage, metrics and GPOS pair kerning in 1/64 pixel, ligatures off. It
//! writes the generated Rust modules, their coverage files and a manifest.
//!
//! Everything is made in memory first: if any check fails - a pin, a
//! character the font lacks - nothing is written. The output depends on the
//! inputs alone (no time, path or host in it), so two runs, on the host or
//! in the dev container, give the same bytes.

use std::fmt;
use std::path::{Path, PathBuf};

mod emit;
mod raster;
mod spec;

pub use emit::{char_literal, rust_string, toml_string};
pub use raster::to_64;
pub use spec::{FONTS_DIR, SPEC_FILE};

/// What can stop the tool. Each names the file, or the character, at fault.
#[derive(Debug)]
pub enum Error {
    /// The command line was not understood.
    Usage(String),
    /// A file or folder could not be read or written: what was being done,
    /// and why it failed.
    Io {
        /// What the tool was doing, naming the path.
        what: String,
        /// The operating system's error.
        error: std::io::Error,
    },
    /// `glyphs.toml`, or the crate's `Cargo.toml`, is malformed or
    /// inconsistent.
    Spec(String),
    /// Files that do not match `glyphs.toml`: one line each, naming the
    /// file.
    Pins(Vec<String>),
    /// A font file the tool cannot use as `glyphs.toml` says.
    Font {
        /// The font file, as `glyphs.toml` names it.
        file: String,
        /// What is wrong with it.
        message: String,
    },
    /// Characters of the set the font lacks: one line each, naming the
    /// character.
    MissingGlyphs {
        /// The font file, as `glyphs.toml` names it.
        file: String,
        /// Each character the font lacks.
        missing: Vec<String>,
    },
    /// Something the atlas format cannot hold.
    Format {
        /// The font file, as `glyphs.toml` names it.
        file: String,
        /// What does not fit.
        message: String,
    },
    /// The output folder holds something the tool did not write.
    OutDir(String),
}

impl Error {
    /// The process exit status for this error: 2 for a command line not
    /// understood, 1 for everything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}\n{USAGE}"),
            Self::Io { what, error } => write!(f, "cannot {what}: {error}"),
            Self::Spec(message) | Self::OutDir(message) => f.write_str(message),
            Self::Pins(problems) => {
                write!(
                    f,
                    "the font files do not match {SPEC_FILE}, so nothing was written:"
                )?;
                problems
                    .iter()
                    .try_for_each(|line| write!(f, "\n    {line}"))
            }
            Self::Font { file, message } | Self::Format { file, message } => {
                write!(f, "{file}: {message}")
            }
            Self::MissingGlyphs { file, missing } => {
                write!(
                    f,
                    "{file} has no glyph for {} character(s) of the set, so nothing was written:",
                    missing.len()
                )?;
                missing
                    .iter()
                    .try_for_each(|line| write!(f, "\n    {line}"))
            }
        }
    }
}

impl std::error::Error for Error {}

/// How to run the tool.
pub const USAGE: &str = "usage: glyph-atlas [--out DIR] CRATE\n\
    \n\
    Reads CRATE/glyphs.toml and the font files it pins, and writes the glyph atlases into DIR\n\
    (default: CRATE/src/atlas), replacing what the tool wrote there before.";

/// The generated files, in memory: (file name, contents), sorted by name.
pub struct Atlases {
    /// Every file the tool writes, sorted by name.
    pub files: Vec<(String, Vec<u8>)>,
    /// One line saying what was made.
    pub summary: String,
}

/// Makes the atlases of the crate in `crate_dir`, in memory.
///
/// # Errors
///
/// Any [`Error`] but [`Error::Usage`] and [`Error::OutDir`]: nothing is
/// written in any case.
pub fn generate(crate_dir: &Path) -> Result<Atlases, Error> {
    let spec = spec::Spec::load(crate_dir)?;
    let inputs = spec::check_pins(crate_dir, &spec)?;
    let mut faces = Vec::with_capacity(spec.faces.len());
    for (face, (chars, _)) in spec.faces.iter().zip(&spec.face_sets) {
        let pinned = spec.file(&face.file)?;
        let data = inputs
            .get(&face.file)
            .ok_or_else(|| Error::Spec(format!("{} is not read", face.file)))?;
        let atlas =
            raster::rasterise(&face.file, data, &pinned.version, &spec, chars, &face.sizes)?;
        faces.push((face, pinned, atlas));
    }
    emit::files(&spec, &faces)
}

/// Writes `atlases` into `out`, creating it if need be: each file whose
/// contents differ, and removes any file the tool wrote before that it no
/// longer makes.
///
/// # Errors
///
/// [`Error::OutDir`] before anything is changed if `out` holds a folder,
/// or a file the tool would not have written (only `mod.rs`,
/// `manifest.toml`, `*.rs` and `*.coverage`); [`Error::Io`] if writing
/// fails.
pub fn write(atlases: &Atlases, out: &Path) -> Result<(), Error> {
    let io = |what: String| move |error| Error::Io { what, error };
    std::fs::create_dir_all(out).map_err(io(format!("create {}", out.display())))?;
    let mut stale = Vec::new();
    let entries = std::fs::read_dir(out).map_err(io(format!("read {}", out.display())))?;
    for entry in entries {
        let entry = entry.map_err(io(format!("read {}", out.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .map_err(io(format!("read {}", entry.path().display())))?;
        let ours = kind.is_file()
            && (name == "manifest.toml"
                || Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext == "rs" || ext == "coverage"));
        if !ours {
            return Err(Error::OutDir(format!(
                "{} holds {name}, which the glyph atlas tool did not write: refusing to write \
                 there (name an empty folder or the atlas crate's src/atlas)",
                out.display()
            )));
        }
        if !atlases.files.iter().any(|(file, _)| *file == name) {
            stale.push(entry.path());
        }
    }
    for path in stale {
        std::fs::remove_file(&path).map_err(io(format!("remove {}", path.display())))?;
    }
    for (name, contents) in &atlases.files {
        let path = out.join(name);
        if std::fs::read(&path).is_ok_and(|old| old == *contents) {
            continue;
        }
        std::fs::write(&path, contents).map_err(io(format!("write {}", path.display())))?;
    }
    Ok(())
}

/// Runs the command line `args` (without the program name): makes the
/// atlases and writes them. Returns the summary to print.
///
/// # Errors
///
/// [`Error::Usage`] for arguments not understood; otherwise as
/// [`generate`] and [`write`].
pub fn run(args: &[String]) -> Result<String, Error> {
    let mut out: Option<PathBuf> = None;
    let mut crate_dir: Option<PathBuf> = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--out" => match rest.next() {
                Some(dir) if out.is_none() => out = Some(PathBuf::from(dir)),
                Some(_) => return Err(Error::Usage("--out is given twice".into())),
                None => return Err(Error::Usage("--out needs a folder".into())),
            },
            "-h" | "--help" => return Ok(USAGE.into()),
            other if other.starts_with('-') => {
                return Err(Error::Usage(format!("unknown option {other}")));
            }
            other if crate_dir.is_none() => crate_dir = Some(PathBuf::from(other)),
            other => return Err(Error::Usage(format!("one crate only: {other} is a second"))),
        }
    }
    let crate_dir =
        crate_dir.ok_or_else(|| Error::Usage("name the atlas crate's folder".into()))?;
    let out = out.unwrap_or_else(|| crate_dir.join("src").join("atlas"));
    let atlases = generate(&crate_dir)?;
    write(&atlases, &out)?;
    Ok(format!("{}; written to {}", atlases.summary, out.display()))
}
