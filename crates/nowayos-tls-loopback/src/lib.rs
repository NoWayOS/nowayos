// SPDX-License-Identifier: MIT OR Apache-2.0
//! A test instrument: a rustls client and a
//! rustls server, both on `nowayos-tls-crypto`, joined by memory buffers
//! with no network, so that a TLS 1.3 handshake can run inside one
//! protection domain, on its heap arena, and on the host under a counting
//! allocator. **Never in a product image:** the server signs with a key made
//! from the caller's random source at start, which a test seeds with a
//! fixed value.
//!
//! * The server presents an RFC 7250 **raw public key** - Ed25519, made at
//!   start from the random source - so no key or certificate file exists
//!   anywhere. The client pins it: it accepts that key and no other, and
//!   checks the server's CertificateVerify with it.
//! * Both use rustls's **unbuffered** API, as a protection domain must
//!   (rustls's buffered connections need `std`): each side's records are
//!   appended to the other's incoming buffer.
//! * TLS 1.3 only, no resumption, no session tickets, no client
//!   certificate; the client offers every suite and group the provider has.
//! * Then the client sends [`CLIENT_MESSAGE`] and the server
//!   [`SERVER_MESSAGE`]: data crosses both ways.
//!
//! [`Certificate::Largest`] pads the server's Certificate message to the
//! largest rustls accepts (a 65,535-byte payload): the handshake that sets
//! the arena's size, [`ARENA_BYTES`].

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{Resumption, UnbufferedClientConnection};
use rustls::crypto::{CryptoProvider, SecureRandom, verify_tls13_signature_with_raw_key};
use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
use rustls::server::{
    AlwaysResolvesServerRawPublicKeys, NoServerSessionStorage, UnbufferedServerConnection,
};
use rustls::sign::{CertifiedKey, Signer, SigningKey};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, InsufficientSizeError, UnbufferedStatus};
use rustls::{
    CipherSuite, ClientConfig, DigitallySignedStruct, NamedGroup, ServerConfig, SignatureAlgorithm,
    SignatureScheme,
};

/// The heap arena, in bytes, of a protection domain that runs these
/// handshakes: twice the larger peak the host test measures
/// ([`LARGEST_PEAK`], the [`Certificate::Largest`] handshake), rounded up
/// to whole 4 KiB pages, so that the largest handshake rustls accepts never
/// exhausts it. The margin is for what the counts leave out:
/// dlmalloc's chunk headers and alignment, and above all fragmentation, as
/// 16 KiB records and 64 KiB messages are made, grown and freed. Half
/// again was not enough: in `nowayos-tls-test` that arena (400 KiB)
/// refused a 64 KiB request with 200 KiB in use. `nowayos-tls-test` shows
/// this one holds both handshakes in a PD.
pub const ARENA_BYTES: usize = (LARGEST_PEAK * 2).div_ceil(PAGE) * PAGE;

/// A page, in bytes.
const PAGE: usize = 4096;

/// The host test's peak for the [`Certificate::Largest`] handshake (the
/// server's Certificate message 65,535 bytes), both ends in one heap, in
/// bytes requested, rustls 0.23.46: `tests/arena.rs` under `make test`,
/// where the workspace's dev-dependencies turn on rustls's `std` feature
/// (a PD's build, without it, uses no more). Measured 2026-10-14.
pub const LARGEST_PEAK: usize = 270_725;

/// The host test's peak for the [`Certificate::Plain`] handshake, measured
/// as [`LARGEST_PEAK`] is.
pub const PLAIN_PEAK: usize = 32_776;

/// The name the client asks for (any name: the server's key is pinned).
pub const SERVER_NAME: &str = "loopback.nowayos.test";

/// What the client sends once the handshake is done.
pub const CLIENT_MESSAGE: &[u8] = b"nowayos tls-loopback: client to server";

/// What the server sends once the handshake is done.
pub const SERVER_MESSAGE: &[u8] = b"nowayos tls-loopback: server to client";

/// The largest handshake message payload rustls accepts, in bytes
/// (rustls 0.23.46, `MAX_HANDSHAKE_SIZE`).
pub const LARGEST_HANDSHAKE_PAYLOAD: usize = 0xffff;

/// The wall-clock time both ends are given: a raw public key has no
/// validity period, so nothing checks it, but rustls without `std` needs
/// a clock. 2026-10-14T00:00:00Z.
const FIXED_TIME_SECS: u64 = 1_792_022_400;

/// How many times each side is stepped before the run is called stalled.
const MAX_STEPS: usize = 256;

