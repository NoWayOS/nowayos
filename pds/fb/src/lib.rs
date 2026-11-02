// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-fb`: showing frames on a framebuffer the firmware set up and the
//! display hardware scans out - the Raspberry Pi 5's HDMI framebuffer.
//!
//! The board's facts give the framebuffer's window and mode
//! (`firmware_fb` in `platforms/rpi5b_2gb.toml`: a `simple-framebuffer`,
//! 1920 x 1080 `r5g6b5`, its window the whole of it). [`bind_window`] reads
//! them from the facts region (`nowayos_region::device_facts`, read-only
//! region 5) and binds the window (`nowayos_hw::Scanout`, from device N's
//! symbols); [`bind`] also places a frame of the given size on the screen
//! ([`Placement::in_window`]): a frame the size of the window at its own
//! pixels - the UI drawing at the monitor's resolution, 1920 x 1080 on
//! the Pi 5 - or a smaller one scaled by the largest whole number
//! that fits, centred. A [`Screen`] then clears the window to black and
//! shows a frame a row at a time ([`Screen::show_row`]), or only the
//! rectangles that changed ([`Screen::show_rect`]): each row
//! converted from the frame's pixel format to the screen's and written as
//! many times as the scale says, every byte written by `show_row` fed to a
//! [`FrameHash`] - the scan-out hash, which a host test can compute from
//! the same frame - and [`Screen::read_back`] hashes what the window holds
//! where the frame is shown, to show the memory holds what was written.
//!
//! [`Screen`] works on any [`Window`]: [`Scanout`] in a PD, plain memory in
//! this crate's host tests. No `unsafe` code; the window's binding is
//! `nowayos-hw`'s.

#![cfg_attr(not(test), no_std)]

use core::fmt;

use nowayos_display::{FrameHash, PixelFormat, Placement, Rect, Size, scale_row};
use nowayos_hw::{HwError, Scanout, device};
use nowayos_region::{DeviceFacts, FramebufferFormat, FramebufferMode, RegionError, device_facts};

/// The compatible string of a framebuffer the firmware set up, as the
/// board's facts give it: the devicetree binding's name.
pub const COMPATIBLE: &[u8] = b"simple-framebuffer";

/// The longest screen row this crate writes, in bytes.
pub const MAX_ROW: usize = 8192;

/// Bytes the window is written and read in: [`Scanout`]'s word.
const WORD: usize = 8;

/// [`WORD`], as the pixel arithmetic of [`Screen::show_rect`] counts it.
const WORD_PIXELS: u32 = 8;

/// The greatest common divisor of `a` and `b` (Euclid's).
const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while let Some(rest) = a.checked_rem(b) {
        a = b;
        b = rest;
    }
    a
}

/// Why a frame cannot be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbError {
    /// The board's facts could not be read.
    Facts(RegionError),
    /// The device is not a framebuffer with a mode: its facts are another
    /// device's.
    NotAFramebuffer,
    /// The window could not be bound, written or read.
    Device(HwError),
    /// The frame cannot be shown whole in the window: it is larger than the
    /// screen, or would start above the window's first row.
    DoesNotFit {
        /// The frame's size.
        frame: Size,
    },
    /// A row of the screen or of the placed frame is longer than
    /// [`MAX_ROW`], or does not start and end on a word of the window.
    RowNotWritable,
    /// A row given is not one of the frame's, or not its width in its
    /// format.
    BadRow {
        /// The row's number in the frame.
        y: u32,
    },
}

impl fmt::Display for FbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Facts(error) => write!(f, "the board's facts: {error}"),
            Self::NotAFramebuffer => f.write_str(
                "the device's facts are not a framebuffer's (simple-framebuffer, with a mode)",
            ),
            Self::Device(error) => write!(f, "the framebuffer's window: {error}"),
            Self::DoesNotFit { frame } => write!(
                f,
                "a {}x{} frame cannot be shown whole in the framebuffer's window",
                frame.width, frame.height
            ),
            Self::RowNotWritable => {
                f.write_str("a row is too long, or not whole words of the window")
            }
            Self::BadRow { y } => write!(f, "row {y} is not one of the frame's rows"),
        }
    }
}

/// Memory a framebuffer's window is written and read in: whole words, by
/// byte offset from the window's start.
pub trait Window {
    /// The window's bytes.
    fn len(&self) -> usize;

