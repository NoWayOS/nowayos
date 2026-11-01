// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stack's TCP connections for its socket client, against a
//! host on the same wire: a second smoltcp interface at 10.0.2.100 with an
//! echo service on port 7, a source of [`SOURCE_LEN`] bytes on port 19 and
//! nothing on port 9. The client's requests and
//! the stack's reports are `nowayos-socket`'s records, and the client's own
//! table (`ClientTable`) checks every report the stack sends.
//!
//! * a connection to a listed destination opens, carries bytes both ways
//!   and closes;
//! * a stream five 64 KiB windows long arrives whole and in order, in
//!   records of up to `MAX_DATA` bytes, through a ring that holds
//!   `RING_SLOTS` a millisecond, in the time only the 64 KiB window and the
//!   larger records allow;
//! * one to a destination the allow list does not hold is refused, and the
//!   stack says so;
//! * one asked for before the stack has a lease waits for it, and fails
//!   unreachable if none comes;
//! * one to a listed port nobody listens on is refused by the host;
//! * a record that breaks the protocol is dropped and reported;
//! * and whatever records the client sends, the stack never panics and
//!   every report it sends keeps the protocol (property test).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    missing_docs,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::num::NonZeroU64;
use std::rc::Rc;

use nowayos_ip::smoltcp;
use nowayos_ip::{Lease, Notice, Reports, Requests, Stack, StackSeed, TcpRoom, storage};
use nowayos_net::arp::{Ipv4, Mac};
use nowayos_socket::{
    AllowList, ClientTable, Close, Connect, ConnectionId, Destination, FailReason, MAX_DATA,
    RING_SLOTS, RecordError, Report, Request, SendData,
};
use nowayos_time::Instant;
use proptest::prelude::*;
use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as NetInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Cidr};

const OUR_MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
const HOST_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x64];
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 100);
const MS: u64 = 1_000_000;

/// The bytes the host's source sends each connection, then closes: five
/// 64 KiB windows.
const SOURCE_LEN: usize = 5 * 65536;

/// The source's byte at `offset`: a pattern that a byte lost, repeated or
/// out of order breaks.
fn source_byte(offset: usize) -> u8 {
    (offset % 251) as u8
}

/// One end of an Ethernet wire: frames in, frames out.
struct End {
    inbox: Rc<RefCell<VecDeque<Vec<u8>>>>,
    outbox: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

fn wire() -> (End, End) {
    let a = Rc::new(RefCell::new(VecDeque::new()));
    let b = Rc::new(RefCell::new(VecDeque::new()));
    (
        End {
            inbox: a.clone(),
            outbox: b.clone(),
        },
        End {
            inbox: b,
            outbox: a,
        },
    )
}

struct Rx(Vec<u8>);
struct Tx(Rc<RefCell<VecDeque<Vec<u8>>>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let result = f(&mut frame);
        self.0.borrow_mut().push_back(frame);
        result
    }
}

impl Device for End {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx;

    fn receive(&mut self, _: NetInstant) -> Option<(Rx, Tx)> {
        let frame = self.inbox.borrow_mut().pop_front()?;
        Some((Rx(frame), Tx(self.outbox.clone())))
    }

