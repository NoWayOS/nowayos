// SPDX-License-Identifier: MIT OR Apache-2.0
//! A device's facts, as the board file gives them: the configuration a
//! driver reads to know which device it holds and how much of its page is
//! registers.
//!
//! A board's facts (`platforms/BOARD.toml`) name each device by its
//! devicetree compatible string and say how many bytes of its window, from
//! its start, are registers - fewer than the page when the device's
//! registers end early, as BCM2712's debug UART's do at 0x200. A system
//! description gives a driver its device's facts in a memory region
//! prefilled from `device-NAME.bin`, which `tools/board-system` writes from
//! the board file when the image is built, and maps it read-only into the
//! PD that holds the device (`tools/check-system` rule 12 refuses it
//! anywhere else). The record, in 32-bit little-endian words:
//!
//! | Word | Holds |
//! | --- | --- |
//! | 0 | the record's version, [`FACTS_VERSION`] |
//! | 1 | the bytes of the window that are registers, from its start |
//! | 2 | the compatible string's length in bytes, 1 to [`COMPATIBLE_MAX`] |
//! | 3 on | the compatible string, printable ASCII, zero-padded to a word |
//!
//! A framebuffer the firmware set up has a record of version
//! [`FACTS_VERSION_FRAMEBUFFER`]: the same, with the compatible string
//! zero-padded to [`COMPATIBLE_MAX`] bytes, then its mode in five more
//! words - width, height, stride, format (1: `r5g6b5`, 2: `x8r8g8b8`) and the screen row
//! its window starts at - and its "registers" are the window's bytes of
//! pixels, rows `first_row` to `height - 1` ([`FramebufferMode`]).
//!
//! A device on a bus has a record of version [`FACTS_VERSION_BUS`]:
//! the same, with the compatible string zero-padded to [`COMPATIBLE_MAX`]
//! bytes, then two 64-bit bus addresses, each low word first - where the
//! driver places the device's window, BAR 0, or 0 for none ([`pci_bar0`]),
//! and where the device sees physical address 0 ([`dma_offset`]). QEMU's
//! `virt` machine has no firmware to assign BARs, so the xHCI PD places its
//! controller's BAR 0 through the function's configuration page at the bus
//! address its board file chose; RP1, on the Raspberry Pi 5, sees DRAM at
//! physical address plus 0x10_0000_0000 through BCM2712's PCIe controller.
//! Any other record has neither (`pci_bar0` is `None`, `dma_offset` 0).
//!
//! [`pci_bar0`]: DeviceFacts::pci_bar0
//! [`dma_offset`]: DeviceFacts::dma_offset
//!
//! It is configuration the image builder wrote, in a region no PD can
//! write, but it is still copied out whole and checked before it is used:
//! an unknown version, a length out of range or a character that is not
//! printable is a typed error, never a guess.

use crate::RegionError;
use crate::mapping::Access;
use crate::words::Words;

/// The read-only region that holds the facts of the device a driver PD
/// holds, by NoWayOS's convention: prefilled from `device-NAME.bin`, NAME
/// the board's name for the device (`debug_uart`).
pub const DEVICE_FACTS_REGION: usize = 5;

/// The longest compatible string the facts hold, in bytes.
pub const COMPATIBLE_MAX: usize = 64;

/// The version of the facts' record this crate reads, as
/// `tools/board-system` writes it.
pub const FACTS_VERSION: u32 = 1;

/// The version of the record of a framebuffer's facts, with its mode.
pub const FACTS_VERSION_FRAMEBUFFER: u32 = 2;

/// The version of the record of a device on a bus, with its bus addresses.
pub const FACTS_VERSION_BUS: u32 = 3;

/// The words before the compatible string: version, registers, length.
const HEADER_WORDS: usize = 3;

/// The words of a framebuffer's mode, after the compatible string's.
const MODE_WORDS: usize = 5;

/// The words of a device's bus addresses, after the compatible string's.
const BUS_WORDS: usize = 4;

