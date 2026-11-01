// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-gem-net`: the NIC driver PD for a Cadence GEM MAC - on the
//! Raspberry Pi 5, RP1's. It passes
//! Ethernet frames between the MAC and one client - the network stack -
//! through sDDF's network queues, as `nowayos-virtio-net` does in QEMU, and
//! never sees a frame's bytes.
//!
//! **Polled**: no interrupt from RP1 reaches the CPU, so
//! a timer from the timer PD wakes it every [`TICK_MS`] ms to move frames,
//! and every [`LINK_POLL_TICKS`] ticks it reads the PHY's link.
//!
//! **Starting**, in order: it releases the PHY's reset, which the firmware
//! leaves held (RP1's GPIO 32, `nowayos-hw`'s `rp1`), and waits for the PHY;
//! identifies the MAC (a GEM_GXL, 64-bit addressing, one queue) and resets
//! it - receive and transmit off, interrupts masked, both rings empty in
//! its DMA region, the station address set (`nowayos-hw`'s `gem`); tests
//! the MAC's DMA and rings with no link, one frame looped back inside the
//! MAC from and into a page of its own, and reports it; finds
//! the PHY on MDIO by its identifier; and restarts auto-negotiation through
//! the PHY's clause 22 standard registers, advertising every mode the GEM
//! runs. When the PHY reports a link, it sets the MAC's speed and duplex to
//! the link's (`nowayos_gem::phy::Link::resolve`) and turns receive and
//! transmit on; when the link goes, it turns them off and gives the client
//! back the frames it had not sent. Each change is reported.
//!
//! **Neither the client nor the MAC is trusted**, as in `nowayos-virtio-net`:
//! every descriptor from the client is copied out and checked
//! (`nowayos_net::DataRegion`, `BufferSet`) before its buffer reaches the
//! MAC, and every descriptor word the MAC writes is copied out and checked
//! (`nowayos_gem::ring`) before a frame reaches the client. What fails is
//! dropped and counted, and the counts reported when they change. Frames
//! the client sends while there is no link are dropped and counted, their
//! buffers given straight back.
//!
//! Its MAC address is the stack's, fixed. Everything raw is in
//! `nowayos-hw`; this PD has no `unsafe` code (the workspace lints forbid
//! it). Written from AMD's UG1085 and UG1087 (the GEM), Raspberry Pi's
//! *RP1 Peripherals* and the pinned firmware's device tree (RP1's
//! addresses, the PHY's reset line and address), and IEEE 802.3 clauses 22
//! and 28 (the PHY); no driver source.

#![no_std]

use core::fmt;

use nowayos_gem::brcm;
use nowayos_gem::phy::{self, Link, PhyId, Registers};
use nowayos_gem::ring::Received;
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::gem::{Gem, SELF_TEST_LEN};
use nowayos_hw::rp1::release_phy_reset;
use nowayos_hw::{DmaRegion, HwError, device, dma_region, io_region};
use nowayos_net::arp::Mac;
use nowayos_net::{
    BUFFERS, BufferSet, Consumer, DATA_REGION_LEN, DataRegion, Descriptor, ETHERNET_HEADER_LEN,
    MAX_FRAME_LEN, Producer, QueueError,
};
use nowayos_region::{NetQueue, RegionError, device_facts, net_queue, read_write};
use nowayos_timer_api::Client as Timer;
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// The devices: the MAC's registers, then RP1's GPIO bank 1 and its pads,
/// for the PHY's reset.
const MAC_REGISTERS: usize = 0;
const GPIO_BANK: usize = 1;
const GPIO_PADS: usize = 2;

/// The DMA region of both descriptor rings.
const RINGS: usize = 0;

/// The DMA region of the start-up self-test: a page of the PD's own.
const SCRATCH: usize = 1;

/// The I/O regions: the client's receive and transmit buffers, by physical
/// address only.
const RX_BUFFERS: usize = 0;
const TX_BUFFERS: usize = 1;

/// The queues with the client, read-write regions 0 to 3.
const RX_FREE: usize = 0;
const RX_ACTIVE: usize = 1;
const TX_FREE: usize = 2;
const TX_ACTIVE: usize = 3;

/// The channel to the client.
const CLIENT: Channel = Channel::new(0);

/// The channel to the timer PD: the PD calls it, and it notifies the PD.
const TIMER: Timer = Timer::new(Channel::new(1));

