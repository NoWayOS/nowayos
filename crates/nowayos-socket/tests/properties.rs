// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests: every record round-trips with its own kind,
//! version and fixed size; malformed, truncated, wrong-version or wrong-kind
//! input, a data length beyond the bound and non-zero padding are typed
//! errors, never panics; whatever decodes re-encodes to the same bytes; and
//! each side's connection table refuses what no honest peer sends - leaving
//! itself unchanged - while two honest sides, their records crossing in
//! flight in any order, never refuse each other.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::VecDeque;
use std::net::Ipv4Addr;

use nowayos_ipc::{DecodeError, HEADER_LEN, Header, Message, decode, encode, encoded_len};
use nowayos_socket::{
    CLOSE_KIND, CLOSED_KIND, CONNECT_KIND, CONNECTED_KIND, ClientTable, Close, Closed, Connect,
    Connected, ConnectionId, ConnectionState, FAILED_KIND, FailReason, Failed, MAX_DATA,
    ProtocolError, RECEIVED_KIND, RESOLVE_KIND, RESOLVED_KIND, ReceivedData, RecordError, Report,
    Request, SEND_KIND, SLOT_LEN, SendData, StackTable, Verdict, is_unicast_destination,
};
use proptest::prelude::*;

// ---- Records ---------------------------------------------------------------

fn id() -> impl Strategy<Value = ConnectionId> {
    any::<u16>().prop_map(ConnectionId)
}

fn destination() -> impl Strategy<Value = Ipv4Addr> {
    any::<[u8; 4]>()
        .prop_map(Ipv4Addr::from)
        .prop_filter("a unicast destination", |address| {
            is_unicast_destination(*address)
        })
}

fn data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 1..=16),
        prop::collection::vec(any::<u8>(), 1..=MAX_DATA),
        Just(vec![0xff; MAX_DATA]),
    ]
}

fn reason() -> impl Strategy<Value = FailReason> {
    prop::sample::select(FailReason::ALL.to_vec())
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        (id(), destination(), 1..=u16::MAX).prop_map(|(id, address, port)| Request::Connect(
            Connect::new(id, address, port).unwrap()
        )),
        (id(), data()).prop_map(|(id, bytes)| Request::Send(SendData::new(id, &bytes).unwrap())),
        id().prop_map(|id| Request::Close(Close::new(id))),
    ]
}

fn report() -> impl Strategy<Value = Report> {
    prop_oneof![
        id().prop_map(|id| Report::Connected(Connected::new(id))),
        (id(), data())
            .prop_map(|(id, bytes)| Report::Received(ReceivedData::new(id, &bytes).unwrap())),
        id().prop_map(|id| Report::Closed(Closed::new(id))),
        (id(), reason()).prop_map(|(id, reason)| Report::Failed(Failed::new(id, reason))),
    ]
}

fn encoded_request(request: &Request) -> Vec<u8> {
    let mut slot = vec![0u8; SLOT_LEN];
    let len = request.encode(&mut slot).unwrap();
    slot.truncate(len);
    slot
}

fn encoded_report(report: &Report) -> Vec<u8> {
    let mut slot = vec![0u8; SLOT_LEN];
    let len = report.encode(&mut slot).unwrap();
    slot.truncate(len);
    slot
}

/// The record's kind and fixed length, from its type.
fn request_layout(request: &Request) -> (u16, usize) {
    match request {
        Request::Connect(_) => (CONNECT_KIND, encoded_len::<Connect>()),
        Request::Send(_) => (SEND_KIND, encoded_len::<SendData>()),
        Request::Close(_) => (CLOSE_KIND, encoded_len::<Close>()),
    }
}

fn report_layout(report: &Report) -> (u16, usize) {
    match report {
        Report::Connected(_) => (CONNECTED_KIND, encoded_len::<Connected>()),
        Report::Received(_) => (RECEIVED_KIND, encoded_len::<ReceivedData>()),
        Report::Closed(_) => (CLOSED_KIND, encoded_len::<Closed>()),
        Report::Failed(_) => (FAILED_KIND, encoded_len::<Failed>()),
    }
}

/// Whatever decodes from `bytes` must re-encode to exactly the bytes it came
/// from: one encoding per record.
fn canonical_request(bytes: &[u8]) {
    if let Ok(request) = Request::decode(bytes) {
        let again = encoded_request(&request);
        assert_eq!(&again[..], &bytes[..again.len()]);
    }
}

