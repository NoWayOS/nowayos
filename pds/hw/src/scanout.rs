// SPDX-License-Identifier: MIT OR Apache-2.0
//! A framebuffer the firmware set up and the display hardware scans out:
//! the Raspberry Pi 5's, at the address and in the mode its board
//! file gives (`firmware_fb` in `platforms/rpi5b_2gb.toml`). With the other
//! files `lib.rs` names, the only `unsafe` code in `nowayos-hw`.
//!
//! The window is bound like a device's register window - from
//! `nowayos_device_N_*`, on a map of the board's exact window, into one PD -
//! but it is memory, not registers: what is written there is shown, and
//! reading it back has no side effect. So it is viewed as aligned 64-bit
//! words, each access one atomic load or store: a whole row of pixels takes
//! an eighth of the stores byte accesses would on uncached memory.
//!
//! **Cached or not**: a display PD may map the window cached,
//! under the grant `cached` (`tools/check-system`'s one exception to rule 7),
//! and then writes at memory speed. So every [`write`](Scanout::write) is
//! cleaned to memory (DC CVAC, [`crate::cpu::clean_data_cache`]) before it
//! returns: the display hardware, which reads memory, never shows a stale
//! line. On a window mapped uncached the clean finds nothing to write back.
//!
//! **`unsafe`:** [`Scanout::new`] turns the patched address into a
//! `&'static [AtomicU64]`, on the same grounds as `dma.rs` turns a DMA
//! region into atomic bytes; its SAFETY comment gives them.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{Device, HwError};

/// Bytes of one access: a word.
const WORD: usize = 8;

/// The window of a framebuffer the display hardware scans out, written and
/// read a word at a time. Get one from [`Scanout::new`].
#[derive(Debug)]
pub struct Scanout {
    words: &'static [AtomicU64],
}

impl Scanout {
    /// Views the first `len` bytes of `device`'s window - its bytes of
    /// pixels, from the board's facts - consuming the device, so the window
    /// has one view. A last part word of `len` is left out.
    ///
    /// # Errors
    ///
    /// [`HwError::TooSmall`] if the window is smaller than `len`;
    /// [`HwError::Misaligned`] if it does not start on a word;
    /// [`HwError::OutOfRange`] if it would run past the end of the address
    /// space.
    pub fn new(device: Device, len: usize) -> Result<Self, HwError> {
        let (vaddr, size) = (device.vaddr(), device.size());
        if size < len {
            return Err(HwError::TooSmall { size, needed: len });
        }
        if !vaddr.is_multiple_of(WORD) {
            return Err(HwError::Misaligned { vaddr, align: WORD });
        }
        if vaddr.checked_add(len).is_none() {
            return Err(HwError::OutOfRange { vaddr });
        }
        let count = len.checked_div(WORD).unwrap_or(0);
        // The window is memory the Microkit tool mapped for us, outside any
        // Rust allocation, so the pointer carries exposed provenance.
        let ptr = core::ptr::with_exposed_provenance::<AtomicU64>(vaddr);
        // SAFETY: `from_raw_parts` needs `count` valid, initialised
        // `AtomicU64`s at a non-null, aligned `ptr`, not mutated except
        // through atomics while the slice lives, within one allocation of at
        // most `isize::MAX` bytes.
        // * Non-null and aligned: `vaddr` is a patched, non-zero address
        //   (`device` refuses 0) and a multiple of 8 (checked above).
        // * Valid for `count` words for the rest of the program: they lie in
        //   the `size` bytes the Microkit tool patched from one `<map>` of
        //   the board's window (checked above, without wrapping), which it
        //   never unmaps while the PD exists; Microkit windows are far below
        //   `isize::MAX`.
        // * Initialised, and any bit pattern a valid `AtomicU64`: the
        //   window is RAM - frames Microkit makes, zeroed, at boot - which
        //   holds pixels, so a read has no side effect.
        // * Mutated only atomically as far as Rust can tell: this PD
        //   reaches the window only through this slice (a device is bound
        //   once and `new` consumes it; tools/check-system refuses a
        //   description that maps a region twice into one PD, or a device
        //   into two PDs), and the display hardware only reads it.
        let words = unsafe { core::slice::from_raw_parts(ptr, count) };
        Ok(Self { words })
    }

