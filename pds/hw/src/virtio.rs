// SPDX-License-Identifier: MIT OR Apache-2.0
//! virtio over memory-mapped I/O: finding a device and negotiating its
//! features - the set-up every virtio driver PD (input, network,
//! display) shares.
//!
//! Written from the virtio 1.2 specification (OASIS, "Virtual I/O Device
//! (VIRTIO) Version 1.2", sections 2.1 to 2.2, 3.1 and 4.2): its
//! memory-mapped register layout and its initialisation sequence. Only the
//! modern interface (register version 2) is supported; QEMU's `virt` machine
//! offers it with `-global virtio-mmio.force-legacy=false`.
//!
//! A driver PD is granted one page of virtio-mmio transports - eight of
//! 0x200 bytes on QEMU's `virt` machine - and [`VirtioPage::take`] hands out
//! the transports holding its devices, found by magic value, version and
//! device ID, each once ([`VirtioMmio::find`] for one device).
//! [`VirtioMmio::negotiate`] then runs the handshake of section 3.1.1:
//! reset, ACKNOWLEDGE, DRIVER, the features both sides offer (only those the
//! driver asks for, and always `VIRTIO_F_VERSION_1`), FEATURES_OK, and a
//! check that the device kept FEATURES_OK - setting FAILED when anything
//! goes wrong.
//!
//! The registers that let the device read or write memory - the queue
//! addresses, QueueReady, QueueNotify and the DRIVER_OK status bit - are
//! written only by [`VirtioMmio::set_up_queue`], [`VirtioMmio::driver_ok`]
//! and [`VirtioMmio::notify`], each with one of `virtqueue`'s queues, whose
//! addresses `nowayos-hw` computed inside the queue's own DMA region,
//! or its packet buffers' I/O region. The device-specific
//! configuration space is only read, a byte at a time
//! ([`VirtioMmio::read_config`]: virtio-net's MAC address). The handshake
//! is written against [`Transport`], so host tests run it against a
//! simulated device.

use core::mem::{offset_of, size_of};

use safe_mmio::fields::{ReadPure, WriteOnly};
use safe_mmio::{UniqueMmioPointer, field, field_shared};

use crate::device::RegisterBlock;
use crate::virtqueue::Virtqueue;
use crate::{Device, HwError};

/// `MagicValue`: "virt", little-endian.
pub const MAGIC: u32 = 0x7472_6976;

/// The register version of the modern (virtio 1.x) interface.
pub const VERSION: u32 = 2;

/// Device IDs (section 5): the input device.
pub const DEVICE_INPUT: u32 = 18;

/// Device IDs (section 5): the network card.
pub const DEVICE_NET: u32 = 1;

/// Device IDs (section 5): the console (section 5.3), a serial port.
pub const DEVICE_CONSOLE: u32 = 3;

/// Device IDs (section 5): the entropy device (section 5.4), virtio-rng.
pub const DEVICE_ENTROPY: u32 = 4;

/// `VIRTIO_F_VERSION_1`, feature bit 32: a modern device. Always accepted.
pub const F_VERSION_1: u64 = 1 << 32;

/// Device status bits (section 2.1).
pub mod status {
    /// The driver has noticed the device.
    pub const ACKNOWLEDGE: u32 = 1;
    /// The driver knows how to drive it.
    pub const DRIVER: u32 = 2;
    /// The driver is ready: the device may use its queues.
    pub const DRIVER_OK: u32 = 4;
    /// Feature negotiation is complete.
    pub const FEATURES_OK: u32 = 8;
    /// The device has hit an error and needs a reset.
    pub const DEVICE_NEEDS_RESET: u32 = 64;
    /// The driver has given up on the device.
    pub const FAILED: u32 = 128;
}

/// Transports in one page on QEMU's `virt` machine.
pub const TRANSPORTS: usize = 8;

