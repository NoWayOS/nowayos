// SPDX-License-Identifier: MIT OR Apache-2.0
//! The provider against an independent TLS server, OpenSSL's `s_server`
//! from the dev image: these interop
//! and negative tests stand where rustls's unpublished `rustls-provider-test`
//! was meant to.
//!
//! Every key and certificate is made by `openssl` at test time in a fresh
//! folder under the system's temporary directory, and deleted after; none
//! is stored (`make secrets`). Outside the dev container the tests need
//! OpenSSL 3's `openssl` on the path, and are skipped, saying so, without
//! it; inside it, a missing `openssl` fails them.
//!
//! * Each cipher suite with each key-exchange group: the handshake
//!   completes, data crosses both ways (`s_server -rev` sends each line
//!   back reversed), and the suite and group negotiated are the one each the
//!   client offered - which OpenSSL's own report of the suite confirms.
//! * Server certificates for ECDSA P-256 and P-384, RSA and Ed25519 keys,
//!   each under a test root, chosen so that every algorithm the provider
//!   verifies with checks a real signature: each passes.
//! * A certificate signed by another key, a CertificateVerify from a key
//!   other than the certificate's, and a server offering only TLS 1.2 are
//!   each refused before any application data.
//! * One bit flipped in an encrypted record by a relay: the client ends the
//!   connection with `bad_record_mac` - which OpenSSL reports receiving - and
//!   returns no plaintext.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use nowayos_tls_crypto::{
    TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256, provider,
};
use rustls::client::ClientConnection;
use rustls::crypto::{CryptoProvider, GetRandomFailed, SecureRandom};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::server::ServerConnection;
use rustls::sign::{CertifiedKey, Signer, SigningKey, SingleCertAndKey};
use rustls::{
    CertificateError, CipherSuite, ClientConfig, Error, NamedGroup, RootCertStore, ServerConfig,
    SignatureAlgorithm, SignatureScheme, SupportedCipherSuite,
};

/// The operating system's random source, for tests only: the provider takes
/// whatever source its caller passes.
#[derive(Debug)]
struct OsRandom;

impl SecureRandom for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(buf))
            .map_err(|_| GetRandomFailed)
    }
}

static RANDOM: OsRandom = OsRandom;

/// The provider, offering only `suite` and `group`.
fn provider_offering(suite: SupportedCipherSuite, group: NamedGroup) -> CryptoProvider {
    let mut provider = provider(&RANDOM);
    provider
        .cipher_suites
        .retain(|offered| offered.suite() == suite.suite());
    provider.kx_groups.retain(|offered| offered.name() == group);
    assert_eq!(provider.cipher_suites.len(), 1);
    assert_eq!(provider.kx_groups.len(), 1);
    provider
}

/// A TLS 1.3 client on `provider` trusting only `root` (DER).
fn client_config(provider: CryptoProvider, root: &[u8]) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(root.to_vec())).unwrap();
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// `true` if the tests can run here; `false` (and a note) only outside the
/// dev container, where OpenSSL 3 may not be installed.
fn have_openssl() -> bool {
    let version = Command::new("openssl").arg("version").output();
    let ok = matches!(&version, Ok(out) if out.status.success()
        && String::from_utf8_lossy(&out.stdout).starts_with("OpenSSL 3"));
    if !ok {
        assert!(
            std::env::var_os("NOWAYOS_CONTAINER").is_none(),
            "the dev image's openssl (OpenSSL 3) is missing or not first on the path"
        );
        eprintln!("skipped: these tests need OpenSSL 3's `openssl` (the dev image has it)");
    }
    ok
}

/// A fresh folder for one test's keys and certificates, deleted on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nowayos-tls-crypto-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs `openssl` with `args` in `dir`, failing the test if it fails.
fn openssl(dir: &Path, args: &[&str]) {
    let out = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "openssl {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A kind of key, as `openssl genpkey` makes it.
#[derive(Clone, Copy, Debug)]
enum KeyKind {
    P256,
    P384,
    Rsa2048,
    Ed25519,
}

impl KeyKind {
    fn genpkey_args(self) -> &'static [&'static str] {
        match self {
            Self::P256 => &["-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256"],
            Self::P384 => &["-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-384"],
            Self::Rsa2048 => &["-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"],
            Self::Ed25519 => &["-algorithm", "ED25519"],
        }
    }
}

