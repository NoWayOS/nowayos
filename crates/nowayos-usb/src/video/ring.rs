// SPDX-License-Identifier: MIT OR Apache-2.0
//! The bounded ring of frame slots between the USB host PD and its client:
//! fixed-size slots of whole frames, each behind a record, and
//! two counts, each written by one side alone.
//!
//! Three regions, so that each side writes only what it owns (the
//! narrowest rights, as `nowayos-region`'s rings):
//!
//! | Region | Holds | USB host PD | Client |
//! | --- | --- | --- | --- |
//! | frames | [`Layout::slots`] slots of [`Layout::slot_bytes`]: a [`Record`], then the frame | `rw` | `r` |
//! | the host's page | [`word`]s: the layout's version, slots and slot size, the frames written, the drop counts, the camera's offer | `rw` | `r` |
//! | the client's page | words [`RELEASED`]: the frames it has finished with; [`WANTED`]: whether it wants frames | `r` | `rw` |
//!
//! Frame `n` (counting from 0, wrapping at 2^32) goes in slot `n` modulo the
//! slots. The host writes a frame and its record into a slot no frame
//! unreleased holds, then stores the count written with release ordering
//! and notifies; the client loads it with acquire ordering, copies each new
//! slot's record out before checking it ([`Record::decode`]), reads the
//! frame, and stores its count released, with release ordering. **Each
//! side checks the other's count** - [`released`] and [`ready`] - so a
//! client that lies cannot make the host write a slot it is reading, and
//! a host that lies cannot make the client read past the slots; neither
//! can make the other read or write outside the regions. A frame's bytes
//! are opaque: only their length is checked.
//!
//! **The camera streams only while the client wants frames**: it
//! stores 1 in word [`WANTED`] to ask for them and 0 to stop; the host
//! checks the word ([`wanted`]) each time it looks, and takes any other
//! value as a stop. So that the client knows what a camera would give
//! before it wants any, the host's page holds its **offer** - the frame
//! size and interval it is set up for, [`word::SIZE`] and
//! [`word::INTERVAL`] - from when it is set up until it goes; the client
//! checks it ([`offer`]).
//!
//! All integers are little-endian. The record, 16 bytes at the slot's
//! start:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 4 | the frame's number |
//! | 4 | 4 | its bytes, at most the slot's less 16 |
//! | 8 | 2 | its width |
//! | 10 | 2 | its height |
//! | 12 | 4 | its interval, in 100 ns units |

use core::fmt;

use crate::{le16, le32};

/// The layout's version, word [`word::VERSION`] of the host's page: 2 since
/// the client's page has [`WANTED`].
pub const VERSION: u32 = 2;

/// The bytes of a slot's record.
pub const RECORD_LEN: usize = 16;

/// The most slots a ring has.
pub const MAX_SLOTS: u32 = 16;

/// The words of the host's page.
pub mod word {
    /// The layout's version: [`super::VERSION`].
    pub const VERSION: usize = 0;
    /// The slots.
    pub const SLOTS: usize = 1;
    /// The bytes of each slot.
    pub const SLOT_BYTES: usize = 2;
    /// The frames written, wrapping.
    pub const WRITTEN: usize = 3;
    /// Frames dropped as the client was behind, wrapping.
    pub const BEHIND: usize = 4;
    /// Frames dropped as damaged or too large, wrapping.
    pub const DAMAGED: usize = 5;
    /// The camera's frame size as set up, its width in bits 15:0 and its
    /// height in bits 31:16 ([`super::Offer::size_word`]); 0 while no
    /// camera is set up. Stored after [`INTERVAL`] and cleared
    /// before it, so a client that loads it first and sees a size sees its
    /// interval too.
    pub const SIZE: usize = 6;
    /// The camera's frame interval as set up, in 100 ns units.
    pub const INTERVAL: usize = 7;
}

/// A word of the client's page: the frames it has finished with,
/// wrapping.
pub const RELEASED: usize = 0;

/// A word of the client's page: 1 while it wants frames, 0 while it does
/// not.
pub const WANTED: usize = 1;

