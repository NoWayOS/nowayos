// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Arm PL011 UART: its register block and a safe, typed view of it.
//!
//! Written from the Arm PrimeCell UART (PL011) Technical Reference Manual,
//! revision r1p5 (Arm DDI 0183G): "Summary of registers" and the register
//! descriptions of UARTDR, UARTFR, UARTCR, UARTPeriphID0-3 and
//! UARTPCellID0-3; and, for which PL011 a board has, the board file's facts
//! (`platforms/BOARD.toml`, each with its source). No other source.
//!
//! # Which PL011, and how much of it
//!
//! A board's facts name its UART by its devicetree compatible string and
//! say how many bytes of its page are registers (`registers`; the whole
//! page if not given). The system description hands both to the driver
//! (`nowayos_region::device_facts`, from `device-NAME.bin`), and
//! [`Pl011::new`] takes them with the device. It drives two kinds
//! ([`Kind`]):
//!
//! * `arm,pl011` ([`COMPATIBLE`]): Arm's PrimeCell PL011, its whole 4 KiB
//!   block. Its identification registers, at 0xFE0 to 0xFFC, are read and
//!   checked before a view is handed out (QEMU's UART).
//! * `arm,pl011-axi` ([`COMPATIBLE_AXI`]): a PL011 on an AXI bus whose
//!   window ends before the identification registers - BCM2712's debug UART
//!   on the Raspberry Pi 5, whose devicetree gives it 0x200 bytes and names
//!   it compatible with `arm,pl011` too. There is nothing there to read, so
//!   the board's facts identify it, and the view never reads past UARTCR.
//!
//! Any other compatible string is refused, and so is a window too small for
//! the registers its kind's view reaches: a view never reads outside the
//! device's register window.
//!
//! # What the view reaches
//!
//! Only what a polled driver needs: the flags, the control register (read
//! only), the data register and, on a PrimeCell, the identification
//! registers once. Line settings, interrupt and DMA control are left as the
//! boot firmware set them and are not reachable through this view; in
//! particular UARTDMACR, which could make the UART take part in DMA, is
//! beyond the view's registers, and padding in a PrimeCell's block. The
//! UART's interrupt and its settings come when a driver needs them.

use core::mem::{offset_of, size_of};

use safe_mmio::fields::{ReadPure, ReadWrite};
use safe_mmio::{SharedMmioPointer, UniqueMmioPointer, field, field_shared};

use crate::device::RegisterBlock;
use crate::{Device, HwError};

/// The PL011's part number, from UARTPeriphID0-1.
pub const PART: u16 = 0x011;

/// Arm's designer code, from UARTPeriphID1-2.
pub const DESIGNER_ARM: u8 = 0x41;

/// UARTPCellID0-3: the same for every PrimeCell peripheral.
const PRIMECELL_ID: [u8; 4] = [0x0D, 0xF0, 0x05, 0xB1];

/// The devicetree compatible string of Arm's PrimeCell PL011: its whole
/// 4 KiB block, identification registers included.
pub const COMPATIBLE: &str = "arm,pl011";

/// The devicetree compatible string of a PL011 on an AXI bus whose window
/// holds no identification registers (BCM2712's, on the Raspberry Pi 5).
pub const COMPATIBLE_AXI: &str = "arm,pl011-axi";

/// The registers this view uses, 0x000 to UARTCR at 0x030, as the TRM's
/// "Summary of registers" lays them out: inside every PL011's window,
/// BCM2712's 0x200 bytes included. Registers the view does not use are
/// padding, never accessed.
#[repr(C)]
pub(crate) struct Registers {
    /// 0x000 UARTDR, the data register. A write queues a byte to send; a
    /// read takes the oldest received byte, with its error bits, off the
    /// receive FIFO - a side effect, so reading needs `&mut`.
    dr: ReadWrite<u32>,
    /// 0x004 UARTRSR/UARTECR, then 0x008-0x014 reserved.
    _reserved_004: [u32; 5],
    /// 0x018 UARTFR, the flag register. Reading has no side effect.
    fr: ReadPure<u32>,
    /// 0x01C reserved, then 0x020 UARTILPR, 0x024 UARTIBRD, 0x028 UARTFBRD
    /// and 0x02C UARTLCR_H: line settings, left as they are.
    _reserved_01c: [u32; 5],
    /// 0x030 UARTCR, the control register. Read only here; reading has no
    /// side effect.
    cr: ReadPure<u32>,
}

