// SPDX-License-Identifier: MIT OR Apache-2.0
//! Device contexts (xHCI 1.2, section 6.2): what a driver tells the
//! controller about a device, and what the controller keeps.
//!
//! A driver builds an **input context** for the Address Device, Evaluate
//! Context and Configure Endpoint commands: an Input Control Context saying
//! which contexts to add, then a Slot Context and up to 31 Endpoint
//! Contexts, each 32 or 64 bytes (HCCPARAMS1 CSZ). [`InputContext::write`]
//! lays one out in a buffer of the driver's; `nowayos-hw` copies it into DMA
//! memory, with the addresses it computed.
//!
//! The controller keeps the **output device context**, which the driver only
//! reads back: [`SlotReport::decode`] takes a copy of its slot context.

use core::fmt;

use crate::port::Speed;
use crate::{bits, le32, put32};

/// The entries of an input context: the control context, the slot context
/// and 31 endpoint contexts.
pub const INPUT_ENTRIES: usize = 33;

/// The entries of a device context: the slot context and 31 endpoints.
pub const DEVICE_ENTRIES: usize = 32;

/// The size of each context, as HCCPARAMS1 CSZ says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextSize {
    /// 32 bytes (CSZ = 0).
    Bytes32,
    /// 64 bytes (CSZ = 1): the first 32 as for 32, the rest reserved.
    Bytes64,
}

impl ContextSize {
    /// From HCCPARAMS1 CSZ.
    #[must_use]
    pub const fn from_csz(csz: bool) -> Self {
        if csz { Self::Bytes64 } else { Self::Bytes32 }
    }

    /// Bytes per context.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::Bytes32 => 32,
            Self::Bytes64 => 64,
        }
    }

    /// Bytes of an input context: 33 contexts.
    #[must_use]
    pub const fn input_len(self) -> usize {
        match self {
            Self::Bytes32 => 32 * INPUT_ENTRIES,
            Self::Bytes64 => 64 * INPUT_ENTRIES,
        }
    }

    /// Bytes of a device context: 32 contexts.
    #[must_use]
    pub const fn device_len(self) -> usize {
        match self {
            Self::Bytes32 => 32 * DEVICE_ENTRIES,
            Self::Bytes64 => 64 * DEVICE_ENTRIES,
        }
    }
}

/// An endpoint's Device Context Index (section 4.5.1): 1 for the default
/// control endpoint, then `2 * number + 1` for IN endpoints and
/// `2 * number` for OUT; `None` for endpoint number 0 or above 15.
#[must_use]
pub const fn dci(endpoint_address: u8) -> Option<u8> {
    let number = endpoint_address & 0x0F;
    if number == 0 {
        return None;
    }
    let base = number << 1;
    Some(if endpoint_address & 0x80 != 0 {
        base | 1
    } else {
        base
    })
}

/// The kinds of endpoint this driver sets up (section 6.2.3, EP Type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// A bidirectional control endpoint: the default one.
    Control,
    /// An interrupt IN endpoint: a HID device's reports.
    InterruptIn,
    /// An isochronous IN endpoint: a microphone's or a camera's stream.
    IsochIn,
    /// An isochronous OUT endpoint: a speaker's stream.
    IsochOut,
}

impl EndpointKind {
    const fn ep_type(self) -> u32 {
        match self {
            Self::Control => 4,
            Self::InterruptIn => 7,
            Self::IsochIn => 5,
            Self::IsochOut => 1,
        }
    }

    /// Whether it is isochronous.
    #[must_use]
    pub const fn isochronous(self) -> bool {
        matches!(self, Self::IsochIn | Self::IsochOut)
    }
}