/// Why the ring or a record was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// The host's page names another layout version.
    Version(u32),
    /// Slots and a slot size that do not fit the region, or none.
    Layout {
        /// The slots.
        slots: u32,
        /// Each one's bytes.
        slot_bytes: u32,
    },
    /// A count from the other side that would put more than the slots in
    /// flight, or move back.
    Count {
        /// The count read.
        found: u32,
    },
    /// A record for another frame than the one its slot should hold.
    Sequence {
        /// The number in the record.
        found: u32,
        /// The frame expected.
        expected: u32,
    },
    /// A client's [`WANTED`] word that is neither 0 nor 1.
    Wanted {
        /// The word read.
        found: u32,
    },
    /// A record whose frame is longer than its slot holds.
    Length {
        /// Its length.
        len: u32,
        /// What the slot holds.
        room: usize,
    },
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(found) => write!(f, "layout version {found}, not {VERSION}"),
            Self::Layout { slots, slot_bytes } => {
                write!(f, "{slots} slots of {slot_bytes} bytes do not fit")
            }
            Self::Count { found } => write!(f, "a count of {found} out of range"),
            Self::Sequence { found, expected } => {
                write!(f, "a record for frame {found}, not {expected}")
            }
            Self::Wanted { found } => write!(f, "a frames-wanted word of {found}, not 0 or 1"),
            Self::Length { len, room } => write!(f, "a frame of {len} bytes in a slot of {room}"),
        }
    }
}

/// The slots of a ring over a region, checked to fit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    slots: u32,
    slot_bytes: u32,
}

impl Layout {
    /// `slots` slots over a frames region of `region_len` bytes, each as
    /// large as fits, in whole 4 KiB pages: the host's layout.
    ///
    /// # Errors
    ///
    /// [`RingError::Layout`] for slots outside 1 to [`MAX_SLOTS`], or a
    /// region too small for a page each.
    pub fn new(slots: u32, region_len: usize) -> Result<Self, RingError> {
        let each = region_len
            .checked_div(usize::try_from(slots).unwrap_or(usize::MAX))
            .unwrap_or(0);
        let slot_bytes = u32::try_from(each & !0xFFF).unwrap_or(0xFFFF_F000);
        Self::check(VERSION, slots, slot_bytes, region_len)
    }

    /// The layout the host's page states - its version, slots and slot
    /// size, as loaded - checked against the client's own view of the
    /// frames region, `region_len` bytes: the client's layout.
    ///
    /// # Errors
    ///
    /// [`RingError::Version`] for another version; [`RingError::Layout`]
    /// for slots outside 1 to [`MAX_SLOTS`], a slot no larger than its
    /// record or not a multiple of 8 bytes, or slots that together do not
    /// fit the region.
    pub fn check(
        version: u32,
        slots: u32,
        slot_bytes: u32,
        region_len: usize,
    ) -> Result<Self, RingError> {
        if version != VERSION {
            return Err(RingError::Version(version));
        }
        let refused = RingError::Layout { slots, slot_bytes };
        let total = u64::from(slots).saturating_mul(u64::from(slot_bytes));
        if !(1..=MAX_SLOTS).contains(&slots)
            || usize::try_from(slot_bytes).map_or(true, |bytes| bytes <= RECORD_LEN)
            || !slot_bytes.is_multiple_of(8)
            || total > u64::try_from(region_len).unwrap_or(u64::MAX)
        {
            return Err(refused);
        }
        Ok(Self { slots, slot_bytes })
    }

    /// The slots.
    #[must_use]
    pub const fn slots(&self) -> u32 {
        self.slots
    }

    /// The bytes of each slot.
    #[must_use]
    pub const fn slot_bytes(&self) -> u32 {
        self.slot_bytes
    }

    /// The most bytes of a frame a slot holds: the slot less its record.
    #[must_use]
    pub fn room(&self) -> usize {
        usize::try_from(self.slot_bytes)
            .unwrap_or(0)
            .saturating_sub(RECORD_LEN)
    }

    /// Where frame `sequence`'s slot starts in the frames region: inside
    /// it, a whole slot before its end.
    #[must_use]
    pub fn slot_at(&self, sequence: u32) -> usize {
        let index = sequence.checked_rem(self.slots).unwrap_or(0);
        usize::try_from(u64::from(index).saturating_mul(u64::from(self.slot_bytes))).unwrap_or(0)
    }
}

/// A slot's record: which frame it holds, and the frame's length, size and
/// interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// The frame's number.
    pub sequence: u32,
    /// Its bytes, after the record.
    pub len: u32,
    /// Its width.
    pub width: u16,
    /// Its height.
    pub height: u16,
    /// Its frame interval, in 100 ns units.
    pub interval: u32,
}