    fn transmit(&mut self, _: NetInstant) -> Option<Tx> {
        Some(Tx(self.outbox.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = 1514;
        capabilities
    }
}

/// The host: an echo service on port 7, listening again after each
/// connection; a source on port 19, which sends [`SOURCE_LEN`] bytes and
/// closes; nothing on any other port.
struct Host {
    end: End,
    interface: Interface,
    sockets: SocketSet<'static>,
    echo: smoltcp::iface::SocketHandle,
    source: smoltcp::iface::SocketHandle,
    sourced: usize,
}

impl Host {
    fn new(end: End) -> Self {
        let mut end = end;
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(HOST_MAC)));
        config.random_seed = 7;
        let mut interface = Interface::new(config, &mut end, NetInstant::from_micros(0));
        interface.update_ip_addrs(|addrs| {
            addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(HOST, 24))).unwrap();
        });
        let storage: &'static mut [SocketStorage<'static>; 2] =
            Box::leak(Box::new([SocketStorage::EMPTY, SocketStorage::EMPTY]));
        let mut sockets = SocketSet::new(&mut storage[..]);
        let socket = |rx_len: usize, tx_len: usize, port: u16| {
            let rx: &'static mut [u8] = Box::leak(vec![0; rx_len].into_boxed_slice());
            let tx: &'static mut [u8] = Box::leak(vec![0; tx_len].into_boxed_slice());
            let mut socket =
                tcp::Socket::new(tcp::SocketBuffer::new(rx), tcp::SocketBuffer::new(tx));
            socket.listen(port).unwrap();
            socket
        };
        let echo = sockets.add(socket(4096, 4096, 7));
        let source = sockets.add(socket(4096, 65536, 19));
        Self {
            end,
            interface,
            sockets,
            echo,
            source,
            sourced: 0,
        }
    }

    fn poll(&mut self, now: u64) {
        let now = NetInstant::from_micros((now / 1000) as i64);
        self.interface.poll(now, &mut self.end, &mut self.sockets);
        let socket = self.sockets.get_mut::<tcp::Socket>(self.echo);
        let mut buf = [0u8; 1024];
        while socket.can_recv() && socket.can_send() {
            let len = socket.recv_slice(&mut buf).unwrap();
            socket.send_slice(&buf[..len]).unwrap();
        }
        if socket.state() == tcp::State::CloseWait {
            socket.close();
        }
        if socket.state() == tcp::State::Closed {
            socket.listen(7).unwrap();
        }
        let socket = self.sockets.get_mut::<tcp::Socket>(self.source);
        while socket.may_send() && self.sourced < SOURCE_LEN {
            let sourced = self.sourced;
            let sent = socket
                .send(|buf| {
                    let len = buf.len().min(SOURCE_LEN - sourced);
                    for (offset, byte) in buf[..len].iter_mut().enumerate() {
                        *byte = source_byte(sourced + offset);
                    }
                    (len, len)
                })
                .unwrap();
            if sent == 0 {
                break;
            }
            self.sourced += sent;
        }
        if self.sourced == SOURCE_LEN && socket.state() == tcp::State::Established {
            socket.close();
        }
        self.interface.poll(now, &mut self.end, &mut self.sockets);
    }
}

/// What the client asks, in order.
#[derive(Default)]
struct Asks(VecDeque<Result<Request, RecordError>>);

impl Requests for Asks {
    fn next(&mut self) -> Option<Result<Request, RecordError>> {
        self.0.pop_front()
    }
}

/// What the client hears, checked by its own table, and its ring's room.
/// Once the client sends a request its own table refuses, the two tables
/// need not agree, and only the stack's guards it: reports are checked to
/// keep the protocol only while the client keeps it.
#[derive(Default)]
struct Hears {
    table: ClientTable,
    reports: Vec<Report>,
    room: usize,
    hostile: bool,
}

impl Reports for Hears {
    fn has_room(&self) -> bool {
        self.room > 0
    }

    fn send(&mut self, report: &Report) -> bool {
        assert!(self.room > 0);
        self.room -= 1;
        // The client's own table checks every report: one it would
        // ignore - data after it closed - keeps the protocol too.
        let verdict = self.table.receive(report);
        assert!(self.hostile || verdict.is_ok(), "{report:?}: {verdict:?}");
        self.reports.push(report.clone());
        true
    }
}

/// The stack's world: its wire end, the host, the client, the time.
struct World {
    ours: End,
    host: Host,
    asks: Asks,
    hears: Hears,
    notices: Vec<Notice>,
    now: u64,
}

impl World {
    fn new() -> Self {
        let (ours, theirs) = wire();
        Self {
            ours,
            host: Host::new(theirs),
            asks: Asks::default(),
            hears: Hears::default(),
            notices: Vec::new(),
            now: 0,
        }
    }

    /// The client sends `request`, checked by its table.
    fn ask(&mut self, request: Request) {
        self.hears.table.send(&request).unwrap();
        self.asks.0.push_back(Ok(request));
    }

    /// Runs everything for `ms` milliseconds, a millisecond a step, the
    /// client's ring with room for `room` reports a step.
    fn run(&mut self, stack: &mut Stack<'_>, ms: u64) {
        for _ in 0..ms {
            self.now += MS;
            let now = Instant::from_nanos(self.now);
            self.hears.room = RING_SLOTS;
            stack.poll(&mut self.ours, now);
            let notices = &mut self.notices;
            stack.serve(now, &mut self.asks, &mut self.hears, &mut |notice| {
                notices.push(notice);
            });
            stack.poll(&mut self.ours, now);
            self.host.poll(self.now);
        }
    }

    fn take_reports(&mut self) -> Vec<Report> {
        std::mem::take(&mut self.hears.reports)
    }
}

