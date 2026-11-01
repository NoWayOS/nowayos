// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-net-stack`: the network stack PD.
//! It runs smoltcp over
//! the NIC driver's four sDDF queues, as the driver's one client, and takes
//! an address from a DHCP server.
//!
//! It parses hostile frames, so it holds nothing of the card's - no device
//! memory, interrupt or physical address (role `network`,
//! `tools/grant-rules.toml`) - and maps the receive buffers read-only.
//! Every descriptor the driver passes back is checked and every frame
//! copied into private memory before smoltcp sees it; a malformed or
//! oversize frame is dropped and counted, and the counts are reported
//! whenever they change (`nowayos_ip::Link`).
//!
//! **No seed, no interface**. Its seed comes on a seed
//! ring of its own (`nowayos-seed`): from a test PD in a test system until
//! the entropy PD exists. With no seed ring it says it is not ready
//! and never brings its interface up; with an empty one it waits, not
//! ready, until a seed arrives - smoltcp never runs on its default seed of
//! 0. **Monotonic time only**: the time comes from the timer PD, which also
//! wakes it when smoltcp has something due (a DHCP retransmission, a
//! renewal).
//!
//! **Its socket client**, when the system gives it one: a pair of
//! `nowayos-ipc` rings of `nowayos-socket`'s records - the client's
//! requests, the stack's reports - a channel, and the system's allow list
//! (`tcp-allow-...bin`, read-only region 2). The stack opens TCP connections
//! for the client to the destinations the list holds and refuses any other,
//! saying so; every record is decoded and checked against the connection's
//! state before use (`nowayos_ip::Stack::serve`). A system with no client
//! rings gets the DHCP lease and nothing more.
//!
//! Its MAC address is fixed - the one each system's `.qemu` file gives the
//! card - until the driver hands the card's own over (Known Issues).
//!
//! The socket storage smoltcp borrows, and the connections' buffers
//! (`TcpRoom`, 272 KiB: a 64 KiB window each), live in `init`'s frame,
//! which runs the PD's event loop itself (rust-sel4's `Handler::run`, the
//! loop the runtime runs for every PD) and never returns: a PD has no heap,
//! and smoltcp's sockets borrow their storage for as long as they live.
//! Every system gives the PD a `stack_size` of 0x80000 to hold them.
//!
//! No `unsafe` code; regions are bound through `nowayos-region`.

#![no_std]

use core::sync::atomic::{AtomicBool, Ordering};

use nowayos_bounded::Wipe;
use nowayos_display::FrameMemory;
use nowayos_ip::{
    Counts, Event, Link, Notice, Queues, Reports, Requests, RxMemory, Stack, StackSeed, Storage,
    TcpRoom, TxMemory, storage,
};
use nowayos_ipc::{Consumer, Producer};
use nowayos_net::arp::Mac;
use nowayos_net::{BUFFERS, DATA_REGION_LEN};
use nowayos_region::{
    Frame, NetQueue, ReadOnly, ReadWrite, RegionError, RingRx, RingTx, net_queue, read_only,
    read_write, ring_consumer, ring_producer,
};
use nowayos_seed::{SEED_RING_SLOT, SEED_RING_SLOTS};
use nowayos_socket::{
    ALLOW_LIST_MAX_LEN, AllowList, RING_SLOTS, RecordError, Report, Request, SLOT_LEN,
};
use nowayos_timer_api::{Client as Timer, MAX_PENDING, MAX_TIMEOUT_NANOS};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, NullHandler, debug_println};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: net-stack:";

/// The queues with the driver, read-write regions 0 to 3.
const RX_FREE: usize = 0;
const RX_ACTIVE: usize = 1;
const TX_FREE: usize = 2;
const TX_ACTIVE: usize = 3;

/// The receive buffers (read-only region 0) and the transmit buffers
/// (read-write region 4).
const RX_BUFFERS: usize = 0;
const TX_BUFFERS: usize = 4;

/// The seed ring: its control region read-write (5), its messages
/// read-only (1).
const SEED_CONTROL: usize = 5;
const SEED_MESSAGES: usize = 1;

/// The channel to the driver, and to the timer PD, which it calls and which
/// notifies it when a timeout expires. Channel 2 is from the seed's source,
/// which notifies it when a seed waits; while it waits for a seed, any
/// notification makes it look, so it need not name that channel.
const DRIVER: Channel = Channel::new(0);
const TIMER: Timer = Timer::new(Channel::new(1));