/// Makes `NAME.key` (PEM).
fn make_key(dir: &Path, name: &str, kind: KeyKind) {
    let key = format!("{name}.key");
    let mut args = vec!["genpkey"];
    args.extend_from_slice(kind.genpkey_args());
    args.extend_from_slice(&["-out", &key]);
    openssl(dir, &args);
}

/// Makes a root: `NAME.key` and a self-signed `NAME.pem`, subject `cn`.
fn make_root(dir: &Path, name: &str, kind: KeyKind, cn: &str) {
    make_key(dir, name, kind);
    let (key, pem, subject) = (
        format!("{name}.key"),
        format!("{name}.pem"),
        format!("/CN={cn}"),
    );
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-new",
            "-key",
            &key,
            "-out",
            &pem,
            "-days",
            "2",
            "-subj",
            &subject,
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign",
        ],
    );
}

/// Makes a server key `NAME.key` and its certificate `NAME.pem` for
/// `localhost`, signed by root `ca` with `sign_args` (a digest, padding).
fn make_leaf(dir: &Path, name: &str, kind: KeyKind, ca: &str, sign_args: &[&str]) {
    make_key(dir, name, kind);
    sign_leaf(dir, name, name, ca, sign_args);
}

/// Makes `cert.pem` for the existing key `key.key`, signed by root `ca`.
fn sign_leaf(dir: &Path, key: &str, cert: &str, ca: &str, sign_args: &[&str]) {
    let ext = dir.join("leaf.ext");
    fs::write(
        &ext,
        "basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    let (key, csr, pem) = (
        format!("{key}.key"),
        format!("{cert}.csr"),
        format!("{cert}.pem"),
    );
    let (ca_pem, ca_key) = (format!("{ca}.pem"), format!("{ca}.key"));
    openssl(
        dir,
        &[
            "req",
            "-new",
            "-key",
            &key,
            "-out",
            &csr,
            "-subj",
            "/CN=localhost",
        ],
    );
    let mut args = vec![
        "x509",
        "-req",
        "-in",
        &csr,
        "-CA",
        &ca_pem,
        "-CAkey",
        &ca_key,
        "-set_serial",
        "2",
        "-days",
        "2",
        "-extfile",
        "leaf.ext",
        "-out",
        &pem,
    ];
    args.extend_from_slice(sign_args);
    openssl(dir, &args);
}

/// A certificate's DER.
fn der(dir: &Path, name: &str) -> Vec<u8> {
    let pem = format!("{name}.pem");
    let out = Command::new("openssl")
        .args(["x509", "-in", &pem, "-outform", "DER"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}

/// A running `openssl s_server`, accepting one connection, and what it
/// prints.
struct Server {
    child: Child,
    port: u16,
    output: mpsc::Receiver<String>,
}

impl Server {
    /// Starts `s_server` in `dir` with certificate and key `name`, sending
    /// each line it receives back reversed, plus `extra` arguments.
    fn start(dir: &Path, name: &str, extra: &[&str]) -> Self {
        let (cert, key) = (format!("{name}.pem"), format!("{name}.key"));
        let mut child = Command::new("openssl")
            .args([
                "s_server",
                "-accept",
                "127.0.0.1:0",
                "-naccept",
                "1",
                "-cert",
                &cert,
                "-key",
                &key,
                "-rev",
                "-num_tickets",
                "0",
            ])
            .args(extra)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, output) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        let tx_err = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx_err.send(line);
            }
        });
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let port = loop {
            let line = output
                .recv_timeout(Duration::from_secs(30))
                .expect("s_server reports where it accepts");
            if let Some(address) = line.strip_prefix("ACCEPT ") {
                break address.rsplit(':').next().unwrap().parse().unwrap();
            }
        };
        Self {
            child,
            port,
            output,
        }
    }

    fn connect(&self) -> TcpStream {
        let socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        socket
    }

    /// Everything the server printed, once it has exited.
    fn finish(mut self) -> Vec<String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.output.try_iter().collect()
    }
}

