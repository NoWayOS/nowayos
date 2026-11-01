// SPDX-License-Identifier: MIT OR Apache-2.0
//! A Cadence GEM Ethernet MAC: its registers, and its descriptor rings in
//! the DMA region this view owns. On the
//! Raspberry Pi 5 the MAC is RP1's GEM_GXL, reached over PCIe as U-Boot's
//! enumeration left it, polled (no interrupt reaches the Pi's CPU).
//!
//! Written from AMD's *Zynq UltraScale+ Device TRM* (UG1085, chapter 34) and
//! *Register Reference* (UG1087, the GEM module), a GEM_GXL of the same
//! module as RP1's (*RP1 Peripherals*, chapter 7): see `nowayos-gem`, which
//! holds every rule here that needs no hardware - the register values, MDIO
//! frames, the PHY's link and the rings' checks. No driver source was read.
//!
//! **Everything the MAC is given lies in this PD's memory or its client's
//! buffers.** The rings are in the DMA region; the buffers are the client's
//! receive and transmit data regions ([`IoRegion`]s: this PD knows their
//! physical addresses and never maps them). Every address the MAC
//! is told - the rings' bases, each buffer's - is computed here by
//! [`DmaRegion::device_address`] or [`IoRegion::device_address`], inside
//! its region, plus the board's DMA offset for the MAC's bus (on the Pi 5,
//! RP1 sees DRAM from 0x10_0000_0000). The registers that start DMA
//! (`receive_q_ptr`, `transmit_q_ptr` and their upper halves, and
//! `network_control`'s enables and transmit start) are written only by this
//! module, with those addresses.
//!
//! **Nothing the MAC writes is trusted:** every descriptor word is copied
//! out of the region and checked by `nowayos_gem::ring` before use, and the
//! buffer handed back is always this module's record, never an address the
//! MAC wrote. The MAC's own DMA is not confined - RP1's is not - so
//! the MAC is trusted to follow its rings, as any DMA master is; the
//! network is not.
//!
//! The registers are viewed as plain 32-bit words ([`Device::view_words`]),
//! each access at an offset `nowayos_gem::regs` names, checked against the
//! window first, and each states why it is safe. No register read here
//! changes state when read (`int_status`, which may clear on read, is only
//! written; the statistics registers are never read).

use nowayos_gem::regs::{
    self, DMA_CONFIG_VALUE, GEM_GXL_MODULE, control, module_and_revision, network_config,
    station_address, status,
};
use nowayos_gem::ring::{DescriptorMemory, Received, RxRing, Sent, TxRing};
use nowayos_gem::{mdio, phy};
use nowayos_net::BUFFER_SIZE;
use safe_mmio::UniqueMmioPointer;

use crate::cpu;
use crate::{Device, DmaRegion, HwError, IoRegion};

/// Slots in each ring: one per buffer of the client's data region.
pub const SLOTS: usize = nowayos_net::BUFFERS;

/// Where the receive ring is in the DMA region.
const RX_RING: usize = 0;
/// Where the transmit ring is.
const TX_RING: usize = RxRing::<SLOTS>::BYTES;
/// The DMA region's least size: both rings.
pub const DMA_SIZE: usize = TX_RING + TxRing::<SLOTS>::BYTES;

/// How long an MDIO frame may take, in microseconds: a 64-bit frame at the
/// slowest MDC IEEE 802.3 allows (no minimum) is the PHY's business, so
/// this is generous for the board's 2 MHz or so.
const MDIO_TIMEOUT_US: u64 = 10_000;

/// The self-test frame's length: the shortest Ethernet frame without its
/// FCS, which the MAC appends on the way out and removes on the way in.
pub const SELF_TEST_LEN: usize = 60;

/// Where the self-test receives, in its scratch region: a buffer away from
/// the frame it sends.
const SELF_TEST_RX: usize = BUFFER_SIZE;

/// The id the rings give the self-test's buffers, which no client buffer
/// has (theirs are offsets in a 256 KiB region).
const SELF_TEST_BUFFER: usize = usize::MAX;

/// How long the self-test waits for its frame, in microseconds.
const SELF_TEST_TIMEOUT_US: u64 = 20_000;

