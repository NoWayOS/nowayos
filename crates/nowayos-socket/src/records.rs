// SPDX-License-Identifier: MIT OR Apache-2.0
//! The records: their kinds, wire layouts and checks.

use core::fmt;
use core::net::Ipv4Addr;
use core::num::NonZeroU16;

use nowayos_ipc::{
    DecodeError, EncodeError, Header, Message, Reader, Writer, decode, encode, encoded_len,
};

/// Kind of [`Connect`], on the requests ring.
pub const CONNECT_KIND: u16 = 0x0501;
/// Kind of [`SendData`], on the requests ring.
pub const SEND_KIND: u16 = 0x0502;
/// Kind of [`Close`], on the requests ring.
pub const CLOSE_KIND: u16 = 0x0503;
/// Reserved on the requests ring for a name lookup, which the stack will
/// answer with [`RESOLVED_KIND`] (the stack resolves names;
/// whether a product may use names is its own decision). No record has this
/// kind yet, so a decoder refuses it as an unknown kind.
pub const RESOLVE_KIND: u16 = 0x0504;
/// Kind of [`Connected`], on the reports ring.
pub const CONNECTED_KIND: u16 = 0x0581;
/// Kind of [`ReceivedData`], on the reports ring.
pub const RECEIVED_KIND: u16 = 0x0582;
/// Kind of [`Closed`], on the reports ring.
pub const CLOSED_KIND: u16 = 0x0583;
/// Kind of [`Failed`], on the reports ring.
pub const FAILED_KIND: u16 = 0x0584;
/// Reserved on the reports ring for a name lookup's answer: see
/// [`RESOLVE_KIND`].
pub const RESOLVED_KIND: u16 = 0x0585;

/// The layout version of the records that only name a connection, and of
/// [`Connect`] and [`Failed`].
const VERSION: u16 = 1;

/// The layout version of [`SendData`] and [`ReceivedData`]: 2 since they
/// carry up to [`MAX_DATA`] = 976 bytes in a 992-byte slot;
/// version 1 carried 240 in a 256-byte slot.
const DATA_VERSION: u16 = 2;

/// The most stream bytes one [`SendData`] or [`ReceivedData`] carries: the
/// one data bound. A record carries from 1 to this many.
pub const MAX_DATA: usize = 976;

/// The bytes of a data record's payload before its data: the connection,
/// the length and four reserved bytes.
const DATA_PREFIX: usize = 8;

/// The longest record, [`SendData`] and [`ReceivedData`]: a ring slot this
/// long holds every record here, header included.
pub const SLOT_LEN: usize = encoded_len::<SendData>();

/// The slots in each of a client's two rings with the stack: 32 [`SLOT_LEN`]-byte slots, about 31 KB of stream bytes a ring.
pub const RING_SLOTS: usize = 32;

/// The bytes of each ring's messages region, eight pages: the producer's
/// index, [`RING_SLOTS`] slots of [`SLOT_LEN`] and its restart epoch
/// (`nowayos-region`'s ring layout) take 31,752 of them. A system gives
/// both rings' messages regions this size, and each control a page.
pub const RING_REGION_LEN: usize = 0x8000;

// The data records are the longest, and fill a 992-byte slot exactly;
// every other record fits in it; and a ring's index, slots and epoch fit
// its messages region. A slot, and one byte more, stays within the 1,000
// bytes up to which Kani's solver flattens an array: past that its proofs
// of the decoders ran out of memory (4 KiB slots).
const _: () = {
    assert!(SLOT_LEN == 992);
    assert!(4 + RING_SLOTS * SLOT_LEN + 4 <= RING_REGION_LEN);
    assert!(encoded_len::<ReceivedData>() == SLOT_LEN);
    assert!(encoded_len::<Connect>() <= SLOT_LEN);
    assert!(encoded_len::<Close>() <= SLOT_LEN);
    assert!(encoded_len::<Failed>() <= SLOT_LEN);
    assert!(MAX_DATA <= u16::MAX as usize);
};

/// A connection's name, chosen by the client in its [`Connect`] and used by
/// every record about the connection afterwards. Names are per ring pair:
/// two clients may use the same name for two different connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(pub u16);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connection {}", self.0)
    }
}

