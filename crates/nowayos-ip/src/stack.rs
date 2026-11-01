// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stack: smoltcp's interface and its one socket, a DHCP client, over a
//! [`Link`](crate::Link).
//!
//! * **A seed, or nothing.** [`Stack::new`] takes a [`StackSeed`], which is
//!   never zero: smoltcp's default seed is 0, and with it the DHCP
//!   transaction ids - and, later, TCP's initial sequence numbers - would be
//!   the same on every device at every boot. A seed comes from the entropy
//!   PD (`nowayos_seed`); a PD without one does not make a stack.
//! * **Monotonic time only.** Every call takes `nowayos_time`'s [`Instant`],
//!   from the timer PD - never wall-clock time - so a clock step cannot
//!   break retransmission or a lease.
//! * **Bounded work.** [`Stack::poll`] handles at most [`MAX_FRAMES_PER_POLL`]
//!   received frames, then sends what is due; smoltcp's own `poll` would
//!   take frames for as long as the driver passes them.
//! * **No panic on what the network says.** smoltcp checks a lease's
//!   address before it reports it; the stack checks it again - unicast, as
//!   smoltcp's interface insists on pain of a panic - before giving it to
//!   the interface, and ignores a lease that fails.

use core::fmt;
use core::net::Ipv4Addr;
use core::num::NonZeroU64;

use nowayos_net::arp::{Ipv4, Mac};
use nowayos_seed::Seed;
use nowayos_socket::{AllowList, MAX_CONNECTIONS};
use nowayos_time::Instant;
use smoltcp::iface::{
    Config, Interface, PollIngressSingleResult, PollResult, SocketHandle, SocketSet, SocketStorage,
};
use smoltcp::phy::{Device, Medium};
use smoltcp::socket::dhcpv4;
use smoltcp::time::{Duration as NetDuration, Instant as NetInstant};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Cidr};

use crate::connections::{Connections, Notice, Reports, Requests, TcpRoom};

/// The sockets a stack holds: the DHCP client, and one TCP socket for each
/// of its client's connections.
pub const SOCKETS: usize = 1 + MAX_CONNECTIONS;

/// How long the DHCP client waits for an offer before it asks again.
/// smoltcp waits 10 s; a stack that starts before its link is up loses its
/// first DISCOVER, and a board run held to a few seconds by its watchdog
/// would end before the second.
pub const DHCP_DISCOVER_RETRY_SECS: u64 = 2;

/// The most received frames one [`Stack::poll`] handles.
pub const MAX_FRAMES_PER_POLL: usize = 64;

/// The most rounds of sending one [`Stack::poll`] makes. Each is bounded
/// work in smoltcp; with one socket, one round sends all that is due.
const MAX_EGRESS_ROUNDS: usize = 4;

/// Room for the stack's sockets, which smoltcp keeps in memory its caller
/// owns: a PD has no heap.
pub type Storage<'a> = [SocketStorage<'a>; SOCKETS];

/// Empty room for the stack's sockets.
#[must_use]
pub const fn storage<'a>() -> Storage<'a> {
    [SocketStorage::EMPTY; SOCKETS]
}

/// smoltcp's `random_seed`, never zero.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackSeed(NonZeroU64);

impl StackSeed {
    /// The seed for smoltcp from a seed off the seed ring: its words folded
    /// together ([`Seed::fold_u64`]). `None` if they fold to zero, which
    /// smoltcp would take for its default.
    #[must_use]
    pub fn from_seed(seed: &Seed) -> Option<Self> {
        NonZeroU64::new(seed.fold_u64()).map(Self)
    }

    /// A seed from a value that is not zero: for tests and fuzzing.
    #[must_use]
    pub const fn from_non_zero(seed: NonZeroU64) -> Self {
        Self(seed)
    }
}

impl fmt::Debug for StackSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StackSeed(..)")
    }
}

