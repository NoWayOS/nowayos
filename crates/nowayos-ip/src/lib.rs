// SPDX-License-Identifier: MIT OR Apache-2.0
//! The network stack PD's logic: smoltcp over the NIC
//! driver's sDDF queues, the DHCP client that takes the stack's address,
//! and TCP connections for the stack's socket client.
//!
//! The stack PD is the PD that parses hostile frames - Ethernet, ARP, IPv4,
//! UDP, DHCP and TCP - so it holds nothing of the card's: no device memory,
//! interrupt or physical address. It is the NIC driver's one client:
//!
//! * [`Link`] is smoltcp's device over the four queues and the two data
//!   regions. Every descriptor is checked, every frame is **copied into
//!   private memory before smoltcp sees it**, and a malformed or oversize
//!   frame is dropped and counted, never panicked on ([`Counts`]);
//! * [`Stack`] is smoltcp's interface with a DHCP client and, made with
//!   [`Stack::with_connections`], up to four TCP connections for one
//!   socket client ([`Stack::serve`]): `nowayos-socket`'s records in, each
//!   checked before use, its reports out, and a connection opened only to
//!   a destination the system's allow list holds ([`Notice`]). It runs
//!   only with a seed ([`StackSeed`], never smoltcp's default of 0) and on
//!   monotonic time from the timer PD, never wall-clock time.
//!
//! smoltcp 0.14.0 (0BSD, signed off: Licensing Options item 19) is
//! built with only `medium-ethernet`, `proto-ipv4`, `proto-dhcpv4`,
//! `socket-dhcpv4` and `socket-tcp`: `deny.toml` holds every workspace to
//! exactly those. IPv6, DNS and UDP sockets come with the product that
//! needs them.
//!
//! The PD itself (`pds/net-stack`) binds the regions and the client's
//! rings, asks the timer PD for the time and takes its seed off its seed
//! ring (`nowayos-seed`); this crate needs no seL4, so it is tested on the
//! host - its connections against a second smoltcp interface on an
//! in-memory wire - and `make fuzz` feeds arbitrary frames through [`Link`]
//! and [`Stack`] (`net_stack_receive`), and hostile TCP segments and
//! records through its connections at once (`net_stack_tcp`). No `unsafe`
//! code.
//!
//! Provisional: one socket client, TCP over IPv4.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod connections;
mod link;
mod stack;

pub use connections::{
    CLOSE_TIMEOUT_SECS, CONNECT_TIMEOUT_SECS, LEASE_WAIT_SECS, Notice, RX_BUFFER_LEN, Reports,
    Requests, TX_BUFFER_LEN, TcpRoom,
};
pub use link::{Counts, Link, LinkError, Queues, RxMemory, RxToken, TxMemory, TxToken};
pub use stack::{
    Event, Lease, MAX_FRAMES_PER_POLL, SOCKETS, Stack, StackError, StackSeed, Storage, storage,
};

/// smoltcp itself, for the frames a test or the fuzz harness builds with
/// its `wire` module.
pub use smoltcp;
