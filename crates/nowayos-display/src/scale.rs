// SPDX-License-Identifier: MIT OR Apache-2.0
//! Showing a frame on a screen: at the screen's own pixels, as a UI drawing
//! at the monitor's resolution does, or - a frame smaller than the
//! screen - scaled up by a whole number and centred ([`Placement`]), one
//! row at a time, converted to the screen's pixel format ([`scale_row`]) -
//! how a display PD showed the UI's 800 x 480 frames on the Pi 5's 1920 x
//! 1080 HDMI framebuffer, and still does for a system built for
//! them - and the display test pattern every display PD draws first
//! ([`test_pattern`]).
//!
//! A whole-number scale keeps every source pixel a square block of screen
//! pixels: no filtering, so the screen shows exactly the frame the UI drew
//! and a hash of what was written can be computed on the host.

use crate::{DEFAULT_LAYOUT, DeviceScale, FramebufferLayout, PixelFormat, Size, scale_for};

/// The smallest screen, in layout units, NoWayOS's screens are made for:
/// the kiosk's 800 x 480, the Raspberry Pi Touch Display's.
pub const LEAST_SCREEN: Size = Size::new(800, 480);

/// The device scale a display announces for a screen `pixels` in size
/// when no product says otherwise: the largest quarter step from 1 to 4
/// that leaves at least [`DEFAULT_LAYOUT`]'s 960 x 540 layout units
/// ([`scale_for`]) - 2 for the Pi 5's 1920 x 1080, 4 for 3840 x 2160, 2.5
/// for 2560 x 1440 (1024 x 576 units), 1 for the kiosk's 800 x 480.
///
/// ```
/// use nowayos_display::{DeviceScale, Size, device_scale};
///
/// assert_eq!(device_scale(Size::new(1920, 1080)), DeviceScale::TWO);
/// assert_eq!(device_scale(Size::new(800, 480)), DeviceScale::ONE);
/// assert_eq!(device_scale(Size::new(3840, 2160)), DeviceScale::MAX);
/// ```
#[must_use]
pub fn device_scale(pixels: Size) -> DeviceScale {
    scale_for(pixels, DEFAULT_LAYOUT)
}

/// Where and how large a frame is shown on a screen: scaled by
/// [`scale`](Self::scale) in both directions, its top-left corner at
/// ([`x`](Self::x), [`y`](Self::y)).
///
/// ```
/// use nowayos_display::{Placement, Size};
///
/// // The kiosk's 800 x 480 UI on a 1920 x 1080 monitor: twice the size,
/// // with a border of 160 pixels left and right and 60 above and below.
/// let shown = Placement::centred(Size::new(800, 480), Size::new(1920, 1080)).unwrap();
/// assert_eq!((shown.scale(), shown.x(), shown.y()), (2, 160, 60));
/// assert_eq!(shown.size(), Size::new(1600, 960));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    scale: u32,
    x: u32,
    y: u32,
    size: Size,
}

impl Placement {
    /// The largest whole-number scale at which `frame` fits `screen`,
    /// centred, and starts no higher than row `top`: the frame keeps clear
    /// of the screen's first `top` rows, and as many below it - a smaller
    /// scale if need be. How a display PD places a frame where the
    /// rows above `top` are not its to write: on the Pi 5, whose firmware
    /// framebuffer's first 16 rows hold the kernel's objects, an
    /// 800 x 480 frame is still shown at 2x, but a 960 x 540 one at 1x, as
    /// 2x would need all 1080 rows. `None` if no scale fits.
    ///
    /// ```
    /// use nowayos_display::{Placement, Size};
    ///
    /// let screen = Size::new(1920, 1080);
    /// let kiosk = Placement::centred_below(Size::new(800, 480), screen, 16).unwrap();
    /// assert_eq!((kiosk.scale(), kiosk.x(), kiosk.y()), (2, 160, 60));
    /// let wide = Placement::centred_below(Size::new(960, 540), screen, 16).unwrap();
    /// assert_eq!((wide.scale(), wide.x(), wide.y()), (1, 480, 270));
    /// ```
    #[must_use]
    pub fn centred_below(frame: Size, screen: Size, top: u32) -> Option<Self> {
        let rows = screen.height.checked_sub(top.checked_mul(2)?)?;
        let scale = Self::centred(frame, Size::new(screen.width, rows))?.scale;
        let size = Size::new(
            frame.width.checked_mul(scale)?,
            frame.height.checked_mul(scale)?,
        );
        Some(Self {
            scale,
            x: screen.width.checked_sub(size.width)?.checked_div(2)?,
            y: screen.height.checked_sub(size.height)?.checked_div(2)?,
            size,
        })
    }