/// An interrupt endpoint's service interval in xHCI's form: 2^n units of
/// 125 microseconds (section 6.2.3.6). At low and full speed bInterval
/// counts 1 ms frames (1 to 255), so n is 3 plus its base-2 logarithm,
/// rounded down - never longer than asked; at high speed and above it is
/// already an exponent, bInterval - 1 (USB 2.0, table 9-13). Clamped to
/// what each speed allows: 3 to 10 at low and full speed, 0 to 15 above.
#[must_use]
pub const fn interrupt_interval(speed: Speed, b_interval: u8) -> u8 {
    match speed {
        Speed::Low | Speed::Full => {
            let frames = if b_interval == 0 { 1 } else { b_interval };
            // ilog2 of a nonzero u8 is 0 to 7.
            let n = (frames.ilog2() as u8).saturating_add(3);
            if n > 10 { 10 } else { n }
        }
        Speed::High | Speed::Super | Speed::SuperPlus => {
            let n = b_interval.saturating_sub(1);
            if n > 15 { 15 } else { n }
        }
    }
}

/// An isochronous endpoint's service interval in xHCI's form, 2^n units
/// of 125 microseconds (section 6.2.3.6, table 6-12): bInterval is an
/// exponent at every speed, 1 to 16 - 2^(bInterval - 1) frames at full
/// speed, so n is bInterval + 2 (3 to 18); 2^(bInterval - 1) microframes
/// at high speed and above, so n is bInterval - 1 (0 to 15). A bInterval
/// outside 1 to 16 is clamped into it. Low speed has no isochronous
/// endpoints (USB 2.0, section 5.6): it is taken as full speed.
#[must_use]
pub const fn isoch_interval(speed: Speed, b_interval: u8) -> u8 {
    let b = if b_interval == 0 {
        1
    } else if b_interval > 16 {
        16
    } else {
        b_interval
    };
    match speed {
        Speed::Low | Speed::Full => b.saturating_add(2),
        Speed::High | Speed::Super | Speed::SuperPlus => b.saturating_sub(1),
    }
}

/// The microseconds an endpoint context's Interval `n` stands for: 125
/// times 2^n, for n up to 18 (32,768 ms); more is taken as 18.
#[must_use]
pub const fn interval_micros(n: u8) -> u32 {
    let n = if n > 18 { 18 } else { n };
    125u32 << n
}

/// What the slot context of an input context says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The device's speed.
    pub speed: Speed,
    /// The root hub port it is attached to, from 1.
    pub root_port: u8,
    /// Context Entries: the highest Device Context Index in use, 1 to 31.
    pub entries: u8,
}

/// What one endpoint context of an input context says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Its Device Context Index, 1 to 31.
    pub dci: u8,
    /// Its kind.
    pub kind: EndpointKind,
    /// Max Packet Size, 1 to 1024.
    pub max_packet: u16,
    /// Max Burst Size: for a high-speed isochronous or interrupt endpoint,
    /// the transactions it takes each microframe beyond the first, 0 to 2
    /// (wMaxPacketSize bits 12:11; section 6.2.3.4); 0 otherwise.
    pub max_burst: u8,
    /// The interval exponent ([`interrupt_interval`],
    /// [`isoch_interval`]); 0 for control.
    pub interval: u8,
    /// The bus address of its transfer ring's first TRB, 16-byte aligned.
    pub dequeue: u64,
    /// The ring's cycle state there (Dequeue Cycle State).
    pub cycle: bool,
}

/// Why an input context was not written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextError {
    /// The buffer is shorter than an input context.
    BufferTooSmall,
    /// A Device Context Index outside 1 to 31, or given twice.
    BadIndex(u8),
    /// A packet size outside 1 to 1024, or a dequeue pointer off a 16-byte
    /// boundary.
    BadEndpoint(u8),
    /// A slot context naming port 0, or Context Entries outside 1 to 31.
    BadSlot,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BufferTooSmall => f.write_str("a buffer shorter than an input context"),
            Self::BadIndex(dci) => write!(f, "endpoint context index {dci}"),
            Self::BadEndpoint(dci) => write!(f, "endpoint {dci}'s packet size or ring"),
            Self::BadSlot => f.write_str("a slot context's port or entries"),
        }
    }
}