impl Record {
    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        let fields = self
            .sequence
            .to_le_bytes()
            .into_iter()
            .chain(self.len.to_le_bytes())
            .chain(self.width.to_le_bytes())
            .chain(self.height.to_le_bytes())
            .chain(self.interval.to_le_bytes());
        for (slot, byte) in out.iter_mut().zip(fields) {
            *slot = byte;
        }
        out
    }

    /// The record copied out of frame `sequence`'s slot, checked: it must
    /// name that frame, and a length its slot holds.
    ///
    /// # Errors
    ///
    /// [`RingError::Sequence`] for a record of another frame;
    /// [`RingError::Length`] for a frame longer than [`Layout::room`].
    pub fn decode(
        bytes: &[u8; RECORD_LEN],
        sequence: u32,
        layout: &Layout,
    ) -> Result<Self, RingError> {
        let record = Self {
            sequence: le32(bytes, 0).unwrap_or(0),
            len: le32(bytes, 4).unwrap_or(0),
            width: le16(bytes, 8).unwrap_or(0),
            height: le16(bytes, 10).unwrap_or(0),
            interval: le32(bytes, 12).unwrap_or(0),
        };
        if record.sequence != sequence {
            return Err(RingError::Sequence {
                found: record.sequence,
                expected: sequence,
            });
        }
        let room = layout.room();
        if usize::try_from(record.len).map_or(true, |len| len > room) {
            return Err(RingError::Length {
                len: record.len,
                room,
            });
        }
        Ok(record)
    }
}

/// The host's check of the client's count released, `found` as loaded:
/// at or past `before`, the count it last took, and not past `written`.
/// Returns the count to take.
///
/// # Errors
///
/// [`RingError::Count`] for a count that moves back or past `written`;
/// the host keeps `before`, as if the client had released nothing.
pub fn released(written: u32, before: u32, found: u32) -> Result<u32, RingError> {
    if found.wrapping_sub(before) > written.wrapping_sub(before) {
        return Err(RingError::Count { found });
    }
    Ok(found)
}

/// What the host's camera is set up to give: its frame size and
/// interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The frames' width.
    pub width: u16,
    /// The frames' height.
    pub height: u16,
    /// The frame interval, in 100 ns units.
    pub interval: u32,
}

impl Offer {
    /// Word [`word::SIZE`] for it.
    #[must_use]
    pub const fn size_word(&self) -> u32 {
        (self.width as u32) | ((self.height as u32) << 16)
    }
}

/// The client's check of the host's offer, words [`word::SIZE`] and
/// [`word::INTERVAL`] as loaded, the size first: `None` while no camera is
/// set up - a size of 0 - or for words that make no offer, a width, height
/// or interval of 0.
#[must_use]
pub const fn offer(size: u32, interval: u32) -> Option<Offer> {
    let width = (size & 0xFFFF) as u16;
    let height = (size >> 16) as u16;
    if width == 0 || height == 0 || interval == 0 {
        return None;
    }
    Some(Offer {
        width,
        height,
        interval,
    })
}

/// The host's check of the client's [`WANTED`] word, `found` as loaded:
/// whether it wants frames.
///
/// # Errors
///
/// [`RingError::Wanted`] for a word that is neither 0 nor 1; the host
/// takes it as a stop.
pub const fn wanted(found: u32) -> Result<bool, RingError> {
    match found {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(RingError::Wanted { found }),
    }
}

/// The slots free for the host to write: the slots less the frames written
/// and not yet released.
#[must_use]
pub fn free(layout: &Layout, written: u32, released: u32) -> u32 {
    layout
        .slots
        .saturating_sub(written.wrapping_sub(released).min(layout.slots))
}

