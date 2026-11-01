// SPDX-License-Identifier: MIT OR Apache-2.0
//! The error type. Every way binding or viewing a device can fail has a
//! variant here; nothing in this crate panics instead.

use core::fmt;

/// Why a device could not be bound or viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HwError {
    /// There is no device with this number: the index is not below
    /// [`DEVICES`](crate::DEVICES).
    NoSuchDevice {
        /// The index asked for.
        index: usize,
    },
    /// The Microkit tool did not patch the device's address: the system
    /// description has no `<map>` with `setvar_vaddr` set to `symbol` for
    /// this PD.
    NotMapped {
        /// The address symbol that is still zero.
        symbol: &'static str,
    },
    /// The device's address was patched but its size was not: the `<map>`
    /// has no `setvar_size`.
    SizeUnknown {
        /// The size symbol that is still zero.
        symbol: &'static str,
    },
    /// The device was already bound in this PD. Each device is bound once,
    /// so there is only ever one view of its registers.
    AlreadyTaken {
        /// The device's address symbol.
        symbol: &'static str,
    },
    /// The device's address is not aligned for its register block.
    Misaligned {
        /// The device's virtual address.
        vaddr: usize,
        /// The alignment the register block needs.
        align: usize,
    },
    /// The mapping is smaller than the device's register block.
    TooSmall {
        /// The mapping's size in bytes.
        size: usize,
        /// The bytes the register block needs.
        needed: usize,
    },
    /// The register block would run past the end of the address space.
    OutOfRange {
        /// The device's virtual address.
        vaddr: usize,
    },
    /// A DMA region's physical address was not patched: the system
    /// description has no `<setvar>` with `region_paddr` for `symbol` in this
    /// PD.
    PhysicalAddressUnknown {
        /// The physical-address symbol that is still unpatched.
        symbol: &'static str,
    },
    /// A span of a DMA region asked for does not lie inside it. Nothing was
    /// read, written or handed to a device.
    OutsideRegion {
        /// Where the span starts in the region.
        offset: usize,
        /// The span's length in bytes.
        len: usize,
        /// The region's size in bytes.
        size: usize,
    },
    /// The device does not read as the one the view is for: the system
    /// description bound another device to this symbol.
    NoSignature {
        /// The device the view is for.
        expected: &'static str,
        /// What its identifying register read as.
        read: u64,
    },
    /// The device reported an error for a transfer, or did not finish it.
    TransferFailed {
        /// The device.
        device: &'static str,
        /// The status it left.
        status: u32,
    },
    /// The identification registers do not name the device the view is for:
    /// the system description bound another device to this symbol.
    WrongDevice {
        /// The device the view is for, such as `"PL011"`.
        expected: &'static str,
        /// The part number read from the identification registers.
        part: u16,
        /// The designer code read from the identification registers.
        designer: u8,
    },
    /// The board's facts name a device this view does not drive: its
    /// devicetree compatible string is none the view knows. Nothing was
    /// read.
    NotCompatible {
        /// The device the view is for, such as `"PL011"`.
        expected: &'static str,
    },
    /// The board's facts give the device fewer bytes of registers than the
    /// view of its kind reads: the view would read outside the device's
    /// register window. Nothing was read.
    WindowTooSmall {
        /// The bytes of the device's window that are registers, from the
        /// board's facts.
        window: usize,
        /// The bytes the view reads, from the window's start.
        needed: usize,
    },
    /// No virtio-mmio transport in the page this PD was granted holds a
    /// device with this ID.
    NoVirtioDevice {
        /// The device ID looked for.
        id: u32,
    },
    /// The virtio device is there, but only through the legacy interface
    /// (register version 1): only virtio 1.x (version 2) is supported.
    VirtioVersion {
        /// The register version read.
        found: u32,
    },
    /// The virtio device does not offer `VIRTIO_F_VERSION_1`, or did not
    /// keep FEATURES_OK: it refused the features. FAILED is set.
    FeaturesRefused {
        /// The features the device offered.
        offered: u64,
        /// The features the driver accepted.
        accepted: u64,
    },
    /// The virtio device asked for a reset (DEVICE_NEEDS_RESET). FAILED is
    /// set.
    DeviceFailed {
        /// The status the device reported.
        status: u32,
    },
    /// The virtio device's queue is smaller than the driver's, does not
    /// exist (`max` 0), or is already set up.
    QueueTooBig {
        /// The queue's index.
        index: u32,
        /// The entries the driver's queue has.
        wanted: u32,
        /// The most the device takes.
        max: u32,
    },
    /// An I/O region's physical address is not page-aligned, the size the
    /// PD asked for is 0, or the region would run past the end of the
    /// physical address space.
    IoRegionInvalid {
        /// The region's physical-address symbol.
        symbol: &'static str,
        /// The physical address patched into it.
        paddr: usize,
        /// The size the PD asked for.
        size: usize,
    },
    /// The virtio device's configuration changed every time it was read
    /// (its generation counter kept moving).
    ConfigUnstable,
    /// A frame's length is outside what the device is given: from an
    /// Ethernet header to the largest frame.
    FrameLength {
        /// The length asked for.
        len: usize,
    },
    /// A PCI function's configuration space is not what its view drives,
    /// or its BAR did not take the address given.
    PciFunction {
        /// What was wrong.
        reason: &'static str,
    },
    /// An xHCI controller's capability registers, or the window they are
    /// read through, were refused.
    XhciCapabilities(nowayos_usb::caps::CapsError),
    /// An xHCI controller, or the DMA region given it, is not what this
    /// driver can use.
    Xhci {
        /// What was wrong.
        reason: &'static str,
    },
    /// An xHCI controller did not finish `what` in time.
    XhciTimeout {
        /// What it was doing.
        what: &'static str,
    },
    /// A GEM MAC, the memory given it, or RP1's GPIO for its PHY's reset,
    /// is not what this driver can use.
    Gem {
        /// What was wrong.
        reason: &'static str,
    },
    /// The PM watchdog's window, or a timeout asked of it, is not what its
    /// view can use. Nothing was written.
    Watchdog {
        /// What was wrong.
        reason: &'static str,
    },
    /// The Pi 5's fan - RP1's PWM1, its clock or GPIO45 - is not what its
    /// view can use.
    Fan {
        /// What was wrong.
        reason: &'static str,
    },
    /// The VideoCore mailbox, or the firmware's answer through it, is not
    /// what its view can use.
    Mailbox {
        /// What was wrong.
        reason: &'static str,
    },
    /// The display's scan position cannot be read.
    Scan {
        /// What was wrong.
        reason: &'static str,
    },
    /// A GEM MAC did not finish `what` in time.
    GemTimeout {
        /// What it was doing.
        what: &'static str,
    },
    /// A GEM MAC's descriptor ring refused the driver's operation.
    GemRing(nowayos_gem::ring::RingError),
    /// A PHY management frame was refused.
    Mdio(nowayos_gem::mdio::MdioError),
}