/// An input context: which contexts to add, and their values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputContext<'a> {
    /// The slot context, added (A0) when given.
    pub slot: Option<Slot>,
    /// Endpoint contexts, each added (A`dci`).
    pub endpoints: &'a [Endpoint],
    /// The configuration value the Configure Endpoint command records
    /// (Input Control Context dword 7); 0 otherwise.
    pub configuration: u8,
}

impl InputContext<'_> {
    /// Lays the input context out in `out`, which is zeroed first: the
    /// Input Control Context's add flags, then each context given, at
    /// entry 1 for the slot and `dci + 1` for an endpoint.
    ///
    /// # Errors
    ///
    /// [`ContextError`] for a buffer that is too short or a context out of
    /// range; `out` is then left zeroed, or as it was if too short.
    pub fn write(&self, size: ContextSize, out: &mut [u8]) -> Result<(), ContextError> {
        let len = size.input_len();
        let out = out.get_mut(..len).ok_or(ContextError::BufferTooSmall)?;
        out.fill(0);
        let entry = size.bytes();
        let mut add: u32 = 0;
        if let Some(slot) = self.slot {
            if slot.root_port == 0 || !(1..=31).contains(&slot.entries) {
                return Err(ContextError::BadSlot);
            }
            add |= 1;
            let words = [
                (u32::from(slot.speed.id()) << 20) | (u32::from(slot.entries) << 27),
                u32::from(slot.root_port) << 16,
                0,
                0,
            ];
            write_words(out, entry, &words).ok_or(ContextError::BufferTooSmall)?;
        }
        for endpoint in self.endpoints {
            let dci = endpoint.dci;
            if !(1..=31).contains(&dci) {
                return Err(ContextError::BadIndex(dci));
            }
            let flag = 1u32.checked_shl(u32::from(dci)).unwrap_or(0);
            if add & flag != 0 {
                out.fill(0);
                return Err(ContextError::BadIndex(dci));
            }
            if !(1..=1024).contains(&endpoint.max_packet)
                || endpoint.dequeue & 0xF != 0
                || endpoint.max_burst > 2
            {
                out.fill(0);
                return Err(ContextError::BadEndpoint(dci));
            }
            add |= flag;
            let max_packet = u32::from(endpoint.max_packet);
            let burst = u32::from(endpoint.max_burst);
            // The bytes each interval: a packet per transaction, at most
            // 3 x 1024, so the Max ESIT Payload's high byte is 0 (section
            // 6.2.3.8). A TD of this driver's is one interval's: its
            // Average TRB Length too.
            let esit = max_packet.saturating_mul(burst.saturating_add(1));
            // CErr 3 - three tries - but 0 for isochronous endpoints, which
            // are never retried (section 6.2.3, table 6-9).
            let errors = if endpoint.kind.isochronous() { 0 } else { 3 };
            let dequeue = endpoint.dequeue | u64::from(endpoint.cycle);
            let words = [
                // Interval; Mult 0 below SuperSpeed.
                u32::from(endpoint.interval) << 16,
                // CErr, EP Type, Max Burst Size, Max Packet Size.
                (errors << 1) | (endpoint.kind.ep_type() << 3) | (burst << 8) | (max_packet << 16),
                dequeue as u32,
                (dequeue >> 32) as u32,
                // Average TRB Length: 8 for control (section 4.14.1.1),
                // an interval's bytes otherwise; Max ESIT Payload low.
                match endpoint.kind {
                    EndpointKind::Control => 8,
                    EndpointKind::InterruptIn | EndpointKind::IsochIn | EndpointKind::IsochOut => {
                        esit | (esit << 16)
                    }
                },
            ];
            let at = usize::from(dci)
                .checked_add(1)
                .and_then(|index| index.checked_mul(entry))
                .ok_or(ContextError::BadIndex(dci))?;
            let target = out.get_mut(at..).ok_or(ContextError::BufferTooSmall)?;
            write_words(target, 0, &words).ok_or(ContextError::BufferTooSmall)?;
        }
        put32(out, 4, add).ok_or(ContextError::BufferTooSmall)?;
        put32(out, 28, u32::from(self.configuration)).ok_or(ContextError::BufferTooSmall)?;
        Ok(())
    }
}