/// What one client connection did.
struct Outcome {
    /// The handshake's or the session's error, if any.
    error: Option<Error>,
    /// The plaintext the client received.
    received: Vec<u8>,
    suite: Option<CipherSuite>,
    group: Option<NamedGroup>,
    handshaking: bool,
}

/// Connects a client with `config` over `socket`, sends `LINE`, and reads
/// until the reversed line has come back, the connection ends or fails.
fn run_client(config: Arc<ClientConfig>, socket: &mut TcpStream) -> Outcome {
    const LINE: &[u8] = b"nowayos-tls-crypto interop\n";
    let name = ServerName::try_from("localhost").unwrap();
    let mut conn = ClientConnection::new(config, name).unwrap();
    let mut received = Vec::new();
    let mut error = None;
    // The handshake, then the line out.
    while conn.is_handshaking() && error.is_none() {
        if let Err(e) = conn.complete_io(socket) {
            error = Some(rustls_error(e));
        }
    }
    if error.is_none() {
        conn.writer().write_all(LINE).unwrap();
        while conn.wants_write() {
            conn.write_tls(socket).unwrap();
        }
        // The reversed line back, or an error.
        while !received.ends_with(b"\n") && error.is_none() {
            match conn.read_tls(socket) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    error = Some(Error::General(e.to_string()));
                    break;
                }
            }
            match conn.process_new_packets() {
                Ok(_) => {
                    let mut buf = [0u8; 256];
                    while let Ok(n) = conn.reader().read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        received.extend_from_slice(&buf[..n]);
                    }
                }
                Err(e) => {
                    // The last gasp: the alert rustls queued for the error.
                    while conn.wants_write() {
                        if conn.write_tls(socket).is_err() {
                            break;
                        }
                    }
                    error = Some(e);
                }
            }
        }
    }
    let _ = socket.shutdown(Shutdown::Both);
    Outcome {
        error,
        received,
        suite: conn.negotiated_cipher_suite().map(|s| s.suite()),
        group: conn.negotiated_key_exchange_group().map(|g| g.name()),
        handshaking: conn.is_handshaking(),
    }
}

/// The rustls error inside an I/O error from `complete_io`.
fn rustls_error(e: std::io::Error) -> Error {
    match e.get_ref().and_then(|inner| inner.downcast_ref::<Error>()) {
        Some(inner) => inner.clone(),
        None => Error::General(e.to_string()),
    }
}

const REVERSED: &[u8] = b"poretni otpyrc-slt-soyawon\n";

/// The suites and groups, as rustls and OpenSSL name them.
const SUITES: [(&SupportedCipherSuite, &str); 3] = [
    (&TLS13_AES_128_GCM_SHA256, "TLS_AES_128_GCM_SHA256"),
    (&TLS13_AES_256_GCM_SHA384, "TLS_AES_256_GCM_SHA384"),
    (
        &TLS13_CHACHA20_POLY1305_SHA256,
        "TLS_CHACHA20_POLY1305_SHA256",
    ),
];
const GROUPS: [(NamedGroup, &str); 2] = [
    (NamedGroup::X25519, "x25519"),
    (NamedGroup::secp256r1, "secp256r1"),
];