    /// Whether the window has no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes `bytes` at `offset`.
    ///
    /// # Errors
    ///
    /// As [`Scanout::write`].
    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), HwError>;

    /// Reads the bytes at `offset` into `out`.
    ///
    /// # Errors
    ///
    /// As [`Scanout::read`].
    fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), HwError>;
}

impl Window for Scanout {
    fn len(&self) -> usize {
        Scanout::len(self)
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), HwError> {
        Scanout::write(self, offset, bytes)
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), HwError> {
        Scanout::read(self, offset, out)
    }
}

/// The board's framebuffer - device `index`'s window, with the facts in
/// read-only region 5 - and its mode, before any frame is placed on it:
/// what a display PD reads to choose the frames it asks the UI for.
///
/// # Errors
///
/// [`FbError::Facts`] when the PD is given no facts (a board without the
/// framebuffer); otherwise as [`mode_of`] and [`Scanout::new`].
pub fn bind_window(index: usize) -> Result<(Scanout, FramebufferMode), FbError> {
    let facts = device_facts().map_err(FbError::Facts)?;
    let mode = mode_of(&facts)?;
    let device = device(index).map_err(FbError::Device)?;
    let window = Scanout::new(device, facts.registers()).map_err(FbError::Device)?;
    Ok((window, mode))
}

/// The size of the part of the screen `mode`'s window holds: its whole
/// width, from its first row to the last - 1920 x 1080 on the Pi 5. A frame
/// of this size is shown at the monitor's own pixels.
#[must_use]
pub fn window_size(mode: &FramebufferMode) -> Size {
    Size::new(mode.width(), mode.height().saturating_sub(mode.first_row()))
}

/// The screen's pixel format in a mode.
#[must_use]
pub const fn format_of(mode: &FramebufferMode) -> PixelFormat {
    match mode.format() {
        FramebufferFormat::R5g6b5 => PixelFormat::Rgb565,
    }
}

/// The mode in a device's facts, if they are a framebuffer's.
///
/// # Errors
///
/// [`FbError::NotAFramebuffer`] unless the compatible string is
/// [`COMPATIBLE`] and the facts carry a mode.
pub fn mode_of(facts: &DeviceFacts) -> Result<FramebufferMode, FbError> {
    if facts.compatible() != COMPATIBLE {
        return Err(FbError::NotAFramebuffer);
    }
    facts.framebuffer().ok_or(FbError::NotAFramebuffer)
}

/// The board's framebuffer - device `index`'s window, with the facts in
/// read-only region 5 - set to show frames of `frame` pixels in
/// `frame_format`.
///
/// # Errors
///
/// [`FbError::Facts`] when the PD is given no facts (a board without the
/// framebuffer); otherwise as [`mode_of`], [`Scanout::new`] and
/// [`Screen::new`].
pub fn bind(
    index: usize,
    frame: Size,
    frame_format: PixelFormat,
) -> Result<Screen<Scanout>, FbError> {
    let (window, mode) = bind_window(index)?;
    Screen::new(window, mode, frame, frame_format)
}

/// A framebuffer's window, set to show frames of one size, scaled and
/// centred.
pub struct Screen<W> {
    window: W,
    mode: FramebufferMode,
    format: PixelFormat,
    placement: Placement,
    frame: Size,
    frame_format: PixelFormat,
    /// Bytes of a screen row; of the placed frame's rows; the placed
    /// frame's left edge, from a row's start.
    stride: usize,
    placed: usize,
    left: usize,
    row: [u8; MAX_ROW],
}