/// Writes `words` little-endian from byte `at` of `out`.
fn write_words(out: &mut [u8], at: usize, words: &[u32]) -> Option<()> {
    for (index, word) in words.iter().enumerate() {
        put32(out, index.checked_mul(4)?.checked_add(at)?, *word)?;
    }
    Some(())
}

/// A slot's state, from its output slot context (section 6.2.2, Slot
/// State).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// Disabled or enabled: no address yet.
    Enabled,
    /// Default: the device answers at address 0.
    Default,
    /// Addressed: the device has its address.
    Addressed,
    /// Configured: its endpoints are set up.
    Configured,
}

/// What the controller's slot context says about a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotReport {
    /// The USB address the controller gave the device.
    pub address: u8,
    /// The slot's state.
    pub state: SlotState,
}

impl SlotReport {
    /// Decodes the first 16 bytes of a copy of an output slot context.
    /// `None` for a reserved Slot State value or a copy that is too short.
    #[must_use]
    pub fn decode(slot_context: &[u8]) -> Option<Self> {
        let word = le32(slot_context, 12)?;
        let state = match bits(word, 27, 5) {
            0 => SlotState::Enabled,
            1 => SlotState::Default,
            2 => SlotState::Addressed,
            3 => SlotState::Configured,
            _ => return None,
        };
        Some(Self {
            address: bits(word, 0, 8) as u8,
            state,
        })
    }
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

    fn word(bytes: &[u8], at: usize) -> u32 {
        le32(bytes, at).unwrap()
    }

    #[test]
    fn endpoint_indexes() {
        assert_eq!(dci(0x00), None);
        assert_eq!(dci(0x80), None);
        assert_eq!(dci(0x81), Some(3));
        assert_eq!(dci(0x01), Some(2));
        assert_eq!(dci(0x82), Some(5));
        assert_eq!(dci(0x8F), Some(31));
        assert_eq!(dci(0x0F), Some(30));
    }

    #[test]
    fn intervals() {
        // A keyboard asking every 10 ms at full speed: every 8 ms.
        assert_eq!(interrupt_interval(Speed::Full, 10), 6);
        assert_eq!(interrupt_interval(Speed::Low, 1), 3);
        assert_eq!(interrupt_interval(Speed::Low, 0), 3);
        assert_eq!(interrupt_interval(Speed::Full, 255), 10);
        assert_eq!(interrupt_interval(Speed::High, 4), 3);
        assert_eq!(interrupt_interval(Speed::High, 0), 0);
        assert_eq!(interrupt_interval(Speed::High, 200), 15);
    }

    #[test]
    fn an_address_device_input_context() {
        let mut out = [0xAAu8; 64 * INPUT_ENTRIES];
        let ep0 = Endpoint {
            dci: 1,
            kind: EndpointKind::Control,
            max_packet: 8,
            max_burst: 0,
            interval: 0,
            dequeue: 0x4000_5000,
            cycle: true,
        };
        InputContext {
            slot: Some(Slot {
                speed: Speed::Low,
                root_port: 2,
                entries: 1,
            }),
            endpoints: &[ep0],
            configuration: 0,
        }
        .write(ContextSize::Bytes64, &mut out)
        .unwrap();
        // Add A0 and A1, drop nothing.
        assert_eq!(word(&out, 0), 0);
        assert_eq!(word(&out, 4), 0b11);
        // Slot context at 64: low speed, one entry; port 2.
        assert_eq!(word(&out, 64), (2 << 20) | (1 << 27));
        assert_eq!(word(&out, 68), 2 << 16);
        // EP0 at 128: control, CErr 3, packet 8; ring with DCS 1; 8.
        assert_eq!(word(&out, 132), (3 << 1) | (4 << 3) | (8 << 16));
        assert_eq!(word(&out, 136), 0x4000_5001);
        assert_eq!(word(&out, 140), 0);
        assert_eq!(word(&out, 144), 8);
        assert!(out[192..].iter().all(|b| *b == 0));
        // 32-byte contexts put the same at 32 and 64.
        let mut small = [0u8; 32 * INPUT_ENTRIES];
        InputContext {
            slot: Some(Slot {
                speed: Speed::High,
                root_port: 1,
                entries: 1,
            }),
            endpoints: &[ep0],
            configuration: 0,
        }
        .write(ContextSize::Bytes32, &mut small)
        .unwrap();
        assert_eq!(word(&small, 32), (3 << 20) | (1 << 27));
        assert_eq!(word(&small, 72), 0x4000_5001);
    }

