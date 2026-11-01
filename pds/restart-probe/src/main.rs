// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-restart-probe`: the three PDs of the restart test
//! (`systems/restart-test.system`), each picked by the PD name the system
//! description gives it.
//!
//! * `restarter` is `worker`'s parent. When `worker` faults, it restarts it
//!   at its restart entry (`nowayos-restart`), from the header of `worker`'s
//!   restart image, which it maps read-only - at most three times. It reads
//!   a fault's label only, never parsing the fault (a panic's label
//!   does not parse).
//! * `worker` serves `client` over two rings: requests in, answers out. It
//!   answers each `Work` request with `Done`, counts each `Tick`, and panics
//!   on a `Crash` request, as a PD does on input it cannot handle. At each
//!   start it shows that its memory is as its program was loaded - a counter
//!   in `.bss` reads 1, a word in `.data` holds its initial value, a
//!   once-only flag is clear - and sets the runtime's panic hook, which
//!   panics if one is set already. Then it starts its next restart epoch on
//!   both rings, drops the requests in flight, and notifies `client`.
//! * `client` asks for work, then sends a tick, a crash request, another
//!   tick and more work in one go. When it sees the worker's new restart
//!   epoch, it sends the unanswered work again - per message type, and not
//!   the ticks or the crash request - and the restarted worker answers it.
//!
//! Test instruments: never in a product image.

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use nowayos_ipc::{
    Consumer, DecodeError, EncodeError, EpochChange, EpochWatch, Message, Producer, Reader, Writer,
};
use nowayos_region::{RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer};
use nowayos_restart::image::HEADER_LEN;
use nowayos_restart::{Header, Start, restart, started};
use sel4_microkit::{
    Channel, ChannelSet, Child, Handler, Infallible, MessageInfo, debug_println, pd_name,
    protection_domain,
};

/// Start of every line this program prints.
const TAG: &str = "NOWAYOS: restart-probe:";

/// The channel between `worker` and `client` (number 0 at both ends).
const PEER: Channel = Channel::new(0);

/// `worker`'s child id under `restarter`.
const WORKER: usize = 0;

/// How many times `restarter` restarts `worker`.
const MAX_RESTARTS: u32 = 3;

/// Both rings: slots, and bytes per slot.
const SLOTS: usize = 8;
const SLOT: usize = 16;

/// What `worker` keeps in `.bss` and `.data`: fresh at every start.
static STARTS: AtomicU32 = AtomicU32::new(0);
static DATA_WORD: AtomicU32 = AtomicU32::new(DATA_WORD_AT_LOAD);
static ONCE: AtomicBool = AtomicBool::new(false);
const DATA_WORD_AT_LOAD: u32 = 0x5EED;

/// Work to do: answered with [`Done`]; sent again if the worker restarts
/// before it answers.
struct Work(u32);

/// An event the worker only counts: never sent again.
struct Tick(u32);

/// The test's stand-in for input a PD cannot handle: the worker panics.
/// Never sent again.
struct Crash;

/// The worker's answer to [`Work`].
struct Done(u32);

/// One `u32` payload: the encoding all but [`Crash`] share.
macro_rules! word_message {
    ($name:ident, $kind:literal) => {
        impl Message for $name {
            const KIND: u16 = $kind;
            const VERSION: u16 = 1;
            const PAYLOAD_LEN: usize = 4;

            fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
                out.write_u32(self.0)
            }

            fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
                input.read_u32().map(Self)
            }
        }
    };
}

word_message!(Work, 0x0301);
word_message!(Tick, 0x0302);
word_message!(Done, 0x0381);

impl Message for Crash {
    const KIND: u16 = 0x0303;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 0;

    fn encode_payload(&self, _out: &mut Writer<'_>) -> Result<(), EncodeError> {
        Ok(())
    }

    fn decode_payload(_input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
}

#[protection_domain]
fn init() -> Probe {
    match pd_name() {
        Ok("restarter") => Probe::Restarter(restarter()),
        Ok("worker") => worker().map_or(Probe::Idle, Probe::Worker),
        Ok("client") => client().map_or(Probe::Idle, Probe::Client),
        _ => {
            debug_println!("{TAG} no part is defined for this PD's name");
            Probe::Idle
        }
    }
}

/// Each PD's part.
enum Probe {
    Restarter(Restarter),
    Worker(Worker),
    Client(Client),
    /// Could not start its part (it said why).
    Idle,
}

impl Handler for Probe {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(PEER) {
            match self {
                Self::Worker(worker) => worker.serve(),
                Self::Client(client) => client.step(),
                Self::Restarter(_) | Self::Idle => {}
            }
        }
        Ok(())
    }

    fn fault(
        &mut self,
        child: Child,
        msg_info: MessageInfo,
    ) -> Result<Option<MessageInfo>, Self::Error> {
        if let Self::Restarter(restarter) = self {
            restarter.fault(child, msg_info.label());
        }
        // No reply: a restarted child's fault message is gone, and a child
        // left stopped stays stopped.
        Ok(None)
    }
}

