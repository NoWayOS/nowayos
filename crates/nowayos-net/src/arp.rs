// SPDX-License-Identifier: MIT OR Apache-2.0
//! ARP for IPv4 over Ethernet: the request the network test client sends,
//! and the parser for what comes back.
//!
//! Written from RFC 826 ("An Ethernet Address Resolution Protocol") and the
//! Ethernet II frame layout (IEEE 802.3, the EtherType field): an Ethernet
//! header - destination, source, EtherType `0x0806` - then the ARP packet:
//!
//! | Offset | Size | Field | Here |
//! | --- | --- | --- | --- |
//! | 0 | 2 | hardware type | 1, Ethernet |
//! | 2 | 2 | protocol type | `0x0800`, IPv4 |
//! | 4 | 1 | hardware address length | 6 |
//! | 5 | 1 | protocol address length | 4 |
//! | 6 | 2 | operation | 1 request, 2 reply |
//! | 8 | 6 | sender hardware address | |
//! | 14 | 4 | sender protocol address | |
//! | 18 | 6 | target hardware address | 0 in a request |
//! | 24 | 4 | target protocol address | |
//!
//! All big-endian (network order). [`parse`] takes a frame the caller has
//! already copied out of shared memory, and checks every field it reads; a
//! frame may carry padding after the packet, as short frames do.

use core::fmt;

use crate::ETHERNET_HEADER_LEN;

/// The EtherType of an ARP packet.
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// The EtherType (and ARP protocol type) of IPv4.
pub const ETHERTYPE_IPV4: u16 = 0x0800;

/// ARP's hardware type for Ethernet.
pub const HARDWARE_ETHERNET: u16 = 1;

/// The bytes of an ARP packet for IPv4 over Ethernet.
pub const PACKET_LEN: usize = 28;

/// The shortest Ethernet frame, without its frame check sequence: shorter
/// frames are padded to it.
pub const MIN_FRAME_LEN: usize = 60;

/// An Ethernet (MAC) address, shown as `52:54:00:12:34:56`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    /// The broadcast address, `ff:ff:ff:ff:ff:ff`.
    pub const BROADCAST: Self = Self([0xff; 6]);
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// An IPv4 address, shown as `10.0.2.2`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ipv4(pub [u8; 4]);

impl fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// An ARP operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    /// 1: who has the target address?
    Request,
    /// 2: the sender has it.
    Reply,
}

impl Operation {
    const fn code(self) -> u16 {
        match self {
            Self::Request => 1,
            Self::Reply => 2,
        }
    }
}

/// An ARP packet and the Ethernet addresses it came with, as [`parse`]
/// read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arp {
    /// The frame's Ethernet destination.
    pub destination: Mac,
    /// The frame's Ethernet source.
    pub source: Mac,
    /// Request or reply.
    pub operation: Operation,
    /// The sender's hardware address.
    pub sender_mac: Mac,
    /// The sender's IPv4 address.
    pub sender_ip: Ipv4,
    /// The target's hardware address (zero in a request).
    pub target_mac: Mac,
    /// The target's IPv4 address.
    pub target_ip: Ipv4,
}

/// Why a frame is not an ARP packet for IPv4 over Ethernet, or could not be
/// written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArpError {
    /// The frame is shorter than an Ethernet header and an ARP packet.
    TooShort {
        /// The frame's length.
        len: usize,
    },
    /// The frame's EtherType is not ARP's.
    NotArp {
        /// The EtherType it carries.
        ethertype: u16,
    },
    /// The packet is not for IPv4 over Ethernet: hardware or protocol type,
    /// or an address length, differ.
    NotIpv4OverEthernet,
    /// The operation is neither a request nor a reply.
    UnknownOperation {
        /// The operation code.
        operation: u16,
    },
    /// The buffer given for a request is too small for one.
    BufferTooSmall {
        /// The buffer's length.
        len: usize,
    },
}

impl fmt::Display for ArpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "a {len}-byte frame is too short for ARP"),
            Self::NotArp { ethertype } => write!(f, "EtherType {ethertype:#06x} is not ARP"),
            Self::NotIpv4OverEthernet => f.write_str("not ARP for IPv4 over Ethernet"),
            Self::UnknownOperation { operation } => write!(f, "unknown ARP operation {operation}"),
            Self::BufferTooSmall { len } => {
                write!(f, "a {len}-byte buffer cannot hold an ARP request")
            }
        }
    }
}

