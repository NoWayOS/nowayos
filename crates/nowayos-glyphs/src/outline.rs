// SPDX-License-Identifier: OFL-1.1
//! The faces as outlines: what the device draws text from at any size and
//! any device scale, with `nowayos-raster`.
//!
//! Each [`Face`] holds, for every character of its set, an [`Outline`] -
//! its advance, the box its points lie in, and its path: lines and
//! quadratic curves, as the font holds them, composites already resolved -
//! with the face's line metrics and pair kerning in font units. Metrics
//! scale to 1/64 pixel at any size as the atlases' did (`Scale::to_64`),
//! so a size an atlas exists for measures exactly as that atlas does.
//!
//! A path's points are in half font units, as TrueType puts the point it
//! implies between two curves' pulls halfway between them: draw them at
//! [`Face::outline_scale`], whose units per em are twice the font's.

use nowayos_raster::{Command, Placement, Point, Scale};

use crate::{KernPair, LineMetrics, NoGlyph};

/// One character's outline, y up: its advance in font units, its path and
/// its box in half font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outline {
    ch: char,
    advance: i32,
    /// Left, bottom, right and top: the box every point lies in, in half
    /// font units.
    bounds: [i16; 4],
    steps: u16,
    points: u16,
    verbs: &'static [u8],
    coords: &'static [i16],
}

impl Outline {
    pub(crate) const fn new(
        ch: char,
        advance: i32,
        bounds: [i16; 4],
        steps: u16,
        points: u16,
    ) -> Self {
        Self {
            ch,
            advance,
            bounds,
            steps,
            points,
            verbs: &[],
            coords: &[],
        }
    }

    /// The character it draws.
    #[must_use]
    pub const fn ch(&self) -> char {
        self.ch
    }

    /// Its advance, in font units.
    #[must_use]
    pub const fn advance(&self) -> i32 {
        self.advance
    }

    /// The bottom-left corner of the box its points lie in, in half font
    /// units.
    #[must_use]
    pub const fn min(&self) -> Point {
        Point::new(self.bounds[0] as i32, self.bounds[1] as i32)
    }

    /// The top-right corner of the box its points lie in, in half font
    /// units.
    #[must_use]
    pub const fn max(&self) -> Point {
        Point::new(self.bounds[2] as i32, self.bounds[3] as i32)
    }

    /// Whether it draws anything: a space has no path.
    #[must_use]
    pub const fn has_ink(&self) -> bool {
        !self.verbs.is_empty()
    }

    /// The ink box it fills at `scale` - the face's
    /// [`outline_scale`](Face::outline_scale) - from the pen on the
    /// baseline; empty for a space.
    #[must_use]
    pub fn placement(&self, scale: Scale) -> Placement {
        if !self.has_ink() {
            return Placement::default();
        }
        Placement::of(self.min(), self.max(), scale).unwrap_or_default()
    }

    /// Its path, in half font units.
    #[must_use]
    pub const fn path(&self) -> Path {
        Path {
            verbs: self.verbs,
            coords: self.coords,
        }
    }
}

/// An outline's path: its steps in order.
#[derive(Clone, Debug)]
pub struct Path {
    verbs: &'static [u8],
    coords: &'static [i16],
}

impl Path {
    fn point(&mut self) -> Option<Point> {
        let (&[x, y], rest) = self.coords.split_first_chunk::<2>()?;
        self.coords = rest;
        Some(Point::new(i32::from(x), i32::from(y)))
    }
}

impl Iterator for Path {
    type Item = Command;

    fn next(&mut self) -> Option<Command> {
        let (&verb, rest) = self.verbs.split_first()?;
        self.verbs = rest;
        Some(match verb {
            b'M' => Command::MoveTo(self.point()?),
            b'L' => Command::LineTo(self.point()?),
            b'Q' => Command::QuadTo(self.point()?, self.point()?),
            b'C' => Command::CurveTo(self.point()?, self.point()?, self.point()?),
            _ => Command::Close,
        })
    }
}

/// One face as outlines: every character of its set, its line metrics and
/// its pair kerning, in font units.
pub struct Face {
    upem: u16,
    line: [i32; 3],
    glyphs: &'static [Outline],
    kerning: &'static [KernPair],
}

impl Face {
    pub(crate) const fn new(
        upem: u16,
        line: [i32; 3],
        glyphs: &'static [Outline],
        kerning: &'static [KernPair],
    ) -> Self {
        Self {
            upem,
            line,
            glyphs,
            kerning,
        }
    }

    /// The font's units per em.
    #[must_use]
    pub const fn upem(&self) -> u16 {
        self.upem
    }

    /// The face's metrics at `size64` 1/64 pixel per em: font units
    /// scaled; `None` for no size or one above
    /// `nowayos_raster::MAX_SIZE64`.
    #[must_use]
    pub const fn scale(&self, size64: u32) -> Option<Scale> {
        Scale::new(size64, self.upem)
    }

    /// The face's outlines at `size64` 1/64 pixel per em: half font units
    /// scaled, twice the font's units per em; `None` for no size, one above
    /// `nowayos_raster::MAX_SIZE64`, or a font of more than 32,767 units
    /// per em.
    #[must_use]
    pub const fn outline_scale(&self, size64: u32) -> Option<Scale> {
        match self.upem.checked_mul(2) {
            Some(halves) => Scale::new(size64, halves),
            None => None,
        }
    }