    /// Where a display PD shows `frame` on `screen` when the screen's first
    /// `top` rows are not its to write: centred, clear of them, at
    /// the largest whole-number scale ([`centred_below`](Self::centred_below)),
    /// as the kiosk's 800 x 480 is shown at 2x on the Pi 5; or, a frame too
    /// tall for that, at its own size in the rows below them, centred
    /// across and down those rows, as a 1920 x 1064 frame is shown from
    /// row 16 of a 1920 x 1080 screen with no border. `None` if it fits
    /// neither way.
    ///
    /// ```
    /// use nowayos_display::{Placement, Size};
    ///
    /// let screen = Size::new(1920, 1080);
    /// let kiosk = Placement::in_window(Size::new(800, 480), screen, 16).unwrap();
    /// assert_eq!((kiosk.scale(), kiosk.x(), kiosk.y()), (2, 160, 60));
    /// let native = Placement::in_window(Size::new(1920, 1064), screen, 16).unwrap();
    /// assert_eq!((native.scale(), native.x(), native.y()), (1, 0, 16));
    /// assert_eq!(Placement::in_window(Size::new(1920, 1065), screen, 16), None);
    /// ```
    #[must_use]
    pub fn in_window(frame: Size, screen: Size, top: u32) -> Option<Self> {
        if let Some(centred) = Self::centred_below(frame, screen, top) {
            return Some(centred);
        }
        // At its own size, never scaled: a larger scale would have been
        // centred clear of the top rows above.
        let rows = screen.height.checked_sub(top)?;
        if frame.width == 0 || frame.height == 0 {
            return None;
        }
        Some(Self {
            scale: 1,
            x: screen.width.checked_sub(frame.width)?.checked_div(2)?,
            y: rows
                .checked_sub(frame.height)?
                .checked_div(2)?
                .checked_add(top)?,
            size: frame,
        })
    }

    /// The largest whole-number scale at which `frame` fits `screen`,
    /// centred (any odd pixel left over goes right and below). `None` if
    /// either is empty or the frame does not fit at its own size.
    #[must_use]
    pub fn centred(frame: Size, screen: Size) -> Option<Self> {
        let across = screen.width.checked_div(frame.width)?;
        let down = screen.height.checked_div(frame.height)?;
        let scale = across.min(down);
        if scale == 0 {
            return None;
        }
        let size = Size::new(
            frame.width.checked_mul(scale)?,
            frame.height.checked_mul(scale)?,
        );
        Some(Self {
            scale,
            x: screen.width.checked_sub(size.width)?.checked_div(2)?,
            y: screen.height.checked_sub(size.height)?.checked_div(2)?,
            size,
        })
    }

    /// How many screen pixels each frame pixel becomes, across and down.
    #[must_use]
    pub const fn scale(&self) -> u32 {
        self.scale
    }

    /// The screen column of the frame's left edge.
    #[must_use]
    pub const fn x(&self) -> u32 {
        self.x
    }

    /// The screen row of the frame's top edge.
    #[must_use]
    pub const fn y(&self) -> u32 {
        self.y
    }

    /// The frame's size on screen.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }
}

