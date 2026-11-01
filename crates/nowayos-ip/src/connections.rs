// SPDX-License-Identifier: MIT OR Apache-2.0
//! TCP connections for the stack's one socket client,
//! over `nowayos-socket`'s
//! records: the client asks, through its requests ring, for a connection
//! to an address and port, sends bytes and closes; the stack reports,
//! through its reports ring, the connection open, the bytes that arrive,
//! and its end. The client never sees a frame.
//!
//! * **Only listed destinations.** A connection is opened only to an
//!   address and port the system's allow list holds (`AllowList`); any
//!   other is refused with `FailReason::NotAllowed`, and the stack says so
//!   ([`Notice::Refused`]).
//! * **Every record checked before use**: decoded (`Request::decode`), then
//!   checked against the connection's state in the stack's table
//!   (`StackTable`); a record that fails is dropped and counted
//!   ([`Notice::Record`], [`Notice::Protocol`]). Every report the stack
//!   sends is checked by the same table first.
//! * **Bounded, and backed by TCP's own flow control.** Each connection has
//!   fixed buffers ([`TcpRoom`]). Bytes the client sends wait in the stack
//!   (one record at a time) while the connection's send buffer is full;
//!   bytes that arrive wait in its receive buffer - closing TCP's window -
//!   while the reports ring is full.
//! * **A connection that cannot be made ends with a reason**: refused by
//!   the host, or reset ([`FailReason::Refused`], `Reset`); no answer in
//!   [`CONNECT_TIMEOUT_SECS`] ([`FailReason::TimedOut`]); no lease within
//!   [`LEASE_WAIT_SECS`] of asking ([`FailReason::Unreachable`]); no free
//!   socket ([`FailReason::NoResources`]).

use core::fmt;

use nowayos_socket::{
    AllowList, Close, Closed, Connect, Connected, ConnectionId, Destination, FailReason, Failed,
    MAX_CONNECTIONS, MAX_DATA, ProtocolError, ReceivedData, RecordError, Report, Request,
    StackTable, Verdict,
};
use nowayos_time::Instant;
use smoltcp::iface::{Context, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{self, State};
use smoltcp::wire::{IpAddress, IpEndpoint};

/// Each connection's receive buffer, in bytes: its TCP window, 64 KiB, so
/// a host keeps a remote screen's redraws flowing while the client takes
/// the last ones (smoltcp scales the window to advertise it whole).
pub const RX_BUFFER_LEN: usize = 65536;

/// Each connection's send buffer, in bytes.
pub const TX_BUFFER_LEN: usize = 4096;

/// How long a connection may take to open before it fails, in seconds.
pub const CONNECT_TIMEOUT_SECS: u64 = 20;

/// How long a connection request waits for the stack's lease before it
/// fails, in seconds.
pub const LEASE_WAIT_SECS: u64 = 30;

/// How long a connection the client closed may take to end, in seconds,
/// before the stack abandons it.
pub const CLOSE_TIMEOUT_SECS: u64 = 10;

/// The first local port the stack uses, and how many there are: IANA's
/// dynamic ports, 49152 to 65535.
const EPHEMERAL_FIRST: u16 = 49152;
const EPHEMERAL_COUNT: u16 = 16384;

/// A second, in nanoseconds of monotonic time.
const SECOND: u64 = 1_000_000_000;

/// The buffers of every connection, which smoltcp borrows for as long as the
/// stack lives: a PD has no heap, so the caller keeps them. 272 KiB:
/// [`RX_BUFFER_LEN`] and [`TX_BUFFER_LEN`] for each of `MAX_CONNECTIONS`.
pub struct TcpRoom {
    rx: [[u8; RX_BUFFER_LEN]; MAX_CONNECTIONS],
    tx: [[u8; TX_BUFFER_LEN]; MAX_CONNECTIONS],
}

impl TcpRoom {
    /// Empty buffers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rx: [[0; RX_BUFFER_LEN]; MAX_CONNECTIONS],
            tx: [[0; TX_BUFFER_LEN]; MAX_CONNECTIONS],
        }
    }

    /// One socket per connection, each over its own buffers.
    pub(crate) fn sockets(&mut self) -> impl Iterator<Item = tcp::Socket<'_>> {
        self.rx.iter_mut().zip(self.tx.iter_mut()).map(|(rx, tx)| {
            tcp::Socket::new(
                tcp::SocketBuffer::new(&mut rx[..]),
                tcp::SocketBuffer::new(&mut tx[..]),
            )
        })
    }
}