impl core::error::Error for ArpError {}

/// Writes a broadcast ARP request into `out` - who has `target_ip`? tell
/// `sender_ip` at `sender_mac` - zero-padded to [`MIN_FRAME_LEN`] bytes,
/// and returns its length.
///
/// # Errors
///
/// [`ArpError::BufferTooSmall`] if `out` is shorter than
/// [`MIN_FRAME_LEN`]; nothing is written.
pub fn request(
    sender_mac: Mac,
    sender_ip: Ipv4,
    target_ip: Ipv4,
    out: &mut [u8],
) -> Result<usize, ArpError> {
    let len = out.len();
    let frame = out
        .get_mut(..MIN_FRAME_LEN)
        .ok_or(ArpError::BufferTooSmall { len })?;
    frame.fill(0);
    let fields: [&[u8]; 11] = [
        &Mac::BROADCAST.0,
        &sender_mac.0,
        &ETHERTYPE_ARP.to_be_bytes(),
        &HARDWARE_ETHERNET.to_be_bytes(),
        &ETHERTYPE_IPV4.to_be_bytes(),
        &[6, 4],
        &Operation::Request.code().to_be_bytes(),
        &sender_mac.0,
        &sender_ip.0,
        &[0; 6],
        &target_ip.0,
    ];
    let mut at = 0usize;
    for field in fields {
        let end = at.saturating_add(field.len());
        if let Some(space) = frame.get_mut(at..end) {
            space.copy_from_slice(field);
        }
        at = end;
    }
    Ok(MIN_FRAME_LEN)
}

