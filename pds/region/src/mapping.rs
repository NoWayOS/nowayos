// SPDX-License-Identifier: MIT OR Apache-2.0
//! The only `unsafe` code in `nowayos-region`, together with everything it
//! relies on. Every other module of the crate is safe code.
//!
//! Three things here need `unsafe`:
//!
//! 1. **The patched symbols** (`patched_table!`). The Microkit tool writes
//!    each mapping's virtual address and size into symbols named by the
//!    system description (`setvar_vaddr`, `setvar_size`). They are defined
//!    here with `export_name`, which fixes their names, and, on the seL4
//!    target, `link_section = ".data"`, which puts them where the tool can
//!    rewrite them: a zero-initialised variable would otherwise be placed in
//!    `.bss`, which has no bytes in the program image to rewrite. Both
//!    attributes are `unsafe` because a second definition of the same symbol
//!    name, or an unsuitable section, would be undefined behaviour. Every
//!    name starts with `nowayos_` and is defined nowhere else, and `.data` is
//!    an ordinary writable data section. The symbols are `AtomicUsize`, so
//!    the compiler cannot assume they still hold their initial zero, and each
//!    is one machine word wide, as the tool checks before patching.
//! 2. **[`SharedLayout`]**, an `unsafe` trait: implementing it promises that
//!    a type is `repr(C)` and made only of atomic integers, so that every bit
//!    pattern is a valid value and all access to it is atomic.
//! 3. **[`Region::view`]**, which turns a region's address into a `&'static`
//!    reference after checking everything that can be checked at run time;
//!    and **[`Region::view_bytes`]**, which turns it into a `&'static` slice
//!    of atomic bytes covering the whole region (a framebuffer, `frame`).
//!
//! A [`Region`] is only ever created from the patched symbols (or, in this
//! crate's tests, from a `&'static` test page). That is the invariant `view`
//! rests on, and the fields are private to this module so that nothing else
//! can forge one.

use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use crate::RegionError;

/// Regions of each kind ([`ReadWrite`], [`ReadOnly`]) a PD can bind:
/// numbers 0 to 15 - sixteen, so a UI PD can hold, beside its display's
/// and input's, the two rings of each of several application PDs.
pub const REGIONS_PER_KIND: usize = 16;

// ---- Access rights ---------------------------------------------------------

mod sealed {
    /// Seals [`Access`](super::Access) and says which symbols a kind uses.
    pub trait Sealed {
        /// `true` for read-only regions.
        const READ_ONLY: bool;
    }
}

/// The rights a region's mapping grants this PD: [`ReadWrite`] or
/// [`ReadOnly`]. Sealed: there are no other implementations.
pub trait Access: sealed::Sealed {}

/// A region mapped `perms="rw"`: this PD may read and write it.
#[derive(Debug)]
pub enum ReadWrite {}

/// A region mapped `perms="r"`: this PD may only read it.
///
/// Views of a read-only region only ever load, and only with relaxed atomic
/// loads of at most four bytes, which Rust documents as valid on memory
/// mapped read-only (`core::sync::atomic`, "Atomic accesses to read-only
/// memory"; the limit on AArch64 is eight bytes).
#[derive(Debug)]
pub enum ReadOnly {}

impl sealed::Sealed for ReadWrite {
    const READ_ONLY: bool = false;
}

impl sealed::Sealed for ReadOnly {
    const READ_ONLY: bool = true;
}

impl Access for ReadWrite {}
impl Access for ReadOnly {}

// ---- The symbols the Microkit tool patches ---------------------------------

/// One mapping's patched symbols, and whether this PD has bound it yet.
struct Patched {
    vaddr: &'static AtomicUsize,
    vaddr_symbol: &'static str,
    size: &'static AtomicUsize,
    size_symbol: &'static str,
    taken: AtomicBool,
}