/// The tick's id with the timer PD.
const TICK: u64 = 0;

/// How often the driver looks at the queues and the rings, in ms.
pub const TICK_MS: u64 = 2;

/// How many ticks between readings of the PHY's link: half a second.
pub const LINK_POLL_TICKS: u32 = 250;

/// How many link readings between reports of the traffic: five seconds.
pub const TRAFFIC_POLLS: u32 = 10;

/// How long the PHY is given after its reset is released, in ms: the
/// device tree's reset is 5 ms; a PHY's start-up takes some more.
const PHY_START_MS: u64 = 50;

/// The station address: the network stack's, fixed.
const OUR_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// One queue with the client.
type Queue = NetQueue<BUFFERS>;

/// Starts the NIC driver PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike.
#[must_use]
pub fn init() -> GemNetPd {
    debug_println!("NOWAYOS: gem-net: starting");
    let mut pd = GemNetPd {
        nic: None,
        link: Link::Down,
        ticks: 0,
        dropped: Dropped::default(),
        reported: Counts::default(),
        polls: 0,
        traffic_reported: Traffic::default(),
    };
    match Nic::start() {
        Ok(nic) => {
            pd.nic = Some(nic);
            debug_println!(
                "NOWAYOS: gem-net: ready, MAC {}: waiting for a link",
                Mac(OUR_MAC)
            );
            pd.poll_link();
            if let Err(error) = TIMER.set_timeout(TICK, TICK_MS.saturating_mul(1_000_000)) {
                debug_println!("NOWAYOS: gem-net: could not set the timer: {error:?}");
            }
        }
        Err(error) => debug_println!("NOWAYOS: gem-net: gave up: {error}"),
    }
    pd
}

/// Why the driver could not start.
enum StartError {
    Hw(HwError),
    Region(RegionError),
    Queue(QueueError),
    NoPhy,
    Buffers,
}

impl From<HwError> for StartError {
    fn from(error: HwError) -> Self {
        Self::Hw(error)
    }
}

impl From<RegionError> for StartError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl From<QueueError> for StartError {
    fn from(error: QueueError) -> Self {
        Self::Queue(error)
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hw(error) => write!(f, "{error}"),
            Self::Region(error) => write!(f, "a region: {error}"),
            Self::Queue(error) => write!(f, "a queue: {error}"),
            Self::NoPhy => write!(f, "no PHY answered on MDIO at any address"),
            Self::Buffers => write!(f, "no data region of {BUFFERS} buffers"),
        }
    }
}

/// The MAC, its PHY and the queues: everything the driver moves frames
/// with.
struct Nic {
    gem: Gem,
    phy: u8,
    rx_free: Consumer<Queue>,
    rx_active: Producer<Queue>,
    tx_active: Consumer<Queue>,
    tx_free: Producer<Queue>,
    /// Both data regions' buffers, for checking descriptors.
    buffers: DataRegion,
    /// Receive buffers the driver holds: with the MAC, or being passed on.
    rx_held: BufferSet<BUFFERS>,
    /// Transmit buffers the driver holds: with the MAC.
    tx_held: BufferSet<BUFFERS>,
    /// Frames passed to the client, and frames the MAC sent.
    traffic: Traffic,
}

/// Frames passed to the client, and frames the MAC sent: reported when they
/// change, every [`TRAFFIC_POLLS`] readings of the link at most, with what
/// the MAC's status registers held - so a link that carries nothing one
/// way or the other shows.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Traffic {
    received: u32,
    sent: u32,
}

/// What the client and the link did that was dropped.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Dropped {
    /// Descriptors from the client that failed their checks.
    descriptors: u32,
    /// Times a client index was out of range.
    indices: u32,
    /// Frames the client sent with no link, or that were queued when it
    /// went.
    no_link: u32,
}

/// Who to notify after a round of work.
#[derive(Default)]
struct Wake {
    rx_active: bool,
    tx_free: bool,
}

/// Busy-waits `ms` milliseconds on the CPU's counter.
fn wait_ms(ms: u64) {
    let ticks = ms
        .saturating_mul(counter_frequency())
        .checked_div(1000)
        .unwrap_or(0);
    let start = counter();
    while counter().saturating_sub(start) < ticks {}
}