/// An IPv4 lease from a DHCP server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The stack's address.
    pub address: Ipv4,
    /// Its network's prefix length.
    pub prefix_len: u8,
    /// The default gateway, if the server named one.
    pub gateway: Option<Ipv4>,
}

impl fmt::Display for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.address, self.prefix_len)?;
        match self.gateway {
            Some(gateway) => write!(f, ", gateway {gateway}"),
            None => f.write_str(", no gateway"),
        }
    }
}

/// What a poll changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A lease was taken, or changed: the interface has its address.
    Leased(Lease),
    /// The lease was lost: the interface has no address.
    Lost,
}

/// Why a stack could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StackError {
    /// The hardware address is not unicast: smoltcp would panic on it.
    NotUnicast(Mac),
    /// The device is not Ethernet.
    NotEthernet,
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnicast(mac) => write!(f, "{mac} is not a unicast address"),
            Self::NotEthernet => f.write_str("the device is not Ethernet"),
        }
    }
}

/// The network stack: smoltcp's interface, at `mac`, a DHCP client, and -
/// made with [`Stack::with_connections`] - TCP connections for one socket
/// client (`connections`).
pub struct Stack<'a> {
    interface: Interface,
    sockets: SocketSet<'a>,
    dhcp: SocketHandle,
    lease: Option<Lease>,
    connections: Option<Connections>,
    seed: u64,
    /// The lease is [`Stack::assume_lease`]'s, not a server's: the DHCP
    /// client's "no lease" does not take it away.
    assumed: bool,
}

/// How long a stack with a connection in flight waits at most between
/// polls, in nanoseconds: its own timeouts need a look now and then.
const BUSY_POLL_NANOS: u64 = 100_000_000;

impl<'a> Stack<'a> {
    /// A stack at `mac` on `device`, seeded with `seed`, keeping its
    /// sockets in `storage` (emptied first). It has no address until a DHCP
    /// server gives it one; the first [`Stack::poll`] asks.
    ///
    /// # Errors
    ///
    /// [`StackError::NotUnicast`] or [`StackError::NotEthernet`]: the
    /// checks smoltcp would otherwise make by panicking.
    pub fn new<D: Device + ?Sized>(
        mac: Mac,
        seed: StackSeed,
        device: &mut D,
        storage: &'a mut Storage<'a>,
        now: Instant,
    ) -> Result<Self, StackError> {
        let address = EthernetAddress(mac.0);
        if !address.is_unicast() {
            return Err(StackError::NotUnicast(mac));
        }
        if device.capabilities().medium != Medium::Ethernet {
            return Err(StackError::NotEthernet);
        }
        let mut config = Config::new(HardwareAddress::Ethernet(address));
        config.random_seed = seed.0.get();
        let interface = Interface::new(config, device, net_instant(now));
        // Every slot empty, so adding the DHCP client cannot find the
        // storage full (smoltcp panics if it does).
        for slot in storage.iter_mut() {
            *slot = SocketStorage::EMPTY;
        }
        let mut sockets = SocketSet::new(&mut storage[..]);
        let mut client = dhcpv4::Socket::new();
        let mut retry = dhcpv4::RetryConfig::default();
        retry.discover_timeout = NetDuration::from_secs(DHCP_DISCOVER_RETRY_SECS);
        client.set_retry_config(retry);
        let dhcp = sockets.add(client);
        Ok(Self {
            interface,
            sockets,
            dhcp,
            lease: None,
            connections: None,
            seed: seed.0.get(),
            assumed: false,
        })
    }