/// Whether `address` can be a TCP destination: a unicast address, not in
/// 0.0.0.0/8 ("this network"), 127.0.0.0/8 (loopback: the stack has none),
/// 224.0.0.0/4 (multicast) or 240.0.0.0/4 (reserved, and the broadcast
/// address).
#[must_use]
pub const fn is_unicast_destination(address: Ipv4Addr) -> bool {
    let [first, _, _, _] = address.octets();
    !(first == 0 || first == 127 || first >= 224)
}

/// Why a record was refused before any table saw it. Decoding never panics
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordError {
    /// The record is malformed: truncated, a reserved header field set, the
    /// wrong version or payload length, a field out of range or padding
    /// that is not zero (the [`DecodeError`] says which).
    Decode(DecodeError),
    /// A well-formed header of a kind this ring does not carry: the other
    /// ring's, a reserved one, or any other.
    UnknownKind {
        /// The kind in the record's header.
        kind: u16,
    },
}

impl From<DecodeError> for RecordError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "malformed socket record: {error}"),
            Self::UnknownKind { kind } => {
                write!(
                    f,
                    "a record of kind {kind:#06x}, which this ring does not carry"
                )
            }
        }
    }
}

impl core::error::Error for RecordError {}

/// Client → stack: open a TCP connection to `address`, port `port`, and
/// call it `connection`.
///
/// Wire layout, 8 bytes, little-endian:
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–1 | connection (`u16`) | any |
/// | 2–3 | port (`u16`) | not 0 |
/// | 4–7 | IPv4 address, most significant octet first | [`is_unicast_destination`] |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connect {
    connection: ConnectionId,
    address: Ipv4Addr,
    port: NonZeroU16,
}

impl Connect {
    /// A request to connect to `address`, port `port`, as `connection`; `None`
    /// if the port is 0 or the address cannot be a TCP destination
    /// ([`is_unicast_destination`]).
    #[must_use]
    pub const fn new(connection: ConnectionId, address: Ipv4Addr, port: u16) -> Option<Self> {
        let Some(port) = NonZeroU16::new(port) else {
            return None;
        };
        if !is_unicast_destination(address) {
            return None;
        }
        Some(Self {
            connection,
            address,
            port,
        })
    }

    /// The connection it opens.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// The address to connect to.
    #[must_use]
    pub const fn address(&self) -> Ipv4Addr {
        self.address
    }

    /// The port to connect to: never 0.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port.get()
    }
}

impl Message for Connect {
    const KIND: u16 = CONNECT_KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = 8;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u16(self.connection.0)?;
        out.write_u16(self.port.get())?;
        out.write_bytes(&self.address.octets())
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let connection = ConnectionId(input.read_u16()?);
        let port = NonZeroU16::new(input.read_u16()?)
            .ok_or(DecodeError::InvalidValue { field: "port" })?;
        let address = Ipv4Addr::from(input.read_array::<4>()?);
        if !is_unicast_destination(address) {
            return Err(DecodeError::InvalidValue { field: "address" });
        }
        Ok(Self {
            connection,
            address,
            port,
        })
    }
}

/// Stream bytes on a connection, from 1 to [`MAX_DATA`] of them: the
/// payload of [`SendData`] (client → stack, kind [`SEND_KIND`]) and of
/// [`ReceivedData`] (stack → client, kind [`RECEIVED_KIND`]).
///
/// The bytes are a stream, not a message: a boundary between two records
/// means nothing, and the TLS PD frames its own records.
///
/// Wire layout, version 2, 8 + [`MAX_DATA`] = 984 bytes, little-endian:
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–1 | connection (`u16`) | any |
/// | 2–3 | length (`u16`) | 1 to [`MAX_DATA`] |
/// | 4–7 | reserved | zero |
/// | 8–983 | the bytes, then zeros | every byte past the length zero |
#[derive(Clone, PartialEq, Eq)]
pub struct Data<const KIND: u16> {
    connection: ConnectionId,
    /// From 1 to `MAX_DATA`.
    len: usize,
    /// The bytes, then zeros: so a record has one encoding.
    bytes: [u8; MAX_DATA],
}

/// Client → stack: send these bytes on the connection.
pub type SendData = Data<SEND_KIND>;

/// Stack → client: these bytes arrived on the connection.
pub type ReceivedData = Data<RECEIVED_KIND>;

