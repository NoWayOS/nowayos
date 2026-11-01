// SPDX-License-Identifier: MIT OR Apache-2.0
//! The restart image: what a protection domain's writable memory held when
//! its program was loaded, so that a restart can put it back.
//!
//! `tools/restart-image` makes it at build time from the writable loadable
//! segments of the PD's program image, its `.data` and `.bss`, and the
//! Microkit tool loads it into a memory region mapped read-only into the PD
//! and, if it restarts the PD, into its parent (`tools/check-system`, rule
//! 13). No PD can change it.
//!
//! # Format (version 1)
//!
//! All integers are little-endian.
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 8 | magic: `BRESTART` |
//! | 8 | 4 | version: 1 |
//! | 12 | 4 | segments: the program's writable loadable segments, 1 to [`MAX_SEGMENTS`] |
//! | 16 | 4 | kept ranges: 0 to [`MAX_KEEPS`] |
//! | 20 | 4 | reserved: 0 |
//! | 24 | 8 | the restart entry: the program's entry point, `nowayos_restart_entry` |
//! | 32 | 8 | the stack pointer the PD started with |
//! | 40 | 32 per segment | its address, its bytes in the file, its bytes in memory, and the offset of its file bytes in this image; in address order, apart |
//! | then | 16 per kept range | its address and size; in address order, apart, each inside one segment's file bytes |
//! | then | | each segment's file bytes, at its offset |
//!
//! A segment's memory past its file bytes - its `.bss` - starts as zeros.
//!
//! A **kept range** is a word the Microkit tool patches into the program as
//! it builds the system image - a region's address, the PD's name, its
//! channels - which the program image does not hold. No code writes one, so
//! a restart keeps its value, and a cold start does not compare it. Should
//! the tool patch a word the image does not keep, the cold start finds that
//! byte different and the PD stops: the image never silently restores a
//! wrong value.

use core::fmt;
use core::ops::Range;

/// The first eight bytes of every restart image.
pub const MAGIC: [u8; 8] = *b"BRESTART";
/// The format version this crate reads.
pub const VERSION: u32 = 1;
/// The bytes before the segment table.
pub const HEADER_LEN: usize = 40;
/// The bytes of one segment's entry in the table.
pub const SEGMENT_LEN: usize = 32;
/// The bytes of one kept range's entry in the table.
pub const KEEP_LEN: usize = 16;
/// The most writable segments a program may have.
pub const MAX_SEGMENTS: usize = 4;
/// The most kept ranges an image may list.
pub const MAX_KEEPS: usize = 64;