impl Nic {
    fn start() -> Result<Self, StartError> {
        let facts = device_facts()?;
        let reset = release_phy_reset(device(GPIO_BANK)?, device(GPIO_PADS)?)?;
        debug_println!(
            "NOWAYOS: gem-net: PHY reset released ({}); waiting {PHY_START_MS} ms",
            if reset.was_released {
                "it was not held"
            } else {
                "the firmware held it"
            }
        );
        wait_ms(PHY_START_MS);
        let mut gem = Gem::new(
            device(MAC_REGISTERS)?,
            dma_region(RINGS)?,
            io_region(RX_BUFFERS, DATA_REGION_LEN)?,
            io_region(TX_BUFFERS, DATA_REGION_LEN)?,
            facts.dma_offset(),
        )?;
        let identity = gem.identity();
        debug_println!(
            "NOWAYOS: gem-net: GEM_GXL revision {:#06x}, design {:#010x}; it sees memory at bus address physical + {:#x}",
            identity.revision,
            identity.design,
            facts.dma_offset()
        );
        gem.reset(OUR_MAC)?;
        debug_println!("NOWAYOS: gem-net: MAC reset; receive and transmit off, rings empty");
        let mut scratch = dma_region(SCRATCH)?;
        self_test(&mut gem, &mut scratch)?;
        let (phy, id) = find_phy(&mut gem).ok_or(StartError::NoPhy)?;
        debug_println!(
            "NOWAYOS: gem-net: PHY at MDIO address {phy}, identifier {:04x}:{:04x} (model {:#04x}, revision {})",
            id.id1,
            id.id2,
            id.model(),
            id.revision()
        );
        match rgmii_delays(&mut gem, phy, id) {
            Ok(Some(delays)) => debug_println!(
                "NOWAYOS: gem-net: PHY's RGMII delays for rgmii-id: receive skew {} (was {}), transmit delay {} (was {}); shadows {:#06x} {:#06x}",
                on_off(brcm::rx_skew_on(delays.misc_after)),
                on_off(brcm::rx_skew_on(delays.misc)),
                on_off(brcm::tx_delay_on(delays.clock_after)),
                on_off(brcm::tx_delay_on(delays.clock)),
                delays.misc_after,
                delays.clock_after
            ),
            Ok(None) => debug_println!(
                "NOWAYOS: gem-net: not the Pi 5's PHY: its RGMII delays left as they are"
            ),
            Err(error) => debug_println!("NOWAYOS: gem-net: the PHY's RGMII delays: {error}"),
        }
        phy_self_test(&mut gem, phy, &mut scratch)?;
        restart_negotiation(&mut gem, phy)?;
        debug_println!(
            "NOWAYOS: gem-net: auto-negotiation restarted: 10, 100 and 1000 Mb/s advertised"
        );
        let rx_free = Consumer::new(net_queue(read_write(RX_FREE)?)?)?;
        let rx_active = Producer::new(net_queue(read_write(RX_ACTIVE)?)?)?;
        let tx_free = Producer::new(net_queue(read_write(TX_FREE)?)?)?;
        let tx_active = Consumer::new(net_queue(read_write(TX_ACTIVE)?)?)?;
        let buffers = DataRegion::new(BUFFERS).ok_or(StartError::Buffers)?;
        Ok(Self {
            gem,
            phy,
            rx_free,
            rx_active,
            tx_active,
            tx_free,
            buffers,
            rx_held: BufferSet::none(),
            tx_held: BufferSet::none(),
            traffic: Traffic::default(),
        })
    }

    /// The PHY's registers, as read now.
    fn phy_registers(&mut self) -> Result<Registers, HwError> {
        // BMSR's link bit latches low: the second read is the present.
        self.gem.mdio_read(self.phy, phy::BMSR)?;
        Ok(Registers {
            bmsr: self.gem.mdio_read(self.phy, phy::BMSR)?,
            anar: self.gem.mdio_read(self.phy, phy::ANAR)?,
            anlpar: self.gem.mdio_read(self.phy, phy::ANLPAR)?,
            gbcr: self.gem.mdio_read(self.phy, phy::GBCR)?,
            gbsr: self.gem.mdio_read(self.phy, phy::GBSR)?,
        })
    }