impl<const KIND: u16> Data<KIND> {
    /// A record of `bytes` on `connection`; `None` if there are none or more
    /// than [`MAX_DATA`].
    #[must_use]
    pub fn new(connection: ConnectionId, bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        let mut data = Self {
            connection,
            len: bytes.len(),
            bytes: [0; MAX_DATA],
        };
        data.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(data)
    }

    /// The connection the bytes belong to.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// The bytes: from 1 to [`MAX_DATA`] of them.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    /// How many bytes it carries: from 1 to [`MAX_DATA`].
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it carries no bytes: never, as [`Data::new`] and the decoder
    /// refuse that.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Names the connection and the length, never the bytes: they are a
/// connection's contents.
impl<const KIND: u16> fmt::Debug for Data<KIND> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Data")
            .field("kind", &KIND)
            .field("connection", &self.connection)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl<const KIND: u16> Message for Data<KIND> {
    const KIND: u16 = KIND;
    const VERSION: u16 = DATA_VERSION;
    const PAYLOAD_LEN: usize = DATA_PREFIX + MAX_DATA;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let len = u16::try_from(self.len)
            .ok()
            .filter(|len| (1..=MAX_DATA).contains(&usize::from(*len)))
            .ok_or(EncodeError::InvalidValue { field: "len" })?;
        out.write_u16(self.connection.0)?;
        out.write_u16(len)?;
        out.write_zeros(4)?;
        out.write_bytes(&self.bytes)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let connection = ConnectionId(input.read_u16()?);
        let len = usize::from(input.read_u16()?);
        input.read_zeros(4)?;
        let bytes: [u8; MAX_DATA] = input.read_array()?;
        if len == 0 || len > MAX_DATA {
            return Err(DecodeError::InvalidValue { field: "len" });
        }
        if !zero_from(&bytes, len) {
            return Err(DecodeError::NonZeroPadding);
        }
        Ok(Self {
            connection,
            len,
            bytes,
        })
    }
}

/// The bytes of a data area checked at a time: [`zero_from`] reads the area
/// as sixteen-byte words, so that its loop runs [`MAX_DATA`] / 16 times
/// whatever the length, and shifts once - few enough steps for Kani to
/// prove the decoder whole.
const WORD: usize = 16;

const _: () = assert!(MAX_DATA.is_multiple_of(WORD), "every byte is in a word");

/// Whether every byte of `bytes` from `len` on is zero: the data area's
/// padding.
fn zero_from(bytes: &[u8; MAX_DATA], len: usize) -> bool {
    // The word `len` falls in, and how many of its bytes are data: the low
    // bytes of its little-endian value, so its padding is the bits above.
    let (Some(first), Some(data)) = (len.checked_div(WORD), len.checked_rem(WORD)) else {
        return false;
    };
    let Some(padding) = u32::try_from(data.saturating_mul(8))
        .ok()
        .and_then(|bits| u128::MAX.checked_shl(bits))
    else {
        return false;
    };
    bytes.chunks_exact(WORD).enumerate().all(|(index, chunk)| {
        let Ok(word) = <[u8; WORD]>::try_from(chunk) else {
            return false;
        };
        let mask = match index.cmp(&first) {
            core::cmp::Ordering::Less => 0,
            core::cmp::Ordering::Equal => padding,
            core::cmp::Ordering::Greater => u128::MAX,
        };
        u128::from_le_bytes(word) & mask == 0
    })
}

/// A record that only names its connection: [`Close`] (client → stack),
/// [`Connected`] and [`Closed`] (stack → client).
///
/// Wire layout, 4 bytes, little-endian:
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–1 | connection (`u16`) | any |
/// | 2–3 | reserved | zero |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionRecord<const KIND: u16> {
    connection: ConnectionId,
}

/// Client → stack: the client is done with the connection. It sends nothing
/// more for it; the stack closes the connection, if it is still open, and
/// ends it with [`Closed`].
pub type Close = ConnectionRecord<CLOSE_KIND>;

/// Stack → client: the connection is open; data may flow.
pub type Connected = ConnectionRecord<CONNECTED_KIND>;

/// Stack → client: the connection is over, cleanly - the peer closed it
/// after the last [`ReceivedData`], or the client's [`Close`] is done. The
/// stack reports nothing more for it.
pub type Closed = ConnectionRecord<CLOSED_KIND>;

