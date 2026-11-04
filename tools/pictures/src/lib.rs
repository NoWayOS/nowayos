// SPDX-License-Identifier: MIT OR Apache-2.0
//! NoWayOS's picture converter: the build-time tool behind the pictures
//! `make atlases` makes.
//!
//! It reads a crate's `pictures.toml` - the module folder to write, the
//! device scales the crate's pictures are made at, and each picture: its
//! name, its PNG or JPEG file with that file's sha256, and its width in
//! layout units (its height too, or the file's shape gives it) - checks
//! every file against its pin and refuses any file in the pictures folder
//! that is not pinned. It decodes each file to 8-bit RGBA, premultiplies
//! it by its alpha, and for each scale resamples it to the picture's size
//! at that scale, each pixel the exact area-weighted mean of the pixels it
//! covers, in integers; a file smaller than a scale needs is refused, as
//! enlarging it would blur it. It writes each picture's pixels at each
//! scale (`NAME_Q.rgba`), the module of statics naming them (`mod.rs`) and
//! the list of names (`names`, one a line, which Screen Compiler checks
//! `<img src>` against).
//!
//! Colours are taken as sRGB as stored; a colour profile in the file is not
//! applied. Everything is made in memory first: if any check fails nothing
//! is written. The output depends on the inputs alone, so two runs, on the
//! host or in the dev container, give the same bytes.

use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The input file, in the crate's folder.
pub const SPEC_FILE: &str = "pictures.toml";
/// The folder of picture files in the crate: every file in it must be
/// pinned in [`SPEC_FILE`].
pub const PICTURES_DIR: &str = "pictures";
/// The largest `pictures.toml` read, in bytes.
const MAX_SPEC_BYTES: u64 = 64 * 1024;
/// The largest picture file read, in bytes.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The largest side of a decoded picture, in pixels.
const MAX_SIDE: u32 = 8192;
/// The largest side of a picture, in layout units: its pixels at scale 4
/// must fit a `u16`.
const MAX_UNITS: u32 = 4096;
/// The most pictures one crate may pin.
const MAX_PICTURES: usize = 256;

/// What can stop the tool. Each names the file at fault.
#[derive(Debug)]
pub enum Error {
    /// The command line was not understood.
    Usage(String),
    /// A file or folder could not be read or written.
    Io {
        /// What the tool was doing, naming the path.
        what: String,
        /// The operating system's error.
        error: std::io::Error,
    },
    /// `pictures.toml` is malformed or inconsistent.
    Spec(String),
    /// Files that do not match `pictures.toml`: one line each.
    Pins(Vec<String>),
    /// A picture file the tool cannot use as `pictures.toml` says.
    Picture {
        /// The file, as `pictures.toml` names it.
        file: String,
        /// What is wrong with it.
        message: String,
    },
    /// The output folder holds something the tool did not write.
    OutDir(String),
}