fn canonical_report(bytes: &[u8]) {
    if let Ok(report) = Report::decode(bytes) {
        let again = encoded_report(&report);
        assert_eq!(&again[..], &bytes[..again.len()]);
    }
}

/// The layout version of a kind's records: 2 for the data records, whose
/// slot grew to 4 KiB, 1 for the rest.
fn version_of(kind: u16) -> u16 {
    if kind == SEND_KIND || kind == RECEIVED_KIND {
        2
    } else {
        1
    }
}

/// A record's valid header in front of `payload`.
fn framed(kind: u16, payload_len: usize, payload: &[u8]) -> Vec<u8> {
    let mut record = vec![0u8; HEADER_LEN + payload_len];
    Header {
        kind,
        version: version_of(kind),
        payload_len: payload_len as u16,
    }
    .write(&mut record)
    .unwrap();
    for (to, from) in record[HEADER_LEN..].iter_mut().zip(payload) {
        *to = *from;
    }
    record
}

proptest! {
    #[test]
    fn every_request_round_trips_with_its_kind_version_and_size(request in request()) {
        let bytes = encoded_request(&request);
        let (kind, len) = request_layout(&request);
        let header = Header::parse(&bytes).unwrap();
        prop_assert_eq!((header.kind, header.version), (kind, version_of(kind)));
        prop_assert_eq!(bytes.len(), len);
        prop_assert!(len <= SLOT_LEN);
        prop_assert_eq!(Request::decode_exact(&bytes), Ok(request.clone()));
        // In a ring slot, with zeros after it.
        let mut slot = [0u8; SLOT_LEN];
        slot[..len].copy_from_slice(&bytes);
        prop_assert_eq!(Request::decode(&slot), Ok(request));
    }

    #[test]
    fn every_report_round_trips_with_its_kind_version_and_size(report in report()) {
        let bytes = encoded_report(&report);
        let (kind, len) = report_layout(&report);
        let header = Header::parse(&bytes).unwrap();
        prop_assert_eq!((header.kind, header.version), (kind, version_of(kind)));
        prop_assert_eq!(bytes.len(), len);
        prop_assert!(len <= SLOT_LEN);
        prop_assert_eq!(Report::decode_exact(&bytes), Ok(report.clone()));
        let mut slot = [0u8; SLOT_LEN];
        slot[..len].copy_from_slice(&bytes);
        prop_assert_eq!(Report::decode(&slot), Ok(report));
    }

    #[test]
    fn data_is_the_bytes_given(id in id(), bytes in data()) {
        let sent = SendData::new(id, &bytes).unwrap();
        prop_assert_eq!(sent.bytes(), &bytes[..]);
        let Ok(Request::Send(back)) = Request::decode(&encoded_request(&Request::Send(sent))) else {
            panic!("a send record that does not decode");
        };
        prop_assert_eq!(back.bytes(), &bytes[..]);
        prop_assert_eq!(back.connection(), id);
    }

    #[test]
    fn a_truncated_record_is_a_typed_error(request in request(), report in report(), cut in any::<prop::sample::Index>()) {
        let bytes = encoded_request(&request);
        let result = Request::decode(&bytes[..cut.index(bytes.len())]);
        prop_assert!(
            matches!(result, Err(RecordError::Decode(DecodeError::Truncated { .. }))),
            "{:?}", result
        );
        let bytes = encoded_report(&report);
        let result = Report::decode(&bytes[..cut.index(bytes.len())]);
        prop_assert!(
            matches!(result, Err(RecordError::Decode(DecodeError::Truncated { .. }))),
            "{:?}", result
        );
    }

    #[test]
    fn a_wrong_version_or_header_is_a_typed_error(request in request(), version in 1..=u16::MAX, reserved in 1..=u16::MAX) {
        let expected = version_of(request_layout(&request).0);
        prop_assume!(version != expected);
        let good = encoded_request(&request);
        let mut bad = good.clone();
        bad[2..4].copy_from_slice(&version.to_le_bytes());
        let result = Request::decode(&bad);
        prop_assert_eq!(
            result,
            Err(RecordError::Decode(DecodeError::UnsupportedVersion { expected, found: version }))
        );
        bad = good.clone();
        bad[2..4].copy_from_slice(&0u16.to_le_bytes());
        let result = Request::decode(&bad);
        prop_assert_eq!(
            result,
            Err(RecordError::Decode(DecodeError::UnsupportedVersion { expected, found: 0 }))
        );
        bad = good.clone();
        bad[6..8].copy_from_slice(&reserved.to_le_bytes());
        prop_assert_eq!(Request::decode(&bad), Err(RecordError::Decode(DecodeError::ReservedNotZero)));
        bad = good;
        let declared = bad.len() - HEADER_LEN;
        bad[4..6].copy_from_slice(&((declared + 1) as u16).to_le_bytes());
        let result = Request::decode(&bad);
        prop_assert_eq!(
            result,
            Err(RecordError::Decode(DecodeError::WrongLength { expected: declared, found: declared + 1 }))
        );
    }

    #[test]
    fn a_record_of_the_wrong_kind_is_a_typed_error(request in request(), report in report()) {
        // Each ring's records, offered to the other ring's decoder.
        let bytes = encoded_request(&request);
        let (kind, _) = request_layout(&request);
        prop_assert_eq!(Report::decode(&bytes), Err(RecordError::UnknownKind { kind }));
        let bytes = encoded_report(&report);
        let (kind, _) = report_layout(&report);
        prop_assert_eq!(Request::decode(&bytes), Err(RecordError::UnknownKind { kind }));
        // A record decoded as one of another kind.
        let connect = encoded_request(&Request::Connect(
            Connect::new(ConnectionId(1), Ipv4Addr::new(10, 0, 2, 2), 443).unwrap(),
        ));
        prop_assert_eq!(
            decode::<Close>(&connect),
            Err(DecodeError::WrongKind { expected: CLOSE_KIND, found: CONNECT_KIND })
        );
    }

    #[test]
    fn a_reserved_or_unknown_kind_is_refused(kind in any::<u16>(), payload in prop::collection::vec(any::<u8>(), 0..64)) {
        let record = framed(kind, payload.len(), &payload);
        let requests = [CONNECT_KIND, SEND_KIND, CLOSE_KIND];
        let reports = [CONNECTED_KIND, RECEIVED_KIND, CLOSED_KIND, FAILED_KIND];
        if !requests.contains(&kind) {
            prop_assert_eq!(Request::decode(&record), Err(RecordError::UnknownKind { kind }));
        }
        if !reports.contains(&kind) {
            prop_assert_eq!(Report::decode(&record), Err(RecordError::UnknownKind { kind }));
        }
        for reserved in [RESOLVE_KIND, RESOLVED_KIND] {
            let record = framed(reserved, payload.len(), &payload);
            prop_assert_eq!(Request::decode(&record), Err(RecordError::UnknownKind { kind: reserved }));
            prop_assert_eq!(Report::decode(&record), Err(RecordError::UnknownKind { kind: reserved }));
        }
    }

    #[test]
    fn a_data_length_beyond_the_bound_or_zero_is_refused(id in id(), bytes in data(), len in prop_oneof![Just(0u16), (MAX_DATA as u16 + 1)..=u16::MAX]) {
        let mut record = encoded_request(&Request::Send(SendData::new(id, &bytes).unwrap()));
        record[10..12].copy_from_slice(&len.to_le_bytes());
        prop_assert_eq!(
            Request::decode(&record),
            Err(RecordError::Decode(DecodeError::InvalidValue { field: "len" }))
        );
        let mut record = encoded_report(&Report::Received(ReceivedData::new(id, &bytes).unwrap()));
        record[10..12].copy_from_slice(&len.to_le_bytes());
        prop_assert_eq!(
            Report::decode(&record),
            Err(RecordError::Decode(DecodeError::InvalidValue { field: "len" }))
        );
    }

    #[test]
    fn non_zero_padding_is_refused(id in id(), bytes in prop::collection::vec(any::<u8>(), 1..MAX_DATA), at in any::<prop::sample::Index>(), value in 1..=u8::MAX) {
        // Any byte past the data's length.
        let mut record = encoded_request(&Request::Send(SendData::new(id, &bytes).unwrap()));
        let start = HEADER_LEN + 8 + bytes.len();
        let position = start + at.index(SLOT_LEN - start);
        record[position] = value;
        prop_assert_eq!(Request::decode(&record), Err(RecordError::Decode(DecodeError::NonZeroPadding)));
        // Any of a data record's four reserved bytes.
        let mut record = encoded_report(&Report::Received(ReceivedData::new(id, &bytes).unwrap()));
        record[HEADER_LEN + 4 + at.index(4)] = value;
        prop_assert_eq!(Report::decode(&record), Err(RecordError::Decode(DecodeError::NonZeroPadding)));
        // The reserved bytes of the short records.
        let mut record = encoded_report(&Report::Connected(Connected::new(id)));
        record[HEADER_LEN + 2 + at.index(2)] = value;
        prop_assert_eq!(Report::decode(&record), Err(RecordError::Decode(DecodeError::NonZeroPadding)));
        let mut record = encoded_report(&Report::Failed(Failed::new(id, FailReason::Refused)));
        record[HEADER_LEN + 3] = value;
        prop_assert_eq!(Report::decode(&record), Err(RecordError::Decode(DecodeError::NonZeroPadding)));
    }

    #[test]
    fn any_bytes_decode_to_a_record_or_a_typed_error(bytes in prop::collection::vec(any::<u8>(), 0..SLOT_LEN + 8)) {
        // Nothing panics, and whatever decodes is canonical.
        canonical_request(&bytes);
        canonical_report(&bytes);
        let _ = Request::decode_exact(&bytes);
        let _ = Report::decode_exact(&bytes);
    }

    #[test]
    fn any_payload_behind_a_valid_header_is_a_record_or_a_typed_error(
        kind in prop::sample::select(vec![CONNECT_KIND, SEND_KIND, CLOSE_KIND, CONNECTED_KIND, RECEIVED_KIND, CLOSED_KIND, FAILED_KIND]),
        payload in prop::collection::vec(any::<u8>(), 0..SLOT_LEN),
        zero_tail in any::<bool>(),
    ) {
        let payload_len = match kind {
            CONNECT_KIND => Connect::PAYLOAD_LEN,
            SEND_KIND | RECEIVED_KIND => SendData::PAYLOAD_LEN,
            CLOSE_KIND | CONNECTED_KIND | CLOSED_KIND => Close::PAYLOAD_LEN,
            _ => Failed::PAYLOAD_LEN,
        };
        let mut payload = payload;
        if zero_tail && payload.len() >= 8 {
            // A short length, zero reserved bytes and zeros past the data,
            // so that data records often decode.
            let len = usize::from(payload[2] % 32) + 1;
            payload[2..4].copy_from_slice(&(len as u16).to_le_bytes());
            payload[4..8].fill(0);
            payload.iter_mut().skip(8 + len).for_each(|byte| *byte = 0);
        }
        let record = framed(kind, payload_len, &payload);
        canonical_request(&record);
        canonical_report(&record);
    }
}

