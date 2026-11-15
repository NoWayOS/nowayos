// SPDX-License-Identifier: MIT OR Apache-2.0
//! An application on `nowayos-ui`, running on the developer's machine, shown
//! in a browser window on that machine.
//!
//! A developer's tool, for the host only: nothing here goes on a device, and
//! no browser comes any nearer to one. A program gives [`Server::run`] its
//! [`Application`] - the same Rust that runs in its protection domains,
//! built for the host - and opens `http://localhost:8017/`. The page shows
//! each frame the application draws, and sends what is clicked and typed on
//! it back as canonical input events ([`nowayos_input::InputEvent`]), as an
//! input PD would.
//!
//! **Time** is the window's: milliseconds from [`START`], passing as the
//! clock on the wall does. The application is told the time with every
//! batch of input and about fifty times a second between.
//!
//! **A session** can be kept in a file ([`Options::session`]): every batch
//! of input with its time. A program started again with that file plays it
//! back first, so an application rebuilt after an edit comes back to the
//! screen it was on. *Start again* on the page forgets it.
//!
//! **A notice** is text shown above the screen: a build that failed, say.
//! Whatever restarts the program posts it (`POST /notice`, empty to clear).
//!
//! **Who may connect.** The window listens on the loopback address unless
//! told otherwise ([`Options::listen`]; in a container, every address of the
//! container, published to the host's loopback only). It answers only
//! requests that name this machine as their host, and changes nothing
//! without a header another site's page cannot send ([`http::OURS`]). All
//! of what it reads is bounded and refused if it is not exactly what the
//! page sends ([`http`], [`events`]). It has no encryption and no sign-in:
//! it is for one developer on one machine, showing synthetic data.
//!
//! Provisional.

pub mod bmp;
pub mod events;
pub mod http;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nowayos_input::InputEvent;
use nowayos_testkit::Framebuffer;

use http::{Method, Request, RequestError};

/// The time the window's clock starts at, in milliseconds.
pub const START: u64 = 1_000;
/// The port the window listens on unless told otherwise.
pub const PORT: u16 = 8017;

/// How long the application is left between being told the time.
const TICK: Duration = Duration::from_millis(20);
/// How long a page's question "has anything changed?" is held open.
const WAIT: Duration = Duration::from_secs(20);
/// How long a connection may take to send its request.
const SLOW: Duration = Duration::from_secs(5);
/// The most connections served at once.
const MAX_CONNECTIONS: usize = 32;
/// The largest session file played back, in bytes.
const MAX_SESSION: u64 = 4 * 1024 * 1024;
/// The longest status shown, in characters.
const MAX_STATUS: usize = 200;

/// The page, with `__TITLE__` where the window's title goes.
const PAGE: &str = include_str!("page.html");

/// An application the window can show: its UI and whatever it talks to,
/// joined as its protection domains join them.
pub trait Application {
    /// One batch of input at time `now`, as an input PD delivers it; the
    /// application then settles (the end of input, its messages passed).
    fn input(&mut self, events: &[InputEvent], now: u64);

    /// Time has reached `now`: every deadline up to it is met, in order.
    /// Returns whether what is shown may have changed.
    fn advance(&mut self, now: u64) -> bool;

    /// The screen as it is now.
    ///
    /// # Errors
    ///
    /// Why it could not be drawn; the window shows that in place of a new
    /// frame.
    fn draw(&mut self) -> Result<Framebuffer, String>;

    /// A word for where the application is - its screen's name - shown
    /// beside the window's title.
    fn status(&self) -> String {
        String::new()
    }
}

/// How a window is set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The page's title.
    pub title: String,
    /// The address to listen on: the loopback address and [`PORT`] unless
    /// changed.
    pub listen: SocketAddr,
    /// A file that keeps the session, played back at the start.
    pub session: Option<PathBuf>,
}

