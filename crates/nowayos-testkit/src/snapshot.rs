// SPDX-License-Identifier: MIT OR Apache-2.0
//! Golden-image snapshots.
//!
//! A snapshot test renders a frame and compares it pixel for pixel with a
//! PNG file checked into the repository. When a change to the rendering is
//! intended, regenerate the golden files and review the new images like any
//! other change (a golden file whose pixels are unchanged is left as it is):
//!
//! ```text
//! NOWAYOS_UPDATE_SNAPSHOTS=1 make test
//! ```
//!
//! On a mismatch the rendered frame is written beside the golden file as
//! `NAME.actual.png` (ignored by git) so the two can be compared by eye.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Diff, Framebuffer, PngError};

/// Set this environment variable (to anything but `0` or empty) to write
/// golden files instead of comparing against them.
pub const UPDATE_ENV: &str = "NOWAYOS_UPDATE_SNAPSHOTS";

/// Why a snapshot check failed.
#[derive(Debug)]
pub enum SnapshotError {
    /// There is no golden file yet.
    Missing {
        /// The golden file expected.
        golden: PathBuf,
    },
    /// Reading or writing a file failed.
    Io {
        /// The file involved.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// The golden file is not an acceptable PNG.
    Corrupt {
        /// The golden file.
        golden: PathBuf,
        /// What is wrong with it.
        error: PngError,
    },
    /// The frame could not be encoded, to write it as a file.
    Encode {
        /// The file it was for.
        path: PathBuf,
        /// Why not.
        error: PngError,
    },
    /// The frame differs from the golden image.
    Mismatch {
        /// The golden file.
        golden: PathBuf,
        /// Where the rendered frame was written for comparison.
        actual: PathBuf,
        /// How they differ.
        diff: Diff,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { golden } => write!(
                f,
                "no golden image at {}; create it with {UPDATE_ENV}=1 and review it",
                golden.display()
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Corrupt { golden, error } => write!(f, "{}: {error}", golden.display()),
            Self::Encode { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Mismatch {
                golden,
                actual,
                diff,
            } => write!(
                f,
                "frame differs from {} ({diff}); rendered frame written to {}. If the change \
                 is intended, rerun with {UPDATE_ENV}=1 and review the new image",
                golden.display(),
                actual.display()
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Whether to compare with the golden file or (re)write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Compare; fail on any difference.
    Compare,
    /// Write the golden file from this frame.
    Update,
}

impl Mode {
    /// [`Mode::Update`] if [`UPDATE_ENV`] is set to anything but `0` or
    /// empty, otherwise [`Mode::Compare`].
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var_os(UPDATE_ENV) {
            Some(value) if !value.is_empty() && value != "0" => Self::Update,
            _ => Self::Compare,
        }
    }
}

/// Checks `frame` against the golden PNG at `golden`, or rewrites the golden
/// file when [`UPDATE_ENV`] is set and its pixels differ.
///
/// # Errors
///
/// A [`SnapshotError`] describing what is missing or different.
pub fn check_snapshot(frame: &Framebuffer, golden: &Path) -> Result<(), SnapshotError> {
    check_snapshot_with(frame, golden, Mode::from_env())
}

/// [`check_snapshot`] with an explicit [`Mode`].
///
/// # Errors
///
/// A [`SnapshotError`] describing what is missing or different.
pub fn check_snapshot_with(
    frame: &Framebuffer,
    golden: &Path,
    mode: Mode,
) -> Result<(), SnapshotError> {
    let expected = match std::fs::read(golden) {
        Ok(bytes) => Framebuffer::from_png(&bytes).map_err(|error| SnapshotError::Corrupt {
            golden: golden.to_path_buf(),
            error,
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(SnapshotError::Missing {
            golden: golden.to_path_buf(),
        }),
        Err(source) => Err(SnapshotError::Io {
            path: golden.to_path_buf(),
            source,
        }),
    };
    if mode == Mode::Update {
        // Unchanged pixels leave the file as it is, whichever encoder wrote it.
        if expected.is_ok_and(|expected| expected.diff(frame).is_none()) {
            return Ok(());
        }
        if let Some(dir) = golden.parent() {
            std::fs::create_dir_all(dir).map_err(|source| SnapshotError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        return write_png(frame, golden);
    }
    match expected?.diff(frame) {
        None => Ok(()),
        Some(diff) => {
            let actual = golden.with_extension("actual.png");
            write_png(frame, &actual)?;
            Err(SnapshotError::Mismatch {
                golden: golden.to_path_buf(),
                actual,
                diff,
            })
        }
    }
}

/// Writes `frame` to `path` as a PNG.
fn write_png(frame: &Framebuffer, path: &Path) -> Result<(), SnapshotError> {
    let bytes = frame.to_png().map_err(|error| SnapshotError::Encode {
        path: path.to_path_buf(),
        error,
    })?;
    std::fs::write(path, bytes).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use embedded_graphics::pixelcolor::{Rgb888, RgbColor};

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("nowayos-testkit-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_then_update_then_match_then_mismatch() {
        let scratch = Scratch::new("cycle");
        let golden = scratch.0.join("shot.png");
        let frame = Framebuffer::filled(4, 3, Rgb888::BLUE).unwrap();

        match check_snapshot_with(&frame, &golden, Mode::Compare) {
            Err(SnapshotError::Missing { .. }) => {}
            other => panic!("expected Missing, got {other:?}"),
        }
        check_snapshot_with(&frame, &golden, Mode::Update).unwrap();
        check_snapshot_with(&frame, &golden, Mode::Compare).unwrap();

        let mut changed = frame.clone();
        changed.set(3, 2, Rgb888::RED);
        match check_snapshot_with(&changed, &golden, Mode::Compare) {
            Err(SnapshotError::Mismatch { actual, diff, .. }) => {
                assert_eq!(diff.differing, 1);
                assert_eq!(diff.first, Some((3, 2)));
                assert_eq!(actual, scratch.0.join("shot.actual.png"));
                let written = Framebuffer::from_png(&std::fs::read(&actual).unwrap()).unwrap();
                assert_eq!(written, changed);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn update_leaves_unchanged_pixels_alone_and_mends_a_corrupt_file() {
        let scratch = Scratch::new("unchanged");
        let golden = scratch.0.join("shot.png");
        let frame = Framebuffer::filled(4, 3, Rgb888::GREEN).unwrap();
        // The same pixels, in a file the testkit would not write byte for
        // byte: an empty private chunk, which readers skip, after the header.
        let mut other = frame.to_png().unwrap();
        other.splice(33..33, *b"\0\0\0\0noTe\x5a\x39\xde\xb6");
        std::fs::write(&golden, &other).unwrap();
        check_snapshot_with(&frame, &golden, Mode::Compare).unwrap();
        check_snapshot_with(&frame, &golden, Mode::Update).unwrap();
        assert_eq!(std::fs::read(&golden).unwrap(), other);

        std::fs::write(&golden, b"not an image").unwrap();
        check_snapshot_with(&frame, &golden, Mode::Update).unwrap();
        assert_eq!(std::fs::read(&golden).unwrap(), frame.to_png().unwrap());
    }

    #[test]
    fn corrupt_golden_is_reported() {
        let scratch = Scratch::new("corrupt");
        let golden = scratch.0.join("bad.png");
        std::fs::write(&golden, b"not an image").unwrap();
        let frame = Framebuffer::new(1, 1).unwrap();
        match check_snapshot_with(&frame, &golden, Mode::Compare) {
            Err(SnapshotError::Corrupt {
                error: PngError::Malformed(_),
                ..
            }) => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }
}