impl Default for TcpRoom {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the client's requests come from: its ring, copied out record by
/// record.
pub trait Requests {
    /// The next request, decoded, or why it was refused; `None` when there
    /// is none.
    fn next(&mut self) -> Option<Result<Request, RecordError>>;
}

/// Where the stack's reports go: the client's ring.
pub trait Reports {
    /// Whether one more report fits now.
    fn has_room(&self) -> bool;
    /// Sends `report`; `false` if it did not fit.
    fn send(&mut self, report: &Report) -> bool;
}

/// What the stack's PD says about its connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// A connection asked for and refused: the destination is not listed.
    Refused {
        /// The client's name for it.
        connection: ConnectionId,
        /// Where it asked to go.
        destination: Destination,
    },
    /// A connection open.
    Opened {
        /// The client's name for it.
        connection: ConnectionId,
        /// Where to.
        destination: Destination,
    },
    /// A connection over, cleanly or with a reason; one refused is
    /// [`Notice::Refused`] alone.
    Ended {
        /// The client's name for it.
        connection: ConnectionId,
        /// `None` for a clean close.
        reason: Option<FailReason>,
    },
    /// A request that did not decode, dropped.
    Record(RecordError),
    /// A request the table refused, dropped.
    Protocol(ProtocolError),
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused {
                connection,
                destination,
            } => write!(
                f,
                "refused {connection} to {destination}: not in the system's allow list"
            ),
            Self::Opened {
                connection,
                destination,
            } => write!(f, "{connection} to {destination} open"),
            Self::Ended {
                connection,
                reason: None,
            } => write!(f, "{connection} closed"),
            Self::Ended {
                connection,
                reason: Some(reason),
            } => write!(f, "{connection} failed: {reason}"),
            Self::Record(error) => write!(f, "a request dropped: {error}"),
            Self::Protocol(error) => write!(f, "a request dropped: {error}"),
        }
    }
}

/// Where a connection is, as the stack sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// SYN sent; the client waits for `Connected` or `Failed`.
    Opening,
    /// `Connected` sent: bytes flow.
    Open,
    /// The stack has sent its end (`Closed` or `Failed`); it waits for the
    /// client's `Close`, if it has not seen it.
    Ended,
}

/// One connection, in the socket at `slot`.
#[derive(Debug, Clone, Copy)]
struct Conn {
    id: ConnectionId,
    destination: Destination,
    phase: Phase,
    /// When the phase began, or the client closed: for the timeouts.
    since: Instant,
    /// The client's `Close` seen.
    client_closed: bool,
}

/// A request the stack has taken but not yet finished with. One at most,
/// held in place: a PD has no heap to box the larger variant into.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one at most, held in place: a PD has no heap"
)]
enum Pending {
    /// A `Connect`, waiting for the stack's lease, since then.
    Connect(Connect, Instant),
    /// A `SendData`, its first `sent` bytes in the socket.
    Send(nowayos_socket::SendData, usize),
}

/// The connections of the stack's one client.
pub(crate) struct Connections {
    handles: [SocketHandle; MAX_CONNECTIONS],
    conns: [Option<Conn>; MAX_CONNECTIONS],
    table: StackTable,
    allow: AllowList,
    pending: Option<Pending>,
    next_port: u16,
}

impl Connections {
    /// The connections over the sockets `handles`, to `allow`'s
    /// destinations, their local ports starting from `seed`'s choice.
    pub(crate) fn new(
        handles: [SocketHandle; MAX_CONNECTIONS],
        allow: AllowList,
        seed: u64,
    ) -> Self {
        Self {
            handles,
            conns: [None; MAX_CONNECTIONS],
            table: StackTable::new(),
            allow,
            pending: None,
            next_port: seed
                .checked_rem(u64::from(EPHEMERAL_COUNT))
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(0),
        }
    }