/// The self-test frame: to the station address (so the MAC's filter takes
/// it), from a locally administered address, the IEEE local experimental
/// EtherType 0x88B5, and a counting payload.
fn self_test_frame(station: [u8; 6]) -> [u8; SELF_TEST_LEN] {
    let mut frame = [0u8; SELF_TEST_LEN];
    for (index, byte) in frame.iter_mut().enumerate() {
        *byte = (index & 0xFF) as u8;
    }
    let [a, b, c, d, e, f] = station;
    let header = [a, b, c, d, e, f, 0x02, 0, 0, 0, 0, 0x01, 0x88, 0xB5];
    if let Some(start) = frame.get_mut(..header.len()) {
        start.copy_from_slice(&header);
    }
    frame
}

/// What [`Gem::loopback_self_test`] saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelfTest {
    /// The MAC reported the frame sent without error.
    pub sent: bool,
    /// What the receive ring gave back, if anything.
    pub received: Option<Received>,
    /// It came back whole and byte for byte.
    pub matched: bool,
    /// `receive_status` afterwards.
    pub rx_status: u32,
    /// `transmit_status` afterwards.
    pub tx_status: u32,
}

/// The MAC as identified when the view was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// `revision_reg`'s revision (RP1's: 0x0109, GEM_GXL 1p09).
    pub revision: u32,
    /// `designcfg_debug6`, as read.
    pub design: u32,
    /// `network_config` as found.
    pub config: u32,
    /// `dma_config` as found.
    pub dma: u32,
}

/// The rings' memory: the DMA region, as `nowayos-gem` reads and writes
/// it, with a DSB for each barrier.
#[derive(Debug)]
struct Rings(DmaRegion);

impl DescriptorMemory for Rings {
    fn read_word(&self, offset: usize) -> Option<u32> {
        let mut bytes = [0u8; 4];
        self.0.read(offset, &mut bytes).ok()?;
        Some(u32::from_le_bytes(bytes))
    }

    fn write_word(&mut self, offset: usize, value: u32) -> Option<()> {
        self.0.write(offset, &value.to_le_bytes()).ok()
    }

    fn barrier(&self) {
        cpu::dsb();
    }
}

/// A GEM MAC, its registers, its rings and the client's buffers. Make one
/// with [`Gem::new`], then [`Gem::reset`] it.
#[derive(Debug)]
pub struct Gem {
    regs: UniqueMmioPointer<'static, [u32]>,
    rings: Rings,
    rx: RxRing<SLOTS>,
    tx: TxRing<SLOTS>,
    rx_buffers: IoRegion,
    tx_buffers: IoRegion,
    bus_offset: u64,
    identity: Identity,
    station: [u8; 6],
}

impl Gem {
    /// Views `device` as a GEM MAC with `dma` for its rings and the client's
    /// `rx_buffers` and `tx_buffers`, at bus address physical plus
    /// `bus_offset`: reads and checks its identity - a GEM_GXL, able to take
    /// 64-bit addresses, with one queue - and writes nothing.
    ///
    /// # Errors
    ///
    /// Any error of viewing the device; [`HwError::Gem`] for a window too
    /// small for the registers used, a MAC this driver does not drive, or a
    /// DMA region smaller than [`DMA_SIZE`].
    pub fn new(
        device: Device,
        dma: DmaRegion,
        rx_buffers: IoRegion,
        tx_buffers: IoRegion,
        bus_offset: u64,
    ) -> Result<Self, HwError> {
        if device.size() < regs::WINDOW_USED {
            return Err(HwError::Gem {
                reason: "a window smaller than the registers this driver uses",
            });
        }
        if dma.len() < DMA_SIZE {
            return Err(HwError::Gem {
                reason: "a DMA region smaller than the two rings",
            });
        }
        let mut regs = device.view_words()?;
        let (module, revision) = module_and_revision(read_word(&mut regs, regs::REVISION)?);
        let design = read_word(&mut regs, regs::DESIGNCFG_DEBUG6)?;
        if module != GEM_GXL_MODULE {
            return Err(HwError::Gem {
                reason: "not a GEM_GXL (its revision register's module is not 7)",
            });
        }
        if design & regs::DESIGN_ADDRESS_64 == 0 {
            return Err(HwError::Gem {
                reason: "a MAC that cannot take 64-bit addresses",
            });
        }
        if design & regs::DESIGN_PRIORITY_QUEUES != 0 {
            return Err(HwError::Gem {
                reason: "a MAC with priority queues, which this driver would have to park",
            });
        }
        let config = read_word(&mut regs, regs::NETWORK_CONFIG)?;
        let dma_found = read_word(&mut regs, regs::DMA_CONFIG)?;
        Ok(Self {
            regs,
            rings: Rings(dma),
            rx: RxRing::new(RX_RING),
            tx: TxRing::new(TX_RING),
            rx_buffers,
            tx_buffers,
            bus_offset,
            station: [0; 6],
            identity: Identity {
                revision,
                design,
                config,
                dma: dma_found,
            },
        })
    }