/// One virtio-mmio transport's registers (section 4.2.2), up to the
/// device-specific configuration space, which this layer does not touch.
#[repr(C)]
pub(crate) struct Registers {
    magic: ReadPure<u32>,
    version: ReadPure<u32>,
    device_id: ReadPure<u32>,
    vendor_id: ReadPure<u32>,
    device_features: ReadPure<u32>,
    device_features_sel: WriteOnly<u32>,
    _reserved_018: [u32; 2],
    driver_features: WriteOnly<u32>,
    driver_features_sel: WriteOnly<u32>,
    _reserved_028: [u32; 2],
    queue_sel: WriteOnly<u32>,
    queue_num_max: ReadPure<u32>,
    queue_num: WriteOnly<u32>,
    _reserved_03c: [u32; 2],
    /// Written only through `write_unsafe`: 1 lets the device use the queue.
    queue_ready: ReadPure<u32>,
    _reserved_048: [u32; 2],
    /// Write-only; written only through `write_unsafe`, never read.
    queue_notify: ReadPure<u32>,
    _reserved_054: [u32; 3],
    interrupt_status: ReadPure<u32>,
    interrupt_ack: WriteOnly<u32>,
    _reserved_068: [u32; 2],
    status: ReadPure<u32>,
    _reserved_074: [u32; 3],
    /// The queue's three device addresses: write-only, written only through
    /// `write_unsafe`, never read.
    queue_desc_low: ReadPure<u32>,
    queue_desc_high: ReadPure<u32>,
    _reserved_088: [u32; 2],
    queue_driver_low: ReadPure<u32>,
    queue_driver_high: ReadPure<u32>,
    _reserved_098: [u32; 2],
    queue_device_low: ReadPure<u32>,
    queue_device_high: ReadPure<u32>,
    _reserved_0a8: [u32; 21],
    config_generation: ReadPure<u32>,
    /// The device-specific configuration space, read a byte at a time
    /// (`read_config`), never written.
    config: [ReadPure<u8>; CONFIG_LEN],
}

/// The bytes of a transport's device-specific configuration space, from
/// offset 0x100 to the end of the transport.
pub const CONFIG_LEN: usize = 0x100;

/// How many times [`VirtioMmio::read_config`] reads the configuration
/// before giving up on it settling.
const CONFIG_ATTEMPTS: usize = 4;

const _: () = {
    assert!(offset_of!(Registers, version) == 0x004);
    assert!(offset_of!(Registers, device_id) == 0x008);
    assert!(offset_of!(Registers, device_features) == 0x010);
    assert!(offset_of!(Registers, device_features_sel) == 0x014);
    assert!(offset_of!(Registers, driver_features) == 0x020);
    assert!(offset_of!(Registers, driver_features_sel) == 0x024);
    assert!(offset_of!(Registers, queue_sel) == 0x030);
    assert!(offset_of!(Registers, queue_num_max) == 0x034);
    assert!(offset_of!(Registers, queue_num) == 0x038);
    assert!(offset_of!(Registers, queue_ready) == 0x044);
    assert!(offset_of!(Registers, queue_notify) == 0x050);
    assert!(offset_of!(Registers, interrupt_status) == 0x060);
    assert!(offset_of!(Registers, interrupt_ack) == 0x064);
    assert!(offset_of!(Registers, status) == 0x070);
    assert!(offset_of!(Registers, queue_desc_low) == 0x080);
    assert!(offset_of!(Registers, queue_driver_low) == 0x090);
    assert!(offset_of!(Registers, queue_device_low) == 0x0a0);
    assert!(offset_of!(Registers, queue_device_high) == 0x0a4);
    assert!(offset_of!(Registers, config_generation) == 0x0fc);
    assert!(offset_of!(Registers, config) == 0x100);
    assert!(size_of::<Registers>() == 0x200);
};

/// A page of transports.
pub(crate) type Page = [Registers; TRANSPORTS];

const _: () = assert!(size_of::<Page>() == 0x1000);