#[test]
fn every_suite_with_every_group_completes_and_carries_data_both_ways() {
    if !have_openssl() {
        return;
    }
    let dir = Scratch::new("suites");
    make_root(&dir.0, "root", KeyKind::P256, "nowayos test root");
    make_leaf(&dir.0, "server", KeyKind::P256, "root", &["-sha256"]);
    let root = der(&dir.0, "root");
    for (suite, openssl_name) in SUITES {
        for (group, openssl_group) in GROUPS {
            let server = Server::start(&dir.0, "server", &[]);
            let config = client_config(provider_offering(*suite, group), &root);
            let outcome = run_client(config, &mut server.connect());
            let printed = server.finish();
            assert!(
                outcome.error.is_none(),
                "{openssl_name} {group:?}: {:?}",
                outcome.error
            );
            assert_eq!(outcome.received, REVERSED, "{openssl_name} {group:?}");
            assert_eq!(outcome.suite, Some(suite.suite()));
            assert_eq!(outcome.group, Some(group));
            assert!(
                printed.iter().any(|l| l.contains(openssl_name)),
                "OpenSSL reports {openssl_name}: {printed:?}"
            );
            let offered = format!("Supported groups: {openssl_group}");
            assert!(
                printed.contains(&offered),
                "OpenSSL saw only {openssl_group} offered: {printed:?}"
            );
        }
    }
}

/// One server certificate chain, and how OpenSSL signs with its key.
struct Chain {
    name: &'static str,
    root: KeyKind,
    leaf: KeyKind,
    /// How the root signs the server's certificate.
    cert_sign: &'static [&'static str],
    /// The signature algorithms the server may sign the handshake with.
    sigalgs: &'static str,
}

/// Every algorithm the provider verifies with checks a real signature: the
/// root's over the certificate, or the server's CertificateVerify.
const CHAINS: [Chain; 6] = [
    // ECDSA_P384_SHA256 (certificate), ECDSA_P256_SHA256 (handshake).
    Chain {
        name: "p256",
        root: KeyKind::P384,
        leaf: KeyKind::P256,
        cert_sign: &["-sha256"],
        sigalgs: "ECDSA+SHA256",
    },
    // ECDSA_P256_SHA384 (certificate), ECDSA_P384_SHA384 (handshake).
    Chain {
        name: "p384",
        root: KeyKind::P256,
        leaf: KeyKind::P384,
        cert_sign: &["-sha384"],
        sigalgs: "ECDSA+SHA384",
    },
    // RSA_PKCS1_SHA256 (certificate), RSA_PSS_SHA256 (handshake).
    Chain {
        name: "rsa-sha256",
        root: KeyKind::Rsa2048,
        leaf: KeyKind::Rsa2048,
        cert_sign: &["-sha256"],
        sigalgs: "RSA-PSS+SHA256",
    },
    // RSA_PKCS1_SHA384, RSA_PSS_SHA384.
    Chain {
        name: "rsa-sha384",
        root: KeyKind::Rsa2048,
        leaf: KeyKind::Rsa2048,
        cert_sign: &["-sha384"],
        sigalgs: "RSA-PSS+SHA384",
    },
    // RSA_PKCS1_SHA512, RSA_PSS_SHA512.
    Chain {
        name: "rsa-sha512",
        root: KeyKind::Rsa2048,
        leaf: KeyKind::Rsa2048,
        cert_sign: &["-sha512"],
        sigalgs: "RSA-PSS+SHA512",
    },
    // ED25519, both.
    Chain {
        name: "ed25519",
        root: KeyKind::Ed25519,
        leaf: KeyKind::Ed25519,
        cert_sign: &[],
        sigalgs: "ed25519",
    },
];

#[test]
fn certificates_for_every_key_kind_verify() {
    if !have_openssl() {
        return;
    }
    for chain in CHAINS {
        let dir = Scratch::new(chain.name);
        make_root(&dir.0, "root", chain.root, "nowayos test root");
        make_leaf(&dir.0, "server", chain.leaf, "root", chain.cert_sign);
        let server = Server::start(&dir.0, "server", &["-sigalgs", chain.sigalgs]);
        let config = client_config(
            provider_offering(TLS13_AES_128_GCM_SHA256, NamedGroup::X25519),
            &der(&dir.0, "root"),
        );
        let outcome = run_client(config, &mut server.connect());
        let printed = server.finish();
        assert!(
            outcome.error.is_none(),
            "{}: {:?}; server: {printed:?}",
            chain.name,
            outcome.error
        );
        assert_eq!(outcome.received, REVERSED, "{}", chain.name);
    }
}