/// Converts one row of pixels in format `from` to format `to`, each pixel
/// written `scale` times side by side, into the start of `out`. Returns the
/// bytes written: `row`'s pixels × `scale` × `to`'s bytes a pixel.
///
/// `None`, writing nothing, if `row` is not whole pixels of `from`, `out`
/// is too short, or `scale` is 0.
///
/// A row already in `to`, at scale 1, in a format with no unused bytes
/// (`Rgb565`, `Rgb888`, `Bgr888`), is copied as it is: what converting each
/// pixel would write, at memory speed (the display PD's copy of a
/// 1080p frame was bound by the per-pixel path, not by the framebuffer).
///
/// ```
/// use nowayos_display::{PixelFormat, scale_row};
///
/// // One red and one blue Xrgb8888 pixel, twice the width, in Rgb565.
/// let row = [0, 0, 0xFF, 0, 0xFF, 0, 0, 0];
/// let mut out = [0u8; 8];
/// assert_eq!(scale_row(PixelFormat::Xrgb8888, &row, PixelFormat::Rgb565, 2, &mut out), Some(8));
/// assert_eq!(out, [0x00, 0xF8, 0x00, 0xF8, 0x1F, 0x00, 0x1F, 0x00]);
/// ```
#[must_use]
pub fn scale_row(
    from: PixelFormat,
    row: &[u8],
    to: PixelFormat,
    scale: u32,
    out: &mut [u8],
) -> Option<usize> {
    let read = usize::from(from.bytes_per_pixel());
    let write = usize::from(to.bytes_per_pixel());
    let copies = usize::try_from(scale).ok().filter(|&n| n > 0)?;
    if !row.len().is_multiple_of(read) {
        return None;
    }
    let pixels = row.len().checked_div(read)?;
    let needed = pixels.checked_mul(copies)?.checked_mul(write)?;
    let out = out.get_mut(..needed)?;
    if from == to
        && copies == 1
        && matches!(
            from,
            PixelFormat::Rgb565 | PixelFormat::Rgb888 | PixelFormat::Bgr888
        )
    {
        out.copy_from_slice(row);
        return Some(needed);
    }
    let mut encoded = [0u8; 4];
    let encoded = encoded.get_mut(..write)?;
    for (pixel, block) in row
        .chunks_exact(read)
        .zip(out.chunks_exact_mut(write.checked_mul(copies)?))
    {
        let (r, g, b) = from.decode(pixel)?;
        to.encode(r, g, b, encoded)?;
        for copy in block.chunks_exact_mut(write) {
            copy.copy_from_slice(encoded);
        }
    }
    Some(needed)
}

/// The kiosk's frame: what a display PD asks a UI for when the
/// framebuffer region their system shares is too small for the screen at
/// its own pixels - an older system - 800 x 480 `Xrgb8888` at
/// scale 1, which the display shows at the largest whole scale that fits.
pub const KIOSK_FRAME: Size = Size::new(800, 480);

/// The frame a display PD asks its UI for ([`frame_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameChoice {
    /// The frame's layout, packed.
    pub layout: FramebufferLayout,
    /// Its device scale.
    pub scale: DeviceScale,
    /// Whether it is the screen at its own pixels; else the kiosk's frame.
    pub native: bool,
}

/// The frame a display PD asks its UI for, given the screen it
/// shows - `pixels` in `format`, the board's window or a stand-in - and the
/// bytes of the framebuffer region it shares with the UI: the screen at its
/// own pixels and device scale ([`device_scale`]) when the region holds a
/// frame that size; else the kiosk's frame ([`KIOSK_FRAME`], `Xrgb8888`,
/// scale 1), for a system built for it; `None` if the region holds neither.
///
/// ```
/// use nowayos_display::{PixelFormat, Size, frame_for};
///
/// let window = Size::new(1920, 1080);
/// let native = frame_for(window, PixelFormat::Rgb565, 0x40_0000).unwrap();
/// assert_eq!((native.layout.size(), native.scale.quarters(), native.native), (window, 8, true));
/// let kiosk = frame_for(window, PixelFormat::Rgb565, 0x17_7000).unwrap();
/// assert_eq!((kiosk.layout.size(), kiosk.scale.quarters(), kiosk.native), (Size::new(800, 480), 4, false));
/// assert_eq!(frame_for(window, PixelFormat::Rgb565, 4096), None);
/// ```
#[must_use]
pub fn frame_for(pixels: Size, format: PixelFormat, region_len: usize) -> Option<FrameChoice> {
    let native = FramebufferLayout::packed(pixels.width, pixels.height, format).ok()?;
    if native.byte_len() <= region_len {
        return Some(FrameChoice {
            layout: native,
            scale: device_scale(pixels),
            native: true,
        });
    }
    let kiosk =
        FramebufferLayout::packed(KIOSK_FRAME.width, KIOSK_FRAME.height, PixelFormat::Xrgb8888)
            .ok()?;
    (kiosk.byte_len() <= region_len).then_some(FrameChoice {
        layout: kiosk,
        scale: DeviceScale::ONE,
        native: false,
    })
}

/// The display test pattern's size: the kiosk's 800 x 480 screen.
pub const TEST_PATTERN: Size = Size::new(800, 480);

/// Rows of colour bars in the test pattern; the rows below them are a grey
/// ramp.
const BAR_ROWS: u32 = 320;