/// A PrimeCell PL011's whole 4 KiB block: the registers the view uses,
/// then the rest of the block, then the identification registers.
#[repr(C)]
pub(crate) struct PrimeCell {
    /// 0x000-0x033: the registers the view uses.
    registers: Registers,
    /// 0x034 UARTIFLS to 0x048 UARTDMACR (interrupt and DMA control), then
    /// 0x04C-0xFDC reserved, test and ID-expansion space.
    _reserved_034: [u32; 1003],
    /// 0xFE0-0xFEC UARTPeriphID0-3. Only bits 7:0 are defined.
    periph_id: [ReadPure<u32>; 4],
    /// 0xFF0-0xFFC UARTPCellID0-3. Only bits 7:0 are defined.
    pcell_id: [ReadPure<u32>; 4],
}

// Every offset the views use, as the TRM gives it: a layout mistake fails
// the build. The registers the view uses end where UARTIFLS begins, well
// inside an AXI PL011's 0x200 bytes.
const _: () = {
    assert!(offset_of!(Registers, dr) == 0x000);
    assert!(offset_of!(Registers, fr) == 0x018);
    assert!(offset_of!(Registers, cr) == 0x030);
    assert!(size_of::<Registers>() == 0x034);
    assert!(offset_of!(PrimeCell, registers) == 0x000);
    assert!(offset_of!(PrimeCell, periph_id) == 0xFE0);
    assert!(offset_of!(PrimeCell, pcell_id) == 0xFF0);
    assert!(size_of::<PrimeCell>() == 0x1000);
};

// SAFETY: `repr(C)`; every register at the offset the TRM gives (asserted
// above) and 32 bits wide, as the PL011's registers are accessed. The
// wrappers match the TRM: reading UARTFR and UARTCR has no side effect
// (`ReadPure`); UARTDR is `ReadWrite` because a read pops the receive FIFO,
// which is safe at any time - it changes no memory - and a write queues a
// byte. Everything else up to UARTCR is padding, never accessed.
unsafe impl RegisterBlock for Registers {}

// SAFETY: `repr(C)`; `Registers` at 0x000 (a register block, above), the
// identification registers at the offsets the TRM gives (asserted above),
// 32 bits wide, and reading them has no side effect (`ReadPure`). The
// interrupt and DMA control registers are padding, never accessed.
unsafe impl RegisterBlock for PrimeCell {}

/// Which PL011 a board's facts name: what may be read, and how it is
/// identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `arm,pl011`: the whole 4 KiB block; identified by its identification
    /// registers.
    PrimeCell,
    /// `arm,pl011-axi`: no identification registers in its window;
    /// identified by the board's facts.
    Axi,
}

impl Kind {
    /// The kind a devicetree compatible string names, or `None` for any
    /// other device.
    #[must_use]
    pub fn from_compatible(compatible: &[u8]) -> Option<Self> {
        if compatible == COMPATIBLE.as_bytes() {
            Some(Self::PrimeCell)
        } else if compatible == COMPATIBLE_AXI.as_bytes() {
            Some(Self::Axi)
        } else {
            None
        }
    }

    /// Its devicetree compatible string.
    #[must_use]
    pub const fn compatible(self) -> &'static str {
        match self {
            Self::PrimeCell => COMPATIBLE,
            Self::Axi => COMPATIBLE_AXI,
        }
    }

    /// The bytes of the window the view reads, from its start: the whole
    /// block for a PrimeCell, up to UARTCR for an AXI PL011.
    #[must_use]
    pub const fn reaches(self) -> usize {
        match self {
            Self::PrimeCell => size_of::<PrimeCell>(),
            Self::Axi => size_of::<Registers>(),
        }
    }
}

/// What a PrimeCell's identification registers say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// Part number: [`PART`] for a PL011.
    pub part: u16,
    /// Designer code: [`DESIGNER_ARM`] for Arm.
    pub designer: u8,
    /// Revision, from UARTPeriphID2 bits 7:4 (the TRM's table: 0 for r1p0,
    /// 1 for r1p1, 2 for r1p3 and r1p4, 3 for r1p5).
    pub revision: u8,
}

/// UARTFR, the flag register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u32);

impl Flags {
    /// The raw register value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// TXFE (bit 7): the transmit FIFO (or holding register) is empty.
    #[must_use]
    pub const fn tx_empty(self) -> bool {
        self.0 & (1 << 7) != 0
    }