    /// The MAC as identified by [`Gem::new`].
    #[must_use]
    pub const fn identity(&self) -> Identity {
        self.identity
    }

    /// Resets the MAC to a known state and sets it up, receiving and
    /// transmitting nothing yet: receive and transmit off, every interrupt
    /// masked and cleared (the driver polls), the statistics cleared, the
    /// address hash empty, `network_config` for 1000 Mb/s full duplex until
    /// the PHY says otherwise ([`Gem::set_link`]), `dma_config` as
    /// `nowayos-gem` computes it, `mac` as the station address, both rings
    /// empty and their bases given, and the MDIO port on.
    ///
    /// # Errors
    ///
    /// [`HwError::GemRing`] if a ring does not fit the DMA region, or
    /// [`HwError::OutsideRegion`] for a ring's address.
    pub fn reset(&mut self, mac: [u8; 6]) -> Result<(), HwError> {
        self.write(regs::NETWORK_CONTROL, 0)?;
        self.write(regs::INT_DISABLE, u32::MAX)?;
        self.write(regs::INT_STATUS, u32::MAX)?;
        self.write(regs::TRANSMIT_STATUS, u32::MAX)?;
        self.write(regs::RECEIVE_STATUS, u32::MAX)?;
        self.write(regs::NETWORK_CONTROL, control::CLEAR_STATS)?;
        self.write(regs::HASH_BOTTOM, 0)?;
        self.write(regs::HASH_TOP, 0)?;
        self.write(
            regs::NETWORK_CONFIG,
            network_config(
                self.identity.config,
                phy::Speed::Mbps1000,
                phy::Duplex::Full,
            ),
        )?;
        self.write(regs::DMA_CONFIG, DMA_CONFIG_VALUE)?;
        self.station = mac;
        let (bottom, top) = station_address(mac);
        self.write(regs::SPEC_ADD1_BOTTOM, bottom)?;
        self.write(regs::SPEC_ADD1_TOP, top)?;
        self.rx.init(&mut self.rings).map_err(HwError::GemRing)?;
        self.tx.init(&mut self.rings).map_err(HwError::GemRing)?;
        let rx_base = self.ring_address(RX_RING, RxRing::<SLOTS>::BYTES)?;
        let tx_base = self.ring_address(TX_RING, TxRing::<SLOTS>::BYTES)?;
        // The rings are in memory before the MAC is told where.
        cpu::dsb();
        self.write(regs::UPPER_RX_Q_BASE, (rx_base >> 32) as u32)?;
        self.write(regs::RECEIVE_Q_PTR, rx_base as u32)?;
        self.write(regs::UPPER_TX_Q_BASE, (tx_base >> 32) as u32)?;
        self.write(regs::TRANSMIT_Q_PTR, tx_base as u32)?;
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE)
    }

    /// Reads register `register` of the PHY at `phy` (clause 22).
    ///
    /// # Errors
    ///
    /// [`HwError::GemTimeout`] if the management port does not finish;
    /// [`HwError::Mdio`] for an address over five bits or a reply no PHY
    /// drove.
    pub fn mdio_read(&mut self, phy: u8, register: u8) -> Result<u16, HwError> {
        let frame = mdio::read_frame(phy, register).map_err(HwError::Mdio)?;
        let reply = self.mdio(frame)?;
        mdio::read_reply(reply).map_err(HwError::Mdio)
    }

    /// Writes `data` to register `register` of the PHY at `phy` (clause
    /// 22).
    ///
    /// # Errors
    ///
    /// As [`Gem::mdio_read`], but for the reply.
    pub fn mdio_write(&mut self, phy: u8, register: u8, data: u16) -> Result<(), HwError> {
        let frame = mdio::write_frame(phy, register, data).map_err(HwError::Mdio)?;
        self.mdio(frame).map(|_| ())
    }

    /// Sends one management frame and returns `phy_management` once it is
    /// over.
    fn mdio(&mut self, frame: u32) -> Result<u32, HwError> {
        self.wait_mdio_idle()?;
        self.write(regs::PHY_MANAGEMENT, frame)?;
        self.wait_mdio_idle()?;
        self.read(regs::PHY_MANAGEMENT)
    }

    /// Waits for `network_status`'s `man_done`, for at most
    /// [`MDIO_TIMEOUT_US`].
    fn wait_mdio_idle(&mut self) -> Result<(), HwError> {
        let ticks = MDIO_TIMEOUT_US
            .saturating_mul(cpu::counter_frequency())
            .checked_div(1_000_000)
            .unwrap_or(0);
        let start = cpu::counter();
        loop {
            if self.read(regs::NETWORK_STATUS)? & status::MDIO_IDLE != 0 {
                return Ok(());
            }
            if cpu::counter().saturating_sub(start) > ticks {
                return Err(HwError::GemTimeout {
                    what: "a PHY management frame",
                });
            }
        }
    }

    /// Sets the MAC's speed and duplex to the link's, with receive and
    /// transmit off while it changes, then on.
    ///
    /// # Errors
    ///
    /// None in practice: a register outside the window, which [`Gem::new`]
    /// checked.
    pub fn set_link(&mut self, speed: phy::Speed, duplex: phy::Duplex) -> Result<(), HwError> {
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE)?;
        self.write(
            regs::NETWORK_CONFIG,
            network_config(self.identity.config, speed, duplex),
        )?;
        self.write(
            regs::NETWORK_CONTROL,
            control::MDIO_ENABLE | control::RX_ENABLE | control::TX_ENABLE,
        )
    }

    /// Stops receiving and transmitting (the link went down). The receive
    /// ring keeps its buffers. Turning the transmitter off returns the MAC
    /// to the transmit ring's base (UG1087, `enable_transmit`), so the
    /// frames still queued are given up: take their buffers back with
    /// [`Gem::abandon`] until it says none, then [`Gem::restart_tx`].
    ///
    /// # Errors
    ///
    /// As [`Gem::set_link`].
    pub fn stop(&mut self) -> Result<(), HwError> {
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE)
    }

    /// The buffer of a frame still queued after [`Gem::stop`], given up;
    /// `Ok(None)` once there are none.
    ///
    /// # Errors
    ///
    /// [`HwError::GemRing`] as the ring's.
    pub fn abandon(&mut self) -> Result<Option<usize>, HwError> {
        self.tx.abandon(&mut self.rings).map_err(HwError::GemRing)
    }

    /// Sets the emptied transmit ring up again and tells the MAC its base,
    /// as [`Gem::reset`] did, with the transmitter still off.
    ///
    /// # Errors
    ///
    /// [`HwError::GemRing`] or [`HwError::OutsideRegion`] as in
    /// [`Gem::reset`].
    pub fn restart_tx(&mut self) -> Result<(), HwError> {
        self.tx.init(&mut self.rings).map_err(HwError::GemRing)?;
        let tx_base = self.ring_address(TX_RING, TxRing::<SLOTS>::BYTES)?;
        cpu::dsb();
        self.write(regs::UPPER_TX_Q_BASE, (tx_base >> 32) as u32)?;
        self.write(regs::TRANSMIT_Q_PTR, tx_base as u32)
    }

    /// Tells the MAC the receive ring's base again, its slots emptied: with
    /// receive off, as `receive_q_ptr` takes writes only then (UG1087).
    fn restart_rx(&mut self) -> Result<(), HwError> {
        self.rx.init(&mut self.rings).map_err(HwError::GemRing)?;
        let rx_base = self.ring_address(RX_RING, RxRing::<SLOTS>::BYTES)?;
        cpu::dsb();
        self.write(regs::UPPER_RX_Q_BASE, (rx_base >> 32) as u32)?;
        self.write(regs::RECEIVE_Q_PTR, rx_base as u32)
    }

    /// A self-test of the MAC's DMA and both rings, with no link and no
    /// PHY: sends one [`SELF_TEST_LEN`]-byte frame from `scratch` with
    /// transmit looped back to receive inside the MAC (`loopback_local`,
    /// which a GEM may not have), receives it into `scratch` too, and
    /// compares. Call it after [`Gem::reset`] and before the client's
    /// buffers are offered: it leaves receive and transmit off, loopback
    /// off and both rings empty, as [`Gem::reset`] did. `scratch` is a DMA
    /// region of the PD's own, at least a page.
    ///
    /// # Errors
    ///
    /// [`HwError::Gem`] for a scratch region under a page;
    /// [`HwError::GemRing`] or a register error as the other methods'.
    pub fn loopback_self_test(&mut self, scratch: &mut DmaRegion) -> Result<SelfTest, HwError> {
        self.looped_frame(scratch, true)
    }

    /// The same self-test through the PHY, which the caller has put in
    /// loopback (clause 22's BMCR bit 14) at `speed` and `duplex`: the frame
    /// crosses the RGMII bus out and back, so its clock delays are tested.
    /// The MAC is set to `speed` and `duplex` first; it is left as
    /// [`Gem::loopback_self_test`] leaves it.
    ///
    /// # Errors
    ///
    /// As [`Gem::loopback_self_test`].
    pub fn phy_loopback_self_test(
        &mut self,
        scratch: &mut DmaRegion,
        speed: phy::Speed,
        duplex: phy::Duplex,
    ) -> Result<SelfTest, HwError> {
        self.write(
            regs::NETWORK_CONFIG,
            network_config(self.identity.config, speed, duplex),
        )?;
        self.looped_frame(scratch, false)
    }

    /// One frame from `scratch` out and back - inside the MAC if `local`,
    /// else wherever the PHY loops it - received into `scratch` and
    /// compared.
    fn looped_frame(&mut self, scratch: &mut DmaRegion, local: bool) -> Result<SelfTest, HwError> {
        let loopback = if local { control::LOOPBACK_LOCAL } else { 0 };
        if scratch.len() < SELF_TEST_RX.saturating_add(BUFFER_SIZE) {
            return Err(HwError::Gem {
                reason: "a self-test scratch region under 4 KiB",
            });
        }
        let frame = self_test_frame(self.station);
        scratch.write(0, &frame)?;
        scratch.write(SELF_TEST_RX, &[0u8; SELF_TEST_LEN])?;
        let tx = self.bus(scratch.device_address(0, SELF_TEST_LEN)?)?;
        let rx = self.bus(scratch.device_address(SELF_TEST_RX, BUFFER_SIZE)?)?;
        self.rx
            .offer(&mut self.rings, SELF_TEST_BUFFER, rx)
            .map_err(HwError::GemRing)?;
        self.tx
            .send(&mut self.rings, SELF_TEST_BUFFER, tx, SELF_TEST_LEN)
            .map_err(HwError::GemRing)?;
        // Loopback first, with receive and transmit off; then both on.
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE | loopback)?;
        self.write(
            regs::NETWORK_CONTROL,
            control::MDIO_ENABLE | loopback | control::RX_ENABLE | control::TX_ENABLE,
        )?;
        self.kick()?;
        let ticks = SELF_TEST_TIMEOUT_US
            .saturating_mul(cpu::counter_frequency())
            .checked_div(1_000_000)
            .unwrap_or(0);
        let start = cpu::counter();
        let mut result = SelfTest::default();
        while cpu::counter().saturating_sub(start) <= ticks {
            if !result.sent
                && let Some(sent) = self.tx.reclaim(&mut self.rings).map_err(HwError::GemRing)?
            {
                result.sent = sent.ok;
            }
            if result.received.is_none() {
                result.received = self.rx.take(&mut self.rings).map_err(HwError::GemRing)?;
            }
            if result.sent && result.received.is_some() {
                break;
            }
        }
        if let Some(Received::Frame { len, .. }) = result.received {
            let mut back = [0u8; SELF_TEST_LEN];
            scratch.read(SELF_TEST_RX, &mut back)?;
            result.matched = len == SELF_TEST_LEN && back == frame;
        }
        let (rx_status, tx_status) = self.take_status()?;
        result.rx_status = rx_status;
        result.tx_status = tx_status;
        // Everything off, loopback off, both rings empty and at their base.
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE | loopback)?;
        self.write(regs::NETWORK_CONTROL, control::MDIO_ENABLE)?;
        self.restart_rx()?;
        self.restart_tx()?;
        Ok(result)
    }

    /// Hands the client's receive buffer at `offset` to the MAC.
    /// `Ok(false)` if the ring is full.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the buffer is not inside the receive
    /// region; [`HwError::GemRing`] as the ring's.
    pub fn offer(&mut self, offset: usize) -> Result<bool, HwError> {
        let address = self.bus(self.rx_buffers.device_address(offset, BUFFER_SIZE)?)?;
        self.rx
            .offer(&mut self.rings, offset, address)
            .map_err(HwError::GemRing)
    }

    /// The next frame the MAC has received, checked (`nowayos_gem::ring`):
    /// a frame, or a buffer whose frame was dropped; `Ok(None)` if none.
    ///
    /// # Errors
    ///
    /// [`HwError::GemRing`] as the ring's.
    pub fn receive(&mut self) -> Result<Option<Received>, HwError> {
        self.rx.take(&mut self.rings).map_err(HwError::GemRing)
    }

    /// Queues the `len`-byte frame in the client's transmit buffer at
    /// `offset`. `Ok(false)` if the ring is full. Call [`Gem::kick`] after
    /// queueing.
    ///
    /// # Errors
    ///
    /// [`HwError::OutsideRegion`] if the frame is not inside the transmit
    /// region; [`HwError::GemRing`] as the ring's.
    pub fn send(&mut self, offset: usize, len: usize) -> Result<bool, HwError> {
        let address = self.bus(self.tx_buffers.device_address(offset, len)?)?;
        self.tx
            .send(&mut self.rings, offset, address, len)
            .map_err(HwError::GemRing)
    }

    /// Starts transmission of whatever is queued (`tx_start_pclk`).
    ///
    /// # Errors
    ///
    /// As [`Gem::set_link`].
    pub fn kick(&mut self) -> Result<(), HwError> {
        // The descriptors are in memory before the MAC is told to look.
        cpu::dsb();
        let value = self.read(regs::NETWORK_CONTROL)?;
        self.write(regs::NETWORK_CONTROL, value | control::TX_START)
    }

    /// The next frame the MAC has sent: its buffer the driver's again.
    /// `Ok(None)` if none.
    ///
    /// # Errors
    ///
    /// [`HwError::GemRing`] as the ring's.
    pub fn reclaim(&mut self) -> Result<Option<Sent>, HwError> {
        self.tx.reclaim(&mut self.rings).map_err(HwError::GemRing)
    }

    /// The rings' counts: frames dropped as oversize, as malformed, and as
    /// moved, and frames the MAC reported an error sending.
    #[must_use]
    pub const fn dropped(&self) -> (u32, u32, u32, u32) {
        (
            self.rx.oversize(),
            self.rx.malformed(),
            self.rx.moved(),
            self.tx.errors(),
        )
    }

    /// `receive_status` and `transmit_status`, cleared once read: what the
    /// DMA saw (a buffer not available, an overrun, a bus error).
    ///
    /// # Errors
    ///
    /// As [`Gem::set_link`].
    pub fn take_status(&mut self) -> Result<(u32, u32), HwError> {
        let rx = self.read(regs::RECEIVE_STATUS)?;
        let tx = self.read(regs::TRANSMIT_STATUS)?;
        self.write(regs::RECEIVE_STATUS, rx)?;
        self.write(regs::TRANSMIT_STATUS, tx)?;
        Ok((rx, tx))
    }

    /// The MAC's statistics registers, as read now, in
    /// [`regs::STATISTICS`]'s order: frames sent and received whole,
    /// broadcast frames received, and the receive errors - for a report.
    ///
    /// # Errors
    ///
    /// As [`Gem::set_link`].
    pub fn statistics(&mut self) -> Result<[u32; regs::STATISTICS.len()], HwError> {
        let mut counts = [0; regs::STATISTICS.len()];
        for (count, at) in counts.iter_mut().zip(regs::STATISTICS) {
            *count = self.read(at)?;
        }
        Ok(counts)
    }

    /// The bus address of the ring of `len` bytes at `offset` in the DMA
    /// region.
    fn ring_address(&self, offset: usize, len: usize) -> Result<u64, HwError> {
        self.bus(self.rings.0.device_address(offset, len)?)
    }

    /// A physical address as the MAC sees it.
    fn bus(&self, physical: u64) -> Result<u64, HwError> {
        physical.checked_add(self.bus_offset).ok_or(HwError::Gem {
            reason: "a bus address past 64 bits",
        })
    }

    /// Reads the register word at byte `at` of the window.
    fn read(&mut self, at: usize) -> Result<u32, HwError> {
        read_word(&mut self.regs, at)
    }

    /// Writes the register word at byte `at` of the window. Only this
    /// module calls it, and only with the offsets `nowayos_gem::regs`
    /// names, each with a value it computed.
    fn write(&mut self, at: usize, value: u32) -> Result<(), HwError> {
        let outside = HwError::Gem {
            reason: "a register outside the window",
        };
        if !at.is_multiple_of(4) {
            return Err(outside);
        }
        let mut word = self.regs.get(at / 4).ok_or(outside)?;
        // SAFETY: `write_unsafe` needs a write that is safe to make. The
        // index is inside the window (`get`), and the callers are this
        // module's alone (`write` is private), each writing a register
        // `nowayos_gem::regs` names with a value computed here. The writes
        // that let the MAC read or write memory - the rings' bases and their
        // upper halves, the receive and transmit enables and transmit start
        // - carry or follow addresses `DmaRegion::device_address` and
        // `IoRegion::device_address` computed inside the regions this view
        // owns, so the MAC is only ever pointed into them (module
        // documentation). The others - interrupts masked and cleared, the
        // status registers cleared, the statistics cleared, the hash, the
        // configuration, the station address, an MDIO frame - touch no
        // memory.
        unsafe { word.write_unsafe(value) };
        Ok(())
    }
}

