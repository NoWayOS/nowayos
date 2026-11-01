// SPDX-License-Identifier: MIT OR Apache-2.0
//! A one-page region viewed as 32-bit words.
//!
//! For simple shared state that is not a message queue: a status word, a
//! configuration value, a test pattern. Every access is a single atomic
//! 32-bit load or store, so a peer writing the same page concurrently can
//! only produce values the caller then has to validate, never undefined
//! behaviour.

use core::marker::PhantomData;
use core::sync::atomic::Ordering;

use crate::RegionError;
use crate::mapping::{Access, PageLayout, ReadWrite, Region};
use crate::ring::load_acquire_shared;

pub use crate::mapping::PAGE_WORDS;

/// A 4 KiB region as [`PAGE_WORDS`] 32-bit words, with the region's rights
/// `A`: [`Words<ReadOnly>`](crate::ReadOnly) can only load,
/// [`Words<ReadWrite>`] can also store.
pub struct Words<A: Access> {
    page: &'static PageLayout,
    access: PhantomData<A>,
}

impl<A: Access> Region<A> {
    /// Views the region's first 4 KiB as 32-bit words.
    ///
    /// # Errors
    ///
    /// [`RegionError::TooSmall`] if the region is smaller than 4 KiB;
    /// [`RegionError::Misaligned`] if it is not 4-byte aligned (a Microkit
    /// region is always page-aligned).
    pub fn into_words(self) -> Result<Words<A>, RegionError> {
        Ok(Words {
            page: self.view()?,
            access: PhantomData,
        })
    }
}

impl<A: Access> Words<A> {
    /// Loads word `index` with acquire semantics, or `None` if `index` is
    /// not below [`PAGE_WORDS`]. Valid on read-only mappings.
    #[must_use]
    pub fn load(&self, index: usize) -> Option<u32> {
        self.page.words.get(index).map(load_acquire_shared)
    }
}

impl Words<ReadWrite> {
    /// Stores `value` in word `index` with release ordering. Returns `false`
    /// (and stores nothing) if `index` is not below [`PAGE_WORDS`].
    pub fn store(&self, index: usize, value: u32) -> bool {
        match self.page.words.get(index) {
            Some(word) => {
                word.store(value, Ordering::Release);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::ReadOnly;
    use crate::mapping::testing::{PAGE, TestPage};

    #[test]
    fn a_read_write_page_stores_and_loads_words() {
        let page = TestPage::fresh();
        let words = Region::<ReadWrite>::test_page(page).into_words().unwrap();
        assert_eq!(words.load(0), Some(0));
        assert!(words.store(0, 0x5AFE_C0DE));
        assert!(words.store(PAGE_WORDS - 1, 7));
        assert_eq!(words.load(0), Some(0x5AFE_C0DE));
        assert_eq!(words.load(PAGE_WORDS - 1), Some(7));
        // Native (little-endian) byte order in memory.
        assert_eq!([0, 1, 2, 3].map(|i| page.byte(i)), [0xDE, 0xC0, 0xFE, 0x5A]);
        assert_eq!(page.byte(PAGE - 4), 7);
    }

    #[test]
    fn a_read_only_page_sees_a_peers_writes() {
        let page = TestPage::fresh();
        let writer = Region::<ReadWrite>::test_page(page).into_words().unwrap();
        // A second view of the same page stands in for the peer PD, which
        // maps the page read-write while this PD maps it read-only.
        let reader = Region::<ReadOnly>::test_page(page).into_words().unwrap();
        assert!(writer.store(3, 42));
        assert_eq!(reader.load(3), Some(42));
    }

    #[test]
    fn out_of_range_words_are_refused() {
        let words = Region::<ReadWrite>::test_page(TestPage::fresh())
            .into_words()
            .unwrap();
        assert_eq!(words.load(PAGE_WORDS), None);
        assert!(!words.store(PAGE_WORDS, 1));
        assert!(!words.store(usize::MAX, 1));
    }

    #[test]
    fn a_region_smaller_than_a_page_is_refused() {
        let region = Region::<ReadWrite>::in_test_page(TestPage::fresh(), 0, 1024).unwrap();
        assert_eq!(
            region.into_words().err(),
            Some(RegionError::TooSmall {
                size: 1024,
                needed: 4096
            })
        );
    }
}