    /// TXFF (bit 5): the transmit FIFO (or holding register) is full.
    #[must_use]
    pub const fn tx_full(self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// RXFE (bit 4): the receive FIFO (or holding register) is empty.
    #[must_use]
    pub const fn rx_empty(self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// BUSY (bit 3): the UART is still sending.
    #[must_use]
    pub const fn busy(self) -> bool {
        self.0 & (1 << 3) != 0
    }
}

/// UARTCR, the control register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control(u32);

impl Control {
    /// The raw register value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// UARTEN (bit 0): the UART is enabled.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.0 & 1 != 0
    }

    /// TXE (bit 8): the transmit section is enabled.
    #[must_use]
    pub const fn transmit_enabled(self) -> bool {
        self.0 & (1 << 8) != 0
    }

    /// RXE (bit 9): the receive section is enabled.
    #[must_use]
    pub const fn receive_enabled(self) -> bool {
        self.0 & (1 << 9) != 0
    }
}

/// A byte taken off the receive FIFO, with UARTDR's error bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// DATA (bits 7:0).
    pub byte: u8,
    /// OE, BE, PE and FE (bits 11:8): overrun, break, parity and framing
    /// errors, in bits 3 to 0. Zero for a clean byte.
    pub errors: u8,
}

/// The transmit FIFO is full: try again once a byte has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxFull;

/// A safe view of one PL011 UART's registers. Make one with
/// [`Pl011::new`].
#[derive(Debug)]
pub struct Pl011<'a> {
    regs: UniqueMmioPointer<'a, Registers>,
    kind: Kind,
    identity: Option<Identity>,
}

impl Pl011<'static> {
    /// Views `device` as the PL011 the board's facts name: `compatible`,
    /// its devicetree compatible string, and `registers`, the bytes of its
    /// window that are registers, from its start - both as
    /// `nowayos_region::device_facts` reads them. A PrimeCell's
    /// identification registers are checked before the view is handed out;
    /// an AXI PL011, which has none in its window, is identified by those
    /// facts alone. Nothing outside the first `registers` bytes is ever
    /// read.
    ///
    /// # Errors
    ///
    /// [`HwError::NotCompatible`] if `compatible` names no PL011 this view
    /// drives, and [`HwError::WindowTooSmall`] if `registers` is fewer
    /// bytes than its kind's view reads - both before any register is read;
    /// any error of viewing the device (the mapping is smaller than the
    /// view, say); [`HwError::WrongDevice`] if a PrimeCell's identification
    /// registers do not name an Arm PL011.
    pub fn new(device: Device, compatible: &[u8], registers: usize) -> Result<Self, HwError> {
        let kind = Kind::from_compatible(compatible)
            .ok_or(HwError::NotCompatible { expected: "PL011" })?;
        if registers < kind.reaches() {
            return Err(HwError::WindowTooSmall {
                window: registers,
                needed: kind.reaches(),
            });
        }
        match kind {
            Kind::PrimeCell => Self::checked(device.view()?),
            Kind::Axi => Ok(Self::from_facts(device.view()?)),
        }
    }
}

impl<'a> Pl011<'a> {
    /// A PrimeCell's view, once the identification registers in `block`
    /// name an Arm PL011.
    fn checked(mut block: UniqueMmioPointer<'a, PrimeCell>) -> Result<Self, HwError> {
        let periph = field_shared!(block, periph_id);
        let pcell = field_shared!(block, pcell_id);
        let [id0, id1, id2] = [id(&periph, 0), id(&periph, 1), id(&periph, 2)];
        let identity = Identity {
            part: u16::from(id0) | (u16::from(id1 & 0x0F) << 8),
            designer: (id1 >> 4) | ((id2 & 0x0F) << 4),
            revision: id2 >> 4,
        };
        let primecell = [0, 1, 2, 3].map(|index| id(&pcell, index));
        if primecell != PRIMECELL_ID || identity.part != PART || identity.designer != DESIGNER_ARM {
            return Err(HwError::WrongDevice {
                expected: "PL011",
                part: identity.part,
                designer: identity.designer,
            });
        }
        let registers = block.ptr_nonnull().cast::<Registers>();
        // SAFETY: `split_child` needs a properly aligned, valid pointer to
        // MMIO of a `Registers` within `block`'s, split off once, with
        // `block` never used afterwards. `Registers` is `PrimeCell`'s first
        // field, at offset 0 (asserted above), so `block`'s own pointer,
        // cast, points to it, aligned and inside the block; it is split off
        // here only, and `block` is dropped unused when this returns.
        let regs = unsafe { block.split_child(registers) };
        Ok(Self {
            regs,
            kind: Kind::PrimeCell,
            identity: Some(identity),
        })
    }

    /// An AXI PL011's view: identified by the board's facts, so nothing is
    /// read.
    const fn from_facts(regs: UniqueMmioPointer<'a, Registers>) -> Self {
        Self {
            regs,
            kind: Kind::Axi,
            identity: None,
        }
    }