/// Parses an ARP packet for IPv4 over Ethernet from `frame`, which the
/// caller has copied into private memory. Bytes after the packet (padding)
/// are ignored.
///
/// # Errors
///
/// [`ArpError::TooShort`], [`ArpError::NotArp`],
/// [`ArpError::NotIpv4OverEthernet`] or [`ArpError::UnknownOperation`].
pub fn parse(frame: &[u8]) -> Result<Arp, ArpError> {
    let too_short = ArpError::TooShort { len: frame.len() };
    let (ethernet, rest) = frame
        .split_first_chunk::<ETHERNET_HEADER_LEN>()
        .ok_or(too_short)?;
    let (packet, _padding) = rest.split_first_chunk::<PACKET_LEN>().ok_or(too_short)?;
    let [d0, d1, d2, d3, d4, d5, s0, s1, s2, s3, s4, s5, t0, t1] = *ethernet;
    let ethertype = u16::from_be_bytes([t0, t1]);
    if ethertype != ETHERTYPE_ARP {
        return Err(ArpError::NotArp { ethertype });
    }
    let [
        h0,
        h1,
        p0,
        p1,
        hlen,
        plen,
        o0,
        o1,
        sm0,
        sm1,
        sm2,
        sm3,
        sm4,
        sm5,
        si0,
        si1,
        si2,
        si3,
        tm0,
        tm1,
        tm2,
        tm3,
        tm4,
        tm5,
        ti0,
        ti1,
        ti2,
        ti3,
    ] = *packet;
    if u16::from_be_bytes([h0, h1]) != HARDWARE_ETHERNET
        || u16::from_be_bytes([p0, p1]) != ETHERTYPE_IPV4
        || hlen != 6
        || plen != 4
    {
        return Err(ArpError::NotIpv4OverEthernet);
    }
    let operation = match u16::from_be_bytes([o0, o1]) {
        1 => Operation::Request,
        2 => Operation::Reply,
        operation => return Err(ArpError::UnknownOperation { operation }),
    };
    Ok(Arp {
        destination: Mac([d0, d1, d2, d3, d4, d5]),
        source: Mac([s0, s1, s2, s3, s4, s5]),
        operation,
        sender_mac: Mac([sm0, sm1, sm2, sm3, sm4, sm5]),
        sender_ip: Ipv4([si0, si1, si2, si3]),
        target_mac: Mac([tm0, tm1, tm2, tm3, tm4, tm5]),
        target_ip: Ipv4([ti0, ti1, ti2, ti3]),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const OURS: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    const GATEWAY_MAC: Mac = Mac([0x52, 0x55, 0x0a, 0x00, 0x02, 0x02]);
    const US: Ipv4 = Ipv4([10, 0, 2, 15]);
    const GATEWAY: Ipv4 = Ipv4([10, 0, 2, 2]);

    /// A reply to our request, as a gateway would send it: 42 bytes and
    /// `padding` more.
    fn reply(padding: usize) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&OURS.0);
        frame.extend_from_slice(&GATEWAY_MAC.0);
        frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 2]);
        frame.extend_from_slice(&GATEWAY_MAC.0);
        frame.extend_from_slice(&GATEWAY.0);
        frame.extend_from_slice(&OURS.0);
        frame.extend_from_slice(&US.0);
        frame.resize(42 + padding, 0);
        frame
    }

    #[test]
    fn a_request_is_laid_out_as_rfc_826_says() {
        let mut out = [0xaa; 64];
        assert_eq!(request(OURS, US, GATEWAY, &mut out), Ok(60));
        assert_eq!(&out[..6], &[0xff; 6], "broadcast");
        assert_eq!(&out[6..12], &OURS.0);
        assert_eq!(&out[12..22], &[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 1]);
        assert_eq!(&out[22..28], &OURS.0);
        assert_eq!(&out[28..32], &US.0);
        assert_eq!(&out[32..38], &[0; 6], "target hardware address unknown");
        assert_eq!(&out[38..42], &GATEWAY.0);
        assert_eq!(&out[42..60], &[0; 18], "padded with zeros");
        assert_eq!(&out[60..], &[0xaa; 4], "nothing written past the frame");
        assert_eq!(
            request(OURS, US, GATEWAY, &mut [0; 59]),
            Err(ArpError::BufferTooSmall { len: 59 })
        );
    }

    #[test]
    fn a_reply_parses_with_or_without_padding() {
        for padding in [0, 22] {
            assert_eq!(
                parse(&reply(padding)),
                Ok(Arp {
                    destination: OURS,
                    source: GATEWAY_MAC,
                    operation: Operation::Reply,
                    sender_mac: GATEWAY_MAC,
                    sender_ip: GATEWAY,
                    target_mac: OURS,
                    target_ip: US,
                })
            );
        }
    }

    #[test]
    fn what_is_not_arp_for_ipv4_over_ethernet_is_refused() {
        assert_eq!(parse(&reply(0)[..41]), Err(ArpError::TooShort { len: 41 }));
        let mut ipv4 = reply(0);
        ipv4[12..14].copy_from_slice(&[0x08, 0x00]);
        assert_eq!(parse(&ipv4), Err(ArpError::NotArp { ethertype: 0x0800 }));
        for (at, value) in [(15, 6), (16, 0x86), (18, 8), (19, 16)] {
            let mut other = reply(0);
            other[at] = value;
            assert_eq!(
                parse(&other),
                Err(ArpError::NotIpv4OverEthernet),
                "byte {at}"
            );
        }
        let mut unknown = reply(0);
        unknown[21] = 9;
        assert_eq!(
            parse(&unknown),
            Err(ArpError::UnknownOperation { operation: 9 })
        );
    }

    #[test]
    fn addresses_show_as_people_write_them() {
        assert_eq!(GATEWAY_MAC.to_string(), "52:55:0a:00:02:02");
        assert_eq!(GATEWAY.to_string(), "10.0.2.2");
        assert_eq!(Mac::BROADCAST.to_string(), "ff:ff:ff:ff:ff:ff");
    }

    proptest! {
        #[test]
        fn parsing_never_panics(frame in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse(&frame);
        }

        #[test]
        fn a_request_parses_back(mac in any::<[u8; 6]>(), ip in any::<[u8; 4]>(), target in any::<[u8; 4]>()) {
            let mut out = [0u8; 2048];
            let len = request(Mac(mac), Ipv4(ip), Ipv4(target), &mut out).unwrap();
            prop_assert_eq!(
                parse(&out[..len]),
                Ok(Arp {
                    destination: Mac::BROADCAST,
                    source: Mac(mac),
                    operation: Operation::Request,
                    sender_mac: Mac(mac),
                    sender_ip: Ipv4(ip),
                    target_mac: Mac::default(),
                    target_ip: Ipv4(target),
                })
            );
        }

        #[test]
        fn every_truncation_is_too_short(len in 0usize..42) {
            prop_assert_eq!(parse(&reply(0)[..len]), Err(ArpError::TooShort { len }));
        }
    }
}