/// An Ed25519 public key in a SubjectPublicKeyInfo (RFC 8410): this prefix,
/// then the 32-byte key.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Which Certificate message the server sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Certificate {
    /// Its raw public key alone (a 44-byte entry).
    Plain,
    /// Its raw public key and one filler entry, making the message's payload
    /// exactly [`LARGEST_HANDSHAKE_PAYLOAD`] bytes. The client accepts the
    /// filler in this mode only.
    Largest,
    /// One byte more than [`Certificate::Largest`]: rustls must refuse it.
    TooLarge,
}

/// Which end of the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The rustls client.
    Client,
    /// The rustls server.
    Server,
}

/// Why a run failed.
#[derive(Debug)]
pub enum Failure {
    /// rustls failed at one end.
    Tls(End, rustls::Error),
    /// The random source failed while the server's key was made.
    Random,
    /// rustls was in a state this instrument does not drive (0-RTT data).
    UnexpectedState(End),
    /// Neither end could make progress, or the run took too many steps.
    Stalled,
    /// The data an end received is not what the other sent.
    WrongData(End),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tls(end, error) => write!(f, "{end:?}: {error}"),
            Self::Random => f.write_str("the random source failed"),
            Self::UnexpectedState(end) => write!(f, "{end:?}: an unexpected rustls state"),
            Self::Stalled => f.write_str("neither end can make progress"),
            Self::WrongData(end) => write!(f, "{end:?}: received the wrong data"),
        }
    }
}

/// What a run showed.
#[derive(Debug)]
pub struct Report {
    /// The suite both ends negotiated.
    pub suite: CipherSuite,
    /// The key-exchange group both ends negotiated.
    pub group: NamedGroup,
    /// The server's Certificate message's payload, in bytes.
    pub certificate_payload: usize,
}

/// Runs one handshake and the exchange of data, with every random byte -
/// the server's key, both ends' randoms and key shares - from `random`.
///
/// # Errors
///
/// The first [`Failure`]; a refused handshake never reaches the data.
pub fn run(random: &'static dyn SecureRandom, certificate: Certificate) -> Result<Report, Failure> {
    run_pinning(random, certificate, false)
}

/// [`run`] with the plain Certificate message and the client pinning
/// another key - the server's with one bit changed - so that the client
/// must refuse the server.
///
/// # Errors
///
/// [`Failure::Tls`] at the client, which is the point; any other
/// [`Failure`] as [`run`].
pub fn run_pinning_another_key(random: &'static dyn SecureRandom) -> Result<Report, Failure> {
    run_pinning(random, Certificate::Plain, true)
}

fn run_pinning(
    random: &'static dyn SecureRandom,
    certificate: Certificate,
    another_key: bool,
) -> Result<Report, Failure> {
    let provider = Arc::new(nowayos_tls_crypto::provider(random));
    let server_key = ServerKey::from_random(random)?;
    let spki = server_key.spki();
    let filler = filler_len(spki.len(), certificate);
    let mut chain = vec![CertificateDer::from(spki.clone())];
    if let Some(len) = filler {
        chain.push(CertificateDer::from(vec![0xA5; len]));
    }
    let certificate_payload = certificate_payload_len(&chain);

    let mut pinned = spki;
    if another_key && let Some(last) = pinned.last_mut() {
        *last ^= 1;
    }
    let client_config = client_config(Arc::clone(&provider), pinned, filler.is_some())?;
    let server_config = server_config(provider, chain, server_key)?;
    let name = ServerName::try_from(SERVER_NAME)
        .map_err(|_| Failure::Tls(End::Client, rustls::Error::General(String::from("name"))))?;
    let mut client = UnbufferedClientConnection::new(client_config, name)
        .map_err(|e| Failure::Tls(End::Client, e))?;
    let mut server =
        UnbufferedServerConnection::new(server_config).map_err(|e| Failure::Tls(End::Server, e))?;

    let mut client_end = Endpoint::new(End::Client, CLIENT_MESSAGE, SERVER_MESSAGE);
    let mut server_end = Endpoint::new(End::Server, SERVER_MESSAGE, CLIENT_MESSAGE);
    for _ in 0..MAX_STEPS {
        let client_step = client_end.step(&mut Client(&mut client), &mut server_end.inbox)?;
        let server_step = server_end.step(&mut Server(&mut server), &mut client_end.inbox)?;
        if client_end.done() && server_end.done() {
            let suite = client
                .negotiated_cipher_suite()
                .map(|suite| suite.suite())
                .ok_or(Failure::Stalled)?;
            let group = client
                .negotiated_key_exchange_group()
                .map(|group| group.name())
                .ok_or(Failure::Stalled)?;
            return Ok(Report {
                suite,
                group,
                certificate_payload,
            });
        }
        if client_step == Step::Blocked && server_step == Step::Blocked {
            return Err(Failure::Stalled);
        }
    }
    Err(Failure::Stalled)
}

