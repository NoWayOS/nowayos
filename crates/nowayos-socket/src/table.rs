// SPDX-License-Identifier: MIT OR Apache-2.0
//! Each side's connection table: which connections are open, and what each
//! may carry next (see the crate documentation, "A connection's life").
//!
//! Both sides keep the same three facts about a connection: whether
//! [`Connected`](crate::Connected) has passed between them, whether the
//! client's [`Close`](crate::Close) has, and whether the stack's end -
//! [`Closed`](crate::Closed) or [`Failed`](crate::Failed) - has. Each side
//! learns its own records' facts when it sends them and the peer's when they
//! arrive, so the two tables can differ by what is in flight. The rules for
//! a record *received* allow for that: they accept everything an honest peer
//! may have sent before it saw this side's latest record, and nothing else.
//! The rules for a record *sent* are a side's check on itself.

use core::fmt;

use crate::records::{ConnectionId, Report, Request};

/// The most connections each side's table holds, by default: the kiosk's
/// backend needs one at a time, and a client must not tie up more of the
/// stack than this. Both ends of a ring pair use the same capacity, so the
/// stack refuses a [`Connect`](crate::Connect) beyond it only from a client
/// that broke its own table's limit.
pub const MAX_CONNECTIONS: usize = 4;

/// What one side knows of a connection in its table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ConnectionState {
    /// [`Connected`](crate::Connected) has passed: data may flow.
    pub connected: bool,
    /// The client's [`Close`](crate::Close) has passed: it sends nothing
    /// more for the connection.
    pub client_closed: bool,
    /// The stack's end, [`Closed`](crate::Closed) or
    /// [`Failed`](crate::Failed), has passed: it reports nothing more for
    /// the connection.
    pub stack_ended: bool,
}

/// What a side does with a record its table accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Act on it: open the connection, deliver the data, close, or tell the
    /// application.
    Act,
    /// Valid, but sent before the peer learned the connection was ending -
    /// data in flight towards a side that has already ended it, say.
    /// Nothing to do.
    Ignore,
}

/// Why a table refused a record. The table is unchanged, and the record is
/// not acted on: an honest peer never sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The record names a connection that is not in the table.
    NotOpen {
        /// The connection named.
        connection: ConnectionId,
    },
    /// A [`Connect`](crate::Connect) names a connection already in the
    /// table: its name is not free until both sides have ended it.
    AlreadyOpen {
        /// The connection named.
        connection: ConnectionId,
    },
    /// A [`Connect`](crate::Connect) would open more connections than the
    /// table holds.
    TableFull {
        /// The connection it would have opened.
        connection: ConnectionId,
        /// The table's capacity.
        capacity: usize,
    },
    /// Data before [`Connected`](crate::Connected).
    DataBeforeConnected {
        /// The connection named.
        connection: ConnectionId,
    },
    /// Data after its sender ended the connection, or saw it ended.
    DataAfterClose {
        /// The connection named.
        connection: ConnectionId,
    },
    /// A second [`Connected`](crate::Connected).
    AlreadyConnected {
        /// The connection named.
        connection: ConnectionId,
    },
    /// [`Connected`](crate::Connected) after the stack's end, or - sent by
    /// the stack - after the client's [`Close`](crate::Close), which the
    /// stack answers with [`Closed`](crate::Closed) instead.
    ConnectedAfterClose {
        /// The connection named.
        connection: ConnectionId,
    },
    /// A second [`Close`](crate::Close), or a second end from the stack.
    AlreadyClosed {
        /// The connection named.
        connection: ConnectionId,
    },
}

