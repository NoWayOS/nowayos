// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed, rights-aware views of the memory regions a Microkit system
//! description maps into a protection domain, and the `nowayos-ipc` ring
//! over them.
//!
//! Protection domains (PDs) exchange data through shared memory regions
//! declared in the system description.
//! The Microkit tool maps each region at a fixed virtual
//! address and writes that address (and the region's size) into symbols in
//! the PD's program image. This crate turns those numbers into safe, typed
//! views, so the PDs themselves need no `unsafe`:
//!
//! * [`read_write`] / [`read_only`] bind a region by number;
//! * [`ring_producer`] / [`ring_consumer`] turn two regions into the two ends
//!   of a [`nowayos_ipc`] ring ([`RingTx`], [`RingRx`]), implementing
//!   [`nowayos_ipc::ProducerMemory`] and [`nowayos_ipc::ConsumerMemory`]
//!   respectively — only that side, so the wrong end cannot be attached;
//! * [`Region::into_words`] views one page as 32-bit words ([`Words`]);
//! * [`Region::into_frame`] views a whole framebuffer-sized region as bytes
//!   ([`Frame`]): drawn into on a read-write map, only loaded from on a
//!   read-only one - a framebuffer, or a network data region's packet
//!   buffers;
//! * [`net_queue`] views a read-write region as one of sDDF's network
//!   queues ([`NetQueue`]), for `nowayos_net`'s queue ends;
//! * [`screen`], [`build_time`] and [`device_facts`] read the
//!   configuration a system description prefills: the screen's size, the
//!   image's build time, and the board's facts about the device a driver
//!   holds.
//!
//! # Binding regions from the system description
//!
//! The crate owns a fixed set of symbols for the Microkit tool to patch. A
//! `<map>` element names one pair, and **the name says which rights the map
//! must grant**:
//!
//! | Symbols (N = 0 to 15) | The `<map>` must have | Bind with |
//! | --- | --- | --- |
//! | `nowayos_rw_region_N_vaddr`, `nowayos_rw_region_N_size` | `perms="rw"` | [`read_write`]`(N)` |
//! | `nowayos_ro_region_N_vaddr`, `nowayos_ro_region_N_size` | `perms="r"` | [`read_only`]`(N)` |
//!
//! ```xml
//! <map mr="input_ring_messages" vaddr="0x4000_0000" perms="r"
//!      setvar_vaddr="nowayos_ro_region_0_vaddr" setvar_size="nowayos_ro_region_0_size" />
//! ```
//!
//! The address, size and alignment are checked when a region is viewed; the
//! rights cannot be checked from inside the PD. So **`tools/check-system`
//! checks, when the system image is built, that every `nowayos_ro_*` symbol
//! sits on a `perms="r"` map and every `nowayos_rw_*` symbol on a
//! `perms="rw"` map**, that the rights the Microkit tool actually gave each
//! page match, and that no memory region is mapped twice into one PD. A read-only view only
//! ever loads, so a `nowayos_ro_*` symbol on a writable map would merely be
//! wider than needed; a `nowayos_rw_*` symbol on a read-only map would make
//! the first write fault the PD.
//!
//! Each region can be bound once per PD, and each binding is viewed once, so
//! the same memory is never viewed through two different layouts — provided
//! the system description maps each memory region at most once into the PD,
//! which this crate cannot see and `tools/check-system` enforces.
//!
//! # The ring over two regions
//!
//! A ring uses **two** regions so that each side can write only what it
//! owns (the "narrowest rights" rule):
//!
//! | Region | Holds | Producer maps it | Consumer maps it |
//! | --- | --- | --- | --- |
//! | messages | producer index, slots | `rw` | `r` |
//! | control | consumer index | `r` | `rw` |
//!
//! A side can therefore trust its own index — the other side cannot write
//! it — and re-reading it after a restart is safe. The other side's index is
//! still validated on every use by [`nowayos_ipc`].
//!
//! # `unsafe`
//!
//! This crate is on the approved-`unsafe` list. All of its `unsafe` code is
//! in one private module, `mapping` (`src/mapping.rs`), which documents every
//! invariant it relies on; every other module is safe code.
//!
//! Provisional: this API may change. The network path uses the seL4 Device
//! Driver Framework's network queue layout byte for byte ([`net_queue`]); other queues keep the ring above.