    #[test]
    fn a_configure_endpoint_input_context() {
        let mut out = [0u8; 64 * INPUT_ENTRIES];
        let keyboard = Endpoint {
            dci: 3,
            kind: EndpointKind::InterruptIn,
            max_packet: 8,
            max_burst: 0,
            interval: 6,
            dequeue: 0x1_0000_0000,
            cycle: true,
        };
        InputContext {
            slot: Some(Slot {
                speed: Speed::Full,
                root_port: 1,
                entries: 3,
            }),
            endpoints: &[keyboard],
            configuration: 1,
        }
        .write(ContextSize::Bytes64, &mut out)
        .unwrap();
        assert_eq!(word(&out, 4), 1 | (1 << 3));
        assert_eq!(word(&out, 28), 1);
        assert_eq!(word(&out, 64) >> 27, 3);
        // DCI 3 at entry 4: 256.
        assert_eq!(word(&out, 256), 6 << 16);
        assert_eq!(word(&out, 260), (3 << 1) | (7 << 3) | (8 << 16));
        assert_eq!((word(&out, 264), word(&out, 268)), (1, 1));
        assert_eq!(word(&out, 272), 8 | (8 << 16));
    }

    #[test]
    fn isochronous_endpoints() {
        // Full speed, bInterval 1: every frame, 2^3 x 125 us.
        assert_eq!(isoch_interval(Speed::Full, 1), 3);
        assert_eq!(interval_micros(3), 1000);
        assert_eq!(isoch_interval(Speed::Full, 16), 18);
        assert_eq!(isoch_interval(Speed::Full, 0), 3);
        assert_eq!(isoch_interval(Speed::Full, 200), 18);
        // High speed, bInterval 4: every 8 microframes, 1 ms; 1: every one.
        assert_eq!(isoch_interval(Speed::High, 4), 3);
        assert_eq!(isoch_interval(Speed::High, 1), 0);
        assert_eq!(interval_micros(0), 125);
        assert_eq!(isoch_interval(Speed::High, 16), 15);
        assert_eq!(interval_micros(200), 125 << 18);

        let mut out = [0u8; 32 * INPUT_ENTRIES];
        let speaker = Endpoint {
            dci: 2,
            kind: EndpointKind::IsochOut,
            max_packet: 192,
            max_burst: 0,
            interval: 3,
            dequeue: 0x4000_9000,
            cycle: true,
        };
        let camera = Endpoint {
            dci: 3,
            kind: EndpointKind::IsochIn,
            max_packet: 1024,
            max_burst: 2,
            interval: 0,
            dequeue: 0x4000_A000,
            cycle: true,
        };
        InputContext {
            slot: None,
            endpoints: &[speaker, camera],
            configuration: 1,
        }
        .write(ContextSize::Bytes32, &mut out)
        .unwrap();
        // DCI 2 at entry 3 (96): interval 3; CErr 0, EP Type 1 (Isoch Out),
        // Max Burst 0, 192 bytes; Average TRB Length and Max ESIT 192.
        assert_eq!(word(&out, 96), 3 << 16);
        assert_eq!(word(&out, 100), (1 << 3) | (192 << 16));
        assert_eq!(word(&out, 104), 0x4000_9001);
        assert_eq!(word(&out, 112), 192 | (192 << 16));
        // DCI 3 at entry 4 (128): EP Type 5 (Isoch In), Max Burst 2, 3072
        // bytes an interval.
        assert_eq!(word(&out, 132), (5 << 3) | (2 << 8) | (1024 << 16));
        assert_eq!(word(&out, 144), 3072 | (3072 << 16));
        // A burst no USB 2.0 endpoint has is refused.
        let too_wide = Endpoint {
            max_burst: 3,
            ..camera
        };
        assert_eq!(
            InputContext {
                slot: None,
                endpoints: &[too_wide],
                configuration: 0,
            }
            .write(ContextSize::Bytes32, &mut out),
            Err(ContextError::BadEndpoint(3))
        );
    }

