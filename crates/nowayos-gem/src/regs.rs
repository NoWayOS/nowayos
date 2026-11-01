// SPDX-License-Identifier: MIT OR Apache-2.0
//! The GEM's registers the driver uses (UG1087, GEM module), and the values
//! it writes. Offsets are bytes from the start of the MAC's window.

use nowayos_net::BUFFER_SIZE;

use crate::phy::{Duplex, Speed};

/// `network_control`: transmit and receive enables, MDIO, statistics.
pub const NETWORK_CONTROL: usize = 0x000;
/// `network_config`: speed, duplex, filtering, MDC divider, bus width.
pub const NETWORK_CONFIG: usize = 0x004;
/// `network_status`: the PHY management logic idle (`man_done`).
pub const NETWORK_STATUS: usize = 0x008;
/// `dma_config`: buffer sizes, bursts, endianness, address width.
pub const DMA_CONFIG: usize = 0x010;
/// `transmit_status`: write one to clear each bit.
pub const TRANSMIT_STATUS: usize = 0x014;
/// `receive_q_ptr`: the receive ring's base, low 32 bits.
pub const RECEIVE_Q_PTR: usize = 0x018;
/// `transmit_q_ptr`: the transmit ring's base, low 32 bits.
pub const TRANSMIT_Q_PTR: usize = 0x01C;
/// `receive_status`: write one to clear each bit.
pub const RECEIVE_STATUS: usize = 0x020;
/// `int_status`: write one to clear each bit.
pub const INT_STATUS: usize = 0x024;
/// `int_disable`: write one to mask each interrupt.
pub const INT_DISABLE: usize = 0x02C;
/// `phy_management`: one MDIO frame per write.
pub const PHY_MANAGEMENT: usize = 0x034;
/// `hash_bottom` and `hash_top`: the address hash, unused (all zeros).
pub const HASH_BOTTOM: usize = 0x080;
/// See [`HASH_BOTTOM`].
pub const HASH_TOP: usize = 0x084;
/// `spec_add1_bottom`: the station address's first four bytes.
pub const SPEC_ADD1_BOTTOM: usize = 0x088;
/// `spec_add1_top`: its last two (and the filter's type, destination).
pub const SPEC_ADD1_TOP: usize = 0x08C;
/// `revision_reg`: the module's identification and revision.
pub const REVISION: usize = 0x0FC;
/// The statistics registers the driver reports (UG1087, GEM module
/// summary): `frames_txed_ok`, `frames_rxed_ok`, `broadcast_rxed`,
/// `undersize_frames`, `fcs_errors`, `rx_length_errors`,
/// `rx_symbol_errors`, `alignment_errors`, `rx_resource_errors` and
/// `rx_overruns`, in that order. Read only for a report.
pub const STATISTICS: [usize; 10] = [
    0x108, 0x158, 0x15C, 0x184, 0x190, 0x194, 0x198, 0x19C, 0x1A0, 0x1A4,
];
/// `designcfg_debug6`: the configuration the MAC was built with.
pub const DESIGNCFG_DEBUG6: usize = 0x294;
/// `upper_tx_q_base_addr`: the transmit ring's base, high 32 bits.
pub const UPPER_TX_Q_BASE: usize = 0x4C8;
/// `upper_rx_q_base_addr`: the receive ring's base, high 32 bits.
pub const UPPER_RX_Q_BASE: usize = 0x4D4;

/// Every register the driver writes, and every one it reads, for a window
/// check: the highest offset plus a word.
pub const WINDOW_USED: usize = UPPER_RX_Q_BASE + 4;

/// `network_control` bits.
pub mod control {
    /// `tx_start_pclk`: start transmitting (self-clearing).
    pub const TX_START: u32 = 1 << 9;
    /// `clear_all_stats_regs`: zero the statistics (self-clearing).
    pub const CLEAR_STATS: u32 = 1 << 5;
    /// `man_port_en`: the MDIO port on.
    pub const MDIO_ENABLE: u32 = 1 << 4;
    /// `enable_transmit`.
    pub const TX_ENABLE: u32 = 1 << 3;
    /// `enable_receive`.
    pub const RX_ENABLE: u32 = 1 << 2;
    /// `loopback_local`: transmit connected to receive inside the MAC,
    /// full duplex (optional in a GEM, UG1087).
    pub const LOOPBACK_LOCAL: u32 = 1 << 1;
}