/// The register word at byte `at` of a MAC's window.
fn read_word(regs: &mut UniqueMmioPointer<'static, [u32]>, at: usize) -> Result<u32, HwError> {
    let outside = HwError::Gem {
        reason: "a register outside the window",
    };
    if !at.is_multiple_of(4) {
        return Err(outside);
    }
    let mut word = regs.get(at / 4).ok_or(outside)?;
    // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
    // window (`get` checked the index), and of a register `nowayos_gem::regs`
    // names, none of which changes state when read (UG1087: the
    // configuration, status, PHY management, revision and design registers
    // are read-only or read-write; the status registers clear only when
    // written; `int_status` and the statistics are never read).
    Ok(unsafe { word.read_unsafe() })
}

// Not under Miri: the MAC's registers are read and written through
// safe-mmio, whose accesses are inline assembly, which Miri cannot run;
// make pds-test runs these on the host.
#[cfg(all(test, not(miri)))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test code: a failed check panics, and tests index and compute on known data"
    )]

    use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

    use super::*;

    /// A MAC's window in ordinary memory, RP1's identity in it.
    fn window() -> &'static [AtomicU32] {
        let words: &'static mut [AtomicU32] = Box::leak(
            (0..0x1400)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        words[regs::REVISION / 4].store(0x0007_0109, Ordering::Relaxed);
        words[regs::DESIGNCFG_DEBUG6 / 4].store(0x0882_0000, Ordering::Relaxed);
        words[regs::NETWORK_CONFIG / 4].store(0x0054_0000, Ordering::Relaxed);
        words[regs::NETWORK_STATUS / 4].store(0x6, Ordering::Relaxed);
        words
    }

    fn memory() -> &'static [AtomicU8] {
        Box::leak(
            (0..DMA_SIZE)
                .map(|_| AtomicU8::new(0xA5))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    fn gem(words: &'static [AtomicU32]) -> Gem {
        Gem::new(
            Device::for_test(words.as_ptr().expose_provenance(), words.len() * 4),
            DmaRegion::for_test(memory(), 0x3000_0000),
            IoRegion::for_test(0x2000_0000, 0x4_0000),
            IoRegion::for_test(0x2100_0000, 0x4_0000),
            0x10_0000_0000,
        )
        .unwrap()
    }

    #[test]
    fn identifies_rp1s_mac_and_refuses_others() {
        let words = window();
        assert_eq!(
            gem(words).identity(),
            Identity {
                revision: 0x0109,
                design: 0x0882_0000,
                config: 0x0054_0000,
                dma: 0,
            }
        );
        let other = window();
        other[regs::REVISION / 4].store(0x0002_0109, Ordering::Relaxed);
        let refused = Gem::new(
            Device::for_test(other.as_ptr().expose_provenance(), other.len() * 4),
            DmaRegion::for_test(memory(), 0x3000_0000),
            IoRegion::for_test(0x2000_0000, 0x4_0000),
            IoRegion::for_test(0x2100_0000, 0x4_0000),
            0,
        );
        assert!(matches!(refused, Err(HwError::Gem { .. })));
    }

    #[test]
    fn reset_points_the_mac_at_its_rings_above_the_bus_offset() {
        let words = window();
        let mut gem = gem(words);
        gem.reset([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]).unwrap();
        let read = |at: usize| words[at / 4].load(Ordering::Relaxed);
        assert_eq!(read(regs::NETWORK_CONTROL), control::MDIO_ENABLE);
        assert_eq!(read(regs::DMA_CONFIG), DMA_CONFIG_VALUE);
        assert_eq!(read(regs::NETWORK_CONFIG), 0x0056_0402);
        assert_eq!(read(regs::SPEC_ADD1_BOTTOM), 0x1200_5452);
        assert_eq!(read(regs::SPEC_ADD1_TOP), 0x5634);
        assert_eq!(read(regs::UPPER_RX_Q_BASE), 0x10);
        assert_eq!(read(regs::RECEIVE_Q_PTR), 0x3000_0000);
        assert_eq!(read(regs::TRANSMIT_Q_PTR), 0x3000_0000 + TX_RING as u32);
        // A receive buffer goes to the MAC at its bus address.
        assert_eq!(gem.offer(0x800), Ok(true));
        assert_eq!(
            gem.offer(0x4_0000),
            Err(HwError::OutsideRegion {
                offset: 0x4_0000,
                len: 2048,
                size: 0x4_0000
            })
        );
        let mut word = [0u8; 4];
        gem.rings.0.read(0, &mut word).unwrap();
        assert_eq!(u32::from_le_bytes(word), 0x2000_0800);
        gem.rings.0.read(8, &mut word).unwrap();
        assert_eq!(u32::from_le_bytes(word), 0x10);
        assert_eq!(gem.send(0, 60), Ok(true));
        gem.kick().unwrap();
        assert_eq!(
            read(regs::NETWORK_CONTROL),
            control::MDIO_ENABLE | control::TX_START
        );
        assert_eq!(gem.receive(), Ok(None));
        assert_eq!(gem.reclaim(), Ok(None));
    }

    #[test]
    fn mdio_reads_through_the_management_register() {
        let words = window();
        let mut gem = gem(words);
        // What a PHY that answered leaves in the register.
        words[regs::PHY_MANAGEMENT / 4].store(0x6086_7949, Ordering::Relaxed);
        // The model's register keeps what is written, so the read returns
        // the frame itself: a turnaround of 10 and no data.
        assert_eq!(gem.mdio_read(1, 1), Ok(0));
        assert_eq!(
            words[regs::PHY_MANAGEMENT / 4].load(Ordering::Relaxed),
            0x6086_0000
        );
        assert_eq!(gem.mdio_write(1, 0, 0x1340), Ok(()));
        assert_eq!(
            words[regs::PHY_MANAGEMENT / 4].load(Ordering::Relaxed),
            0x5082_1340
        );
        assert!(matches!(gem.mdio_read(32, 0), Err(HwError::Mdio(_))));
    }
}