impl<W: Window> Screen<W> {
    /// A screen showing frames of `frame` pixels in `frame_format` in
    /// `window`, which holds `mode`'s rows from its first row.
    ///
    /// # Errors
    ///
    /// [`FbError::DoesNotFit`] if the frame is larger than the screen, would
    /// start above the window's first row, or the window is shorter than its
    /// rows; [`FbError::RowNotWritable`] if a row is longer than
    /// [`MAX_ROW`] or does not start and end on a word.
    pub fn new(
        window: W,
        mode: FramebufferMode,
        frame: Size,
        frame_format: PixelFormat,
    ) -> Result<Self, FbError> {
        let format = format_of(&mode);
        let does_not_fit = FbError::DoesNotFit { frame };
        // The largest scale whose centred frame starts on or below the
        // window's first row; or, too tall for that, at its own
        // size in the window - the window's size, from its first row.
        let placement = Placement::in_window(
            frame,
            Size::new(mode.width(), mode.height()),
            mode.first_row(),
        )
        .ok_or(does_not_fit)?;
        let rows = mode.height().saturating_sub(mode.first_row());
        let stride = usize::try_from(mode.stride()).map_err(|_| FbError::RowNotWritable)?;
        let bytes = |pixels: u32| {
            usize::try_from(pixels)
                .ok()
                .and_then(|pixels| pixels.checked_mul(usize::from(format.bytes_per_pixel())))
                .ok_or(FbError::RowNotWritable)
        };
        let placed = bytes(placement.size().width)?;
        let left = bytes(placement.x())?;
        if placement.y() < mode.first_row()
            || usize::try_from(rows)
                .ok()
                .and_then(|rows| rows.checked_mul(stride))
                .is_none_or(|needed| window.len() < needed)
        {
            return Err(does_not_fit);
        }
        if stride > MAX_ROW
            || [stride, placed, left]
                .iter()
                .any(|bytes| !bytes.is_multiple_of(WORD))
        {
            return Err(FbError::RowNotWritable);
        }
        Ok(Self {
            window,
            mode,
            format,
            placement,
            frame,
            frame_format,
            stride,
            placed,
            left,
            row: [0; MAX_ROW],
        })
    }

    /// The framebuffer's mode.
    pub const fn mode(&self) -> FramebufferMode {
        self.mode
    }

    /// Where frames are shown, and at what scale.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// The monitor's rows that showing `rect` of a frame writes, from the
    /// first to before the last, counted from the monitor's top: what the
    /// display's scan must be clear of while it is copied. `None`
    /// if the rows overflow.
    #[must_use]
    pub fn rows_of(&self, rect: Rect) -> Option<(u32, u32)> {
        let scale = self.placement.scale();
        let top = u32::try_from(rect.origin.y).ok()?;
        let bottom = top.checked_add(rect.size.height)?;
        let on_screen = |row: u32| row.checked_mul(scale)?.checked_add(self.placement.y());
        Some((on_screen(top)?, on_screen(bottom)?))
    }

    /// The screen's pixel format.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Writes black to every row of the window.
    ///
    /// # Errors
    ///
    /// [`FbError::Device`] if a row cannot be written.
    pub fn clear(&mut self) -> Result<(), FbError> {
        let black = self
            .row
            .get_mut(..self.stride)
            .ok_or(FbError::RowNotWritable)?;
        black.fill(0);
        let rows = self.mode.height().saturating_sub(self.mode.first_row());
        for row in 0..rows {
            let offset = usize::try_from(row)
                .ok()
                .and_then(|row| row.checked_mul(self.stride))
                .ok_or(FbError::RowNotWritable)?;
            self.window.write(offset, black).map_err(FbError::Device)?;
        }
        Ok(())
    }

    /// Shows row `y` of a frame, `row` - the frame's width of pixels in its
    /// format - converted to the screen's format, scaled and written as the
    /// placement says, every byte written fed to `hash`.
    ///
    /// # Errors
    ///
    /// [`FbError::BadRow`] if `y` is not a row of the frame or `row` not its
    /// width; [`FbError::Device`] if the window cannot be written.
    pub fn show_row(&mut self, y: u32, row: &[u8], hash: &mut FrameHash) -> Result<(), FbError> {
        let bad = FbError::BadRow { y };
        let width = usize::try_from(self.frame.width).map_err(|_| bad)?;
        if y >= self.frame.height
            || width.checked_mul(usize::from(self.frame_format.bytes_per_pixel()))
                != Some(row.len())
        {
            return Err(bad);
        }
        let scale = self.placement.scale();
        let written = scale_row(self.frame_format, row, self.format, scale, &mut self.row)
            .filter(|&written| written == self.placed)
            .ok_or(bad)?;
        let scaled = self.row.get(..written).ok_or(bad)?;
        for copy in 0..scale {
            let offset = self.offset_of(y, copy).ok_or(bad)?;
            self.window.write(offset, scaled).map_err(FbError::Device)?;
            hash.update(scaled);
        }
        Ok(())
    }