    /// One round with the link up: frames from the MAC to the client, sent
    /// buffers back to the client, empty buffers and frames from the client
    /// to the MAC.
    fn service(&mut self, dropped: &mut Dropped) -> Wake {
        let mut wake = Wake::default();
        self.receive(dropped, &mut wake);
        self.reclaim(dropped, &mut wake);
        self.provide(dropped);
        let sent = self.transmit(dropped, &mut wake, true);
        // sDDF's protocol: ask the client for a notification, then look
        // once more, for what it queued without one.
        self.rx_free.request_signal();
        self.tx_active.request_signal();
        self.provide(dropped);
        let sent_again = self.transmit(dropped, &mut wake, true);
        if (sent || sent_again) && self.gem.kick().is_err() {
            debug_println!("NOWAYOS: gem-net: could not start transmission");
        }
        wake
    }

    /// One round with no link: receive buffers still go to the MAC, for
    /// when the link comes; frames to send are dropped, their buffers given
    /// straight back.
    fn service_without_link(&mut self, dropped: &mut Dropped) -> Wake {
        let mut wake = Wake::default();
        self.provide(dropped);
        self.transmit(dropped, &mut wake, false);
        self.rx_free.request_signal();
        self.tx_active.request_signal();
        self.provide(dropped);
        self.transmit(dropped, &mut wake, false);
        wake
    }

    /// Frames the MAC has received: passed to the client, or - dropped by
    /// the ring's checks, or with no room - offered to the MAC again.
    fn receive(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            let buffer = match self.gem.receive() {
                Ok(Some(Received::Frame { buffer, len })) => {
                    let descriptor = Descriptor::new(as_u64(buffer), as_u16(len));
                    match self.rx_active.enqueue(descriptor) {
                        Ok(()) => {
                            self.rx_held.remove(index(buffer));
                            wake.rx_active = true;
                            self.traffic.received = self.traffic.received.saturating_add(1);
                            continue;
                        }
                        Err(_) => {
                            dropped.indices = dropped.indices.saturating_add(1);
                            buffer
                        }
                    }
                }
                Ok(Some(Received::Dropped { buffer, .. })) => buffer,
                Ok(None) => return,
                Err(error) => {
                    debug_println!("NOWAYOS: gem-net: receive ring: {error}");
                    return;
                }
            };
            // The driver still holds the buffer: give it back to the MAC.
            if !matches!(self.gem.offer(buffer), Ok(true)) {
                self.rx_held.remove(index(buffer));
            }
        }
    }

    /// Transmit buffers the MAC has sent: back to the client.
    fn reclaim(&mut self, dropped: &mut Dropped, wake: &mut Wake) {
        loop {
            match self.gem.reclaim() {
                Ok(Some(sent)) => {
                    self.traffic.sent = self.traffic.sent.saturating_add(1);
                    self.give_back(sent.buffer, dropped, wake);
                }
                Ok(None) => return,
                Err(error) => {
                    debug_println!("NOWAYOS: gem-net: transmit ring: {error}");
                    return;
                }
            }
        }
    }

    /// A transmit buffer back to the client, on its free queue.
    fn give_back(&mut self, buffer: usize, dropped: &mut Dropped, wake: &mut Wake) {
        self.tx_held.remove(index(buffer));
        if self
            .tx_free
            .enqueue(Descriptor::new(as_u64(buffer), 0))
            .is_ok()
        {
            wake.tx_free = true;
        } else {
            dropped.indices = dropped.indices.saturating_add(1);
        }
    }

    /// Empty receive buffers from the client: checked, then offered to the
    /// MAC.
    fn provide(&mut self, dropped: &mut Dropped) {
        loop {
            let descriptor = match self.rx_free.dequeue() {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => return,
                Err(_) => {
                    dropped.indices = dropped.indices.saturating_add(1);
                    return;
                }
            };
            let buffer = match self.buffers.buffer(descriptor) {
                Ok(buffer) if self.rx_held.insert(buffer) => buffer,
                // Outside the region, off a buffer boundary, or a buffer
                // the driver already holds.
                _ => {
                    dropped.descriptors = dropped.descriptors.saturating_add(1);
                    continue;
                }
            };
            let offered = self
                .buffers
                .offset_of(buffer)
                .is_some_and(|offset| matches!(self.gem.offer(offset), Ok(true)));
            if !offered {
                self.rx_held.remove(buffer);
                dropped.descriptors = dropped.descriptors.saturating_add(1);
            }
        }
    }

    /// Frames from the client: checked, then queued for the MAC - or, with
    /// no link, dropped and their buffers given back. Whether any was
    /// queued.
    fn transmit(&mut self, dropped: &mut Dropped, wake: &mut Wake, link: bool) -> bool {
        let mut queued = false;
        loop {
            let descriptor = match self.tx_active.dequeue() {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => return queued,
                Err(_) => {
                    dropped.indices = dropped.indices.saturating_add(1);
                    return queued;
                }
            };
            let frame = match self
                .buffers
                .frame(descriptor, ETHERNET_HEADER_LEN..=MAX_FRAME_LEN)
            {
                Ok(frame) if self.tx_held.insert(frame.buffer) => frame,
                // Outside the region, off a buffer boundary, a length out
                // of range, or a buffer already being sent.
                _ => {
                    dropped.descriptors = dropped.descriptors.saturating_add(1);
                    continue;
                }
            };
            if !link {
                dropped.no_link = dropped.no_link.saturating_add(1);
                self.give_back(frame.offset, dropped, wake);
                continue;
            }
            if matches!(self.gem.send(frame.offset, frame.len), Ok(true)) {
                queued = true;
            } else {
                self.tx_held.remove(frame.buffer);
                dropped.descriptors = dropped.descriptors.saturating_add(1);
            }
        }
    }

    /// The link went: receive and transmit off, and the frames still queued
    /// back to the client.
    fn link_lost(&mut self, dropped: &mut Dropped, wake: &mut Wake) -> Result<(), HwError> {
        self.gem.stop()?;
        while let Some(buffer) = self.gem.abandon()? {
            dropped.no_link = dropped.no_link.saturating_add(1);
            self.give_back(buffer, dropped, wake);
        }
        self.gem.restart_tx()
    }
}