impl Error {
    /// The process exit status: 2 for a command line not understood, 1 for
    /// everything else.
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
                    "the picture files do not match {SPEC_FILE}, so nothing was written:"
                )?;
                problems
                    .iter()
                    .try_for_each(|line| write!(f, "\n    {line}"))
            }
            Self::Picture { file, message } => write!(f, "{file}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// How to run the tool.
pub const USAGE: &str = "usage: pictures [--out DIR] CRATE";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecFile {
    module: String,
    scales: Vec<u8>,
    #[serde(rename = "picture")]
    pictures: Vec<PictureSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PictureSpec {
    name: String,
    file: String,
    sha256: String,
    width: u32,
    height: Option<u32>,
}

/// A picture decoded: 8-bit RGBA, premultiplied, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
    /// Its pixels: premultiplied red, green, blue and alpha.
    pub rgba: Vec<u8>,
}

/// Runs the tool on its arguments (those after the program's name), and
/// says what it wrote.
///
/// # Errors
///
/// An [`Error`] naming what stopped it; nothing is written then.
pub fn run(args: &[String]) -> Result<String, Error> {
    let (out, krate) = match args {
        [krate] if !krate.starts_with('-') => (None, PathBuf::from(krate)),
        [flag, out, krate] if flag == "--out" => (Some(PathBuf::from(out)), PathBuf::from(krate)),
        _ => return Err(Error::Usage("expected a crate folder".to_owned())),
    };
    let spec = read_spec(&krate)?;
    let out = out.unwrap_or_else(|| krate.join(&spec.module));
    let files = convert(&krate, &spec)?;
    write_out(&out, &files)?;
    Ok(format!(
        "pictures: {} picture(s) at {} scale(s) written to {}",
        spec.pictures.len(),
        spec.scales.len(),
        out.display()
    ))
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(path).map_err(|error| Error::Io {
        what: format!("open {}", path.display()),
        error,
    })?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| Error::Io {
            what: format!("read {}", path.display()),
            error,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(Error::Spec(format!(
            "{} is larger than {limit} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn read_spec(krate: &Path) -> Result<SpecFile, Error> {
    let bytes = read_limited(&krate.join(SPEC_FILE), MAX_SPEC_BYTES)?;
    let text =
        String::from_utf8(bytes).map_err(|_| Error::Spec(format!("{SPEC_FILE} is not UTF-8")))?;
    let spec: SpecFile =
        toml::from_str(&text).map_err(|error| Error::Spec(format!("{SPEC_FILE}: {error}")))?;
    let module = Path::new(&spec.module);
    if !module.is_relative()
        || module
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: module must be a folder inside the crate"
        )));
    }
    if spec.scales.is_empty()
        || !spec
            .scales
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a < b))
    {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: scales must rise, at least one"
        )));
    }
    if let Some(scale) = spec
        .scales
        .iter()
        .find(|&&scale| !(4..=16).contains(&scale))
    {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: scale {scale} is not 4 to 16 quarters"
        )));
    }
    if spec.pictures.is_empty() || spec.pictures.len() > MAX_PICTURES {
        return Err(Error::Spec(format!(
            "{SPEC_FILE}: 1 to {MAX_PICTURES} pictures"
        )));
    }
    let mut names = BTreeSet::new();
    for picture in &spec.pictures {
        let name = &picture.name;
        let good = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && name.len() <= 64;
        if !good {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: picture name {name:?} must be a lower-case letter, then letters, digits and _"
            )));
        }
        if !names.insert(name.as_str()) {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: picture {name:?} named twice"
            )));
        }
        if picture.width == 0
            || picture.width > MAX_UNITS
            || picture.height.is_some_and(|h| h == 0 || h > MAX_UNITS)
        {
            return Err(Error::Spec(format!(
                "{SPEC_FILE}: picture {name:?}: sides 1 to {MAX_UNITS} units"
            )));
        }
    }
    Ok(spec)
}

/// Every file under `dir`, relative to the crate, sorted.
fn files_under(krate: &Path, dir: &Path, found: &mut Vec<String>) -> Result<(), Error> {
    let entries = std::fs::read_dir(krate.join(dir)).map_err(|error| Error::Io {
        what: format!("list {}", dir.display()),
        error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::Io {
            what: format!("list {}", dir.display()),
            error,
        })?;
        let path = dir.join(entry.file_name());
        if entry.path().is_dir() {
            files_under(krate, &path, found)?;
        } else if found.len() < MAX_PICTURES.saturating_mul(2) {
            found.push(path.to_string_lossy().into_owned());
        } else {
            return Err(Error::Spec(format!("{PICTURES_DIR}/ holds too many files")));
        }
    }
    Ok(())
}