/// `worker`'s parent.
struct Restarter {
    /// The header of `worker`'s restart image (`nowayos_ro_region_0`).
    image: Option<Header>,
    restarts: u32,
}

fn restarter() -> Restarter {
    let image = read_only(0)
        .and_then(|region| region.into_frame(HEADER_LEN))
        .ok()
        .and_then(|frame| {
            let mut bytes = [0_u8; HEADER_LEN];
            frame.load(0, &mut bytes).then_some(bytes)
        })
        .and_then(|bytes| Header::parse(&bytes).ok());
    if image.is_some() {
        debug_println!(
            "{TAG} restarter: starting; it restarts worker (child 0) from its restart image"
        );
    } else {
        debug_println!("{TAG} restarter: cannot read worker's restart image: it cannot restart it");
    }
    Restarter { image, restarts: 0 }
}

impl Restarter {
    fn fault(&mut self, child: Child, label: u64) {
        if child.index() != WORKER {
            debug_println!(
                "{TAG} restarter: child {} faulted (label {label}): left stopped",
                child.index()
            );
            return;
        }
        let Some(image) = self.image else {
            debug_println!(
                "{TAG} restarter: worker faulted (label {label}); cannot restart it: no restart image"
            );
            return;
        };
        if self.restarts >= MAX_RESTARTS {
            debug_println!(
                "{TAG} restarter: worker faulted again (label {label}); restarted {MAX_RESTARTS} times already: left stopped"
            );
            return;
        }
        self.restarts = self.restarts.saturating_add(1);
        debug_println!("{TAG} restarter: worker fault detail: label {label}");
        match restart(child, &image) {
            Ok(()) => debug_println!(
                "{TAG} restarter: worker stopped by a fault; restarted it at its restart entry (restart {} of {MAX_RESTARTS})",
                self.restarts
            ),
            Err(error) => debug_println!("{TAG} restarter: cannot restart worker: {error}"),
        }
    }
}

/// `worker`'s own panic hook: setting it shows the runtime's once-only cell
/// is fresh, since setting it twice panics.
fn worker_panic_hook(info: &PanicInfo<'_>) {
    debug_println!("{TAG} worker panicked: {}", info.message());
}

/// `worker`: its ends of the two rings.
struct Worker {
    /// Requests from `client`: control read-write (`nowayos_rw_region_0`),
    /// messages read-only (`nowayos_ro_region_0`).
    requests: RingRx<SLOTS, SLOT>,
    /// Answers to `client`: messages read-write (`nowayos_rw_region_1`),
    /// control read-only (`nowayos_ro_region_1`).
    answers: RingTx<SLOTS, SLOT>,
}

fn worker() -> Option<Worker> {
    let how = match started() {
        Start::ColdChecked => "started from its checked restart image",
        Start::Restarted => "restarted from its restart image",
        Start::Cold => "started without a restart image",
        Start::NotThroughEntry => "started without its restart entry",
    };
    let starts = STARTS.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let word = DATA_WORD.fetch_add(1, Ordering::Relaxed);
    let once = ONCE.swap(true, Ordering::Relaxed);
    sel4_microkit::panicking::set_hook(&worker_panic_hook);
    debug_println!(
        "{TAG} worker: {how} - .bss counter {starts}, .data word {word:#x}, once-only flag {}",
        if once { "SET" } else { "clear" }
    );
    if starts != 1 || word != DATA_WORD_AT_LOAD || once {
        debug_println!("{TAG} worker: SURVIVED: memory from before this start is still there");
    }
    let rings = read_write(0)
        .and_then(|control| ring_consumer(control, read_only(0)?))
        .and_then(|requests| Ok((requests, ring_producer(read_write(1)?, read_only(1)?)?)));
    let Ok((requests, answers)) = rings else {
        debug_println!("{TAG} worker: gave up: its rings are not mapped as it expects");
        return None;
    };
    let (Ok(mut rx), Ok(mut tx)) = (Consumer::new(&requests), Producer::new(&answers)) else {
        debug_println!("{TAG} worker: gave up: a ring is broken");
        return None;
    };
    let epoch = rx.begin_epoch();
    tx.begin_epoch();
    let dropped = rx.skip_in_flight().unwrap_or(0);
    debug_println!(
        "{TAG} worker: restart epoch {epoch}; client's epoch {}; dropped {dropped} request(s) in flight",
        rx.peer_epoch()
    );
    PEER.notify();
    Some(Worker { requests, answers })
}