#[test]
fn a_zero_payload_data_record_is_refused_but_a_full_one_decodes() {
    let empty = framed(SEND_KIND, SendData::PAYLOAD_LEN, &[]);
    assert_eq!(
        Request::decode(&empty),
        Err(RecordError::Decode(DecodeError::InvalidValue {
            field: "len"
        }))
    );
    let mut payload = vec![0u8; SendData::PAYLOAD_LEN];
    payload[2..4].copy_from_slice(&(MAX_DATA as u16).to_le_bytes());
    payload[8..].fill(0xee);
    let full = framed(RECEIVED_KIND, SendData::PAYLOAD_LEN, &payload);
    let Ok(Report::Received(data)) = Report::decode(&full) else {
        panic!("a full data record does not decode");
    };
    assert_eq!(data.bytes(), &[0xee; MAX_DATA][..]);
    let mut out = [0u8; SLOT_LEN];
    assert_eq!(encode(&data, &mut out), Ok(SLOT_LEN));
}

#[test]
fn padding_is_checked_from_the_byte_after_the_data_at_every_word_boundary() {
    let ends = [MAX_DATA - 17, MAX_DATA - 16, MAX_DATA - 1, MAX_DATA];
    for len in [1, 15, 16, 17, 31, 32, 33].into_iter().chain(ends) {
        let mut payload = vec![0u8; SendData::PAYLOAD_LEN];
        payload[2..4].copy_from_slice(&(len as u16).to_le_bytes());
        payload[8..8 + len].fill(0xff);
        let record = framed(SEND_KIND, SendData::PAYLOAD_LEN, &payload);
        let Ok(Request::Send(data)) = Request::decode(&record) else {
            panic!("{len} bytes with zeros after them do not decode");
        };
        assert_eq!(data.bytes(), &vec![0xff; len][..]);
        if len < MAX_DATA {
            payload[8 + len] = 1;
            let record = framed(SEND_KIND, SendData::PAYLOAD_LEN, &payload);
            assert_eq!(
                Request::decode(&record),
                Err(RecordError::Decode(DecodeError::NonZeroPadding)),
                "a byte set just after {len} bytes"
            );
        }
    }
}

