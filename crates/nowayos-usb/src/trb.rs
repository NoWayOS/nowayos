// SPDX-License-Identifier: MIT OR Apache-2.0
//! Transfer Request Blocks (xHCI 1.2, sections 4.11 and 6.4): the 16-byte
//! records on every ring.
//!
//! A TRB is four little-endian dwords: a 64-bit parameter (dwords 0 and 1),
//! a status dword (2) and a control dword (3). The control dword holds the
//! cycle bit (bit 0), whose value tells the ring's consumer whether the
//! record is new, and the TRB type (bits 15:10).
//!
//! **Written by the driver** - commands on the command ring (section 6.4.3)
//! and transfers on a transfer ring (6.4.1): the encoders here leave the
//! cycle bit clear; the ring that places a TRB sets it. An encoder that
//! carries an address takes it as given: `nowayos-hw` computes every address
//! it hands one, inside the DMA region it owns.
//!
//! **Written by the controller** - events on the event ring (6.4.2):
//! [`Event::decode`] takes a copy of the 16 bytes, already out of DMA
//! memory, and checks every field it uses. A controller, or a device through
//! it, may write anything there; a record that is not an event, or whose
//! fields are out of range, is a typed [`EventError`], never a guess and
//! never a panic.

use core::fmt;

use crate::bits;

/// The bytes of one TRB.
pub const TRB_LEN: usize = 16;

/// TRB types (xHCI 1.2, table 6-91), as the control dword's bits 15:10 hold
/// them.
pub mod kind {
    /// A Normal transfer TRB: one buffer of a bulk or interrupt transfer.
    pub const NORMAL: u8 = 1;
    /// A control transfer's Setup Stage.
    pub const SETUP_STAGE: u8 = 2;
    /// A control transfer's Data Stage.
    pub const DATA_STAGE: u8 = 3;
    /// A control transfer's Status Stage.
    pub const STATUS_STAGE: u8 = 4;
    /// An Isoch TRB: the first TRB of an isochronous transfer descriptor,
    /// one service interval's data (section 6.4.1.3).
    pub const ISOCH: u8 = 5;
    /// A Link TRB: the ring continues at another segment.
    pub const LINK: u8 = 6;
    /// The Enable Slot command.
    pub const ENABLE_SLOT: u8 = 9;
    /// The Disable Slot command.
    pub const DISABLE_SLOT: u8 = 10;
    /// The Address Device command.
    pub const ADDRESS_DEVICE: u8 = 11;
    /// The Configure Endpoint command.
    pub const CONFIGURE_ENDPOINT: u8 = 12;
    /// The Evaluate Context command.
    pub const EVALUATE_CONTEXT: u8 = 13;
    /// The Reset Endpoint command.
    pub const RESET_ENDPOINT: u8 = 14;
    /// The Set TR Dequeue Pointer command.
    pub const SET_TR_DEQUEUE: u8 = 16;
    /// The No Op command: completes, doing nothing.
    pub const NO_OP_COMMAND: u8 = 23;
    /// A Transfer Event.
    pub const TRANSFER_EVENT: u8 = 32;
    /// A Command Completion Event.
    pub const COMMAND_COMPLETION: u8 = 33;
    /// A Port Status Change Event.
    pub const PORT_STATUS_CHANGE: u8 = 34;
    /// A Bandwidth Request Event.
    pub const BANDWIDTH_REQUEST: u8 = 35;
    /// A Doorbell Event.
    pub const DOORBELL: u8 = 36;
    /// A Host Controller Event.
    pub const HOST_CONTROLLER: u8 = 37;
    /// A Device Notification Event.
    pub const DEVICE_NOTIFICATION: u8 = 38;
    /// An MFINDEX Wrap Event.
    pub const MFINDEX_WRAP: u8 = 39;
}

// Control dword bits (section 6.4).
const CYCLE: u32 = 1;
/// Link TRB: Toggle Cycle.
const TOGGLE_CYCLE: u32 = 1 << 1;
/// Interrupt-on Short Packet.
const ISP: u32 = 1 << 2;
/// Interrupt On Completion.
const IOC: u32 = 1 << 5;
/// Immediate Data: the parameter holds the data itself.
const IDT: u32 = 1 << 6;
/// Data Stage and Status Stage: the direction is IN.
const DIR_IN: u32 = 1 << 16;
/// Transfer Event: the parameter is an Event Data TRB's value.
const EVENT_DATA: u32 = 1 << 2;
/// Isoch TRB: Start Isoch ASAP - the next service interval free after the
/// transfer descriptor before it (section 4.11.2.5), so no Frame ID.
const SIA: u32 = 1 << 31;