// SAFETY: `repr(C)`; every register at the offset section 4.2.2 gives and 32
// bits wide, and the device-specific configuration space bytes from 0x100
// (asserted above), eight transports of 0x200 bytes to the page. Reads of
// MagicValue, Version, DeviceID, VendorID, DeviceFeatures, QueueNumMax,
// InterruptStatus, QueueReady, Status, ConfigGeneration and the
// configuration space have no side effect (`ReadPure`; section 2.5: the
// driver reads the configuration, which only the device changes, moving
// its generation on). The safe writes - the feature
// selectors, DriverFeatures, QueueSel, QueueNum and InterruptACK
// (`WriteOnly`) - make no device access memory: they choose, size and
// acknowledge, and QueueNum is written only while the queue is not ready.
// Every write that lets the device use memory - Status with DRIVER_OK,
// QueueReady, QueueNotify and the queue addresses - has a `ReadPure` wrapper
// with no safe write, and is written only through `write_unsafe`, in
// `set_status`, `set_up_queue`, `driver_ok` and `notify`. The write-only
// queue registers are never read, and the configuration space is never
// written.
unsafe impl RegisterBlock for Page {}

/// The registers the handshake uses, so that host tests can run it against
/// a simulated device. Nothing here lets a device use memory.
pub trait Transport {
    /// Reads 32 bits of the device's features: `select` 0 for bits 0-31,
    /// 1 for bits 32-63.
    fn device_features(&mut self, select: u32) -> u32;
    /// Writes 32 bits of the features the driver accepts.
    fn set_driver_features(&mut self, select: u32, bits: u32);
    /// Reads the device status.
    fn status(&mut self) -> u32;
    /// Writes the device status: never with [`status::DRIVER_OK`].
    fn set_status(&mut self, bits: u32);
}

/// The page of transports a PD was granted, split into its transports.
#[derive(Debug)]
pub struct VirtioPage<'p> {
    transports: [Option<UniqueMmioPointer<'p, Registers>>; TRANSPORTS],
}

impl VirtioPage<'static> {
    /// The page of transports `device` maps.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device.
    pub fn new(device: Device) -> Result<Self, HwError> {
        Ok(Self::from_view(device.view::<Page>()?))
    }
}

impl<'p> VirtioPage<'p> {
    fn from_view(page: UniqueMmioPointer<'p, Page>) -> Self {
        Self {
            transports: page.split().map(Some),
        }
    }

    /// The first transport, from the page's start, that holds a device with
    /// ID `device_id` and has not been taken yet.
    ///
    /// # Errors
    ///
    /// [`HwError::NoSignature`] if a transport's magic value is not
    /// [`MAGIC`] (the system description bound something else);
    /// [`HwError::VirtioVersion`] if the device is found but only through
    /// the legacy interface; [`HwError::NoVirtioDevice`] if no transport
    /// left holds such a device.
    pub fn take(&mut self, device_id: u32) -> Result<VirtioMmio<'p>, HwError> {
        for (index, slot) in self.transports.iter_mut().enumerate() {
            let Some(transport) = slot.as_ref() else {
                continue;
            };
            let magic = field_shared!(transport, magic).read();
            if magic != MAGIC {
                return Err(HwError::NoSignature {
                    expected: "virtio-mmio",
                    read: u64::from(magic),
                });
            }
            if field_shared!(transport, device_id).read() != device_id {
                continue;
            }
            let version = field_shared!(transport, version).read();
            if version != VERSION {
                return Err(HwError::VirtioVersion { found: version });
            }
            if let Some(regs) = slot.take() {
                return Ok(VirtioMmio {
                    regs,
                    index,
                    device_id,
                });
            }
        }
        Err(HwError::NoVirtioDevice { id: device_id })
    }
}

/// One virtio-mmio transport, holding a device: from [`VirtioPage::take`]
/// or [`VirtioMmio::find`].
#[derive(Debug)]
pub struct VirtioMmio<'p> {
    regs: UniqueMmioPointer<'p, Registers>,
    index: usize,
    device_id: u32,
}

impl VirtioMmio<'static> {
    /// The transport, in the page of transports `device` maps, that holds a
    /// device with ID `device_id`.
    ///
    /// # Errors
    ///
    /// As [`VirtioPage::new`] and [`VirtioPage::take`].
    pub fn find(device: Device, device_id: u32) -> Result<Self, HwError> {
        VirtioPage::new(device)?.take(device_id)
    }
}