// ---- Connection tables -------------------------------------------------------

/// A small space of names, so records often name open connections.
fn small_id() -> impl Strategy<Value = ConnectionId> {
    (0u16..4).prop_map(ConnectionId)
}

/// The records a table step can carry, with any name.
#[derive(Clone, Debug)]
enum Step {
    Connect(ConnectionId),
    Send(ConnectionId),
    Close(ConnectionId),
    Connected(ConnectionId),
    Received(ConnectionId),
    Closed(ConnectionId),
    Failed(ConnectionId),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        small_id().prop_map(Step::Connect),
        small_id().prop_map(Step::Send),
        small_id().prop_map(Step::Close),
        small_id().prop_map(Step::Connected),
        small_id().prop_map(Step::Received),
        small_id().prop_map(Step::Closed),
        small_id().prop_map(Step::Failed),
    ]
}

fn as_request(step: &Step) -> Option<Request> {
    Some(match *step {
        Step::Connect(id) => {
            Request::Connect(Connect::new(id, Ipv4Addr::new(192, 0, 2, 1), 443).unwrap())
        }
        Step::Send(id) => Request::Send(SendData::new(id, b"data").unwrap()),
        Step::Close(id) => Request::Close(Close::new(id)),
        _ => return None,
    })
}

fn as_report(step: &Step) -> Option<Report> {
    Some(match *step {
        Step::Connected(id) => Report::Connected(Connected::new(id)),
        Step::Received(id) => Report::Received(ReceivedData::new(id, b"data").unwrap()),
        Step::Closed(id) => Report::Closed(Closed::new(id)),
        Step::Failed(id) => Report::Failed(Failed::new(id, FailReason::TimedOut)),
        _ => return None,
    })
}