impl ProtocolError {
    /// The connection the refused record named.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match *self {
            Self::NotOpen { connection }
            | Self::AlreadyOpen { connection }
            | Self::TableFull { connection, .. }
            | Self::DataBeforeConnected { connection }
            | Self::DataAfterClose { connection }
            | Self::AlreadyConnected { connection }
            | Self::ConnectedAfterClose { connection }
            | Self::AlreadyClosed { connection } => connection,
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpen { connection } => write!(f, "{connection} is not open"),
            Self::AlreadyOpen { connection } => write!(f, "{connection} is already open"),
            Self::TableFull {
                connection,
                capacity,
            } => write!(
                f,
                "{connection} would be one more than the {capacity} the table holds"
            ),
            Self::DataBeforeConnected { connection } => {
                write!(f, "data on {connection} before it was connected")
            }
            Self::DataAfterClose { connection } => {
                write!(f, "data on {connection} after it was closed")
            }
            Self::AlreadyConnected { connection } => {
                write!(f, "{connection} was already connected")
            }
            Self::ConnectedAfterClose { connection } => {
                write!(f, "{connection} connected after it was closed")
            }
            Self::AlreadyClosed { connection } => write!(f, "{connection} was already closed"),
        }
    }
}

impl core::error::Error for ProtocolError {}

/// One connection in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Entry {
    connection: ConnectionId,
    state: ConnectionState,
}

/// The table both sides keep: up to `N` connections, in fixed storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Table<const N: usize> {
    entries: [Option<Entry>; N],
}

impl<const N: usize> Table<N> {
    const fn new() -> Self {
        Self { entries: [None; N] }
    }

    fn state(&self, connection: ConnectionId) -> Option<ConnectionState> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.connection == connection)
            .map(|entry| entry.state)
    }

    /// The connection's state to change, or `NotOpen`.
    fn entry_mut(
        &mut self,
        connection: ConnectionId,
    ) -> Result<&mut ConnectionState, ProtocolError> {
        self.entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.connection == connection)
            .map(|entry| &mut entry.state)
            .ok_or(ProtocolError::NotOpen { connection })
    }

    /// Opens `connection`: its name must be free, and the table not full.
    fn open(&mut self, connection: ConnectionId) -> Result<(), ProtocolError> {
        if self.state(connection).is_some() {
            return Err(ProtocolError::AlreadyOpen { connection });
        }
        let free = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(ProtocolError::TableFull {
                connection,
                capacity: N,
            })?;
        *free = Some(Entry {
            connection,
            state: ConnectionState::default(),
        });
        Ok(())
    }

    /// Frees `connection`'s name, once both sides have ended it.
    fn release_if_ended(&mut self, connection: ConnectionId) {
        for slot in &mut self.entries {
            if slot.is_some_and(|entry| {
                entry.connection == connection
                    && entry.state.client_closed
                    && entry.state.stack_ended
            }) {
                *slot = None;
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    fn connections(&self) -> impl Iterator<Item = (ConnectionId, ConnectionState)> + '_ {
        self.entries
            .iter()
            .flatten()
            .map(|entry| (entry.connection, entry.state))
    }

    /// The client's [`Close`](crate::Close), sent or received: the same rule
    /// either way, as it may cross only the stack's end, which allows it.
    fn client_close(&mut self, connection: ConnectionId) -> Result<(), ProtocolError> {
        let state = self.entry_mut(connection)?;
        if state.client_closed {
            return Err(ProtocolError::AlreadyClosed { connection });
        }
        state.client_closed = true;
        self.release_if_ended(connection);
        Ok(())
    }

    /// The stack's end, [`Closed`](crate::Closed) or
    /// [`Failed`](crate::Failed), sent or received.
    fn stack_end(&mut self, connection: ConnectionId) -> Result<(), ProtocolError> {
        let state = self.entry_mut(connection)?;
        if state.stack_ended {
            return Err(ProtocolError::AlreadyClosed { connection });
        }
        state.stack_ended = true;
        self.release_if_ended(connection);
        Ok(())
    }
}

/// The stack's connection table for one client's ring pair: it checks each
/// request the client sends ([`StackTable::receive`]) and each report the
/// stack is about to send ([`StackTable::send`]).
///
/// ```
/// use core::net::Ipv4Addr;
///
/// use nowayos_socket::{
///     ConnectionId, Connect, Connected, ProtocolError, Report, Request, SendData, StackTable,
///     Verdict,
/// };
///
/// let mut table: StackTable = StackTable::new();
/// let id = ConnectionId(1);
/// let connect = Request::Connect(Connect::new(id, Ipv4Addr::new(10, 0, 2, 2), 443).unwrap());
/// let hello = Request::Send(SendData::new(id, b"hello").unwrap());
///
/// assert_eq!(table.receive(&connect), Ok(Verdict::Act));
/// // Data before the stack has said the connection is open: refused.
/// assert_eq!(
///     table.receive(&hello),
///     Err(ProtocolError::DataBeforeConnected { connection: id })
/// );
/// table.send(&Report::Connected(Connected::new(id))).unwrap();
/// assert_eq!(table.receive(&hello), Ok(Verdict::Act));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StackTable<const N: usize = MAX_CONNECTIONS> {
    table: Table<N>,
}

