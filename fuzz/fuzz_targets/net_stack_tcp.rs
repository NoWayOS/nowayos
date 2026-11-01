// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the network stack's TCP connections for its socket client
//! (`nowayos-ip`) - hostile segments from the wire and hostile
//! records from the client, at once.
//!
//! The stack holds a lease (10.0.2.15/24) and an allow list of 10.0.2.100
//! ports 7 and 9. Each step is one of:
//!
//! * a request from the client: a connect - to a listed destination, an
//!   unlisted one or any - data, or a close, for any of six connection
//!   names; or a record of any bytes, decoded as the PD decodes its ring;
//! * a TCP segment from 10.0.2.100 to the stack: any ports, flags, window,
//!   options and payload, its sequence and acknowledgement numbers either
//!   any or the ones that follow the stack's last segment, so the fuzzer
//!   reaches the connections' state machine and not only the parser (the
//!   fuzz build skips smoltcp's checksum checks, `cfg(fuzzing)`);
//! * an ARP reply from 10.0.2.100, so the stack can send to it, or a frame
//!   of any bytes;
//! * time passing.
//!
//! The reports ring has room for between none and eight reports a step.
//! After each step the stack is polled and served, as its PD does it.
//!
//! Checks: nothing panics; every frame the stack sends is a whole Ethernet
//! frame from its own MAC address; every TCP segment that opens a
//! connection or carries data goes to a listed destination (a reset in
//! answer to a stray segment may go anywhere); and while the client keeps
//! the protocol, every report the stack sends does too - the client's own
//! table, which saw the same requests, accepts each.

#![no_main]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::num::NonZeroU64;
use std::rc::Rc;

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_ip::smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use nowayos_ip::smoltcp::time::Instant as NetInstant;
use nowayos_ip::{Lease, Notice, Reports, Requests, Stack, StackSeed, TcpRoom, storage};
use nowayos_net::arp::{Ipv4, Mac};
use nowayos_socket::{
    AllowList, ClientTable, Close, Connect, ConnectionId, Destination, MAX_DATA, RecordError,
    Report, Request, SLOT_LEN, SendData,
};
use nowayos_time::Instant;

/// The most steps one input takes.
const MAX_STEPS: usize = 96;
/// The stack's MAC address and its leased address.
const MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
const OURS: [u8; 4] = [10, 0, 2, 15];
/// The peer: 10.0.2.100, and its MAC address.
const PEER: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 100);
const PEER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x64];
/// The ports the allow list holds for the peer.
const LISTED: [u16; 2] = [7, 9];
/// A millisecond in nanoseconds.
const MS: u64 = 1_000_000;
/// Ethernet and IPv4 headers before a TCP header.
const IP_AT: usize = 14;
const TCP_AT: usize = 34;
/// The longest frame the device carries.
const MTU: usize = 1514;
/// The longest data, record and frame of any bytes a step makes, and one
/// more.
const DATA_LIMIT: usize = MAX_DATA.saturating_add(1);
const RECORD_LIMIT: usize = SLOT_LEN.saturating_add(1);
const FRAME_LIMIT: usize = MTU.saturating_add(1);

/// The stack's end of the wire: frames in, frames out.
#[derive(Default)]
struct Wire {
    inbox: VecDeque<Vec<u8>>,
    outbox: Rc<RefCell<Vec<Vec<u8>>>>,
}

struct Rx(Vec<u8>);
struct Tx(Rc<RefCell<Vec<Vec<u8>>>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let result = f(&mut frame);
        self.0.borrow_mut().push(frame);
        result
    }
}

impl Device for Wire {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx;

    fn receive(&mut self, _: NetInstant) -> Option<(Rx, Tx)> {
        let frame = self.inbox.pop_front()?;
        Some((Rx(frame), Tx(self.outbox.clone())))
    }

    fn transmit(&mut self, _: NetInstant) -> Option<Tx> {
        Some(Tx(self.outbox.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = MTU;
        capabilities
    }
}

/// The client's requests, in order.
#[derive(Default)]
struct Asks(VecDeque<Result<Request, RecordError>>);

impl Requests for Asks {
    fn next(&mut self) -> Option<Result<Request, RecordError>> {
        self.0.pop_front()
    }
}

/// The client's end of the reports ring: its own table checks each report
/// while the client has kept the protocol itself. (Once it sends a request
/// its table refuses, its table and the stack's need not agree: the stack's
/// own table guards it then.)
#[derive(Default)]
struct Hears {
    table: ClientTable,
    room: usize,
    hostile: bool,
}

impl Reports for Hears {
    fn has_room(&self) -> bool {
        self.room > 0
    }

    fn send(&mut self, report: &Report) -> bool {
        assert!(self.room > 0, "a report sent with no room for it");
        self.room = self.room.saturating_sub(1);
        let verdict = self.table.receive(report);
        assert!(
            self.hostile || verdict.is_ok(),
            "a report that breaks the protocol: {report:?}: {verdict:?}"
        );
        true
    }
}

/// The stack's last TCP segment to the peer: its ports, the sequence number
/// after it and the one it acknowledged.
#[derive(Clone, Copy, Default)]
struct Last {
    local_port: u16,
    remote_port: u16,
    next_seq: u32,
    ack: u32,
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at.saturating_add(2))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u16::from_be_bytes)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at.saturating_add(4))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