/// The model both tables must agree with: the documented rules, written
/// out as a decision for each record. Returns the error, or the verdict and
/// the connection's state after the record (`None`: its name is free).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    /// The stack receiving requests, and checking the reports it sends.
    Stack,
    /// The client receiving reports, and checking the requests it sends.
    Client,
}

type Model = Vec<(ConnectionId, ConnectionState)>;

fn model_step(
    model: &Model,
    capacity: usize,
    side: Side,
    step: &Step,
) -> Result<(Verdict, Model), ProtocolError> {
    use ProtocolError as E;
    let mut next = model.clone();
    let find = |id: ConnectionId| model.iter().position(|(open, _)| *open == id);
    let ended = |state: &ConnectionState| state.client_closed && state.stack_ended;
    let (id, verdict) = match *step {
        Step::Connect(id) => {
            if find(id).is_some() {
                return Err(E::AlreadyOpen { connection: id });
            }
            if model.len() >= capacity {
                return Err(E::TableFull {
                    connection: id,
                    capacity,
                });
            }
            next.push((id, ConnectionState::default()));
            return Ok((Verdict::Act, next));
        }
        Step::Send(id) | Step::Received(id) => {
            let at = find(id).ok_or(E::NotOpen { connection: id })?;
            let state = model[at].1;
            let incoming = matches!(
                (side, step),
                (Side::Stack, Step::Send(_)) | (Side::Client, Step::Received(_))
            );
            // The sender's own end, and what it has seen, rule data out; the
            // receiver's own end only makes data in flight pointless.
            let (sender_ended, receiver_ended) = match step {
                Step::Send(_) => (state.client_closed, state.stack_ended),
                _ => (state.stack_ended, state.client_closed),
            };
            if incoming {
                if sender_ended {
                    return Err(E::DataAfterClose { connection: id });
                }
                if !state.connected {
                    return Err(E::DataBeforeConnected { connection: id });
                }
                let verdict = if receiver_ended {
                    Verdict::Ignore
                } else {
                    Verdict::Act
                };
                return Ok((verdict, next));
            }
            if sender_ended || receiver_ended {
                return Err(E::DataAfterClose { connection: id });
            }
            if !state.connected {
                return Err(E::DataBeforeConnected { connection: id });
            }
            return Ok((Verdict::Act, next));
        }
        Step::Connected(id) => {
            let at = find(id).ok_or(E::NotOpen { connection: id })?;
            let state = &mut next[at].1;
            if state.connected {
                return Err(E::AlreadyConnected { connection: id });
            }
            let refused = match side {
                // The stack never answers a client's close with Connected.
                Side::Stack => state.stack_ended || state.client_closed,
                Side::Client => state.stack_ended,
            };
            if refused {
                return Err(E::ConnectedAfterClose { connection: id });
            }
            state.connected = true;
            let verdict = if side == Side::Client && state.client_closed {
                Verdict::Ignore
            } else {
                Verdict::Act
            };
            (id, verdict)
        }
        Step::Close(id) => {
            let at = find(id).ok_or(E::NotOpen { connection: id })?;
            let state = &mut next[at].1;
            if state.client_closed {
                return Err(E::AlreadyClosed { connection: id });
            }
            state.client_closed = true;
            (id, Verdict::Act)
        }
        Step::Closed(id) | Step::Failed(id) => {
            let at = find(id).ok_or(E::NotOpen { connection: id })?;
            let state = &mut next[at].1;
            if state.stack_ended {
                return Err(E::AlreadyClosed { connection: id });
            }
            state.stack_ended = true;
            (id, Verdict::Act)
        }
    };
    next.retain(|(open, state)| *open != id || !ended(state));
    Ok((verdict, next))
}