impl<const N: usize> Default for StackTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> StackTable<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: Table::new(),
        }
    }

    /// Checks a request the client sent, and applies it to the table.
    ///
    /// * [`Connect`](crate::Connect): its name must be free and the table
    ///   not full;
    /// * [`SendData`](crate::SendData): the connection must be open, the
    ///   stack must have sent [`Connected`](crate::Connected), and the client
    ///   must not have closed it. Data that crossed the stack's own end is
    ///   [`Verdict::Ignore`];
    /// * [`Close`](crate::Close): the connection must be open and not closed
    ///   by the client already. Once the stack has ended it too, its name is
    ///   free.
    ///
    /// # Errors
    ///
    /// The [`ProtocolError`] the request breaks; the table is unchanged.
    pub fn receive(&mut self, request: &Request) -> Result<Verdict, ProtocolError> {
        match request {
            Request::Connect(connect) => {
                self.table.open(connect.connection())?;
                Ok(Verdict::Act)
            }
            Request::Send(data) => {
                let connection = data.connection();
                let state = self
                    .table
                    .state(connection)
                    .ok_or(ProtocolError::NotOpen { connection })?;
                if state.client_closed {
                    Err(ProtocolError::DataAfterClose { connection })
                } else if !state.connected {
                    Err(ProtocolError::DataBeforeConnected { connection })
                } else if state.stack_ended {
                    Ok(Verdict::Ignore)
                } else {
                    Ok(Verdict::Act)
                }
            }
            Request::Close(close) => {
                self.table.client_close(close.connection())?;
                Ok(Verdict::Act)
            }
        }
    }

    /// Checks a report the stack is about to send, and applies it: the
    /// stack's check on itself.
    ///
    /// * [`Connected`](crate::Connected): once, before the stack's end and
    ///   before the client's close - a client that closes a connection still
    ///   connecting gets [`Closed`](crate::Closed);
    /// * [`ReceivedData`](crate::ReceivedData): after
    ///   [`Connected`](crate::Connected), before the stack's end and the
    ///   client's close;
    /// * [`Closed`](crate::Closed) or [`Failed`](crate::Failed): once. Once
    ///   the client has closed it too, its name is free.
    ///
    /// # Errors
    ///
    /// The [`ProtocolError`] the report would break; the table is unchanged,
    /// and the report must not be sent.
    pub fn send(&mut self, report: &Report) -> Result<(), ProtocolError> {
        match report {
            Report::Connected(connected) => {
                let connection = connected.connection();
                let state = self.table.entry_mut(connection)?;
                if state.connected {
                    return Err(ProtocolError::AlreadyConnected { connection });
                }
                if state.stack_ended || state.client_closed {
                    return Err(ProtocolError::ConnectedAfterClose { connection });
                }
                state.connected = true;
                Ok(())
            }
            Report::Received(data) => {
                let connection = data.connection();
                let state = self
                    .table
                    .state(connection)
                    .ok_or(ProtocolError::NotOpen { connection })?;
                if state.stack_ended || state.client_closed {
                    Err(ProtocolError::DataAfterClose { connection })
                } else if !state.connected {
                    Err(ProtocolError::DataBeforeConnected { connection })
                } else {
                    Ok(())
                }
            }
            Report::Closed(closed) => self.table.stack_end(closed.connection()),
            Report::Failed(failed) => self.table.stack_end(failed.connection()),
        }
    }

    /// What the table knows of `connection`, if it is open.
    #[must_use]
    pub fn state(&self, connection: ConnectionId) -> Option<ConnectionState> {
        self.table.state(connection)
    }

    /// Every open connection and its state.
    pub fn connections(&self) -> impl Iterator<Item = (ConnectionId, ConnectionState)> + '_ {
        self.table.connections()
    }

    /// How many connections are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether no connection is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most connections the table holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }
}