/// The filler entry's length for `certificate`, given the key's entry.
fn filler_len(spki_len: usize, certificate: Certificate) -> Option<usize> {
    // The payload: a 1-byte empty request context, a 3-byte list length,
    // and per entry a 3-byte length, the data and 2 bytes of (no)
    // extensions.
    let fixed = 1 + 3 + (3 + 2) + (3 + 2);
    let largest = LARGEST_HANDSHAKE_PAYLOAD
        .checked_sub(fixed)?
        .checked_sub(spki_len)?;
    match certificate {
        Certificate::Plain => None,
        Certificate::Largest => Some(largest),
        Certificate::TooLarge => largest.checked_add(1),
    }
}

/// The Certificate message's payload length for `chain`.
fn certificate_payload_len(chain: &[CertificateDer<'_>]) -> usize {
    chain.iter().fold(1 + 3, |len, entry| {
        len.saturating_add(3 + 2).saturating_add(entry.len())
    })
}

fn client_config(
    provider: Arc<CryptoProvider>,
    spki: Vec<u8>,
    filler_allowed: bool,
) -> Result<Arc<ClientConfig>, Failure> {
    let mut config = ClientConfig::builder_with_details(provider, Arc::new(FixedTime))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| Failure::Tls(End::Client, e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedKey {
            spki,
            filler_allowed,
        }))
        .with_no_client_auth();
    config.resumption = Resumption::disabled();
    Ok(Arc::new(config))
}

fn server_config(
    provider: Arc<CryptoProvider>,
    chain: Vec<CertificateDer<'static>>,
    key: ServerKey,
) -> Result<Arc<ServerConfig>, Failure> {
    let certified = Arc::new(CertifiedKey::new(chain, Arc::new(key)));
    let mut config = ServerConfig::builder_with_details(provider, Arc::new(FixedTime))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| Failure::Tls(End::Server, e))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(certified)));
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(NoServerSessionStorage {});
    Ok(Arc::new(config))
}

/// A fixed wall-clock time.
#[derive(Debug)]
struct FixedTime;

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            FIXED_TIME_SECS,
        )))
    }
}

/// The server's Ed25519 key: test-only, made from the random source.
#[derive(Clone)]
struct ServerKey(ed25519_dalek::SigningKey);

impl fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerKey(Ed25519)")
    }
}

impl ServerKey {
    fn from_random(random: &dyn SecureRandom) -> Result<Self, Failure> {
        let mut secret = [0u8; ed25519_dalek::SECRET_KEY_LENGTH];
        random.fill(&mut secret).map_err(|_| Failure::Random)?;
        let key = ed25519_dalek::SigningKey::from_bytes(&secret);
        secret.fill(0);
        Ok(Self(key))
    }

    /// The public key as a SubjectPublicKeyInfo.
    fn spki(&self) -> Vec<u8> {
        let mut spki = Vec::from(ED25519_SPKI_PREFIX);
        spki.extend_from_slice(self.0.verifying_key().as_bytes());
        spki
    }
}

impl SigningKey for ServerKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ED25519)
            .then(|| Box::new(self.clone()) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ED25519
    }
}