/// A TRB, as its four dwords.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    /// Dwords 0 and 1.
    pub parameter: u64,
    /// Dword 2.
    pub status: u32,
    /// Dword 3: the cycle bit, the type and the type's own fields.
    pub control: u32,
}

impl Trb {
    /// A TRB of type `kind` with everything else zero.
    #[must_use]
    pub const fn of_kind(kind: u8) -> Self {
        Self {
            parameter: 0,
            status: 0,
            control: (kind as u32 & 0x3F) << 10,
        }
    }

    /// The TRB's bytes, as a ring holds them.
    #[must_use]
    pub fn to_bytes(self) -> [u8; TRB_LEN] {
        let mut out = [0u8; TRB_LEN];
        for (to, from) in out.iter_mut().zip(
            self.parameter
                .to_le_bytes()
                .into_iter()
                .chain(self.status.to_le_bytes())
                .chain(self.control.to_le_bytes()),
        ) {
            *to = from;
        }
        out
    }

    /// The TRB `bytes` hold.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; TRB_LEN]) -> Self {
        let [
            p0,
            p1,
            p2,
            p3,
            p4,
            p5,
            p6,
            p7,
            s0,
            s1,
            s2,
            s3,
            c0,
            c1,
            c2,
            c3,
        ] = *bytes;
        Self {
            parameter: u64::from_le_bytes([p0, p1, p2, p3, p4, p5, p6, p7]),
            status: u32::from_le_bytes([s0, s1, s2, s3]),
            control: u32::from_le_bytes([c0, c1, c2, c3]),
        }
    }

    /// The cycle bit.
    #[must_use]
    pub const fn cycle(self) -> bool {
        self.control & CYCLE != 0
    }

    /// The same TRB with its cycle bit set to `cycle`.
    #[must_use]
    pub const fn with_cycle(self, cycle: bool) -> Self {
        Self {
            control: if cycle {
                self.control | CYCLE
            } else {
                self.control & !CYCLE
            },
            ..self
        }
    }

    /// The TRB type, bits 15:10 of the control dword.
    #[must_use]
    pub const fn kind(self) -> u8 {
        bits(self.control, 10, 6) as u8
    }

    /// The slot ID a command names, bits 31:24 of the control dword.
    #[must_use]
    pub const fn slot(self) -> u8 {
        bits(self.control, 24, 8) as u8
    }
}

// ---- Commands (section 6.4.3) ----------------------------------------------

/// A Link TRB to the segment at bus address `segment`, toggling the
/// consumer's cycle state when it follows the link if `toggle`.
#[must_use]
pub const fn link(segment: u64, toggle: bool) -> Trb {
    let trb = Trb::of_kind(kind::LINK);
    Trb {
        parameter: segment & !0xF,
        control: if toggle {
            trb.control | TOGGLE_CYCLE
        } else {
            trb.control
        },
        ..trb
    }
}

/// The No Op command.
#[must_use]
pub const fn no_op_command() -> Trb {
    Trb::of_kind(kind::NO_OP_COMMAND)
}

/// The Enable Slot command, for a slot of the default type (0, USB).
#[must_use]
pub const fn enable_slot() -> Trb {
    Trb::of_kind(kind::ENABLE_SLOT)
}

/// A command naming `slot`, with an input context at bus address
/// `input_context` (16-byte aligned: the low four bits are dropped).
const fn context_command(kind: u8, input_context: u64, slot: u8) -> Trb {
    let trb = Trb::of_kind(kind);
    Trb {
        parameter: input_context & !0xF,
        control: trb.control | ((slot as u32) << 24),
        ..trb
    }
}

/// The Disable Slot command for `slot`.
#[must_use]
pub const fn disable_slot(slot: u8) -> Trb {
    context_command(kind::DISABLE_SLOT, 0, slot)
}

/// The Address Device command for `slot`, with its input context at
/// `input_context`. The Block Set Address Request flag is clear: the
/// controller sends SET_ADDRESS itself.
#[must_use]
pub const fn address_device(input_context: u64, slot: u8) -> Trb {
    context_command(kind::ADDRESS_DEVICE, input_context, slot)
}