/// A client's connection table for its ring pair with the stack: it checks
/// each request the client is about to send ([`ClientTable::send`]) and each
/// report the stack sends ([`ClientTable::receive`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientTable<const N: usize = MAX_CONNECTIONS> {
    table: Table<N>,
}

impl<const N: usize> Default for ClientTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ClientTable<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: Table::new(),
        }
    }

    /// Checks a request the client is about to send, and applies it: the
    /// client's check on itself.
    ///
    /// * [`Connect`](crate::Connect): a free name, and room in the table;
    /// * [`SendData`](crate::SendData): after [`Connected`](crate::Connected),
    ///   before the client's close and the stack's end;
    /// * [`Close`](crate::Close): once. Once the stack has ended the
    ///   connection too, its name is free.
    ///
    /// # Errors
    ///
    /// The [`ProtocolError`] the request would break; the table is
    /// unchanged, and the request must not be sent.
    pub fn send(&mut self, request: &Request) -> Result<(), ProtocolError> {
        match request {
            Request::Connect(connect) => self.table.open(connect.connection()),
            Request::Send(data) => {
                let connection = data.connection();
                let state = self
                    .table
                    .state(connection)
                    .ok_or(ProtocolError::NotOpen { connection })?;
                if state.client_closed || state.stack_ended {
                    Err(ProtocolError::DataAfterClose { connection })
                } else if !state.connected {
                    Err(ProtocolError::DataBeforeConnected { connection })
                } else {
                    Ok(())
                }
            }
            Request::Close(close) => self.table.client_close(close.connection()),
        }
    }

    /// Checks a report the stack sent, and applies it to the table.
    ///
    /// * [`Connected`](crate::Connected): once, before the stack's end. One
    ///   that crossed the client's close is [`Verdict::Ignore`];
    /// * [`ReceivedData`](crate::ReceivedData): after
    ///   [`Connected`](crate::Connected) and before the stack's end. Data
    ///   that crossed the client's close is [`Verdict::Ignore`];
    /// * [`Closed`](crate::Closed) or [`Failed`](crate::Failed): once. Once
    ///   the client has closed the connection too, its name is free.
    ///
    /// # Errors
    ///
    /// The [`ProtocolError`] the report breaks; the table is unchanged.
    pub fn receive(&mut self, report: &Report) -> Result<Verdict, ProtocolError> {
        match report {
            Report::Connected(connected) => {
                let connection = connected.connection();
                let state = self.table.entry_mut(connection)?;
                if state.connected {
                    return Err(ProtocolError::AlreadyConnected { connection });
                }
                if state.stack_ended {
                    return Err(ProtocolError::ConnectedAfterClose { connection });
                }
                state.connected = true;
                Ok(if state.client_closed {
                    Verdict::Ignore
                } else {
                    Verdict::Act
                })
            }
            Report::Received(data) => {
                let connection = data.connection();
                let state = self
                    .table
                    .state(connection)
                    .ok_or(ProtocolError::NotOpen { connection })?;
                if state.stack_ended {
                    Err(ProtocolError::DataAfterClose { connection })
                } else if !state.connected {
                    Err(ProtocolError::DataBeforeConnected { connection })
                } else if state.client_closed {
                    Ok(Verdict::Ignore)
                } else {
                    Ok(Verdict::Act)
                }
            }
            Report::Closed(closed) => {
                self.table.stack_end(closed.connection())?;
                Ok(Verdict::Act)
            }
            Report::Failed(failed) => {
                self.table.stack_end(failed.connection())?;
                Ok(Verdict::Act)
            }
        }
    }

    /// What the table knows of `connection`, if it is open.
    #[must_use]
    pub fn state(&self, connection: ConnectionId) -> Option<ConnectionState> {
        self.table.state(connection)
    }

    /// Every open connection and its state.
    pub fn connections(&self) -> impl Iterator<Item = (ConnectionId, ConnectionState)> + '_ {
        self.table.connections()
    }

    /// How many connections are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether no connection is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most connections the table holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failed check panics")]