/// Everything the tool writes, by file name: made in memory first.
fn convert(krate: &Path, spec: &SpecFile) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let mut problems = Vec::new();
    let mut found = Vec::new();
    files_under(krate, Path::new(PICTURES_DIR), &mut found)?;
    let pinned: BTreeSet<&str> = spec
        .pictures
        .iter()
        .map(|picture| picture.file.as_str())
        .collect();
    for file in &found {
        if !pinned.contains(file.as_str()) {
            problems.push(format!("{file}: in {PICTURES_DIR}/ but not pinned"));
        }
    }
    let mut decoded = Vec::new();
    for picture in &spec.pictures {
        if !Path::new(&picture.file).starts_with(PICTURES_DIR) {
            problems.push(format!("{}: not in {PICTURES_DIR}/", picture.file));
            continue;
        }
        let bytes = read_limited(&krate.join(&picture.file), MAX_FILE_BYTES)?;
        let digest = Sha256::digest(&bytes);
        let hex = digest.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        if hex != picture.sha256 {
            problems.push(format!(
                "{}: sha256 is {hex}, pinned {}",
                picture.file, picture.sha256
            ));
            continue;
        }
        decoded.push((picture, bytes));
    }
    if !problems.is_empty() {
        return Err(Error::Pins(problems));
    }
    let mut files = Vec::new();
    let mut statics = Vec::new();
    for (picture, bytes) in decoded {
        let source = decode(&picture.file, &bytes)?;
        let height = match picture.height {
            Some(height) => height,
            None => {
                let exact = u64::from(picture.width)
                    .saturating_mul(u64::from(source.height))
                    .saturating_mul(2)
                    .checked_div(u64::from(source.width))
                    .unwrap_or(0);
                u32::try_from(exact.saturating_add(1) / 2)
                    .ok()
                    .filter(|&h| h > 0 && h <= MAX_UNITS)
                    .ok_or_else(|| Error::Picture {
                        file: picture.file.clone(),
                        message: "its shape gives no height from 1 to 4096 units".to_owned(),
                    })?
            }
        };
        let mut made = Vec::new();
        for &quarters in &spec.scales {
            let (width, high) = (scaled(picture.width, quarters), scaled(height, quarters));
            if width > source.width || high > source.height {
                return Err(Error::Picture {
                    file: picture.file.clone(),
                    message: format!(
                        "{} x {} pixels, smaller than its {} x {} at scale {} needs: enlarging it would blur it",
                        source.width,
                        source.height,
                        width,
                        high,
                        scale_name(quarters)
                    ),
                });
            }
            let pixels = resample(&source, width, high);
            files.push((format!("{}_{quarters}.rgba", picture.name), pixels.rgba));
            made.push((quarters, width, high));
        }
        statics.push((picture, height, made));
    }
    files.push(("mod.rs".to_owned(), module_rs(&statics).into_bytes()));
    let names = spec
        .pictures
        .iter()
        .fold(String::new(), |mut names, picture| {
            names.push_str(&picture.name);
            names.push('\n');
            names
        });
    files.push(("names".to_owned(), names.into_bytes()));
    files.sort();
    Ok(files)
}

/// `units` layout units at `quarters`, to the nearest whole pixel (a half
/// rounds up), as `nowayos_display::DeviceScale::px` gives it.
#[must_use]
pub fn scaled(units: u32, quarters: u8) -> u32 {
    let pixels = u64::from(units)
        .saturating_mul(u64::from(quarters))
        .saturating_add(2)
        / 4;
    u32::try_from(pixels).unwrap_or(u32::MAX)
}

fn scale_name(quarters: u8) -> String {
    let whole = quarters / 4;
    match quarters % 4 {
        0 => format!("{whole}"),
        1 => format!("{whole}.25"),
        2 => format!("{whole}.5"),
        _ => format!("{whole}.75"),
    }
}

