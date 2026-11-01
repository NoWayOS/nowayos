// SPDX-License-Identifier: MIT OR Apache-2.0
//! An xHCI USB host controller: its registers, and its rings and contexts
//! in the DMA region it owns.
//!
//! Written from Intel's *eXtensible Host Controller Interface for Universal
//! Serial Bus (xHCI)*, revision 1.2: the register interface (chapter 5),
//! the data structures (chapter 6), controller initialisation (section
//! 4.2), device slots and contexts (4.3 to 4.6), and the rings (4.9 to
//! 4.11). On the Raspberry Pi 5 the controllers are RP1's, two Synopsys
//! `dwc_usb3` controllers RP1's datasheet says conform to xHCI (RP1
//! Peripherals, chapter 5); nothing here depends on that beyond the
//! specification. No driver source was read.
//!
//! **Everything the controller is given lies in this PD's DMA region.** The
//! controller reads its rings and contexts and writes events, contexts and
//! data there by DMA, at bus addresses: the region's physical addresses
//! plus the board's offset for the controller's bus (on the Pi 5, RP1 sees
//! DRAM 0x10_0000_0000 on: `Xhci::new`). Every address it is ever told - the device
//! context base address array and its entries, the scratchpad buffers, the
//! command ring, the event ring and its segment table, each device's input
//! and output contexts and transfer rings, every data buffer - is computed
//! here by [`DmaRegion::device_address`], inside the region, which this
//! view owns: no safe code can put another address on a ring. The
//! registers that start DMA (USBCMD's Run/Stop, CRCR, DCBAAP, ERSTBA, the
//! doorbells) are written only by this module, with those addresses.
//!
//! **Nothing the controller or a device writes is trusted.** An event is
//! copied out of the ring before it is decoded and checked
//! (`nowayos_usb::trb::Event::decode`), and a TRB pointer in it is matched
//! against the ring it names: a completion for a command never queued, a
//! transfer on an endpoint not configured or a pointer off its ring is
//! refused ([`HwError::Xhci`]). Data a device sends - descriptors, reports -
//! is copied out before the caller decodes it.
//!
//! The controller's own DMA is not confined: nothing between RP1 and the
//! Pi 5's memory limits where it may write (no IOMMU on that PCIe link), so
//! the controller is trusted to follow its rings, as any DMA master is.
//! Its USB devices are not: they reach memory only through the buffers
//! this module gives the controller.
//!
//! The registers are viewed as plain 32-bit words ([`Device::view_words`]):
//! an xHCI controller's capability registers give where its others are, so
//! no fixed layout fits every controller. Every offset used comes from
//! [`nowayos_usb::caps::Layout`], checked to lie inside the window before
//! any register past the capabilities is touched; each access states why it
//! is safe. No register of the controller has a read side effect (xHCI 1.2,
//! section 5.1: its registers are read-only, read-write, write-1-to-clear or
//! write-1-to-set).
//!
//! **Isochronous streams**: given a DMA region of at least
//! [`DMA_SIZE_STREAMING`] bytes, up to [`MAX_STREAMS`] isochronous
//! endpoints at once, IN or OUT, at full or high speed, each with a ring
//! of its own and buffers for the transfer descriptors queued on it
//! (section 4.11.2.3: one TD a service interval, an Isoch TRB each, Start
//! Isoch ASAP set). The caller keeps each queued ahead of the controller
//! ([`Xhci::queue_isoch`]) and rings its doorbell; each TD's completion is
//! matched with its own TRB and counted - served, or missed - by
//! `nowayos_usb::isoch::Queue`, and a ring that ran empty is counted too,
//! never an error. A smaller region gives no streams, and the driver runs
//! as before. A camera's video endpoint, which its own pages
//! cannot keep fed, takes a pool of buffer pages instead
//! ([`Xhci::stream_pool`], given a region of [`DMA_SIZE_CAMERA`]), and its
//! IN TDs' data is copied out with [`Xhci::stream_data`]; a control
//! transfer can carry data to the device ([`Xhci::control_out`]: a
//! camera's probe and commit).
//!
//! Provisional: polled (interrupter 0's interrupt stays off: the PD drains
//! the event ring on a timer), one event ring segment, up to [`MAX_DEVICES`]
//! devices on root ports, the default control endpoint and up to
//! [`MAX_ENDPOINTS`] interrupt IN endpoints each, and [`MAX_STREAMS`]
//! isochronous endpoints in all, each TD of one TRB within one page.

use nowayos_usb::audio::Stream;
use nowayos_usb::caps::{
    self, CAPABILITY_WORDS, Capabilities, Layout, Protocol, extended, interrupter, legacy,
    operational, usbcmd, usbsts,
};
use nowayos_usb::context::{
    ContextSize, Endpoint, EndpointKind, InputContext, Slot, SlotReport, dci, interrupt_interval,
    isoch_interval,
};
use nowayos_usb::descriptor::BootInterface;
use nowayos_usb::isoch::{self, Queue, Stats};
use nowayos_usb::port::{PortStatus, PortWrite, Speed};
use nowayos_usb::request::Setup;
use nowayos_usb::trb::{self, Completion, DataStage, Event, TRB_LEN, Trb};
use safe_mmio::UniqueMmioPointer;

use crate::cpu::{counter, counter_frequency, dsb};
use crate::{Device, DmaRegion, HwError};

/// The device slots this driver enables (CONFIG MaxSlotsEn): a keyboard, a
/// mouse and two more.
pub const MAX_DEVICES: usize = 4;

/// The interrupt IN endpoints set up per device: a keyboard's and a mouse's,
/// as a receiver presents them.
pub const MAX_ENDPOINTS: usize = 2;

/// The bytes of the control transfer buffer: a configuration up to
/// `nowayos_usb::enumerate::CONFIG_MAX` (the large buffer takes up to
/// `CONFIG_MAX_STREAMING`).
pub const CONTROL_BUFFER: usize = 512;

/// The bytes of each report buffer: a full-speed interrupt packet.
pub const REPORT_BUFFER: usize = 64;

/// Reports kept queued on each interrupt endpoint.
pub const REPORTS_QUEUED: usize = 8;

/// The scratchpad buffers this driver can give a controller (RP1's need 2).
pub const MAX_SCRATCHPADS: usize = 4;

/// The isochronous endpoints this driver streams at once, on all its
/// devices together: a headset's two and a camera's microphone.
pub const MAX_STREAMS: usize = 4;

/// The page size every structure is laid out in, and the only one this
/// driver takes (PAGESIZE bit 0).
const PAGE: usize = 4096;

/// TRBs in each ring, its Link TRB included.
const COMMAND_TRBS: usize = 64;
const EVENT_TRBS: usize = 256;
const TRANSFER_TRBS: usize = 16;

// The DMA region's layout, in bytes. Each structure lies inside one page,
// so none crosses a 64 KiB boundary (sections 6.1 and 6.5).
const DCBAA: usize = 0x0000;
const ERST: usize = 0x0800;
const SCRATCH_ARRAY: usize = 0x0C00;
const COMMAND_RING: usize = 0x1000;
const EVENT_RING: usize = 0x2000;
const SCRATCH_PAGES: usize = 0x3000;
const DEVICES_BASE: usize = SCRATCH_PAGES + MAX_SCRATCHPADS * PAGE;
const DEVICE_STRIDE: usize = 3 * PAGE;
// Within each device's three pages.
const OUTPUT_CONTEXT: usize = 0x0000;
const INPUT_CONTEXT: usize = 0x1000;
const CONTROL_RING: usize = 0x2000;
const INTERRUPT_RINGS: usize = 0x2100;
const RING_BYTES: usize = TRANSFER_TRBS * TRB_LEN;
const CONTROL_DATA: usize = 0x2300;
const REPORTS: usize = CONTROL_DATA + CONTROL_BUFFER;
const REPORTS_PER_ENDPOINT: usize = REPORTS_QUEUED * REPORT_BUFFER;

/// The DMA region a controller needs, in bytes: give it at least this.
pub const DMA_SIZE: usize = DEVICES_BASE + MAX_DEVICES * DEVICE_STRIDE;

// Each stream's pages, after the devices': its ring's page, then its
// buffers' pages. A TD's buffer never crosses a page, so never a 64 KiB
// boundary (section 4.11.7.1).
const STREAMS_BASE: usize = DMA_SIZE;
const STREAM_TRBS: usize = PAGE / TRB_LEN;
const STREAM_BUFFER_PAGES: usize = 5;
const STREAM_STRIDE: usize = (1 + STREAM_BUFFER_PAGES) * PAGE;

/// The bytes of the large control buffer a streaming controller has:
/// a camera's whole configuration, its video formats listed ahead
/// of its microphone - far past [`CONTROL_BUFFER`].
pub const CONTROL_BUFFER_LARGE: usize = 8192;

// Twice the large buffer's size, after the streams': an 8 KiB window in it
// always crosses no 64 KiB boundary, as a Data Stage's buffer must not
// (section 4.11.7.1), wherever the region lies.
const LARGE_BASE: usize = STREAMS_BASE + MAX_STREAMS * STREAM_STRIDE;
const LARGE_AREA: usize = 2 * CONTROL_BUFFER_LARGE;

/// The DMA region a controller needs to stream isochronous endpoints,
/// in bytes: give it at least this, or it streams none and reads
/// no configuration longer than [`CONTROL_BUFFER`].
pub const DMA_SIZE_STREAMING: usize = LARGE_BASE + LARGE_AREA;

// A pool of buffer pages for one stream, after the large buffer:
// a camera's video endpoint moves up to 3 KiB every 125 us microframe, so
// the 8 ms between two of the PD's ticks are 64 TDs, where a stream's own
// pages hold 5. One TD a page, so none crosses a page, as many as a
// stream's ring holds.
const POOL_BASE: usize = DMA_SIZE_STREAMING;
const POOL_PAGES: usize = STREAM_TRBS - 2;