mod tests {
    use core::net::Ipv4Addr;

    use super::*;
    use crate::records::{
        Close, Closed, Connect, Connected, FailReason, Failed, ReceivedData, SendData,
    };

    const A: ConnectionId = ConnectionId(1);
    const B: ConnectionId = ConnectionId(2);

    fn connect(id: ConnectionId) -> Request {
        Request::Connect(Connect::new(id, Ipv4Addr::new(10, 0, 2, 2), 443).unwrap())
    }

    fn send(id: ConnectionId) -> Request {
        Request::Send(SendData::new(id, b"GET").unwrap())
    }

    fn close(id: ConnectionId) -> Request {
        Request::Close(Close::new(id))
    }

    fn connected(id: ConnectionId) -> Report {
        Report::Connected(Connected::new(id))
    }

    fn received(id: ConnectionId) -> Report {
        Report::Received(ReceivedData::new(id, b"200").unwrap())
    }

    fn closed(id: ConnectionId) -> Report {
        Report::Closed(Closed::new(id))
    }

    fn failed(id: ConnectionId) -> Report {
        Report::Failed(Failed::new(id, FailReason::Reset))
    }

    #[test]
    fn a_connection_from_connect_to_both_ends() {
        let mut stack: StackTable = StackTable::new();
        let mut client: ClientTable = ClientTable::new();
        client.send(&connect(A)).unwrap();
        assert_eq!(stack.receive(&connect(A)), Ok(Verdict::Act));
        for report in [connected(A), received(A)] {
            stack.send(&report).unwrap();
            assert_eq!(client.receive(&report), Ok(Verdict::Act));
        }
        client.send(&send(A)).unwrap();
        assert_eq!(stack.receive(&send(A)), Ok(Verdict::Act));
        // The client closes; the stack, having closed the connection, ends it.
        client.send(&close(A)).unwrap();
        assert_eq!(stack.receive(&close(A)), Ok(Verdict::Act));
        stack.send(&closed(A)).unwrap();
        assert!(stack.is_empty(), "both ends seen: the name is free");
        assert_eq!(client.receive(&closed(A)), Ok(Verdict::Act));
        assert!(client.is_empty());
        // The name can be used again.
        client.send(&connect(A)).unwrap();
        assert_eq!(stack.receive(&connect(A)), Ok(Verdict::Act));
    }

    #[test]
    fn records_crossing_an_end_are_ignored_not_refused() {
        let mut stack: StackTable = StackTable::new();
        let mut client: ClientTable = ClientTable::new();
        client.send(&connect(A)).unwrap();
        stack.receive(&connect(A)).unwrap();
        stack.send(&connected(A)).unwrap();
        client.receive(&connected(A)).unwrap();
        // The client sends data while the stack, at the same moment, ends
        // the connection: each side's record crosses the other's.
        client.send(&send(A)).unwrap();
        stack.send(&failed(A)).unwrap();
        assert_eq!(stack.receive(&send(A)), Ok(Verdict::Ignore));
        assert_eq!(client.receive(&failed(A)), Ok(Verdict::Act));
        // The client must close it to free the name, at both ends.
        assert_eq!(
            client.send(&send(A)),
            Err(ProtocolError::DataAfterClose { connection: A })
        );
        client.send(&close(A)).unwrap();
        assert!(client.is_empty());
        assert_eq!(stack.receive(&close(A)), Ok(Verdict::Act));
        assert!(stack.is_empty());

        // The client closes while the stack's data is in flight.
        client.send(&connect(B)).unwrap();
        stack.receive(&connect(B)).unwrap();
        stack.send(&connected(B)).unwrap();
        stack.send(&received(B)).unwrap();
        client.send(&close(B)).unwrap();
        assert_eq!(client.receive(&connected(B)), Ok(Verdict::Ignore));
        assert_eq!(client.receive(&received(B)), Ok(Verdict::Ignore));
        stack.receive(&close(B)).unwrap();
        assert_eq!(
            stack.send(&received(B)),
            Err(ProtocolError::DataAfterClose { connection: B })
        );
        stack.send(&closed(B)).unwrap();
        assert_eq!(client.receive(&closed(B)), Ok(Verdict::Act));
        assert!(client.is_empty() && stack.is_empty());
    }