/// Asserts the connection was refused before any application data.
fn assert_refused(outcome: &Outcome, what: &str) {
    assert!(outcome.error.is_some(), "{what}: accepted");
    assert!(outcome.handshaking, "{what}: the handshake completed");
    assert!(outcome.received.is_empty(), "{what}: data was received");
}

#[test]
fn a_certificate_signed_by_another_key_is_refused() {
    if !have_openssl() {
        return;
    }
    let dir = Scratch::new("other-signer");
    // Two roots with the same name; the client trusts the first, and the
    // server's certificate is signed by the second.
    make_root(&dir.0, "root", KeyKind::P256, "nowayos test root");
    make_root(&dir.0, "impostor", KeyKind::P256, "nowayos test root");
    make_leaf(&dir.0, "server", KeyKind::P256, "impostor", &["-sha256"]);
    let server = Server::start(&dir.0, "server", &[]);
    let config = client_config(
        provider_offering(TLS13_AES_128_GCM_SHA256, NamedGroup::X25519),
        &der(&dir.0, "root"),
    );
    let outcome = run_client(config, &mut server.connect());
    server.finish();
    assert_refused(&outcome, "a certificate signed by another key");
    assert!(
        matches!(
            outcome.error,
            Some(Error::InvalidCertificate(CertificateError::BadSignature))
        ),
        "{:?}",
        outcome.error
    );
}

#[test]
fn a_server_offering_only_tls12_is_refused() {
    if !have_openssl() {
        return;
    }
    let dir = Scratch::new("tls12");
    make_root(&dir.0, "root", KeyKind::P256, "nowayos test root");
    make_leaf(&dir.0, "server", KeyKind::P256, "root", &["-sha256"]);
    let server = Server::start(&dir.0, "server", &["-tls1_2"]);
    let config = client_config(
        provider_offering(TLS13_AES_128_GCM_SHA256, NamedGroup::X25519),
        &der(&dir.0, "root"),
    );
    let outcome = run_client(config, &mut server.connect());
    server.finish();
    assert_refused(&outcome, "a TLS 1.2-only server");
}

/// A test-only ECDSA P-256 signing key, for the in-process server below:
/// the provider itself never signs.
#[derive(Debug)]
struct TestSigner(p256::ecdsa::SigningKey);

impl SigningKey for TestSigner {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(TestSigner(self.0.clone())) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

impl Signer for TestSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        use p256::ecdsa::signature::Signer as _;
        let signature: p256::ecdsa::Signature = self.0.sign(message);
        Ok(signature.to_der().as_bytes().to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}

#[test]
fn a_certificate_verify_from_another_key_is_refused() {
    if !have_openssl() {
        return;
    }
    // OpenSSL will not serve a certificate with another key's signature, so
    // the server here is rustls, on this provider, with a test signer whose
    // key is not the certificate's.
    let dir = Scratch::new("other-key");
    make_root(&dir.0, "root", KeyKind::P256, "nowayos test root");
    make_leaf(&dir.0, "server", KeyKind::P256, "root", &["-sha256"]);
    let mut seed = [0u8; 32];
    RANDOM.fill(&mut seed).unwrap();
    let other = p256::ecdsa::SigningKey::from_slice(&seed).unwrap();
    let certified = CertifiedKey::new(
        vec![CertificateDer::from(der(&dir.0, "server"))],
        Arc::new(TestSigner(other)),
    );
    let server_config = Arc::new(
        ServerConfig::builder_with_provider(Arc::new(provider(&RANDOM)))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut conn = ServerConnection::new(server_config).unwrap();
        let mut application_data = Vec::new();
        while conn.complete_io(&mut socket).is_ok() {
            let _ = conn.reader().read_to_end(&mut application_data);
            if !conn.wants_read() {
                break;
            }
        }
        application_data
    });
    let config = client_config(
        provider_offering(TLS13_AES_128_GCM_SHA256, NamedGroup::X25519),
        &der(&dir.0, "root"),
    );
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let outcome = run_client(config, &mut socket);
    let server_received = server.join().unwrap();
    assert_refused(&outcome, "a CertificateVerify from another key");
    assert!(
        matches!(
            outcome.error,
            Some(Error::InvalidCertificate(CertificateError::BadSignature))
        ),
        "{:?}",
        outcome.error
    );
    assert!(
        server_received.is_empty(),
        "the server received application data"
    );
}