fn sorted(mut model: Model) -> Model {
    model.sort_by_key(|(id, _)| *id);
    model
}

fn stack_model(table: &StackTable<2>) -> Model {
    sorted(table.connections().collect())
}

fn client_model(table: &ClientTable<2>) -> Model {
    sorted(table.connections().collect())
}

proptest! {
    #[test]
    fn the_stack_table_follows_the_rules_and_refusals_change_nothing(steps in prop::collection::vec(step(), 0..64)) {
        let mut table: StackTable<2> = StackTable::new();
        let mut model = Model::new();
        for step in &steps {
            let before = table.clone();
            let (actual, expected) = if let Some(request) = as_request(step) {
                (table.receive(&request), model_step(&model, 2, Side::Stack, step))
            } else {
                let report = as_report(step).unwrap();
                (table.send(&report).map(|()| Verdict::Act), model_step(&model, 2, Side::Stack, step))
            };
            match (actual, expected) {
                (Ok(verdict), Ok((expected_verdict, next))) => {
                    prop_assert_eq!(verdict, expected_verdict, "{:?}", step);
                    model = sorted(next);
                }
                (Err(error), Err(expected_error)) => {
                    prop_assert_eq!(error, expected_error, "{:?}", step);
                    prop_assert_eq!(&table, &before, "a refusal changed the table");
                }
                (actual, expected) => prop_assert!(false, "{step:?}: {actual:?}, expected {expected:?}"),
            }
            prop_assert_eq!(stack_model(&table), model.clone());
            prop_assert!(table.len() <= table.capacity());
        }
    }

    #[test]
    fn the_client_table_follows_the_rules_and_refusals_change_nothing(steps in prop::collection::vec(step(), 0..64)) {
        let mut table: ClientTable<2> = ClientTable::new();
        let mut model = Model::new();
        for step in &steps {
            let before = table.clone();
            let actual = if let Some(report) = as_report(step) {
                table.receive(&report)
            } else {
                table.send(&as_request(step).unwrap()).map(|()| Verdict::Act)
            };
            match (actual, model_step(&model, 2, Side::Client, step)) {
                (Ok(verdict), Ok((expected_verdict, next))) => {
                    prop_assert_eq!(verdict, expected_verdict, "{:?}", step);
                    model = sorted(next);
                }
                (Err(error), Err(expected_error)) => {
                    prop_assert_eq!(error, expected_error, "{:?}", step);
                    prop_assert_eq!(&table, &before, "a refusal changed the table");
                }
                (actual, expected) => prop_assert!(false, "{step:?}: {actual:?}, expected {expected:?}"),
            }
            prop_assert_eq!(client_model(&table), model.clone());
        }
    }

    #[test]
    fn the_four_refusals_the_story_names(steps in prop::collection::vec(step(), 0..64)) {
        // Whatever came before: a record naming a connection not open, data
        // before Connected or after its sender's end, and a Connect beyond
        // the capacity are each refused, and change nothing.
        let mut stack: StackTable<2> = StackTable::new();
        for step in &steps {
            let before = stack.clone();
            let result = match as_request(step) {
                Some(request) => stack.receive(&request).map(|_| ()),
                None => stack.send(&as_report(step).unwrap()),
            };
            let id = match step {
                Step::Connect(id) | Step::Send(id) | Step::Close(id) | Step::Connected(id)
                | Step::Received(id) | Step::Closed(id) | Step::Failed(id) => *id,
            };
            let state = before.state(id);
            match (step, state) {
                (Step::Connect(_), None) if before.len() == 2 => {
                    prop_assert_eq!(result, Err(ProtocolError::TableFull { connection: id, capacity: 2 }));
                }
                (Step::Connect(_), _) => {}
                (_, None) => prop_assert_eq!(result, Err(ProtocolError::NotOpen { connection: id })),
                (Step::Send(_), Some(state)) if state.client_closed => {
                    prop_assert_eq!(result, Err(ProtocolError::DataAfterClose { connection: id }));
                }
                (Step::Send(_), Some(state)) if !state.connected => {
                    prop_assert_eq!(result, Err(ProtocolError::DataBeforeConnected { connection: id }));
                }
                _ => {}
            }
            if result.is_err() {
                prop_assert_eq!(&stack, &before);
            }
        }
    }
}

