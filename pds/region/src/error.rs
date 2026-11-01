// SPDX-License-Identifier: MIT OR Apache-2.0
//! The error type. Every way binding or viewing a region can fail has a
//! variant here; nothing in this crate panics instead.

use core::fmt;

/// Why a region could not be bound or viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegionError {
    /// There is no region with this number: the index is not below
    /// [`REGIONS_PER_KIND`](crate::REGIONS_PER_KIND).
    NoSuchRegion {
        /// The index asked for.
        index: usize,
    },
    /// The Microkit tool did not patch the region's address: the system
    /// description has no `<map>` with `setvar_vaddr` set to `symbol` for
    /// this PD.
    NotMapped {
        /// The address symbol that is still zero.
        symbol: &'static str,
    },
    /// The region's address was patched but its size was not: the `<map>`
    /// has no `setvar_size`.
    SizeUnknown {
        /// The size symbol that is still zero.
        symbol: &'static str,
    },
    /// The region was already bound in this PD. Each region is bound once,
    /// so it is only ever viewed through one layout.
    AlreadyTaken {
        /// The region's address symbol.
        symbol: &'static str,
    },
    /// The region's address is not aligned for the view asked for.
    Misaligned {
        /// The region's virtual address.
        vaddr: usize,
        /// The alignment the view needs.
        align: usize,
    },
    /// The region is smaller than the view asked for.
    TooSmall {
        /// The region's size in bytes.
        size: usize,
        /// The bytes the view needs.
        needed: usize,
    },
    /// The view would run past the end of the address space.
    OutOfRange {
        /// The region's virtual address.
        vaddr: usize,
    },
    /// The two regions given to a ring overlap in this PD's address space.
    /// Each side's index must live in memory the other side cannot write.
    ///
    /// Only addresses can be compared here: two `<map>`s of the *same*
    /// memory region at different addresses look disjoint from inside the
    /// PD. `tools/check-system` refuses such a system description when the
    /// image is built.
    Overlap,
    /// The image's build time is past what a wall-clock time can hold
    /// (`nowayos_time::Utc`, 2554): `make image` never writes one.
    NotATime {
        /// The count of seconds since 1970 the region holds.
        seconds: u64,
    },
    /// The image's build time is 0, 1970 itself: what a region nothing was
    /// written to holds, and no floor at all - `make image` never writes
    /// it, so the clock fails closed rather than believe any sample since
    /// 1970.
    NoBuildTime,
    /// The device facts' region holds no record (version 0): what a region
    /// nothing was written to holds - `make image` always writes one, from
    /// `device-NAME.bin` - so the driver refuses its device.
    NoDeviceFacts,
    /// The device facts' record is of a version this PD does not read, or
    /// breaks its rules (`reason`): `make image` never writes one.
    BadDeviceFacts {
        /// What is wrong with it.
        reason: &'static str,
    },
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchRegion { index } => write!(
                f,
                "no region {index}: indices run from 0 to {}",
                crate::REGIONS_PER_KIND.saturating_sub(1)
            ),
            Self::NotMapped { symbol } => write!(
                f,
                "region not mapped: no <map> in the system description sets `{symbol}`"
            ),
            Self::SizeUnknown { symbol } => write!(
                f,
                "region size unknown: the <map> must also set setvar_size=\"{symbol}\""
            ),
            Self::AlreadyTaken { symbol } => {
                write!(f, "region `{symbol}` is already bound in this PD")
            }
            Self::Misaligned { vaddr, align } => {
                write!(f, "region at {vaddr:#x} is not {align}-byte aligned")
            }
            Self::TooSmall { size, needed } => write!(
                f,
                "region of {size} bytes is too small: {needed} bytes needed"
            ),
            Self::OutOfRange { vaddr } => {
                write!(
                    f,
                    "region at {vaddr:#x} runs past the end of the address space"
                )
            }
            Self::Overlap => f.write_str("the ring's two regions overlap"),
            Self::NotATime { seconds } => write!(
                f,
                "a build time of {seconds} s after 1970 is past what a wall-clock time can hold"
            ),
            Self::NoBuildTime => f.write_str(
                "the build time is 0, 1970 itself: the region holds no build time (was it prefilled from build-time.bin?)",
            ),
            Self::NoDeviceFacts => f.write_str(
                "the region holds no device facts (was it prefilled from device-NAME.bin?)",
            ),
            Self::BadDeviceFacts { reason } => {
                write!(f, "the device facts are refused: {reason}")
            }
        }
    }
}

impl core::error::Error for RegionError {}