impl fmt::Display for HwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchDevice { index } => write!(
                f,
                "no device {index}: indices run from 0 to {}",
                crate::DEVICES.saturating_sub(1)
            ),
            Self::NotMapped { symbol } => write!(
                f,
                "{symbol} is not patched: no <map> in the system description binds it"
            ),
            Self::SizeUnknown { symbol } => {
                write!(f, "{symbol} is not patched: the <map> needs setvar_size")
            }
            Self::AlreadyTaken { symbol } => {
                write!(f, "the device at {symbol} is already bound in this PD")
            }
            Self::Misaligned { vaddr, align } => write!(
                f,
                "device address {vaddr:#x} is not aligned to {align} bytes"
            ),
            Self::TooSmall { size, needed } => write!(
                f,
                "the device mapping is {size} bytes; its registers need {needed}"
            ),
            Self::OutOfRange { vaddr } => write!(
                f,
                "the registers at {vaddr:#x} would run past the end of the address space"
            ),
            Self::PhysicalAddressUnknown { symbol } => write!(
                f,
                "{symbol} is not patched: the system description needs a <setvar> with region_paddr"
            ),
            Self::OutsideRegion { offset, len, size } => write!(
                f,
                "{len} bytes at offset {offset} do not fit in a DMA region of {size} bytes"
            ),
            Self::NoSignature { expected, read } => write!(
                f,
                "expected a {expected}, but its identifying register reads {read:#018x}"
            ),
            Self::TransferFailed { device, status } => {
                write!(f, "{device} failed a transfer (status {status:#010x})")
            }
            Self::WrongDevice {
                expected,
                part,
                designer,
            } => write!(
                f,
                "expected a {expected}, but the device identifies as part {part:#05x} \
                 from designer {designer:#04x}"
            ),
            Self::NotCompatible { expected } => write!(
                f,
                "the board's facts name another device: its compatible string is not one a \
                 {expected} view drives"
            ),
            Self::WindowTooSmall { window, needed } => write!(
                f,
                "the board's facts give the device {window:#x} bytes of registers, but its view \
                 reads {needed:#x}"
            ),
            Self::NoVirtioDevice { id } => write!(
                f,
                "no virtio device with ID {id} in the virtio-mmio transports granted"
            ),
            Self::VirtioVersion { found } => write!(
                f,
                "the virtio device offers register version {found}, not 2 (virtio 1.x): \
                 QEMU needs -global virtio-mmio.force-legacy=false"
            ),
            Self::FeaturesRefused { offered, accepted } => write!(
                f,
                "the virtio device refused the features {accepted:#x} (it offered {offered:#x})"
            ),
            Self::DeviceFailed { status } => {
                write!(f, "the virtio device needs a reset (status {status:#04x})")
            }
            Self::QueueTooBig { index, wanted, max } => write!(
                f,
                "virtio queue {index} takes at most {max} entries (0: absent or already set up), \
                 not {wanted}"
            ),
            Self::IoRegionInvalid {
                symbol,
                paddr,
                size,
            } => write!(
                f,
                "{symbol}: an I/O region of {size} bytes at physical address {paddr:#x} \
                 (it must be non-empty, page-aligned and inside memory)"
            ),
            Self::ConfigUnstable => {
                f.write_str("the virtio device's configuration kept changing while it was read")
            }
            Self::FrameLength { len } => write!(f, "a {len}-byte frame"),
            Self::PciFunction { reason } => write!(f, "the PCI function: {reason}"),
            Self::XhciCapabilities(error) => {
                write!(f, "the xHCI controller's capability registers: {error}")
            }
            Self::Xhci { reason } => write!(f, "the xHCI controller: {reason}"),
            Self::XhciTimeout { what } => {
                write!(f, "the xHCI controller did not finish {what} in time")
            }
            Self::Gem { reason } => write!(f, "the GEM MAC: {reason}"),
            Self::Watchdog { reason } => write!(f, "the PM watchdog: {reason}"),
            Self::Fan { reason } => write!(f, "the fan: {reason}"),
            Self::Mailbox { reason } => write!(f, "the firmware's mailbox: {reason}"),
            Self::Scan { reason } => write!(f, "the display's scan: {reason}"),
            Self::GemTimeout { what } => write!(f, "the GEM MAC did not finish {what} in time"),
            Self::GemRing(error) => write!(f, "a GEM descriptor ring: {error:?}"),
            Self::Mdio(nowayos_gem::mdio::MdioError::Address) => {
                write!(f, "a PHY or register address over 31")
            }
            Self::Mdio(nowayos_gem::mdio::MdioError::NoReply) => {
                write!(f, "no PHY answered on MDIO")
            }
        }
    }
}
