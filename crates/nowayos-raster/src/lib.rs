// SPDX-License-Identifier: MIT OR Apache-2.0
//! Anti-aliased coverage of filled outlines, drawn at any size on the
//! device, and a bounded cache of what it has drawn.
//!
//! **Provisional**: `nowayos-render-eg` draws text with it; either may
//! change this API.
//!
//! An outline is a path of [`Command`]s in font units, y up - lines and
//! quadratic or cubic curves, each contour closed - as `nowayos-glyphs`
//! holds a glyph's. A [`Scale`] sets its size in 1/64 pixel per em, so any
//! size and any device scale, fractional ones too, is drawn from the one
//! outline; [`Placement::of`] gives the ink box it fills at that size, and
//! [`Rasteriser::render`] fills that box with 8-bit coverage, row by row
//! from the top, nonzero winding.
//!
//! Coverage is the exact area of each pixel the outline covers, its curves
//! cut into lines first: each line adds its signed area to the cells of the
//! pixels it crosses, and a running sum along each row turns those into
//! coverage (the accumulation method of Raph Levien's font-rs, Apache-2.0,
//! here in integer arithmetic). Points are held in 1/256 pixel and areas in
//! 1/65,536 pixel, every product in 64 bits: nothing depends on floating
//! point, so the host, QEMU and the board draw the same bytes.
//!
//! Nothing allocates. A [`Rasteriser`] holds the lines of one outline (at
//! most `LINES`) and `CELLS` cells, which it fills a band of rows at a
//! time, so a glyph of any height is drawn in fixed memory; a row wider
//! than the cells hold, or an outline of more lines, is refused
//! ([`RasterError`]), never cut short. A [`GlyphCache`] keeps what has been
//! drawn, so each glyph at each size is rasterised once.
//!
//! ```
//! use nowayos_raster::{Command, Placement, Point, Rasteriser, Scale};
//!
//! // A square of 1,000 font units, drawn at 16 pixels per em: 16 x 16
//! // pixels, every one covered.
//! let square = [
//!     Command::MoveTo(Point::new(0, 0)),
//!     Command::LineTo(Point::new(1000, 0)),
//!     Command::LineTo(Point::new(1000, 1000)),
//!     Command::LineTo(Point::new(0, 1000)),
//!     Command::Close,
//! ];
//! let scale = Scale::new(16 * 64, 1000).unwrap();
//! let placement = Placement::of(Point::new(0, 0), Point::new(1000, 1000), scale).unwrap();
//! assert_eq!((placement.width(), placement.height(), placement.top()), (16, 16, 16));
//! let mut rasteriser: Rasteriser<64, 1024> = Rasteriser::new();
//! let mut coverage = [0; 16 * 16];
//! rasteriser.render(square, scale, placement, &mut coverage).unwrap();
//! assert!(coverage.iter().all(|&c| c == 255));
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod cache;

pub use cache::{CachedGlyph, GlyphCache, GlyphKey, Slot};

/// One pixel, in 1/256 pixel.
const ONE: i64 = 256;
/// Half a pixel, in 1/256 pixel.
const HALF: i64 = 128;
/// Full coverage of one pixel, in 1/65,536 pixel.
const FULL: i64 = 65_536;
/// Half of [`FULL`], for rounding.
const FULL_HALF: i64 = 32_768;
/// The largest size an outline is drawn at, in 1/64 pixel per em: 4,096
/// pixels, far above any text, so every product below stays in 64 bits.
pub const MAX_SIZE64: u32 = 262_144;
/// The most lines one curve is cut into.
const MAX_STEPS: i64 = 256;

/// A point of an outline, in font units, y up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    /// Rightwards.
    pub x: i32,
    /// Upwards.
    pub y: i32,
}