impl<'p> VirtioMmio<'p> {
    /// Which of the page's transports the device is on.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The device's ID.
    #[must_use]
    pub const fn device_id(&self) -> u32 {
        self.device_id
    }

    /// Runs the handshake up to FEATURES_OK, accepting the features in
    /// `wanted` that the device offers, and `VIRTIO_F_VERSION_1`. Returns
    /// the accepted features.
    ///
    /// # Errors
    ///
    /// As [`negotiate`].
    pub fn negotiate(&mut self, wanted: u64) -> Result<u64, HwError> {
        negotiate(&mut self.transport(), wanted)
    }

    /// Tells the device where queue `index` is - `queue`'s descriptor table
    /// and rings, in its own DMA region - and makes it ready. Call after
    /// [`VirtioMmio::negotiate`] and before [`VirtioMmio::driver_ok`].
    ///
    /// # Errors
    ///
    /// [`HwError::QueueTooBig`] if the device's queue is smaller than
    /// `queue`'s or does not exist; the queue's own errors for its
    /// addresses.
    pub fn set_up_queue<Q: Virtqueue>(&mut self, index: u32, queue: &Q) -> Result<(), HwError> {
        let addresses = queue.addresses()?;
        let size = queue.size();
        let registers = &mut self.regs;
        field!(*registers, queue_sel).write(index);
        let max = field_shared!(*registers, queue_num_max).read();
        if max == 0 || max < size {
            return Err(HwError::QueueTooBig {
                index,
                wanted: size,
                max,
            });
        }
        if field_shared!(*registers, queue_ready).read() != 0 {
            return Err(HwError::QueueTooBig {
                index,
                wanted: size,
                max: 0,
            });
        }
        field!(*registers, queue_num).write(size);
        let halves = |address: u64| {
            (
                (address & 0xFFFF_FFFF) as u32,
                address.wrapping_shr(32) as u32,
            )
        };
        let (desc_low, desc_high) = halves(addresses.desc);
        let (driver_low, driver_high) = halves(addresses.driver);
        let (device_low, device_high) = halves(addresses.device);
        // Each `write_unsafe` below needs its write to be safe to make. The
        // three addresses are `queue`'s descriptor table, available ring and
        // used ring, which `Virtqueue::addresses` computed with
        // `DmaRegion::device_address`, checked to lie inside the queue's own
        // DMA region - memory the system description maps into this PD only,
        // whose physical address it granted (tools/check-system), and which
        // Rust code reaches only through `DmaRegion`'s atomic copies. Only
        // this crate's queues implement `Virtqueue` (it is sealed). The
        // queue is not ready (checked above), so the device uses none of
        // them until QueueReady is written, last; from then on it reads the
        // descriptors and the available ring and writes the used ring, and
        // reads or writes the buffers the descriptors name: inside the same
        // region (an `RxQueue`'s buffers, a `ChainQueue`'s headers), or a
        // `ChainQueue`'s packet buffers in its `IoRegion` - memory this PD
        // never maps (tools/check-system), each address checked to lie
        // inside the region by `IoRegion::device_address` - so the device
        // touches no memory Rust code reaches except by atomic copies.
        let mut field_desc_low = field!(*registers, queue_desc_low);
        // SAFETY: the descriptor table's address, inside the queue's region,
        // the queue not ready (see above).
        unsafe { field_desc_low.write_unsafe(ReadPure(desc_low)) };
        let mut field_desc_high = field!(*registers, queue_desc_high);
        // SAFETY: as for the low half.
        unsafe { field_desc_high.write_unsafe(ReadPure(desc_high)) };
        let mut field_driver_low = field!(*registers, queue_driver_low);
        // SAFETY: the available ring's address, inside the queue's region,
        // the queue not ready (see above).
        unsafe { field_driver_low.write_unsafe(ReadPure(driver_low)) };
        let mut field_driver_high = field!(*registers, queue_driver_high);
        // SAFETY: as for the low half.
        unsafe { field_driver_high.write_unsafe(ReadPure(driver_high)) };
        let mut field_device_low = field!(*registers, queue_device_low);
        // SAFETY: the used ring's address, inside the queue's region, the
        // queue not ready (see above).
        unsafe { field_device_low.write_unsafe(ReadPure(device_low)) };
        let mut field_device_high = field!(*registers, queue_device_high);
        // SAFETY: as for the low half.
        unsafe { field_device_high.write_unsafe(ReadPure(device_high)) };
        let mut field_ready = field!(*registers, queue_ready);
        // SAFETY: the queue's three addresses, all inside its own region, are
        // written above; from now on the device uses only them and the
        // buffers the descriptors name, inside the same region or the
        // queue's I/O region (see above).
        unsafe { field_ready.write_unsafe(ReadPure(1)) };
        Ok(())
    }