    /// The bytes of the window this view reaches.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.words.len().saturating_mul(WORD)
    }

    /// Whether the view reaches no byte.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Writes `bytes` at `offset`, a word at a time, then cleans them to
    /// memory, so the display hardware sees them whether the window is
    /// mapped cached or not.
    ///
    /// # Errors
    ///
    /// [`HwError::Misaligned`] unless `offset` and the length are whole
    /// words; [`HwError::OutsideRegion`] if they do not fit. Nothing is
    /// written then.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), HwError> {
        for (cell, word) in self
            .span(offset, bytes.len())?
            .iter()
            .zip(bytes.chunks_exact(WORD))
        {
            let mut value = [0u8; WORD];
            value.copy_from_slice(word);
            cell.store(u64::from_le_bytes(value), Ordering::Relaxed);
        }
        let start = self.words.as_ptr().addr().saturating_add(offset);
        crate::cpu::clean_data_cache(start, bytes.len());
        Ok(())
    }

    /// Reads the window's bytes at `offset` into `out`, a word at a time.
    ///
    /// # Errors
    ///
    /// As for [`write`](Self::write); nothing is read then.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), HwError> {
        let span = self.span(offset, out.len())?;
        for (word, cell) in out.chunks_exact_mut(WORD).zip(span) {
            word.copy_from_slice(&cell.load(Ordering::Relaxed).to_le_bytes());
        }
        Ok(())
    }

    /// The words of the `len` bytes at `offset`, if both are whole words
    /// and they lie inside the window.
    fn span(&self, offset: usize, len: usize) -> Result<&[AtomicU64], HwError> {
        if !offset.is_multiple_of(WORD) || !len.is_multiple_of(WORD) {
            return Err(HwError::Misaligned {
                vaddr: offset,
                align: WORD,
            });
        }
        let first = offset.checked_div(WORD).unwrap_or(0);
        first
            .checked_add(len.checked_div(WORD).unwrap_or(0))
            .and_then(|end| self.words.get(first..end))
            .ok_or(HwError::OutsideRegion {
                offset,
                len,
                size: self.len(),
            })
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
    //! These run under Miri too: a scan-out view is plain memory accesses.

    use super::*;

    /// Test memory standing in for a window: atomics, so writing through
    /// the view is sound.
    #[repr(C, align(4096))]
    struct Window([AtomicU64; 64]);

    /// A zeroed window per test: the tests run in parallel.
    static WINDOWS: [Window; 3] = [const { Window([const { AtomicU64::new(0) }; 64]) }; 3];

    fn window(test: usize) -> &'static Window {
        &WINDOWS[test]
    }

    fn vaddr(window: &'static Window) -> usize {
        core::ptr::from_ref(window).expose_provenance()
    }

    #[test]
    fn rows_are_written_and_read_back_a_word_at_a_time() {
        let memory = window(0);
        let mut scanout = Scanout::new(Device::for_test(vaddr(memory), 512), 500).unwrap();
        assert_eq!(scanout.len(), 496, "a last part word is left out");
        let row: std::vec::Vec<u8> = (0..32u8).collect();
        scanout.write(16, &row).unwrap();
        assert_eq!(
            memory.0[2].load(Ordering::Relaxed),
            u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7])
        );
        let mut back = [0u8; 32];
        scanout.read(16, &mut back).unwrap();
        assert_eq!(&back[..], &row[..]);
        assert_eq!(
            memory.0[6].load(Ordering::Relaxed),
            0,
            "nothing past the row"
        );
    }

    #[test]
    fn a_part_word_or_a_span_outside_is_refused_and_nothing_written() {
        let memory = window(1);
        let mut scanout = Scanout::new(Device::for_test(vaddr(memory), 512), 64).unwrap();
        assert_eq!(
            scanout.write(4, &[1; 8]),
            Err(HwError::Misaligned { vaddr: 4, align: 8 })
        );
        assert_eq!(
            scanout.write(0, &[1; 12]),
            Err(HwError::Misaligned { vaddr: 0, align: 8 })
        );
        assert_eq!(
            scanout.write(56, &[1; 16]),
            Err(HwError::OutsideRegion {
                offset: 56,
                len: 16,
                size: 64
            })
        );
        assert_eq!(
            scanout.read(usize::MAX - 7, &mut [0; 8]),
            Err(HwError::OutsideRegion {
                offset: usize::MAX - 7,
                len: 8,
                size: 64
            })
        );
        assert!(memory.0.iter().all(|w| w.load(Ordering::Relaxed) == 0));
    }

    #[test]
    fn a_window_too_small_or_misaligned_is_refused() {
        let memory = window(2);
        assert_eq!(
            Scanout::new(Device::for_test(vaddr(memory), 64), 65).unwrap_err(),
            HwError::TooSmall {
                size: 64,
                needed: 65
            }
        );
        assert_eq!(
            Scanout::new(Device::for_test(vaddr(memory) + 4, 64), 8).unwrap_err(),
            HwError::Misaligned {
                vaddr: vaddr(memory) + 4,
                align: 8
            }
        );
        assert_eq!(
            Scanout::new(Device::for_test(usize::MAX - 7, 64), 16).unwrap_err(),
            HwError::OutOfRange {
                vaddr: usize::MAX - 7
            }
        );
    }
}