fn allowed() -> AllowList {
    AllowList::new(&[
        Destination {
            address: HOST,
            port: 7,
        },
        Destination {
            address: HOST,
            port: 9,
        },
        Destination {
            address: HOST,
            port: 19,
        },
    ])
    .unwrap()
}

fn seed() -> StackSeed {
    StackSeed::from_non_zero(NonZeroU64::new(0x5eed).unwrap())
}

fn lease() -> Lease {
    Lease {
        address: Ipv4([10, 0, 2, 15]),
        prefix_len: 24,
        gateway: None,
    }
}

fn connect(id: u16, port: u16) -> Request {
    Request::Connect(Connect::new(ConnectionId(id), HOST, port).unwrap())
}

#[test]
fn a_listed_connection_opens_carries_bytes_both_ways_and_closes() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    stack.assume_lease(lease());
    world.ask(connect(1, 7));
    world.run(&mut stack, 200);
    assert_eq!(
        world.take_reports(),
        [Report::Connected(nowayos_socket::Connected::new(
            ConnectionId(1)
        ))]
    );
    let message = b"nowayos tcp echo, both ways";
    world.ask(Request::Send(
        SendData::new(ConnectionId(1), message).unwrap(),
    ));
    world.run(&mut stack, 200);
    let echoed: Vec<u8> = world
        .take_reports()
        .iter()
        .flat_map(|report| match report {
            Report::Received(data) => data.bytes().to_vec(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(echoed, message);
    world.ask(Request::Close(Close::new(ConnectionId(1))));
    world.run(&mut stack, 500);
    assert_eq!(
        world.take_reports(),
        [Report::Closed(nowayos_socket::Closed::new(ConnectionId(1)))]
    );
    assert!(world.hears.table.is_empty(), "the name is free again");
    assert!(
        world
            .notices
            .iter()
            .any(|n| matches!(n, Notice::Opened { .. }))
    );
    // The name can be used again, for a second connection.
    world.ask(connect(1, 7));
    world.run(&mut stack, 200);
    assert_eq!(
        world.take_reports(),
        [Report::Connected(nowayos_socket::Connected::new(
            ConnectionId(1)
        ))]
    );
}

#[test]
fn a_stream_of_five_windows_arrives_whole_in_order_and_fast_in_large_records() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    stack.assume_lease(lease());
    world.ask(connect(1, 19));
    let mut ms = 0;
    while !matches!(world.hears.reports.last(), Some(Report::Closed(_))) {
        assert!(ms < 1000, "the stream is not in after a second");
        world.run(&mut stack, 1);
        ms += 1;
    }
    // 14 ms: the host keeps a 64 KiB window in flight, and the ring takes
    // 32 records of up to 976 bytes a millisecond. An 8 KiB window took
    // 47 ms; version 1's eight 240-byte records would take 171 at the least.
    assert!(ms <= 25, "the stream took {ms} ms");
    let reports = world.take_reports();
    assert_eq!(
        reports.first(),
        Some(&Report::Connected(nowayos_socket::Connected::new(
            ConnectionId(1)
        )))
    );
    assert_eq!(
        reports.last(),
        Some(&Report::Closed(nowayos_socket::Closed::new(ConnectionId(
            1
        ))))
    );
    let records: Vec<&[u8]> = reports[1..reports.len() - 1]
        .iter()
        .map(|report| match report {
            Report::Received(data) => data.bytes(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(records.iter().all(|bytes| bytes.len() <= MAX_DATA));
    let full = records
        .iter()
        .filter(|bytes| bytes.len() == MAX_DATA)
        .count();
    assert!(full * 2 > records.len(), "{full} of {} full", records.len());
    let stream: Vec<u8> = records.concat();
    assert_eq!(stream.len(), SOURCE_LEN);
    assert!(
        stream
            .iter()
            .enumerate()
            .all(|(offset, byte)| *byte == source_byte(offset)),
        "every byte in order"
    );
}

#[test]
fn a_destination_not_listed_is_refused_and_said_so() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    stack.assume_lease(lease());
    world.ask(connect(1, 8));
    world.ask(Request::Connect(
        Connect::new(ConnectionId(2), Ipv4Addr::new(10, 0, 2, 2), 7).unwrap(),
    ));
    world.run(&mut stack, 10);
    assert_eq!(
        world.take_reports(),
        [
            Report::Failed(nowayos_socket::Failed::new(
                ConnectionId(1),
                FailReason::NotAllowed
            )),
            Report::Failed(nowayos_socket::Failed::new(
                ConnectionId(2),
                FailReason::NotAllowed
            )),
        ]
    );
    assert_eq!(
        world.notices[0],
        Notice::Refused {
            connection: ConnectionId(1),
            destination: Destination {
                address: HOST,
                port: 8
            }
        }
    );
    assert_eq!(
        world.notices[0].to_string(),
        "refused connection 1 to 10.0.2.100:8: not in the system's allow list"
    );
    // One notice a refusal, which says why.
    assert_eq!(world.notices.len(), 2, "{:?}", world.notices);
    assert!(matches!(
        world.notices[1],
        Notice::Refused {
            connection: ConnectionId(2),
            ..
        }
    ));
    // Nothing went on the wire for either.
    assert!(
        world
            .host
            .end
            .inbox
            .borrow()
            .iter()
            .all(|frame| frame.len() < 54 || frame[23] != 6)
    );
}

#[test]
fn a_connection_waits_for_the_lease_and_fails_unreachable_without_one() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    world.ask(connect(1, 7));
    world.run(&mut stack, 1000);
    assert_eq!(world.take_reports(), []);
    // The lease arrives: the connection goes ahead.
    stack.assume_lease(lease());
    world.run(&mut stack, 200);
    assert_eq!(
        world.take_reports(),
        [Report::Connected(nowayos_socket::Connected::new(
            ConnectionId(1)
        ))]
    );

    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    world.ask(connect(1, 7));
    world.run(&mut stack, 1);
    world.now += 31_000 * MS;
    world.run(&mut stack, 1);
    assert_eq!(
        world.take_reports(),
        [Report::Failed(nowayos_socket::Failed::new(
            ConnectionId(1),
            FailReason::Unreachable
        ))]
    );
}

#[test]
fn a_listed_port_nobody_listens_on_is_refused_by_the_host() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    stack.assume_lease(lease());
    world.ask(connect(3, 9));
    world.run(&mut stack, 300);
    assert_eq!(
        world.take_reports(),
        [Report::Failed(nowayos_socket::Failed::new(
            ConnectionId(3),
            FailReason::Refused
        ))]
    );
    world.ask(Request::Close(Close::new(ConnectionId(3))));
    world.run(&mut stack, 5);
    assert!(world.hears.table.is_empty());
}

#[test]
fn a_record_that_breaks_the_protocol_is_dropped_and_reported() {
    let mut world = World::new();
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut stack = Stack::with_connections(
        OUR_MAC,
        seed(),
        &mut world.ours,
        &mut room,
        &mut tcp,
        allowed(),
        Instant::from_nanos(0),
    )
    .unwrap();
    stack.assume_lease(lease());
    // Data for a connection never opened, and a record that did not decode.
    world.asks.0.push_back(Ok(Request::Send(
        SendData::new(ConnectionId(9), b"x").unwrap(),
    )));
    world
        .asks
        .0
        .push_back(Err(RecordError::UnknownKind { kind: 0x0581 }));
    world.run(&mut stack, 5);
    assert_eq!(world.take_reports(), []);
    assert!(matches!(world.notices[0], Notice::Protocol(_)));
    assert!(matches!(world.notices[1], Notice::Record(_)));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Whatever records the client sends - valid for its table or not -
    /// the stack never panics, and every report keeps the protocol (the
    /// client's table checks each).
    #[test]
    fn any_records_never_break_the_stack(
        records in proptest::collection::vec(
            (0u8..4, 0u16..6, 0u16..12, proptest::collection::vec(any::<u8>(), 1..64)),
            0..24,
        )
    ) {
        let mut world = World::new();
        let mut room = storage();
        let mut tcp = TcpRoom::new();
        let mut stack = Stack::with_connections(
            OUR_MAC, seed(), &mut world.ours, &mut room, &mut tcp, allowed(), Instant::from_nanos(0),
        )
        .unwrap();
        stack.assume_lease(lease());
        for (kind, id, port, bytes) in records {
            let id = ConnectionId(id);
            let request = match kind {
                0 => Request::Connect(Connect::new(id, HOST, port.max(1)).unwrap()),
                1 => Request::Send(SendData::new(id, &bytes).unwrap()),
                2 => Request::Close(Close::new(id)),
                _ => {
                    world.asks.0.push_back(Err(RecordError::UnknownKind { kind: 1 }));
                    continue;
                }
            };
            // Valid for the client's table, or sent anyway.
            world.hears.hostile |= world.hears.table.send(&request).is_err();
            world.asks.0.push_back(Ok(request));
            world.run(&mut stack, 3);
        }
        world.run(&mut stack, 100);
    }
}