impl Patched {
    const fn new(
        vaddr: &'static AtomicUsize,
        vaddr_symbol: &'static str,
        size: &'static AtomicUsize,
        size_symbol: &'static str,
    ) -> Self {
        Self {
            vaddr,
            vaddr_symbol,
            size,
            size_symbol,
            taken: AtomicBool::new(false),
        }
    }
}

/// Defines the address and size symbols for one kind of region, and the
/// table of them. See the module documentation for why the attributes are
/// needed and why they are sound.
macro_rules! patched_table {
    ($table:ident: $([$vaddr:ident = $vaddr_symbol:literal, $size:ident = $size_symbol:literal]),* $(,)?) => {
        $(
            #[unsafe(export_name = $vaddr_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $vaddr: AtomicUsize = AtomicUsize::new(0);

            #[unsafe(export_name = $size_symbol)]
            #[cfg_attr(target_os = "none", unsafe(link_section = ".data"))]
            static $size: AtomicUsize = AtomicUsize::new(0);
        )*

        static $table: [Patched; REGIONS_PER_KIND] = [
            $(Patched::new(&$vaddr, $vaddr_symbol, &$size, $size_symbol)),*
        ];
    };
}

patched_table!(RW_SYMBOLS:
    [RW0_VADDR = "nowayos_rw_region_0_vaddr", RW0_SIZE = "nowayos_rw_region_0_size"],
    [RW1_VADDR = "nowayos_rw_region_1_vaddr", RW1_SIZE = "nowayos_rw_region_1_size"],
    [RW2_VADDR = "nowayos_rw_region_2_vaddr", RW2_SIZE = "nowayos_rw_region_2_size"],
    [RW3_VADDR = "nowayos_rw_region_3_vaddr", RW3_SIZE = "nowayos_rw_region_3_size"],
    [RW4_VADDR = "nowayos_rw_region_4_vaddr", RW4_SIZE = "nowayos_rw_region_4_size"],
    [RW5_VADDR = "nowayos_rw_region_5_vaddr", RW5_SIZE = "nowayos_rw_region_5_size"],
    [RW6_VADDR = "nowayos_rw_region_6_vaddr", RW6_SIZE = "nowayos_rw_region_6_size"],
    [RW7_VADDR = "nowayos_rw_region_7_vaddr", RW7_SIZE = "nowayos_rw_region_7_size"],
    [RW8_VADDR = "nowayos_rw_region_8_vaddr", RW8_SIZE = "nowayos_rw_region_8_size"],
    [RW9_VADDR = "nowayos_rw_region_9_vaddr", RW9_SIZE = "nowayos_rw_region_9_size"],
    [RW10_VADDR = "nowayos_rw_region_10_vaddr", RW10_SIZE = "nowayos_rw_region_10_size"],
    [RW11_VADDR = "nowayos_rw_region_11_vaddr", RW11_SIZE = "nowayos_rw_region_11_size"],
    [RW12_VADDR = "nowayos_rw_region_12_vaddr", RW12_SIZE = "nowayos_rw_region_12_size"],
    [RW13_VADDR = "nowayos_rw_region_13_vaddr", RW13_SIZE = "nowayos_rw_region_13_size"],
    [RW14_VADDR = "nowayos_rw_region_14_vaddr", RW14_SIZE = "nowayos_rw_region_14_size"],
    [RW15_VADDR = "nowayos_rw_region_15_vaddr", RW15_SIZE = "nowayos_rw_region_15_size"],
);