impl Point {
    /// The point (`x`, `y`).
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// One step of an outline's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// Starts a contour at the point, closing the one before.
    MoveTo(Point),
    /// A straight line to the point.
    LineTo(Point),
    /// A quadratic curve through the first point's pull to the second.
    QuadTo(Point, Point),
    /// A cubic curve through the first two points' pull to the third.
    CurveTo(Point, Point, Point),
    /// Closes the contour: a line back to where it started.
    Close,
}

/// The size an outline is drawn at: `size64` 1/64 pixel per em of `upem`
/// font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scale {
    size64: u32,
    upem: u16,
}

impl Scale {
    /// `size64` 1/64 pixel per em of `upem` font units; `None` for no size,
    /// a size above [`MAX_SIZE64`] or no units per em.
    #[must_use]
    pub const fn new(size64: u32, upem: u16) -> Option<Self> {
        if size64 == 0 || size64 > MAX_SIZE64 || upem == 0 {
            None
        } else {
            Some(Self { size64, upem })
        }
    }

    /// The size, in 1/64 pixel per em.
    #[must_use]
    pub const fn size64(&self) -> u32 {
        self.size64
    }

    /// The font units per em.
    #[must_use]
    pub const fn upem(&self) -> u16 {
        self.upem
    }

    /// `units` at this size in 1/64 pixel, rounded half away from zero - as
    /// the glyph atlases' metrics were scaled.
    ///
    /// ```
    /// use nowayos_raster::Scale;
    ///
    /// // 750 units of 1,000 at 18 pixels per em: 13.5 pixels.
    /// assert_eq!(Scale::new(18 * 64, 1000).unwrap().to_64(750), 864);
    /// ```
    #[must_use]
    pub const fn to_64(&self, units: i32) -> i32 {
        let scaled = div_round(
            (units as i64).saturating_mul(self.size64 as i64),
            self.upem as i64,
        );
        if scaled > i32::MAX as i64 {
            i32::MAX
        } else if scaled < i32::MIN as i64 {
            i32::MIN
        } else {
            scaled as i32
        }
    }

    /// `units` at this size in 1/256 pixel, rounded half away from zero.
    #[must_use]
    pub const fn to_256(&self, units: i32) -> i64 {
        div_round(
            (units as i64)
                .saturating_mul(self.size64 as i64)
                .saturating_mul(4),
            self.upem as i64,
        )
    }
}

/// Where an outline's coverage lies: its ink box, from the pen on the
/// baseline - [`left`](Self::left) pixels right and [`top`](Self::top)
/// pixels up to its top-left corner - and [`width`](Self::width) by
/// [`height`](Self::height) pixels in size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Placement {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl Placement {
    /// The ink box at `scale` of an outline whose every point lies within
    /// `min` and `max` font units: the whole pixels it touches. `None` if
    /// `min` is not below and left of `max`, or the box is too large to
    /// address.
    #[must_use]
    pub fn of(min: Point, max: Point, scale: Scale) -> Option<Self> {
        if min.x > max.x || min.y > max.y {
            return None;
        }
        let left = scale.to_256(min.x).div_euclid(ONE);
        let right = ceil_div(scale.to_256(max.x), ONE);
        let top = ceil_div(scale.to_256(max.y), ONE);
        let bottom = scale.to_256(min.y).div_euclid(ONE);
        Some(Self {
            left: i32::try_from(left).ok()?,
            top: i32::try_from(top).ok()?,
            width: u32::try_from(right.checked_sub(left)?).ok()?,
            height: u32::try_from(top.checked_sub(bottom)?).ok()?,
        })
    }

    /// From the pen to the ink box's left edge, in pixels.
    #[must_use]
    pub const fn left(&self) -> i32 {
        self.left
    }

    /// From the baseline up to the ink box's top edge, in pixels.
    #[must_use]
    pub const fn top(&self) -> i32 {
        self.top
    }

    /// The ink box's width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The ink box's height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How many bytes its coverage takes: one a pixel.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(u64::from(self.width).saturating_mul(u64::from(self.height)))
            .unwrap_or(usize::MAX)
    }

    /// Whether it holds no pixels: a space.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Why an outline was not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// A row of its ink box is wider than the rasteriser's cells hold, or
    /// its coverage would not fit the room given.
    TooLarge,
    /// It is cut into more lines than the rasteriser holds.
    TooComplex,
    /// The buffer given is smaller than its ink box.
    Buffer,
}