    /// One round: the client's requests, then every connection's news, as
    /// reports. `leased` says whether the stack has an address.
    #[allow(
        clippy::too_many_arguments,
        reason = "the round's whole context, passed on"
    )]
    pub(crate) fn serve(
        &mut self,
        cx: &mut Context,
        sockets: &mut SocketSet<'_>,
        leased: bool,
        now: Instant,
        requests: &mut impl Requests,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        self.take_requests(cx, sockets, leased, now, requests, reports, notices);
        self.follow(sockets, now, reports, notices);
    }

    /// Takes the client's requests until one must wait: a `Connect` for a
    /// lease, data for room in its socket, a report for room in the ring.
    #[allow(
        clippy::too_many_arguments,
        reason = "the round's whole context, passed on"
    )]
    fn take_requests(
        &mut self,
        cx: &mut Context,
        sockets: &mut SocketSet<'_>,
        leased: bool,
        now: Instant,
        requests: &mut impl Requests,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        loop {
            if let Some(pending) = self.pending.take() {
                if !self.finish(pending, cx, sockets, leased, now, reports, notices) {
                    return;
                }
                continue;
            }
            // A request may need a report at once: only take one while one
            // more fits.
            if !reports.has_room() {
                return;
            }
            let request = match requests.next() {
                None => return,
                Some(Err(error)) => {
                    notices(Notice::Record(error));
                    continue;
                }
                Some(Ok(request)) => request,
            };
            match self.table.receive(&request) {
                Err(error) => notices(Notice::Protocol(error)),
                Ok(Verdict::Ignore) => {}
                Ok(Verdict::Act) => {
                    let pending = match request {
                        Request::Connect(connect) => Pending::Connect(connect, now),
                        Request::Send(data) => Pending::Send(data, 0),
                        Request::Close(close) => {
                            self.client_close(close, sockets, now);
                            continue;
                        }
                    };
                    if !self.finish(pending, cx, sockets, leased, now, reports, notices) {
                        return;
                    }
                }
            }
        }
    }

    /// Acts on `pending`; `false` if it must wait, kept for the next round.
    #[allow(
        clippy::too_many_arguments,
        reason = "the round's whole context, passed on"
    )]
    fn finish(
        &mut self,
        pending: Pending,
        cx: &mut Context,
        sockets: &mut SocketSet<'_>,
        leased: bool,
        now: Instant,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) -> bool {
        match pending {
            Pending::Connect(connect, asked) => {
                let destination = Destination {
                    address: connect.address(),
                    port: connect.port(),
                };
                let id = connect.connection();
                let opened = if !self.allow.permits(destination.address, destination.port) {
                    Err(FailReason::NotAllowed)
                } else if leased {
                    self.open(id, destination, cx, sockets, now)
                } else if seconds_since(asked, now) >= LEASE_WAIT_SECS {
                    Err(FailReason::Unreachable)
                } else {
                    self.pending = Some(Pending::Connect(connect, asked));
                    return false;
                };
                let Err(reason) = opened else {
                    return true;
                };
                if self.fail(id, destination, reason, reports, notices) {
                    return true;
                }
                // No room for the report: the request waits for some.
                self.pending = Some(Pending::Connect(connect, asked));
                false
            }
            Pending::Send(data, sent) => {
                let id = data.connection();
                let Some(slot) = self.slot_of(id) else {
                    return true;
                };
                let socket = sockets.get_mut::<tcp::Socket>(self.handle(slot));
                let rest = data.bytes().get(sent..).unwrap_or_default();
                let taken = socket.send_slice(rest).unwrap_or(0);
                let sent = sent.saturating_add(taken);
                if sent < data.bytes().len() {
                    self.pending = Some(Pending::Send(data, sent));
                    return false;
                }
                true
            }
        }
    }

    /// Opens a connection to `destination` in a free socket.
    fn open(
        &mut self,
        id: ConnectionId,
        destination: Destination,
        cx: &mut Context,
        sockets: &mut SocketSet<'_>,
        now: Instant,
    ) -> Result<(), FailReason> {
        let slot = (0..MAX_CONNECTIONS)
            .find(|slot| {
                self.conns.get(*slot).is_some_and(Option::is_none)
                    && sockets.get::<tcp::Socket>(self.handle(*slot)).state() == State::Closed
            })
            .ok_or(FailReason::NoResources)?;
        let port = EPHEMERAL_FIRST.saturating_add(self.next_port % EPHEMERAL_COUNT);
        self.next_port = self.next_port.wrapping_add(1) % EPHEMERAL_COUNT;
        let remote = IpEndpoint::new(IpAddress::Ipv4(destination.address), destination.port);
        sockets
            .get_mut::<tcp::Socket>(self.handle(slot))
            .connect(cx, remote, port)
            .map_err(|_| FailReason::Unreachable)?;
        if let Some(conn) = self.conns.get_mut(slot) {
            *conn = Some(Conn {
                id,
                destination,
                phase: Phase::Opening,
                since: now,
                client_closed: false,
            });
        }
        Ok(())
    }

    /// The client is done with a connection: FIN after whatever it queued;
    /// once the stack has sent its end too, the connection is gone.
    fn client_close(&mut self, close: Close, sockets: &mut SocketSet<'_>, now: Instant) {
        let Some(slot) = self.slot_of(close.connection()) else {
            return;
        };
        let handle = self.handle(slot);
        let Some(Some(conn)) = self.conns.get_mut(slot) else {
            return;
        };
        conn.client_closed = true;
        if conn.phase == Phase::Ended {
            sockets.get_mut::<tcp::Socket>(handle).abort();
            self.release(slot);
        } else {
            conn.since = now;
            sockets.get_mut::<tcp::Socket>(handle).close();
        }
    }

    /// Every connection's news: open, bytes in, its end.
    fn follow(
        &mut self,
        sockets: &mut SocketSet<'_>,
        now: Instant,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        for slot in 0..MAX_CONNECTIONS {
            let Some(Some(conn)) = self.conns.get(slot).copied() else {
                continue;
            };
            let handle = self.handle(slot);
            match conn.phase {
                Phase::Opening => {
                    let state = sockets.get::<tcp::Socket>(handle).state();
                    if state == State::Established {
                        if self.report(&Report::Connected(Connected::new(conn.id)), reports) {
                            notices(Notice::Opened {
                                connection: conn.id,
                                destination: conn.destination,
                            });
                            self.set_phase(slot, Phase::Open, now);
                        }
                    } else if state == State::Closed {
                        self.end(
                            slot,
                            Some(FailReason::Refused),
                            sockets,
                            now,
                            reports,
                            notices,
                        );
                    } else if seconds_since(conn.since, now) >= CONNECT_TIMEOUT_SECS {
                        sockets.get_mut::<tcp::Socket>(handle).abort();
                        self.end(
                            slot,
                            Some(FailReason::TimedOut),
                            sockets,
                            now,
                            reports,
                            notices,
                        );
                    }
                }
                Phase::Open => self.follow_open(slot, conn, sockets, now, reports, notices),
                Phase::Ended => {}
            }
        }
    }

    /// An open connection: bytes in, while the ring has room; then its end,
    /// once every byte is out.
    fn follow_open(
        &mut self,
        slot: usize,
        conn: Conn,
        sockets: &mut SocketSet<'_>,
        now: Instant,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        let handle = self.handle(slot);
        while reports.has_room() {
            let mut bytes = [0u8; MAX_DATA];
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            let len = match socket.recv_slice(&mut bytes) {
                Ok(len) if len > 0 => len,
                _ => break,
            };
            let Some(data) = bytes
                .get(..len)
                .and_then(|bytes| ReceivedData::new(conn.id, bytes))
            else {
                break;
            };
            if !self.report(&Report::Received(data), reports) {
                break;
            }
        }
        let socket = sockets.get::<tcp::Socket>(handle);
        if socket.recv_queue() > 0 {
            return;
        }
        let state = socket.state();
        let reason = match state {
            // The peer's FIN: nothing more will arrive.
            State::CloseWait | State::LastAck | State::TimeWait | State::Closing => None,
            // Closed with no FIN from the peer: reset - unless the client's
            // close finished it.
            State::Closed if conn.client_closed => None,
            State::Closed => Some(FailReason::Reset),
            _ => {
                if conn.client_closed && seconds_since(conn.since, now) >= CLOSE_TIMEOUT_SECS {
                    sockets.get_mut::<tcp::Socket>(handle).abort();
                    None
                } else {
                    return;
                }
            }
        };
        self.end(slot, reason, sockets, now, reports, notices);
    }

    /// The stack's end of a connection: `Closed`, or `Failed` with
    /// `reason`; the connection is gone once the client's `Close` is in.
    fn end(
        &mut self,
        slot: usize,
        reason: Option<FailReason>,
        sockets: &mut SocketSet<'_>,
        now: Instant,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        let Some(Some(conn)) = self.conns.get(slot).copied() else {
            return;
        };
        let report = match reason {
            None => Report::Closed(Closed::new(conn.id)),
            Some(reason) => Report::Failed(Failed::new(conn.id, reason)),
        };
        if !self.report(&report, reports) {
            return;
        }
        notices(Notice::Ended {
            connection: conn.id,
            reason,
        });
        let handle = self.handle(slot);
        if conn.client_closed {
            // Both ends are in: the socket is free once TCP is done with it.
            if sockets.get::<tcp::Socket>(handle).state() != State::TimeWait {
                sockets.get_mut::<tcp::Socket>(handle).abort();
            }
            self.release(slot);
        } else {
            // The peer is done; finish our side of it.
            sockets.get_mut::<tcp::Socket>(handle).close();
            self.set_phase(slot, Phase::Ended, now);
        }
    }

    /// `Failed` for a connection to `destination` that never got a socket,
    /// and its notice - a refusal says why; `false` if the report must
    /// wait.
    fn fail(
        &mut self,
        id: ConnectionId,
        destination: Destination,
        reason: FailReason,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) -> bool {
        if !self.report(&Report::Failed(Failed::new(id, reason)), reports) {
            return false;
        }
        notices(if reason == FailReason::NotAllowed {
            Notice::Refused {
                connection: id,
                destination,
            }
        } else {
            Notice::Ended {
                connection: id,
                reason: Some(reason),
            }
        });
        true
    }

    /// Sends `report`, checked by the table first; `false` if the ring had
    /// no room or the table refused it (never, for what this module sends).
    fn report(&mut self, report: &Report, reports: &mut impl Reports) -> bool {
        if !reports.has_room() || self.table.send(report).is_err() {
            return false;
        }
        reports.send(report)
    }

    fn set_phase(&mut self, slot: usize, phase: Phase, now: Instant) {
        if let Some(Some(conn)) = self.conns.get_mut(slot) {
            conn.phase = phase;
            conn.since = now;
        }
    }

    fn release(&mut self, slot: usize) {
        if let Some(conn) = self.conns.get_mut(slot) {
            *conn = None;
        }
    }

    fn slot_of(&self, id: ConnectionId) -> Option<usize> {
        self.conns
            .iter()
            .position(|conn| conn.is_some_and(|conn| conn.id == id))
    }

    fn handle(&self, slot: usize) -> SocketHandle {
        self.handles
            .get(slot)
            .or(self.handles.first())
            .copied()
            .unwrap_or_default()
    }

    /// Whether a connection is in flight: smoltcp must be polled for it.
    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some() || self.conns.iter().any(Option::is_some)
    }
}

/// Whole seconds from `then` to `now` (0 if `now` is earlier).
fn seconds_since(then: Instant, now: Instant) -> u64 {
    now.as_nanos()
        .saturating_sub(then.as_nanos())
        .checked_div(SECOND)
        .unwrap_or(0)
}