#![cfg_attr(not(test), no_std)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod error;
mod facts;
mod frame;
#[allow(
    unsafe_code,
    reason = "the approved-unsafe module (tools/unsafe-allowlist.toml)"
)]
mod mapping;
mod net;
mod ring;
mod words;

pub use error::RegionError;
pub use facts::{
    COMPATIBLE_MAX, DEVICE_FACTS_REGION, DeviceFacts, FACTS_VERSION, FACTS_VERSION_BUS,
    FACTS_VERSION_FRAMEBUFFER, FramebufferFormat, FramebufferMode, device_facts,
};
pub use frame::{FRAME_ALIGN, Frame};
pub use mapping::{Access, REGIONS_PER_KIND, ReadOnly, ReadWrite, Region};
pub use net::{NetQueue, net_queue};
pub use ring::{RingRx, RingTx, ring_consumer, ring_producer};
pub use words::{PAGE_WORDS, Words};

use nowayos_time::Utc;

/// Binds read-write region `index`: the `<map>` whose `setvar_vaddr` and
/// `setvar_size` are `nowayos_rw_region_{index}_vaddr` and
/// `nowayos_rw_region_{index}_size`, with `perms="rw"`.
///
/// # Errors
///
/// [`RegionError::NoSuchRegion`] if `index` is not below
/// [`REGIONS_PER_KIND`]; [`RegionError::NotMapped`] if the system
/// description does not map it; [`RegionError::SizeUnknown`] if the map has
/// no `setvar_size`; [`RegionError::AlreadyTaken`] if it was bound before.
pub fn read_write(index: usize) -> Result<Region<ReadWrite>, RegionError> {
    Region::take(index)
}

/// Binds read-only region `index`: the `<map>` whose `setvar_vaddr` and
/// `setvar_size` are `nowayos_ro_region_{index}_vaddr` and
/// `nowayos_ro_region_{index}_size`, with `perms="r"`.
///
/// # Errors
///
/// As for [`read_write`].
pub fn read_only(index: usize) -> Result<Region<ReadOnly>, RegionError> {
    Region::take(index)
}

/// The read-only region that holds the screen's size, by NoWayOS's
/// convention: the system description prefills it from a file named
/// `screen-WxH.bin` (`tools/board-system` writes it), and maps it read-only
/// into every PD that needs the screen - the display, input drivers, the
/// input broker.
pub const SCREEN_REGION: usize = 7;

/// The screen's width and height in pixels, as the system description gives
/// them in read-only region [`SCREEN_REGION`]: words 0 and 1. Not
/// validated: the caller checks them against what it can handle.
///
/// # Errors
///
/// As for [`read_only`], or [`RegionError::TooSmall`] if the region is
/// smaller than a page.
pub fn screen() -> Result<(u32, u32), RegionError> {
    let words = read_only(SCREEN_REGION)?.into_words()?;
    match (words.load(0), words.load(1)) {
        (Some(width), Some(height)) => Ok((width, height)),
        _ => Err(RegionError::TooSmall { size: 0, needed: 8 }),
    }
}

/// The read-only region that holds the image's build time, by NoWayOS's
/// convention: the system description prefills it from `build-time.bin`
/// (`tools/board-system` writes it when the image is built), and maps
/// it read-only into the clock PD and any other PD that needs the floor
/// for wall-clock time.
pub const BUILD_TIME_REGION: usize = 6;

/// The image's build time - the floor for wall-clock time - as the system
/// description gives it in read-only region [`BUILD_TIME_REGION`]: words 0
/// and 1, a 64-bit little-endian count of seconds since
/// 1970-01-01T00:00:00Z.
///
/// # Errors
///
/// As for [`read_only`] - [`RegionError::NotMapped`] when the system gives
/// no such region; [`RegionError::TooSmall`] if the region is smaller than
/// a page; [`RegionError::NoBuildTime`] if the count is 0, as in a region
/// nothing was written to; [`RegionError::NotATime`] if it is past what a
/// [`Utc`] holds (2554). `make image` writes neither.
pub fn build_time() -> Result<Utc, RegionError> {
    build_time_in(&read_only(BUILD_TIME_REGION)?.into_words()?)
}