    /// As [`Stack::new`], with TCP connections for one socket client: one
    /// socket per connection, over `room`'s buffers, to `allow`'s
    /// destinations only. Serve the client with [`Stack::serve`].
    ///
    /// # Errors
    ///
    /// As [`Stack::new`].
    pub fn with_connections<D: Device + ?Sized>(
        mac: Mac,
        seed: StackSeed,
        device: &mut D,
        storage: &'a mut Storage<'a>,
        room: &'a mut TcpRoom,
        allow: AllowList,
        now: Instant,
    ) -> Result<Self, StackError> {
        let mut stack = Self::new(mac, seed, device, storage, now)?;
        let mut handles = [SocketHandle::default(); MAX_CONNECTIONS];
        // SOCKETS slots: the DHCP client's and one per connection, so adding
        // these cannot find the storage full.
        for (handle, socket) in handles.iter_mut().zip(room.sockets()) {
            *handle = stack.sockets.add(socket);
        }
        stack.connections = Some(Connections::new(handles, allow, stack.seed));
        Ok(stack)
    }

    /// Serves the socket client: takes its `requests`, opens, feeds and
    /// closes connections, and sends `reports`; says what happened through
    /// `notices`. Nothing for a stack made with [`Stack::new`]. Call it
    /// between [`Stack::poll`]s.
    pub fn serve(
        &mut self,
        now: Instant,
        requests: &mut impl Requests,
        reports: &mut impl Reports,
        notices: &mut impl FnMut(Notice),
    ) {
        let leased = self.lease.is_some();
        if let Some(connections) = self.connections.as_mut() {
            connections.serve(
                self.interface.context(),
                &mut self.sockets,
                leased,
                now,
                requests,
                reports,
                notices,
            );
        }
    }

    /// Gives the interface `lease` as if a DHCP server had: for tests and
    /// fuzzing, which have no server. The DHCP client keeps asking, and a
    /// real lease replaces this one.
    pub fn assume_lease(&mut self, lease: Lease) {
        let cidr = Ipv4Cidr::new(Ipv4Addr::from(lease.address.0), lease.prefix_len);
        if !is_unicast(cidr.address()) || lease.prefix_len > 32 {
            return;
        }
        self.configure(cidr, lease.gateway.map(|gateway| Ipv4Addr::from(gateway.0)));
        self.lease = Some(lease);
        self.assumed = true;
    }

    /// One round: the frames received - at most [`MAX_FRAMES_PER_POLL`] -
    /// then whatever is due to be sent, then the DHCP client's news.
    pub fn poll<D: Device + ?Sized>(&mut self, device: &mut D, now: Instant) -> Option<Event> {
        let now = net_instant(now);
        self.interface.poll_maintenance(now);
        for _ in 0..MAX_FRAMES_PER_POLL {
            if self
                .interface
                .poll_ingress_single(now, device, &mut self.sockets)
                == PollIngressSingleResult::None
            {
                break;
            }
        }
        for _ in 0..MAX_EGRESS_ROUNDS {
            if self.interface.poll_egress(now, device, &mut self.sockets) == PollResult::None {
                break;
            }
        }
        self.dhcp_news()
    }

    /// How long until the stack must be polled again, in nanoseconds of
    /// monotonic time: 0 if it is due now; `None` if nothing is due.
    pub fn poll_delay(&mut self, now: Instant) -> Option<u64> {
        let delay = self
            .interface
            .poll_delay(net_instant(now), &self.sockets)
            .map(|delay| delay.total_micros().saturating_mul(1000));
        if self.connections.as_ref().is_some_and(Connections::busy) {
            return Some(delay.map_or(BUSY_POLL_NANOS, |delay| delay.min(BUSY_POLL_NANOS)));
        }
        delay
    }

    /// The lease held, if any.
    #[must_use]
    pub const fn lease(&self) -> Option<Lease> {
        self.lease
    }