impl<const KIND: u16> ConnectionRecord<KIND> {
    /// The record for `connection`.
    #[must_use]
    pub const fn new(connection: ConnectionId) -> Self {
        Self { connection }
    }

    /// The connection it names.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }
}

impl<const KIND: u16> Message for ConnectionRecord<KIND> {
    const KIND: u16 = KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = 4;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u16(self.connection.0)?;
        out.write_zeros(2)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let connection = ConnectionId(input.read_u16()?);
        input.read_zeros(2)?;
        Ok(Self { connection })
    }
}

/// Why a connection failed, as [`Failed`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailReason {
    /// The host refused the connection: a reset answered the stack's
    /// request to connect.
    Refused,
    /// The host reset the connection after it was open.
    Reset,
    /// The host stopped answering: no answer to the request to connect, or
    /// data never acknowledged, within the stack's time limits.
    TimedOut,
    /// The stack has no route to the address: no address of its own yet,
    /// or no gateway towards it.
    Unreachable,
    /// The stack had no connection or buffer free for it.
    NoResources,
    /// The stack does not connect to this address or port: the system
    /// allows only the hosts it is configured for.
    NotAllowed,
}

impl FailReason {
    /// Every reason, in wire order.
    pub const ALL: [Self; 6] = [
        Self::Refused,
        Self::Reset,
        Self::TimedOut,
        Self::Unreachable,
        Self::NoResources,
        Self::NotAllowed,
    ];

    /// The reason's code on the wire: 1 to 6.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Refused => 1,
            Self::Reset => 2,
            Self::TimedOut => 3,
            Self::Unreachable => 4,
            Self::NoResources => 5,
            Self::NotAllowed => 6,
        }
    }

    /// The reason a code stands for; `None` for 0 and anything above 6.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Refused),
            2 => Some(Self::Reset),
            3 => Some(Self::TimedOut),
            4 => Some(Self::Unreachable),
            5 => Some(Self::NoResources),
            6 => Some(Self::NotAllowed),
            _ => None,
        }
    }
}

impl fmt::Display for FailReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Refused => "refused by the host",
            Self::Reset => "reset by the host",
            Self::TimedOut => "timed out",
            Self::Unreachable => "no route to the host",
            Self::NoResources => "no connection or buffer free in the stack",
            Self::NotAllowed => "not a host this system may reach",
        })
    }
}

/// Stack → client: the connection is over, and why. The stack reports
/// nothing more for it.
///
/// Wire layout, 4 bytes, little-endian:
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–1 | connection (`u16`) | any |
/// | 2 | reason | 1 to 6: [`FailReason::code`] |
/// | 3 | reserved | zero |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    connection: ConnectionId,
    reason: FailReason,
}

impl Failed {
    /// The record that `connection` failed for `reason`.
    #[must_use]
    pub const fn new(connection: ConnectionId, reason: FailReason) -> Self {
        Self { connection, reason }
    }

    /// The connection that failed.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Why.
    #[must_use]
    pub const fn reason(&self) -> FailReason {
        self.reason
    }
}

impl Message for Failed {
    const KIND: u16 = FAILED_KIND;
    const VERSION: u16 = VERSION;
    const PAYLOAD_LEN: usize = 4;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u16(self.connection.0)?;
        out.write_u8(self.reason.code())?;
        out.write_zeros(1)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let connection = ConnectionId(input.read_u16()?);
        let reason = FailReason::from_code(input.read_u8()?)
            .ok_or(DecodeError::InvalidValue { field: "reason" })?;
        input.read_zeros(1)?;
        Ok(Self { connection, reason })
    }
}

/// `Ok` if `bytes` is exactly a record of `len` bytes, which decoded from
/// its start; `TrailingBytes` if anything follows it.
fn exactly(bytes: &[u8], len: usize) -> Result<(), RecordError> {
    match bytes.len().saturating_sub(len) {
        0 => Ok(()),
        unread => Err(RecordError::Decode(DecodeError::TrailingBytes { unread })),
    }
}

/// A record on the requests ring, client → stack.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "records are fixed-size values copied out of ring slots, in PDs with no heap to box them in"
)]
pub enum Request {
    /// Open a connection.
    Connect(Connect),
    /// Send bytes on it.
    Send(SendData),
    /// The client is done with it.
    Close(Close),
}

