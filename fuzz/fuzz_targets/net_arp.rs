// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-net`'s ARP parser, which the network test
//! client runs on frames a driver passes it, until smoltcp's own ARP takes
//! its place in the network stack PD.
//!
//! The frame is either the fuzzer's bytes as they are, or a gateway's reply
//! with some of its bytes changed and cut to any length, so that every field
//! check is reached from the first inputs. `arp::parse` must:
//!
//! * never panic, whatever the bytes and length;
//! * accept exactly a frame of at least 42 bytes - an Ethernet header and an
//!   ARP packet, padding after it ignored - with EtherType `0x0806`,
//!   hardware type 1, protocol type `0x0800`, address lengths 6 and 4, and
//!   operation 1 or 2, reading each field from its offset in RFC 826's
//!   layout, worked out here again;
//! * refuse every other frame with the error for the first check it breaks,
//!   in that order.
//!
//! What parses is answered as the client would: a request built from its
//! sender and target, into a buffer of any size, either refused for a
//! buffer shorter than a frame, writing nothing, or written as 60 bytes,
//! nothing past them, that parse back to the same addresses.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_net::arp::{self, Arp, ArpError, Ipv4, MIN_FRAME_LEN, Mac, Operation};

/// The longest frame tried: a buffer's length, and more.
const MAX: usize = 2100;

/// The bytes of [`REPLY`]: the shortest frame.
const REPLY_LEN: usize = 60;

/// A gateway's reply to the client's request, padded to the shortest
/// frame: what an honest frame holds.
const REPLY: [u8; REPLY_LEN] = [
    0x52, 0x54, 0x00, 0x12, 0x34, 0x56, // destination
    0x52, 0x55, 0x0a, 0x00, 0x02, 0x02, // source
    0x08, 0x06, // EtherType: ARP
    0x00, 0x01, 0x08, 0x00, 6, 4, // Ethernet, IPv4, 6, 4
    0x00, 0x02, // reply
    0x52, 0x55, 0x0a, 0x00, 0x02, 0x02, // sender's MAC
    10, 0, 2, 2, // sender's IPv4
    0x52, 0x54, 0x00, 0x12, 0x34, 0x56, // target's MAC
    10, 0, 2, 15, // target's IPv4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // padding
];

/// `N` bytes of `frame` from `at`, if it has them.
fn field<const N: usize>(frame: &[u8], at: usize) -> Option<[u8; N]> {
    frame.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// A big-endian `u16` at `at`.
fn be16(frame: &[u8], at: usize) -> Option<u16> {
    field::<2>(frame, at).map(u16::from_be_bytes)
}

/// What `arp::parse` must give for `frame`, worked out from RFC 826's
/// layout: offsets in the frame, the Ethernet header's 14 bytes first.
fn expected(frame: &[u8]) -> Result<Arp, ArpError> {
    let too_short = ArpError::TooShort { len: frame.len() };
    if frame.len() < 42 {
        return Err(too_short);
    }
    let ethertype = be16(frame, 12).ok_or(too_short)?;
    if ethertype != 0x0806 {
        return Err(ArpError::NotArp { ethertype });
    }
    if be16(frame, 14) != Some(1)
        || be16(frame, 16) != Some(0x0800)
        || frame.get(18) != Some(&6)
        || frame.get(19) != Some(&4)
    {
        return Err(ArpError::NotIpv4OverEthernet);
    }
    let operation = match be16(frame, 20).ok_or(too_short)? {
        1 => Operation::Request,
        2 => Operation::Reply,
        operation => return Err(ArpError::UnknownOperation { operation }),
    };
    let mac = |at| field::<6>(frame, at).map(Mac).ok_or(too_short);
    let ip = |at| field::<4>(frame, at).map(Ipv4).ok_or(too_short);
    Ok(Arp {
        destination: mac(0)?,
        source: mac(6)?,
        operation,
        sender_mac: mac(22)?,
        sender_ip: ip(28)?,
        target_mac: mac(32)?,
        target_ip: ip(38)?,
    })
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; MAX];
    let len = if input.byte().is_multiple_of(4) {
        // The fuzzer's bytes as they are.
        let rest = input.rest();
        let len = rest.len().min(MAX);
        for (to, from) in buf.iter_mut().zip(rest) {
            *to = *from;
        }
        len
    } else {
        // The reply, some bytes changed, cut or padded to any length.
        for (to, from) in buf.iter_mut().zip(REPLY) {
            *to = from;
        }
        for _ in 0..input.byte() % 6 {
            let at = usize::from(input.byte()) % REPLY_LEN;
            if let Some(byte) = buf.get_mut(at) {
                *byte = input.byte();
            }
        }
        match input.byte() % 4 {
            0 => usize::from(input.byte()) % 64,
            1 => usize::from(input.u16()) % MAX,
            _ => REPLY_LEN,
        }
    };
    let frame = buf.get(..len).unwrap_or_default();

    let parsed = arp::parse(frame);
    assert_eq!(parsed, expected(frame), "arp::parse of {frame:02x?}");

    // The client's answer: a request from what parsed.
    let Ok(arp) = parsed else {
        return;
    };
    let room = usize::from(input.byte()).saturating_mul(2);
    let mut out = [0xA5u8; 512];
    let space = out.get_mut(..room.min(512)).unwrap_or_default();
    let space_len = space.len();
    match arp::request(arp.sender_mac, arp.sender_ip, arp.target_ip, space) {
        Ok(written) => {
            assert!(space_len >= MIN_FRAME_LEN && written == MIN_FRAME_LEN);
            assert!(
                out.get(MIN_FRAME_LEN..)
                    .is_some_and(|rest| rest.iter().all(|b| *b == 0xA5)),
                "a request wrote past its frame"
            );
            let again = out.get(..written).map(arp::parse);
            assert_eq!(
                again,
                Some(Ok(Arp {
                    destination: Mac::BROADCAST,
                    source: arp.sender_mac,
                    operation: Operation::Request,
                    sender_mac: arp.sender_mac,
                    sender_ip: arp.sender_ip,
                    target_mac: Mac::default(),
                    target_ip: arp.target_ip,
                })),
                "a request does not parse back"
            );
        }
        Err(error) => {
            assert!(space_len < MIN_FRAME_LEN);
            assert_eq!(error, ArpError::BufferTooSmall { len: space_len });
            assert!(out.iter().all(|b| *b == 0xA5), "a refused request wrote");
        }
    }
});