    /// Sets DRIVER_OK: the device may now use the queues set up with
    /// [`VirtioMmio::set_up_queue`].
    pub fn driver_ok(&mut self) {
        let registers = &mut self.regs;
        let now = field_shared!(*registers, status).read();
        // SAFETY: `write_unsafe` needs the write to be safe to make. DRIVER_OK
        // lets the device use the queues that are ready, and only
        // `set_up_queue` makes a queue ready, with addresses inside that
        // queue's own DMA region (see its SAFETY comment); a queue never set
        // up is not ready, so the device uses no other memory.
        unsafe {
            field!(*registers, status).write_unsafe(ReadPure(now | status::DRIVER_OK));
        }
    }

    /// Tells the device that queue `index`'s available ring has new
    /// buffers.
    pub fn notify(&mut self, index: u32) {
        let registers = &mut self.regs;
        let mut notify = field!(*registers, queue_notify);
        // SAFETY: `write_unsafe` needs the write to be safe to make. A
        // notification makes the device look at queue `index`'s available
        // ring; a ready queue's rings and buffers lie inside its own DMA
        // region (`set_up_queue`), and a queue not set up is not ready, so
        // the device ignores it.
        unsafe { notify.write_unsafe(ReadPure(index)) };
    }

    /// Reads and acknowledges the device's interrupt causes (bit 0: a used
    /// buffer; bit 1: its configuration changed).
    pub fn acknowledge_interrupt(&mut self) -> u32 {
        let registers = &mut self.regs;
        let causes = field_shared!(*registers, interrupt_status).read();
        field!(*registers, interrupt_ack).write(causes);
        causes
    }

    /// Copies `out.len()` bytes of the device-specific configuration space
    /// from `offset` into `out`, a byte at a time, until two reads of
    /// ConfigGeneration around the copy agree (section 2.5.1). Byte reads
    /// suit only fields of 8-bit elements, such as virtio-net's `mac`
    /// (section 4.2.2.2).
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the bytes are not all inside the
    /// configuration space; [`HwError::ConfigUnstable`] if the generation
    /// moved on every attempt.
    pub fn read_config(&mut self, offset: usize, out: &mut [u8]) -> Result<(), HwError> {
        let end = offset
            .checked_add(out.len())
            .filter(|end| *end <= CONFIG_LEN)
            .ok_or(HwError::OutsideRegion {
                offset,
                len: out.len(),
                size: CONFIG_LEN,
            })?;
        let registers = &self.regs;
        for _ in 0..CONFIG_ATTEMPTS {
            let before = field_shared!(*registers, config_generation).read();
            let config = field_shared!(*registers, config);
            for (byte, at) in out.iter_mut().zip(offset..end) {
                *byte = config.get(at).map_or(0, |cell| cell.read());
            }
            if field_shared!(*registers, config_generation).read() == before {
                return Ok(());
            }
        }
        Err(HwError::ConfigUnstable)
    }

    fn transport(&mut self) -> MmioTransport<'_, 'p> {
        MmioTransport {
            regs: &mut self.regs,
        }
    }
}