/// The record's words, the longest compatible string's and the longer of a
/// mode's and the bus addresses included.
const RECORD_WORDS: usize = HEADER_WORDS
    + COMPATIBLE_MAX / 4
    + if MODE_WORDS > BUS_WORDS {
        MODE_WORDS
    } else {
        BUS_WORDS
    };

/// A framebuffer's pixel format, as the facts carry it: the devicetree
/// simple-framebuffer binding's name for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferFormat {
    /// `r5g6b5`: a 16-bit little-endian pixel, red in bits 15 to 11, green
    /// in 10 to 5, blue in 4 to 0. Code 1.
    R5g6b5,
    /// `x8r8g8b8`: a 32-bit little-endian pixel, red in bits 23 to 16,
    /// green in 15 to 8, blue in 7 to 0, the top byte unused. Code 2.
    X8r8g8b8,
}

impl FramebufferFormat {
    /// Bytes a pixel.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::R5g6b5 => 2,
            Self::X8r8g8b8 => 4,
        }
    }

    /// The devicetree simple-framebuffer binding's name for it, as board
    /// files and the console give it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::R5g6b5 => "r5g6b5",
            Self::X8r8g8b8 => "x8r8g8b8",
        }
    }
}

/// The mode of a framebuffer the firmware set up and scans out: the
/// screen's width and height in pixels, the bytes from one row to the
/// next, the pixel format, and the screen row its window starts at - a
/// window starts on a page boundary and may leave out the screen's first
/// rows. Byte 0 of the window is the first pixel of row
/// [`first_row`](Self::first_row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferMode {
    width: u32,
    height: u32,
    stride: u32,
    format: FramebufferFormat,
    first_row: u32,
}

impl FramebufferMode {
    /// A mode with these values, if they keep the rules the facts are
    /// checked by: some pixels, a stride that holds a row, and a first row
    /// on the screen. (`device_facts` also checks the window holds exactly
    /// the rows from the first row.)
    #[must_use]
    pub fn new(
        width: u32,
        height: u32,
        stride: u32,
        format: FramebufferFormat,
        first_row: u32,
    ) -> Option<Self> {
        Self::checked(width, height, stride, format, first_row).ok()
    }

    /// As [`new`](Self::new), saying which rule is broken.
    fn checked(
        width: u32,
        height: u32,
        stride: u32,
        format: FramebufferFormat,
        first_row: u32,
    ) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("a framebuffer with no pixels");
        }
        if width
            .checked_mul(format.bytes_per_pixel())
            .is_none_or(|row| stride < row)
        {
            return Err("a framebuffer's stride shorter than its row");
        }
        if first_row >= height {
            return Err("a framebuffer whose window starts below its last row");
        }
        Ok(Self {
            width,
            height,
            stride,
            format,
            first_row,
        })
    }

    /// The screen's width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The screen's height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes from the start of one row to the start of the next.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// The pixel format.
    #[must_use]
    pub const fn format(&self) -> FramebufferFormat {
        self.format
    }

    /// The screen row the window starts at: rows above it are not the
    /// PD's to draw.
    #[must_use]
    pub const fn first_row(&self) -> u32 {
        self.first_row
    }
}

/// A device's facts from the board file: its devicetree compatible string
/// and how many bytes of its window are registers. Read them with
/// [`device_facts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFacts {
    compatible: [u8; COMPATIBLE_MAX],
    len: usize,
    registers: usize,
    framebuffer: Option<FramebufferMode>,
    pci_bar0: Option<u64>,
    dma_offset: u64,
}

impl DeviceFacts {
    /// The device's devicetree compatible string, such as `arm,pl011-axi`:
    /// printable ASCII, 1 to [`COMPATIBLE_MAX`] bytes.
    #[must_use]
    pub fn compatible(&self) -> &[u8] {
        self.compatible.get(..self.len).unwrap_or_default()
    }

    /// The bytes of the device's window that are registers, from its
    /// start: a driver reads no further.
    #[must_use]
    pub const fn registers(&self) -> usize {
        self.registers
    }

