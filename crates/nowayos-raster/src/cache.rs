// SPDX-License-Identifier: MIT OR Apache-2.0
//! A bounded cache of drawn glyphs, in storage its owner gives it: a
//! buffer of coverage and a table of [`Slot`]s, looked up by [`GlyphKey`]
//! with open addressing. When either fills, it is emptied and starts
//! again - text on a screen uses a few sizes of a few faces, so that is
//! rare, and it keeps the cache simple and every lookup bounded. Its owner
//! chooses where the storage lives: a PD's own frame, or a test's heap.

use crate::{Placement, RasterError};

/// What a cached glyph is found by: the face, its size in 1/64 pixel per
/// em and the character, as the caller numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// The face, as the caller numbers its faces.
    pub face: u16,
    /// The size, in 1/64 pixel per em.
    pub size64: u32,
    /// The character.
    pub ch: char,
}

/// A glyph from the cache: its ink box and its coverage, row by row from
/// the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachedGlyph<'a> {
    placement: Placement,
    coverage: &'a [u8],
}

impl<'a> CachedGlyph<'a> {
    /// Its ink box.
    #[must_use]
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// Its coverage: `placement().len()` bytes, one a pixel.
    #[must_use]
    pub const fn coverage(&self) -> &'a [u8] {
        self.coverage
    }
}

/// One entry of a [`GlyphCache`]'s table: give the cache as many as it
/// should hold glyphs, each [`Slot::EMPTY`].
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    key: Option<GlyphKey>,
    start: usize,
    placement: Placement,
}

impl Slot {
    /// An empty entry.
    pub const EMPTY: Self = EMPTY;
}

const EMPTY: Slot = Slot {
    key: None,
    start: 0,
    placement: Placement {
        left: 0,
        top: 0,
        width: 0,
        height: 0,
    },
};

/// Drawn glyphs, at most as many as its table has slots, three quarters
/// full at most, in the coverage buffer it is given.
pub struct GlyphCache<'a> {
    pixels: &'a mut [u8],
    used: usize,
    slots: &'a mut [Slot],
    filled: usize,
    clears: u32,
}

impl<'a> GlyphCache<'a> {
    /// An empty cache over `pixels` and `slots`; whatever they held is
    /// forgotten.
    #[must_use]
    pub fn new(pixels: &'a mut [u8], slots: &'a mut [Slot]) -> Self {
        slots.fill(EMPTY);
        Self {
            pixels,
            used: 0,
            slots,
            filled: 0,
            clears: 0,
        }
    }

    /// The glyph cached for `key`, if it is.
    #[must_use]
    pub fn get(&self, key: GlyphKey) -> Option<CachedGlyph<'_>> {
        let slot = self.find(key).ok()?;
        self.glyph(slot)
    }

    /// The glyph for `key`: the cached one, or else `draw`n into room the
    /// cache makes for `placement`'s coverage - emptying itself first if it
    /// has none - and kept.
    ///
    /// # Errors
    ///
    /// [`RasterError::TooLarge`] if the coverage is larger than the whole
    /// cache, or what `draw` returns; then nothing is kept.
    pub fn get_or_draw<F>(
        &mut self,
        key: GlyphKey,
        placement: Placement,
        draw: F,
    ) -> Result<CachedGlyph<'_>, RasterError>
    where
        F: FnOnce(&mut [u8]) -> Result<(), RasterError>,
    {
        let index = match self.find(key) {
            Ok(index) => index,
            Err(_) => self.draw(key, placement, draw)?,
        };
        self.glyph(index).ok_or(RasterError::TooLarge)
    }

    /// Empties the cache.
    pub fn clear(&mut self) {
        self.slots.fill(EMPTY);
        self.used = 0;
        self.filled = 0;
        self.clears = self.clears.saturating_add(1);
    }

    /// How many glyphs it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.filled
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// How many bytes of coverage it holds.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    /// How many times it has been emptied, by [`clear`](Self::clear) or
    /// because it was full.
    #[must_use]
    pub const fn clears(&self) -> u32 {
        self.clears
    }

    fn draw<F>(
        &mut self,
        key: GlyphKey,
        placement: Placement,
        draw: F,
    ) -> Result<usize, RasterError>
    where
        F: FnOnce(&mut [u8]) -> Result<(), RasterError>,
    {
        let len = placement.len();
        if len > self.pixels.len() || self.slots.is_empty() {
            return Err(RasterError::TooLarge);
        }
        // At most three quarters full, so a search always ends soon.
        let crowded =
            self.filled.saturating_add(1).saturating_mul(4) > self.slots.len().saturating_mul(3);
        let room = self.pixels.len().saturating_sub(self.used);
        if crowded || len > room {
            self.clear();
        }
        let index = match self.find(key) {
            Err(Some(index)) => index,
            Ok(index) => return Ok(index),
            Err(None) => return Err(RasterError::TooLarge),
        };
        let start = self.used;
        let end = start.checked_add(len).ok_or(RasterError::TooLarge)?;
        let pixels = self
            .pixels
            .get_mut(start..end)
            .ok_or(RasterError::TooLarge)?;
        draw(pixels)?;
        let slot = self.slots.get_mut(index).ok_or(RasterError::TooLarge)?;
        *slot = Slot {
            key: Some(key),
            start,
            placement,
        };
        self.used = end;
        self.filled = self.filled.saturating_add(1);
        Ok(index)
    }

    /// The slot holding `key`, or else the empty slot where it would go
    /// (`None` in a full table).
    fn find(&self, key: GlyphKey) -> Result<usize, Option<usize>> {
        let first = usize::try_from(hash(key))
            .unwrap_or(0)
            .checked_rem(self.slots.len())
            .ok_or(None)?;
        for step in 0..self.slots.len() {
            let index = first
                .saturating_add(step)
                .checked_rem(self.slots.len())
                .ok_or(None)?;
            match self.slots.get(index).and_then(|slot| slot.key) {
                Some(held) if held == key => return Ok(index),
                Some(_) => {}
                None => return Err(Some(index)),
            }
        }
        Err(None)
    }

    fn glyph(&self, index: usize) -> Option<CachedGlyph<'_>> {
        let slot = self.slots.get(index)?;
        let end = slot.start.checked_add(slot.placement.len())?;
        Some(CachedGlyph {
            placement: slot.placement,
            coverage: self.pixels.get(slot.start..end)?,
        })
    }
}

/// Mixes a key's three numbers into one (Fibonacci hashing).
fn hash(key: GlyphKey) -> u64 {
    let folded = u64::from(key.face).wrapping_shl(53)
        ^ u64::from(key.size64).wrapping_shl(21)
        ^ u64::from(key.ch);
    folded.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_right(29)
}