/// The socket client's channel: it notifies the stack of requests, the
/// stack it of reports.
const CLIENT: Channel = Channel::new(3);

/// The socket client's rings: the stack's reports (messages read-write 6,
/// the client's index read-only 3), and the client's requests (the stack's
/// index read-write 7, messages read-only 4); and the allow list,
/// read-only 2.
const REPORTS_MESSAGES: usize = 6;
const REPORTS_CONTROL: usize = 3;
const REQUESTS_CONTROL: usize = 7;
const REQUESTS_MESSAGES: usize = 4;
const ALLOW_LIST: usize = 2;

/// The stack's MAC address: what each system's `.qemu` file gives the card.
const OUR_MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

/// The id of the one timeout the stack keeps: when smoltcp is next due.
const POLL: u64 = 1;

/// The shortest wait between polls: a millisecond.
const MIN_DELAY_NANOS: u64 = 1_000_000;

/// One queue with the driver.
type Queue = NetQueue<BUFFERS>;

/// The link to the driver.
type NetLink = Link<Queue, RxBuffers, TxBuffers, BUFFERS>;

/// The stack's end of its seed ring.
type SeedRing = RingRx<SEED_RING_SLOTS, SEED_RING_SLOT>;

/// The socket client's two rings, the stack's ends.
struct SocketClient {
    requests: RingRx<RING_SLOTS, SLOT_LEN>,
    reports: RingTx<RING_SLOTS, SLOT_LEN>,
    allow: AllowList,
}

/// The client's requests, off its ring, each copied out and decoded.
struct RingRequests<'r>(Option<Consumer<'r, RingRx<RING_SLOTS, SLOT_LEN>, SLOT_LEN>>);

impl Requests for RingRequests<'_> {
    fn next(&mut self) -> Option<Result<Request, RecordError>> {
        let record = self.0.as_mut()?.try_recv_record().ok()??;
        Some(Request::decode(record.as_bytes()))
    }
}

/// The stack's reports, onto the client's ring.
struct RingReports<'r> {
    tx: Option<Producer<'r, RingTx<RING_SLOTS, SLOT_LEN>, SLOT_LEN>>,
    sent: bool,
}

impl Reports for RingReports<'_> {
    fn has_room(&self) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.free_slots().is_ok_and(|free| free > 0))
    }

    fn send(&mut self, report: &Report) -> bool {
        let Some(tx) = self.tx.as_mut() else {
            return false;
        };
        let mut slot = [0u8; SLOT_LEN];
        let sent = report
            .encode(&mut slot)
            .ok()
            .and_then(|len| slot.get(..len))
            .is_some_and(|bytes| tx.try_send_bytes(bytes).is_ok());
        self.sent |= sent;
        sent
    }
}

/// The receive buffers, mapped read-only.
struct RxBuffers(Frame<ReadOnly>);

impl RxMemory for RxBuffers {
    fn load(&self, offset: usize, out: &mut [u8]) -> bool {
        self.0.load(offset, out)
    }
}

/// The transmit buffers, mapped read-write.
struct TxBuffers(Frame<ReadWrite>);

impl TxMemory for TxBuffers {
    fn store(&mut self, offset: usize, bytes: &[u8]) -> bool {
        self.0.store(offset, bytes)
    }
}

/// Whether the stack withholds its client's destinations from the console
/// ([`init_withholding_destinations`]). Set once, before the stack starts.
static WITHHOLD: AtomicBool = AtomicBool::new(false);

/// Runs the network stack PD: what its program's `#[protection_domain]` entry
/// calls, for NoWayOS's systems and a product's own program alike. It runs the
/// PD's event loop itself and never returns (see the crate's documentation).
#[must_use]
pub fn init() -> NullHandler {
    run()
}

/// As [`init`], but the console never names a destination: the allow list is
/// counted and each connection said to go to an allowed destination - for a
/// system whose destination must not reach a log, such as a lab host's
/// address.
#[must_use]
pub fn init_withholding_destinations() -> NullHandler {
    WITHHOLD.store(true, Ordering::Relaxed);
    run()
}

fn run() -> NullHandler {
    debug_println!("{TAG} starting");
    let mut room = storage();
    let mut tcp = TcpRoom::new();
    let mut pd = StackPd::start(&mut room, &mut tcp);
    // The PD's event loop, run here so that `room` outlives the stack.
    run_forever(&mut pd)
}