/// The Configure Endpoint command for `slot`, with its input context at
/// `input_context`.
#[must_use]
pub const fn configure_endpoint(input_context: u64, slot: u8) -> Trb {
    context_command(kind::CONFIGURE_ENDPOINT, input_context, slot)
}

/// The Evaluate Context command for `slot`, with its input context at
/// `input_context`.
#[must_use]
pub const fn evaluate_context(input_context: u64, slot: u8) -> Trb {
    context_command(kind::EVALUATE_CONTEXT, input_context, slot)
}

/// The Reset Endpoint command for endpoint `dci` (its Device Context
/// Index, 1 to 31) of `slot`: after a stall halts it.
#[must_use]
pub const fn reset_endpoint(slot: u8, dci: u8) -> Trb {
    let trb = context_command(kind::RESET_ENDPOINT, 0, slot);
    Trb {
        control: trb.control | ((dci as u32 & 0x1F) << 16),
        ..trb
    }
}

/// The Set TR Dequeue Pointer command for endpoint `dci` of `slot`: the
/// endpoint's ring restarts at bus address `dequeue`, with cycle state
/// `cycle`.
#[must_use]
pub const fn set_tr_dequeue(slot: u8, dci: u8, dequeue: u64, cycle: bool) -> Trb {
    let trb = context_command(kind::SET_TR_DEQUEUE, 0, slot);
    Trb {
        parameter: (dequeue & !0xF) | if cycle { 1 } else { 0 },
        control: trb.control | ((dci as u32 & 0x1F) << 16),
        ..trb
    }
}

// ---- Transfers (section 6.4.1) ---------------------------------------------

/// What a control transfer's Data Stage does (Setup Stage TRB, bits 17:16:
/// Transfer Type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataStage {
    /// No Data Stage.
    None,
    /// The host sends data.
    Out,
    /// The device sends data.
    In,
}

/// A control transfer's Setup Stage TRB, the 8-byte setup packet carried
/// in the TRB itself (Immediate Data).
#[must_use]
pub fn setup_stage(setup: [u8; 8], data: DataStage) -> Trb {
    let trt: u32 = match data {
        DataStage::None => 0,
        DataStage::Out => 2,
        DataStage::In => 3,
    };
    let trb = Trb::of_kind(kind::SETUP_STAGE);
    Trb {
        parameter: u64::from_le_bytes(setup),
        // TRB Transfer Length: always 8.
        status: 8,
        control: trb.control | IDT | (trt << 16),
    }
}

/// A control transfer's Data Stage TRB: `len` bytes at bus address
/// `buffer`, IN (from the device) if `inbound`. Interrupt-on Short Packet
/// is set, so a device that sends less is reported.
#[must_use]
pub const fn data_stage(buffer: u64, len: u16, inbound: bool) -> Trb {
    let trb = Trb::of_kind(kind::DATA_STAGE);
    Trb {
        parameter: buffer,
        status: len as u32,
        control: trb.control | ISP | if inbound { DIR_IN } else { 0 },
    }
}

/// A control transfer's Status Stage TRB, Interrupt On Completion set. Its
/// direction is the opposite of the Data Stage's, or IN when there is none
/// (USB 2.0, section 8.5.3): pass `inbound` accordingly.
#[must_use]
pub const fn status_stage(inbound: bool) -> Trb {
    let trb = Trb::of_kind(kind::STATUS_STAGE);
    Trb {
        control: trb.control | IOC | if inbound { DIR_IN } else { 0 },
        ..trb
    }
}

/// A Normal TRB: `len` bytes at bus address `buffer`, Interrupt On
/// Completion and Interrupt-on Short Packet set - one interrupt IN report.
#[must_use]
pub const fn normal(buffer: u64, len: u16) -> Trb {
    let trb = Trb::of_kind(kind::NORMAL);
    Trb {
        parameter: buffer,
        status: len as u32,
        control: trb.control | IOC | ISP,
    }
}

/// The largest transfer one Isoch TRB of this driver's describes: a
/// high-speed endpoint's three 1024-byte packets in one microframe (USB
/// 2.0, section 5.6.3), well inside the TRB Transfer Length field's 17
/// bits.
pub const ISOCH_MAX: u16 = 3 * 1024;