    /// The face's vertical metrics at `scale`, in 1/64 pixel.
    #[must_use]
    pub const fn line_metrics(&self, scale: Scale) -> LineMetrics {
        LineMetrics::new(
            scale.to_64(self.line[0]),
            scale.to_64(self.line[1]),
            scale.to_64(self.line[2]),
        )
    }

    /// Every outline, in the order of their characters: the face's whole
    /// character set.
    #[must_use]
    pub const fn glyphs(&self) -> &'static [Outline] {
        self.glyphs
    }

    /// The outline that draws `ch`.
    ///
    /// # Errors
    ///
    /// [`NoGlyph`] if `ch` is outside the face's set.
    pub fn glyph(&self, ch: char) -> Result<&'static Outline, NoGlyph> {
        let index = self.index(ch)?;
        self.glyphs.get(index).ok_or(NoGlyph)
    }

    /// The adjustment to the pen between `left` and `right`, in font
    /// units: negative brings them closer. 0 for a pair the font does not
    /// kern, or a character outside the set.
    #[must_use]
    pub fn kerning_units(&self, left: char, right: char) -> i32 {
        let (Ok(left), Ok(right)) = (self.index(left), self.index(right)) else {
            return 0;
        };
        let (Ok(left), Ok(right)) = (u16::try_from(left), u16::try_from(right)) else {
            return 0;
        };
        self.kerning
            .binary_search_by(|pair| (pair.left, pair.right).cmp(&(left, right)))
            .ok()
            .and_then(|found| self.kerning.get(found))
            .map_or(0, |pair| i32::from(pair.units))
    }

    fn index(&self, ch: char) -> Result<usize, NoGlyph> {
        self.glyphs
            .binary_search_by(|glyph| glyph.ch.cmp(&ch))
            .map_err(|_| NoGlyph)
    }
}

/// How many coordinates a step takes: two for each point, `None` for a
/// step that is not one.
const fn coords_of(verb: u8) -> Option<usize> {
    match verb {
        b'M' | b'L' => Some(2),
        b'Q' => Some(4),
        b'C' => Some(6),
        b'Z' => Some(0),
        _ => None,
    }
}

/// `glyphs`, each given its steps and points: the next `steps` bytes of
/// `verbs` and the next `2 * points` values of `coords`, in order.
pub(crate) const fn lay_out_outlines<const N: usize>(
    verbs: &'static [u8],
    coords: &'static [i16],
    mut glyphs: [Outline; N],
) -> [Outline; N] {
    let (mut verbs, mut coords) = (verbs, coords);
    let mut todo: &mut [Outline] = &mut glyphs;
    while let Some((glyph, tail)) = todo.split_first_mut() {
        let (steps, after): (&'static [u8], &'static [u8]) =
            match verbs.split_at_checked(glyph.steps as usize) {
                Some(split) => split,
                None => (&[], &[]),
            };
        let (points, rest): (&'static [i16], &'static [i16]) =
            match coords.split_at_checked((glyph.points as usize).saturating_mul(2)) {
                Some(split) => split,
                None => (&[], &[]),
            };
        glyph.verbs = steps;
        glyph.coords = points;
        verbs = after;
        coords = rest;
        todo = tail;
    }
    glyphs
}

/// Whether `glyphs` are exactly `chars`, in order, each taking exactly its
/// steps and points - every step a known one, taking its points, every
/// point inside the outline's box - and all of them together the whole of
/// `verbs_len` steps and `coords_len` coordinates.
pub(crate) const fn outlines_fit(
    glyphs: &[Outline],
    chars: &[char],
    verbs_len: usize,
    coords_len: usize,
) -> bool {
    if glyphs.len() != chars.len() {
        return false;
    }
    let (mut steps_total, mut coords_total) = (0_usize, 0_usize);
    let (mut glyphs, mut chars) = (glyphs, chars);
    while let (Some((glyph, glyph_rest)), Some((&ch, char_rest))) =
        (glyphs.split_first(), chars.split_first())
    {
        if glyph.ch != ch || glyph.verbs.len() != glyph.steps as usize {
            return false;
        }
        if glyph.coords.len() != (glyph.points as usize).saturating_mul(2) {
            return false;
        }
        let mut taken = 0_usize;
        let mut verbs = glyph.verbs;
        while let Some((&verb, rest)) = verbs.split_first() {
            let Some(count) = coords_of(verb) else {
                return false;
            };
            taken = taken.saturating_add(count);
            verbs = rest;
        }
        if taken != glyph.coords.len() {
            return false;
        }
        let [left, bottom, right, top] = glyph.bounds;
        let mut coords = glyph.coords;
        while let Some((&[x, y], rest)) = coords.split_first_chunk::<2>() {
            if x < left || x > right || y < bottom || y > top {
                return false;
            }
            coords = rest;
        }
        steps_total = steps_total.saturating_add(glyph.verbs.len());
        coords_total = coords_total.saturating_add(glyph.coords.len());
        glyphs = glyph_rest;
        chars = char_rest;
    }
    steps_total == verbs_len && coords_total == coords_len
}