/// Why a restart image cannot be used, or what a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// Shorter than its header and tables.
    TooShort,
    /// It does not start with [`MAGIC`].
    NotAnImage,
    /// A version this crate does not read.
    UnknownVersion(u32),
    /// The reserved field is not zero.
    ReservedNotZero,
    /// No segments, or more than [`MAX_SEGMENTS`].
    SegmentCount(u32),
    /// More than [`MAX_KEEPS`] kept ranges.
    KeepCount(u32),
    /// The restart entry is zero.
    NoEntry,
    /// The stack pointer is zero or not 16-byte aligned.
    StackTop(u64),
    /// A segment's sizes are impossible, or its file bytes lie outside the
    /// image.
    BadSegment {
        /// Its place in the table, from 0.
        index: usize,
    },
    /// A segment starts before the one before it ends.
    SegmentsOverlap {
        /// Its place in the table, from 0.
        index: usize,
    },
    /// A kept range is empty, lies outside every segment's file bytes, or
    /// starts before the one before it ends.
    BadKeep {
        /// Its place in the table, from 0.
        index: usize,
    },
    /// The program's writable segments are not the image's: it was made
    /// from another program.
    ProgramMismatch,
    /// The memory given for a segment is not its size in memory.
    MemoryLength,
    /// At a cold start, a byte of a segment's file bytes is not what the
    /// image holds.
    Differs {
        /// The byte's address.
        vaddr: u64,
    },
    /// At a cold start, a byte past a segment's file bytes is not zero.
    NotZero {
        /// The byte's address.
        vaddr: u64,
    },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("the restart image is shorter than its tables"),
            Self::NotAnImage => f.write_str("the region is not a restart image (no BRESTART)"),
            Self::UnknownVersion(version) => {
                write!(f, "restart image version {version}: this program reads version 1")
            }
            Self::ReservedNotZero => f.write_str("the restart image's reserved field is not zero"),
            Self::SegmentCount(count) => write!(
                f,
                "the restart image lists {count} segments: 1 to {MAX_SEGMENTS} are allowed"
            ),
            Self::KeepCount(count) => write!(
                f,
                "the restart image lists {count} kept ranges: at most {MAX_KEEPS} are allowed"
            ),
            Self::NoEntry => f.write_str("the restart image has no restart entry"),
            Self::StackTop(top) => write!(
                f,
                "the restart image's stack pointer {top:#x} is zero or not 16-byte aligned"
            ),
            Self::BadSegment { index } => write!(
                f,
                "the restart image's segment {index} has impossible sizes, or bytes outside the image"
            ),
            Self::SegmentsOverlap { index } => write!(
                f,
                "the restart image's segment {index} starts before the one before it ends"
            ),
            Self::BadKeep { index } => write!(
                f,
                "the restart image's kept range {index} is empty, outside every segment's file bytes, or overlaps the one before"
            ),
            Self::ProgramMismatch => f.write_str(
                "the program's writable segments are not the restart image's: it was made from another program",
            ),
            Self::MemoryLength => {
                f.write_str("the memory given for a segment is not its size in memory")
            }
            Self::Differs { vaddr } => write!(
                f,
                "byte {vaddr:#x} is not what the restart image holds: it was made from another program, or the Microkit tool patched a word it does not keep"
            ),
            Self::NotZero { vaddr } => {
                write!(f, "byte {vaddr:#x}, in .bss, is not zero at the cold start")
            }
        }
    }
}

/// The restart image's header: what a parent needs to restart the PD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The restart entry: where the PD's program starts, cold or restarted.
    pub entry: u64,
    /// The stack pointer the PD started with.
    pub stack_top: u64,
    segments: usize,
    keeps: usize,
}

/// One of the program's writable loadable segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Its address.
    pub vaddr: u64,
    /// Its bytes in the program image: `.data` and the like.
    pub filesz: u64,
    /// Its bytes in memory: the file bytes, then zeros (`.bss`).
    pub memsz: u64,
    offset: u64,
}

/// A range the Microkit tool patches, which a restart keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    /// Its address.
    pub vaddr: u64,
    /// Its size in bytes.
    pub size: u64,
}

/// The little-endian `u32` at `offset`, if `bytes` holds it.
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

/// The little-endian `u64` at `offset`, if `bytes` holds it.
fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let field = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes(field.try_into().ok()?))
}

/// The range `start..start + len` as offsets into this address space, if
/// it fits.
fn span(start: u64, len: u64) -> Option<Range<usize>> {
    let start = usize::try_from(start).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    Some(start..end)
}

impl Header {
    /// Reads and checks the header alone: enough for a parent, which copies
    /// only the first [`HEADER_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`ImageError`] for a header that is short, of another format or
    /// version, or whose counts, entry or stack pointer are impossible.
    pub fn parse(bytes: &[u8]) -> Result<Self, ImageError> {
        let magic = bytes.get(..MAGIC.len()).ok_or(ImageError::TooShort)?;
        if bytes.len() < HEADER_LEN {
            return Err(ImageError::TooShort);
        }
        if magic != MAGIC {
            return Err(ImageError::NotAnImage);
        }
        let field32 = |offset| u32_at(bytes, offset).ok_or(ImageError::TooShort);
        let field64 = |offset| u64_at(bytes, offset).ok_or(ImageError::TooShort);
        let version = field32(8)?;
        if version != VERSION {
            return Err(ImageError::UnknownVersion(version));
        }
        let segments = field32(12)?;
        let keeps = field32(16)?;
        if field32(20)? != 0 {
            return Err(ImageError::ReservedNotZero);
        }
        let segments_len =
            usize::try_from(segments).map_err(|_| ImageError::SegmentCount(segments))?;
        if segments_len == 0 || segments_len > MAX_SEGMENTS {
            return Err(ImageError::SegmentCount(segments));
        }
        let keeps_len = usize::try_from(keeps).map_err(|_| ImageError::KeepCount(keeps))?;
        if keeps_len > MAX_KEEPS {
            return Err(ImageError::KeepCount(keeps));
        }
        let entry = field64(24)?;
        if entry == 0 {
            return Err(ImageError::NoEntry);
        }
        let stack_top = field64(32)?;
        if stack_top == 0 || stack_top % 16 != 0 {
            return Err(ImageError::StackTop(stack_top));
        }
        Ok(Self {
            entry,
            stack_top,
            segments: segments_len,
            keeps: keeps_len,
        })
    }