    /// A framebuffer's mode, when the device is one the firmware set up;
    /// its [`registers`](Self::registers) are then the window's bytes of
    /// pixels.
    #[must_use]
    pub const fn framebuffer(&self) -> Option<FramebufferMode> {
        self.framebuffer
    }

    /// For a PCI function the driver places itself, the bus address its
    /// window - BAR 0 - goes at (a [`FACTS_VERSION_BUS`] record); `None`
    /// when the board's firmware placed it, or it is no PCI function.
    #[must_use]
    pub const fn pci_bar0(&self) -> Option<u64> {
        self.pci_bar0
    }

    /// Where the device sees memory: the bus address of physical address
    /// 0, which a driver adds to every physical address it hands the
    /// device. 0 when the device sees memory as the CPU does.
    #[must_use]
    pub const fn dma_offset(&self) -> u64 {
        self.dma_offset
    }
}

/// The facts of the device this PD holds, as the system description gives
/// them in read-only region [`DEVICE_FACTS_REGION`].
///
/// # Errors
///
/// As for [`read_only`](crate::read_only) - [`RegionError::NotMapped`] when
/// the system gives no facts; [`RegionError::TooSmall`] if the region is
/// smaller than a page; [`RegionError::NoDeviceFacts`] if it holds no
/// record (version 0, as in a region nothing was written to);
/// [`RegionError::BadDeviceFacts`] if the record is of another version or
/// breaks its rules. `make image` writes none of those.
pub fn device_facts() -> Result<DeviceFacts, RegionError> {
    facts_in(&crate::read_only(DEVICE_FACTS_REGION)?.into_words()?)
}

/// The facts held in `words`.
pub(crate) fn facts_in<A: Access>(words: &Words<A>) -> Result<DeviceFacts, RegionError> {
    // Copy the whole record out first, then check the copy.
    let mut record = [0u32; RECORD_WORDS];
    for (index, word) in record.iter_mut().enumerate() {
        *word = words.load(index).ok_or(RegionError::TooSmall {
            size: 0,
            needed: RECORD_WORDS.saturating_mul(4),
        })?;
    }
    let [version, registers, len, rest @ ..] = record;
    let (string, extra) = rest.split_at(COMPATIBLE_MAX / 4);
    let (framebuffer, (pci_bar0, dma_offset)) = match version {
        0 => return Err(RegionError::NoDeviceFacts),
        FACTS_VERSION => (None, (None, 0)),
        FACTS_VERSION_FRAMEBUFFER => (
            Some(mode_in(
                extra.get(..MODE_WORDS).unwrap_or_default(),
                registers,
            )?),
            (None, 0),
        ),
        FACTS_VERSION_BUS => (None, bus_in(extra)),
        _ => {
            return Err(RegionError::BadDeviceFacts {
                reason: "a version this PD does not read",
            });
        }
    };
    let registers = usize::try_from(registers).unwrap_or(0);
    if registers == 0 {
        return Err(RegionError::BadDeviceFacts {
            reason: "no bytes of registers",
        });
    }
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if !(1..=COMPATIBLE_MAX).contains(&len) {
        return Err(RegionError::BadDeviceFacts {
            reason: "a compatible string of no bytes, or of more than 64",
        });
    }
    let mut compatible = [0u8; COMPATIBLE_MAX];
    for (bytes, word) in compatible.chunks_exact_mut(4).zip(string) {
        bytes.copy_from_slice(&word.to_ne_bytes());
    }
    let printable = compatible
        .get(..len)
        .is_some_and(|text| text.iter().all(u8::is_ascii_graphic));
    if !printable {
        return Err(RegionError::BadDeviceFacts {
            reason: "a compatible string that is not printable ASCII",
        });
    }
    Ok(DeviceFacts {
        compatible,
        len,
        registers,
        framebuffer,
        pci_bar0,
        dma_offset,
    })
}