/// Checks one frame the stack sent, and notes it in `last` if it is a TCP
/// segment to the peer.
fn check_sent(frame: &[u8], allow: &AllowList, last: &mut Last) {
    assert!(
        (IP_AT..=MTU).contains(&frame.len()),
        "a {}-byte frame sent",
        frame.len()
    );
    assert_eq!(
        frame.get(6..12),
        Some(&MAC.0[..]),
        "a frame not from the stack's MAC address"
    );
    if frame.get(12..14) != Some(&[0x08, 0x00]) || frame.get(IP_AT.saturating_add(9)) != Some(&6) {
        return;
    }
    let header_len = usize::from(frame.get(IP_AT).copied().unwrap_or(0) & 0x0f).saturating_mul(4);
    let total = usize::from(be16(frame, IP_AT.saturating_add(2)));
    let tcp_at = IP_AT.saturating_add(header_len);
    let destination: [u8; 4] = frame
        .get(IP_AT.saturating_add(16)..IP_AT.saturating_add(20))
        .and_then(|b| b.try_into().ok())
        .unwrap_or_default();
    let remote_port = be16(frame, tcp_at.saturating_add(2));
    let flags = frame.get(tcp_at.saturating_add(13)).copied().unwrap_or(0);
    let offset = usize::from(frame.get(tcp_at.saturating_add(12)).copied().unwrap_or(0) >> 4)
        .saturating_mul(4);
    let payload = total.saturating_sub(header_len).saturating_sub(offset);
    let (syn, fin) = (flags & 0x02 != 0, flags & 0x01 != 0);
    if syn || payload > 0 {
        assert!(
            allow.permits(Ipv4Addr::from(destination), remote_port),
            "a segment that opens a connection or carries data to {}:{remote_port}",
            Ipv4Addr::from(destination)
        );
    }
    if Ipv4Addr::from(destination) == PEER {
        let seq = be32(frame, tcp_at.saturating_add(4));
        let len = u32::try_from(payload)
            .unwrap_or(0)
            .saturating_add(u32::from(syn))
            .saturating_add(u32::from(fin));
        *last = Last {
            local_port: be16(frame, tcp_at),
            remote_port,
            next_seq: seq.wrapping_add(len),
            ack: be32(frame, tcp_at.saturating_add(8)),
        };
    }
}

/// An ARP reply from the peer to the stack.
fn arp_reply() -> Vec<u8> {
    let mut frame = Vec::with_capacity(42);
    frame.extend_from_slice(&MAC.0);
    frame.extend_from_slice(&PEER_MAC);
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 2]);
    frame.extend_from_slice(&PEER_MAC);
    frame.extend_from_slice(&PEER.octets());
    frame.extend_from_slice(&MAC.0);
    frame.extend_from_slice(&OURS);
    frame
}