/// A relay between a client and a server that, once armed, flips one bit
/// in the next encrypted record the server sends.
fn relay(server_port: u16, armed: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (client, _) = listener.accept().unwrap();
        let server = TcpStream::connect(("127.0.0.1", server_port)).unwrap();
        let (mut client_in, mut server_out) =
            (client.try_clone().unwrap(), server.try_clone().unwrap());
        thread::spawn(move || {
            let _ = std::io::copy(&mut client_in, &mut server_out);
            let _ = server_out.shutdown(Shutdown::Write);
        });
        let (mut server_in, mut client_out) = (server, client);
        loop {
            let mut header = [0u8; 5];
            if server_in.read_exact(&mut header).is_err() {
                break;
            }
            let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
            let mut body = vec![0u8; len];
            if server_in.read_exact(&mut body).is_err() {
                break;
            }
            // Content type 23: an encrypted record. Flip the lowest bit of
            // its first byte of ciphertext, once.
            if header[0] == 23 && !body.is_empty() && armed.swap(false, Ordering::SeqCst) {
                body[0] ^= 1;
            }
            if client_out.write_all(&header).is_err() || client_out.write_all(&body).is_err() {
                break;
            }
        }
        let _ = client_out.shutdown(Shutdown::Both);
    });
    port
}

#[test]
fn one_flipped_bit_ends_the_connection_with_bad_record_mac() {
    if !have_openssl() {
        return;
    }
    let dir = Scratch::new("bitflip");
    make_root(&dir.0, "root", KeyKind::P256, "nowayos test root");
    make_leaf(&dir.0, "server", KeyKind::P256, "root", &["-sha256"]);
    let server = Server::start(&dir.0, "server", &["-msg"]);
    let armed = Arc::new(AtomicBool::new(false));
    let port = relay(server.port, Arc::clone(&armed));
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let config = client_config(
        provider_offering(TLS13_AES_128_GCM_SHA256, NamedGroup::X25519),
        &der(&dir.0, "root"),
    );
    let name = ServerName::try_from("localhost").unwrap();
    let mut conn = ClientConnection::new(config, name).unwrap();
    while conn.is_handshaking() {
        conn.complete_io(&mut socket).unwrap();
    }
    // The server's next record - its answer to this line - gets one bit
    // flipped on the way.
    armed.store(true, Ordering::SeqCst);
    conn.writer().write_all(b"flip me\n").unwrap();
    while conn.wants_write() {
        conn.write_tls(&mut socket).unwrap();
    }
    let mut received = Vec::new();
    let error = loop {
        if conn.read_tls(&mut socket).unwrap() == 0 {
            panic!("the connection ended before the flipped record arrived");
        }
        match conn.process_new_packets() {
            Ok(_) => {
                let _ = conn.reader().read_to_end(&mut received);
            }
            Err(e) => break e,
        }
    };
    assert_eq!(error, Error::DecryptError);
    assert!(received.is_empty(), "plaintext was returned: {received:?}");
    // rustls answers a record that fails to decrypt with bad_record_mac.
    assert!(conn.wants_write(), "no alert queued");
    while conn.wants_write() {
        conn.write_tls(&mut socket).unwrap();
    }
    assert!(conn.reader().read(&mut [0u8; 16]).is_err());
    let _ = socket.shutdown(Shutdown::Both);
    thread::sleep(Duration::from_millis(500));
    let printed = server.finish();
    // s_server -msg prints each record it receives; the client's alert is
    // the last.
    assert!(
        printed
            .iter()
            .any(|l| l.starts_with("<<<") && l.contains("Alert") && l.contains("bad_record_mac")),
        "OpenSSL reports receiving bad_record_mac: {printed:?}"
    );
}