/// The Transfer Burst Count and Transfer Last Burst Packet Count of an
/// isochronous transfer descriptor of `packets` packets on an endpoint whose
/// Max Burst Size is `max_burst` (xHCI 1.2, section 4.11.2.3):
/// TBC = ROUNDUP(packets / (Max Burst Size + 1)) - 1, and TLBPC the packets
/// of the last burst - 1, or Max Burst Size when the bursts come out even.
/// A transfer of no packets counts as one, a zero-length packet.
#[must_use]
pub const fn burst_counts(packets: u16, max_burst: u8) -> (u8, u8) {
    let packets = if packets == 0 { 1 } else { packets as u32 };
    // At least 1: no division by zero.
    let burst = (max_burst as u32).saturating_add(1);
    let tbc = packets.div_ceil(burst).saturating_sub(1);
    let residue = match packets.checked_rem(burst) {
        Some(residue) => residue,
        None => 0,
    };
    let tlbpc = if residue == 0 {
        max_burst as u32
    } else {
        residue.saturating_sub(1)
    };
    // Fields of 2 and 4 bits: a TD of at most ISOCH_MAX bytes at 1024 a
    // packet is at most 3 packets; anything larger is clamped, never
    // spilled into the fields beside them.
    (
        if tbc > 3 { 3 } else { tbc as u8 },
        if tlbpc > 15 { 15 } else { tlbpc as u8 },
    )
}

/// An Isoch TRB (section 6.4.1.3) for a transfer descriptor of one TRB:
/// `len` bytes (at most [`ISOCH_MAX`]) at bus address `buffer`, `packets`
/// packets on an endpoint of Max Burst Size `max_burst`, IN if `inbound`.
/// Start Isoch ASAP is set, so the controller gives it the service interval
/// after the one before it; Interrupt On Completion is set, so each
/// interval's transfer is reported, and for IN Interrupt-on Short Packet
/// too, as a device may send less than asked. TD Size is 0: no TRB follows
/// in the descriptor.
#[must_use]
pub const fn isoch(buffer: u64, len: u16, packets: u16, max_burst: u8, inbound: bool) -> Trb {
    let len = if len > ISOCH_MAX { ISOCH_MAX } else { len };
    let (tbc, tlbpc) = burst_counts(packets, max_burst);
    let trb = Trb::of_kind(kind::ISOCH);
    Trb {
        parameter: buffer,
        status: len as u32,
        control: trb.control
            | IOC
            | SIA
            | ((tbc as u32) << 7)
            | ((tlbpc as u32) << 16)
            | if inbound { ISP } else { 0 },
    }
}

// ---- Events (section 6.4.2) ------------------------------------------------

/// A TRB completion code (xHCI 1.2, table 6-90).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion(pub u8);

impl Completion {
    /// Success.
    pub const SUCCESS: Self = Self(1);
    /// Data Buffer Error.
    pub const DATA_BUFFER: Self = Self(2);
    /// Babble Detected: the device sent more than asked for.
    pub const BABBLE: Self = Self(3);
    /// USB Transaction Error.
    pub const TRANSACTION: Self = Self(4);
    /// TRB Error: a TRB the controller could not accept.
    pub const TRB: Self = Self(5);
    /// Stall Error: the device stalled the endpoint.
    pub const STALL: Self = Self(6);
    /// Resource Error.
    pub const RESOURCE: Self = Self(7);
    /// Bandwidth Error: a Configure Endpoint the bus has no room for.
    pub const BANDWIDTH: Self = Self(8);
    /// No Slots Available.
    pub const NO_SLOTS: Self = Self(9);
    /// Slot Not Enabled.
    pub const SLOT_NOT_ENABLED: Self = Self(11);
    /// Endpoint Not Enabled.
    pub const ENDPOINT_NOT_ENABLED: Self = Self(12);
    /// Short Packet: the transfer ended with less than asked for.
    pub const SHORT_PACKET: Self = Self(13);
    /// Ring Underrun: an isochronous OUT endpoint's ring was empty when its
    /// interval came (section 4.10.3.1); its TRB pointer names no TRB.
    pub const RING_UNDERRUN: Self = Self(14);
    /// Ring Overrun: an isochronous IN endpoint's ring was empty when its
    /// interval came; its TRB pointer names no TRB.
    pub const RING_OVERRUN: Self = Self(15);
    /// Parameter Error: a context the controller did not accept.
    pub const PARAMETER: Self = Self(17);
    /// Context State Error.
    pub const CONTEXT_STATE: Self = Self(19);
    /// Missed Service Error: an isochronous transfer descriptor the
    /// controller could not serve in its interval; its data is lost and the
    /// endpoint goes on (section 4.10.3.2).
    pub const MISSED_SERVICE: Self = Self(23);
    /// Command Ring Stopped: the event the command ring's stop, after an
    /// abort, ends with (section 4.6.1.2).
    pub const COMMAND_RING_STOPPED: Self = Self(24);
    /// Command Aborted: a command the driver aborted.
    pub const COMMAND_ABORTED: Self = Self(25);
    /// Isoch Buffer Overrun: a device sent an isochronous IN endpoint more
    /// than its buffer holds; the endpoint goes on (table 6-90, note 115).
    pub const ISOCH_BUFFER_OVERRUN: Self = Self(31);