/// rust-sel4's event loop for `pd`, which never returns.
#[allow(
    unreachable_code,
    reason = "Handler::run never returns: its type, Result<Never, Infallible>, has no value"
)]
fn run_forever(pd: &mut StackPd<'_>) -> ! {
    match pd.run() {
        Ok(never) => match never {},
        Err(infallible) => match infallible {},
    }
}

/// Where the stack is.
#[allow(
    clippy::large_enum_variant,
    reason = "one State lives for the PD's whole life, in init's frame: a PD has no heap to box into"
)]
enum State<'a> {
    /// It could not bind its regions: it does nothing.
    Down,
    /// It has no seed ring: never ready.
    NoSeedRing,
    /// Its link is bound; it waits for a seed.
    Waiting { link: NetLink, ring: SeedRing },
    /// Seeded, its interface up.
    Running { link: NetLink, stack: Stack<'a> },
}

/// The stack PD.
struct StackPd<'a> {
    state: State<'a>,
    /// The sockets' storage, until the stack takes it.
    room: Option<&'a mut Storage<'a>>,
    /// The connections' buffers, until the stack takes them.
    tcp: Option<&'a mut TcpRoom>,
    /// The socket client, if the system gives one.
    client: Option<SocketClient>,
    /// The dropped counts last reported.
    reported: Counts,
    /// Whether "waiting for a seed" was said.
    said_waiting: bool,
    /// Whether a failure of the timer PD was reported.
    said_timer: bool,
}

impl<'a> StackPd<'a> {
    /// Binds the link and the seed ring, and brings the interface up if a
    /// seed is already there.
    fn start(room: &'a mut Storage<'a>, tcp: &'a mut TcpRoom) -> Self {
        let mut pd = Self {
            state: State::Down,
            room: Some(room),
            tcp: Some(tcp),
            client: None,
            reported: Counts::default(),
            said_waiting: false,
            said_timer: false,
        };
        let link = match bind_link() {
            Ok(link) => link,
            Err(error) => {
                debug_println!("{TAG} gave up: {error}");
                return pd;
            }
        };
        pd.client = bind_client();
        let ring = match bind_seed_ring() {
            Ok(ring) => ring,
            Err(error) => {
                debug_println!("{TAG} not ready: no seed ring ({error}): no interface brought up");
                pd.state = State::NoSeedRing;
                return pd;
            }
        };
        pd.state = State::Waiting { link, ring };
        pd.take_seed();
        pd
    }

    /// Takes a seed off the seed ring, if one waits, and brings the
    /// interface up with it. The time is read first: if the timer PD fails,
    /// the seed stays on its ring for the next notification to retry.
    fn take_seed(&mut self) {
        let State::Waiting { ring, .. } = &self.state else {
            return;
        };
        let now = match TIMER.now() {
            Ok(now) => now,
            Err(error) => {
                if !self.said_timer {
                    self.said_timer = true;
                    debug_println!("{TAG} not ready: no monotonic time: {error}");
                }
                return;
            }
        };
        let mut seed = None;
        for _ in 0..SEED_RING_SLOTS {
            match nowayos_seed::take(ring) {
                Ok(Some(mut taken)) => {
                    let folded = StackSeed::from_seed(&taken);
                    taken.wipe();
                    match folded {
                        Some(folded) => {
                            seed = Some(folded);
                            break;
                        }
                        None => debug_println!("{TAG} a seed refused: it folds to zero"),
                    }
                }
                Ok(None) => break,
                Err(error) => debug_println!("{TAG} a seed refused: {error}"),
            }
        }
        let Some(seed) = seed else {
            if !self.said_waiting {
                self.said_waiting = true;
                debug_println!("{TAG} not ready: waiting for a seed on its seed ring");
            }
            return;
        };
        let (State::Waiting { mut link, .. }, Some(room)) = (
            core::mem::replace(&mut self.state, State::Down),
            self.room.take(),
        ) else {
            return;
        };
        let made = match (self.client.as_ref(), self.tcp.take()) {
            (Some(client), Some(tcp)) => {
                Stack::with_connections(OUR_MAC, seed, &mut link, room, tcp, client.allow, now)
            }
            _ => Stack::new(OUR_MAC, seed, &mut link, room, now),
        };
        match made {
            Ok(stack) => {
                link.start();
                debug_println!("{TAG} seeded; interface up at {OUR_MAC}: asking for a DHCP lease");
                self.state = State::Running { link, stack };
                self.service();
            }
            Err(error) => debug_println!("{TAG} gave up: {error}"),
        }
    }

