// SPDX-License-Identifier: MIT OR Apache-2.0
//! A framebuffer-sized region, as bytes.
//!
//! The UI PD draws its frames into a region the display PD also maps: read-
//! write in the UI PD, read-only in the display PD.
//! Either PD may be compromised, so each sees the region
//! as atomic bytes - every bit pattern valid, every access atomic - and
//! neither can make the other's view undefined behaviour:
//!
//! * a [`Frame<ReadWrite>`] is [`FrameMemory`]: `nowayos-render-eg`'s
//!   `RawFramebuffer` draws straight into it, with relaxed byte stores, which
//!   compile to ordinary stores;
//! * a [`Frame<ReadOnly>`](crate::ReadOnly) only loads, so the display PD
//!   cannot be handed a view it could write through:
//!
//! ```compile_fail
//! fn draw_into<M: nowayos_display::FrameMemory>(_: M) {}
//! let frame = nowayos_region::read_only(0).unwrap().into_frame(4096).unwrap();
//! draw_into(frame); // a read-only frame has no store
//! ```
//!
//! The pixels reach the display PD when the UI PD publishes its `Present`
//! message with release ordering (the ring does), and the display PD loads
//! them after reading that message with acquire ordering.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU8, Ordering};

use nowayos_display::FrameMemory;

use crate::RegionError;
use crate::mapping::{Access, ReadWrite, Region};

/// The alignment a frame's region must have: a page, as every Microkit
/// region does.
pub const FRAME_ALIGN: usize = 4096;

/// A whole region as bytes, with the region's rights `A`.
#[derive(Debug)]
pub struct Frame<A: Access> {
    bytes: &'static [AtomicU8],
    access: PhantomData<A>,
}

impl<A: Access> Region<A> {
    /// Views the whole region as a frame of bytes, which must hold at least
    /// `needed` of them - a framebuffer layout's `byte_len`.
    ///
    /// # Errors
    ///
    /// [`RegionError::Misaligned`] if the region is not page-aligned;
    /// [`RegionError::TooSmall`] if it is smaller than `needed`;
    /// [`RegionError::OutOfRange`] if it runs past the end of the address
    /// space.
    pub fn into_frame(self, needed: usize) -> Result<Frame<A>, RegionError> {
        if !self.vaddr().is_multiple_of(FRAME_ALIGN) {
            return Err(RegionError::Misaligned {
                vaddr: self.vaddr(),
                align: FRAME_ALIGN,
            });
        }
        if self.size() < needed {
            return Err(RegionError::TooSmall {
                size: self.size(),
                needed,
            });
        }
        Ok(Frame {
            bytes: self.view_bytes()?,
            access: PhantomData,
        })
    }
}

impl<A: Access> Frame<A> {
    /// The region's size in bytes: the frame covers exactly the region.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the frame has no bytes (never, once bound).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Copies the bytes at `offset` into `out`. Returns `false` (copying
    /// nothing) if they are not all inside the frame.
    pub fn load(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some(span) = offset
            .checked_add(out.len())
            .and_then(|end| self.bytes.get(offset..end))
        else {
            return false;
        };
        for (byte, cell) in out.iter_mut().zip(span) {
            *byte = cell.load(Ordering::Relaxed);
        }
        true
    }
}

impl FrameMemory for Frame<ReadWrite> {
    fn frame_len(&self) -> usize {
        self.len()
    }

    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let mut shared = self.bytes;
        shared.store(offset, bytes)
    }

    /// Within the frame, as the bytes copy within themselves: what a scroll
    /// moves of what is drawn (#10073), rather than drawing it again.
    fn copy_within(&mut self, from: usize, to: usize, len: usize) -> bool {
        let mut shared = self.bytes;
        shared.copy_within(from, to, len)
    }
}

/// A borrowed frame draws the same, so a PD can keep its frame and lend it
/// to the renderer for each frame it draws.
impl FrameMemory for &mut Frame<ReadWrite> {
    fn frame_len(&self) -> usize {
        self.len()
    }

    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        (**self).store(offset, bytes)
    }

    fn copy_within(&mut self, from: usize, to: usize, len: usize) -> bool {
        (**self).copy_within(from, to, len)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failed check panics")]
mod tests {
    //! These run under Miri too. One test page stands in for the region
    //! both PDs map.

    use nowayos_display::{FramebufferLayout, PixelFormat};

    use super::*;
    use crate::ReadOnly;
    use crate::mapping::testing::{PAGE, TestPage};

    #[test]
    fn the_ui_draws_and_the_display_reads_the_same_bytes() {
        let page = TestPage::fresh();
        let mut ui = Region::<ReadWrite>::test_page(page)
            .into_frame(PAGE)
            .unwrap();
        let display = Region::<ReadOnly>::test_page(page)
            .into_frame(PAGE)
            .unwrap();
        assert_eq!(
            (ui.len(), display.len()),
            (PAGE, PAGE),
            "the frame covers exactly the region"
        );
        assert!(!ui.is_empty());

        let layout = FramebufferLayout::packed(32, 32, PixelFormat::Xrgb8888).unwrap();
        assert!(layout.store_pixel(&mut ui, 1, 0, (0x11, 0x22, 0x33)));
        assert!(!layout.store_pixel(&mut ui, 32, 0, (1, 1, 1)), "off-screen");
        let mut pixel = [0; 4];
        assert!(display.load(4, &mut pixel));
        assert_eq!(pixel, [0x33, 0x22, 0x11, 0]);
        assert_eq!(page.byte(6), 0x11);

        assert!(!display.load(PAGE - 3, &mut pixel), "past the end");
        assert!(!display.load(usize::MAX, &mut pixel));
        assert!(!ui.store(PAGE - 1, &[1, 2]));
    }

    #[test]
    fn a_frame_copies_within_itself_as_a_scroll_moves_what_is_drawn() {
        let page = TestPage::fresh();
        let mut ui = Region::<ReadWrite>::test_page(page)
            .into_frame(PAGE)
            .unwrap();
        assert!(ui.store(0, &[1, 2, 3, 4, 5, 6]));
        // Overlapping, forward and back, through a borrow as the renderer
        // holds it.
        let mut lent = &mut ui;
        assert!(FrameMemory::copy_within(&mut lent, 0, 2, 4));
        assert_eq!(
            (0..6).map(|n| page.byte(n)).collect::<Vec<_>>(),
            [1, 2, 1, 2, 3, 4]
        );
        assert!(FrameMemory::copy_within(&mut lent, 2, 0, 4));
        assert_eq!(
            (0..6).map(|n| page.byte(n)).collect::<Vec<_>>(),
            [1, 2, 3, 4, 3, 4]
        );
        assert!(
            !FrameMemory::copy_within(&mut lent, PAGE - 2, 0, 4),
            "past the end"
        );
    }

    #[test]
    fn a_misaligned_or_small_region_is_refused() {
        let page = TestPage::fresh();
        let shifted = Region::<ReadWrite>::in_test_page(page, 8, 64).unwrap();
        assert_eq!(
            shifted.into_frame(64).unwrap_err(),
            RegionError::Misaligned {
                vaddr: page.vaddr() + 8,
                align: FRAME_ALIGN
            }
        );
        let small = Region::<ReadOnly>::in_test_page(page, 0, 1024).unwrap();
        assert_eq!(
            small.into_frame(1025).unwrap_err(),
            RegionError::TooSmall {
                size: 1024,
                needed: 1025
            }
        );
    }
}