/// The DMA region a controller needs to stream a camera's video,
/// in bytes: [`DMA_SIZE_STREAMING`]'s, and a pool of buffers for one stream
/// ([`Xhci::stream_pool`]).
pub const DMA_SIZE_CAMERA: usize = POOL_BASE + POOL_PAGES * PAGE;

const _: () = {
    assert!(ERST + 16 <= SCRATCH_ARRAY);
    assert!((MAX_DEVICES + 1) * 8 <= ERST);
    assert!(SCRATCH_ARRAY + MAX_SCRATCHPADS * 8 <= COMMAND_RING);
    assert!(COMMAND_RING + COMMAND_TRBS * TRB_LEN <= EVENT_RING);
    assert!(EVENT_RING + EVENT_TRBS * TRB_LEN <= SCRATCH_PAGES);
    assert!(INTERRUPT_RINGS + MAX_ENDPOINTS * RING_BYTES <= CONTROL_DATA);
    assert!(REPORTS + MAX_ENDPOINTS * REPORTS_PER_ENDPOINT <= DEVICE_STRIDE);
    assert!(CONTROL_RING + RING_BYTES <= INTERRUPT_RINGS);
    // A 64-byte context's input context fits its page.
    assert!(33 * 64 <= PAGE);
    assert!(REPORTS_QUEUED < TRANSFER_TRBS);
    // A ring of TDs never fills: at most its TRBs less its Link TRB and
    // one more, so the enqueue point never reaches the dequeue point.
    assert!(STREAM_TRBS - 2 <= isoch::MAX_QUEUED);
};

/// How long the controller may take to halt (16 ms, section 5.4.1) and to
/// reset or become ready (no bound in the specification: one second). The
/// halt is given more than its 16 ms: on the Pi 5, RP1's controller, left
/// running by the firmware with a keyboard in use, once took longer than
/// 20 ms (2026-10-07).
const HALT_MS: u64 = 100;
const RESET_MS: u64 = 1000;

/// CRCR's Command Abort bit (section 5.4.5).
const CRCR_ABORT: u64 = 1 << 2;

/// CRCR's Command Ring Running bit, read-only.
const CRCR_RUNNING: u32 = 1 << 3;

/// A ring this driver produces TRBs on: where it is in the DMA region, its
/// length (Link TRB included), where the next TRB goes and the cycle state.
#[derive(Clone, Copy, Debug)]
struct Ring {
    base: usize,
    len: usize,
    enqueue: usize,
    cycle: bool,
}

impl Ring {
    const fn new(base: usize, len: usize) -> Self {
        Self {
            base,
            len,
            enqueue: 0,
            cycle: true,
        }
    }

    /// The offset of TRB `index` in the DMA region.
    fn offset(&self, index: usize) -> Option<usize> {
        index.checked_mul(TRB_LEN)?.checked_add(self.base)
    }

    /// The index of the TRB at bus address `address`, if it is one of this
    /// ring's (not its Link TRB), given the ring's own bus address.
    fn index_of(&self, address: u64, ring_address: u64) -> Option<usize> {
        let offset = usize::try_from(address.checked_sub(ring_address)?).ok()?;
        let index = offset / TRB_LEN;
        (offset.is_multiple_of(TRB_LEN) && index < self.len.checked_sub(1)?).then_some(index)
    }
}

/// A control transfer in flight on a device's default endpoint: the TRB
/// indices of its stages, what was asked for, and what came.
#[derive(Clone, Copy, Debug)]
struct Control {
    data: Option<usize>,
    status: usize,
    requested: usize,
    short: Option<usize>,
}

/// An interrupt IN endpoint: its index, ring, packet size, and which
/// report buffer each queued TRB fills.
#[derive(Clone, Copy, Debug)]
struct Interrupt {
    dci: u8,
    ring: Ring,
    max_packet: usize,
    pending: [Option<u8>; TRANSFER_TRBS],
}

/// An isochronous endpoint being streamed: its device slot and index, its
/// ring, its packets and its TDs in flight.
#[derive(Clone, Copy, Debug)]
struct StreamSlot {
    slot: u8,
    dci: u8,
    ring: Ring,
    inbound: bool,
    /// The most bytes one TD moves: an interval's.
    td_bytes: usize,
    max_packet: usize,
    max_burst: u8,
    /// TD buffers in each of its buffer pages.
    per_page: usize,
    /// Where its buffer pages start: the page after its ring's, or the
    /// pool's.
    buffers: usize,
    queue: Queue,
}

/// One device slot's state.
#[derive(Clone, Copy, Debug)]
struct DeviceSlot {
    control_ring: Ring,
    control: Option<Control>,
    control_len: usize,
    /// Whether its last control transfer's data went to the large buffer.
    control_large: bool,
    interrupts: [Option<Interrupt>; MAX_ENDPOINTS],
}

impl DeviceSlot {
    fn new(base: usize) -> Self {
        Self {
            control_ring: Ring::new(base.saturating_add(CONTROL_RING), TRANSFER_TRBS),
            control: None,
            control_len: 0,
            control_large: false,
            interrupts: [None; MAX_ENDPOINTS],
        }
    }
}

/// A command this driver queued, as queuing it returned: its completion
/// names it, so a late completion of a command given up on is never taken
/// for a later one of the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandId(u32);

/// A queued command: its TRB type and its id.
#[derive(Clone, Copy, Debug)]
struct Queued {
    kind: u8,
    id: CommandId,
}

/// What the controller reported, checked against what this driver asked of
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XhciEvent {
    /// A command completed. `kind` is the command's TRB type
    /// (`nowayos_usb::trb::kind`), from this driver's own record of it.
    Command {
        /// The command.
        kind: u8,
        /// How it completed.
        code: Completion,
        /// The slot the controller names: for Enable Slot, the slot it
        /// enabled.
        slot: u8,
        /// Which command: what queuing it returned.
        id: CommandId,
    },
    /// A root port's status changed: read it with [`Xhci::port_status`].
    Port {
        /// The port, from 1.
        port: u8,
    },
    /// A control transfer on `slot`'s default endpoint ended: on success,
    /// `len` bytes came (read them with [`Xhci::control_data`]).
    Control {
        /// The slot.
        slot: u8,
        /// How it ended: [`Completion::SUCCESS`] when the whole transfer
        /// did, short or not.
        code: Completion,
        /// The bytes the Data Stage brought.
        len: usize,
    },
    /// A report came on one of `slot`'s interrupt endpoints, copied out
    /// here; the buffer is already queued again.
    Report {
        /// The slot.
        slot: u8,
        /// Which of the device's interrupt endpoints, in the order
        /// [`Xhci::configure_endpoints`] was given them.
        endpoint: usize,
        /// The report's bytes: the first `len`.
        bytes: [u8; REPORT_BUFFER],
        /// How many came.
        len: usize,
    },
    /// An interrupt endpoint's transfer failed: a stall or a transaction
    /// error. Its reports stop until the device is enumerated again.
    ReportFailed {
        /// The slot.
        slot: u8,
        /// Which endpoint.
        endpoint: usize,
        /// How.
        code: Completion,
    },
    /// One of stream `stream`'s TDs completed: served, or missed, as
    /// `code` says - its counts ([`Xhci::stream_stats`]) already have it.
    Stream {
        /// The stream: what [`Xhci::stream_of`] gave.
        stream: usize,
        /// How the TD completed.
        code: Completion,
        /// The bytes it moved.
        len: usize,
        /// Its buffer: an IN TD's bytes are there, for
        /// [`Xhci::stream_data`], until a new TD takes it.
        buffer: usize,
    },
    /// Stream `stream`'s ring was empty when its interval came: counted as
    /// an interval missed; queuing TDs and ringing its doorbell restarts
    /// it.
    StreamEmpty {
        /// The stream.
        stream: usize,
    },
    /// The controller reports an error of its own.
    HostController {
        /// Which.
        code: Completion,
    },
    /// An event this driver does not act on.
    Ignored,
}

/// An xHCI controller, its registers and its DMA region. Make one with
/// [`Xhci::new`].
#[derive(Debug)]
pub struct Xhci {
    regs: UniqueMmioPointer<'static, [u32]>,
    caps: Capabilities,
    layout: Layout,
    protocols: [Option<Protocol>; 4],
    legacy: Option<usize>,
    context: ContextSize,
    dma: DmaRegion,
    bus_offset: u64,
    refused: Option<(u64, u64)>,
    commands: Ring,
    queued: [Option<Queued>; COMMAND_TRBS],
    next_id: u32,
    events: usize,
    event_cycle: bool,
    devices: [Option<DeviceSlot>; MAX_DEVICES],
    streams: [Option<StreamSlot>; MAX_STREAMS],
}