impl Request {
    /// Decodes the request at the start of `bytes` - a ring slot, copied
    /// out - whichever kind it is. Bytes after the record are ignored, as
    /// `nowayos_ipc::decode` ignores a slot's unused tail.
    ///
    /// # Errors
    ///
    /// [`RecordError::UnknownKind`] for a well-formed header of any kind but
    /// the three requests; otherwise [`RecordError::Decode`], as
    /// `nowayos_ipc::decode` and each record's checks report it.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        match Header::parse(bytes)?.kind {
            CONNECT_KIND => Ok(Self::Connect(decode(bytes)?)),
            SEND_KIND => Ok(Self::Send(decode(bytes)?)),
            CLOSE_KIND => Ok(Self::Close(decode(bytes)?)),
            kind => Err(RecordError::UnknownKind { kind }),
        }
    }

    /// As [`Request::decode`], but `bytes` must be exactly one record.
    ///
    /// # Errors
    ///
    /// As [`Request::decode`], and `DecodeError::TrailingBytes` for bytes
    /// after the record.
    pub fn decode_exact(bytes: &[u8]) -> Result<Self, RecordError> {
        let request = Self::decode(bytes)?;
        exactly(bytes, request.encoded_len()).map(|()| request)
    }

    /// The length of its record, header included.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::Connect(_) => encoded_len::<Connect>(),
            Self::Send(_) => encoded_len::<SendData>(),
            Self::Close(_) => encoded_len::<Close>(),
        }
    }

    /// Encodes the request at the start of `out`, returning the record's
    /// length.
    ///
    /// # Errors
    ///
    /// As `nowayos_ipc::encode`: `out` too short for the record.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        match self {
            Self::Connect(record) => encode(record, out),
            Self::Send(record) => encode(record, out),
            Self::Close(record) => encode(record, out),
        }
    }

    /// The connection it names.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::Connect(record) => record.connection(),
            Self::Send(record) => record.connection(),
            Self::Close(record) => record.connection(),
        }
    }
}

/// A record on the reports ring, stack → client.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "records are fixed-size values copied out of ring slots, in PDs with no heap to box them in"
)]
pub enum Report {
    /// The connection is open.
    Connected(Connected),
    /// Bytes arrived on it.
    Received(ReceivedData),
    /// It is over, cleanly.
    Closed(Closed),
    /// It is over, and why.
    Failed(Failed),
}