/// The handshake of section 3.1.1 up to FEATURES_OK, against any
/// [`Transport`]: reset, ACKNOWLEDGE, DRIVER, accept the features in
/// `wanted` that the device offers plus `VIRTIO_F_VERSION_1`, FEATURES_OK,
/// and check the device kept it. Returns the accepted features.
///
/// # Errors
///
/// [`HwError::FeaturesRefused`] if the device does not offer
/// `VIRTIO_F_VERSION_1` or clears FEATURES_OK; [`HwError::DeviceFailed`] if
/// it asks for a reset. Either way FAILED is set first.
pub fn negotiate<T: Transport>(transport: &mut T, wanted: u64) -> Result<u64, HwError> {
    transport.set_status(0);
    transport.set_status(status::ACKNOWLEDGE);
    transport.set_status(status::ACKNOWLEDGE | status::DRIVER);
    let offered = u64::from(transport.device_features(0))
        | u64::from(transport.device_features(1)).wrapping_shl(32);
    let accepted = (wanted | F_VERSION_1) & offered;
    if accepted & F_VERSION_1 == 0 {
        transport.set_status(status::FAILED);
        return Err(HwError::FeaturesRefused { offered, accepted });
    }
    // The low, then the high, 32 bits.
    transport.set_driver_features(0, (accepted & 0xFFFF_FFFF) as u32);
    transport.set_driver_features(1, accepted.wrapping_shr(32) as u32);
    transport.set_status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK);
    let now = transport.status();
    if now & status::DEVICE_NEEDS_RESET != 0 {
        transport.set_status(status::FAILED);
        return Err(HwError::DeviceFailed { status: now });
    }
    if now & status::FEATURES_OK == 0 {
        transport.set_status(status::FAILED);
        return Err(HwError::FeaturesRefused { offered, accepted });
    }
    Ok(accepted)
}

/// A real transport's registers, for the handshake.
struct MmioTransport<'a, 'p> {
    regs: &'a mut UniqueMmioPointer<'p, Registers>,
}

impl Transport for MmioTransport<'_, '_> {
    fn device_features(&mut self, select: u32) -> u32 {
        field!(*self.regs, device_features_sel).write(select);
        field_shared!(*self.regs, device_features).read()
    }

    fn set_driver_features(&mut self, select: u32, bits: u32) {
        field!(*self.regs, driver_features_sel).write(select);
        field!(*self.regs, driver_features).write(bits);
    }

    fn status(&mut self) -> u32 {
        field_shared!(*self.regs, status).read()
    }