impl Xhci {
    /// Views `device` as an xHCI controller with `dma` as its memory: reads
    /// and checks the capability registers, the window's every register the
    /// driver uses and the extended capabilities, and that `dma` holds
    /// [`DMA_SIZE`] bytes. Touches nothing past the capability registers
    /// but the extended capabilities, which it only reads.
    ///
    /// `bus_offset` is where the controller sees memory: the bus address of
    /// physical address 0, added to every address it is given - 0 where it
    /// sees memory as the CPU does (QEMU), 0x10_0000_0000 for RP1's on the
    /// Pi 5. It is the board's, from its facts about the controller
    /// (`dma_offset` in its board file), and the PD has no other source: a
    /// wrong one would point the controller at other memory, as a wrong
    /// device window would point a driver at other registers.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::XhciCapabilities`] for
    /// capability registers that are refused or point outside the window;
    /// [`HwError::Xhci`] for a controller that needs more scratchpad
    /// buffers than [`MAX_SCRATCHPADS`] or a DMA region that is too small.
    pub fn new(device: Device, dma: DmaRegion, bus_offset: u64) -> Result<Self, HwError> {
        let window = device.size();
        let mut regs = device.view_words()?;
        let mut words = [0u32; CAPABILITY_WORDS];
        for (index, word) in words.iter_mut().enumerate() {
            *word = read_word(&mut regs, index.saturating_mul(4))?;
        }
        let caps = Capabilities::decode(&words).map_err(HwError::XhciCapabilities)?;
        let layout = caps.layout(window).map_err(HwError::XhciCapabilities)?;
        if usize::from(caps.scratchpads) > MAX_SCRATCHPADS {
            return Err(HwError::Xhci {
                reason: "the controller needs more scratchpad buffers than this driver gives",
            });
        }
        if dma.len() < DMA_SIZE {
            return Err(HwError::Xhci {
                reason: "a DMA region smaller than the controller's structures",
            });
        }
        let mut xhci = Self {
            regs,
            caps,
            layout,
            protocols: [None; 4],
            legacy: None,
            context: ContextSize::from_csz(caps.context_64),
            dma,
            bus_offset,
            refused: None,
            commands: Ring::new(COMMAND_RING, COMMAND_TRBS),
            queued: [None; COMMAND_TRBS],
            next_id: 0,
            events: 0,
            event_cycle: true,
            devices: [None; MAX_DEVICES],
            streams: [None; MAX_STREAMS],
        };
        xhci.find_extended(window)?;
        Ok(xhci)
    }

    /// Walks the extended capabilities (section 7), at most 32 of them and
    /// never outside the window, noting the Supported Protocols and where
    /// USB Legacy Support is.
    fn find_extended(&mut self, window: usize) -> Result<(), HwError> {
        let (mut at, _) = self.layout.extended();
        let mut found = 0;
        for _ in 0..32 {
            if at == 0 || at.checked_add(12).is_none_or(|end| end > window) {
                break;
            }
            let header = self.read(at)?;
            let (id, next) = caps::extended_header(header);
            if id == extended::PROTOCOL {
                let words = [
                    header,
                    self.read(at.saturating_add(4))?,
                    self.read(at.saturating_add(8))?,
                ];
                if let (Some(protocol), Some(slot)) =
                    (Protocol::decode(words), self.protocols.get_mut(found))
                {
                    *slot = Some(protocol);
                    found = found.saturating_add(1);
                }
            } else if id == extended::LEGACY {
                self.legacy = Some(at);
            }
            if next == 0 {
                break;
            }
            at = at.saturating_add(usize::from(next).saturating_mul(4));
        }
        Ok(())
    }

    /// The controller's capability registers, decoded.
    #[must_use]
    pub const fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Its Supported Protocol capabilities: which ports are USB 2.0 and
    /// which USB 3.
    pub fn protocols(&self) -> impl Iterator<Item = &Protocol> {
        self.protocols.iter().flatten()
    }

    /// The USB major revision of `port`'s protocol, 2 or 3, if a Supported
    /// Protocol capability names it.
    #[must_use]
    pub fn major_of(&self, port: u8) -> Option<u8> {
        self.protocols()
            .find(|protocol| protocol.has(port))
            .map(|protocol| protocol.major)
    }

    /// USBCMD and USBSTS as the controller left them: for a report of what
    /// the firmware handed over.
    ///
    /// # Errors
    ///
    /// None in practice: both are inside the window.
    pub fn command_and_status(&mut self) -> Result<(u32, u32), HwError> {
        Ok((
            self.read_operational(operational::USBCMD)?,
            self.read_operational(operational::USBSTS)?,
        ))
    }

    /// Takes the controller from firmware that claims it through USB Legacy
    /// Support (section 4.22.1): sets OS Owned and waits for BIOS Owned to
    /// clear. Returns whether the firmware held it.
    ///
    /// # Errors
    ///
    /// [`HwError::XhciTimeout`] if the firmware does not let go in a
    /// second.
    pub fn take_from_firmware(&mut self) -> Result<bool, HwError> {
        let Some(at) = self.legacy else {
            return Ok(false);
        };
        let word = self.read(at)?;
        if word & legacy::BIOS_OWNED == 0 {
            return Ok(false);
        }
        self.write(at, word | legacy::OS_OWNED)?;
        self.wait(RESET_MS, "the firmware's hand-over", |xhci| {
            Ok(xhci.read(at)? & legacy::BIOS_OWNED == 0)
        })?;
        Ok(true)
    }

    /// Stops the controller - the firmware may have left it running, with
    /// rings in memory that is no longer its own - and resets it (section
    /// 4.2): Run/Stop cleared, HCHalted awaited, then Host Controller
    /// Reset, and Controller Not Ready awaited. Forgets every device.
    ///
    /// # Errors
    ///
    /// [`HwError::XhciTimeout`] if it does not halt or reset in time.
    pub fn halt_and_reset(&mut self) -> Result<(), HwError> {
        let command = self.read_operational(operational::USBCMD)?;
        // Stopping touches no memory.
        self.write_operational(
            operational::USBCMD,
            command & !(usbcmd::RUN | usbcmd::INTERRUPTS),
        )?;
        self.wait(HALT_MS, "halting", |xhci| {
            Ok(xhci.read_operational(operational::USBSTS)? & usbsts::HALTED != 0)
        })?;
        self.write_operational(operational::USBCMD, usbcmd::RESET)?;
        self.wait(RESET_MS, "its reset", |xhci| {
            Ok(
                xhci.read_operational(operational::USBCMD)? & usbcmd::RESET == 0
                    && xhci.read_operational(operational::USBSTS)? & usbsts::NOT_READY == 0,
            )
        })?;
        self.devices = [None; MAX_DEVICES];
        self.streams = [None; MAX_STREAMS];
        self.queued = [None; COMMAND_TRBS];
        Ok(())
    }