    #[test]
    fn what_no_honest_peer_sends_is_refused_and_changes_nothing() {
        let mut stack: StackTable<2> = StackTable::new();
        let before = stack.clone();
        assert_eq!(
            stack.receive(&send(A)),
            Err(ProtocolError::NotOpen { connection: A })
        );
        assert_eq!(
            stack.receive(&close(A)),
            Err(ProtocolError::NotOpen { connection: A })
        );
        assert_eq!(stack, before);
        stack.receive(&connect(A)).unwrap();
        stack.receive(&connect(B)).unwrap();
        let full = stack.clone();
        assert_eq!(
            stack.receive(&connect(A)),
            Err(ProtocolError::AlreadyOpen { connection: A })
        );
        assert_eq!(
            stack.receive(&connect(ConnectionId(3))),
            Err(ProtocolError::TableFull {
                connection: ConnectionId(3),
                capacity: 2
            })
        );
        assert_eq!(
            stack.receive(&send(A)),
            Err(ProtocolError::DataBeforeConnected { connection: A })
        );
        assert_eq!(stack, full);
        stack.receive(&close(A)).unwrap();
        let closing = stack.clone();
        assert_eq!(
            stack.receive(&send(A)),
            Err(ProtocolError::DataAfterClose { connection: A })
        );
        assert_eq!(
            stack.receive(&close(A)),
            Err(ProtocolError::AlreadyClosed { connection: A })
        );
        assert_eq!(stack, closing);

        let mut client: ClientTable<2> = ClientTable::new();
        assert_eq!(
            client.receive(&connected(A)),
            Err(ProtocolError::NotOpen { connection: A })
        );
        client.send(&connect(A)).unwrap();
        assert_eq!(
            client.receive(&received(A)),
            Err(ProtocolError::DataBeforeConnected { connection: A })
        );
        client.receive(&connected(A)).unwrap();
        assert_eq!(
            client.receive(&connected(A)),
            Err(ProtocolError::AlreadyConnected { connection: A })
        );
        client.receive(&closed(A)).unwrap();
        let ended = client.clone();
        assert_eq!(
            client.receive(&received(A)),
            Err(ProtocolError::DataAfterClose { connection: A })
        );
        assert_eq!(
            client.receive(&failed(A)),
            Err(ProtocolError::AlreadyClosed { connection: A })
        );
        assert_eq!(client, ended);
    }

    #[test]
    fn the_stack_checks_itself() {
        let mut stack: StackTable = StackTable::new();
        assert_eq!(
            stack.send(&connected(A)),
            Err(ProtocolError::NotOpen { connection: A })
        );
        stack.receive(&connect(A)).unwrap();
        assert_eq!(
            stack.send(&received(A)),
            Err(ProtocolError::DataBeforeConnected { connection: A })
        );
        // A connection the client closed while it was connecting is answered
        // with Closed, never Connected.
        stack.receive(&close(A)).unwrap();
        assert_eq!(
            stack.send(&connected(A)),
            Err(ProtocolError::ConnectedAfterClose { connection: A })
        );
        stack.send(&closed(A)).unwrap();
        assert!(stack.is_empty());
        assert_eq!(stack.capacity(), MAX_CONNECTIONS);
    }

    #[test]
    fn states_are_visible() {
        let mut client: ClientTable = ClientTable::new();
        client.send(&connect(A)).unwrap();
        client.receive(&connected(A)).unwrap();
        assert_eq!(
            client.state(A),
            Some(ConnectionState {
                connected: true,
                client_closed: false,
                stack_ended: false
            })
        );
        assert_eq!(client.connections().count(), 1);
        assert_eq!(client.len(), 1);
        assert_eq!(client.state(B), None);
        assert_eq!(
            ProtocolError::TableFull {
                connection: B,
                capacity: 4
            }
            .to_string(),
            "connection 2 would be one more than the 4 the table holds"
        );
        assert_eq!(
            ProtocolError::DataAfterClose { connection: A }.connection(),
            A
        );
    }
}
