// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-socket-test`: a test instrument, in
//! `systems/tcp-test.system`: the network stack's socket client, as the TLS
//! PD or a remote-display session PD will be one, holding no network device.
//!
//! Through its pair of `nowayos-ipc` rings of `nowayos-socket`'s records it
//! asks the stack for a TCP connection to 10.0.2.100 port 7, where the QEMU
//! system's user network runs an echo service (`tcp-test.qemu`); sends a
//! line and checks it comes back byte for byte; closes the connection and
//! waits for the stack's end of it. Then it asks for 10.0.2.100 port 8 and
//! 10.0.2.2 port 7, which the system's allow list does not hold, and
//! checks each is refused as not allowed. Every report is checked by its
//! own connection table (`ClientTable`) before it is used; one line a step.
//! Never in a demo or product system.

#![no_std]
#![no_main]

use core::net::Ipv4Addr;

use nowayos_ipc::{Consumer, Producer};
use nowayos_region::{RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer};
use nowayos_socket::{
    ClientTable, Close, Connect, ConnectionId, FailReason, RING_SLOTS, Report, Request, SLOT_LEN,
    SendData, Verdict,
};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, protection_domain};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: socket-test:";

/// The channel to the stack.
const STACK: Channel = Channel::new(0);

/// Its requests ring (messages read-write 0, the stack's index read-only
/// 0) and the stack's reports ring (its index read-write 1, messages
/// read-only 1).
const REQUESTS_MESSAGES: usize = 0;
const REQUESTS_CONTROL: usize = 0;
const REPORTS_CONTROL: usize = 1;
const REPORTS_MESSAGES: usize = 1;

/// The echo service, which the allow list holds.
const ECHO: (Ipv4Addr, u16) = (Ipv4Addr::new(10, 0, 2, 100), 7);

/// What it sends and expects back.
const LINE: &[u8] = b"nowayos socket-test: an echo over TCP, both ways\n";

/// Destinations the allow list does not hold: another port, another host.
const REFUSED: [(Ipv4Addr, u16); 2] = [
    (Ipv4Addr::new(10, 0, 2, 100), 8),
    (Ipv4Addr::new(10, 0, 2, 2), 7),
];

/// Where the test is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Waiting for the echo connection to open.
    Opening,
    /// Waiting for the line to come back; how much has.
    Echoing(usize),
    /// Waiting for the stack's end of the connection.
    Closing,
    /// Waiting for the refusal of `REFUSED[n]`.
    Refusing(usize),
    /// Finished, or stopped.
    Done,
}

struct SocketTest {
    requests: RingTx<RING_SLOTS, SLOT_LEN>,
    reports: RingRx<RING_SLOTS, SLOT_LEN>,
    table: ClientTable,
    step: Step,
    echoed: [u8; LINE.len()],
}

#[protection_domain]
fn init() -> SocketTestPd {
    debug_println!("{TAG} starting");
    let rings = read_write(REQUESTS_MESSAGES)
        .and_then(|messages| ring_producer(messages, read_only(REQUESTS_CONTROL)?))
        .and_then(|requests| {
            Ok((
                requests,
                ring_consumer(read_write(REPORTS_CONTROL)?, read_only(REPORTS_MESSAGES)?)?,
            ))
        });
    let (requests, reports) = match rings {
        Ok(rings) => rings,
        Err(error) => {
            debug_println!("{TAG} gave up: its rings: {error}");
            return SocketTestPd(None);
        }
    };
    let mut test = SocketTest {
        requests,
        reports,
        table: ClientTable::new(),
        step: Step::Opening,
        echoed: [0; LINE.len()],
    };
    // Each line is printed before its request goes: the stack, above this
    // PD, answers at once, and its lines come after.
    debug_println!("{TAG} asking for a connection to {}:{}", ECHO.0, ECHO.1);
    let connect = Connect::new(ConnectionId(1), ECHO.0, ECHO.1);
    if !connect.is_some_and(|connect| test.ask(Request::Connect(connect))) {
        test.stop("could not ask for the connection");
    }
    SocketTestPd(Some(test))
}

impl SocketTest {
    /// Sends `request`, checked by the table first; notifies the stack.
    fn ask(&mut self, request: Request) -> bool {
        if self.table.send(&request).is_err() {
            return false;
        }
        let Ok(mut tx) = Producer::new(&self.requests) else {
            return false;
        };
        let mut slot = [0u8; SLOT_LEN];
        let sent = request
            .encode(&mut slot)
            .ok()
            .and_then(|len| slot.get(..len))
            .is_some_and(|bytes| tx.try_send_bytes(bytes).is_ok());
        if sent {
            STACK.notify();
        }
        sent
    }