    /// Sets the controller up in its DMA region and runs it (section
    /// 4.2): every structure cleared, MaxSlotsEn [`MAX_DEVICES`], the
    /// device context base address array and its scratchpad buffers, the
    /// command ring, interrupter 0's event ring - its interrupt off: the
    /// driver polls - then Run/Stop. Call after [`Xhci::halt_and_reset`].
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] if the controller does not take 4 KiB pages;
    /// [`HwError::XhciTimeout`] if it does not start.
    pub fn start(&mut self) -> Result<(), HwError> {
        if self.read_operational(operational::PAGESIZE)? & 1 == 0 {
            return Err(HwError::Xhci {
                reason: "the controller does not take 4 KiB pages",
            });
        }
        let zeros = [0u8; PAGE];
        let mut at = 0;
        while at < DMA_SIZE {
            self.dma.write(at, &zeros)?;
            at = at.saturating_add(PAGE);
        }
        // Scratchpad buffers: their array, its entries, and the array as
        // the device context base address array's entry 0 (section 4.20).
        let scratchpads = usize::from(self.caps.scratchpads);
        for index in 0..scratchpads {
            let page = PAGE.saturating_mul(index).saturating_add(SCRATCH_PAGES);
            let address = self.bus(page, PAGE)?;
            self.put64(
                SCRATCH_ARRAY.saturating_add(index.saturating_mul(8)),
                address,
            )?;
        }
        if scratchpads > 0 {
            let array = self.bus(SCRATCH_ARRAY, scratchpads.saturating_mul(8))?;
            self.put64(DCBAA, array)?;
        }
        // The command ring's Link TRB, back to its start, toggling the cycle.
        let command_ring = self.bus(COMMAND_RING, COMMAND_TRBS * TRB_LEN)?;
        self.commands = Ring::new(COMMAND_RING, COMMAND_TRBS);
        self.queued = [None; COMMAND_TRBS];
        // The event ring: one segment, one entry in its table.
        let event_ring = self.bus(EVENT_RING, EVENT_TRBS * TRB_LEN)?;
        self.put64(ERST, event_ring)?;
        self.dma.write(
            ERST.saturating_add(8),
            &u32::try_from(EVENT_TRBS).unwrap_or(0).to_le_bytes(),
        )?;
        self.events = 0;
        self.event_cycle = true;
        let erst = self.bus(ERST, 16)?;
        let dcbaa = self.bus(DCBAA, (MAX_DEVICES.saturating_add(1)).saturating_mul(8))?;
        // Everything above must be in memory before the controller is told
        // where it is.
        dsb();
        let slots = MAX_DEVICES.min(usize::from(self.caps.max_slots));
        self.write_operational(operational::CONFIG, u32::try_from(slots).unwrap_or(1))?;
        self.write_pointer(Pointer::Dcbaap, dcbaa)?;
        self.write_pointer(Pointer::Crcr, command_ring | 1)?;
        self.write_interrupter(interrupter::IMAN, 0b01)?;
        self.write_interrupter(interrupter::IMOD, 0)?;
        self.write_interrupter(interrupter::ERSTSZ, 1)?;
        self.write_pointer(Pointer::Erdp, event_ring)?;
        // ERSTBA last: writing it starts the event ring (section 5.5.2.3.2).
        self.write_pointer(Pointer::Erstba, erst)?;
        let command = self.read_operational(operational::USBCMD)?;
        self.write_operational(
            operational::USBCMD,
            (command | usbcmd::RUN) & !usbcmd::INTERRUPTS,
        )?;
        self.wait(HALT_MS, "starting", |xhci| {
            Ok(xhci.read_operational(operational::USBSTS)? & usbsts::HALTED == 0)
        })
    }

    /// USBSTS: Host System Error or Host Controller Error, if either is
    /// set.
    ///
    /// # Errors
    ///
    /// None in practice: USBSTS is inside the window.
    pub fn failed(&mut self) -> Result<Option<u32>, HwError> {
        let status = self.read_operational(operational::USBSTS)?;
        Ok(
            (status & (usbsts::HOST_SYSTEM_ERROR | usbsts::CONTROLLER_ERROR) != 0)
                .then_some(status),
        )
    }

    /// Port `port`'s PORTSC (ports count from 1).
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a port the controller does not have.
    pub fn port_status(&mut self, port: u8) -> Result<PortStatus, HwError> {
        let at = self.layout.portsc(port).ok_or(HwError::Xhci {
            reason: "a port the controller does not have",
        })?;
        Ok(PortStatus(self.read(at)?))
    }

    /// Makes one change through port `port`'s PORTSC, leaving the rest as
    /// they are ([`PortStatus::write_value`]).
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a port the controller does not have.
    pub fn port_write(&mut self, port: u8, write: PortWrite) -> Result<(), HwError> {
        let status = self.port_status(port)?;
        let at = self.layout.portsc(port).ok_or(HwError::Xhci {
            reason: "a port the controller does not have",
        })?;
        // PORTSC writes power, reset or clear status bits of the port: none
        // touches memory.
        self.write(at, status.write_value(write))
    }

    // ---- Commands ---------------------------------------------------------

    /// Queues a No Op command: its completion shows the command ring and the
    /// event ring work, both ways through DMA.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] if the command ring is full.
    pub fn no_op(&mut self) -> Result<CommandId, HwError> {
        self.command(trb::no_op_command())
    }

    /// Queues Enable Slot. The completion's slot is the device's from then
    /// on; [`Xhci::address_device`] takes it.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] if the command ring is full.
    pub fn enable_slot(&mut self) -> Result<CommandId, HwError> {
        self.command(trb::enable_slot())
    }

    /// The device slot `slot` names (1 to [`MAX_DEVICES`]): its index here.
    fn index_of(slot: u8) -> Result<usize, HwError> {
        usize::from(slot)
            .checked_sub(1)
            .filter(|index| *index < MAX_DEVICES)
            .ok_or(HwError::Xhci {
                reason: "a slot outside those this driver enables",
            })
    }

    /// The DMA offset of device slot `index`'s pages.
    fn device_base(index: usize) -> usize {
        index
            .saturating_mul(DEVICE_STRIDE)
            .saturating_add(DEVICES_BASE)
    }

    /// Sets slot `slot` up for the device on root port `port` and queues
    /// Address Device: its output context cleared and entered in the
    /// device context base address array, its default control endpoint's
    /// ring started afresh, and an input context with the slot and that
    /// endpoint, `max_packet` bytes a packet.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a slot this driver did not enable or a context
    /// out of range; any DMA error.
    pub fn address_device(
        &mut self,
        slot: u8,
        port: u8,
        speed: Speed,
        max_packet: u16,
    ) -> Result<CommandId, HwError> {
        let index = Self::index_of(slot)?;
        let base = Self::device_base(index);
        self.dma.write(base, &[0u8; PAGE])?;
        self.dma
            .write(base.saturating_add(CONTROL_RING), &[0u8; RING_BYTES])?;
        let device = DeviceSlot::new(base);
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        let output = self.bus(
            base.saturating_add(OUTPUT_CONTEXT),
            self.context.device_len(),
        )?;
        self.put64(
            DCBAA.saturating_add(usize::from(slot).saturating_mul(8)),
            output,
        )?;
        let ep0 = self.control_endpoint(&device, max_packet)?;
        let input = self.input_context(
            index,
            &InputContext {
                slot: Some(Slot {
                    speed,
                    root_port: port,
                    entries: 1,
                }),
                endpoints: &[ep0],
                configuration: 0,
            },
        )?;
        self.command(trb::address_device(input, slot))
    }

    /// Queues Evaluate Context: the default control endpoint now takes
    /// `max_packet` bytes a packet.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn evaluate_control(&mut self, slot: u8, max_packet: u16) -> Result<CommandId, HwError> {
        let index = Self::index_of(slot)?;
        let device = self.device(index)?;
        let ep0 = self.control_endpoint(&device, max_packet)?;
        let input = self.input_context(
            index,
            &InputContext {
                slot: None,
                endpoints: &[ep0],
                configuration: 0,
            },
        )?;
        self.command(trb::evaluate_context(input, slot))
    }

    /// Sets up an interrupt IN endpoint for each of `boot` - at most
    /// [`MAX_ENDPOINTS`] - and an isochronous endpoint for each of
    /// `streams`, each with a fresh ring, and queues Configure
    /// Endpoint for them, with configuration value `configuration`. A
    /// stream is started once its alternate setting is selected: queue its
    /// TDs ([`Xhci::queue_isoch`]) and ring its doorbell
    /// ([`Xhci::ring_stream`]).
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for no endpoints, more than [`MAX_ENDPOINTS`]
    /// interrupt endpoints, more streams than [`Xhci::streams_available`],
    /// an endpoint address with no index or a packet size this driver's
    /// buffers cannot take; as for [`Xhci::address_device`].
    pub fn configure_endpoints(
        &mut self,
        slot: u8,
        port: u8,
        speed: Speed,
        configuration: u8,
        boot: &[BootInterface],
        streams: &[Stream],
    ) -> Result<CommandId, HwError> {
        let index = Self::index_of(slot)?;
        let mut device = self.device(index)?;
        if (boot.is_empty() && streams.is_empty()) || boot.len() > MAX_ENDPOINTS {
            return Err(HwError::Xhci {
                reason: "no endpoints, or more interrupt endpoints than this driver sets up",
            });
        }
        if streams.len() > self.streams_available() {
            return Err(HwError::Xhci {
                reason: "more isochronous streams than this driver has room for",
            });
        }
        let base = Self::device_base(index);
        let unused = Endpoint {
            dci: 1,
            kind: EndpointKind::InterruptIn,
            max_packet: 8,
            max_burst: 0,
            interval: 0,
            dequeue: 0,
            cycle: true,
        };
        // A device's streams and their feedback endpoints, and a
        // camera's video endpoint with them.
        let mut endpoints = [unused; MAX_ENDPOINTS + MAX_STREAMS + 1];
        let mut entries = 1u8;
        for (number, (interface, endpoint)) in boot.iter().zip(endpoints.iter_mut()).enumerate() {
            let bad = HwError::Xhci {
                reason: "an interrupt endpoint this driver cannot set up",
            };
            let dci = dci(interface.endpoint).ok_or(bad)?;
            if usize::from(interface.max_packet) > REPORT_BUFFER {
                return Err(bad);
            }
            let ring_base = number
                .saturating_mul(RING_BYTES)
                .saturating_add(INTERRUPT_RINGS)
                .saturating_add(base);
            self.dma.write(ring_base, &[0u8; RING_BYTES])?;
            let ring = Ring::new(ring_base, TRANSFER_TRBS);
            *endpoint = Endpoint {
                dci,
                kind: EndpointKind::InterruptIn,
                max_packet: interface.max_packet,
                max_burst: 0,
                interval: interrupt_interval(speed, interface.interval),
                dequeue: self.bus(ring_base, RING_BYTES)?,
                cycle: true,
            };
            if let Some(entry) = device.interrupts.get_mut(number) {
                *entry = Some(Interrupt {
                    dci,
                    ring,
                    max_packet: usize::from(interface.max_packet),
                    pending: [None; TRANSFER_TRBS],
                });
            }
            entries = entries.max(dci);
        }
        // The streams', after the interrupt endpoints'.
        let mut pool = self.streams;
        let mut count = boot.len();
        for stream in streams {
            let bad = HwError::Xhci {
                reason: "an isochronous endpoint this driver cannot set up",
            };
            let dci = dci(stream.endpoint).ok_or(bad)?;
            // Transactions beyond the first each microframe are high
            // speed's alone (section 6.2.3.4).
            let max_burst = if speed == Speed::High {
                stream.max_burst()
            } else {
                0
            };
            let max_packet = usize::from(stream.max_packet);
            let td_bytes = max_packet.saturating_mul(usize::from(max_burst).saturating_add(1));
            if max_packet == 0 || td_bytes > usize::from(trb::ISOCH_MAX) {
                return Err(bad);
            }
            let id = pool.iter().position(Option::is_none).ok_or(bad)?;
            let ring_base = id
                .saturating_mul(STREAM_STRIDE)
                .saturating_add(STREAMS_BASE);
            self.dma.write(ring_base, &[0u8; PAGE])?;
            let per_page = PAGE.checked_div(td_bytes).unwrap_or(1).max(1);
            let buffers = per_page
                .saturating_mul(STREAM_BUFFER_PAGES)
                .min(STREAM_TRBS.saturating_sub(2));
            let inbound = stream.inbound();
            if let Some(entry) = pool.get_mut(id) {
                *entry = Some(StreamSlot {
                    slot,
                    dci,
                    ring: Ring::new(ring_base, STREAM_TRBS),
                    inbound,
                    td_bytes,
                    max_packet,
                    max_burst,
                    per_page,
                    buffers: ring_base.saturating_add(PAGE),
                    queue: Queue::new(buffers),
                });
            }
            if let Some(endpoint) = endpoints.get_mut(count) {
                *endpoint = Endpoint {
                    dci,
                    kind: if inbound {
                        EndpointKind::IsochIn
                    } else {
                        EndpointKind::IsochOut
                    },
                    max_packet: stream.max_packet,
                    max_burst,
                    interval: isoch_interval(speed, stream.interval),
                    dequeue: self.bus(ring_base, PAGE)?,
                    cycle: true,
                };
                count = count.saturating_add(1);
            } else {
                return Err(bad);
            }
            entries = entries.max(dci);
        }
        let used = endpoints.get(..count).unwrap_or_default();
        let input = self.input_context(
            index,
            &InputContext {
                slot: Some(Slot {
                    speed,
                    root_port: port,
                    entries,
                }),
                endpoints: used,
                configuration,
            },
        )?;
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        self.streams = pool;
        self.command(trb::configure_endpoint(input, slot))
    }

    /// Queues Reset Endpoint for `slot`'s default control endpoint, halted
    /// by a stall, then Set TR Dequeue Pointer to where its ring stands, so
    /// the failed transfer's remaining stages are skipped (section 4.6.8).
    /// Returns both commands' ids: the first's failure or the second's
    /// completion ends it.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn reset_control(&mut self, slot: u8) -> Result<(CommandId, CommandId), HwError> {
        let index = Self::index_of(slot)?;
        let mut device = self.device(index)?;
        device.control = None;
        let ring = device.control_ring;
        let at = ring.offset(ring.enqueue).ok_or(HwError::Xhci {
            reason: "a ring position outside the region",
        })?;
        let dequeue = self.bus(at, TRB_LEN)?;
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        let reset = self.command(trb::reset_endpoint(slot, 1))?;
        let dequeue = self.command(trb::set_tr_dequeue(slot, 1, dequeue, ring.cycle))?;
        Ok((reset, dequeue))
    }

    /// Queues Disable Slot for `slot`, and forgets the device.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn disable_slot(&mut self, slot: u8) -> Result<CommandId, HwError> {
        let index = Self::index_of(slot)?;
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = None;
        }
        // Its streams stop with it: their places are free again.
        for entry in &mut self.streams {
            if entry.is_some_and(|stream| stream.slot == slot) {
                *entry = None;
            }
        }
        self.command(trb::disable_slot(slot))
    }

    /// What `slot`'s output slot context says: its address and state.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn slot_report(&self, slot: u8) -> Result<Option<SlotReport>, HwError> {
        let index = Self::index_of(slot)?;
        let mut copy = [0u8; 16];
        self.dma.read(
            Self::device_base(index).saturating_add(OUTPUT_CONTEXT),
            &mut copy,
        )?;
        Ok(SlotReport::decode(&copy))
    }

    // ---- Transfers ---------------------------------------------------------

    /// The offset of the large control buffer, if the region has room for
    /// it ([`DMA_SIZE_STREAMING`]): the start of its area, or the 64 KiB
    /// boundary inside the area's first half, so it crosses none.
    fn large_buffer(&self) -> Option<usize> {
        if self.dma.len() < DMA_SIZE_STREAMING {
            return None;
        }
        let start = self.bus(LARGE_BASE, LARGE_AREA).ok()?;
        let to_boundary = usize::try_from(0x1_0000u64.saturating_sub(start & 0xFFFF)).ok()?;
        Some(if to_boundary >= CONTROL_BUFFER_LARGE {
            LARGE_BASE
        } else {
            LARGE_BASE.saturating_add(to_boundary)
        })
    }

    /// Makes control request `setup` on `slot`'s default endpoint: a Setup
    /// Stage, a Data Stage into the slot's control buffer if the request
    /// reads data (at most [`CONTROL_BUFFER`] bytes, or
    /// [`CONTROL_BUFFER_LARGE`] into the large buffer where the region has
    /// one), and a Status Stage; then rings the slot's doorbell for
    /// endpoint 1.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a transfer already in flight, one that writes
    /// data to the device ([`Xhci::control_out`] makes those) or is longer
    /// than the buffer; as for [`Xhci::address_device`].
    pub fn control(&mut self, slot: u8, setup: Setup) -> Result<(), HwError> {
        self.control_transfer(slot, setup, None)
    }

    /// Makes control request `setup` on `slot`'s default endpoint with a
    /// Data Stage to the device: `data` - exactly the request's
    /// wLength, 1 to [`CONTROL_BUFFER`] bytes - copied into the slot's
    /// control buffer, then a Setup Stage, an OUT Data Stage and an IN
    /// Status Stage; then rings the slot's doorbell for endpoint 1. Its
    /// completion is an [`XhciEvent::Control`], as for [`Xhci::control`].
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a transfer already in flight, a request that
    /// reads, or data of another length; as for [`Xhci::address_device`].
    pub fn control_out(&mut self, slot: u8, setup: Setup, data: &[u8]) -> Result<(), HwError> {
        self.control_transfer(slot, setup, Some(data))
    }

    /// [`Xhci::control`], or with `out` [`Xhci::control_out`].
    fn control_transfer(
        &mut self,
        slot: u8,
        setup: Setup,
        out: Option<&[u8]>,
    ) -> Result<(), HwError> {
        let index = Self::index_of(slot)?;
        let mut device = self.device(index)?;
        let len = usize::from(setup.length);
        let large = if len > CONTROL_BUFFER && out.is_none() {
            self.large_buffer().filter(|_| len <= CONTROL_BUFFER_LARGE)
        } else {
            None
        };
        let unmade = match out {
            None => (len > 0 && !setup.inbound()) || (len > CONTROL_BUFFER && large.is_none()),
            Some(data) => setup.inbound() || len == 0 || data.len() != len || len > CONTROL_BUFFER,
        };
        if device.control.is_some() || unmade {
            return Err(HwError::Xhci {
                reason: "a control transfer this driver does not make",
            });
        }
        let base = Self::device_base(index);
        let buffer_at = large.unwrap_or(base.saturating_add(CONTROL_DATA));
        if let Some(data) = out {
            self.dma.write(buffer_at, data)?;
        }
        device.control_large = large.is_some();
        let data_stage = if len == 0 {
            DataStage::None
        } else if out.is_some() {
            DataStage::Out
        } else {
            DataStage::In
        };
        let mut ring = device.control_ring;
        self.produce(&mut ring, trb::setup_stage(setup.to_bytes(), data_stage))?;
        let data = if len == 0 {
            None
        } else {
            let buffer = self.bus(buffer_at, len)?;
            Some(self.produce(
                &mut ring,
                trb::data_stage(buffer, setup.length, out.is_none()),
            )?)
        };
        // The Status Stage runs the other way from the data, or IN.
        let status = self.produce(&mut ring, trb::status_stage(len == 0 || out.is_some()))?;
        device.control_ring = ring;
        device.control = Some(Control {
            data,
            status,
            requested: len,
            short: None,
        });
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        self.ring_doorbell(slot, 1)
    }

    /// Copies the last control transfer's data, as [`XhciEvent::Control`]
    /// counted it, into `out`; returns how many bytes.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn control_data(&self, slot: u8, out: &mut [u8]) -> Result<usize, HwError> {
        let index = Self::index_of(slot)?;
        let device = self.device(index)?;
        let (at, room) = match self.large_buffer() {
            Some(at) if device.control_large => (at, CONTROL_BUFFER_LARGE),
            _ => (
                Self::device_base(index).saturating_add(CONTROL_DATA),
                CONTROL_BUFFER,
            ),
        };
        let len = device.control_len.min(out.len()).min(room);
        let target = out.get_mut(..len).unwrap_or_default();
        self.dma.read(at, target)?;
        Ok(len)
    }

    /// Queues [`REPORTS_QUEUED`] report buffers on each of `slot`'s
    /// interrupt endpoints and rings their doorbells.
    ///
    /// # Errors
    ///
    /// As for [`Xhci::address_device`].
    pub fn start_reports(&mut self, slot: u8) -> Result<(), HwError> {
        let index = Self::index_of(slot)?;
        for endpoint in 0..MAX_ENDPOINTS {
            for buffer in 0..REPORTS_QUEUED {
                if !self.queue_report(index, endpoint, buffer)? {
                    break;
                }
            }
            let device = self.device(index)?;
            if let Some(Some(interrupt)) = device.interrupts.get(endpoint) {
                self.ring_doorbell(slot, interrupt.dci)?;
            }
        }
        Ok(())
    }

    /// Queues report buffer `buffer` on device `index`'s interrupt endpoint
    /// `endpoint`; false if it has no such endpoint.
    fn queue_report(
        &mut self,
        index: usize,
        endpoint: usize,
        buffer: usize,
    ) -> Result<bool, HwError> {
        let mut device = self.device(index)?;
        let Some(Some(mut interrupt)) = device.interrupts.get(endpoint).copied() else {
            return Ok(false);
        };
        let at = buffer
            .saturating_mul(REPORT_BUFFER)
            .saturating_add(endpoint.saturating_mul(REPORTS_PER_ENDPOINT))
            .saturating_add(REPORTS)
            .saturating_add(Self::device_base(index));
        let address = self.bus(at, interrupt.max_packet)?;
        let len = u16::try_from(interrupt.max_packet).unwrap_or(0);
        let mut ring = interrupt.ring;
        let trb_index = self.produce(&mut ring, trb::normal(address, len))?;
        interrupt.ring = ring;
        if let Some(pending) = interrupt.pending.get_mut(trb_index) {
            *pending = u8::try_from(buffer).ok();
        }
        if let Some(entry) = device.interrupts.get_mut(endpoint) {
            *entry = Some(interrupt);
        }
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        Ok(true)
    }

    // ---- Isochronous streams -------------------------------------

    /// How many more isochronous endpoints this driver can stream: none
    /// unless its DMA region holds [`DMA_SIZE_STREAMING`] bytes.
    #[must_use]
    pub fn streams_available(&self) -> usize {
        if self.dma.len() < DMA_SIZE_STREAMING {
            return 0;
        }
        self.streams.iter().filter(|entry| entry.is_none()).count()
    }

    /// The stream [`Xhci::configure_endpoints`] set up for `slot`'s
    /// endpoint at address `endpoint`, if any.
    #[must_use]
    pub fn stream_of(&self, slot: u8, endpoint: u8) -> Option<usize> {
        let dci = dci(endpoint)?;
        self.streams
            .iter()
            .position(|entry| entry.is_some_and(|stream| stream.slot == slot && stream.dci == dci))
    }

    fn stream(&self, stream: usize) -> Result<StreamSlot, HwError> {
        self.streams
            .get(stream)
            .copied()
            .flatten()
            .ok_or(HwError::Xhci {
                reason: "a stream this driver has not set up",
            })
    }

    /// Stream `stream`'s counts so far: TDs served and missed, bytes moved.
    #[must_use]
    pub fn stream_stats(&self, stream: usize) -> Option<Stats> {
        self.stream(stream).ok().map(|found| found.queue.stats())
    }

    /// Stream `stream`'s TDs in flight, and the most it can have: one per
    /// buffer.
    #[must_use]
    pub fn stream_queued(&self, stream: usize) -> Option<(usize, usize)> {
        self.stream(stream)
            .ok()
            .map(|found| (found.queue.len(), found.queue.capacity()))
    }

    /// The most bytes one of stream `stream`'s TDs moves: an interval's.
    #[must_use]
    pub fn stream_td_bytes(&self, stream: usize) -> Option<usize> {
        self.stream(stream).ok().map(|found| found.td_bytes)
    }

    /// Queues one TD - one service interval - on stream `stream`: for an
    /// OUT endpoint `data`'s bytes, copied into the TD's buffer first; for
    /// IN, a buffer of an interval's bytes for the device to fill (`data`
    /// is ignored). The doorbell is not rung: ring it once for all the TDs
    /// queued ([`Xhci::ring_stream`]).
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a stream not set up, every buffer in flight,
    /// or OUT data longer than an interval's; any DMA error.
    pub fn queue_isoch(&mut self, stream: usize, data: &[u8]) -> Result<(), HwError> {
        let mut found = self.stream(stream)?;
        let full = HwError::Xhci {
            reason: "a stream with every buffer in flight",
        };
        let buffer = found.queue.next_buffer().ok_or(full)?;
        let at = Self::td_buffer(&found, buffer).ok_or(full)?;
        let len = if found.inbound {
            found.td_bytes
        } else {
            if data.len() > found.td_bytes {
                return Err(HwError::Xhci {
                    reason: "more data than one interval of the stream carries",
                });
            }
            self.dma.write(at, data)?;
            data.len()
        };
        let packets = len.div_ceil(found.max_packet.max(1)).max(1);
        let address = self.bus(at, len.max(1))?;
        let len16 = u16::try_from(len).map_err(|_| full)?;
        let trb = trb::isoch(
            address,
            len16,
            u16::try_from(packets).unwrap_or(u16::MAX),
            found.max_burst,
            found.inbound,
        );
        let mut ring = found.ring;
        let index = self.produce(&mut ring, trb)?;
        found.ring = ring;
        if found.queue.push(index, len16) != Some(buffer) {
            return Err(full);
        }
        if let Some(entry) = self.streams.get_mut(stream) {
            *entry = Some(found);
        }
        Ok(())
    }

    /// Where buffer `buffer` of `stream` starts in the region: `per_page` of
    /// them a page, from its first buffer page.
    fn td_buffer(stream: &StreamSlot, buffer: usize) -> Option<usize> {
        let page = buffer.checked_div(stream.per_page)?;
        let within = buffer.checked_rem(stream.per_page)?;
        page.checked_mul(PAGE)?
            .checked_add(within.checked_mul(stream.td_bytes)?)?
            .checked_add(stream.buffers)
    }

    /// Gives stream `stream` the pool of buffers past
    /// [`DMA_SIZE_STREAMING`] in place of its own pages: one TD a
    /// page, as many as its ring holds - for a camera's video endpoint,
    /// which its own pages cannot keep fed between two ticks. Call it
    /// before any TD is queued on the stream; returns how many TDs it can
    /// then have in flight.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a stream not set up or with TDs in flight, a
    /// DMA region smaller than [`DMA_SIZE_CAMERA`], or a pool another
    /// stream holds.
    pub fn stream_pool(&mut self, stream: usize) -> Result<usize, HwError> {
        let mut found = self.stream(stream)?;
        if self.dma.len() < DMA_SIZE_CAMERA
            || !found.queue.is_empty()
            || self
                .streams
                .iter()
                .flatten()
                .any(|other| other.buffers == POOL_BASE)
        {
            return Err(HwError::Xhci {
                reason: "no pool of buffers for the stream",
            });
        }
        found.buffers = POOL_BASE;
        found.per_page = 1;
        found.queue = Queue::new(POOL_PAGES);
        let capacity = found.queue.capacity();
        if let Some(entry) = self.streams.get_mut(stream) {
            *entry = Some(found);
        }
        Ok(capacity)
    }

    /// Copies what stream `stream`'s IN TD in buffer `buffer` brought -
    /// `len` bytes, as [`XhciEvent::Stream`] named them, at most an
    /// interval's - into `out`; returns how many. Read it before
    /// the next TD queued takes the buffer.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a stream not set up or not IN, or a buffer it
    /// does not have; any DMA error.
    pub fn stream_data(
        &self,
        stream: usize,
        buffer: usize,
        len: usize,
        out: &mut [u8],
    ) -> Result<usize, HwError> {
        let found = self.stream(stream)?;
        let refused = HwError::Xhci {
            reason: "a buffer of no IN stream",
        };
        if !found.inbound || buffer >= found.queue.capacity() {
            return Err(refused);
        }
        let at = Self::td_buffer(&found, buffer).ok_or(refused)?;
        let len = len.min(found.td_bytes).min(out.len());
        self.dma.read(at, out.get_mut(..len).unwrap_or_default())?;
        Ok(len)
    }

    /// Rings stream `stream`'s doorbell: the controller takes the TDs
    /// queued, and an endpoint whose ring ran empty starts again.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a stream not set up.
    pub fn ring_stream(&mut self, stream: usize) -> Result<(), HwError> {
        let found = self.stream(stream)?;
        self.ring_doorbell(found.slot, found.dci)
    }

    /// A Transfer Event on stream `stream`: a TD completed, matched with
    /// its own TRB, or the ring ran empty.
    fn stream_event(
        &mut self,
        stream: usize,
        trb: u64,
        residual: u32,
        code: Completion,
    ) -> Result<XhciEvent, HwError> {
        let refused = |reason| HwError::Xhci { reason };
        let mut found = self.stream(stream)?;
        // An empty ring's event names the TRB where the ring stood, or none
        // (section 4.10.3.1): not a TD.
        if code == Completion::RING_UNDERRUN || code == Completion::RING_OVERRUN {
            found.queue.ran_empty();
            if let Some(entry) = self.streams.get_mut(stream) {
                *entry = Some(found);
            }
            return Ok(XhciEvent::StreamEmpty { stream });
        }
        let ring_address = self.bus(found.ring.base, STREAM_TRBS.saturating_mul(TRB_LEN))?;
        let at = found
            .ring
            .index_of(trb, ring_address)
            .ok_or(refused("a transfer event for a TRB off its stream's ring"))?;
        let residue = usize::try_from(residual).unwrap_or(usize::MAX);
        let done = found
            .queue
            .complete(at, code, residue)
            .map_err(|_| refused("a transfer event for a TD not queued"))?;
        if let Some(entry) = self.streams.get_mut(stream) {
            *entry = Some(found);
        }
        Ok(XhciEvent::Stream {
            stream,
            code,
            len: done.len,
            buffer: done.buffer,
        })
    }

    // ---- Events ------------------------------------------------------------

    /// The next event on the event ring, if the controller has written one:
    /// copied out, decoded, checked against what was queued, and the ring's
    /// dequeue pointer moved on past it. A report's buffer is queued again.
    ///
    /// # Errors
    ///
    /// [`HwError::Xhci`] for a record that is not an event, or an event that
    /// names a command, endpoint or TRB this driver did not queue - the
    /// event is consumed and refused, and the next call reads on.
    pub fn next_event(&mut self) -> Result<Option<XhciEvent>, HwError> {
        let at = self
            .events
            .saturating_mul(TRB_LEN)
            .saturating_add(EVENT_RING);
        // The control dword first: its cycle bit says whether the record is
        // new. Only then, after a barrier, the whole record - read sooner,
        // its first bytes could be from before the controller wrote it.
        let mut control = [0u8; 4];
        self.dma.read(at.saturating_add(12), &mut control)?;
        if u32::from_le_bytes(control) & 1 != u32::from(self.event_cycle) {
            return Ok(None);
        }
        dsb();
        let mut copy = [0u8; TRB_LEN];
        self.dma.read(at, &mut copy)?;
        if Trb::from_bytes(&copy).cycle() != self.event_cycle {
            return Ok(None);
        }
        // Consumed: the next one, and the controller told so.
        self.events = self.events.saturating_add(1);
        if self.events >= EVENT_TRBS {
            self.events = 0;
            self.event_cycle = !self.event_cycle;
        }
        let dequeue = self.bus(
            self.events
                .saturating_mul(TRB_LEN)
                .saturating_add(EVENT_RING),
            TRB_LEN,
        )?;
        // Event Handler Busy is write-1-to-clear (section 5.5.2.3.3).
        self.write_pointer(Pointer::Erdp, dequeue | (1 << 3))?;
        let refused = |reason| HwError::Xhci { reason };
        let event = Event::decode(&copy).map_err(|_| refused("a record that is not an event"))?;
        Ok(Some(match event {
            // The command ring stopped after an abort: its TRB pointer
            // names where the ring stands, which may be no command at all.
            Event::CommandCompletion {
                code: Completion::COMMAND_RING_STOPPED,
                ..
            } => XhciEvent::Ignored,
            Event::CommandCompletion { trb, code, slot } => {
                let ring_address = self.bus(COMMAND_RING, COMMAND_TRBS * TRB_LEN)?;
                self.refused = Some((trb, ring_address));
                let index = self
                    .commands
                    .index_of(trb, ring_address)
                    .ok_or(refused("a completion for a TRB off the command ring"))?;
                self.refused = None;
                let queued = self
                    .queued
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or(refused("a completion for a command never queued"))?;
                XhciEvent::Command {
                    kind: queued.kind,
                    code,
                    slot,
                    id: queued.id,
                }
            }
            Event::PortStatusChange { port } => {
                if self.layout.portsc(port).is_none() {
                    return Err(refused("a port status change for a port it does not have"));
                }
                XhciEvent::Port { port }
            }
            Event::Transfer {
                trb,
                residual,
                code,
                endpoint,
                slot,
            } => self.transfer(trb, residual, code, endpoint, slot)?,
            Event::HostController { code } => XhciEvent::HostController { code },
            Event::Ignored { .. } => XhciEvent::Ignored,
        }))
    }

    /// Matches a Transfer Event with the ring and TRB it names.
    fn transfer(
        &mut self,
        trb: u64,
        residual: u32,
        code: Completion,
        endpoint: u8,
        slot: u8,
    ) -> Result<XhciEvent, HwError> {
        let refused = |reason| HwError::Xhci { reason };
        if let Some(stream) = self.streams.iter().position(|entry| {
            entry.is_some_and(|found| found.slot == slot && found.dci == endpoint)
        }) {
            return self.stream_event(stream, trb, residual, code);
        }
        let index = Self::index_of(slot)?;
        let mut device = self.device(index)?;
        let residual = usize::try_from(residual).unwrap_or(usize::MAX);
        if endpoint == 1 {
            let ring = device.control_ring;
            let ring_address = self.bus(ring.base, RING_BYTES)?;
            let at = ring
                .index_of(trb, ring_address)
                .ok_or(refused("a transfer event for a TRB off the control ring"))?;
            let mut control = device.control.ok_or(refused(
                "a transfer event with no control transfer in flight",
            ))?;
            let event = if Some(at) == control.data && code == Completion::SHORT_PACKET {
                // A short Data Stage: the Status Stage's event follows.
                control.short = Some(control.requested.saturating_sub(residual));
                device.control = Some(control);
                XhciEvent::Ignored
            } else if at == control.status || Some(at) == control.data {
                // The Status Stage completed, or a stage failed: either ends
                // the transfer.
                let len = if code == Completion::SUCCESS && at == control.status {
                    control.short.unwrap_or(control.requested)
                } else {
                    0
                };
                device.control = None;
                device.control_len = len;
                XhciEvent::Control {
                    slot,
                    code: if at == control.status {
                        code
                    } else if code.ok() {
                        Completion::TRB
                    } else {
                        code
                    },
                    len,
                }
            } else if !code.ok() {
                // The Setup Stage failed.
                device.control = None;
                device.control_len = 0;
                XhciEvent::Control { slot, code, len: 0 }
            } else {
                return Err(refused("a transfer event for a Setup Stage that succeeded"));
            };
            if let Some(entry) = self.devices.get_mut(index) {
                *entry = Some(device);
            }
            return Ok(event);
        }
        let number = device
            .interrupts
            .iter()
            .position(|interrupt| interrupt.is_some_and(|interrupt| interrupt.dci == endpoint))
            .ok_or(refused("a transfer event for an endpoint not configured"))?;
        let mut interrupt = device
            .interrupts
            .get(number)
            .copied()
            .flatten()
            .ok_or(refused("a transfer event for an endpoint not configured"))?;
        let ring_address = self.bus(interrupt.ring.base, RING_BYTES)?;
        let at = interrupt
            .ring
            .index_of(trb, ring_address)
            .ok_or(refused("a transfer event for a TRB off its ring"))?;
        let buffer = interrupt
            .pending
            .get_mut(at)
            .and_then(Option::take)
            .ok_or(refused("a transfer event for a TRB not queued"))?;
        if let Some(entry) = device.interrupts.get_mut(number) {
            *entry = Some(interrupt);
        }
        if let Some(entry) = self.devices.get_mut(index) {
            *entry = Some(device);
        }
        if !code.ok() {
            return Ok(XhciEvent::ReportFailed {
                slot,
                endpoint: number,
                code,
            });
        }
        let len = interrupt.max_packet.saturating_sub(residual);
        let mut bytes = [0u8; REPORT_BUFFER];
        let source = usize::from(buffer)
            .saturating_mul(REPORT_BUFFER)
            .saturating_add(number.saturating_mul(REPORTS_PER_ENDPOINT))
            .saturating_add(REPORTS)
            .saturating_add(Self::device_base(index));
        // Copied out before anything looks at it; then the buffer goes back.
        self.dma
            .read(source, bytes.get_mut(..len).unwrap_or_default())?;
        self.queue_report(index, number, usize::from(buffer))?;
        self.ring_doorbell(slot, interrupt.dci)?;
        Ok(XhciEvent::Report {
            slot,
            endpoint: number,
            bytes,
            len,
        })
    }

    // ---- Rings and contexts -----------------------------------------------

    /// The bus address the controller must use for the `len` bytes at
    /// `offset` in the DMA region: their physical address, checked to lie
    /// inside the region, plus where the controller sees memory.
    fn bus(&self, offset: usize, len: usize) -> Result<u64, HwError> {
        self.dma
            .device_address(offset, len)?
            .checked_add(self.bus_offset)
            .ok_or(HwError::Xhci {
                reason: "a bus address past the end of the address space",
            })
    }

    /// The TRB pointer of the last command completion refused for naming no
    /// TRB on the command ring, and where the ring is: for a report.
    #[must_use]
    pub const fn last_refused(&self) -> Option<(u64, u64)> {
        self.refused
    }

    /// CRCR's low word, as the controller reports it - Command Ring Running
    /// is bit 3 - and the DMA region's physical and bus addresses: for a
    /// report when a command does not come back.
    ///
    /// # Errors
    ///
    /// None in practice: CRCR is inside the window.
    pub fn command_ring_state(&mut self) -> Result<(u32, u64, u64), HwError> {
        let crcr = self.read_operational(operational::CRCR)?;
        Ok((crcr, self.dma.paddr(), self.bus(0, 1)?))
    }

    fn device(&self, index: usize) -> Result<DeviceSlot, HwError> {
        self.devices
            .get(index)
            .copied()
            .flatten()
            .ok_or(HwError::Xhci {
                reason: "a slot with no device set up",
            })
    }

    /// The default control endpoint's context for `device`, its ring at
    /// its current position.
    fn control_endpoint(&self, device: &DeviceSlot, max_packet: u16) -> Result<Endpoint, HwError> {
        let ring = device.control_ring;
        let at = ring.offset(ring.enqueue).ok_or(HwError::Xhci {
            reason: "a ring position outside the region",
        })?;
        Ok(Endpoint {
            dci: 1,
            kind: EndpointKind::Control,
            max_packet,
            max_burst: 0,
            interval: 0,
            dequeue: self.bus(at, TRB_LEN)?,
            cycle: ring.cycle,
        })
    }

    /// Lays `input` out in device `index`'s input context page; returns its
    /// bus address.
    fn input_context(&mut self, index: usize, input: &InputContext<'_>) -> Result<u64, HwError> {
        let mut bytes = [0u8; 33 * 64];
        input
            .write(self.context, &mut bytes)
            .map_err(|_| HwError::Xhci {
                reason: "an input context out of range",
            })?;
        let len = self.context.input_len();
        let at = Self::device_base(index).saturating_add(INPUT_CONTEXT);
        self.dma.write(at, bytes.get(..len).unwrap_or_default())?;
        self.bus(at, len)
    }

    /// Queues command `trb` and rings the controller's doorbell; returns
    /// the id its completion will carry.
    fn command(&mut self, trb: Trb) -> Result<CommandId, HwError> {
        let kind = trb.kind();
        let mut ring = self.commands;
        if self.queued.iter().flatten().count() >= COMMAND_TRBS.saturating_sub(2) {
            return Err(HwError::Xhci {
                reason: "the command ring is full",
            });
        }
        let index = self.produce(&mut ring, trb)?;
        self.commands = ring;
        let id = CommandId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        if let Some(slot) = self.queued.get_mut(index) {
            *slot = Some(Queued { kind, id });
        }
        self.ring_doorbell(0, 0)?;
        Ok(id)
    }

    /// Aborts the command the controller is running, one that has not
    /// completed in time (section 4.6.1.2): Command Abort set in CRCR, then
    /// Command Ring Running awaited clear. The command completes as
    /// [`Completion::COMMAND_ABORTED`], with its id, and the ring stops;
    /// then the doorbell starts it again on any command queued after it.
    ///
    /// # Errors
    ///
    /// [`HwError::XhciTimeout`] if the ring does not stop in time.
    pub fn abort_command(&mut self) -> Result<(), HwError> {
        if self.read_operational(operational::CRCR)? & CRCR_RUNNING == 0 {
            return Ok(());
        }
        // While the ring runs the controller ignores CRCR's pointer; should
        // it stop before this write, the pointer is where it stopped - this
        // driver's next TRB - so the write never moves it.
        let ring = self.commands;
        let at = ring.offset(ring.enqueue).ok_or(HwError::Xhci {
            reason: "a ring position outside the region",
        })?;
        let next = self.bus(at, TRB_LEN)?;
        self.write_pointer(Pointer::Crcr, next | CRCR_ABORT | u64::from(ring.cycle))?;
        self.wait(RESET_MS, "the command ring's stop", |xhci| {
            Ok(xhci.read_operational(operational::CRCR)? & CRCR_RUNNING == 0)
        })?;
        self.ring_doorbell(0, 0)
    }

    /// Writes `trb` at `ring`'s enqueue position with the ring's cycle bit,
    /// its first twelve bytes first and then, after a barrier, the control
    /// dword that hands it to the controller; and moves the position on,
    /// following the Link TRB at the end back to the start. Returns the
    /// TRB's index.
    fn produce(&mut self, ring: &mut Ring, trb: Trb) -> Result<usize, HwError> {
        let index = ring.enqueue;
        self.put_trb(ring, index, trb.with_cycle(ring.cycle))?;
        let next = index.saturating_add(1);
        if next >= ring.len.saturating_sub(1) {
            // The Link TRB: back to the start, toggling the cycle state.
            let start = self.bus(ring.base, ring.len.saturating_mul(TRB_LEN))?;
            let link_index = ring.len.saturating_sub(1);
            self.put_trb(
                ring,
                link_index,
                trb::link(start, true).with_cycle(ring.cycle),
            )?;
            ring.enqueue = 0;
            ring.cycle = !ring.cycle;
        } else {
            ring.enqueue = next;
        }
        Ok(index)
    }

    fn put_trb(&mut self, ring: &Ring, index: usize, trb: Trb) -> Result<(), HwError> {
        let at = ring.offset(index).ok_or(HwError::Xhci {
            reason: "a ring position outside the region",
        })?;
        let bytes = trb.to_bytes();
        let (body, control) = bytes.split_at(12);
        self.dma.write(at, body)?;
        dsb();
        self.dma.write(at.saturating_add(12), control)?;
        dsb();
        Ok(())
    }

    /// Writes a 64-bit address into the DMA region, little-endian.
    fn put64(&mut self, at: usize, value: u64) -> Result<(), HwError> {
        self.dma.write(at, &value.to_le_bytes())
    }

    // ---- Registers ---------------------------------------------------------

    /// Busy-waits up to `ms` milliseconds on the counter for `done`.
    fn wait(
        &mut self,
        ms: u64,
        what: &'static str,
        mut done: impl FnMut(&mut Self) -> Result<bool, HwError>,
    ) -> Result<(), HwError> {
        let ticks = counter_frequency().saturating_mul(ms) / 1000;
        let start = counter();
        loop {
            if done(self)? {
                return Ok(());
            }
            if counter().wrapping_sub(start) > ticks {
                return Err(HwError::XhciTimeout { what });
            }
            core::hint::spin_loop();
        }
    }

    /// The register word at byte `at` of the window.
    fn read(&mut self, at: usize) -> Result<u32, HwError> {
        read_word(&mut self.regs, at)
    }

    /// Writes the register word at byte `at` of the window. Only this
    /// module calls it, and only with the offsets of USBCMD, CONFIG, the
    /// interrupter's IMAN, IMOD and ERSTSZ, PORTSC, USB Legacy Support's
    /// semaphore, the 64-bit pointer registers through
    /// [`Xhci::write_pointer`] and the doorbells through
    /// [`Xhci::ring_doorbell`] - each with a value it computed.
    fn write(&mut self, at: usize, value: u32) -> Result<(), HwError> {
        let outside = HwError::Xhci {
            reason: "a register outside the window",
        };
        if !at.is_multiple_of(4) {
            return Err(outside);
        }
        let mut word = self.regs.get(at / 4).ok_or(outside)?;
        // SAFETY: `write_unsafe` needs a write that is safe to make. The
        // index is inside the window (`get`), and the callers are this
        // module's alone (`write` is private), each writing a register the
        // layout located with a value computed here: the writes that let the
        // controller read or write memory - Run/Stop, the command ring,
        // device context array, event ring segment table and dequeue
        // pointers, and the doorbells that start rings - carry or refer to
        // addresses `DmaRegion::device_address` computed inside the DMA
        // region this view owns, so the controller is only ever pointed into
        // it (module documentation). The others - halting, reset, CONFIG,
        // the interrupter's interrupt off, PORTSC's power, reset and change
        // bits, the legacy semaphore - touch no memory.
        unsafe { word.write_unsafe(value) };
        Ok(())
    }

    fn read_operational(&mut self, register: usize) -> Result<u32, HwError> {
        let at = self.layout.operational(register).ok_or(HwError::Xhci {
            reason: "an operational register outside the window",
        })?;
        self.read(at)
    }

    fn write_operational(&mut self, register: usize, value: u32) -> Result<(), HwError> {
        let at = self.layout.operational(register).ok_or(HwError::Xhci {
            reason: "an operational register outside the window",
        })?;
        self.write(at, value)
    }

    fn write_interrupter(&mut self, register: usize, value: u32) -> Result<(), HwError> {
        let at = self.layout.interrupter(register).ok_or(HwError::Xhci {
            reason: "an interrupter register outside the window",
        })?;
        self.write(at, value)
    }

    /// Writes a 64-bit pointer register as two 32-bit writes, the low word
    /// first (section 5.1, for controllers reached without 64-bit
    /// accesses). The address must be the controller's to take: 32 bits
    /// unless it takes 64 (HCCPARAMS1 AC64).
    fn write_pointer(&mut self, register: Pointer, value: u64) -> Result<(), HwError> {
        if !self.caps.addressing_64 && value > u64::from(u32::MAX) {
            return Err(HwError::Xhci {
                reason: "an address above 4 GiB for a 32-bit controller",
            });
        }
        let at = match register {
            Pointer::Crcr => self.layout.operational(operational::CRCR),
            Pointer::Dcbaap => self.layout.operational(operational::DCBAAP),
            Pointer::Erstba => self.layout.interrupter(interrupter::ERSTBA),
            Pointer::Erdp => self.layout.interrupter(interrupter::ERDP),
        }
        .ok_or(HwError::Xhci {
            reason: "a pointer register outside the window",
        })?;
        self.write(at, value as u32)?;
        self.write(at.saturating_add(4), (value >> 32) as u32)
    }

    /// Rings doorbell `doorbell` (0, the controller's, for the command
    /// ring; a slot's for its endpoint `target`).
    fn ring_doorbell(&mut self, doorbell: u8, target: u8) -> Result<(), HwError> {
        let slots = u8::try_from(MAX_DEVICES).unwrap_or(u8::MAX);
        let at = self.layout.doorbell(doorbell, slots).ok_or(HwError::Xhci {
            reason: "a doorbell outside those this driver uses",
        })?;
        // The TRBs it hands over are in memory first (`put_trb`'s barrier).
        self.write(at, u32::from(target))
    }
}