impl core::fmt::Display for RasterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the outline's ink box is too large to draw",
            Self::TooComplex => "the outline has too many lines to draw",
            Self::Buffer => "the coverage buffer is smaller than the ink box",
        })
    }
}

/// One line of a flattened outline, in 1/256 pixel from the ink box's
/// top-left corner, y down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Line {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

/// Draws outlines' coverage in fixed memory: at most `LINES` lines an
/// outline, and `CELLS` cells, a band of rows of the ink box at a time.
/// About `LINES * 32 + CELLS * 4` bytes; build it once, where it will stay
/// (a PD's own frame), as a move would copy all of that.
pub struct Rasteriser<const LINES: usize, const CELLS: usize> {
    lines: [Line; LINES],
    count: usize,
    cells: [i32; CELLS],
}

impl<const LINES: usize, const CELLS: usize> Default for Rasteriser<LINES, CELLS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const LINES: usize, const CELLS: usize> Rasteriser<LINES, CELLS> {
    /// An empty rasteriser.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: [Line {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            }; LINES],
            count: 0,
            cells: [0; CELLS],
        }
    }

    /// Fills the first `placement.len()` bytes of `out` with the coverage
    /// of `path` at `scale`, row by row from the top of `placement` - the
    /// ink box [`Placement::of`] gives for the path's bounds at that scale.
    /// Ink outside the box is not drawn.
    ///
    /// # Errors
    ///
    /// [`RasterError::Buffer`] if `out` is smaller than the box,
    /// [`RasterError::TooLarge`] if a row of it is wider than `CELLS - 2`
    /// pixels, [`RasterError::TooComplex`] if the path is cut into more
    /// than `LINES` lines. `out` is left zeroed.
    pub fn render<I: IntoIterator<Item = Command>>(
        &mut self,
        path: I,
        scale: Scale,
        placement: Placement,
        out: &mut [u8],
    ) -> Result<(), RasterError> {
        let len = placement.len();
        let out = out.get_mut(..len).ok_or(RasterError::Buffer)?;
        out.fill(0);
        if placement.is_empty() {
            return Ok(());
        }
        let width = usize::try_from(placement.width).map_err(|_| RasterError::TooLarge)?;
        let stride = width.checked_add(2).ok_or(RasterError::TooLarge)?;
        let band = CELLS.checked_div(stride).unwrap_or(0);
        if band == 0 {
            return Err(RasterError::TooLarge);
        }
        self.flatten(path, scale, placement)?;
        let edge = i64::from(placement.width).saturating_mul(ONE);
        let mut start = 0usize;
        for rows in out.chunks_mut(width.saturating_mul(band)) {
            let count = rows.len().checked_div(width).unwrap_or(0);
            let used = count.saturating_mul(stride);
            if let Some(cells) = self.cells.get_mut(..used) {
                cells.fill(0);
            }
            let band_top = i64::try_from(start).unwrap_or(i64::MAX).saturating_mul(ONE);
            let band_bottom = i64::try_from(start.saturating_add(count))
                .unwrap_or(i64::MAX)
                .saturating_mul(ONE);
            let lines = self.lines.get(..self.count).unwrap_or(&[]);
            for line in lines {
                draw_line(&mut self.cells, *line, band_top, band_bottom, stride, edge);
            }
            for (row, pixels) in rows.chunks_mut(width).enumerate() {
                let first = row.saturating_mul(stride);
                let cells = self
                    .cells
                    .get(first..first.saturating_add(width))
                    .unwrap_or(&[]);
                let mut sum: i64 = 0;
                for (pixel, &cell) in pixels.iter_mut().zip(cells) {
                    sum = sum.saturating_add(i64::from(cell));
                    *pixel = coverage(sum);
                }
            }
            start = start.saturating_add(count);
        }
        Ok(())
    }

    /// Cuts `path` into lines in 1/256 pixel from `placement`'s top-left
    /// corner, y down, closing every contour.
    fn flatten<I: IntoIterator<Item = Command>>(
        &mut self,
        path: I,
        scale: Scale,
        placement: Placement,
    ) -> Result<(), RasterError> {
        self.count = 0;
        let origin_x = i64::from(placement.left).saturating_mul(ONE);
        let origin_y = i64::from(placement.top).saturating_mul(ONE);
        let at = |point: Point| {
            (
                scale.to_256(point.x).saturating_sub(origin_x),
                origin_y.saturating_sub(scale.to_256(point.y)),
            )
        };
        let mut start = (0, 0);
        let mut pen = (0, 0);
        for command in path {
            match command {
                Command::MoveTo(point) => {
                    self.line(pen, start)?;
                    start = at(point);
                    pen = start;
                }
                Command::LineTo(point) => {
                    let to = at(point);
                    self.line(pen, to)?;
                    pen = to;
                }
                Command::QuadTo(control, point) => {
                    let (c, to) = (at(control), at(point));
                    self.quad(pen, c, to)?;
                    pen = to;
                }
                Command::CurveTo(first, second, point) => {
                    let (c1, c2, to) = (at(first), at(second), at(point));
                    self.cubic(pen, c1, c2, to)?;
                    pen = to;
                }
                Command::Close => {
                    self.line(pen, start)?;
                    pen = start;
                }
            }
        }
        self.line(pen, start)
    }

    /// Adds the line from `from` to `to`, unless it is level: a level line
    /// covers nothing.
    fn line(&mut self, from: (i64, i64), to: (i64, i64)) -> Result<(), RasterError> {
        if from.1 == to.1 {
            return Ok(());
        }
        let slot = self
            .lines
            .get_mut(self.count)
            .ok_or(RasterError::TooComplex)?;
        *slot = Line {
            x0: from.0,
            y0: from.1,
            x1: to.0,
            y1: to.1,
        };
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    /// Adds a quadratic curve, cut into as many lines as its bend needs.
    fn quad(
        &mut self,
        from: (i64, i64),
        control: (i64, i64),
        to: (i64, i64),
    ) -> Result<(), RasterError> {
        let bend = bend(from, control, to);
        let steps = steps(bend);
        let whole = steps.saturating_mul(steps);
        let mut pen = from;
        for i in 1..=steps {
            let rest = steps.saturating_sub(i);
            let point = |a: i64, b: i64, c: i64| {
                div_round(
                    rest.saturating_mul(rest)
                        .saturating_mul(a)
                        .saturating_add(i.saturating_mul(rest).saturating_mul(2).saturating_mul(b))
                        .saturating_add(i.saturating_mul(i).saturating_mul(c)),
                    whole,
                )
            };
            let next = if i == steps {
                to
            } else {
                (
                    point(from.0, control.0, to.0),
                    point(from.1, control.1, to.1),
                )
            };
            self.line(pen, next)?;
            pen = next;
        }
        Ok(())
    }

    /// Adds a cubic curve, cut into as many lines as its bends need.
    fn cubic(
        &mut self,
        from: (i64, i64),
        first: (i64, i64),
        second: (i64, i64),
        to: (i64, i64),
    ) -> Result<(), RasterError> {
        let bend = bend(from, first, second).max(bend(first, second, to));
        let steps = steps(bend);
        let whole = steps.saturating_mul(steps).saturating_mul(steps);
        let mut pen = from;
        for i in 1..=steps {
            let rest = steps.saturating_sub(i);
            let point = |a: i64, b: i64, c: i64, d: i64| {
                let terms = rest
                    .saturating_mul(rest)
                    .saturating_mul(rest)
                    .saturating_mul(a)
                    .saturating_add(
                        rest.saturating_mul(rest)
                            .saturating_mul(i)
                            .saturating_mul(3)
                            .saturating_mul(b),
                    )
                    .saturating_add(
                        rest.saturating_mul(i)
                            .saturating_mul(i)
                            .saturating_mul(3)
                            .saturating_mul(c),
                    )
                    .saturating_add(i.saturating_mul(i).saturating_mul(i).saturating_mul(d));
                div_round(terms, whole)
            };
            let next = if i == steps {
                to
            } else {
                (
                    point(from.0, first.0, second.0, to.0),
                    point(from.1, first.1, second.1, to.1),
                )
            };
            self.line(pen, next)?;
            pen = next;
        }
        Ok(())
    }
}