    /// Whether the TRB completed: success, or a short packet, which a
    /// transfer of unknown length ends with.
    #[must_use]
    pub const fn ok(self) -> bool {
        self.0 == Self::SUCCESS.0 || self.0 == Self::SHORT_PACKET.0
    }
}

impl fmt::Display for Completion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::SUCCESS => "success",
            Self::DATA_BUFFER => "data buffer error",
            Self::BABBLE => "babble",
            Self::TRANSACTION => "USB transaction error",
            Self::TRB => "TRB error",
            Self::STALL => "stall",
            Self::RESOURCE => "resource error",
            Self::BANDWIDTH => "bandwidth error",
            Self::NO_SLOTS => "no slots available",
            Self::SLOT_NOT_ENABLED => "slot not enabled",
            Self::ENDPOINT_NOT_ENABLED => "endpoint not enabled",
            Self::SHORT_PACKET => "short packet",
            Self::RING_UNDERRUN => "ring underrun",
            Self::RING_OVERRUN => "ring overrun",
            Self::PARAMETER => "parameter error",
            Self::CONTEXT_STATE => "context state error",
            Self::MISSED_SERVICE => "missed service error",
            Self::COMMAND_RING_STOPPED => "command ring stopped",
            Self::COMMAND_ABORTED => "command aborted",
            Self::ISOCH_BUFFER_OVERRUN => "isoch buffer overrun",
            _ => return write!(f, "completion code {}", self.0),
        };
        f.write_str(name)
    }
}

/// An event the controller wrote, decoded and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A transfer TRB completed (section 6.4.2.1).
    Transfer {
        /// The bus address of the TRB that completed, as the controller
        /// reports it: still to be checked against the ring it names.
        trb: u64,
        /// The bytes of the transfer not done (TRB Transfer Length), at
        /// most 2^24 - 1.
        residual: u32,
        /// How it completed.
        code: Completion,
        /// The endpoint's Device Context Index, 1 to 31.
        endpoint: u8,
        /// The device slot, 1 to 255.
        slot: u8,
    },
    /// A command completed (section 6.4.2.2).
    CommandCompletion {
        /// The bus address of the command TRB, as the controller reports
        /// it: still to be checked against the command ring.
        trb: u64,
        /// How it completed.
        code: Completion,
        /// The slot it concerns: the slot an Enable Slot command enabled,
        /// for one. 0 when none.
        slot: u8,
    },
    /// A root hub port's status changed (section 6.4.2.3).
    PortStatusChange {
        /// The port, from 1.
        port: u8,
    },
    /// The controller reports an error of its own (section 6.4.2.6).
    HostController {
        /// Which.
        code: Completion,
    },
    /// An event of a kind this driver does not act on: a bandwidth
    /// request, doorbell, device notification or MFINDEX wrap.
    Ignored {
        /// Its TRB type, 35, 36, 38 or 39.
        kind: u8,
    },
}

/// Why a record on the event ring was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// Its type is not an event's (32 to 39).
    NotAnEvent {
        /// The TRB type it holds.
        kind: u8,
    },
    /// A Transfer Event names endpoint 0 or slot 0.
    BadTransfer {
        /// The endpoint ID it gives.
        endpoint: u8,
        /// The slot ID it gives.
        slot: u8,
    },
    /// A Port Status Change Event names port 0.
    BadPort,
    /// A TRB pointer that is not 16-byte aligned.
    Misaligned {
        /// The pointer.
        trb: u64,
    },
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotAnEvent { kind } => write!(f, "a TRB of type {kind} on the event ring"),
            Self::BadTransfer { endpoint, slot } => {
                write!(f, "a transfer event for endpoint {endpoint} of slot {slot}")
            }
            Self::BadPort => f.write_str("a port status change for port 0"),
            Self::Misaligned { trb } => write!(f, "a TRB pointer {trb:#x} off a 16-byte boundary"),
        }
    }
}