/// The test pattern's colour bars, left to right, each 100 pixels wide:
/// white, yellow, cyan, green, magenta, red, blue, black. Swapped red and
/// blue show at once: yellow and cyan, and red and blue, change places.
pub const TEST_PATTERN_BARS: [(&str, (u8, u8, u8)); 8] = [
    ("white", (0xFF, 0xFF, 0xFF)),
    ("yellow", (0xFF, 0xFF, 0x00)),
    ("cyan", (0x00, 0xFF, 0xFF)),
    ("green", (0x00, 0xFF, 0x00)),
    ("magenta", (0xFF, 0x00, 0xFF)),
    ("red", (0xFF, 0x00, 0x00)),
    ("blue", (0x00, 0x00, 0xFF)),
    ("black", (0x00, 0x00, 0x00)),
];

/// The colour of the display test pattern at `(x, y)`, as (red, green,
/// blue): eight colour bars ([`TEST_PATTERN_BARS`]) over the top 320 rows,
/// and below them a grey ramp, black at the left to white at the right.
/// Black outside [`TEST_PATTERN`]. QEMU's ramfb display PD and the
/// Pi 5's framebuffer test both draw it.
#[must_use]
pub fn test_pattern(x: u32, y: u32) -> (u8, u8, u8) {
    if x >= TEST_PATTERN.width || y >= TEST_PATTERN.height {
        return (0, 0, 0);
    }
    if y < BAR_ROWS {
        return x
            .checked_div(100)
            .and_then(|bar| TEST_PATTERN_BARS.get(usize::try_from(bar).ok()?))
            .map_or((0, 0, 0), |&(_, colour)| colour);
    }
    // Black at x = 0 to white at x = 799.
    let last = TEST_PATTERN.width.saturating_sub(1);
    let grey = x.saturating_mul(255).checked_div(last).unwrap_or(0);
    let grey = u8::try_from(grey).unwrap_or(u8::MAX);
    (grey, grey, grey)
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
    use proptest::prelude::*;

    #[test]
    fn the_ui_on_the_pi_5s_monitor_is_twice_its_size_and_centred() {
        let shown = Placement::centred(Size::new(800, 480), Size::new(1920, 1080)).unwrap();
        assert_eq!((shown.scale(), shown.x(), shown.y()), (2, 160, 60));
        // The same size: shown as it is, at the origin.
        let same = Placement::centred(Size::new(800, 480), Size::new(800, 480)).unwrap();
        assert_eq!((same.scale(), same.x(), same.y()), (1, 0, 0));
        // One pixel too wide, or nothing at all: nowhere to show it.
        assert_eq!(
            Placement::centred(Size::new(801, 480), Size::new(800, 480)),
            None
        );
        assert_eq!(Placement::centred(Size::ZERO, Size::new(800, 480)), None);
        assert_eq!(Placement::centred(Size::new(1, 1), Size::ZERO), None);
    }

    #[test]
    fn a_frame_shown_below_the_top_rows_is_never_scaled_up() {
        // A small frame on a screen whose top half is not the PD's to
        // write: centred_below finds no rows, so it goes at its own size,
        // centred in the rows below (proptest found it once scaled to 2x).
        let placed = Placement::in_window(Size::new(1, 1), Size::new(2, 38), 19).unwrap();
        assert_eq!((placed.scale(), placed.x(), placed.y()), (1, 0, 28));
        assert_eq!(placed.size(), Size::new(1, 1));
        assert_eq!(
            Placement::in_window(Size::new(1, 20), Size::new(2, 38), 19),
            None
        );
    }

    proptest! {
        #[test]
        fn a_placement_always_fits_the_screen_and_no_larger_scale_would(
            fw in 1u32..5000, fh in 1u32..5000, sw in 0u32..20000, sh in 0u32..20000,
        ) {
            match Placement::centred(Size::new(fw, fh), Size::new(sw, sh)) {
                Some(p) => {
                    let (w, h) = (p.size().width, p.size().height);
                    prop_assert_eq!((w, h), (fw * p.scale(), fh * p.scale()));
                    prop_assert!(p.x() + w <= sw && p.y() + h <= sh);
                    prop_assert!(fw * (p.scale() + 1) > sw || fh * (p.scale() + 1) > sh);
                    prop_assert!(p.x() <= sw - w - p.x() && sw - w - p.x() <= p.x() + 1);
                }
                None => prop_assert!(fw > sw || fh > sh),
            }
        }

        #[test]
        fn a_placement_below_a_row_keeps_clear_of_it(
            fw in 1u32..3000, fh in 1u32..3000, sw in 0u32..5000, sh in 0u32..5000, top in 0u32..64,
        ) {
            match Placement::centred_below(Size::new(fw, fh), Size::new(sw, sh), top) {
                Some(p) => {
                    let (w, h) = (p.size().width, p.size().height);
                    prop_assert_eq!((w, h), (fw * p.scale(), fh * p.scale()));
                    prop_assert!(p.x() + w <= sw && p.y() >= top && p.y() + h <= sh - top);
                    // No larger scale keeps clear of the top rows.
                    prop_assert!(fw * (p.scale() + 1) > sw || fh * (p.scale() + 1) + 2 * top > sh);
                }
                None => prop_assert!(fw > sw || fh + 2 * top > sh),
            }
        }

        #[test]
        fn a_placement_in_the_window_keeps_below_its_first_rows(
            fw in 1u32..3000, fh in 1u32..3000, sw in 0u32..5000, sh in 0u32..5000, top in 0u32..64,
        ) {
            match Placement::in_window(Size::new(fw, fh), Size::new(sw, sh), top) {
                Some(p) => {
                    let (w, h) = (p.size().width, p.size().height);
                    prop_assert_eq!((w, h), (fw * p.scale(), fh * p.scale()));
                    prop_assert!(p.x() + w <= sw && p.y() >= top && p.y() + h <= sh);
                    // Centred clear of the top rows whenever it can be.
                    if let Some(centred) = Placement::centred_below(Size::new(fw, fh), Size::new(sw, sh), top) {
                        prop_assert_eq!(p, centred);
                    } else {
                        prop_assert_eq!(p.scale(), 1);
                    }
                }
                None => prop_assert!(fw > sw || fh + top > sh),
            }
        }

        #[test]
        fn a_scaled_row_repeats_each_converted_pixel(
            pixels in proptest::collection::vec(any::<[u8; 4]>(), 0..40), scale in 1u32..4,
        ) {
            let row: std::vec::Vec<u8> = pixels.iter().flatten().copied().collect();
            let mut out = std::vec![0u8; pixels.len() * 2 * scale as usize];
            let written = scale_row(PixelFormat::Xrgb8888, &row, PixelFormat::Rgb565, scale, &mut out);
            prop_assert_eq!(written, Some(out.len()));
            for (index, pixel) in pixels.iter().enumerate() {
                let mut one = [0u8; 2];
                PixelFormat::Rgb565.encode(pixel[2], pixel[1], pixel[0], &mut one).unwrap();
                for copy in 0..scale as usize {
                    let at = (index * scale as usize + copy) * 2;
                    prop_assert_eq!(&out[at..at + 2], &one[..]);
                }
            }
        }
    }

    #[test]
    fn the_device_scale_keeps_at_least_960_by_540_units() {
        for (pixels, quarters) in [
            ((1920, 1080), 8),
            ((3840, 2160), 16),
            ((2560, 1440), 10),
            ((2400, 1440), 10),
            ((1600, 960), 6),
            ((1920, 1064), 7),
            ((1280, 720), 5),
            ((800, 480), 4),
            ((799, 479), 4),
            ((7680, 4320), 16),
            ((0, 0), 4),
        ] {
            assert_eq!(
                device_scale(Size::new(pixels.0, pixels.1)).quarters(),
                quarters,
                "{pixels:?}"
            );
        }
    }

    /// Both of a display PD's choices: the screen at its own pixels in a
    /// region that holds it; the kiosk's 800 x 480 frames, at scale 1, in a
    /// older system, whose region holds 1,536,000 bytes; and
    /// nothing in a region too small for either.
    #[test]
    fn a_display_asks_for_its_screen_or_else_the_kiosks_frame() {
        let window = Size::new(1920, 1080);
        for (region, size, format, scale, native) in [
            (0x40_0000, window, PixelFormat::Rgb565, 2, true),
            (4_147_200, window, PixelFormat::Rgb565, 2, true),
            (4_147_199, KIOSK_FRAME, PixelFormat::Xrgb8888, 1, false),
            (0x17_7000, KIOSK_FRAME, PixelFormat::Xrgb8888, 1, false),
            (1_536_000, KIOSK_FRAME, PixelFormat::Xrgb8888, 1, false),
        ] {
            let choice = frame_for(window, PixelFormat::Rgb565, region).unwrap();
            assert_eq!(
                (
                    choice.layout.size(),
                    choice.layout.format(),
                    choice.scale.quarters(),
                    choice.native
                ),
                (size, format, scale * 4, native),
                "{region} bytes"
            );
            assert!(choice.layout.byte_len() <= region);
        }
        assert_eq!(frame_for(window, PixelFormat::Rgb565, 1_535_999), None);
        assert_eq!(frame_for(Size::ZERO, PixelFormat::Rgb565, 0x40_0000), None);
        // An 800 x 480 screen is its own frame at scale 1.
        let small = frame_for(KIOSK_FRAME, PixelFormat::Rgb565, 0x40_0000).unwrap();
        assert_eq!((small.scale, small.native), (DeviceScale::ONE, true));
    }

    #[test]
    fn scale_row_copies_a_row_already_in_the_format_as_converting_would() {
        // The fast path: for every format with no unused bytes, a row in
        // that format at scale 1 comes out exactly as converting each pixel
        // (decode, then encode) writes it.
        let row: [u8; 48] = core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11));
        for format in [
            PixelFormat::Rgb565,
            PixelFormat::Rgb888,
            PixelFormat::Bgr888,
        ] {
            let mut fast = [0u8; 48];
            assert_eq!(scale_row(format, &row, format, 1, &mut fast), Some(48));
            let mut slow = [0u8; 48];
            let size = usize::from(format.bytes_per_pixel());
            for (pixel, out) in row.chunks_exact(size).zip(slow.chunks_exact_mut(size)) {
                let (r, g, b) = format.decode(pixel).unwrap();
                format.encode(r, g, b, out).unwrap();
            }
            assert_eq!(fast, slow, "{format:?}");
        }
        // A format with an unused byte is still converted: that byte is
        // written as zero.
        let mut out = [0xAAu8; 8];
        let row = [1, 2, 3, 0xFF, 4, 5, 6, 0xFF];
        assert_eq!(
            scale_row(
                PixelFormat::Xrgb8888,
                &row,
                PixelFormat::Xrgb8888,
                1,
                &mut out
            ),
            Some(8)
        );
        assert_eq!(out, [1, 2, 3, 0, 4, 5, 6, 0]);
    }

    #[test]
    fn scale_row_refuses_part_pixels_short_output_and_no_scale() {
        let mut out = [0xAAu8; 8];
        assert_eq!(
            scale_row(
                PixelFormat::Xrgb8888,
                &[0; 5],
                PixelFormat::Rgb565,
                1,
                &mut out
            ),
            None
        );
        assert_eq!(
            scale_row(
                PixelFormat::Xrgb8888,
                &[0; 8],
                PixelFormat::Rgb565,
                3,
                &mut out
            ),
            None
        );
        assert_eq!(
            scale_row(
                PixelFormat::Xrgb8888,
                &[0; 8],
                PixelFormat::Rgb565,
                0,
                &mut out
            ),
            None
        );
        assert_eq!(out, [0xAA; 8], "nothing written");
        assert_eq!(
            scale_row(PixelFormat::Xrgb8888, &[], PixelFormat::Rgb565, 2, &mut out),
            Some(0)
        );
    }

    #[test]
    fn the_test_pattern_is_eight_bars_over_a_grey_ramp() {
        let names: std::vec::Vec<&str> = (0..8).map(|bar| TEST_PATTERN_BARS[bar].0).collect();
        assert_eq!(
            names,
            [
                "white", "yellow", "cyan", "green", "magenta", "red", "blue", "black"
            ]
        );
        assert_eq!(test_pattern(0, 0), (0xFF, 0xFF, 0xFF));
        assert_eq!(test_pattern(150, 10), (0xFF, 0xFF, 0x00));
        assert_eq!(test_pattern(599, 319), (0xFF, 0x00, 0x00));
        assert_eq!(test_pattern(799, 0), (0, 0, 0));
        assert_eq!(test_pattern(0, 320), (0, 0, 0));
        assert_eq!(test_pattern(799, 479), (0xFF, 0xFF, 0xFF));
        assert_eq!(test_pattern(400, 400), (127, 127, 127));
        assert_eq!(test_pattern(800, 0), (0, 0, 0), "outside");
        assert_eq!(test_pattern(0, 480), (0, 0, 0), "outside");
    }
}