    #[test]
    fn bad_contexts_are_refused() {
        let ep = |dci, max_packet, dequeue| Endpoint {
            dci,
            kind: EndpointKind::InterruptIn,
            max_packet,
            max_burst: 0,
            interval: 3,
            dequeue,
            cycle: true,
        };
        let write = |slot, endpoints: &[Endpoint]| {
            let mut out = [0u8; 32 * INPUT_ENTRIES];
            InputContext {
                slot,
                endpoints,
                configuration: 0,
            }
            .write(ContextSize::Bytes32, &mut out)
        };
        assert_eq!(write(None, &[ep(0, 8, 0)]), Err(ContextError::BadIndex(0)));
        assert_eq!(
            write(None, &[ep(32, 8, 0)]),
            Err(ContextError::BadIndex(32))
        );
        assert_eq!(
            write(None, &[ep(3, 8, 0), ep(3, 8, 0)]),
            Err(ContextError::BadIndex(3))
        );
        assert_eq!(
            write(None, &[ep(3, 0, 0)]),
            Err(ContextError::BadEndpoint(3))
        );
        assert_eq!(
            write(None, &[ep(3, 1025, 0)]),
            Err(ContextError::BadEndpoint(3))
        );
        assert_eq!(
            write(None, &[ep(3, 8, 8)]),
            Err(ContextError::BadEndpoint(3))
        );
        let bad_slot = Slot {
            speed: Speed::Full,
            root_port: 0,
            entries: 1,
        };
        assert_eq!(write(Some(bad_slot), &[]), Err(ContextError::BadSlot));
        let mut short = [0u8; 100];
        assert_eq!(
            InputContext {
                slot: None,
                endpoints: &[],
                configuration: 0
            }
            .write(ContextSize::Bytes32, &mut short),
            Err(ContextError::BufferTooSmall)
        );
    }

    #[test]
    fn slot_reports() {
        let mut slot = [0u8; 32];
        slot[12..16].copy_from_slice(&((2u32 << 27) | 5).to_le_bytes());
        assert_eq!(
            SlotReport::decode(&slot),
            Some(SlotReport {
                address: 5,
                state: SlotState::Addressed
            })
        );
        slot[12..16].copy_from_slice(&(4u32 << 27).to_le_bytes());
        assert_eq!(SlotReport::decode(&slot), None);
        assert_eq!(SlotReport::decode(&slot[..15]), None);
    }

    proptest! {
        #[test]
        fn writing_never_panics_and_never_writes_past_an_input_context(
            dcis in proptest::collection::vec(0u8..40, 0..6),
            max_packet in 0u16..2000,
            dequeue: u64,
            port: u8,
            entries: u8,
            wide: bool,
            burst in 0u8..4,
        ) {
            let size = ContextSize::from_csz(wide);
            let endpoints: Vec<Endpoint> = dcis.iter().map(|&dci| Endpoint {
                dci,
                kind: EndpointKind::InterruptIn,
                max_packet,
                max_burst: burst,
                interval: 3,
                dequeue,
                cycle: false,
            }).collect();
            let mut out = vec![0x5Au8; size.input_len() + 64];
            let _ = InputContext {
                slot: Some(Slot { speed: Speed::Full, root_port: port, entries }),
                endpoints: &endpoints,
                configuration: 0,
            }.write(size, &mut out);
            prop_assert!(out[size.input_len()..].iter().all(|b| *b == 0x5A));
        }
    }
}
