// SPDX-License-Identifier: MIT OR Apache-2.0
//! The typed socket records between the network stack PD and its socket
//! clients.
//!
//! The network stack PD parses hostile frames and holds the TCP
//! connections; its socket clients - the TLS PD, and a credential PD
//! once one needs it - never see a frame. A client asks for a
//! connection, sends and receives stream bytes and closes it through these
//! records, and nothing else. Each client shares a **pair of `nowayos-ipc`
//! rings** with the stack: requests from the client on one ring, reports
//! from the stack on the other. A client names its connections itself, and
//! the names are per ring pair, so a second client is a second ring pair and
//! needs no new record (the stack serves one client, or a second, such as a
//! credential PD).
//!
//! What the records cover: **TCP connections from a client to an IPv4
//! address and port** - the kiosk's backend, and what the network stack set out to do. UDP,
//! listening sockets, IPv6 and name lookups come with the product that needs
//! them. The kinds a name lookup will use are reserved now
//! ([`RESOLVE_KIND`], [`RESOLVED_KIND`]), so that it arrives as two records
//! added beside these, not as a new layout of them. No record holds an
//! address of the stack's own, a socket handle or anything else a client
//! could use to reach past its connections.
//!
//! # The records
//!
//! Every record is a fixed-size `nowayos-ipc` [`Message`](nowayos_ipc::Message):
//! an 8-byte header naming its kind, layout version and payload length,
//! then the payload. Each record has its own kind, so a record on the wrong
//! ring, or of the wrong kind, is refused by its header:
//!
//! | Record | Kind | Ring | Payload | Says |
//! | --- | --- | --- | --- | --- |
//! | [`Connect`] | `0x0501` | requests | 8 bytes | open a TCP connection to an IPv4 address and port, and call it *connection* |
//! | [`SendData`] | `0x0502` | requests | 984 bytes | send these bytes on the connection |
//! | [`Close`] | `0x0503` | requests | 4 bytes | the client is done with the connection |
//! | reserved | `0x0504` | requests | - | a name lookup, when a product needs one ([`RESOLVE_KIND`]) |
//! | [`Connected`] | `0x0581` | reports | 4 bytes | the connection is open |
//! | [`ReceivedData`] | `0x0582` | reports | 984 bytes | these bytes arrived on the connection |
//! | [`Closed`] | `0x0583` | reports | 4 bytes | the connection is over, cleanly |
//! | [`Failed`] | `0x0584` | reports | 4 bytes | the connection is over, and why ([`FailReason`]) |
//! | reserved | `0x0585` | reports | - | a name lookup's answer ([`RESOLVED_KIND`]) |
//!
//! The kinds are a block of their own, apart from the other records a
//! socket client may send: the TLS PD also sends the clock PD its samples
//! (`nowayos-time`, `0x0401`), so a record written to the wrong one of its
//! rings is refused by its kind as well as its length.
//!
//! Each record names its connection with a [`ConnectionId`]. The stream
//! bytes travel **inside** [`SendData`] and [`ReceivedData`]: from 1 to
//! [`MAX_DATA`] bytes each, the one data bound, so the longest record is
//! [`SLOT_LEN`] bytes and a ring slot that long holds every record. Each
//! record's wire layout is in its documentation; every field is checked on
//! receipt, padding included, so every record has exactly one encoding.
//!
//! **The contract's version is its records'.** The data records are at
//! layout version 2: up to 976 bytes in a 992-byte slot, 32 slots
//! to a ring, each ring's messages in 32 KiB ([`RING_REGION_LEN`]), so a
//! connection's bytes cross in a quarter of the records version 1's 240
//! bytes took, and a ring holds sixteen times the bytes. Every other
//! record is at version 1, unchanged. A peer still on version 1 has its data
//! records refused by their headers, and a system that still gives each
//! ring's messages a page fails as its PDs bind the rings.
//!
//! A record is copied out of its ring slot into private memory before it is
//! decoded - `nowayos-ipc`'s `Consumer` does that - so the stream bytes the
//! receiver checks and uses are the ones it copied, whatever the peer writes
//! into the slot afterwards.
//!
//! # A connection's life
//!
//! Each end of a ring pair keeps a connection table ([`StackTable`] in the
//! stack, [`ClientTable`] in the client) that checks every record it
//! receives, and every record its own PD sends, against the connection's
//! state. Both tables hold [`MAX_CONNECTIONS`] connections:
//!
//! 1. the client sends [`Connect`]; the stack answers [`Connected`] or
//!    [`Failed`];
//! 2. while open, the client sends [`SendData`] and the stack reports
//!    [`ReceivedData`];
//! 3. **each side ends a connection with a record of its own, and the
//!    connection's name is free again once both have**: the client's is
//!    [`Close`] - it sends nothing more for the connection - and the
//!    stack's is [`Closed`] or [`Failed`] - it reports nothing more. The
//!    two may cross, in either order. Until both have been seen, the name
//!    stays in both tables, so a [`Connect`] never reuses a name with
//!    records still in flight for it.
//!
//! A record that the peer may have sent before it learned the connection was
//! ending - data in flight towards a side that has already ended it - is
//! valid, and the table tells the receiver to ignore it ([`Verdict::Ignore`]).
//! Anything an honest peer could never send is refused as a
//! [`ProtocolError`], and the table is unchanged: a record naming a
//! connection that is not open, data before [`Connected`] or after the
//! sender's own end, a second [`Connected`], [`Close`] or end, and a
//! [`Connect`] for a name in use or beyond the table's capacity.
//!
//! # The destinations allowed
//!
//! A client names any address and port; the stack connects only to those
//! the system's configuration lists ([`AllowList`]), and refuses
//! the rest with [`FailReason::NotAllowed`].
//!
//! Provisional: the network stack PD serves one client over
//! these records; a test instrument stands in for the client.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod allow;
#[cfg(kani)]
mod proofs;
mod records;
mod table;

pub use allow::{
    ALLOW_LIST_MAX_LEN, ALLOW_LIST_VERSION, AllowList, AllowListError, Destination,
    MAX_DESTINATIONS,
};
pub use records::{
    CLOSE_KIND, CLOSED_KIND, CONNECT_KIND, CONNECTED_KIND, Close, Closed, Connect, Connected,
    ConnectionId, ConnectionRecord, Data, FAILED_KIND, FailReason, Failed, MAX_DATA, RECEIVED_KIND,
    RESOLVE_KIND, RESOLVED_KIND, RING_REGION_LEN, RING_SLOTS, ReceivedData, RecordError, Report,
    Request, SEND_KIND, SLOT_LEN, SendData, is_unicast_destination,
};
pub use table::{
    ClientTable, ConnectionState, MAX_CONNECTIONS, ProtocolError, StackTable, Verdict,
};