    fn set_status(&mut self, bits: u32) {
        let bits = bits & !status::DRIVER_OK;
        let mut status = field!(*self.regs, status);
        // SAFETY: `write_unsafe` needs the write to be safe to make. A status
        // write can let the device start using its queues only with
        // DRIVER_OK, which is masked out above; every other bit - reset,
        // ACKNOWLEDGE, DRIVER, FEATURES_OK, FAILED - changes the handshake's
        // state and makes no device access memory.
        unsafe { status.write_unsafe(ReadPure(bits)) };
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod handshake_tests {
    //! The handshake against simulated devices: no registers, so these run
    //! under Miri too.

    extern crate std;

    use std::vec::Vec;

    use super::*;

    /// A simulated device: offers `offered`; keeps FEATURES_OK unless told
    /// not to; may ask for a reset.
    struct Simulated {
        offered: u64,
        keeps_features_ok: bool,
        needs_reset: bool,
        status: u32,
        accepted: [u32; 2],
        writes: Vec<u32>,
    }

    impl Simulated {
        fn new(offered: u64) -> Self {
            Self {
                offered,
                keeps_features_ok: true,
                needs_reset: false,
                status: 0,
                accepted: [0; 2],
                writes: Vec::new(),
            }
        }
    }

    impl Transport for Simulated {
        fn device_features(&mut self, select: u32) -> u32 {
            match select {
                0 => (self.offered & 0xFFFF_FFFF) as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            }
        }

        fn set_driver_features(&mut self, select: u32, bits: u32) {
            self.accepted[select as usize] = bits;
        }

        fn status(&mut self) -> u32 {
            self.status
        }

        fn set_status(&mut self, bits: u32) {
            assert_eq!(
                bits & status::DRIVER_OK,
                0,
                "the handshake never sets DRIVER_OK"
            );
            self.writes.push(bits);
            self.status = bits;
            if bits & status::FEATURES_OK != 0 && !self.keeps_features_ok {
                self.status &= !status::FEATURES_OK;
            }
            if self.needs_reset && bits != 0 {
                self.status |= status::DEVICE_NEEDS_RESET;
            }
        }
    }

    const EVENT_IDX: u64 = 1 << 29;
    const RING_RESET: u64 = 1 << 40;

    #[test]
    fn only_the_features_asked_for_and_offered_are_accepted() {
        let mut device = Simulated::new(F_VERSION_1 | EVENT_IDX | RING_RESET | 0b1010);
        let accepted = negotiate(&mut device, 0b0011 | EVENT_IDX).unwrap();
        assert_eq!(accepted, F_VERSION_1 | EVENT_IDX | 0b0010);
        assert_eq!(
            u64::from(device.accepted[0]) | (u64::from(device.accepted[1]) << 32),
            accepted
        );
        assert_eq!(
            device.writes,
            [
                0,
                status::ACKNOWLEDGE,
                status::ACKNOWLEDGE | status::DRIVER,
                status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
            ],
            "reset, ACKNOWLEDGE, DRIVER, FEATURES_OK: section 3.1.1's order"
        );
    }

    #[test]
    fn a_legacy_only_device_is_refused() {
        let mut device = Simulated::new(EVENT_IDX);
        assert_eq!(
            negotiate(&mut device, EVENT_IDX),
            Err(HwError::FeaturesRefused {
                offered: EVENT_IDX,
                accepted: EVENT_IDX
            })
        );
        assert_eq!(device.status, status::FAILED);
        assert!(device.writes.iter().all(|&s| s & status::FEATURES_OK == 0));
    }

    #[test]
    fn a_device_that_refuses_the_features_fails_cleanly() {
        let mut device = Simulated::new(F_VERSION_1 | EVENT_IDX);
        device.keeps_features_ok = false;
        assert!(matches!(
            negotiate(&mut device, EVENT_IDX),
            Err(HwError::FeaturesRefused { .. })
        ));
        assert_eq!(device.status, status::FAILED);
    }

    #[test]
    fn a_device_that_needs_a_reset_fails_cleanly() {
        let mut device = Simulated::new(F_VERSION_1);
        device.needs_reset = true;
        assert!(matches!(
            negotiate(&mut device, 0),
            Err(HwError::DeviceFailed { .. })
        ));
        assert_eq!(device.status & status::FAILED, status::FAILED);
    }
}

#[cfg(all(test, not(miri)))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod transport_tests {
    //! Ordinary memory stands in for a page of transports, laid out as on
    //! QEMU's `virt` machine. No device runs, so these check the search
    //! and the registers the handshake touches.

    use super::*;

    fn transport(magic: u32, version: u32, device_id: u32) -> Registers {
        Registers {
            magic: ReadPure(magic),
            version: ReadPure(version),
            device_id: ReadPure(device_id),
            vendor_id: ReadPure(0x554D_4551),
            device_features: ReadPure(0),
            device_features_sel: WriteOnly(0),
            _reserved_018: [0; 2],
            driver_features: WriteOnly(0),
            driver_features_sel: WriteOnly(0),
            _reserved_028: [0; 2],
            queue_sel: WriteOnly(0),
            queue_num_max: ReadPure(64),
            queue_num: WriteOnly(0),
            _reserved_03c: [0; 2],
            queue_ready: ReadPure(0),
            _reserved_048: [0; 2],
            queue_notify: ReadPure(0),
            _reserved_054: [0; 3],
            interrupt_status: ReadPure(0),
            interrupt_ack: WriteOnly(0),
            _reserved_068: [0; 2],
            status: ReadPure(0),
            _reserved_074: [0; 3],
            queue_desc_low: ReadPure(0),
            queue_desc_high: ReadPure(0),
            _reserved_088: [0; 2],
            queue_driver_low: ReadPure(0),
            queue_driver_high: ReadPure(0),
            _reserved_098: [0; 2],
            queue_device_low: ReadPure(0),
            queue_device_high: ReadPure(0),
            _reserved_0a8: [0; 21],
            config_generation: ReadPure(0),
            config: core::array::from_fn(|at| ReadPure(at as u8)),
        }
    }

    /// QEMU's page of transports 24 to 31, with a device of ID `device_id`
    /// on each transport in `on` (QEMU places the first `-device` on
    /// transport 31, the next on 30).
    fn page(magic: u32, version: u32, device_id: u32, on: &[usize]) -> Page {
        core::array::from_fn(|i| {
            transport(magic, version, if on.contains(&i) { device_id } else { 0 })
        })
    }

    #[test]
    fn devices_are_found_by_magic_version_and_id_each_once() {
        let mut page = page(MAGIC, VERSION, DEVICE_INPUT, &[6, 7]);
        let mut transports = VirtioPage::from_view(UniqueMmioPointer::from(&mut page));
        assert_eq!(transports.take(DEVICE_INPUT).map(|t| t.index()), Ok(6));
        assert_eq!(transports.take(DEVICE_INPUT).map(|t| t.index()), Ok(7));
        assert_eq!(
            transports.take(DEVICE_INPUT).map(|t| t.index()),
            Err(HwError::NoVirtioDevice { id: DEVICE_INPUT })
        );
        assert_eq!(
            transports.take(DEVICE_NET).map(|t| t.index()),
            Err(HwError::NoVirtioDevice { id: DEVICE_NET })
        );
    }

    #[test]
    fn a_legacy_transport_or_another_device_is_refused() {
        let mut legacy = page(MAGIC, 1, DEVICE_INPUT, &[7]);
        assert_eq!(
            VirtioPage::from_view(UniqueMmioPointer::from(&mut legacy))
                .take(DEVICE_INPUT)
                .map(|t| t.index()),
            Err(HwError::VirtioVersion { found: 1 })
        );
        let mut other = page(0x1234_5678, VERSION, DEVICE_INPUT, &[7]);
        assert!(matches!(
            VirtioPage::from_view(UniqueMmioPointer::from(&mut other))
                .take(DEVICE_INPUT)
                .map(|t| t.index()),
            Err(HwError::NoSignature { .. })
        ));
    }

    #[test]
    fn the_configuration_space_is_read_a_byte_at_a_time() {
        let mut page = page(MAGIC, VERSION, DEVICE_NET, &[7]);
        let mut transports = VirtioPage::from_view(UniqueMmioPointer::from(&mut page));
        let mut net = transports.take(DEVICE_NET).unwrap();
        let mut mac = [0u8; 6];
        net.read_config(0, &mut mac).unwrap();
        assert_eq!(mac, [0, 1, 2, 3, 4, 5], "config bytes 0 to 5");
        let mut last = [0u8; 2];
        net.read_config(CONFIG_LEN - 2, &mut last).unwrap();
        assert_eq!(last, [0xfe, 0xff]);
        assert_eq!(
            net.read_config(CONFIG_LEN - 1, &mut last),
            Err(HwError::OutsideRegion {
                offset: CONFIG_LEN - 1,
                len: 2,
                size: CONFIG_LEN
            })
        );
    }

    #[test]
    fn the_status_register_never_gets_driver_ok_from_the_handshake() {
        let mut page = page(MAGIC, VERSION, DEVICE_INPUT, &[7]);
        let mut transports = VirtioPage::from_view(UniqueMmioPointer::from(&mut page));
        let mut input = transports.take(DEVICE_INPUT).unwrap();
        let mut transport = input.transport();
        transport.set_status(status::DRIVER_OK | status::ACKNOWLEDGE);
        assert_eq!(transport.status(), status::ACKNOWLEDGE);
    }
}