/// Decodes a PNG or JPEG file, by its name's extension, to premultiplied
/// 8-bit RGBA.
///
/// # Errors
///
/// [`Error::Picture`] for a file it cannot decode, of another kind, or
/// larger than 8192 pixels a side.
pub fn decode(file: &str, bytes: &[u8]) -> Result<Decoded, Error> {
    let fail = |message: String| Error::Picture {
        file: file.to_owned(),
        message,
    };
    let lower = file.to_ascii_lowercase();
    let (width, height, rgba) = if lower.ends_with(".png") {
        decode_png(bytes).map_err(fail)?
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        decode_jpeg(bytes).map_err(fail)?
    } else {
        return Err(fail("neither .png nor .jpg".to_owned()));
    };
    Ok(Decoded {
        width,
        height,
        rgba: premultiply(rgba),
    })
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!(
            "{width} x {height} pixels: each side 1 to {MAX_SIDE}"
        ));
    }
    Ok(())
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use png::{ColorType, Transformations};
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        Transformations::EXPAND | Transformations::STRIP_16 | Transformations::ALPHA,
    );
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("not a PNG it can read: {error}"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    check_size(width, height)?;
    let size = reader.output_buffer_size().ok_or("too large to decode")?;
    let mut buffer = vec![0; size];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("not a PNG it can read: {error}"))?;
    let data = buffer
        .get(..frame.buffer_size())
        .ok_or("its frame is larger than its buffer")?;
    let rgba = match frame.color_type {
        ColorType::Rgba => data.to_vec(),
        ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|pixel| match pixel {
                [grey, alpha] => [*grey, *grey, *grey, *alpha],
                _ => [0; 4],
            })
            .collect(),
        other => return Err(format!("decodes to {other:?}, not RGBA")),
    };
    Ok((width, height, rgba))
}

fn decode_jpeg(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_strict_mode(true);
    let mut decoder = JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|error| format!("not a JPEG it can read: {error:?}"))?;
    let info = decoder.info().ok_or("not a JPEG it can read: no header")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_size(width, height)?;
    let rgb = decoder
        .decode()
        .map_err(|error| format!("not a JPEG it can read: {error:?}"))?;
    let pixels =
        usize::try_from(u64::from(width).saturating_mul(u64::from(height))).unwrap_or(usize::MAX);
    if rgb.len() != pixels.saturating_mul(3) {
        return Err("decodes to other than RGB".to_owned());
    }
    Ok((
        width,
        height,
        rgb.chunks_exact(3)
            .flat_map(|pixel| match pixel {
                [r, g, b] => [*r, *g, *b, u8::MAX],
                _ => [0; 4],
            })
            .collect(),
    ))
}

/// Each colour multiplied by its pixel's alpha, to the nearest whole value.
fn premultiply(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = pixel {
            let alpha = u32::from(*a);
            for channel in [r, g, b] {
                let product = u32::from(*channel)
                    .saturating_mul(alpha)
                    .saturating_add(127)
                    / 255;
                *channel = u8::try_from(product).unwrap_or(u8::MAX);
            }
        }
    }
    rgba
}

/// For each of `dest` pixels along an axis of `source`, the source pixels
/// it covers and how much of each, in 1/`dest` of a source pixel: they sum
/// to `source`.
fn weights(dest: u32, source: u32) -> Vec<Vec<(usize, u64)>> {
    let (dest64, source64) = (u64::from(dest), u64::from(source));
    (0..dest64)
        .map(|at| {
            // Its span, in 1/dest source pixel.
            let (start, end) = (
                at.saturating_mul(source64),
                at.saturating_add(1).saturating_mul(source64),
            );
            let first = start.checked_div(dest64).unwrap_or(0);
            let last = end.saturating_sub(1).checked_div(dest64).unwrap_or(0);
            (first..=last)
                .filter_map(|pixel| {
                    let from = pixel.saturating_mul(dest64).max(start);
                    let to = pixel.saturating_add(1).saturating_mul(dest64).min(end);
                    let index = usize::try_from(pixel).ok()?;
                    (to > from).then(|| (index, to.saturating_sub(from)))
                })
                .collect()
        })
        .collect()
}