    /// Shows the part of a frame inside `rect` - in frame pixels, widened
    /// to whole words of the window, which it is written in - each row's
    /// pixels read by `load(y, x, out)`: the frame's row `y` from column `x`,
    /// `out`'s length of bytes in the frame's format, `false` if it cannot.
    /// Only those rows are converted, scaled and written, so a change to
    /// one field of a screen at the monitor's own pixels writes a few
    /// thousand bytes, not four million. The widening reads the
    /// frame's own pixels beside the rectangle, which the window shows
    /// already, so nothing else changes on screen.
    ///
    /// # Errors
    ///
    /// [`FbError::BadRow`] if `rect` is not inside the frame or `load`
    /// fails; [`FbError::Device`] if the window cannot be written.
    pub fn show_rect(
        &mut self,
        rect: Rect,
        mut load: impl FnMut(u32, u32, &mut [u8]) -> bool,
    ) -> Result<(), FbError> {
        let bad = |y| FbError::BadRow { y };
        let top = u32::try_from(rect.origin.y).map_err(|_| bad(0))?;
        let left = u32::try_from(rect.origin.x).map_err(|_| bad(top))?;
        let right = left.checked_add(rect.size.width).ok_or(bad(top))?;
        let bottom = top.checked_add(rect.size.height).ok_or(bad(top))?;
        if right > self.frame.width || bottom > self.frame.height {
            return Err(bad(top));
        }
        // Frame pixels a whole number of the window's words take, scaled:
        // the frame's width and its left edge are whole words (`new`), so
        // a span widened to this stays inside the frame's row.
        let scaled = self
            .placement
            .scale()
            .checked_mul(u32::from(self.format.bytes_per_pixel()))
            .ok_or(bad(top))?;
        let step = WORD_PIXELS
            .checked_div(gcd(WORD_PIXELS, scaled))
            .ok_or(bad(top))?;
        let first = left
            .checked_div(step)
            .and_then(|n| n.checked_mul(step))
            .ok_or(bad(top))?;
        let last = right
            .checked_add(step.saturating_sub(1))
            .and_then(|end| end.checked_div(step))
            .and_then(|n| n.checked_mul(step))
            .ok_or(bad(top))?
            .min(self.frame.width);
        let source_bytes = usize::from(self.frame_format.bytes_per_pixel());
        let len = usize::try_from(last.saturating_sub(first))
            .ok()
            .and_then(|pixels| pixels.checked_mul(source_bytes))
            .ok_or(bad(top))?;
        let mut source = [0_u8; MAX_ROW];
        let source = source.get_mut(..len).ok_or(FbError::RowNotWritable)?;
        let screen_left = usize::try_from(first)
            .ok()
            .and_then(|x| x.checked_mul(usize::try_from(scaled).ok()?))
            .ok_or(bad(top))?;
        for y in top..bottom {
            if !load(y, first, source) {
                return Err(bad(y));
            }
            let written = scale_row(
                self.frame_format,
                source,
                self.format,
                self.placement.scale(),
                &mut self.row,
            )
            .ok_or(bad(y))?;
            let bytes = self.row.get(..written).ok_or(bad(y))?;
            for copy in 0..self.placement.scale() {
                let offset = self
                    .offset_of(y, copy)
                    .and_then(|start| start.checked_add(screen_left))
                    .ok_or(bad(y))?;
                self.window.write(offset, bytes).map_err(FbError::Device)?;
            }
        }
        Ok(())
    }

    /// Hands `each_row` what the window holds where frames are shown, row
    /// by row: after [`show_row`](Self::show_row) has shown every row of a
    /// frame, the same bytes it fed its hash.
    ///
    /// # Errors
    ///
    /// [`FbError::Device`] if the window cannot be read.
    pub fn read_back(&mut self, mut each_row: impl FnMut(&[u8])) -> Result<(), FbError> {
        let placed = self
            .row
            .get_mut(..self.placed)
            .ok_or(FbError::RowNotWritable)?;
        for y in 0..self.frame.height {
            for copy in 0..self.placement.scale() {
                let offset =
                    Self::offset(self.mode, self.placement, self.stride, self.left, y, copy)
                        .ok_or(FbError::BadRow { y })?;
                self.window.read(offset, placed).map_err(FbError::Device)?;
                each_row(placed);
            }
        }
        Ok(())
    }