/// The MAC's DMA and rings tested with no link: one frame looped back
/// inside the MAC, and the outcome reported. A failure is reported, not
/// fatal: a GEM need not have local loopback (UG1087).
fn self_test(gem: &mut Gem, scratch: &mut DmaRegion) -> Result<(), StartError> {
    let test = gem.loopback_self_test(scratch)?;
    if test.matched {
        debug_println!(
            "NOWAYOS: gem-net: self-test: a {SELF_TEST_LEN}-byte frame looped back through the MAC's DMA and rings, byte for byte"
        );
    } else {
        debug_println!(
            "NOWAYOS: gem-net: self-test: not looped back (sent {}, received {:?}, receive status {:#x}, transmit status {:#x})",
            test.sent,
            test.received,
            test.rx_status,
            test.tx_status
        );
    }
    Ok(())
}

/// The first PHY that answers on MDIO with an identifier, looking at
/// address 1 first (the device tree's), then the rest.
fn find_phy(gem: &mut Gem) -> Option<(u8, PhyId)> {
    let first = 1u8;
    let others = (0..=nowayos_gem::mdio::MAX_ADDRESS).filter(|address| *address != first);
    for address in core::iter::once(first).chain(others) {
        let id = gem.mdio_read(address, phy::PHYID1).and_then(|id1| {
            Ok(PhyId {
                id1,
                id2: gem.mdio_read(address, phy::PHYID2)?,
            })
        });
        if let Ok(id) = id
            && id.is_present()
        {
            return Some((address, id));
        }
    }
    None
}

/// The Pi 5's PHY's two RGMII delay shadows, before and after.
struct Delays {
    misc: u16,
    clock: u16,
    misc_after: u16,
    clock_after: u16,
}

/// Turns on both of the PHY's RGMII clock delays - the board's link is
/// `rgmii-id`: the PHY delays both clocks, the MAC neither -
/// if it is the Pi 5's Broadcom PHY; `None` for any other, left alone.
/// Each shadow is read, changed in its one bit, written and read back
/// (`nowayos_gem::brcm`).
fn rgmii_delays(gem: &mut Gem, address: u8, id: PhyId) -> Result<Option<Delays>, HwError> {
    if !brcm::is_bcm54213pe(id) {
        return Ok(None);
    }
    gem.mdio_write(address, brcm::AUXCTL, brcm::misc_select())?;
    let before = gem.mdio_read(address, brcm::AUXCTL)?;
    gem.mdio_write(address, brcm::AUXCTL, brcm::misc_with_rx_skew(before))?;
    gem.mdio_write(address, brcm::AUXCTL, brcm::misc_select())?;
    let misc_after = gem.mdio_read(address, brcm::AUXCTL)?;
    gem.mdio_write(address, brcm::SHADOW_1C, brcm::clock_select())?;
    let clock = gem.mdio_read(address, brcm::SHADOW_1C)?;
    gem.mdio_write(address, brcm::SHADOW_1C, brcm::clock_with_tx_delay(clock))?;
    gem.mdio_write(address, brcm::SHADOW_1C, brcm::clock_select())?;
    let clock_after = gem.mdio_read(address, brcm::SHADOW_1C)?;
    Ok(Some(Delays {
        misc: before,
        clock,
        misc_after,
        clock_after,
    }))
}