impl Report {
    /// Decodes the report at the start of `bytes`, whichever kind it is.
    /// Bytes after the record are ignored.
    ///
    /// # Errors
    ///
    /// [`RecordError::UnknownKind`] for a well-formed header of any kind but
    /// the four reports; otherwise [`RecordError::Decode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        match Header::parse(bytes)?.kind {
            CONNECTED_KIND => Ok(Self::Connected(decode(bytes)?)),
            RECEIVED_KIND => Ok(Self::Received(decode(bytes)?)),
            CLOSED_KIND => Ok(Self::Closed(decode(bytes)?)),
            FAILED_KIND => Ok(Self::Failed(decode(bytes)?)),
            kind => Err(RecordError::UnknownKind { kind }),
        }
    }

    /// As [`Report::decode`], but `bytes` must be exactly one record.
    ///
    /// # Errors
    ///
    /// As [`Report::decode`], and `DecodeError::TrailingBytes` for bytes
    /// after the record.
    pub fn decode_exact(bytes: &[u8]) -> Result<Self, RecordError> {
        let report = Self::decode(bytes)?;
        exactly(bytes, report.encoded_len()).map(|()| report)
    }

    /// The length of its record, header included.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::Connected(_) => encoded_len::<Connected>(),
            Self::Received(_) => encoded_len::<ReceivedData>(),
            Self::Closed(_) => encoded_len::<Closed>(),
            Self::Failed(_) => encoded_len::<Failed>(),
        }
    }

    /// Encodes the report at the start of `out`, returning the record's
    /// length.
    ///
    /// # Errors
    ///
    /// As `nowayos_ipc::encode`: `out` too short for the record.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        match self {
            Self::Connected(record) => encode(record, out),
            Self::Received(record) => encode(record, out),
            Self::Closed(record) => encode(record, out),
            Self::Failed(record) => encode(record, out),
        }
    }

    /// The connection it names.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::Connected(record) => record.connection(),
            Self::Received(record) => record.connection(),
            Self::Closed(record) => record.connection(),
            Self::Failed(record) => record.connection(),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{Consumer, HEADER_LEN, InMemoryRing, Producer};

    use super::*;

    const ID: ConnectionId = ConnectionId(0x0102);
    const BACKEND: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

    fn encoded<M: Message>(message: &M) -> Vec<u8> {
        let mut out = vec![0u8; SLOT_LEN];
        let len = encode(message, &mut out).unwrap();
        out.truncate(len);
        out
    }

    #[test]
    fn each_record_has_the_documented_layout() {
        let connect = encoded(&Connect::new(ID, BACKEND, 443).unwrap());
        assert_eq!(
            connect,
            [
                0x01, 0x05, 1, 0, 8, 0, 0, 0, // header: kind, version 1, 8 bytes
                0x02, 0x01, 0xbb, 0x01, 10, 0, 2, 2
            ]
        );
        let close = encoded(&Close::new(ID));
        assert_eq!(close, [0x03, 0x05, 1, 0, 4, 0, 0, 0, 0x02, 0x01, 0, 0]);
        let connected = encoded(&Connected::new(ID));
        assert_eq!(connected[..2], [0x81, 0x05]);
        let closed = encoded(&Closed::new(ID));
        assert_eq!(closed[..2], [0x83, 0x05]);
        let failed = encoded(&Failed::new(ID, FailReason::TimedOut));
        assert_eq!(failed, [0x84, 0x05, 1, 0, 4, 0, 0, 0, 0x02, 0x01, 3, 0]);
        let send = encoded(&SendData::new(ID, b"hello").unwrap());
        assert_eq!(send.len(), SLOT_LEN);
        assert_eq!(
            send[..HEADER_LEN + 13],
            [
                0x02, 0x05, 2, 0, 0xd8, 0x03, 0, 0, // header: version 2, 984 bytes
                0x02, 0x01, 5, 0, 0, 0, 0, 0, b'h', b'e', b'l', b'l', b'o'
            ]
        );
        assert!(send[HEADER_LEN + 13..].iter().all(|byte| *byte == 0));
        let received = encoded(&ReceivedData::new(ID, &[7; MAX_DATA]).unwrap());
        assert_eq!(received[..4], [0x82, 0x05, 2, 0]);
        assert_eq!(received[10..12], [0xd0, 0x03], "976 bytes");
    }

    #[test]
    fn every_record_round_trips_with_its_own_kind_and_version() {
        let requests = [
            Request::Connect(Connect::new(ID, BACKEND, 443).unwrap()),
            Request::Send(SendData::new(ID, &[0xa5; MAX_DATA]).unwrap()),
            Request::Close(Close::new(ID)),
        ];
        for (request, kind, version) in requests
            .iter()
            .zip([CONNECT_KIND, SEND_KIND, CLOSE_KIND])
            .zip([1, 2, 1])
            .map(|((request, kind), version)| (request, kind, version))
        {
            let mut slot = [0u8; SLOT_LEN];
            let len = request.encode(&mut slot).unwrap();
            let header = Header::parse(&slot).unwrap();
            assert_eq!((header.kind, header.version), (kind, version));
            assert_eq!(header.record_len(), len);
            assert_eq!(Request::decode(&slot).as_ref(), Ok(request));
            assert_eq!(Request::decode_exact(&slot[..len]).as_ref(), Ok(request));
            assert_eq!(request.connection(), ID);
        }
        let reports = [
            Report::Connected(Connected::new(ID)),
            Report::Received(ReceivedData::new(ID, b"x").unwrap()),
            Report::Closed(Closed::new(ID)),
            Report::Failed(Failed::new(ID, FailReason::Refused)),
        ];
        for (report, kind, version) in reports
            .iter()
            .zip([CONNECTED_KIND, RECEIVED_KIND, CLOSED_KIND, FAILED_KIND])
            .zip([1, 2, 1, 1])
            .map(|((report, kind), version)| (report, kind, version))
        {
            let mut slot = [0u8; SLOT_LEN];
            let len = report.encode(&mut slot).unwrap();
            let header = Header::parse(&slot).unwrap();
            assert_eq!((header.kind, header.version), (kind, version));
            assert_eq!(Report::decode(&slot).as_ref(), Ok(report));
            assert_eq!(Report::decode_exact(&slot[..len]).as_ref(), Ok(report));
            assert_eq!(report.connection(), ID);
        }
    }

    #[test]
    fn every_record_crosses_a_ring_of_slot_len_slots() {
        let ring: InMemoryRing<8, SLOT_LEN> = InMemoryRing::new();
        let mut tx = Producer::new(&ring).unwrap();
        let mut rx = Consumer::new(&ring).unwrap();
        tx.try_send(&Connect::new(ID, BACKEND, 80).unwrap())
            .unwrap();
        tx.try_send(&SendData::new(ID, &[1; MAX_DATA]).unwrap())
            .unwrap();
        tx.try_send(&Close::new(ID)).unwrap();
        tx.try_send(&Connected::new(ID)).unwrap();
        tx.try_send(&ReceivedData::new(ID, &[2; MAX_DATA]).unwrap())
            .unwrap();
        tx.try_send(&Closed::new(ID)).unwrap();
        tx.try_send(&Failed::new(ID, FailReason::NotAllowed))
            .unwrap();
        let mut kinds = Vec::new();
        while let Some(record) = rx.try_recv_record().unwrap() {
            let bytes = record.as_bytes();
            match Request::decode(bytes) {
                Ok(request) => kinds.push(request.connection()),
                Err(RecordError::UnknownKind { .. }) => {
                    kinds.push(Report::decode(bytes).unwrap().connection());
                }
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(kinds, [ID; 7]);
    }

    #[test]
    fn data_holds_one_to_max_data_bytes() {
        assert!(SendData::new(ID, &[]).is_none());
        assert!(SendData::new(ID, &[0; MAX_DATA + 1]).is_none());
        let one = SendData::new(ID, &[9]).unwrap();
        assert_eq!(
            (one.bytes(), one.len(), one.is_empty()),
            (&[9u8][..], 1, false)
        );
        assert_eq!(
            ReceivedData::new(ID, &[3; MAX_DATA]).unwrap().len(),
            MAX_DATA
        );
        // Debug never shows the bytes.
        let shown = format!("{:?}", SendData::new(ID, b"secret").unwrap());
        assert!(!shown.contains("115"), "{shown}");
        assert!(shown.contains("len: 6"), "{shown}");
    }

    #[test]
    fn connect_takes_only_a_unicast_destination_and_a_port() {
        assert!(Connect::new(ID, BACKEND, 0).is_none());
        for address in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(0, 1, 2, 3),
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(127, 9, 9, 9),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(239, 255, 255, 250),
            Ipv4Addr::new(240, 0, 0, 1),
            Ipv4Addr::BROADCAST,
        ] {
            assert!(!is_unicast_destination(address), "{address}");
            assert!(Connect::new(ID, address, 443).is_none(), "{address}");
        }
        for address in [
            Ipv4Addr::new(1, 0, 0, 0),
            Ipv4Addr::new(10, 0, 2, 2),
            Ipv4Addr::new(126, 255, 255, 255),
            Ipv4Addr::new(128, 0, 0, 1),
            Ipv4Addr::new(223, 255, 255, 255),
        ] {
            assert!(is_unicast_destination(address), "{address}");
        }
    }

    #[test]
    fn fail_reasons_have_codes_one_to_six() {
        for (reason, code) in FailReason::ALL.into_iter().zip(1u8..) {
            assert_eq!(reason.code(), code);
            assert_eq!(FailReason::from_code(code), Some(reason));
            assert!(!reason.to_string().is_empty());
        }
        assert_eq!(FailReason::from_code(0), None);
        assert_eq!(FailReason::from_code(7), None);
    }

    #[test]
    fn reserved_and_crossed_kinds_are_unknown_on_each_ring() {
        let mut slot = [0u8; SLOT_LEN];
        for kind in [RESOLVE_KIND, RESOLVED_KIND, CONNECTED_KIND, 0, 0x7fff] {
            Header {
                kind,
                version: 1,
                payload_len: 4,
            }
            .write(&mut slot)
            .unwrap();
            assert_eq!(
                Request::decode(&slot),
                Err(RecordError::UnknownKind { kind })
            );
        }
        for kind in [RESOLVE_KIND, RESOLVED_KIND, CONNECT_KIND, CLOSE_KIND] {
            Header {
                kind,
                version: 1,
                payload_len: 4,
            }
            .write(&mut slot)
            .unwrap();
            assert_eq!(
                Report::decode(&slot),
                Err(RecordError::UnknownKind { kind })
            );
        }
    }

    #[test]
    fn each_field_is_checked() {
        let mut connect = encoded(&Connect::new(ID, BACKEND, 443).unwrap());
        connect[10..12].copy_from_slice(&[0, 0]);
        assert_eq!(
            Request::decode(&connect),
            Err(RecordError::Decode(DecodeError::InvalidValue {
                field: "port"
            }))
        );
        let mut connect = encoded(&Connect::new(ID, BACKEND, 443).unwrap());
        connect[12] = 127;
        assert_eq!(
            Request::decode(&connect),
            Err(RecordError::Decode(DecodeError::InvalidValue {
                field: "address"
            }))
        );
        let mut close = encoded(&Close::new(ID));
        close[11] = 1;
        assert_eq!(
            Request::decode(&close),
            Err(RecordError::Decode(DecodeError::NonZeroPadding))
        );
        let mut failed = encoded(&Failed::new(ID, FailReason::Reset));
        failed[10] = 7;
        assert_eq!(
            Report::decode(&failed),
            Err(RecordError::Decode(DecodeError::InvalidValue {
                field: "reason"
            }))
        );
        let mut failed = encoded(&Failed::new(ID, FailReason::Reset));
        failed[11] = 1;
        assert_eq!(
            Report::decode(&failed),
            Err(RecordError::Decode(DecodeError::NonZeroPadding))
        );
        let good = encoded(&SendData::new(ID, b"abc").unwrap());
        for (at, value, error) in [
            // A length of 0, and one beyond the bound.
            (10, 0u16, DecodeError::InvalidValue { field: "len" }),
            (
                10,
                (MAX_DATA + 1) as u16,
                DecodeError::InvalidValue { field: "len" },
            ),
            (10, u16::MAX, DecodeError::InvalidValue { field: "len" }),
        ] {
            let mut bad = good.clone();
            bad[at..at + 2].copy_from_slice(&value.to_le_bytes());
            assert_eq!(Request::decode(&bad), Err(RecordError::Decode(error)));
        }
        let mut reserved = good.clone();
        reserved[14] = 1;
        assert_eq!(
            Request::decode(&reserved),
            Err(RecordError::Decode(DecodeError::NonZeroPadding))
        );
        for at in [HEADER_LEN + DATA_PREFIX + 3, SLOT_LEN - 1] {
            let mut padding = good.clone();
            padding[at] = 0x80;
            assert_eq!(
                Request::decode(&padding),
                Err(RecordError::Decode(DecodeError::NonZeroPadding)),
                "a byte past the length at {at}"
            );
        }
    }

    #[test]
    fn padding_is_every_byte_from_the_length_on() {
        let mut bytes = [0u8; MAX_DATA];
        for len in [1, 15, 16, 17, MAX_DATA / 2, MAX_DATA - 1, MAX_DATA] {
            bytes.fill(0);
            bytes[..len].fill(0xff);
            assert!(zero_from(&bytes, len), "{len} bytes, then zeros");
            if len < MAX_DATA {
                for at in [
                    len,
                    (len + 1).min(MAX_DATA - 1),
                    (len + 16).min(MAX_DATA - 1),
                    MAX_DATA - 1,
                ] {
                    let mut stray = bytes;
                    stray[at] = 1;
                    assert!(!zero_from(&stray, len), "{len} bytes, a stray byte at {at}");
                }
            }
        }
        assert!(zero_from(&[0; MAX_DATA], 0));
        assert!(
            zero_from(&[7; MAX_DATA], MAX_DATA + 5),
            "no padding past the area"
        );
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            RecordError::UnknownKind { kind: 0x0504 }.to_string(),
            "a record of kind 0x0504, which this ring does not carry"
        );
        assert!(
            RecordError::Decode(DecodeError::NonZeroPadding)
                .to_string()
                .starts_with("malformed socket record")
        );
        assert_eq!(ConnectionId(7).to_string(), "connection 7");
    }
}