    /// Where the segment table ends: the kept ranges' table starts.
    fn keeps_start(&self) -> Option<usize> {
        HEADER_LEN.checked_add(self.segments.checked_mul(SEGMENT_LEN)?)
    }

    /// Where both tables end.
    fn tables_end(&self) -> Option<usize> {
        self.keeps_start()?
            .checked_add(self.keeps.checked_mul(KEEP_LEN)?)
    }
}

/// A checked restart image over its bytes.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    bytes: &'a [u8],
    header: Header,
}

impl<'a> Image<'a> {
    /// Reads and checks a whole restart image: its header, then every
    /// segment and kept range (see the module documentation).
    ///
    /// # Errors
    ///
    /// [`ImageError`] naming the first thing wrong.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ImageError> {
        let header = Header::parse(bytes)?;
        let tables_end = header.tables_end().ok_or(ImageError::TooShort)?;
        if bytes.len() < tables_end {
            return Err(ImageError::TooShort);
        }
        let image = Self { bytes, header };
        let mut previous_end = 0_u64;
        for index in 0..header.segments {
            let segment = image
                .segment(index)
                .ok_or(ImageError::BadSegment { index })?;
            let data_end = segment
                .offset
                .checked_add(segment.filesz)
                .ok_or(ImageError::BadSegment { index })?;
            let memory = span(segment.vaddr, segment.memsz);
            if segment.memsz == 0
                || segment.filesz > segment.memsz
                || memory.is_none()
                || segment.offset < tables_end as u64
                || data_end > bytes.len() as u64
            {
                return Err(ImageError::BadSegment { index });
            }
            if index > 0 && segment.vaddr < previous_end {
                return Err(ImageError::SegmentsOverlap { index });
            }
            previous_end = segment
                .vaddr
                .checked_add(segment.memsz)
                .ok_or(ImageError::BadSegment { index })?;
        }
        let mut previous_end = 0_u64;
        for index in 0..header.keeps {
            let keep = image.keep(index).ok_or(ImageError::BadKeep { index })?;
            let end = keep
                .vaddr
                .checked_add(keep.size)
                .ok_or(ImageError::BadKeep { index })?;
            let inside = image.segments().any(|segment| {
                segment
                    .vaddr
                    .checked_add(segment.filesz)
                    .is_some_and(|file_end| keep.vaddr >= segment.vaddr && end <= file_end)
            });
            if keep.size == 0 || !inside || (index > 0 && keep.vaddr < previous_end) {
                return Err(ImageError::BadKeep { index });
            }
            previous_end = end;
        }
        Ok(image)
    }

    /// The header: the restart entry and stack pointer.
    #[must_use]
    pub const fn header(&self) -> Header {
        self.header
    }

    /// Segment `index` from the table, unchecked beyond its fields being
    /// there.
    fn segment(&self, index: usize) -> Option<Segment> {
        let start = HEADER_LEN.checked_add(index.checked_mul(SEGMENT_LEN)?)?;
        Some(Segment {
            vaddr: u64_at(self.bytes, start)?,
            filesz: u64_at(self.bytes, start.checked_add(8)?)?,
            memsz: u64_at(self.bytes, start.checked_add(16)?)?,
            offset: u64_at(self.bytes, start.checked_add(24)?)?,
        })
    }

    /// Kept range `index` from the table, unchecked beyond its fields being
    /// there.
    fn keep(&self, index: usize) -> Option<Keep> {
        let start = self
            .header
            .keeps_start()?
            .checked_add(index.checked_mul(KEEP_LEN)?)?;
        Some(Keep {
            vaddr: u64_at(self.bytes, start)?,
            size: u64_at(self.bytes, start.checked_add(8)?)?,
        })
    }

    /// The program's writable segments, in address order.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        (0..self.header.segments).filter_map(|index| self.segment(index))
    }

    /// The kept ranges, in address order.
    pub fn keeps(&self) -> impl Iterator<Item = Keep> + '_ {
        (0..self.header.keeps).filter_map(|index| self.keep(index))
    }

    /// Checks that `writable`, the running program's writable loadable
    /// segments as `(address, file bytes, memory bytes)` in address order,
    /// are exactly the image's.
    ///
    /// # Errors
    ///
    /// [`ImageError::ProgramMismatch`] if they differ in number or in any
    /// field.
    pub fn matches_program(
        &self,
        writable: impl IntoIterator<Item = (u64, u64, u64)>,
    ) -> Result<(), ImageError> {
        let mut writable = writable.into_iter();
        for segment in self.segments() {
            if writable.next() != Some((segment.vaddr, segment.filesz, segment.memsz)) {
                return Err(ImageError::ProgramMismatch);
            }
        }
        match writable.next() {
            None => Ok(()),
            Some(_) => Err(ImageError::ProgramMismatch),
        }
    }

    /// The file bytes of `segment`.
    fn data(&self, segment: &Segment) -> Option<&'a [u8]> {
        self.bytes.get(span(segment.offset, segment.filesz)?)
    }

    /// Calls `piece` with each part of `segment`'s file bytes, as offsets
    /// from its start, that no kept range covers.
    fn unkept(
        &self,
        segment: &Segment,
        mut piece: impl FnMut(Range<usize>) -> Result<(), ImageError>,
    ) -> Result<(), ImageError> {
        let file_end = segment
            .vaddr
            .checked_add(segment.filesz)
            .ok_or(ImageError::MemoryLength)?;
        let offset = |vaddr: u64| {
            vaddr
                .checked_sub(segment.vaddr)
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(ImageError::MemoryLength)
        };
        let mut cursor = 0_usize;
        for keep in self
            .keeps()
            .filter(|keep| keep.vaddr >= segment.vaddr && keep.vaddr < file_end)
        {
            let start = offset(keep.vaddr)?;
            piece(cursor..start)?;
            cursor = offset(
                keep.vaddr
                    .checked_add(keep.size)
                    .ok_or(ImageError::MemoryLength)?,
            )?;
        }
        piece(cursor..offset(file_end)?)
    }

    /// At a cold start: checks that `memory`, all of `segment` as the
    /// program was loaded, holds the image's file bytes - kept ranges
    /// aside - and then zeros.
    ///
    /// # Errors
    ///
    /// [`ImageError::MemoryLength`] if `memory` is not the segment's size
    /// in memory; [`ImageError::Differs`] or [`ImageError::NotZero`] naming
    /// the first byte that is not as it should be.
    pub fn check(&self, segment: &Segment, memory: &[u8]) -> Result<(), ImageError> {
        let data = self.data(segment).ok_or(ImageError::MemoryLength)?;
        let filesz = data.len();
        if Some(memory.len()) != usize::try_from(segment.memsz).ok() {
            return Err(ImageError::MemoryLength);
        }
        let vaddr_of = |offset: usize| {
            segment
                .vaddr
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
        };
        self.unkept(segment, |range| {
            let (Some(found), Some(wanted)) = (memory.get(range.clone()), data.get(range.clone()))
            else {
                return Err(ImageError::MemoryLength);
            };
            match found.iter().zip(wanted).position(|(a, b)| a != b) {
                None => Ok(()),
                Some(at) => Err(ImageError::Differs {
                    vaddr: vaddr_of(range.start.saturating_add(at)),
                }),
            }
        })?;
        let zeros = memory.get(filesz..).ok_or(ImageError::MemoryLength)?;
        match zeros.iter().position(|byte| *byte != 0) {
            None => Ok(()),
            Some(at) => Err(ImageError::NotZero {
                vaddr: vaddr_of(filesz.saturating_add(at)),
            }),
        }
    }

    /// At a restart: puts the image's file bytes back into `memory`, all of
    /// `segment` - kept ranges aside - and zeros the rest.
    ///
    /// # Errors
    ///
    /// [`ImageError::MemoryLength`] if `memory` is not the segment's size in
    /// memory; nothing is written.
    pub fn restore(&self, segment: &Segment, memory: &mut [u8]) -> Result<(), ImageError> {
        let data = self.data(segment).ok_or(ImageError::MemoryLength)?;
        if Some(memory.len()) != usize::try_from(segment.memsz).ok() {
            return Err(ImageError::MemoryLength);
        }
        self.unkept(segment, |range| {
            let (Some(target), Some(source)) =
                (memory.get_mut(range.clone()), data.get(range.clone()))
            else {
                return Err(ImageError::MemoryLength);
            };
            target.copy_from_slice(source);
            Ok(())
        })?;
        memory
            .get_mut(data.len()..)
            .ok_or(ImageError::MemoryLength)?
            .fill(0);
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;

    /// A segment for `build`: address, file bytes, size in memory.
    struct Part<'a> {
        vaddr: u64,
        data: &'a [u8],
        memsz: u64,
    }

    /// A restart image, as tools/restart-image writes one.
    fn build(entry: u64, stack_top: u64, parts: &[Part<'_>], keeps: &[(u64, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        out.extend_from_slice(&(keeps.len() as u32).to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&entry.to_le_bytes());
        out.extend_from_slice(&stack_top.to_le_bytes());
        let mut offset = HEADER_LEN + parts.len() * SEGMENT_LEN + keeps.len() * KEEP_LEN;
        for part in parts {
            for field in [
                part.vaddr,
                part.data.len() as u64,
                part.memsz,
                offset as u64,
            ] {
                out.extend_from_slice(&field.to_le_bytes());
            }
            offset += part.data.len();
        }
        for (vaddr, size) in keeps {
            out.extend_from_slice(&vaddr.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
        }
        for part in parts {
            out.extend_from_slice(part.data);
        }
        out
    }

    const DATA: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];

    /// One segment at 0x1000: 16 file bytes, 32 in memory; bytes 4 to 11
    /// kept.
    fn sample() -> Vec<u8> {
        build(
            0x20_0000,
            0xff_ffff_e000,
            &[Part {
                vaddr: 0x1000,
                data: &DATA,
                memsz: 32,
            }],
            &[(0x1004, 8)],
        )
    }

    #[test]
    fn a_parent_needs_only_the_header() {
        let bytes = sample();
        let header = Header::parse(&bytes[..HEADER_LEN]).unwrap();
        assert_eq!(
            (header.entry, header.stack_top),
            (0x20_0000, 0xff_ffff_e000)
        );
        assert_eq!(Image::parse(&bytes).unwrap().header(), header);
    }

    #[test]
    fn a_cold_start_checks_every_unkept_byte_and_the_zeros() {
        let bytes = sample();
        let image = Image::parse(&bytes).unwrap();
        let segment = image.segments().next().unwrap();
        let mut memory = [0_u8; 32];
        memory[..16].copy_from_slice(&DATA);
        // The kept bytes hold what the Microkit tool patched in.
        memory[4..12].copy_from_slice(&[0xAA; 8]);
        assert_eq!(image.check(&segment, &memory), Ok(()));

        let mut changed = memory;
        changed[13] ^= 1;
        assert_eq!(
            image.check(&segment, &changed),
            Err(ImageError::Differs { vaddr: 0x100d })
        );
        let mut changed = memory;
        changed[2] ^= 1;
        assert_eq!(
            image.check(&segment, &changed),
            Err(ImageError::Differs { vaddr: 0x1002 })
        );
        let mut changed = memory;
        changed[20] = 7;
        assert_eq!(
            image.check(&segment, &changed),
            Err(ImageError::NotZero { vaddr: 0x1014 })
        );
        assert_eq!(
            image.check(&segment, &memory[..31]),
            Err(ImageError::MemoryLength)
        );
    }

    #[test]
    fn a_restart_puts_back_the_file_bytes_keeps_the_kept_and_zeros_the_rest() {
        let bytes = sample();
        let image = Image::parse(&bytes).unwrap();
        let segment = image.segments().next().unwrap();
        // As the fault left it: everything changed, the kept bytes as patched.
        let mut memory = [0xEE_u8; 32];
        memory[4..12].copy_from_slice(&[0xAA; 8]);
        image.restore(&segment, &mut memory).unwrap();
        let mut wanted = [0_u8; 32];
        wanted[..16].copy_from_slice(&DATA);
        wanted[4..12].copy_from_slice(&[0xAA; 8]);
        assert_eq!(memory, wanted);
        // What a restart restores, the next cold-start check accepts.
        assert_eq!(image.check(&segment, &memory), Ok(()));
        // A wrong size writes nothing.
        let mut short = [0xEE_u8; 31];
        assert_eq!(
            image.restore(&segment, &mut short),
            Err(ImageError::MemoryLength)
        );
        assert_eq!(short, [0xEE; 31]);
    }

    #[test]
    fn kept_ranges_may_touch_and_sit_at_either_end() {
        let bytes = build(
            1,
            16,
            &[Part {
                vaddr: 0x2000,
                data: &DATA,
                memsz: 16,
            }],
            &[(0x2000, 4), (0x2004, 4), (0x200c, 4)],
        );
        let image = Image::parse(&bytes).unwrap();
        let segment = image.segments().next().unwrap();
        let mut memory = [0x55_u8; 16];
        image.restore(&segment, &mut memory).unwrap();
        let mut wanted = [0x55_u8; 16];
        wanted[8..12].copy_from_slice(&DATA[8..12]);
        assert_eq!(memory, wanted);
    }

    #[test]
    fn two_segments_each_with_their_own_bytes() {
        let bytes = build(
            1,
            16,
            &[
                Part {
                    vaddr: 0x1000,
                    data: &DATA[..8],
                    memsz: 8,
                },
                Part {
                    vaddr: 0x3000,
                    data: &DATA[8..],
                    memsz: 12,
                },
            ],
            &[(0x3002, 2)],
        );
        let image = Image::parse(&bytes).unwrap();
        let segments: Vec<Segment> = image.segments().collect();
        assert_eq!(segments.len(), 2);
        let mut first = [0_u8; 8];
        image.restore(&segments[0], &mut first).unwrap();
        assert_eq!(first, DATA[..8]);
        let mut second = [0xEE_u8; 12];
        image.restore(&segments[1], &mut second).unwrap();
        assert_eq!(
            second,
            [0x18, 0x19, 0xEE, 0xEE, 0x1c, 0x1d, 0x1e, 0x1f, 0, 0, 0, 0]
        );
        assert_eq!(
            image.matches_program([(0x1000, 8, 8), (0x3000, 8, 12)]),
            Ok(())
        );
        assert_eq!(
            image.matches_program([(0x1000, 8, 8)]),
            Err(ImageError::ProgramMismatch)
        );
        assert_eq!(
            image.matches_program([(0x1000, 8, 8), (0x3000, 8, 16)]),
            Err(ImageError::ProgramMismatch)
        );
        assert_eq!(
            image.matches_program([(0x1000, 8, 8), (0x3000, 8, 12), (0x5000, 1, 1)]),
            Err(ImageError::ProgramMismatch)
        );
    }

    #[test]
    fn impossible_images_are_refused() {
        let part = |vaddr, memsz| Part {
            vaddr,
            data: &DATA,
            memsz,
        };
        let refused = |bytes: Vec<u8>| Image::parse(&bytes).err();
        assert_eq!(
            refused(sample()[..HEADER_LEN - 1].to_vec()),
            Some(ImageError::TooShort)
        );
        let mut magic = sample();
        magic[0] = b'X';
        assert_eq!(refused(magic), Some(ImageError::NotAnImage));
        let mut version = sample();
        version[8] = 2;
        assert_eq!(refused(version), Some(ImageError::UnknownVersion(2)));
        let mut reserved = sample();
        reserved[20] = 1;
        assert_eq!(refused(reserved), Some(ImageError::ReservedNotZero));
        assert_eq!(
            refused(build(1, 16, &[], &[])),
            Some(ImageError::SegmentCount(0))
        );
        let five: Vec<Part<'_>> = (0..5).map(|i| part(0x1000 * (i + 1), 16)).collect();
        assert_eq!(
            refused(build(1, 16, &five, &[])),
            Some(ImageError::SegmentCount(5))
        );
        let keeps: Vec<(u64, u64)> = (0..65).map(|i| (0x1000 + i, 1)).collect();
        assert_eq!(
            refused(build(1, 16, &[part(0x1000, 16)], &keeps)),
            Some(ImageError::KeepCount(65))
        );
        assert_eq!(
            refused(build(0, 16, &[part(0x1000, 16)], &[])),
            Some(ImageError::NoEntry)
        );
        assert_eq!(
            refused(build(1, 8, &[part(0x1000, 16)], &[])),
            Some(ImageError::StackTop(8))
        );
        // File bytes larger than the segment in memory.
        assert_eq!(
            refused(build(1, 16, &[part(0x1000, 15)], &[])),
            Some(ImageError::BadSegment { index: 0 })
        );
        // A segment that runs off the end of the address space.
        assert_eq!(
            refused(build(1, 16, &[part(u64::MAX - 8, 16)], &[])),
            Some(ImageError::BadSegment { index: 0 })
        );
        assert_eq!(
            refused(build(1, 16, &[part(0x1000, 16), part(0x100f, 16)], &[])),
            Some(ImageError::SegmentsOverlap { index: 1 })
        );
        // Kept ranges: empty, outside the file bytes, overlapping.
        for keeps in [
            &[(0x1000, 0)][..],
            &[(0x100c, 8)][..],
            &[(0x0ff8, 8)][..],
            &[(0x1000, 8), (0x1004, 4)][..],
        ] {
            assert!(
                matches!(
                    refused(build(1, 16, &[part(0x1000, 32)], keeps)),
                    Some(ImageError::BadKeep { .. })
                ),
                "{keeps:?}"
            );
        }
        // File bytes that lie outside the image.
        let mut truncated = sample();
        truncated.pop();
        assert_eq!(
            refused(truncated),
            Some(ImageError::BadSegment { index: 0 })
        );
        // File bytes that overlap the tables.
        let mut inside = sample();
        inside[HEADER_LEN + 24..HEADER_LEN + 32].copy_from_slice(&8_u64.to_le_bytes());
        assert_eq!(refused(inside), Some(ImageError::BadSegment { index: 0 }));
    }

    /// Parsing never panics, whatever one byte of the header and tables is,
    /// and wherever the image is cut short; and whatever it accepts, it
    /// can check and restore without panicking. Every byte value on the
    /// host; under Miri, which is far slower, every 15th.
    #[test]
    fn no_single_byte_change_or_truncation_panics() {
        let bytes = sample();
        let tables = HEADER_LEN + SEGMENT_LEN + KEEP_LEN;
        let step = if cfg!(miri) { 15 } else { 1 };
        for at in 0..tables {
            for value in (0..=255_u8).step_by(step) {
                let mut changed = bytes.clone();
                changed[at] = value;
                if let Ok(image) = Image::parse(&changed) {
                    for segment in image.segments() {
                        let mut memory = [0_u8; 32];
                        if let Some(memory) = memory.get_mut(..segment.memsz as usize) {
                            let _ = image.restore(&segment, memory);
                            let _ = image.check(&segment, memory);
                        }
                    }
                }
            }
        }
        for len in 0..bytes.len() {
            assert!(Image::parse(&bytes[..len]).is_err(), "cut to {len}");
            let _ = Header::parse(&bytes[..len]);
        }
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert!(
            ImageError::Differs { vaddr: 0x1234 }
                .to_string()
                .contains("0x1234")
        );
        assert!(
            ImageError::ProgramMismatch
                .to_string()
                .contains("another program")
        );
    }
}