impl Signer for ServerKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        use ed25519_dalek::Signer as _;
        Ok(self.0.sign(message).to_bytes().to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

/// The client's check of the server: its raw public key must be exactly
/// the pinned one, and its CertificateVerify must verify with it.
#[derive(Debug)]
struct PinnedKey {
    spki: Vec<u8>,
    /// Whether entries after the key are accepted: only for the run that
    /// pads the message to its largest.
    filler_allowed: bool,
}

impl ServerCertVerifier for PinnedKey {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.spki.as_slice()
            || (!intermediates.is_empty() && !self.filler_allowed)
        {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(String::from(
            "TLS 1.2 is not offered",
        )))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        if cert.as_ref() != self.spki.as_slice() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &nowayos_tls_crypto::SIGNATURE_VERIFICATION_ALGORITHMS,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        nowayos_tls_crypto::SIGNATURE_VERIFICATION_ALGORITHMS.supported_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// What one step of an end did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// It did something; step it again.
    Progress,
    /// It waits for the other end.
    Blocked,
}

/// One end of the loopback: what it has received and not yet processed,
/// the application data it sends and expects, and how far it has got.
struct Endpoint {
    end: End,
    inbox: Vec<u8>,
    sends: &'static [u8],
    expects: &'static [u8],
    received: Vec<u8>,
    sent: bool,
}

impl Endpoint {
    fn new(end: End, sends: &'static [u8], expects: &'static [u8]) -> Self {
        Self {
            end,
            inbox: Vec::new(),
            sends,
            expects,
            received: Vec::new(),
            sent: false,
        }
    }

    fn done(&self) -> bool {
        self.sent && self.received.len() == self.expects.len()
    }

    /// Processes what is in the inbox once, appending anything this end
    /// sends to `peer_inbox`.
    fn step<C: Side>(&mut self, conn: &mut C, peer_inbox: &mut Vec<u8>) -> Result<Step, Failure> {
        let end = self.end;
        let tls = |error| Failure::Tls(end, error);
        let UnbufferedStatus { mut discard, state } = conn.process(&mut self.inbox);
        let step = match state.map_err(tls)? {
            ConnectionState::ReadTraffic(mut traffic) => {
                while let Some(record) = traffic.next_record() {
                    let record = record.map_err(tls)?;
                    discard = discard.saturating_add(record.discard);
                    self.received.extend_from_slice(record.payload);
                }
                if !self.expects.starts_with(&self.received) {
                    return Err(Failure::WrongData(end));
                }
                Step::Progress
            }
            ConnectionState::EncodeTlsData(mut encode) => {
                append(
                    peer_inbox,
                    |out| encode.encode(out),
                    |e| match e {
                        rustls::unbuffered::EncodeError::InsufficientSize(size) => Ok(size),
                        rustls::unbuffered::EncodeError::AlreadyEncoded => Err(Failure::Stalled),
                    },
                )?;
                Step::Progress
            }
            ConnectionState::TransmitTlsData(transmit) => {
                // What it encoded is already in the peer's inbox.
                transmit.done();
                Step::Progress
            }
            ConnectionState::WriteTraffic(mut write) => {
                if self.sent {
                    Step::Blocked
                } else {
                    let message = self.sends;
                    append(
                        peer_inbox,
                        |out| write.encrypt(message, out),
                        |e| match e {
                            rustls::unbuffered::EncryptError::InsufficientSize(size) => Ok(size),
                            _ => Err(Failure::Stalled),
                        },
                    )?;
                    self.sent = true;
                    Step::Progress
                }
            }
            ConnectionState::BlockedHandshake | ConnectionState::PeerClosed => Step::Blocked,
            ConnectionState::Closed => Step::Blocked,
            _ => return Err(Failure::UnexpectedState(end)),
        };
        if discard > 0 {
            self.inbox.drain(..discard.min(self.inbox.len()));
        }
        Ok(step)
    }
}

/// Appends what `write` puts in an output buffer to `out`, growing the
/// buffer by what rustls asks for when it is too small.
fn append<E>(
    out: &mut Vec<u8>,
    mut write: impl FnMut(&mut [u8]) -> Result<usize, E>,
    needs: impl Fn(E) -> Result<InsufficientSizeError, Failure>,
) -> Result<(), Failure> {
    let start = out.len();
    let mut room = 0;
    loop {
        // Exactly what rustls asks for: no doubling, so the arena's
        // measurement is the protocol's, not the buffer's growth policy.
        out.reserve_exact(room);
        out.resize(start.saturating_add(room), 0);
        let tail = out.get_mut(start..).unwrap_or_default();
        match write(tail) {
            Ok(written) => {
                out.truncate(start.saturating_add(written));
                return Ok(());
            }
            Err(error) => {
                let InsufficientSizeError { required_size } = needs(error)?;
                if required_size <= room {
                    return Err(Failure::Stalled);
                }
                room = required_size;
            }
        }
    }
}

/// rustls's two unbuffered connections, which share no trait for
/// processing records.
trait Side {
    type Data;
    fn process<'c, 'i>(
        &'c mut self,
        incoming: &'i mut [u8],
    ) -> UnbufferedStatus<'c, 'i, Self::Data>;
}

struct Client<'a>(&'a mut UnbufferedClientConnection);

impl Side for Client<'_> {
    type Data = rustls::client::ClientConnectionData;
    fn process<'c, 'i>(
        &'c mut self,
        incoming: &'i mut [u8],
    ) -> UnbufferedStatus<'c, 'i, Self::Data> {
        self.0.process_tls_records(incoming)
    }
}

struct Server<'a>(&'a mut UnbufferedServerConnection);

impl Side for Server<'_> {
    type Data = rustls::server::ServerConnectionData;
    fn process<'c, 'i>(
        &'c mut self,
        incoming: &'i mut [u8],
    ) -> UnbufferedStatus<'c, 'i, Self::Data> {
        self.0.process_tls_records(incoming)
    }
}

#[cfg(test)]
mod tests;