    /// One round with the driver and smoltcp: frames in and out, twice -
    /// the second time after asking the driver for its next notification,
    /// as sDDF's protocol has it - then the driver notified if it asked,
    /// what was dropped reported if it changed, and the timer set for when
    /// smoltcp is next due.
    fn service(&mut self) {
        let State::Running { link, stack } = &mut self.state else {
            return;
        };
        let now = match TIMER.now() {
            Ok(now) => now,
            Err(error) => {
                if !self.said_timer {
                    self.said_timer = true;
                    debug_println!("{TAG} could not read the time: {error}");
                }
                return;
            }
        };
        let mut reported = false;
        for _ in 0..2 {
            link.service();
            match stack.poll(link, now) {
                Some(Event::Leased(lease)) => debug_println!("{TAG} DHCP lease {lease}"),
                Some(Event::Lost) => debug_println!("{TAG} DHCP lease lost"),
                None => {}
            }
            if let Some(client) = self.client.as_ref() {
                let mut requests = RingRequests(Consumer::new(&client.requests).ok());
                let mut reports = RingReports {
                    tx: Producer::new(&client.reports).ok(),
                    sent: false,
                };
                stack.serve(now, &mut requests, &mut reports, &mut |notice| {
                    say(notice);
                });
                reported |= reports.sent;
                // What the client asked may have frames to send now.
                stack.poll(link, now);
            }
            link.request_signals();
        }
        if reported {
            CLIENT.notify();
        }
        if link.wake_driver() {
            DRIVER.notify();
        }
        let dropped = link.counts().dropped();
        if dropped != self.reported {
            self.reported = dropped;
            debug_println!(
                "{TAG} dropped so far: {} oversize frames, {} malformed frames, {} corrupt queue indices, {} frames unsent",
                dropped.oversize,
                dropped.malformed,
                dropped.indices,
                dropped.unsent
            );
        }
        let delay = stack.poll_delay(now);
        self.schedule(delay);
    }

    /// Sets the timer for when smoltcp is next due: `delay` nanoseconds
    /// from now, at least a millisecond; none if nothing is due.
    fn schedule(&mut self, delay: Option<u64>) {
        let _ = TIMER.cancel(POLL);
        let Some(delay) = delay else {
            return;
        };
        if let Err(error) = TIMER.set_timeout(POLL, delay.clamp(MIN_DELAY_NANOS, MAX_TIMEOUT_NANOS))
            && !self.said_timer
        {
            self.said_timer = true;
            debug_println!("{TAG} could not set a timeout: {error}");
        }
    }
}

/// Says what happened to a connection - without its destination, if the
/// stack withholds them.
fn say(notice: Notice) {
    if !WITHHOLD.load(Ordering::Relaxed) {
        debug_println!("{TAG} socket client: {notice}");
        return;
    }
    match notice {
        Notice::Opened { connection, .. } => {
            debug_println!("{TAG} socket client: {connection} to an allowed destination open");
        }
        Notice::Refused { connection, .. } => debug_println!(
            "{TAG} socket client: refused {connection}: not in the system's allow list (destination withheld)"
        ),
        other => debug_println!("{TAG} socket client: {other}"),
    }
}

/// The socket client's rings and allow list, if the system gives them: none
/// if it maps no reports ring; a client with nothing allowed if its allow
/// list is missing or refused, which the stack says.
fn bind_client() -> Option<SocketClient> {
    let reports_messages = match read_write(REPORTS_MESSAGES) {
        Ok(region) => region,
        Err(RegionError::NotMapped { .. }) => return None,
        Err(error) => {
            debug_println!("{TAG} socket client: not served: {error}");
            return None;
        }
    };
    let rings = bind_client_rings(reports_messages);
    let (requests, reports) = match rings {
        Ok(rings) => rings,
        Err(error) => {
            debug_println!("{TAG} socket client: not served: {error}");
            return None;
        }
    };
    let allow = match read_allow_list() {
        Ok(allow) => {
            if WITHHOLD.load(Ordering::Relaxed) {
                debug_println!(
                    "{TAG} socket client: may connect to {} destination(s), withheld",
                    allow.iter().count()
                );
            } else {
                for destination in allow.iter() {
                    debug_println!("{TAG} socket client: may connect to {destination}");
                }
            }
            allow
        }
        Err(error) => {
            debug_println!("{TAG} socket client: no destination allowed: {error}");
            AllowList::empty()
        }
    };
    Some(SocketClient {
        requests,
        reports,
        allow,
    })
}

