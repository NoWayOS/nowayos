// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the socket records between the network stack PD and its
//! socket clients (`nowayos-socket`) - any bytes decoded as each
//! ring's records, and records decoded from the fuzzer's bytes driving the
//! two connection tables.
//!
//! Half the inputs are one record: any bytes, or any payload behind a valid
//! header for one of the seven records. Checks: nothing panics; each ring's
//! decoder agrees with itself (`decode_exact` is `decode` plus "nothing
//! after the record") and with each record's own decoder
//! (`nowayos_fuzz::check_decode`); whatever decodes re-encodes to the
//! bytes it came from; and a data record is the one its bytes make, with
//! only zeros after them.
//!
//! The other half are a sequence of records, each a kind, a connection
//! among four and a payload from the fuzzer behind that kind's header.
//! Each one that decodes is offered to both tables: a request to the
//! client's own check and the stack's, a report to the stack's own check
//! and the client's. Checks: a refusal leaves the table unchanged and names
//! the record's connection; neither table ever holds more than its
//! capacity; and while the two tables agree, a record its sender's table
//! accepts is accepted by the receiver's too, as nothing is in flight.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_socket::{
    ClientTable, Close, Closed, Connect, Connected, ConnectionId, ConnectionState, Data, Failed,
    ReceivedData, Report, Request, SendData, StackTable,
};

/// Each table's capacity: small, so that a full table is common.
const CAPACITY: usize = 2;
/// The most records one input drives through the tables.
const MAX_STEPS: usize = 256;

/// A data record decoded is the one `new` makes from its bytes: 1 to
/// `MAX_DATA` of them, and only zeros after them. Re-encoding alone would
/// not show the zeros, as a record keeps its padding and encodes it back.
fn check_data<const KIND: u16>(data: &Data<KIND>) {
    assert!(
        Data::<KIND>::new(data.connection(), data.bytes()).as_ref() == Some(data),
        "{data:?} is not the record its bytes make"
    );
}

/// `bytes` as each ring's records: the two decoders agree with themselves,
/// and what decodes is canonical.
fn check_ring_decoders(bytes: &[u8]) {
    let request = Request::decode(bytes);
    let exact = Request::decode_exact(bytes);
    assert!(
        exact.is_err() || exact == request,
        "decode {request:?}, decode_exact {exact:?}"
    );
    if let Ok(Request::Send(data)) = &request {
        check_data(data);
    }
    if let Ok(Report::Received(data)) = &Report::decode(bytes) {
        check_data(data);
    }
    if let Ok(request) = request {
        let mut out = [0u8; FRAME_MAX];
        let len = request.encode(&mut out);
        assert!(
            len.is_ok_and(|len| out.get(..len) == bytes.get(..len)),
            "{request:?} re-encodes to other bytes"
        );
    }
    let report = Report::decode(bytes);
    let exact = Report::decode_exact(bytes);
    assert!(
        exact.is_err() || exact == report,
        "decode {report:?}, decode_exact {exact:?}"
    );
    if let Ok(report) = report {
        let mut out = [0u8; FRAME_MAX];
        let len = report.encode(&mut out);
        assert!(
            len.is_ok_and(|len| out.get(..len) == bytes.get(..len)),
            "{report:?} re-encodes to other bytes"
        );
    }
}

/// One record, from any bytes or behind a valid header.
fn one_record(input: &Input<'_>, selector: u8) {
    let mut buf = [0u8; FRAME_MAX];
    let record = match selector % 8 {
        0 => frame::<Connect>(input.rest(), &mut buf),
        1 => frame::<SendData>(input.rest(), &mut buf),
        2 => frame::<Close>(input.rest(), &mut buf),
        3 => frame::<Connected>(input.rest(), &mut buf),
        4 => frame::<ReceivedData>(input.rest(), &mut buf),
        5 => frame::<Closed>(input.rest(), &mut buf),
        6 => frame::<Failed>(input.rest(), &mut buf),
        _ => input.rest(),
    };
    check_header(record);
    check_ring_decoders(record);
    check_decode::<Connect>(record);
    check_decode::<SendData>(record);
    check_decode::<Close>(record);
    check_decode::<Connected>(record);
    check_decode::<ReceivedData>(record);
    check_decode::<Closed>(record);
    check_decode::<Failed>(record);
}