impl Options {
    /// A window with this title on `127.0.0.1:8017`, keeping no session.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            listen: SocketAddr::from(([127, 0, 0, 1], PORT)),
            session: None,
        }
    }

    /// The options a program's command line gives: `--listen ADDRESS:PORT`
    /// and `--session FILE`, each optional.
    ///
    /// # Errors
    ///
    /// What was wrong, and the usage.
    pub fn from_args(title: &str, args: impl Iterator<Item = String>) -> Result<Self, String> {
        const USAGE: &str = "usage: [--listen ADDRESS:PORT] [--session FILE]";
        let mut options = Self::new(title);
        let mut args = args;
        while let Some(arg) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))?;
            match arg.as_str() {
                "--listen" => {
                    options.listen = value.parse().map_err(|_| {
                        format!("--listen takes ADDRESS:PORT, not {value:?}\n{USAGE}")
                    })?;
                }
                "--session" => options.session = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown option {arg}\n{USAGE}")),
            }
        }
        Ok(options)
    }
}

/// What the page is shown, and told when it asks whether anything changed.
struct Shown {
    /// Which run of the program this is: new each time it starts.
    instance: u64,
    /// The frame's number, from 1.
    frame: u64,
    /// The notice's number.
    notice: u64,
    /// The frame, as a BMP file.
    picture: Arc<Vec<u8>>,
    /// The notice's text; empty for none.
    text: Arc<String>,
    /// The application's status.
    status: String,
}

/// What the window's threads share.
struct Shared {
    shown: Mutex<Shown>,
    changed: Condvar,
    connections: AtomicUsize,
    page: String,
}

/// What a connection asks of the thread that holds the application.
enum Command {
    Input(Vec<InputEvent>),
    Restart,
}

/// A lock that outlives a thread that panicked holding it: what it guards
/// is whole after every statement.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A window, listening.
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    options: Options,
}

impl Server {
    /// Starts listening.
    ///
    /// # Errors
    ///
    /// Whatever binding the address reports: it is in use, say.
    pub fn bind(options: Options) -> io::Result<Self> {
        let listener = TcpListener::bind(options.listen)?;
        Ok(Self { listener, options })
    }

    /// The address listened on: the port chosen, if the options gave 0.
    ///
    /// # Errors
    ///
    /// Whatever the socket reports.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Runs the application `start` makes, serving the page until the
    /// program is stopped. `start` is called again for *Start again*.
    ///
    /// # Errors
    ///
    /// `start`'s error, or a thread that could not be started.
    pub fn run<A: Application>(
        self,
        mut start: impl FnMut() -> Result<A, String>,
    ) -> io::Result<()> {
        let instance = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| u64::try_from(since.as_micros()).unwrap_or(1))
            .max(1);
        let shared = Arc::new(Shared {
            shown: Mutex::new(Shown {
                instance,
                frame: 0,
                notice: 0,
                picture: Arc::new(Vec::new()),
                text: Arc::new(String::new()),
                status: String::new(),
            }),
            changed: Condvar::new(),
            connections: AtomicUsize::new(0),
            page: PAGE.replace("__TITLE__", &escape(&self.options.title)),
        });
        let (commands, inbox) = mpsc::channel();
        {
            let shared = Arc::clone(&shared);
            let listener = self.listener;
            thread::Builder::new()
                .name("accept".to_owned())
                .spawn(move || accept(&listener, &shared, &commands))?;
        }

        let mut app = start().map_err(io::Error::other)?;
        let session = self.options.session;
        let mut offset = session
            .as_ref()
            .map_or(START, |path| replay(path, &mut app));
        let mut started = Instant::now();
        publish(&mut app, &shared);
        loop {
            let now = offset.saturating_add(millis(started.elapsed()));
            let mut command = match inbox.recv_timeout(TICK) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            let mut changed = app.advance(now);
            // Everything that has arrived, then one frame.
            while let Some(next) = command.take() {
                match next {
                    Command::Input(events) => {
                        app.input(&events, now);
                        if let Some(path) = &session {
                            record(path, now, &events);
                        }
                    }
                    Command::Restart => {
                        app = start().map_err(io::Error::other)?;
                        if let Some(path) = &session {
                            // Forgotten: an empty file plays nothing back.
                            let _ = File::create(path);
                        }
                        offset = START;
                        started = Instant::now();
                    }
                }
                changed = true;
                command = inbox.try_recv().ok();
            }
            if changed {
                publish(&mut app, &shared);
            }
        }
    }
}