impl Worker {
    fn serve(&mut self) {
        let (Ok(mut rx), Ok(mut tx)) =
            (Consumer::new(&self.requests), Producer::new(&self.answers))
        else {
            debug_println!("{TAG} worker: a ring is broken");
            return;
        };
        let mut answered = false;
        while let Ok(Some(record)) = rx.try_recv_record() {
            match record.header().map(|header| header.kind) {
                Ok(Work::KIND) => {
                    if let Ok(Work(number)) = record.decode::<Work>()
                        && tx.try_send(&Done(number)).is_ok()
                    {
                        answered = true;
                    }
                }
                Ok(Tick::KIND) => {
                    if let Ok(Tick(number)) = record.decode::<Tick>() {
                        debug_println!("{TAG} worker: tick {number}");
                    }
                }
                Ok(Crash::KIND) => {
                    debug_println!(
                        "{TAG} worker: crash request - panicking, as a PD does on input it cannot handle"
                    );
                    crash();
                }
                _ => debug_println!("{TAG} worker: refused a record"),
            }
        }
        if answered {
            PEER.notify();
        }
    }
}

/// The test's stand-in for a bug that input triggers.
#[allow(
    clippy::panic,
    reason = "the worker must fault, as a PD whose code panics on input does"
)]
fn crash() -> ! {
    panic!("asked to crash")
}

/// `client`: its ends of the two rings, what it knows of the worker's
/// restart epoch, and its place in the test.
struct Client {
    /// Requests to `worker`: messages read-write (`nowayos_rw_region_0`),
    /// control read-only (`nowayos_ro_region_0`).
    requests: RingTx<SLOTS, SLOT>,
    /// Answers from `worker`: control read-write (`nowayos_rw_region_1`),
    /// messages read-only (`nowayos_ro_region_1`).
    answers: RingRx<SLOTS, SLOT>,
    worker: EpochWatch,
    /// The work sent and not yet answered.
    waiting: Option<u32>,
}

fn client() -> Option<Client> {
    let rings = read_write(0)
        .and_then(|messages| ring_producer(messages, read_only(0)?))
        .and_then(|requests| Ok((requests, ring_consumer(read_write(1)?, read_only(1)?)?)));
    let Ok((requests, answers)) = rings else {
        debug_println!("{TAG} client: gave up: its rings are not mapped as it expects");
        return None;
    };
    let (Ok(mut tx), Ok(mut rx)) = (Producer::new(&requests), Consumer::new(&answers)) else {
        debug_println!("{TAG} client: gave up: a ring is broken");
        return None;
    };
    let epoch = tx.begin_epoch();
    rx.begin_epoch();
    let _ = rx.skip_in_flight();
    let mut worker = EpochWatch::new();
    let seen = tx.peer_epoch();
    worker.observe(seen);
    debug_println!("{TAG} client: restart epoch {epoch}; worker's epoch {seen}");
    let mut client = Client {
        requests,
        answers,
        worker,
        waiting: None,
    };
    client.send_work(1);
    Some(client)
}

impl Client {
    /// Sends work `number` and waits for its answer.
    fn send_work(&mut self, number: u32) {
        let sent =
            Producer::new(&self.requests).is_ok_and(|mut tx| tx.try_send(&Work(number)).is_ok());
        if sent {
            self.waiting = Some(number);
            PEER.notify();
        } else {
            debug_println!("{TAG} client: could not send work {number}");
        }
    }

    fn step(&mut self) {
        let Ok(worker_epoch) = Producer::new(&self.requests).map(|tx| tx.peer_epoch()) else {
            debug_println!("{TAG} client: the request ring is broken");
            return;
        };
        // First: has the worker started again? Then what was in flight to it
        // is gone, and each message type says what to do about it.
        if let EpochChange::Changed { from, to } = self.worker.observe(worker_epoch) {
            debug_println!("{TAG} client: worker restarted (epoch {from} -> {to})");
            if let Some(number) = self.waiting {
                self.send_work(number);
                debug_println!(
                    "{TAG} client: sent work {number} again; the ticks and the crash request in flight are not sent again"
                );
            }
        }
        let mut done = None;
        if let Ok(mut rx) = Consumer::new(&self.answers) {
            while let Ok(Some(record)) = rx.try_recv_record() {
                if let Ok(Done(number)) = record.decode::<Done>()
                    && self.waiting == Some(number)
                {
                    self.waiting = None;
                    done = Some(number);
                    debug_println!(
                        "{TAG} client: work {number} done (worker epoch {worker_epoch})"
                    );
                }
            }
        }
        match done {
            Some(1) => self.crash_the_worker(),
            Some(2) => debug_println!("{TAG} client: the worker serves again after its restart"),
            _ => {}
        }
    }

    /// Sends a tick, a crash request, another tick and more work, at once.
    fn crash_the_worker(&mut self) {
        let sent = Producer::new(&self.requests).is_ok_and(|mut tx| {
            tx.try_send(&Tick(1)).is_ok()
                && tx.try_send(&Crash).is_ok()
                && tx.try_send(&Tick(2)).is_ok()
                && tx.try_send(&Work(2)).is_ok()
        });
        if sent {
            self.waiting = Some(2);
            debug_println!("{TAG} client: sent tick 1, a crash request, tick 2 and work 2");
            PEER.notify();
        } else {
            debug_println!("{TAG} client: could not send the crash test's requests");
        }
    }
}