impl Event {
    /// Decodes `bytes`, a copy of one record from the event ring, whose
    /// cycle bit the caller has already matched with the ring's.
    ///
    /// # Errors
    ///
    /// [`EventError`] for a record that is not an event, or whose fields
    /// are out of range.
    pub fn decode(bytes: &[u8; TRB_LEN]) -> Result<Self, EventError> {
        let trb = Trb::from_bytes(bytes);
        let code = Completion(bits(trb.status, 24, 8) as u8);
        match trb.kind() {
            kind::TRANSFER_EVENT => {
                let endpoint = bits(trb.control, 16, 5) as u8;
                let slot = trb.slot();
                if endpoint == 0 || slot == 0 {
                    return Err(EventError::BadTransfer { endpoint, slot });
                }
                // An Event Data TRB's parameter is a value of the driver's
                // choosing, not a pointer: this driver queues none, so a
                // controller claiming one is reporting a TRB it never had.
                if trb.control & EVENT_DATA != 0 || trb.parameter & 0xF != 0 {
                    return Err(EventError::Misaligned { trb: trb.parameter });
                }
                Ok(Self::Transfer {
                    trb: trb.parameter,
                    residual: bits(trb.status, 0, 24),
                    code,
                    endpoint,
                    slot,
                })
            }
            kind::COMMAND_COMPLETION => {
                if trb.parameter & 0xF != 0 {
                    return Err(EventError::Misaligned { trb: trb.parameter });
                }
                Ok(Self::CommandCompletion {
                    trb: trb.parameter,
                    code,
                    slot: trb.slot(),
                })
            }
            kind::PORT_STATUS_CHANGE => {
                let port = ((trb.parameter >> 24) & 0xFF) as u8;
                if port == 0 {
                    return Err(EventError::BadPort);
                }
                Ok(Self::PortStatusChange { port })
            }
            kind::HOST_CONTROLLER => Ok(Self::HostController { code }),
            other @ (kind::BANDWIDTH_REQUEST
            | kind::DOORBELL
            | kind::DEVICE_NOTIFICATION
            | kind::MFINDEX_WRAP) => Ok(Self::Ignored { kind: other }),
            other => Err(EventError::NotAnEvent { kind: other }),
        }
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

    /// An event record from its dwords, as a controller writes it.
    fn event(parameter: u64, status: u32, control: u32) -> [u8; TRB_LEN] {
        Trb {
            parameter,
            status,
            control,
        }
        .to_bytes()
    }

    #[test]
    fn bytes_round_trip_little_endian() {
        let trb = Trb {
            parameter: 0x1122_3344_5566_7788,
            status: 0x99AA_BBCC,
            control: 0xDDEE_FF01,
        };
        let bytes = trb.to_bytes();
        assert_eq!(bytes[0], 0x88);
        assert_eq!(bytes[7], 0x11);
        assert_eq!(bytes[8], 0xCC);
        assert_eq!(bytes[15], 0xDD);
        assert_eq!(Trb::from_bytes(&bytes), trb);
        assert!(trb.cycle());
        assert!(!trb.with_cycle(false).cycle());
        assert_eq!(trb.with_cycle(false).with_cycle(true), trb);
    }

    #[test]
    fn commands_carry_their_type_slot_and_context() {
        assert_eq!(no_op_command().kind(), 23);
        assert_eq!(enable_slot().control, 9 << 10);
        let address = address_device(0x1234_5000, 3);
        assert_eq!(address.kind(), kind::ADDRESS_DEVICE);
        assert_eq!(address.slot(), 3);
        assert_eq!(address.parameter, 0x1234_5000);
        assert!(!address.cycle());
        assert_eq!(configure_endpoint(0x1000_000F, 1).parameter, 0x1000_0000);
        assert_eq!(evaluate_context(0x2000, 255).slot(), 255);
        let reset = reset_endpoint(2, 3);
        assert_eq!(
            (reset.kind(), reset.slot(), (reset.control >> 16) & 0x1F),
            (14, 2, 3)
        );
        let dequeue = set_tr_dequeue(2, 3, 0x4000_0040, true);
        assert_eq!(dequeue.parameter, 0x4000_0041);
        assert_eq!(disable_slot(7).slot(), 7);
        let l = link(0x8000_1000, true);
        assert_eq!(l.kind(), kind::LINK);
        assert_eq!(l.control & TOGGLE_CYCLE, TOGGLE_CYCLE);
        assert_eq!(link(0x8000_1000, false).control & TOGGLE_CYCLE, 0);
    }

    #[test]
    fn control_transfer_stages() {
        // GET_DESCRIPTOR(Device, 18): the setup packet as immediate data.
        let setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let stage = setup_stage(setup, DataStage::In);
        assert_eq!(stage.kind(), kind::SETUP_STAGE);
        assert_eq!(stage.parameter.to_le_bytes(), setup);
        assert_eq!(stage.status, 8);
        assert_eq!(stage.control & IDT, IDT);
        assert_eq!((stage.control >> 16) & 3, 3);
        assert_eq!((setup_stage(setup, DataStage::None).control >> 16) & 3, 0);
        assert_eq!((setup_stage(setup, DataStage::Out).control >> 16) & 3, 2);
        let data = data_stage(0x4000_2000, 18, true);
        assert_eq!(
            (data.kind(), data.parameter, data.status),
            (3, 0x4000_2000, 18)
        );
        assert_eq!(data.control & (DIR_IN | ISP), DIR_IN | ISP);
        let status = status_stage(false);
        assert_eq!((status.kind(), status.control & (DIR_IN | IOC)), (4, IOC));
        let n = normal(0x4000_3000, 8);
        assert_eq!(
            (n.kind(), n.status, n.control & (IOC | ISP)),
            (1, 8, IOC | ISP)
        );
    }

    #[test]
    fn isoch_trbs() {
        // One 192-byte packet OUT at full speed: one burst of one packet.
        let out = isoch(0x4000_8000, 192, 1, 0, false);
        assert_eq!(out.kind(), kind::ISOCH);
        assert_eq!((out.parameter, out.status), (0x4000_8000, 192));
        assert_eq!(out.control & (IOC | SIA | ISP), IOC | SIA);
        assert_eq!((out.control >> 7) & 3, 0, "TBC");
        assert_eq!((out.control >> 16) & 0xF, 0, "TLBPC");
        assert_eq!((out.control >> 20) & 0x7FF, 0, "no Frame ID");
        assert!(!out.cycle());
        // IN: short packets reported.
        let inbound = isoch(0x4000_9000, 100, 1, 0, true);
        assert_eq!(inbound.control & ISP, ISP);
        // A high-bandwidth high-speed endpoint, three 1024-byte packets a
        // microframe (Max Burst Size 2): one burst, three packets in it.
        let wide = isoch(0x5000_0000, 3072, 3, 2, true);
        assert_eq!(
            ((wide.control >> 7) & 3, (wide.control >> 16) & 0xF),
            (0, 2)
        );
        assert_eq!(wide.status, 3072);
        // A length past the field's use is clamped.
        assert_eq!(isoch(0, u16::MAX, 3, 2, true).status, u32::from(ISOCH_MAX));
    }

    #[test]
    fn burst_counts_follow_the_specifications_formula() {
        // TBC = ROUNDUP(TDPC / (Max Burst + 1)) - 1; TLBPC = residue - 1,
        // or Max Burst when the residue is 0 (xHCI 1.2, section 4.11.2.3).
        assert_eq!(burst_counts(1, 0), (0, 0));
        assert_eq!(burst_counts(0, 0), (0, 0));
        assert_eq!(burst_counts(3, 0), (2, 0));
        assert_eq!(burst_counts(3, 2), (0, 2));
        assert_eq!(burst_counts(2, 2), (0, 1));
        assert_eq!(burst_counts(4, 2), (1, 0));
        assert_eq!(burst_counts(6, 2), (1, 2));
        // Clamped to the fields' widths.
        assert_eq!(burst_counts(u16::MAX, 0), (3, 0));
        assert_eq!(burst_counts(u16::MAX, 255), (3, 15));
    }

    #[test]
    fn events_decode() {
        // Transfer Event: endpoint 3 of slot 2, short packet, 5 bytes left.
        assert_eq!(
            Event::decode(&event(
                0x4000_3010,
                (13 << 24) | 5,
                (2 << 24) | (3 << 16) | (32 << 10) | 1
            )),
            Ok(Event::Transfer {
                trb: 0x4000_3010,
                residual: 5,
                code: Completion::SHORT_PACKET,
                endpoint: 3,
                slot: 2,
            })
        );
        assert_eq!(
            Event::decode(&event(0x4000_1020, 1 << 24, (1 << 24) | (33 << 10) | 1)),
            Ok(Event::CommandCompletion {
                trb: 0x4000_1020,
                code: Completion::SUCCESS,
                slot: 1,
            })
        );
        assert_eq!(
            Event::decode(&event(3 << 24, 1 << 24, (34 << 10) | 1)),
            Ok(Event::PortStatusChange { port: 3 })
        );
        assert_eq!(
            Event::decode(&event(0, 7 << 24, 37 << 10)),
            Ok(Event::HostController {
                code: Completion::RESOURCE
            })
        );
        assert_eq!(
            Event::decode(&event(0, 0, 39 << 10)),
            Ok(Event::Ignored { kind: 39 })
        );
    }

    #[test]
    fn records_that_are_not_events_are_refused() {
        assert_eq!(
            Event::decode(&event(0, 0, 1 << 10)),
            Err(EventError::NotAnEvent { kind: 1 })
        );
        assert_eq!(
            Event::decode(&[0; TRB_LEN]),
            Err(EventError::NotAnEvent { kind: 0 })
        );
        assert_eq!(
            Event::decode(&event(0x1000, 0, (32 << 10) | (1 << 24))),
            Err(EventError::BadTransfer {
                endpoint: 0,
                slot: 1
            })
        );
        assert_eq!(
            Event::decode(&event(0x1000, 0, (32 << 10) | (1 << 16))),
            Err(EventError::BadTransfer {
                endpoint: 1,
                slot: 0
            })
        );
        assert_eq!(
            Event::decode(&event(0x1004, 0, (32 << 10) | (1 << 16) | (1 << 24))),
            Err(EventError::Misaligned { trb: 0x1004 })
        );
        assert_eq!(
            Event::decode(&event(
                0x1000,
                0,
                (32 << 10) | (1 << 16) | (1 << 24) | EVENT_DATA
            )),
            Err(EventError::Misaligned { trb: 0x1000 })
        );
        assert_eq!(
            Event::decode(&event(0x1008, 0, 33 << 10)),
            Err(EventError::Misaligned { trb: 0x1008 })
        );
        assert_eq!(
            Event::decode(&event(0, 0, 34 << 10)),
            Err(EventError::BadPort)
        );
    }

    #[test]
    fn completion_codes_read_as_words() {
        assert!(Completion::SUCCESS.ok());
        assert!(Completion::SHORT_PACKET.ok());
        assert!(!Completion::STALL.ok());
        assert_eq!(Completion::STALL.to_string(), "stall");
        assert_eq!(
            Completion::MISSED_SERVICE.to_string(),
            "missed service error"
        );
        assert!(!Completion::RING_UNDERRUN.ok() && !Completion::MISSED_SERVICE.ok());
        assert_eq!(Completion(200).to_string(), "completion code 200");
    }

    proptest! {
        #[test]
        fn decoding_never_panics_and_keeps_its_rules(bytes in any::<[u8; TRB_LEN]>()) {
            match Event::decode(&bytes) {
                Ok(Event::Transfer { trb, endpoint, slot, residual, .. }) => {
                    prop_assert!(trb % 16 == 0 && (1..=31).contains(&endpoint) && slot != 0);
                    prop_assert!(residual < 1 << 24);
                }
                Ok(Event::CommandCompletion { trb, .. }) => prop_assert!(trb % 16 == 0),
                Ok(Event::PortStatusChange { port }) => prop_assert!(port != 0),
                Ok(Event::HostController { .. }) => {}
                Ok(Event::Ignored { kind }) => prop_assert!(matches!(kind, 35 | 36 | 38 | 39)),
                Err(_) => {}
            }
        }

        #[test]
        fn any_trb_round_trips(parameter: u64, status: u32, control: u32) {
            let trb = Trb { parameter, status, control };
            prop_assert_eq!(Trb::from_bytes(&trb.to_bytes()), trb);
        }
    }
}