/// One side of an honest pair: a PD that sends a record only when its own
/// table allows it.
#[derive(Clone, Debug)]
enum Action {
    /// The client tries to send this request.
    Client(Step),
    /// The stack tries to send this report.
    Stack(Step),
    /// The oldest request in flight reaches the stack.
    DeliverRequest,
    /// The oldest report in flight reaches the client.
    DeliverReport,
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        3 => prop_oneof![
            small_id().prop_map(Step::Connect),
            small_id().prop_map(Step::Send),
            small_id().prop_map(Step::Close),
        ].prop_map(Action::Client),
        3 => prop_oneof![
            small_id().prop_map(Step::Connected),
            small_id().prop_map(Step::Received),
            small_id().prop_map(Step::Closed),
            small_id().prop_map(Step::Failed),
        ].prop_map(Action::Stack),
        2 => Just(Action::DeliverRequest),
        2 => Just(Action::DeliverReport),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn honest_sides_never_refuse_each_other(actions in prop::collection::vec(action(), 0..200)) {
        let mut client: ClientTable<2> = ClientTable::new();
        let mut stack: StackTable<2> = StackTable::new();
        let mut requests: VecDeque<Request> = VecDeque::new();
        let mut reports: VecDeque<Report> = VecDeque::new();
        for action in &actions {
            match action {
                Action::Client(step) => {
                    let request = as_request(step).unwrap();
                    if client.send(&request).is_ok() {
                        requests.push_back(request);
                    }
                }
                Action::Stack(step) => {
                    let report = as_report(step).unwrap();
                    if stack.send(&report).is_ok() {
                        reports.push_back(report);
                    }
                }
                Action::DeliverRequest => {
                    if let Some(request) = requests.pop_front() {
                        let received = stack.receive(&request);
                        prop_assert!(received.is_ok(), "{:?} refused: {:?}", request, received);
                    }
                }
                Action::DeliverReport => {
                    if let Some(report) = reports.pop_front() {
                        let received = client.receive(&report);
                        prop_assert!(received.is_ok(), "{:?} refused: {:?}", report, received);
                    }
                }
            }
        }
        // Once everything in flight has arrived, both tables hold the same
        // connections in the same states.
        while let Some(request) = requests.pop_front() {
            prop_assert!(stack.receive(&request).is_ok());
        }
        while let Some(report) = reports.pop_front() {
            prop_assert!(client.receive(&report).is_ok());
        }
        prop_assert_eq!(client_model(&client), stack_model(&stack));
    }
}