/// `network_config` bits.
pub mod config {
    /// `data_bus_width`, bits 22:21: kept as the hardware reports it.
    pub const DATA_BUS_WIDTH: u32 = 0b11 << 21;
    /// `mdc_clock_division`, bits 20:18: kept as found (the board's own
    /// setting keeps MDC within IEEE 802.3's 2.5 MHz).
    pub const MDC_CLOCK_DIVISION: u32 = 0b111 << 18;
    /// `fcs_remove`: frames reach memory without their FCS.
    pub const FCS_REMOVE: u32 = 1 << 17;
    /// `gigabit_mode_enable`.
    pub const GIGABIT: u32 = 1 << 10;
    /// `full_duplex`.
    pub const FULL_DUPLEX: u32 = 1 << 1;
    /// `speed`: 100 Mb/s rather than 10.
    pub const SPEED_100: u32 = 1 << 0;
}

/// `network_status` bits.
pub mod status {
    /// `man_done`: the PHY management logic is idle.
    pub const MDIO_IDLE: u32 = 1 << 2;
}

/// `dma_config` bits.
pub mod dma {
    /// `dma_addr_bus_width_1`: 64-bit addresses, so four-word descriptors.
    pub const ADDRESS_64: u32 = 1 << 30;
    /// `rx_buf_size`, bits 23:16, in 64-byte units.
    pub const RX_BUF_SIZE_SHIFT: u32 = 16;
    /// `tx_pbuf_size`: all of the transmit packet buffer.
    pub const TX_PBUF_FULL: u32 = 1 << 10;
    /// `rx_pbuf_size`, bits 9:8: all of the receive packet buffer.
    pub const RX_PBUF_FULL: u32 = 0b11 << 8;
    /// `amba_burst_length`, bits 4:0: INCR16, the longest RP1's AXI port
    /// takes (*RP1 Peripherals* 7.1: a burst over 16 beats is illegal).
    pub const BURST_INCR16: u32 = 0x10;
}

/// The value written to `dma_config`: 64-bit addressing, receive buffers of
/// `nowayos-net`'s [`BUFFER_SIZE`], both packet buffers in full, INCR16
/// bursts, little-endian data and descriptors (both swaps off), no
/// checksum offload and no extended descriptors.
pub const DMA_CONFIG_VALUE: u32 = dma::ADDRESS_64
    | ((BUFFER_SIZE / 64) as u32) << dma::RX_BUF_SIZE_SHIFT
    | dma::TX_PBUF_FULL
    | dma::RX_PBUF_FULL
    | dma::BURST_INCR16;

/// `network_config` for a link at `speed` and `duplex`, from `found`, the
/// value the hardware had: its bus width and MDC divider are kept, every
/// other bit is this driver's. Frames reach memory without their FCS;
/// broadcast frames are taken (DHCP and ARP need them), and only frames
/// for the station address - no hash, no copy-all, no jumbo frames, no
/// pause, no checksum offload.
#[must_use]
pub const fn network_config(found: u32, speed: Speed, duplex: Duplex) -> u32 {
    let mut value =
        (found & (config::DATA_BUS_WIDTH | config::MDC_CLOCK_DIVISION)) | config::FCS_REMOVE;
    value |= match speed {
        Speed::Mbps10 => 0,
        Speed::Mbps100 => config::SPEED_100,
        Speed::Mbps1000 => config::GIGABIT,
    };
    if matches!(duplex, Duplex::Full) {
        value |= config::FULL_DUPLEX;
    }
    value
}

/// `spec_add1_bottom` and `spec_add1_top` for the station address `mac`, in
/// the order the address is received (UG1087: bit 0 of the bottom word is
/// the first byte's least significant bit). The top word's filter type is
/// zero: a destination-address filter.
#[must_use]
pub const fn station_address(mac: [u8; 6]) -> (u32, u32) {
    let bottom = u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]);
    let top = u32::from_le_bytes([mac[4], mac[5], 0, 0]);
    (bottom, top)
}

/// The module identification number in `revision_reg` (bits 27:16): 7 for
/// a GEM_GXL, as UG1087's reset value and RP1's register both read.
pub const GEM_GXL_MODULE: u32 = 0x007;

/// The module identification and the revision `revision_reg` holds.
#[must_use]
pub const fn module_and_revision(revision: u32) -> (u32, u32) {
    ((revision >> 16) & 0xFFF, revision & 0xFFFF)
}

/// `designcfg_debug6`'s `dma_addr_width_is_64b`: the MAC can take 64-bit
/// addresses.
pub const DESIGN_ADDRESS_64: u32 = 1 << 23;

/// `designcfg_debug6`'s `dma_priority_queue1` to `15`: any queue but the
/// first. This driver uses one queue only, and refuses a MAC with more,
/// whose other queues it would have to park.
pub const DESIGN_PRIORITY_QUEUES: u32 = 0x7FFF << 1;