patched_table!(RO_SYMBOLS:
    [RO0_VADDR = "nowayos_ro_region_0_vaddr", RO0_SIZE = "nowayos_ro_region_0_size"],
    [RO1_VADDR = "nowayos_ro_region_1_vaddr", RO1_SIZE = "nowayos_ro_region_1_size"],
    [RO2_VADDR = "nowayos_ro_region_2_vaddr", RO2_SIZE = "nowayos_ro_region_2_size"],
    [RO3_VADDR = "nowayos_ro_region_3_vaddr", RO3_SIZE = "nowayos_ro_region_3_size"],
    [RO4_VADDR = "nowayos_ro_region_4_vaddr", RO4_SIZE = "nowayos_ro_region_4_size"],
    [RO5_VADDR = "nowayos_ro_region_5_vaddr", RO5_SIZE = "nowayos_ro_region_5_size"],
    [RO6_VADDR = "nowayos_ro_region_6_vaddr", RO6_SIZE = "nowayos_ro_region_6_size"],
    [RO7_VADDR = "nowayos_ro_region_7_vaddr", RO7_SIZE = "nowayos_ro_region_7_size"],
    [RO8_VADDR = "nowayos_ro_region_8_vaddr", RO8_SIZE = "nowayos_ro_region_8_size"],
    [RO9_VADDR = "nowayos_ro_region_9_vaddr", RO9_SIZE = "nowayos_ro_region_9_size"],
    [RO10_VADDR = "nowayos_ro_region_10_vaddr", RO10_SIZE = "nowayos_ro_region_10_size"],
    [RO11_VADDR = "nowayos_ro_region_11_vaddr", RO11_SIZE = "nowayos_ro_region_11_size"],
    [RO12_VADDR = "nowayos_ro_region_12_vaddr", RO12_SIZE = "nowayos_ro_region_12_size"],
    [RO13_VADDR = "nowayos_ro_region_13_vaddr", RO13_SIZE = "nowayos_ro_region_13_size"],
    [RO14_VADDR = "nowayos_ro_region_14_vaddr", RO14_SIZE = "nowayos_ro_region_14_size"],
    [RO15_VADDR = "nowayos_ro_region_15_vaddr", RO15_SIZE = "nowayos_ro_region_15_size"],
);

fn symbols<A: Access>() -> &'static [Patched; REGIONS_PER_KIND] {
    if A::READ_ONLY {
        &RO_SYMBOLS
    } else {
        &RW_SYMBOLS
    }
}

// ---- Layouts ---------------------------------------------------------------

/// Memory shared with another PD, which this crate may view as
/// `&'static Self`.
///
/// # Safety
///
/// An implementor must be `repr(C)` and consist only of atomic integers and
/// arrays of them. Then every bit pattern is a valid value — the memory is
/// zeroed by Microkit and afterwards holds whatever a peer wrote — and all
/// access through `&Self` is atomic, which is how memory that another
/// protection domain may write at any moment must be accessed.
pub(crate) unsafe trait SharedLayout: Sync + 'static {}

/// The producer-owned half of a ring, in the ring's *messages* region. See
/// `crate::ring`.
#[repr(C)]
pub(crate) struct MessageLayout<const SLOTS: usize, const SLOT: usize> {
    /// Records ever published, free-running. Written by the producer only.
    pub(crate) producer_index: AtomicU32,
    /// The record slots. Written by the producer only.
    pub(crate) slots: [[AtomicU8; SLOT]; SLOTS],
    /// The producer's restart epoch, after the slots so that their
    /// offsets stay put. Written by the producer only.
    pub(crate) producer_epoch: AtomicU32,
}

// SAFETY: `repr(C)`, and made only of `AtomicU32`s and arrays of `AtomicU8`.
unsafe impl<const SLOTS: usize, const SLOT: usize> SharedLayout for MessageLayout<SLOTS, SLOT> {}

/// The consumer-owned half of a ring, in the ring's *control* region.
#[repr(C)]
pub(crate) struct ControlLayout {
    /// Records ever taken, free-running. Written by the consumer only.
    pub(crate) consumer_index: AtomicU32,
    /// The consumer's restart epoch. Written by the consumer only.
    pub(crate) consumer_epoch: AtomicU32,
}

// SAFETY: `repr(C)`, and made only of `AtomicU32`s.
unsafe impl SharedLayout for ControlLayout {}