/// A device's bus addresses from the words after its compatible string:
/// where its window goes (0: none) and its DMA offset, each low word first.
fn bus_in(words: &[u32]) -> (Option<u64>, u64) {
    let word = |index: usize| u64::from(words.get(index).copied().unwrap_or(0));
    let bar = (word(1) << 32) | word(0);
    let dma = (word(3) << 32) | word(2);
    ((bar != 0).then_some(bar), dma)
}

/// A framebuffer's mode from its five words, checked against its window's
/// `registers` bytes of pixels.
fn mode_in(words: &[u32], registers: u32) -> Result<FramebufferMode, RegionError> {
    let bad = |reason| Err(RegionError::BadDeviceFacts { reason });
    let [width, height, stride, format, first_row] = *words else {
        return bad("a framebuffer's mode cut short");
    };
    let format = match format {
        1 => FramebufferFormat::R5g6b5,
        2 => FramebufferFormat::X8r8g8b8,
        _ => return bad("a framebuffer format this PD does not read"),
    };
    let mode = match FramebufferMode::checked(width, height, stride, format, first_row) {
        Ok(mode) => mode,
        Err(reason) => return bad(reason),
    };
    let rows = height.saturating_sub(first_row);
    if rows.checked_mul(stride) != Some(registers) {
        return bad("a framebuffer whose window is not its rows from its first row");
    }
    Ok(mode)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    extern crate std;

    use std::string::ToString;
    use std::vec::Vec;

    use super::*;
    use crate::mapping::testing::{PAGE, TestPage, patch};
    use crate::mapping::{ReadOnly, ReadWrite, Region};

    /// The record `tools/board-system` writes for a device with
    /// `registers` bytes of registers and compatible string `compatible`.
    fn record(version: u32, registers: u32, compatible: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(&registers.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(compatible.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(compatible);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        bytes
    }

    /// `bytes` at the start of `page`, the rest zero, as a prefill leaves
    /// it - as far as a record reaches, which is all `facts_in` reads (a
    /// fresh page is zero beyond). Writing the whole page each time made
    /// these tests the slowest under Miri.
    fn fill(page: &'static TestPage, bytes: &[u8]) {
        let words = Region::<ReadWrite>::test_page(page).into_words().unwrap();
        for index in 0..RECORD_WORDS.max(bytes.len().div_ceil(4)) {
            let mut word = [0u8; 4];
            for (offset, byte) in word.iter_mut().enumerate() {
                *byte = bytes.get(index * 4 + offset).copied().unwrap_or(0);
            }
            assert!(words.store(index, u32::from_ne_bytes(word)));
        }
    }

    fn read(page: &'static TestPage) -> Result<DeviceFacts, RegionError> {
        facts_in(&Region::<ReadOnly>::test_page(page).into_words().unwrap())
    }

    #[test]
    fn each_boards_debug_uart_reads_as_written() {
        let page = TestPage::fresh();
        for (registers, compatible) in [
            (0x1000, &b"arm,pl011"[..]),
            (0x200, b"arm,pl011-axi"),
            (4, b"x"),
            (u32::MAX, &[b'a'; COMPATIBLE_MAX][..]),
        ] {
            fill(page, &record(FACTS_VERSION, registers, compatible));
            let facts = read(page).unwrap();
            assert_eq!(facts.compatible(), compatible);
            assert_eq!(facts.registers(), usize::try_from(registers).unwrap());
            assert_eq!((facts.pci_bar0(), facts.dma_offset()), (None, 0));
        }
    }

    /// A version 3 record: the same, then the compatible string padded to
    /// its 64 bytes, the BAR's bus address and the DMA offset.
    fn bus_record(registers: u32, compatible: &[u8], bar: u64, dma: u64) -> Vec<u8> {
        let mut bytes = record(FACTS_VERSION_BUS, registers, compatible);
        bytes.resize(12 + COMPATIBLE_MAX, 0);
        bytes.extend_from_slice(&bar.to_le_bytes());
        bytes.extend_from_slice(&dma.to_le_bytes());
        bytes
    }

    #[test]
    fn a_device_on_a_bus_reads_with_its_addresses() {
        let page = TestPage::fresh();
        // QEMU's xHCI: placed by its driver, memory seen as the CPU sees it.
        fill(page, &bus_record(0x4000, b"pci1b36,d", 0x1000_4000, 0));
        let facts = read(page).unwrap();
        assert_eq!(facts.compatible(), b"pci1b36,d");
        assert_eq!(facts.registers(), 0x4000);
        assert_eq!(facts.framebuffer(), None);
        assert_eq!(
            (facts.pci_bar0(), facts.dma_offset()),
            (Some(0x1000_4000), 0)
        );
        // RP1's: placed by the firmware, memory 0x10_0000_0000 on.
        fill(page, &bus_record(0x1000, b"snps,dwc3", 0, 0x10_0000_0000));
        let facts = read(page).unwrap();
        assert_eq!(
            (facts.pci_bar0(), facts.dma_offset()),
            (None, 0x10_0000_0000)
        );
        fill(
            page,
            &bus_record(0x4000, &[b'p'; COMPATIBLE_MAX], 0x80_0000_0000, u64::MAX),
        );
        let facts = read(page).unwrap();
        assert_eq!(
            (facts.pci_bar0(), facts.dma_offset()),
            (Some(0x80_0000_0000), u64::MAX)
        );
        // A framebuffer's record has no bus addresses.
        fill(
            page,
            &framebuffer_record(0x3E_5800, [1920, 1080, 3840, 1, 16]),
        );
        let facts = read(page).unwrap();
        assert_eq!((facts.pci_bar0(), facts.dma_offset()), (None, 0));
    }

    #[test]
    fn a_record_that_breaks_its_rules_is_refused() {
        let page = TestPage::fresh();
        let bad = |reason: &'static str| Err(RegionError::BadDeviceFacts { reason });
        // Nothing written: a typed error, so a driver refuses its device.
        fill(page, &[]);
        assert_eq!(read(page), Err(RegionError::NoDeviceFacts));
        for version in [4, u32::MAX] {
            fill(page, &record(version, 0x200, b"arm,pl011-axi"));
            assert_eq!(read(page), bad("a version this PD does not read"));
        }
        fill(page, &record(FACTS_VERSION, 0, b"arm,pl011"));
        assert_eq!(read(page), bad("no bytes of registers"));
        fill(page, &record(FACTS_VERSION, 0x200, b""));
        assert_eq!(
            read(page),
            bad("a compatible string of no bytes, or of more than 64")
        );
        // One byte more than the record holds: the length is refused before
        // anything is copied by it.
        fill(
            page,
            &record(FACTS_VERSION, 0x200, &[b'a'; COMPATIBLE_MAX + 1]),
        );
        assert_eq!(
            read(page),
            bad("a compatible string of no bytes, or of more than 64")
        );
        let mut huge = record(FACTS_VERSION, 0x200, b"arm,pl011");
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        fill(page, &huge);
        assert_eq!(
            read(page),
            bad("a compatible string of no bytes, or of more than 64")
        );
        for compatible in [&b"arm pl011"[..], b"arm,pl011\0", b"arm,\x7f", b"\xc3\xa9"] {
            fill(page, &record(FACTS_VERSION, 0x200, compatible));
            assert_eq!(
                read(page),
                bad("a compatible string that is not printable ASCII")
            );
        }
        assert!(
            RegionError::NoDeviceFacts
                .to_string()
                .contains("device-NAME.bin")
        );
    }

    /// The record `tools/board-system` writes for a framebuffer: version 2,
    /// the compatible string padded to 64 bytes, then the mode.
    fn framebuffer_record(registers: u32, mode: [u32; 5]) -> Vec<u8> {
        let mut bytes = record(FACTS_VERSION_FRAMEBUFFER, registers, b"simple-framebuffer");
        bytes.resize(12 + COMPATIBLE_MAX, 0);
        for word in mode {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn the_pi_5s_firmware_framebuffer_reads_as_written() {
        // platforms/rpi5b_2gb.toml's firmware_fb, as tools/board-system
        // writes it (tools/test_tools.py checks those bytes).
        let page = TestPage::fresh();
        fill(
            page,
            &framebuffer_record(0x3F_4800, [1920, 1080, 3840, 1, 0]),
        );
        let facts = read(page).unwrap();
        assert_eq!(facts.compatible(), b"simple-framebuffer");
        assert_eq!(facts.registers(), 0x3F_4800);
        let mode = facts.framebuffer().unwrap();
        assert_eq!(
            (
                mode.width(),
                mode.height(),
                mode.stride(),
                mode.format(),
                mode.first_row()
            ),
            (1920, 1080, 3840, FramebufferFormat::R5g6b5, 0)
        );
        assert_eq!(mode.format().bytes_per_pixel(), 2);
        assert_eq!(mode.format().name(), "r5g6b5");
        // The same screen at 32 bits a pixel.
        fill(
            page,
            &framebuffer_record(0x7E_9000, [1920, 1080, 7680, 2, 0]),
        );
        let mode = read(page).unwrap().framebuffer().unwrap();
        assert_eq!(
            (
                mode.stride(),
                mode.format(),
                mode.format().bytes_per_pixel()
            ),
            (7680, FramebufferFormat::X8r8g8b8, 4)
        );
        assert_eq!(mode.format().name(), "x8r8g8b8");
        // A version 1 record has no mode, whatever follows it.
        let mut uart = record(FACTS_VERSION, 0x200, b"arm,pl011-axi");
        uart.resize(12 + COMPATIBLE_MAX, 0);
        uart.extend_from_slice(&[1; 20]);
        fill(page, &uart);
        assert_eq!(read(page).unwrap().framebuffer(), None);
    }

    #[test]
    fn a_framebuffer_mode_that_breaks_its_rules_is_refused() {
        let page = TestPage::fresh();
        let bad = |reason: &'static str| Err(RegionError::BadDeviceFacts { reason });
        for (registers, mode, reason) in [
            (
                3840,
                [1920, 1, 3840, 3, 0],
                "a framebuffer format this PD does not read",
            ),
            (
                3840,
                [1920, 1, 3840, 2, 0],
                "a framebuffer's stride shorter than its row",
            ),
            (3840, [0, 1, 3840, 1, 0], "a framebuffer with no pixels"),
            (3840, [1920, 0, 3840, 1, 0], "a framebuffer with no pixels"),
            (
                3838,
                [1920, 1, 3838, 1, 0],
                "a framebuffer's stride shorter than its row",
            ),
            (
                4,
                [u32::MAX, 1, 4, 1, 0],
                "a framebuffer's stride shorter than its row",
            ),
            (
                3840,
                [1920, 2, 3840, 1, 2],
                "a framebuffer whose window starts below its last row",
            ),
            (
                3840,
                [1920, 2, 3840, 1, 0],
                "a framebuffer whose window is not its rows from its first row",
            ),
            (
                0x3E_5800,
                [1920, 1080, 3840, 1, 15],
                "a framebuffer whose window is not its rows from its first row",
            ),
            (
                0x1_0000,
                [1, u32::MAX, 0x1_0000, 1, 0],
                "a framebuffer whose window is not its rows from its first row",
            ),
        ] {
            fill(page, &framebuffer_record(registers, mode));
            assert_eq!(read(page), bad(reason), "{mode:?}");
        }
    }

    #[test]
    fn the_facts_region_is_bound_by_its_symbols() {
        // Read-only region 5's symbols are this test's alone.
        assert_eq!(
            device_facts().err(),
            Some(RegionError::NotMapped {
                symbol: "nowayos_ro_region_5_vaddr"
            })
        );
        let page = TestPage::fresh();
        fill(page, &record(FACTS_VERSION, 0x200, b"arm,pl011-axi"));
        patch::<ReadOnly>(DEVICE_FACTS_REGION, Some(page), PAGE);
        let facts = device_facts().unwrap();
        assert_eq!(
            (facts.compatible(), facts.registers()),
            (&b"arm,pl011-axi"[..], 0x200)
        );
    }
}