    fn stop(&mut self, why: &str) {
        debug_println!("{TAG} FAILED: {why}");
        self.step = Step::Done;
    }

    /// Every report waiting, each checked by the table, then acted on.
    fn take_reports(&mut self) {
        loop {
            let Ok(mut rx) = Consumer::new(&self.reports) else {
                self.stop("the reports ring is broken");
                return;
            };
            let record = match rx.try_recv_record() {
                Ok(Some(record)) => record,
                Ok(None) => return,
                Err(_) => {
                    self.stop("the reports ring is broken");
                    return;
                }
            };
            let report = match Report::decode(record.as_bytes()) {
                Ok(report) => report,
                Err(error) => {
                    self.stop("a report that did not decode");
                    debug_println!("{TAG} {error}");
                    return;
                }
            };
            match self.table.receive(&report) {
                Ok(Verdict::Act) => self.act(&report),
                Ok(Verdict::Ignore) => {}
                Err(error) => {
                    debug_println!("{TAG} a report refused: {error}");
                    self.stop("the stack broke the protocol");
                }
            }
        }
    }

    fn act(&mut self, report: &Report) {
        match (self.step, report) {
            (Step::Opening, Report::Connected(_)) => {
                debug_println!("{TAG} connected to {}:{}", ECHO.0, ECHO.1);
                self.step = Step::Echoing(0);
                if !SendData::new(ConnectionId(1), LINE)
                    .is_some_and(|data| self.ask(Request::Send(data)))
                {
                    self.stop("could not send the line");
                }
            }
            (Step::Echoing(have), Report::Received(data)) => {
                let end = have.saturating_add(data.len());
                match self.echoed.get_mut(have..end) {
                    Some(room) => room.copy_from_slice(data.bytes()),
                    None => {
                        self.stop("more came back than was sent");
                        return;
                    }
                }
                if end < LINE.len() {
                    self.step = Step::Echoing(end);
                } else if self.echoed == *LINE {
                    debug_println!("{TAG} {} bytes echoed back, byte for byte", LINE.len());
                    if self.ask(Request::Close(Close::new(ConnectionId(1)))) {
                        self.step = Step::Closing;
                    } else {
                        self.stop("could not close");
                    }
                } else {
                    self.stop("the echo differs from the line sent");
                }
            }
            (Step::Closing, Report::Closed(_)) => {
                debug_println!("{TAG} connection closed cleanly");
                self.refuse_next(0);
            }
            (Step::Refusing(index), Report::Failed(failed)) => {
                let Some((address, port)) = REFUSED.get(index).copied() else {
                    self.stop("a refusal out of turn");
                    return;
                };
                if failed.reason() != FailReason::NotAllowed {
                    debug_println!("{TAG} {address}:{port}: {}", failed.reason());
                    self.stop("a destination not allowed failed some other way");
                    return;
                }
                debug_println!("{TAG} {address}:{port} refused: {}", failed.reason());
                let id = failed.connection();
                if !self.ask(Request::Close(Close::new(id))) {
                    self.stop("could not close a refused connection's name");
                    return;
                }
                self.refuse_next(index.saturating_add(1));
            }
            (_, Report::Failed(failed)) => {
                debug_println!("{TAG} {} failed: {}", failed.connection(), failed.reason());
                self.stop("a connection failed");
            }
            _ => self.stop("a report out of turn"),
        }
    }

    /// Asks for `REFUSED[index]`, or finishes.
    fn refuse_next(&mut self, index: usize) {
        let Some((address, port)) = REFUSED.get(index).copied() else {
            debug_println!("{TAG} done");
            self.step = Step::Done;
            return;
        };
        let id = ConnectionId(u16::try_from(index).unwrap_or(0).saturating_add(2));
        debug_println!("{TAG} asking for a connection to {address}:{port}");
        self.step = Step::Refusing(index);
        if !Connect::new(id, address, port)
            .is_some_and(|connect| self.ask(Request::Connect(connect)))
        {
            self.stop("could not ask");
        }
    }
}

/// The test, if its rings were bound.
struct SocketTestPd(Option<SocketTest>);

impl Handler for SocketTestPd {
    type Error = Infallible;

    fn notified(&mut self, _channels: ChannelSet) -> Result<(), Self::Error> {
        if let Some(test) = self.0.as_mut()
            && test.step != Step::Done
        {
            test.take_reports();
        }
        Ok(())
    }
}