/// The register word at byte `at` of a controller's window.
fn read_word(regs: &mut UniqueMmioPointer<'static, [u32]>, at: usize) -> Result<u32, HwError> {
    let outside = HwError::Xhci {
        reason: "a register outside the window",
    };
    if !at.is_multiple_of(4) {
        return Err(outside);
    }
    let mut word = regs.get(at / 4).ok_or(outside)?;
    // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
    // window (`get` checked the index), and no register of an xHCI
    // controller changes state when read (section 5.1: every register is
    // read-only, read-write, write-1-to-clear or write-1-to-set).
    Ok(unsafe { word.read_unsafe() })
}

/// The 64-bit pointer registers this module writes.
#[derive(Clone, Copy)]
enum Pointer {
    Crcr,
    Dcbaap,
    Erstba,
    Erdp,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    //! The layout and ring logic on ordinary memory. Driving a controller
    //! needs one: QEMU's `qemu-xhci` (`systems/usb-test`) and RP1's on the
    //! Pi 5.

    use super::*;

    #[test]
    fn rings_find_their_own_trbs_only() {
        let ring = Ring::new(0x2000, 16);
        assert_eq!(ring.index_of(0x4000_2000, 0x4000_2000), Some(0));
        assert_eq!(ring.index_of(0x4000_20E0, 0x4000_2000), Some(14));
        // The Link TRB, past the end, before the start, off a boundary.
        assert_eq!(ring.index_of(0x4000_20F0, 0x4000_2000), None);
        assert_eq!(ring.index_of(0x4000_2100, 0x4000_2000), None);
        assert_eq!(ring.index_of(0x4000_1FF0, 0x4000_2000), None);
        assert_eq!(ring.index_of(0x4000_2008, 0x4000_2000), None);
        assert_eq!(ring.offset(3), Some(0x2030));
    }