    /// The window offset of copy `copy` of frame row `y`.
    fn offset_of(&self, y: u32, copy: u32) -> Option<usize> {
        Self::offset(self.mode, self.placement, self.stride, self.left, y, copy)
    }

    fn offset(
        mode: FramebufferMode,
        placement: Placement,
        stride: usize,
        left: usize,
        y: u32,
        copy: u32,
    ) -> Option<usize> {
        let screen_row = y
            .checked_mul(placement.scale())?
            .checked_add(copy)?
            .checked_add(placement.y())?;
        let window_row = usize::try_from(screen_row.checked_sub(mode.first_row())?).ok()?;
        window_row.checked_mul(stride)?.checked_add(left)
    }

    /// The window, given back.
    pub fn into_window(self) -> W {
        self.window
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Plain memory standing in for a window, written and read in words.
    struct Memory(Vec<u8>);

    impl Window for Memory {
        fn len(&self) -> usize {
            self.0.len()
        }

        fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), HwError> {
            if !offset.is_multiple_of(WORD) || !bytes.len().is_multiple_of(WORD) {
                return Err(HwError::Misaligned {
                    vaddr: offset,
                    align: WORD,
                });
            }
            self.0
                .get_mut(offset..offset + bytes.len())
                .ok_or(HwError::OutsideRegion {
                    offset,
                    len: bytes.len(),
                    size: 0,
                })?
                .copy_from_slice(bytes);
            Ok(())
        }

        fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), HwError> {
            out.copy_from_slice(&self.0[offset..offset + out.len()]);
            Ok(())
        }
    }

    /// A 1920 x 1080 mode whose window starts at row 16, as the Pi 5's did
    /// under Microkit 2.3.1's release SDK: a window below rows that are not
    /// the display's to write.
    fn pi5() -> FramebufferMode {
        FramebufferMode::new(1920, 1080, 3840, FramebufferFormat::R5g6b5, 16).unwrap()
    }

    fn pi5_window() -> Memory {
        Memory(vec![0xEE; 1064 * 3840])
    }

    /// The Pi 5's mode, as its board facts give it: the window is the
    /// whole framebuffer.
    fn pi5_whole() -> FramebufferMode {
        FramebufferMode::new(1920, 1080, 3840, FramebufferFormat::R5g6b5, 0).unwrap()
    }

    /// A window that is the whole screen is the monitor's every row: a frame
    /// its size is shown at its own pixels from row 0, its rows counted
    /// from the monitor's top, and the kiosk's frame centred as before.
    #[test]
    fn a_window_from_row_0_shows_a_frame_on_every_row() {
        assert_eq!(window_size(&pi5_whole()), Size::new(1920, 1080));
        let mut native = Screen::new(
            Memory(vec![0xEE; 1080 * 3840]),
            pi5_whole(),
            Size::new(1920, 1080),
            PixelFormat::Rgb565,
        )
        .unwrap();
        let shown = native.placement();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (1, 0, 0));
        assert_eq!(native.rows_of(Rect::new(40, 0, 100, 10)), Some((0, 10)));
        assert_eq!(
            native.rows_of(Rect::new(0, 1000, 1920, 80)),
            Some((1000, 1080))
        );
        native.clear().unwrap();
        let memory = native.into_window().0;
        assert!(memory.iter().all(|&b| b == 0), "every row cleared");
        let kiosk = Screen::new(
            Memory(vec![0xEE; 1080 * 3840]),
            pi5_whole(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let shown = kiosk.placement();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (2, 160, 60));
        assert_eq!(kiosk.rows_of(Rect::new(0, 5, 800, 3)), Some((70, 76)));
    }

    #[test]
    fn the_kiosk_ui_is_shown_twice_its_size_in_the_middle_of_the_pi_5s_screen() {
        let mut screen = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let placement = screen.placement();
        assert_eq!(
            (placement.scale(), placement.x(), placement.y()),
            (2, 160, 60)
        );
        screen.clear().unwrap();
        // Row 0: one red pixel, then white.
        let mut row = vec![0xFF_u8; 800 * 4];
        row[..4].copy_from_slice(&[0, 0, 0xFF, 0]);
        let mut written = FrameHash::new();
        screen.show_row(0, &row, &mut written).unwrap();
        let memory = screen.into_window().0;
        // Screen rows 60 and 61 are window rows 44 and 45; the frame's
        // left edge is 160 pixels, 320 bytes, in.
        for window_row in [44, 45] {
            let start = window_row * 3840;
            assert!(
                memory[start..start + 320].iter().all(|&b| b == 0),
                "black border"
            );
            assert_eq!(
                &memory[start + 320..start + 324],
                &[0x00, 0xF8, 0x00, 0xF8],
                "red, twice"
            );
            assert!(
                memory[start + 324..start + 3520].iter().all(|&b| b == 0xFF),
                "white"
            );
            assert!(
                memory[start + 3520..start + 3840].iter().all(|&b| b == 0),
                "black border"
            );
        }
        assert!(
            memory[46 * 3840..47 * 3840].iter().all(|&b| b == 0),
            "the next row untouched"
        );
    }

    #[test]
    fn a_rectangles_rows_are_counted_from_the_monitors_top() {
        let native = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(1920, 1064),
            PixelFormat::Rgb565,
        )
        .unwrap();
        // At its own pixels from the window's first row, 16.
        let rect = Rect::new(40, 0, 100, 10);
        assert_eq!(native.rows_of(rect), Some((16, 26)));
        let rect = Rect::new(0, 1000, 1920, 64);
        assert_eq!(native.rows_of(rect), Some((1016, 1080)));
        // The kiosk's frame, twice its size from row 60: two rows each.
        let kiosk = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let rect = Rect::new(0, 5, 800, 3);
        assert_eq!(kiosk.rows_of(rect), Some((70, 76)));
    }

    #[test]
    fn the_scan_out_hash_is_what_the_window_holds() {
        // An 8 x 1 frame on a 32 x 8 screen whose window starts at row 2: 4x,
        // rows 2 to 5, the whole width.
        let mut screen = Screen::new(
            Memory(vec![0; 6 * 64]),
            FramebufferMode::new(32, 8, 64, FramebufferFormat::R5g6b5, 2).unwrap(),
            Size::new(8, 1),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        assert_eq!((screen.placement().scale(), screen.placement().y()), (4, 2));
        let mut written = FrameHash::new();
        let row: Vec<u8> = (0u8..32).map(|b| b.wrapping_mul(37)).collect();
        screen.show_row(0, &row, &mut written).unwrap();
        let mut back = FrameHash::new();
        screen.read_back(|row| back.update(row)).unwrap();
        assert_eq!(back.finish(), written.finish());
    }

    #[test]
    fn a_frame_too_tall_for_twice_its_size_below_the_first_row_is_shown_smaller() {
        // 960 x 540 at 2x would need all 1080 rows, from row 0 above the
        // window's row 16: it is shown at 1x, centred. 960 x 524 is the
        // tallest 960-wide frame shown at 2x.
        let wide = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(960, 540),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let shown = wide.placement();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (1, 480, 270));
        let tallest = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(960, 524),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let shown = tallest.placement();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (2, 0, 16));
    }

    #[test]
    fn a_frame_that_does_not_fit_or_starts_above_the_window_is_refused() {
        let mode = pi5();
        assert_eq!(
            Screen::new(
                pi5_window(),
                mode,
                Size::new(1921, 1),
                PixelFormat::Xrgb8888
            )
            .err(),
            Some(FbError::DoesNotFit {
                frame: Size::new(1921, 1)
            })
        );
        // Full height: it would start at row 0, above the window's row 16.
        assert_eq!(
            Screen::new(
                pi5_window(),
                mode,
                Size::new(1920, 1080),
                PixelFormat::Xrgb8888
            )
            .err(),
            Some(FbError::DoesNotFit {
                frame: Size::new(1920, 1080)
            })
        );
        // A window shorter than the mode's rows.
        assert_eq!(
            Screen::new(
                Memory(vec![0; 3840]),
                mode,
                Size::new(800, 480),
                PixelFormat::Xrgb8888
            )
            .err(),
            Some(FbError::DoesNotFit {
                frame: Size::new(800, 480)
            })
        );
        // A placed row that is not whole words: 801 pixels at 2x, from x 159.
        assert_eq!(
            Screen::new(
                pi5_window(),
                mode,
                Size::new(801, 480),
                PixelFormat::Xrgb8888
            )
            .err(),
            Some(FbError::RowNotWritable)
        );
    }

    /// A frame the size of the window, as a UI at the monitor's own
    /// pixels draws it: shown at 1x from the window's first byte,
    /// no border; a rectangle shown writes exactly the frame's pixels on
    /// its rows, widened to whole words, and nothing else.
    #[test]
    fn a_window_sized_frame_is_shown_at_its_own_pixels_a_rectangle_at_a_time() {
        let mut screen = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(1920, 1064),
            PixelFormat::Rgb565,
        )
        .unwrap();
        let shown = screen.placement();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (1, 0, 16));
        assert_eq!(window_size(&pi5()), Size::new(1920, 1064));
        let frame: Vec<u8> = (0..1920 * 1064 * 2).map(|i| (i % 253) as u8).collect();
        let load = |y: u32, x: u32, out: &mut [u8]| {
            let at = (y as usize * 1920 + x as usize) * 2;
            out.copy_from_slice(&frame[at..at + out.len()]);
            true
        };
        // Columns 5 to 14 of rows 3 and 4: written from 4 to 15, four
        // pixels a word.
        screen.show_rect(Rect::new(5, 3, 10, 2), load).unwrap();
        let memory = screen.into_window().0;
        for row in 0..1064_usize {
            for x in 0..1920_usize {
                let at = row * 3840 + x * 2;
                let written = (3..5).contains(&row) && (4..16).contains(&x);
                let expected = if written {
                    &frame[at..at + 2]
                } else {
                    &[0xEE, 0xEE][..]
                };
                assert_eq!(&memory[at..at + 2], expected, "({x}, {row})");
            }
        }
    }

    /// A rectangle of a frame shown at 2x writes what showing its whole
    /// rows would, on those rows, and nothing beside them.
    #[test]
    fn a_rectangle_of_a_scaled_frame_matches_its_rows_shown_whole() {
        let frame: Vec<u8> = (0..800 * 480 * 4).map(|i| (i % 249) as u8).collect();
        let row = |y: u32| &frame[y as usize * 3200..(y as usize + 1) * 3200];
        let mut whole = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        for y in 0..480 {
            whole.show_row(y, row(y), &mut FrameHash::new()).unwrap();
        }
        let whole = whole.into_window().0;
        let mut part = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        part.show_rect(Rect::new(101, 7, 3, 1), |y, x, out| {
            let at = (y as usize * 800 + x as usize) * 4;
            out.copy_from_slice(&frame[at..at + out.len()]);
            true
        })
        .unwrap();
        let part = part.into_window().0;
        // Frame row 7 is screen rows 74 and 75: window rows 58 and 59;
        // frame columns 100 to 103 (whole words at 2x) are bytes 720 to 735
        // from the left edge at 320.
        for window_row in 0..1064_usize {
            let start = window_row * 3840;
            for byte in 0..3840_usize {
                let written = (58..60).contains(&window_row) && (720..736).contains(&byte);
                let expected = if written { whole[start + byte] } else { 0xEE };
                assert_eq!(
                    part[start + byte],
                    expected,
                    "row {window_row}, byte {byte}"
                );
            }
        }
        // A rectangle outside the frame, or a failed load, is refused.
        let mut screen = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        assert_eq!(
            screen.show_rect(Rect::new(799, 0, 2, 1), |_, _, _| true),
            Err(FbError::BadRow { y: 0 })
        );
        assert_eq!(
            screen.show_rect(Rect::new(0, 5, 1, 1), |_, _, _| false),
            Err(FbError::BadRow { y: 5 })
        );
    }

    #[test]
    fn rows_that_are_not_the_frames_are_refused() {
        let mut screen = Screen::new(
            pi5_window(),
            pi5(),
            Size::new(800, 480),
            PixelFormat::Xrgb8888,
        )
        .unwrap();
        let mut hash = FrameHash::new();
        assert_eq!(
            screen.show_row(480, &[0; 3200], &mut hash),
            Err(FbError::BadRow { y: 480 })
        );
        assert_eq!(
            screen.show_row(0, &[0; 3196], &mut hash),
            Err(FbError::BadRow { y: 0 })
        );
        assert_eq!(hash.finish(), FrameHash::new().finish(), "nothing written");
    }
}