/// How far a curve through `b` bends away from the line `a` to `c`: the
/// larger of `a - 2b + c`'s two components, in 1/256 pixel.
fn bend(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> i64 {
    let dx =
        a.0.saturating_sub(b.0.saturating_mul(2))
            .saturating_add(c.0);
    let dy =
        a.1.saturating_sub(b.1.saturating_mul(2))
            .saturating_add(c.1);
    dx.saturating_abs().max(dy.saturating_abs())
}

/// How many lines a curve of `bend` is cut into, as FreeType's and
/// swash's rasterisers cut one: halved until each half bends at most a
/// quarter pixel - each halving quarters the bend - so no line strays more
/// than 1/32 pixel from the curve; at most [`MAX_STEPS`].
fn steps(bend: i64) -> i64 {
    let mut bend = bend;
    let mut steps = 1;
    while bend > ONE.saturating_div(4) && steps < MAX_STEPS {
        bend = bend.wrapping_shr(2);
        steps = steps.saturating_mul(2);
    }
    steps
}

/// Adds `line`'s signed area to the cells of the rows from `band_top` to
/// `band_bottom` (1/256 pixel from the ink box's top), `stride` cells a
/// row, its x held within 0 and `edge`.
fn draw_line(
    cells: &mut [i32],
    line: Line,
    band_top: i64,
    band_bottom: i64,
    stride: usize,
    edge: i64,
) {
    let (dir, xa, ya, xb, yb) = if line.y0 < line.y1 {
        (1, line.x0, line.y0, line.x1, line.y1)
    } else {
        (-1, line.x1, line.y1, line.x0, line.y0)
    };
    let top = ya.max(band_top);
    let bottom = yb.min(band_bottom);
    if top >= bottom {
        return;
    }
    let span = yb.saturating_sub(ya);
    let x_at = |y: i64| {
        xa.saturating_add(div_round(
            xb.saturating_sub(xa).saturating_mul(y.saturating_sub(ya)),
            span,
        ))
        .clamp(0, edge)
    };
    let stride64 = i64::try_from(stride).unwrap_or(i64::MAX);
    let mut row = top.div_euclid(ONE);
    loop {
        let row_top = row.saturating_mul(ONE);
        if row_top >= bottom {
            break;
        }
        let from = top.max(row_top);
        let to = bottom.min(row_top.saturating_add(ONE));
        let base = row
            .saturating_sub(band_top.div_euclid(ONE))
            .saturating_mul(stride64);
        if to > from {
            let area = to
                .saturating_sub(from)
                .saturating_mul(ONE)
                .saturating_mul(dir);
            add_row(cells, base, x_at(from), x_at(to), area);
        }
        row = row.saturating_add(1);
    }
}

/// Adds `area` (1/65,536 pixel, signed) across one row's cells, from
/// `base`, for an edge crossing the row from x `x_from` to `x_to` (1/256
/// pixel): each cell gets the share that leaves the cells to its right
/// covered, so the row's cells add up to `area` exactly.
fn add_row(cells: &mut [i32], base: i64, x_from: i64, x_to: i64, area: i64) {
    // Shares are worked out on the area's size and signed after, so a
    // contour drawn the other way round adds exactly the opposite.
    let sign = area.signum();
    let area = area.saturating_abs();
    let (x0, x1) = if x_from <= x_to {
        (x_from, x_to)
    } else {
        (x_to, x_from)
    };
    let x0i = x0.div_euclid(ONE);
    let x1i = ceil_div(x1, ONE);
    let mut add = |column: i64, value: i64| {
        let index = base.saturating_add(column);
        if let Some(cell) = usize::try_from(index).ok().and_then(|i| cells.get_mut(i)) {
            let value = i32::try_from(value.saturating_mul(sign)).unwrap_or(0);
            *cell = cell.saturating_add(value);
        }
    };
    if x1i <= x0i.saturating_add(1) {
        // Within one pixel: the share right of the crossing's middle.
        let middle = x_from
            .saturating_add(x_to)
            .saturating_sub(x0i.saturating_mul(ONE).saturating_mul(2));
        let right = div_floor(area.saturating_mul(middle), ONE.saturating_mul(2));
        add(x0i, area.saturating_sub(right));
        add(x0i.saturating_add(1), right);
        return;
    }
    // Across several pixels: the first and last take a triangle, those
    // between an equal slice, and the one before the last what is left.
    let run = x1.saturating_sub(x0);
    let x0f = x0.saturating_sub(x0i.saturating_mul(ONE));
    let x1f = x1.saturating_sub(x1i.saturating_sub(1).saturating_mul(ONE));
    let twice_run = run.saturating_mul(ONE).saturating_mul(2);
    let first_left = ONE.saturating_sub(x0f);
    let first = div_floor(
        area.saturating_mul(first_left.saturating_mul(first_left)),
        twice_run,
    );
    let last = div_floor(area.saturating_mul(x1f.saturating_mul(x1f)), twice_run);
    add(x0i, first);
    add(x1i, last);
    let mut placed = first.saturating_add(last);
    if x1i > x0i.saturating_add(2) {
        let second = div_floor(
            area.saturating_mul(ONE.saturating_add(HALF).saturating_sub(x0f)),
            run,
        )
        .saturating_sub(first);
        add(x0i.saturating_add(1), second);
        placed = placed.saturating_add(second);
        let slice = div_floor(area.saturating_mul(ONE), run);
        let mut column = x0i.saturating_add(2);
        while column < x1i.saturating_sub(1) {
            add(column, slice);
            placed = placed.saturating_add(slice);
            column = column.saturating_add(1);
        }
    }
    add(x1i.saturating_sub(1), area.saturating_sub(placed));
}

/// The coverage byte for a running sum of signed area: its size, at most
/// one pixel's, from 0 to 255, rounded.
fn coverage(sum: i64) -> u8 {
    let covered = sum.saturating_abs().min(FULL);
    u8::try_from(
        covered
            .saturating_mul(255)
            .saturating_add(FULL_HALF)
            .div_euclid(FULL),
    )
    .unwrap_or(u8::MAX)
}

/// `value / by`, rounded half away from zero; 0 when `by` is not positive.
const fn div_round(value: i64, by: i64) -> i64 {
    if by <= 0 {
        return 0;
    }
    let half = by.wrapping_shr(1);
    let magnitude = match value.saturating_abs().saturating_add(half).checked_div(by) {
        Some(quotient) => quotient,
        None => 0,
    };
    if value >= 0 {
        magnitude
    } else {
        magnitude.saturating_neg()
    }
}

/// `value / by`, rounded down; 0 when `by` is not positive.
fn div_floor(value: i64, by: i64) -> i64 {
    if by <= 0 {
        return 0;
    }
    value.div_euclid(by)
}

/// `value / by`, rounded up; 0 when `by` is not positive.
fn ceil_div(value: i64, by: i64) -> i64 {
    if by <= 0 {
        return 0;
    }
    value
        .div_euclid(by)
        .saturating_add(i64::from(value.rem_euclid(by) != 0))
}

#[cfg(test)]
mod tests;