/// The client's check of the host's count written, `found` as loaded:
/// returns the frames ready to read, at most the slots.
///
/// # Errors
///
/// [`RingError::Count`] for a count that would put more than the slots in
/// flight past `released`, the client's own count - or one behind it.
pub fn ready(layout: &Layout, released: u32, found: u32) -> Result<u32, RingError> {
    let ahead = found.wrapping_sub(released);
    if ahead > layout.slots {
        return Err(RingError::Count { found });
    }
    Ok(ahead)
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
    fn an_offer_round_trips_and_none_is_taken_from_a_zero() {
        let offer_720 = Offer {
            width: 1280,
            height: 720,
            interval: 333_333,
        };
        assert_eq!(offer_720.size_word(), 0x02D0_0500);
        assert_eq!(offer(offer_720.size_word(), 333_333), Some(offer_720));
        assert_eq!(offer(0, 333_333), None, "no camera set up");
        assert_eq!(offer(0x02D0_0000, 333_333), None, "no width");
        assert_eq!(offer(0x0000_0500, 333_333), None, "no height");
        assert_eq!(offer(0x02D0_0500, 0), None, "no interval");
    }

    #[test]
    fn the_wanted_word_is_0_or_1_and_nothing_else() {
        assert_eq!(wanted(0), Ok(false));
        assert_eq!(wanted(1), Ok(true));
        for found in [2, 0x100, u32::MAX] {
            assert_eq!(wanted(found), Err(RingError::Wanted { found }));
        }
    }

    #[test]
    fn a_layout_over_a_region() {
        let layout = Layout::new(4, 2 << 20).unwrap();
        assert_eq!((layout.slots(), layout.slot_bytes()), (4, 512 << 10));
        assert_eq!(layout.room(), (512 << 10) - 16);
        assert_eq!(layout.slot_at(0), 0);
        assert_eq!(layout.slot_at(5), 512 << 10);
        assert_eq!(layout.slot_at(u32::MAX), 3 * (512 << 10));
        // Not a page each, no slots, too many.
        assert!(Layout::new(4, 3 * 4096).is_err());
        assert!(Layout::new(0, 1 << 20).is_err());
        assert!(Layout::new(MAX_SLOTS + 1, 1 << 30).is_err());
        // The client's checks of what the host's page says.
        assert_eq!(
            Layout::check(1, 4, 4096, 1 << 20),
            Err(RingError::Version(1)),
            "a host from before the client's page had WANTED"
        );
        assert!(Layout::check(VERSION, 4, 4096, 4 * 4096).is_ok());
        assert!(Layout::check(VERSION, 4, 4096, 4 * 4096 - 1).is_err());
        assert!(
            Layout::check(VERSION, 2, 16, 1 << 20).is_err(),
            "no room past the record"
        );
        assert!(
            Layout::check(VERSION, 2, 4100, 1 << 20).is_err(),
            "not a multiple of 8"
        );
        assert!(Layout::check(VERSION, 16, 0xFFFF_FFF8, usize::MAX).is_ok());
    }

    #[test]
    fn records_round_trip_and_are_checked() {
        let layout = Layout::new(2, 64 << 10).unwrap();
        let record = Record {
            sequence: 7,
            len: 12_345,
            width: 1280,
            height: 720,
            interval: 333_333,
        };
        let bytes = record.to_bytes();
        assert_eq!(&bytes[..4], &7u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &[0x00, 0x05, 0xD0, 0x02]);
        assert_eq!(Record::decode(&bytes, 7, &layout), Ok(record));
        assert_eq!(
            Record::decode(&bytes, 8, &layout),
            Err(RingError::Sequence {
                found: 7,
                expected: 8
            })
        );
        let long = Record {
            len: (32 << 10) - 15,
            ..record
        };
        assert_eq!(
            Record::decode(&long.to_bytes(), 7, &layout),
            Err(RingError::Length {
                len: (32 << 10) - 15,
                room: (32 << 10) - 16
            })
        );
    }

    #[test]
    fn each_side_checks_the_others_count() {
        let layout = Layout::new(4, 1 << 20).unwrap();
        assert_eq!(free(&layout, 10, 6), 0);
        assert_eq!(free(&layout, 10, 8), 2);
        assert_eq!(free(&layout, 10, 10), 4);
        assert_eq!(released(10, 6, 9), Ok(9));
        assert_eq!(released(10, 6, 6), Ok(6));
        assert_eq!(released(10, 6, 11), Err(RingError::Count { found: 11 }));
        assert_eq!(
            released(10, 6, 5),
            Err(RingError::Count { found: 5 }),
            "back"
        );
        // Across the wrap.
        assert_eq!(released(2, u32::MAX - 1, 1), Ok(1));
        assert_eq!(free(&layout, 2, u32::MAX - 1), 0);
        assert_eq!(ready(&layout, 6, 10), Ok(4));
        assert_eq!(ready(&layout, 6, 11), Err(RingError::Count { found: 11 }));
        assert_eq!(
            ready(&layout, 6, 5),
            Err(RingError::Count { found: 5 }),
            "behind"
        );
        assert_eq!(ready(&layout, u32::MAX, 2), Ok(3));
    }

    proptest! {
        #[test]
        fn any_record_or_count_is_checked_without_panicking(
            bytes: [u8; RECORD_LEN], sequence: u32, slots in 0u32..20, region in 0usize..(8 << 20),
            written: u32, before: u32, found: u32,
        ) {
            if let Ok(layout) = Layout::new(slots, region) {
                prop_assert!(layout.slot_at(sequence) + layout.slot_bytes() as usize <= region);
                if let Ok(record) = Record::decode(&bytes, sequence, &layout) {
                    prop_assert!(record.len as usize <= layout.room());
                }
                if let Ok(count) = ready(&layout, before, found) {
                    prop_assert!(count <= layout.slots());
                }
                if written.wrapping_sub(before) <= layout.slots()
                    && let Ok(taken) = released(written, before, found)
                {
                    prop_assert!(free(&layout, written, taken) >= free(&layout, written, before));
                    prop_assert!(written.wrapping_sub(taken) <= layout.slots());
                }
            }
        }
    }
}