/// One of sDDF's network queues (`nowayos_net::queue`'s layout): the two
/// indices and the flag, then `CAPACITY` descriptors of
/// [`DESCRIPTOR_LEN`](nowayos_net::DESCRIPTOR_LEN) bytes. See `crate::net`.
#[repr(C)]
pub(crate) struct NetQueueLayout<const CAPACITY: usize> {
    /// The producer's index. Written by the producer only, when both sides
    /// keep to the protocol.
    pub(crate) tail: AtomicU16,
    /// The consumer's index. Written by the consumer only, likewise.
    pub(crate) head: AtomicU16,
    /// sDDF's `consumer_signalled` flag. Written by both sides.
    pub(crate) signal: AtomicU32,
    /// The descriptors, as bytes. Written by the producer only, likewise.
    pub(crate) descriptors: [[AtomicU8; nowayos_net::DESCRIPTOR_LEN]; CAPACITY],
}

// SAFETY: `repr(C)`, and made only of atomic integers and arrays of
// `AtomicU8`.
unsafe impl<const CAPACITY: usize> SharedLayout for NetQueueLayout<CAPACITY> {}

/// Number of 32-bit words in a 4 KiB page.
pub const PAGE_WORDS: usize = 1024;

/// One 4 KiB page as 32-bit words. See `crate::words`.
#[repr(C)]
pub(crate) struct PageLayout {
    pub(crate) words: [AtomicU32; PAGE_WORDS],
}

// SAFETY: `repr(C)`, and made only of an array of `AtomicU32`.
unsafe impl SharedLayout for PageLayout {}

// ---- Regions ---------------------------------------------------------------

/// A memory region that the system description maps into this PD with
/// rights `A`, bound but not yet viewed.
///
/// Get one from [`read_write`](crate::read_write) or
/// [`read_only`](crate::read_only), then turn it into exactly one view:
/// [`into_words`](Region::into_words),
/// [`ring_producer`](crate::ring_producer) or
/// [`ring_consumer`](crate::ring_consumer).
#[derive(Debug)]
pub struct Region<A: Access> {
    vaddr: usize,
    size: usize,
    access: PhantomData<A>,
}

impl<A: Access> Region<A> {
    /// Binds region `index` of kind `A` from the patched symbols. Each region
    /// can be bound once.
    pub(crate) fn take(index: usize) -> Result<Self, RegionError> {
        let patched = symbols::<A>()
            .get(index)
            .ok_or(RegionError::NoSuchRegion { index })?;
        let vaddr = patched.vaddr.load(Ordering::Relaxed);
        if vaddr == 0 {
            return Err(RegionError::NotMapped {
                symbol: patched.vaddr_symbol,
            });
        }
        let size = patched.size.load(Ordering::Relaxed);
        if size == 0 {
            return Err(RegionError::SizeUnknown {
                symbol: patched.size_symbol,
            });
        }
        if patched.taken.swap(true, Ordering::Relaxed) {
            return Err(RegionError::AlreadyTaken {
                symbol: patched.vaddr_symbol,
            });
        }
        Ok(Self {
            vaddr,
            size,
            access: PhantomData,
        })
    }

    /// The virtual address the region is mapped at in this PD.
    #[must_use]
    pub const fn vaddr(&self) -> usize {
        self.vaddr
    }