/// The self-test through the PHY: one frame out across the RGMII
/// bus and back, the PHY in loopback at 1000 Mb/s full duplex (clause 22's
/// BMCR), so the bus's clock delays are tested both ways at every start -
/// on the board, the transmit delay placed anywhere but the PHY, or the
/// receive skew off, loses the frame. Leaves the PHY out of loopback, with
/// auto-negotiation on.
fn phy_self_test(gem: &mut Gem, address: u8, scratch: &mut DmaRegion) -> Result<(), StartError> {
    gem.mdio_write(
        address,
        phy::BMCR,
        phy::bmcr::LOOPBACK | phy::bmcr::SPEED_MSB | phy::bmcr::FULL_DUPLEX,
    )?;
    wait_ms(PHY_START_MS);
    let test = gem.phy_loopback_self_test(scratch, phy::Speed::Mbps1000, phy::Duplex::Full);
    gem.mdio_write(address, phy::BMCR, phy::bmcr::AN_ENABLE)?;
    let test = test?;
    if test.matched {
        debug_println!(
            "NOWAYOS: gem-net: self-test: a {SELF_TEST_LEN}-byte frame looped back through the PHY at 1000 Mb/s, byte for byte"
        );
    } else {
        debug_println!(
            "NOWAYOS: gem-net: self-test: not looped back through the PHY (sent {}, received {:?}, receive status {:#x}, transmit status {:#x})",
            test.sent,
            test.received,
            test.rx_status,
            test.tx_status
        );
    }
    Ok(())
}

/// "on" or "off".
const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Advertises every mode the GEM runs and restarts auto-negotiation, with
/// the PHY powered up and not isolated: clause 22's control register and
/// the advertisement registers only.
fn restart_negotiation(gem: &mut Gem, address: u8) -> Result<(), HwError> {
    gem.mdio_write(address, phy::ANAR, phy::ADVERTISE)?;
    gem.mdio_write(address, phy::GBCR, phy::ADVERTISE_1000)?;
    let bmcr = gem.mdio_read(address, phy::BMCR)?;
    let bmcr = (bmcr
        & !(phy::bmcr::POWER_DOWN
            | phy::bmcr::ISOLATE
            | phy::bmcr::LOOPBACK
            | phy::bmcr::SPEED_LSB
            | phy::bmcr::SPEED_MSB
            | phy::bmcr::FULL_DUPLEX))
        | phy::bmcr::AN_ENABLE
        | phy::bmcr::AN_RESTART;
    gem.mdio_write(address, phy::BMCR, bmcr)
}

/// The buffer at `offset`, which came from this PD's own checks.
fn index(offset: usize) -> usize {
    offset
        .checked_div(nowayos_net::BUFFER_SIZE)
        .unwrap_or(usize::MAX)
}

/// An offset in a data region, as a descriptor holds it.
fn as_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// A frame's length, as a descriptor holds it (at most 1514).
fn as_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// The driver: the NIC, if it started, the link last seen, the ticks since
/// it was read, what has been dropped and the counts last reported.
pub struct GemNetPd {
    nic: Option<Nic>,
    link: Link,
    ticks: u32,
    dropped: Dropped,
    reported: Counts,
    /// Link readings since the traffic was last looked at, and what was
    /// reported then.
    polls: u32,
    traffic_reported: Traffic,
}

/// Everything dropped so far, as reported.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    oversize: u32,
    malformed: u32,
    moved: u32,
    send_errors: u32,
    descriptors: u32,
    indices: u32,
    no_link: u32,
}