/// A duration in whole milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Text made safe to put in the page.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Draws the application and, if the frame or its status is new, shows it.
fn publish<A: Application>(app: &mut A, shared: &Shared) {
    let (picture, status) = match app.draw() {
        Ok(frame) => match bmp::encode(&frame) {
            Some(picture) => (Some(picture), app.status()),
            None => (None, "not drawn: the frame has no pixels".to_owned()),
        },
        Err(why) => (None, format!("not drawn: {why}")),
    };
    // One line of printable text.
    let status: String = status
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(MAX_STATUS)
        .collect();
    let mut shown = lock(&shared.shown);
    let mut new = false;
    if let Some(picture) = picture
        && *shown.picture != picture
    {
        shown.picture = Arc::new(picture);
        new = true;
    }
    if shown.status != status {
        shown.status = status;
        new = true;
    }
    if new {
        shown.frame = shown.frame.wrapping_add(1);
        shared.changed.notify_all();
    }
}

/// Adds a batch to the session file. A file that cannot be written loses
/// the session, which is all it is for.
fn record(path: &PathBuf, now: u64, events: &[InputEvent]) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{now}\t{}", events::format_batch(events));
    }
}

/// Plays a session file back into a new application: each line a time and
/// a batch. A line that is not one, or that goes back in time, is passed
/// over. Returns the time the session reached.
fn replay<A: Application>(path: &PathBuf, app: &mut A) -> u64 {
    let mut reached = START;
    let small = fs::metadata(path).is_ok_and(|file| file.len() <= MAX_SESSION);
    let Some(text) = small.then(|| fs::read_to_string(path).ok()).flatten() else {
        return reached;
    };
    for line in text.lines() {
        let Some((time, batch)) = line.split_once('\t') else {
            continue;
        };
        let (Ok(time), Ok(events)) = (time.parse::<u64>(), events::parse_batch(batch)) else {
            continue;
        };
        if time < reached {
            continue;
        }
        reached = time;
        app.advance(time);
        app.input(&events, time);
    }
    reached
}

/// Takes connections, each served on a thread of its own, at most
/// [`MAX_CONNECTIONS`] at once.
fn accept(listener: &TcpListener, shared: &Arc<Shared>, commands: &Sender<Command>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
            let _ = http::respond(
                &mut stream,
                "503 Service Unavailable",
                "text/plain",
                b"busy\n",
            );
            continue;
        }
        let served = Arc::clone(shared);
        let commands = commands.clone();
        let spawned = thread::Builder::new()
            .name("serve".to_owned())
            .spawn(move || {
                // A connection that fails is the page's to make again.
                let _ = serve(&mut stream, &served, &commands);
                served.connections.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Reads a request's head from a connection, and whatever of its body came
/// with it.
fn read_head(stream: &mut TcpStream) -> io::Result<Result<(Request, Vec<u8>), RequestError>> {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = seen.windows(4).position(|window| window == b"\r\n\r\n") {
            let body = seen.split_off(end.saturating_add(4));
            seen.truncate(end);
            return Ok(http::parse_head(&seen).map(|request| (request, body)));
        }
        if seen.len() > http::MAX_HEAD {
            return Ok(Err(RequestError::TooLong));
        }
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Ok(Err(RequestError::Malformed));
        }
        seen.extend_from_slice(chunk.get(..count).unwrap_or_default());
    }
}

/// Reads the rest of a request's body.
fn read_body(stream: &mut TcpStream, mut body: Vec<u8>, length: usize) -> io::Result<Vec<u8>> {
    let mut chunk = [0u8; 1024];
    while body.len() < length {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        body.extend_from_slice(chunk.get(..count).unwrap_or_default());
    }
    body.truncate(length);
    Ok(body)
}