    /// The region's size in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Views the region as a `T`, consuming it, so a region is only ever
    /// viewed through one layout.
    pub(crate) fn view<T: SharedLayout>(self) -> Result<&'static T, RegionError> {
        check_fits::<T>(self.vaddr, self.size)?;
        // The region is memory the Microkit tool mapped for us, outside any
        // Rust allocation, so its pointer carries exposed provenance.
        let ptr = core::ptr::with_exposed_provenance::<T>(self.vaddr);
        // SAFETY: `ptr` is non-null: `take` rejects address 0 (and a test
        // page is a real allocation). It is aligned for `T`: checked above.
        // It is dereferenceable for `size_of::<T>()` bytes for the rest of the
        // program: the Microkit tool patched `vaddr` and `size` from the same
        // `<map>` element, so `size` bytes are mapped at `vaddr`; Microkit
        // never unmaps a memory region while the PD exists; `size` is at
        // least `size_of::<T>()` and the range does not wrap (both checked
        // above). The memory is a valid `T`, now and whatever a peer writes
        // later, and may be changed behind our reference, because `T:
        // SharedLayout` is made only of atomics. This PD accesses the memory
        // only through this one layout, so the same bytes are never accessed
        // as atomics of two different sizes: a region is bound at most once
        // (`take`) and viewed at most once (`view` consumes it), and the
        // system description maps each memory region at most once into the
        // PD (a precondition checked when the image is built, by
        // tools/check-system). (Tests view one test page twice through the
        // same layout, to stand in for the peer PD.)
        //
        // Rights: read-only regions are viewed only by types that load but
        // never store (`crate::ring`, `crate::words`). A read-write view
        // stores, which is sound only if the `<map>` grants write access, as
        // its `nowayos_rw_*` symbol states; were it read-only, the first
        // store would fault the PD (stopped by seL4, never undefined
        // behaviour). tools/check-system checks the symbol against the
        // map's perms and the page rights in the Microkit tool's output.
        Ok(unsafe { &*ptr })
    }
}

impl<A: Access> Region<A> {
    /// Views the whole region as atomic bytes, consuming it, so a region is
    /// only ever viewed through one layout.
    pub(crate) fn view_bytes(self) -> Result<&'static [AtomicU8], RegionError> {
        if self.vaddr.checked_add(self.size).is_none() || isize::try_from(self.size).is_err() {
            return Err(RegionError::OutOfRange { vaddr: self.vaddr });
        }
        // The region is memory the Microkit tool mapped for us, outside any
        // Rust allocation, so its pointer carries exposed provenance.
        let ptr = core::ptr::with_exposed_provenance::<AtomicU8>(self.vaddr);
        // SAFETY: `from_raw_parts` needs `size` valid, initialised
        // `AtomicU8`s at a non-null, aligned `ptr`, within one allocation of
        // at most `isize::MAX` bytes, not mutated except through atomics
        // while the slice lives.
        // * Non-null: `take` rejects address 0 (and a test page is a real
        //   allocation). Aligned: `AtomicU8` needs no alignment.
        // * Valid for `size` bytes for the rest of the program, and within
        //   one allocation: the Microkit tool patched `vaddr` and `size` from
        //   one `<map>` element, and never unmaps a memory region while the
        //   PD exists; the range does not wrap and `size` fits `isize`
        //   (both checked above).
        // * Initialised, and any bit pattern a valid `AtomicU8`: Microkit
        //   zeroes a region at boot; afterwards it holds whatever this PD or
        //   a peer wrote.
        // * Mutated only atomically as far as Rust can tell: this PD reaches
        //   the bytes only through this slice - a region is bound once
        //   (`take`) and viewed once (this consumes it), and the description
        //   maps each region at most once into a PD (tools/check-system) -
        //   and a peer's writes are not Rust accesses, so they are like
        //   another thread's writes to atomics. (Tests view one test page
        //   twice through the same layout, to stand in for the peer.)
        //
        // Rights: a read-only region's slice is only ever loaded from
        // (`crate::frame` gives `Frame<ReadOnly>` loads only), and relaxed
        // single-byte atomic loads are valid on read-only memory
        // (`core::sync::atomic`, "Atomic accesses to read-only memory").
        Ok(unsafe { core::slice::from_raw_parts(ptr, self.size) })
    }
}

/// Checks that a `T` fits at `vaddr` in a region of `size` bytes: aligned,
/// large enough, and not running past the end of the address space.
pub(crate) fn check_fits<T>(vaddr: usize, size: usize) -> Result<(), RegionError> {
    let align = align_of::<T>();
    if !vaddr.is_multiple_of(align) {
        return Err(RegionError::Misaligned { vaddr, align });
    }
    let needed = size_of::<T>();
    if size < needed {
        return Err(RegionError::TooSmall { size, needed });
    }
    if vaddr.checked_add(needed).is_none() {
        return Err(RegionError::OutOfRange { vaddr });
    }
    Ok(())
}