    /// Which PL011 the board's facts name.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// What the identification registers say: `None` for a PL011 the
    /// board's facts identify ([`Kind::Axi`]), which has none to read.
    #[must_use]
    pub const fn identity(&self) -> Option<Identity> {
        self.identity
    }

    /// Reads UARTFR.
    #[must_use]
    pub fn flags(&self) -> Flags {
        Flags(field_shared!(self.regs, fr).read())
    }

    /// Reads UARTCR.
    #[must_use]
    pub fn control(&self) -> Control {
        Control(field_shared!(self.regs, cr).read())
    }

    /// Queues `byte` to send, unless the transmit FIFO is full.
    ///
    /// # Errors
    ///
    /// [`TxFull`] if TXFF is set; nothing is written.
    pub fn try_send(&mut self, byte: u8) -> Result<(), TxFull> {
        if self.flags().tx_full() {
            return Err(TxFull);
        }
        field!(self.regs, dr).write(u32::from(byte));
        Ok(())
    }

    /// Takes the oldest received byte off the receive FIFO, or `None` if it
    /// is empty.
    pub fn try_receive(&mut self) -> Option<Received> {
        if self.flags().rx_empty() {
            return None;
        }
        let word = field!(self.regs, dr).read();
        Some(Received {
            byte: u8::try_from(word & 0xFF).unwrap_or_default(),
            errors: u8::try_from((word >> 8) & 0x0F).unwrap_or_default(),
        })
    }
}

/// Bits 7:0 of ID register `index` in `registers` (0 if there is none).
fn id(registers: &SharedMmioPointer<'_, [ReadPure<u32>; 4]>, index: usize) -> u8 {
    registers.get(index).map_or(0, |register| {
        u8::try_from(register.read() & 0xFF).unwrap_or_default()
    })
}