    #[test]
    fn the_layout_fits_the_region_it_asks_for() {
        assert_eq!(DMA_SIZE, 0x13000);
        // Streaming: four streams of a ring page and five buffer pages,
        // then the large control buffer's 16 KiB.
        assert_eq!(DMA_SIZE_STREAMING, 0x2F000);
        let last = Xhci::device_base(MAX_DEVICES - 1);
        assert_eq!(last + DEVICE_STRIDE, DMA_SIZE);
        assert_eq!(Xhci::index_of(1), Ok(0));
        assert_eq!(Xhci::index_of(4), Ok(3));
        assert!(Xhci::index_of(0).is_err());
        assert!(Xhci::index_of(5).is_err());
    }

    #[test]
    fn a_streams_buffers_lie_in_its_own_pages_or_the_pool() {
        // The camera's pool, 254 pages past the large buffer.
        assert_eq!(DMA_SIZE_CAMERA, 0x2F000 + 254 * PAGE);
        // An audio stream of 192-byte TDs: 21 a page, from the page after
        // its ring's.
        let own = StreamSlot {
            slot: 1,
            dci: 3,
            ring: Ring::new(STREAMS_BASE, STREAM_TRBS),
            inbound: true,
            td_bytes: 192,
            max_packet: 192,
            max_burst: 0,
            per_page: 21,
            buffers: STREAMS_BASE + PAGE,
            queue: Queue::new(105),
        };
        assert_eq!(Xhci::td_buffer(&own, 0), Some(STREAMS_BASE + PAGE));
        assert_eq!(
            Xhci::td_buffer(&own, 22),
            Some(STREAMS_BASE + 2 * PAGE + 192)
        );
        let last = Xhci::td_buffer(&own, 104).unwrap();
        assert!(last + 192 <= STREAMS_BASE + STREAM_STRIDE);
        // A camera's 3 KiB TDs in the pool, one a page, the last inside
        // the region; none crosses a page.
        let pooled = StreamSlot {
            td_bytes: 3072,
            max_packet: 1024,
            max_burst: 2,
            per_page: 1,
            buffers: POOL_BASE,
            queue: Queue::new(POOL_PAGES),
            ..own
        };
        assert_eq!(pooled.queue.capacity(), STREAM_TRBS - 2);
        for buffer in 0..POOL_PAGES {
            let at = Xhci::td_buffer(&pooled, buffer).unwrap();
            assert_eq!(at % PAGE, 0);
            assert!(at + 3072 <= DMA_SIZE_CAMERA);
        }
        assert_eq!(Xhci::td_buffer(&pooled, 1), Some(POOL_BASE + PAGE));
        let none = StreamSlot {
            per_page: 0,
            ..pooled
        };
        assert_eq!(Xhci::td_buffer(&none, 1), None);
    }
}