// ---- Test support ----------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    //! Stand-ins for what the Microkit tool does: pages of memory, and
    //! patching the symbols.

    use super::{Access, AtomicU8, AtomicUsize, Ordering, PhantomData, Region, symbols};

    /// Size of a test page, and of the smallest Microkit memory region.
    pub(crate) const PAGE: usize = 4096;

    /// Test pages available to one test run. Each test takes fresh ones;
    /// raise this if the tests ever need more.
    const POOL: usize = 40;

    /// A page of test memory, aligned and zeroed like a Microkit region.
    #[repr(C, align(4096))]
    pub(crate) struct TestPage {
        bytes: [AtomicU8; PAGE],
    }

    /// The pages are statics, so they live for the whole run — as a Microkit
    /// region lives as long as its PD — without leaking heap memory, which
    /// Miri's leak check would report.
    static PAGES: [TestPage; POOL] = [const {
        TestPage {
            bytes: [const { AtomicU8::new(0) }; PAGE],
        }
    }; POOL];

    static NEXT_PAGE: AtomicUsize = AtomicUsize::new(0);

    impl TestPage {
        /// A zeroed page no other test has used.
        ///
        /// # Panics
        ///
        /// If the pool is used up: raise `POOL`.
        #[allow(
            clippy::panic,
            reason = "test helper: running out of test pages is a set-up error, reported by panicking"
        )]
        pub(crate) fn fresh() -> &'static Self {
            let index = NEXT_PAGE.fetch_add(1, Ordering::Relaxed);
            match PAGES.get(index) {
                Some(page) => page,
                None => panic!("test page pool used up: raise POOL above {POOL}"),
            }
        }

        /// The page's address, as the Microkit tool would patch it.
        pub(crate) fn vaddr(&'static self) -> usize {
            core::ptr::from_ref(self).expose_provenance()
        }

        /// Byte `index` of the page (a relaxed load; 0 if out of range).
        pub(crate) fn byte(&self, index: usize) -> u8 {
            self.bytes
                .get(index)
                .map_or(0, |byte| byte.load(Ordering::Relaxed))
        }

        /// Every byte of the page, each a relaxed load: one borrow of the
        /// page for all of them, where `byte` makes one per byte - which
        /// Miri checks over the whole page each time.
        pub(crate) fn snapshot(&self) -> Vec<u8> {
            self.bytes
                .iter()
                .map(|byte| byte.load(Ordering::Relaxed))
                .collect()
        }
    }

    impl<A: Access> Region<A> {
        /// A region covering `len` bytes of `page` from `offset`, as if the
        /// Microkit tool had mapped it there. `None` if that is not inside
        /// the page, so a test can never view memory it does not own.
        pub(crate) fn in_test_page(
            page: &'static TestPage,
            offset: usize,
            len: usize,
        ) -> Option<Self> {
            let end = offset.checked_add(len)?;
            if end > PAGE {
                return None;
            }
            Some(Self {
                vaddr: page.vaddr().checked_add(offset)?,
                size: len,
                access: PhantomData,
            })
        }

        /// A region covering the whole of `page`.
        pub(crate) fn test_page(page: &'static TestPage) -> Self {
            Self {
                vaddr: page.vaddr(),
                size: PAGE,
                access: PhantomData,
            }
        }
    }

    /// Patches region `index` of kind `A` as the Microkit tool would —
    /// address of `page` (or 0 for "not mapped") and `size` — and marks it
    /// unbound. `size` is clamped to the page, so a test can never view
    /// memory it does not own.
    pub(crate) fn patch<A: Access>(index: usize, page: Option<&'static TestPage>, size: usize) {
        if let Some(patched) = symbols::<A>().get(index) {
            patched
                .vaddr
                .store(page.map_or(0, TestPage::vaddr), Ordering::Relaxed);
            patched.size.store(size.min(PAGE), Ordering::Relaxed);
            patched.taken.store(false, Ordering::Relaxed);
        }
    }
}