impl GemNetPd {
    /// Reads the PHY's link and, if it changed, follows it.
    fn poll_link(&mut self) -> Wake {
        let mut wake = Wake::default();
        let Some(nic) = self.nic.as_mut() else {
            return wake;
        };
        let link = match nic.phy_registers() {
            Ok(registers) => Link::resolve(registers),
            Err(error) => {
                debug_println!("NOWAYOS: gem-net: the PHY's registers: {error}");
                return wake;
            }
        };
        if link == self.link {
            return wake;
        }
        let result = match link {
            Link::Up { speed, duplex } => {
                debug_println!(
                    "NOWAYOS: gem-net: link up at {} Mb/s, {} duplex",
                    speed.mbps(),
                    match duplex {
                        phy::Duplex::Full => "full",
                        phy::Duplex::Half => "half",
                    }
                );
                nic.gem.set_link(speed, duplex)
            }
            Link::Down | Link::NoCommonMode => {
                if matches!(link, Link::NoCommonMode) {
                    debug_println!(
                        "NOWAYOS: gem-net: link, but no mode in common with the other end"
                    );
                } else {
                    debug_println!("NOWAYOS: gem-net: link down");
                }
                if matches!(self.link, Link::Up { .. }) {
                    nic.link_lost(&mut self.dropped, &mut wake)
                } else {
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            debug_println!("NOWAYOS: gem-net: following the link: {error}");
        }
        self.link = link;
        wake
    }

    /// Reports what was dropped, if it changed.
    fn report(&mut self) {
        let Some(nic) = &self.nic else {
            return;
        };
        let (oversize, malformed, moved, send_errors) = nic.gem.dropped();
        let now = Counts {
            oversize,
            malformed,
            moved,
            send_errors,
            descriptors: self.dropped.descriptors,
            indices: self.dropped.indices,
            no_link: self.dropped.no_link,
        };
        if now != self.reported {
            self.reported = now;
            debug_println!(
                "NOWAYOS: gem-net: dropped so far: {} oversize frames, {} malformed frames, {} moved descriptors, {} send errors, {} bad descriptors, {} corrupt queue indices, {} frames with no link",
                now.oversize,
                now.malformed,
                now.moved,
                now.send_errors,
                now.descriptors,
                now.indices,
                now.no_link
            );
        }
    }

    /// Reports the traffic, if it changed, and the MAC's status registers.
    fn report_traffic(&mut self) {
        let Some(nic) = self.nic.as_mut() else {
            return;
        };
        if nic.traffic == self.traffic_reported {
            return;
        }
        self.traffic_reported = nic.traffic;
        let (rx_status, tx_status) = nic.gem.take_status().unwrap_or((0, 0));
        debug_println!(
            "NOWAYOS: gem-net: so far {} frames received, {} sent; receive status {rx_status:#x}, transmit status {tx_status:#x}",
            nic.traffic.received,
            nic.traffic.sent
        );
        if let Ok(
            [
                sent,
                received,
                broadcast,
                undersize,
                fcs,
                length,
                symbol,
                alignment,
                resource,
                overruns,
            ],
        ) = nic.gem.statistics()
        {
            debug_println!(
                "NOWAYOS: gem-net: the MAC counts {sent} frames sent, {received} received ({broadcast} broadcast); receive errors: {undersize} undersize, {fcs} FCS, {length} length, {symbol} symbol, {alignment} alignment, {resource} resource, {overruns} overruns"
            );
        }
    }

    /// One tick: the link, now and then; the queues and rings, always.
    fn tick(&mut self) {
        self.ticks = self.ticks.saturating_add(1);
        let mut wake = Wake::default();
        if self.ticks >= LINK_POLL_TICKS {
            self.ticks = 0;
            wake = self.poll_link();
            self.polls = self.polls.saturating_add(1);
            if self.polls >= TRAFFIC_POLLS {
                self.polls = 0;
                self.report_traffic();
            }
        }
        let Some(nic) = self.nic.as_mut() else {
            return;
        };
        let more = if matches!(self.link, Link::Up { .. }) {
            nic.service(&mut self.dropped)
        } else {
            nic.service_without_link(&mut self.dropped)
        };
        let rx = (wake.rx_active || more.rx_active) && nic.rx_active.signal_wanted();
        let tx = (wake.tx_free || more.tx_free) && nic.tx_free.signal_wanted();
        if rx || tx {
            CLIENT.notify();
        }
        self.report();
        if let Err(error) = TIMER.set_timeout(TICK, TICK_MS.saturating_mul(1_000_000)) {
            debug_println!("NOWAYOS: gem-net: could not set the timer: {error:?}");
        }
    }
}

impl Handler for GemNetPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(TIMER.channel()) {
            while let Ok(Some(_)) = TIMER.expired() {}
            self.tick();
        }
        // The client's notifications need no answer between ticks: the
        // driver looks at every queue on each.
        Ok(())
    }
}