/// Serves one request.
fn serve(stream: &mut TcpStream, shared: &Shared, commands: &Sender<Command>) -> io::Result<()> {
    stream.set_read_timeout(Some(SLOW))?;
    stream.set_write_timeout(Some(WAIT))?;
    let plain = "text/plain; charset=utf-8";
    let (request, body) = match read_head(stream)? {
        Ok(read) => read,
        Err(RequestError::TooLong) => {
            return http::respond(stream, "413 Content Too Large", plain, b"too long\n");
        }
        Err(RequestError::Method) => {
            return http::respond(stream, "405 Method Not Allowed", plain, b"GET or POST\n");
        }
        Err(RequestError::Malformed) => {
            return http::respond(stream, "400 Bad Request", plain, b"not a request\n");
        }
    };
    if !request.local() {
        return http::respond(
            stream,
            "403 Forbidden",
            plain,
            b"this window answers only as localhost\n",
        );
    }
    match (request.method, request.path.as_str()) {
        (Method::Get, "/") => http::respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            shared.page.as_bytes(),
        ),
        (Method::Get, "/wait") => {
            let answer = wait(shared, &request);
            http::respond(stream, "200 OK", plain, answer.as_bytes())
        }
        (Method::Get, "/frame") => {
            let picture = Arc::clone(&lock(&shared.shown).picture);
            http::respond(stream, "200 OK", "image/bmp", &picture)
        }
        (Method::Get, "/notice") => {
            let text = Arc::clone(&lock(&shared.shown).text);
            http::respond(stream, "200 OK", plain, text.as_bytes())
        }
        (Method::Post, _) if !request.ours => http::respond(
            stream,
            "403 Forbidden",
            plain,
            b"not from this window's page\n",
        ),
        (Method::Post, "/input") => {
            let body = read_body(stream, body, request.length)?;
            let events = core::str::from_utf8(&body).ok().map(events::parse_batch);
            let Some(Ok(events)) = events else {
                return http::respond(stream, "400 Bad Request", plain, b"not input events\n");
            };
            // If the application's thread has gone, so has the window.
            let _ = commands.send(Command::Input(events));
            http::respond(stream, "204 No Content", plain, b"")
        }
        (Method::Post, "/restart") => {
            let _ = commands.send(Command::Restart);
            http::respond(stream, "204 No Content", plain, b"")
        }
        (Method::Post, "/notice") => {
            let body = read_body(stream, body, request.length)?;
            let text = String::from_utf8_lossy(&body).trim_end().to_owned();
            let mut shown = lock(&shared.shown);
            if *shown.text != text {
                shown.text = Arc::new(text);
                shown.notice = shown.notice.wrapping_add(1);
                shared.changed.notify_all();
            }
            drop(shown);
            http::respond(stream, "204 No Content", plain, b"")
        }
        _ => http::respond(stream, "404 Not Found", plain, b"no such page\n"),
    }
}

/// Holds the page's question - "I have this run's frame and notice: is
/// there anything newer?" - until there is, or for [`WAIT`], then says what
/// there is now.
fn wait(shared: &Shared, request: &Request) -> String {
    let has = (
        request.number("instance"),
        request.number("frame"),
        request.number("notice"),
    );
    let until = Instant::now().checked_add(WAIT);
    let mut shown = lock(&shared.shown);
    loop {
        let same = has == (Some(shown.instance), Some(shown.frame), Some(shown.notice));
        let left = until.and_then(|until| until.checked_duration_since(Instant::now()));
        let Some(left) = left.filter(|left| same && !left.is_zero()) else {
            break;
        };
        shown = shared
            .changed
            .wait_timeout(shown, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    format!(
        "instance {}\nframe {}\nnotice {}\nstatus {}\n",
        shown.instance, shown.frame, shown.notice, shown.status
    )
}