/// `source` resampled to `width` x `height`, no larger than it: each pixel
/// the exact area-weighted mean of the source pixels it covers, rounded to
/// the nearest whole value. Premultiplied in, premultiplied out, so a colour
/// never exceeds its alpha.
#[must_use]
pub fn resample(source: &Decoded, width: u32, height: u32) -> Decoded {
    let (across, down) = (weights(width, source.width), weights(height, source.height));
    let row_bytes = usize::try_from(source.width).unwrap_or(0).saturating_mul(4);
    // Each source row reduced across first: sums in 1/width pixel.
    let reduced: Vec<Vec<u64>> = source
        .rgba
        .chunks_exact(row_bytes.max(1))
        .map(|row| {
            across
                .iter()
                .flat_map(|spans| {
                    let mut sums = [0_u64; 4];
                    for &(pixel, weight) in spans {
                        let start = pixel.saturating_mul(4);
                        if let Some(values) = row.get(start..start.saturating_add(4)) {
                            for (sum, &value) in sums.iter_mut().zip(values) {
                                *sum = sum.saturating_add(u64::from(value).saturating_mul(weight));
                            }
                        }
                    }
                    sums
                })
                .collect()
        })
        .collect();
    let whole = u64::from(source.width).saturating_mul(u64::from(source.height));
    let mut rgba = Vec::with_capacity(
        usize::try_from(
            u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(4),
        )
        .unwrap_or(0),
    );
    for spans in &down {
        for column in 0..usize::try_from(width).unwrap_or(0).saturating_mul(4) {
            let sum = spans.iter().fold(0_u64, |sum, &(row, weight)| {
                let value = reduced
                    .get(row)
                    .and_then(|row| row.get(column))
                    .copied()
                    .unwrap_or(0);
                sum.saturating_add(value.saturating_mul(weight))
            });
            let mean = sum
                .saturating_add(whole / 2)
                .checked_div(whole)
                .unwrap_or(0);
            rgba.push(u8::try_from(mean).unwrap_or(u8::MAX));
        }
    }
    Decoded {
        width,
        height,
        rgba,
    }
}

/// A picture converted: its spec, its height in layout units and, for
/// each scale, the scale and its size in pixels.
type Made<'a> = (&'a PictureSpec, u32, Vec<(u8, u32, u32)>);