/// The build time held in the first two words of `words`.
fn build_time_in<A: Access>(words: &Words<A>) -> Result<Utc, RegionError> {
    let (Some(low), Some(high)) = (words.load(0), words.load(1)) else {
        return Err(RegionError::TooSmall { size: 0, needed: 8 });
    };
    let [a, b, c, d] = low.to_ne_bytes();
    let [e, f, g, h] = high.to_ne_bytes();
    let seconds = u64::from_le_bytes([a, b, c, d, e, f, g, h]);
    if seconds == 0 {
        return Err(RegionError::NoBuildTime);
    }
    Utc::from_unix_seconds(seconds).ok_or(RegionError::NotATime { seconds })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use core::mem::size_of;
    use std::string::ToString;

    use super::*;
    use crate::mapping::testing::{PAGE, TestPage, patch};
    use crate::mapping::{ControlLayout, MessageLayout, PageLayout, check_fits};

    #[test]
    fn binding_follows_the_patched_symbols() {
        // The only test that touches these regions' symbols, so tests
        // running in parallel cannot interfere with it (read-only region 6,
        // the build time's, is the_build_time_region_is_bound_by_its_symbols'
        // alone).
        let page = TestPage::fresh();

        // Nothing patched: nothing is mapped.
        assert_eq!(
            read_write(0).err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_rw_region_0_vaddr"
            })
        );
        assert_eq!(
            read_only(3).err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_ro_region_3_vaddr"
            })
        );
        // The last of each kind is a region like any other; one more is
        // none (sixteen of each).
        assert_eq!(
            read_write(15).err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_rw_region_15_vaddr"
            })
        );
        assert_eq!(
            read_only(15).err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_ro_region_15_vaddr"
            })
        );
        assert_eq!(
            read_write(REGIONS_PER_KIND).err(),
            Some(RegionError::NoSuchRegion { index: 16 })
        );
        assert_eq!(
            read_only(REGIONS_PER_KIND).err(),
            Some(RegionError::NoSuchRegion { index: 16 })
        );

        // Address patched but not size: the map lacks setvar_size.
        patch::<ReadWrite>(1, Some(page), 0);
        assert_eq!(
            read_write(1).err(),
            Some(RegionError::SizeUnknown {
                symbol: "nowayos_rw_region_1_size"
            })
        );

        // Both patched: it binds, once.
        patch::<ReadWrite>(1, Some(page), PAGE);
        let rw = read_write(1).unwrap();
        assert_eq!(rw.vaddr(), page.vaddr());
        assert_eq!(rw.size(), PAGE);
        assert_eq!(
            read_write(1).err(),
            Some(RegionError::AlreadyTaken {
                symbol: "nowayos_rw_region_1_vaddr"
            })
        );

        // Read-only regions have their own symbols: the same page, bound
        // read-only, as a peer PD would map it.
        assert!(read_only(1).is_err());
        patch::<ReadOnly>(1, Some(page), PAGE);
        let ro = read_only(1).unwrap();

        // A bound region works as a view.
        let writer = rw.into_words().unwrap();
        let reader = ro.into_words().unwrap();
        assert!(writer.store(5, 0x1234_5678));
        assert_eq!(reader.load(5), Some(0x1234_5678));
    }

    /// `seconds` in the first two words of `page`, as `make image` writes
    /// build-time.bin: a 64-bit little-endian count.
    fn write_build_time(page: &'static TestPage, seconds: u64) {
        let words = Region::<ReadWrite>::test_page(page).into_words().unwrap();
        let [a, b, c, d, e, f, g, h] = seconds.to_le_bytes();
        assert!(words.store(0, u32::from_ne_bytes([a, b, c, d])));
        assert!(words.store(1, u32::from_ne_bytes([e, f, g, h])));
    }

    #[test]
    fn the_build_time_is_two_little_endian_words_of_seconds() {
        let page = TestPage::fresh();
        let read = || build_time_in(&Region::<ReadOnly>::test_page(page).into_words().unwrap());
        // Both words matter: a time past 2106 needs the high one.
        for seconds in [1, 1_800_000_000, 5_000_000_000, 18_446_744_073] {
            write_build_time(page, seconds);
            assert_eq!(
                read(),
                Ok(Utc::from_unix_seconds(seconds).unwrap()),
                "{seconds}"
            );
        }
        // Nothing written - 1970 itself - is no floor: a typed error, so the
        // clock fails closed.
        write_build_time(page, 0);
        assert_eq!(read(), Err(RegionError::NoBuildTime));
        assert!(
            RegionError::NoBuildTime
                .to_string()
                .contains("the region holds no build time")
        );
        // Past what a wall-clock time holds: a typed error, never a wrapped time.
        for seconds in [18_446_744_074, u64::MAX] {
            write_build_time(page, seconds);
            assert_eq!(read(), Err(RegionError::NotATime { seconds }));
        }
        assert!(
            RegionError::NotATime { seconds: 1 }
                .to_string()
                .contains("past what a wall-clock time can hold")
        );
    }

    #[test]
    fn the_build_time_region_is_bound_by_its_symbols() {
        // Read-only region 6's symbols are this test's alone.
        assert_eq!(
            build_time().err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_ro_region_6_vaddr"
            })
        );
        let page = TestPage::fresh();
        write_build_time(page, 1_800_000_000);
        // Smaller than a page: too small to view.
        patch::<ReadOnly>(BUILD_TIME_REGION, Some(page), 8);
        assert_eq!(
            build_time().err(),
            Some(RegionError::TooSmall {
                size: 8,
                needed: PAGE
            })
        );
        patch::<ReadOnly>(BUILD_TIME_REGION, Some(page), PAGE);
        assert_eq!(
            build_time(),
            Ok(Utc::from_unix_seconds(1_800_000_000).unwrap())
        );
        // Bound once: a second read is refused.
        assert_eq!(
            build_time().err(),
            Some(RegionError::AlreadyTaken {
                symbol: "nowayos_ro_region_6_vaddr"
            })
        );
    }

    #[test]
    fn views_must_fit() {
        assert_eq!(check_fits::<ControlLayout>(0x1000, 8), Ok(()));
        assert_eq!(
            check_fits::<ControlLayout>(0x1002, 4096),
            Err(RegionError::Misaligned {
                vaddr: 0x1002,
                align: 4
            })
        );
        assert_eq!(
            check_fits::<ControlLayout>(0x1000, 7),
            Err(RegionError::TooSmall { size: 7, needed: 8 })
        );
        assert_eq!(
            check_fits::<PageLayout>(usize::MAX - 4095, 4096),
            Err(RegionError::OutOfRange {
                vaddr: usize::MAX - 4095
            })
        );
        assert_eq!(
            check_fits::<MessageLayout<4, 32>>(0x4000_0000, 4096),
            Ok(())
        );
    }

    #[test]
    fn layouts_have_the_documented_sizes() {
        // Consumer index, then the consumer's restart epoch.
        assert_eq!(size_of::<ControlLayout>(), 8);
        assert_eq!(size_of::<PageLayout>(), 4096);
        // Producer index, then 4 slots of 32 bytes, then the producer's
        // restart epoch; slots that end off a multiple of 4 are padded to it.
        assert_eq!(size_of::<MessageLayout<4, 32>>(), 4 + 4 * 32 + 4);
        assert_eq!(size_of::<MessageLayout<3, 5>>(), 4 + 3 * 5 + 1 + 4);
    }

    #[test]
    fn errors_name_the_symbol_to_fix() {
        let not_mapped = RegionError::NotMapped {
            symbol: "nowayos_ro_region_0_vaddr",
        };
        assert!(not_mapped.to_string().contains("nowayos_ro_region_0_vaddr"));
        let no_size = RegionError::SizeUnknown {
            symbol: "nowayos_rw_region_2_size",
        };
        assert!(
            no_size
                .to_string()
                .contains("setvar_size=\"nowayos_rw_region_2_size\"")
        );
        assert_eq!(
            RegionError::NoSuchRegion { index: 16 }.to_string(),
            "no region 16: indices run from 0 to 15"
        );
    }
}