/// A TCP segment from the peer, of the fuzzer's making, following `last`
/// or not.
fn segment(input: &Input<'_>, last: &Last) -> Vec<u8> {
    let follows = !input.byte().is_multiple_of(4);
    let from_port = match input.byte() % 4 {
        0 => input.u16(),
        _ if follows => last.remote_port,
        choice => LISTED.get(usize::from(choice) % 2).copied().unwrap_or(7),
    };
    let to_port = if follows {
        last.local_port
    } else {
        input.u16()
    };
    let (seq, ack) = if follows {
        (last.ack, last.next_seq)
    } else {
        (input.u32(), input.u32())
    };
    // The usual flags most often: SYN-ACK, ACK, ACK with data, FIN, RST.
    let flags = match input.byte() % 8 {
        0 => 0x12,
        1 | 2 => 0x10,
        3 => 0x18,
        4 => 0x11,
        5 => 0x04,
        6 => 0x14,
        _ => input.byte(),
    };
    let options_len = usize::from(input.byte() % 4).saturating_mul(4);
    let payload_len = usize::from(input.u16()) % 1460;
    let tcp_len = 20usize
        .saturating_add(options_len)
        .saturating_add(payload_len);
    let ip_len = u16::try_from(tcp_len.saturating_add(20)).unwrap_or(u16::MAX);
    let mut frame = Vec::with_capacity(TCP_AT.saturating_add(tcp_len));
    frame.extend_from_slice(&MAC.0);
    frame.extend_from_slice(&PEER_MAC);
    frame.extend_from_slice(&[0x08, 0x00, 0x45, 0x00]);
    frame.extend_from_slice(&ip_len.to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0x40, 0, 64, 6, 0, 0]);
    frame.extend_from_slice(&PEER.octets());
    frame.extend_from_slice(&OURS);
    frame.extend_from_slice(&from_port.to_be_bytes());
    frame.extend_from_slice(&to_port.to_be_bytes());
    frame.extend_from_slice(&seq.to_be_bytes());
    frame.extend_from_slice(&ack.to_be_bytes());
    let offset = u8::try_from(options_len / 4).unwrap_or(0).saturating_add(5) << 4;
    frame.extend_from_slice(&[offset, flags]);
    frame.extend_from_slice(&input.u16().to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0, 0]);
    frame.extend((0..options_len).map(|_| input.byte()));
    frame.extend((0..payload_len).map(|_| input.byte()));
    frame
}

/// A request from the client, or none if the fuzzer's choice makes none.
fn request(input: &Input<'_>) -> Option<Result<Request, RecordError>> {
    let id = ConnectionId(u16::from(input.byte() % 6));
    match input.byte() % 5 {
        0 => {
            let (address, port) = match input.byte() % 4 {
                0 => (PEER, 7),
                1 => (PEER, 9),
                2 => (PEER, 8),
                _ => (Ipv4Addr::from(input.array::<4>()), input.u16()),
            };
            Connect::new(id, address, port).map(|connect| Ok(Request::Connect(connect)))
        }
        1 => {
            let len = usize::from(input.u16()) % DATA_LIMIT;
            let bytes: Vec<u8> = (0..len).map(|_| input.byte()).collect();
            SendData::new(id, &bytes).map(|data| Ok(Request::Send(data)))
        }
        2 => Some(Ok(Request::Close(Close::new(id)))),
        _ => {
            let len = usize::from(input.u16()) % RECORD_LIMIT;
            let bytes: Vec<u8> = (0..len).map(|_| input.byte()).collect();
            Some(Request::decode(&bytes))
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let allow = AllowList::new(&LISTED.map(|port| Destination {
        address: PEER,
        port,
    }));
    let Ok(allow) = allow else {
        return;
    };
    let seed =
        StackSeed::from_non_zero(NonZeroU64::new(input.u32().into()).unwrap_or(NonZeroU64::MIN));
    let mut wire = Wire::default();
    let mut room = storage();
    let mut tcp = Box::new(TcpRoom::new());
    let Ok(mut stack) = Stack::with_connections(
        MAC,
        seed,
        &mut wire,
        &mut room,
        &mut tcp,
        allow,
        Instant::from_nanos(0),
    ) else {
        return;
    };
    stack.assume_lease(Lease {
        address: Ipv4(OURS),
        prefix_len: 24,
        gateway: None,
    });
    let mut asks = Asks::default();
    let mut hears = Hears::default();
    let mut last = Last::default();
    let mut now = 0u64;
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        match input.byte() % 8 {
            0..=2 => {
                if let Some(request) = request(&input) {
                    if let Ok(request) = &request {
                        // Valid for the client's table, or sent anyway.
                        hears.hostile |= hears.table.send(request).is_err();
                    }
                    asks.0.push_back(request);
                }
            }
            3 | 4 => wire.inbox.push_back(segment(&input, &last)),
            5 => wire.inbox.push_back(arp_reply()),
            6 => {
                let len = usize::from(input.u16()) % FRAME_LIMIT;
                wire.inbox
                    .push_back((0..len).map(|_| input.byte()).collect());
            }
            _ => {
                let ms = u64::from(input.u16());
                now = now.saturating_add(ms.saturating_mul(MS));
            }
        }
        now = now.saturating_add(MS);
        hears.room = usize::from(input.byte() % 9);
        let at = Instant::from_nanos(now);
        let _ = stack.poll(&mut wire, at);
        stack.serve(at, &mut asks, &mut hears, &mut |notice: Notice| {
            let _ = notice.to_string();
        });
        let _ = stack.poll(&mut wire, at);
        for frame in wire.outbox.borrow_mut().drain(..) {
            check_sent(&frame, &allow, &mut last);
        }
    }
});