    /// The DHCP client's news, applied to the interface.
    fn dhcp_news(&mut self) -> Option<Event> {
        let event = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll()?;
        match event {
            dhcpv4::Event::Configured(config) => {
                let cidr = config.address;
                if !is_unicast(cidr.address()) || cidr.prefix_len() > 32 {
                    return None;
                }
                let gateway = config.router.filter(|router| is_unicast(*router));
                self.configure(cidr, gateway);
                self.assumed = false;
                let lease = Lease {
                    address: Ipv4(cidr.address().octets()),
                    prefix_len: cidr.prefix_len(),
                    gateway: gateway.map(|router| Ipv4(router.octets())),
                };
                self.lease = Some(lease);
                Some(Event::Leased(lease))
            }
            // smoltcp's client also says this once at the start, with no
            // lease to lose: only a lease held is reported lost.
            dhcpv4::Event::Deconfigured if self.assumed => None,
            dhcpv4::Event::Deconfigured => {
                self.interface.update_ip_addrs(|addrs| addrs.clear());
                self.interface.routes_mut().remove_default_ipv4_route();
                self.lease.take().map(|_| Event::Lost)
            }
        }
    }

    /// Gives the interface `cidr`, unicast, and the default route.
    fn configure(&mut self, cidr: Ipv4Cidr, gateway: Option<Ipv4Addr>) {
        self.interface.update_ip_addrs(|addrs| {
            addrs.clear();
            // One address, in room for at least one: it fits.
            let _ = addrs.push(IpCidr::Ipv4(cidr));
        });
        let routes = self.interface.routes_mut();
        match gateway {
            // A full route table leaves the stack without a default route:
            // it still has its address. One route, in room for at least one.
            Some(router) => {
                let _ = routes.add_default_ipv4_route(router);
            }
            None => {
                routes.remove_default_ipv4_route();
            }
        }
    }
}

/// Whether smoltcp's interface takes `address` as its own: not broadcast,
/// multicast or unspecified - what it panics on otherwise.
fn is_unicast(address: Ipv4Addr) -> bool {
    !(address.is_broadcast() || address.is_multicast() || address.is_unspecified())
}

/// smoltcp's instant - microseconds - for monotonic time in nanoseconds.
fn net_instant(now: Instant) -> NetInstant {
    let micros = now.as_nanos().checked_div(1000).unwrap_or(0);
    NetInstant::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code: a failed check panics")]
mod tests {
    use super::*;

    #[test]
    fn a_seed_that_folds_to_zero_is_refused() {
        let mut cancel = [0u8; 32];
        cancel[0] = 5;
        cancel[8] = 5;
        assert!(StackSeed::from_seed(&Seed::new(cancel).unwrap()).is_none());
        let mut bytes = [0u8; 32];
        bytes[3] = 1;
        let seed = StackSeed::from_seed(&Seed::new(bytes).unwrap()).unwrap();
        assert_eq!(seed.0.get(), 1 << 24);
        assert_eq!(format!("{seed:?}"), "StackSeed(..)");
    }

    #[test]
    fn monotonic_time_becomes_smoltcps_microseconds() {
        assert_eq!(
            net_instant(Instant::from_nanos(1_999)),
            NetInstant::from_micros(1)
        );
        assert_eq!(
            net_instant(Instant::from_nanos(u64::MAX)),
            NetInstant::from_micros(i64::try_from(u64::MAX / 1000).unwrap())
        );
    }

    #[test]
    fn only_unicast_addresses_are_given_to_the_interface() {
        assert!(is_unicast(Ipv4Addr::new(10, 0, 2, 15)));
        for address in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(224, 0, 0, 1),
        ] {
            assert!(!is_unicast(address));
        }
    }

    #[test]
    fn a_lease_prints_as_the_expect_files_read_it() {
        let lease = Lease {
            address: Ipv4([10, 0, 2, 15]),
            prefix_len: 24,
            gateway: Some(Ipv4([10, 0, 2, 2])),
        };
        assert_eq!(lease.to_string(), "10.0.2.15/24, gateway 10.0.2.2");
        let none = Lease {
            gateway: None,
            ..lease
        };
        assert_eq!(none.to_string(), "10.0.2.15/24, no gateway");
    }
}