/// The client's requests ring and the stack's reports ring, around the
/// reports ring's messages region, already taken.
#[allow(
    clippy::type_complexity,
    reason = "the two ring ends, each named by its geometry"
)]
fn bind_client_rings(
    reports_messages: nowayos_region::Region<ReadWrite>,
) -> Result<(RingRx<RING_SLOTS, SLOT_LEN>, RingTx<RING_SLOTS, SLOT_LEN>), RegionError> {
    let reports = ring_producer(reports_messages, read_only(REPORTS_CONTROL)?)?;
    let requests = ring_consumer(read_write(REQUESTS_CONTROL)?, read_only(REQUESTS_MESSAGES)?)?;
    Ok((requests, reports))
}

/// Why the allow list could not be read.
enum AllowListError {
    Region(RegionError),
    List(nowayos_socket::AllowListError),
}

impl core::fmt::Display for AllowListError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Region(error) => write!(f, "its region: {error}"),
            Self::List(error) => write!(f, "{error}"),
        }
    }
}

/// The allow list, from read-only region [`ALLOW_LIST`]'s words.
fn read_allow_list() -> Result<AllowList, AllowListError> {
    let words = read_only(ALLOW_LIST)
        .and_then(|region| region.into_words())
        .map_err(AllowListError::Region)?;
    let mut bytes = [0u8; ALLOW_LIST_MAX_LEN];
    for (index, chunk) in bytes.chunks_exact_mut(4).enumerate() {
        chunk.copy_from_slice(&words.load(index).unwrap_or(0).to_le_bytes());
    }
    AllowList::decode(&bytes).map_err(AllowListError::List)
}

/// The stack's end of its seed ring, if the system description maps one.
fn bind_seed_ring() -> Result<SeedRing, RegionError> {
    ring_consumer(read_write(SEED_CONTROL)?, read_only(SEED_MESSAGES)?)
}

/// The link's queues and buffers, as the system description maps them.
fn bind_link() -> Result<NetLink, BindError> {
    let rx_buffers = read_only(RX_BUFFERS)?.into_frame(DATA_REGION_LEN)?;
    let tx_buffers = read_write(TX_BUFFERS)?.into_frame(DATA_REGION_LEN)?;
    if rx_buffers.len() != DATA_REGION_LEN || tx_buffers.len() != DATA_REGION_LEN {
        return Err(BindError::Size);
    }
    let queues = Queues {
        rx_free: net_queue(read_write(RX_FREE)?)?,
        rx_active: net_queue(read_write(RX_ACTIVE)?)?,
        tx_free: net_queue(read_write(TX_FREE)?)?,
        tx_active: net_queue(read_write(TX_ACTIVE)?)?,
    };
    Link::new(queues, RxBuffers(rx_buffers), TxBuffers(tx_buffers)).map_err(BindError::Link)
}

/// Why the link could not be bound.
enum BindError {
    Region(RegionError),
    Link(nowayos_ip::LinkError),
    Size,
}

impl From<RegionError> for BindError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl core::fmt::Display for BindError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Region(error) => write!(f, "{error}"),
            Self::Link(error) => write!(f, "{error}"),
            Self::Size => write!(f, "a data region is not {DATA_REGION_LEN} bytes"),
        }
    }
}

impl Handler for StackPd<'_> {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(TIMER.channel()) {
            // Collect the expired timeout, so the next one can be set.
            for _ in 0..=MAX_PENDING {
                if !matches!(TIMER.expired(), Ok(Some(_))) {
                    break;
                }
            }
        }
        // While it waits, any notification retries the seed: the seed
        // source notifies once, and a failed read of the time leaves the
        // seed on its ring.
        match self.state {
            State::Waiting { .. } => self.take_seed(),
            State::Running { .. } => self.service(),
            _ => {}
        }
        Ok(())
    }
}