/// The next record of a sequence: a kind, a connection among four and up
/// to 255 bytes of payload from the fuzzer, behind the kind's header. Data
/// records get a length the payload covers more often than not.
fn next_record(input: &Input<'_>, buf: &mut [u8; FRAME_MAX]) -> usize {
    let kind = input.byte() % 7;
    let mut payload = [0u8; FRAME_MAX];
    let connection = u16::from(input.byte() % 4).to_le_bytes();
    let take = usize::from(input.byte());
    if let Some((head, tail)) = payload.split_at_mut_checked(2) {
        head.copy_from_slice(&connection);
        for byte in tail.iter_mut().take(take) {
            *byte = input.byte();
        }
    }
    if matches!(kind, 1 | 4) && !input.byte().is_multiple_of(4) {
        // A plausible length, and the reserved bytes zero.
        let len = u16::from(input.byte() % 16).saturating_add(1);
        if let Some(field) = payload.get_mut(2..4) {
            field.copy_from_slice(&len.to_le_bytes());
        }
        if let Some(reserved) = payload.get_mut(4..8) {
            reserved.fill(0);
        }
        for byte in payload
            .iter_mut()
            .skip(8_usize.saturating_add(usize::from(len)))
        {
            *byte = 0;
        }
    }
    let record = match kind {
        0 => frame::<Connect>(&payload, buf),
        1 => frame::<SendData>(&payload, buf),
        2 => frame::<Close>(&payload, buf),
        3 => frame::<Connected>(&payload, buf),
        4 => frame::<ReceivedData>(&payload, buf),
        5 => frame::<Closed>(&payload, buf),
        _ => frame::<Failed>(&payload, buf),
    };
    record.len()
}

/// Both tables' contents, in order.
fn contents<'a>(
    connections: impl Iterator<Item = (ConnectionId, ConnectionState)> + 'a,
) -> Vec<(ConnectionId, ConnectionState)> {
    let mut contents: Vec<_> = connections.collect();
    contents.sort_by_key(|(connection, _)| *connection);
    contents
}

/// Records decoded from the fuzzer's bytes, driving both tables.
fn drive_tables(input: &Input<'_>) {
    let mut stack: StackTable<CAPACITY> = StackTable::new();
    let mut client: ClientTable<CAPACITY> = ClientTable::new();
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        let mut buf = [0u8; FRAME_MAX];
        let len = next_record(input, &mut buf);
        let bytes = buf.get(..len).unwrap_or_default();
        check_ring_decoders(bytes);
        let in_step = contents(stack.connections()) == contents(client.connections());
        if let Ok(request) = Request::decode(bytes) {
            let (client_before, stack_before) = (client.clone(), stack.clone());
            let sent = client.send(&request);
            let received = stack.receive(&request);
            for (refusal, unchanged) in [
                (sent.err(), client == client_before),
                (received.err(), stack == stack_before),
            ] {
                if let Some(error) = refusal {
                    assert!(
                        unchanged,
                        "{request:?} refused ({error}) but the table changed"
                    );
                    assert_eq!(error.connection(), request.connection());
                }
            }
            assert!(
                !in_step || sent.is_err() || received.is_ok(),
                "{request:?}: the client may send it, but the stack refused it: {received:?}"
            );
        } else if let Ok(report) = Report::decode(bytes) {
            let (client_before, stack_before) = (client.clone(), stack.clone());
            let sent = stack.send(&report);
            let received = client.receive(&report);
            for (refusal, unchanged) in [
                (sent.err(), stack == stack_before),
                (received.err(), client == client_before),
            ] {
                if let Some(error) = refusal {
                    assert!(
                        unchanged,
                        "{report:?} refused ({error}) but the table changed"
                    );
                    assert_eq!(error.connection(), report.connection());
                }
            }
            assert!(
                !in_step || sent.is_err() || received.is_ok(),
                "{report:?}: the stack may send it, but the client refused it: {received:?}"
            );
        }
        assert!(stack.len() <= CAPACITY && client.len() <= CAPACITY);
    }
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mode = input.byte();
    if mode.is_multiple_of(2) {
        one_record(&input, mode / 2);
    } else {
        drive_tables(&input);
    }
});