#[cfg(all(test, not(miri)))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod tests {
    //! Ordinary memory stands in for the UART: the view reads and writes it
    //! through safe-mmio's real load and store instructions. Not under Miri,
    //! which cannot run them.

    use super::*;

    /// The registers the view uses, as a UART that is enabled, with both
    /// FIFOs empty, reads them.
    fn registers() -> Registers {
        Registers {
            dr: ReadWrite(0),
            _reserved_004: [0; 5],
            fr: ReadPure(1 << 7 | 1 << 4),
            _reserved_01c: [0; 5],
            cr: ReadPure(0x301),
        }
    }

    /// A PrimeCell block holding what a QEMU PL011 (revision r1p1) reads as.
    fn uart() -> PrimeCell {
        PrimeCell {
            registers: registers(),
            _reserved_034: [0; 1003],
            periph_id: [0x11, 0x10, 0x14, 0x00].map(ReadPure),
            pcell_id: PRIMECELL_ID.map(|byte| ReadPure(u32::from(byte))),
        }
    }

    /// A device over `memory`, as the system description would bind it,
    /// `size` bytes long.
    fn device_over<T>(memory: &mut T, size: usize) -> Device {
        Device::for_test(core::ptr::from_mut(memory).expose_provenance(), size)
    }

    #[test]
    fn a_pl011_identifies_itself() {
        let mut block = uart();
        let pl011 = Pl011::checked(UniqueMmioPointer::from(&mut block)).unwrap();
        assert_eq!(pl011.kind(), Kind::PrimeCell);
        assert_eq!(
            pl011.identity(),
            Some(Identity {
                part: PART,
                designer: DESIGNER_ARM,
                revision: 1
            })
        );
        let control = pl011.control();
        assert!(control.enabled() && control.transmit_enabled() && control.receive_enabled());
        assert!(pl011.flags().tx_empty() && pl011.flags().rx_empty());
        assert!(!pl011.flags().busy() && !pl011.flags().tx_full());
    }

    #[test]
    fn another_device_is_refused() {
        let cases: [(usize, bool, u32); 4] = [
            (0, true, 0x31), // part 0x031: a PL031, the RTC
            (1, true, 0x20), // designer 0x42
            (2, true, 0x15), // designer 0x51
            (0, false, 0x00),
        ];
        for (index, periph, value) in cases {
            let mut block = uart();
            if periph {
                block.periph_id[index] = ReadPure(value);
            } else {
                block.pcell_id[index] = ReadPure(value);
            }
            let error = Pl011::checked(UniqueMmioPointer::from(&mut block)).unwrap_err();
            assert!(
                matches!(
                    error,
                    HwError::WrongDevice {
                        expected: "PL011",
                        ..
                    }
                ),
                "{error}"
            );
        }
    }

    #[test]
    fn the_board_facts_choose_the_view() {
        // QEMU's: a PrimeCell, its whole page registers - identified by its
        // identification registers.
        let mut block = uart();
        let pl011 = Pl011::new(device_over(&mut block, 0x1000), b"arm,pl011", 0x1000).unwrap();
        assert_eq!(pl011.kind(), Kind::PrimeCell);
        assert_eq!(pl011.identity().map(|identity| identity.part), Some(PART));

        // The same facts over a block whose identification registers name
        // another part are refused: the check stays where the registers are.
        let mut block = uart();
        block.periph_id[0] = ReadPure(0x31);
        assert!(matches!(
            Pl011::new(device_over(&mut block, 0x1000), b"arm,pl011", 0x1000),
            Err(HwError::WrongDevice { part: 0x031, .. })
        ));

        // The Pi 5's: an AXI PL011, 0x200 bytes of registers. Its stand-in
        // is only the registers the view uses - nothing beyond UARTCR
        // exists to be read - and the device is no longer than that, so a
        // view that reached the identification registers could not be made.
        let mut registers = registers();
        let size = size_of::<Registers>();
        {
            let mut pl011 =
                Pl011::new(device_over(&mut registers, size), b"arm,pl011-axi", 0x200).unwrap();
            assert_eq!((pl011.kind(), pl011.identity()), (Kind::Axi, None));
            assert!(pl011.control().transmit_enabled());
            assert_eq!(pl011.try_send(b'P'), Ok(()));
        }
        assert_eq!(registers.dr.0, u32::from(b'P'));
        assert_eq!(Kind::Axi.reaches(), 0x34);
        assert_eq!(Kind::Axi.compatible(), "arm,pl011-axi");
        assert_eq!(Kind::PrimeCell.compatible(), "arm,pl011");
    }

    #[test]
    fn an_unknown_device_is_refused_before_anything_is_read() {
        // A device that cannot be read at all - its address is 4, which no
        // register block is ever mapped at - so any read would fault: each
        // refusal comes before one.
        for compatible in [
            &b"arm,pl031"[..],
            b"brcm,bcm2835-aux-uart",
            b"arm,pl011 ",
            b"ARM,PL011",
            b"arm,pl011-axi\0",
            b"",
        ] {
            let error = Pl011::new(Device::for_test(4, 0x1000), compatible, 0x1000).unwrap_err();
            assert_eq!(error, HwError::NotCompatible { expected: "PL011" });
            assert!(
                error.to_string().contains("not one a PL011 view drives"),
                "{error}"
            );
        }
        // A PrimeCell whose window ends before its identification
        // registers, and an AXI PL011 too small for UARTCR: refused, not
        // read.
        for (compatible, window, needed) in [
            (&b"arm,pl011"[..], 0x200, 0x1000),
            (b"arm,pl011", 0xFFC, 0x1000),
            (b"arm,pl011-axi", 0x30, 0x34),
            (b"arm,pl011-axi", 0, 0x34),
        ] {
            assert_eq!(
                Pl011::new(Device::for_test(4, 0x1000), compatible, window).unwrap_err(),
                HwError::WindowTooSmall { window, needed }
            );
        }
        assert!(
            HwError::WindowTooSmall {
                window: 0x200,
                needed: 0x1000
            }
            .to_string()
            .contains("0x200 bytes of registers")
        );
    }

    /// A view of `block`, for one step of a test.
    fn view(block: &mut PrimeCell) -> Pl011<'_> {
        Pl011::checked(UniqueMmioPointer::from(block)).unwrap()
    }

    #[test]
    fn a_byte_is_sent_only_when_the_fifo_has_room() {
        let mut block = uart();
        assert_eq!(view(&mut block).try_send(b'B'), Ok(()));
        assert_eq!(block.registers.dr.0, u32::from(b'B'));

        block.registers.fr = ReadPure(1 << 5);
        assert_eq!(view(&mut block).try_send(b'C'), Err(TxFull));
        assert_eq!(block.registers.dr.0, u32::from(b'B'));
    }

    #[test]
    fn a_received_byte_comes_with_its_error_bits() {
        let mut block = uart();
        assert_eq!(view(&mut block).try_receive(), None);

        block.registers.fr = ReadPure(0);
        block.registers.dr = ReadWrite(0b1001 << 8 | u32::from(b'x'));
        assert_eq!(
            view(&mut block).try_receive(),
            Some(Received {
                byte: b'x',
                errors: 0b1001
            })
        );
    }
}