/// The generated module: a static for each picture, and every picture
/// listed for tests and tools.
fn module_rs(statics: &[Made<'_>]) -> String {
    let mut out = String::from(
        "// Generated by tools/pictures from pictures.toml (make atlases): do not edit.\n\
         //! The pictures `pictures.toml` pins, converted at build time, each a\n\
         //! static of its own: a program links only those its screens name.\n\
         \n\
         use nowayos_pictures::{Picture, Pixels};\n",
    );
    for (picture, height, made) in statics {
        let _ = write!(
            out,
            "\n/// `{}`: {} x {height} layout units.\n#[rustfmt::skip]\npub static {}: Picture = Picture::new(\n    {},\n    {height},\n    &[\n",
            picture.file,
            picture.width,
            picture.name.to_ascii_uppercase(),
            picture.width,
        );
        for (quarters, width, high) in made {
            let _ = writeln!(
                out,
                "        Pixels::new({quarters}, {width}, {high}, include_bytes!(\"{}_{quarters}.rgba\")),",
                picture.name
            );
        }
        out.push_str("    ],\n);\n");
    }
    let _ = write!(
        out,
        "\n/// Every picture, by name, for tests and tools; never for a device.\n#[rustfmt::skip]\npub static ALL: [(&str, &Picture); {}] = [\n",
        statics.len()
    );
    for (picture, _, _) in statics {
        let _ = writeln!(
            out,
            "    (\"{}\", &{}),",
            picture.name,
            picture.name.to_ascii_uppercase()
        );
    }
    out.push_str("];\n");
    out
}

/// Writes `files` into `out`, which must hold nothing else.
fn write_out(out: &Path, files: &[(String, Vec<u8>)]) -> Result<(), Error> {
    std::fs::create_dir_all(out).map_err(|error| Error::Io {
        what: format!("create {}", out.display()),
        error,
    })?;
    let ours: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let entries = std::fs::read_dir(out).map_err(|error| Error::Io {
        what: format!("list {}", out.display()),
        error,
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !ours.contains(name.as_str()) && !name.ends_with(".rgba") {
            return Err(Error::OutDir(format!(
                "{} holds {name}, which the tool did not write: remove it",
                out.display()
            )));
        }
        if !ours.contains(name.as_str()) {
            std::fs::remove_file(entry.path()).map_err(|error| Error::Io {
                what: format!("remove {}", entry.path().display()),
                error,
            })?;
        }
    }
    for (name, bytes) in files {
        let path = out.join(name);
        std::fs::write(&path, bytes).map_err(|error| Error::Io {
            what: format!("write {}", path.display()),
            error,
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure fails the test"
)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, pixel: [u8; 4]) -> Decoded {
        Decoded {
            width,
            height,
            rgba: (0..width * height).flat_map(|_| pixel).collect(),
        }
    }

    #[test]
    fn the_weights_of_each_pixel_sum_to_the_source() {
        for (dest, source) in [
            (1, 1),
            (1, 7),
            (3, 7),
            (7, 7),
            (160, 640),
            (213, 640),
            (5, 8),
        ] {
            for spans in weights(dest, source) {
                assert_eq!(
                    spans.iter().map(|&(_, w)| w).sum::<u64>(),
                    u64::from(source)
                );
            }
        }
    }

    #[test]
    fn a_flat_colour_stays_itself_at_any_size() {
        let source = flat(37, 23, [10, 20, 30, 40]);
        for (width, height) in [(37, 23), (36, 22), (12, 5), (1, 1)] {
            let out = resample(&source, width, height);
            assert_eq!(out.rgba.len() as u32, width * height * 4);
            assert!(
                out.rgba
                    .chunks_exact(4)
                    .all(|pixel| pixel == [10, 20, 30, 40])
            );
        }
    }

    #[test]
    fn halving_averages_each_square_of_four() {
        let source = Decoded {
            width: 2,
            height: 2,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 0, 0, 0],
        };
        // (0 + 255 + 255 + 0) / 4 = 127.5, so 128; alpha (255 * 3) / 4 = 191.25.
        assert_eq!(resample(&source, 1, 1).rgba, vec![128, 64, 64, 191]);
    }

    #[test]
    fn a_colour_never_exceeds_its_alpha() {
        let source = decode(
            "t.png",
            &std::fs::read("../../crates/nowayos-pictures/pictures/badge.png").unwrap(),
        )
        .unwrap();
        for size in [48, 47, 30, 96, 7] {
            let out = resample(&source, size, size);
            assert!(
                out.rgba
                    .chunks_exact(4)
                    .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
            );
        }
    }

    #[test]
    fn premultiplying_rounds_to_the_nearest() {
        assert_eq!(premultiply(vec![255, 128, 1, 128]), vec![128, 64, 1, 128]);
        assert_eq!(premultiply(vec![200, 100, 50, 0]), vec![0, 0, 0, 0]);
        assert_eq!(
            premultiply(vec![200, 100, 50, 255]),
            vec![200, 100, 50, 255]
        );
    }

    #[test]
    fn sizes_round_as_the_display_does() {
        assert_eq!(scaled(160, 4), 160);
        assert_eq!(scaled(160, 10), 400);
        assert_eq!(scaled(101, 10), 253);
        assert_eq!(scaled(48, 5), 60);
        assert_eq!(scale_name(10), "2.5");
    }
}
