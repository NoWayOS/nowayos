// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window end to end, over real connections on the loopback address:
//! the page is served; input sent as the page sends it reaches the
//! application as canonical events and a new frame follows; only this
//! machine's own page may send it; a session kept in a file is played back
//! into a program started again, and forgotten by *Start again*; a notice
//! posted is shown; and time passes for the application without any input.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed step fails the test"
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::thread;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_dev_window::{Application, Options, START, Server};
use nowayos_input::{InputEvent, KeyCode};
use nowayos_testkit::Framebuffer;

/// Counts the Enter keys pressed, and shows the count as a shade of red;
/// half a second after it starts, it says it woke.
struct Counter {
    presses: u8,
    woke: bool,
}

impl Application for Counter {
    fn input(&mut self, events: &[InputEvent], _now: u64) {
        for event in events {
            if *event == InputEvent::KeyDown(KeyCode::ENTER) {
                self.presses += 1;
            }
        }
    }

    fn advance(&mut self, now: u64) -> bool {
        let woke = now >= START + 500;
        let changed = woke != self.woke;
        self.woke = woke;
        changed
    }

    fn draw(&mut self) -> Result<Framebuffer, String> {
        Framebuffer::filled(4, 2, Rgb888::new(self.presses, 0, 0)).map_err(|err| err.to_string())
    }

    fn status(&self) -> String {
        format!(
            "{} pressed{}",
            self.presses,
            if self.woke { ", woke" } else { "" }
        )
    }
}

/// A window on a port of its own, running a counter.
fn window(session: Option<PathBuf>) -> SocketAddr {
    let mut options = Options::new("A <test> window");
    options.listen = SocketAddr::from(([127, 0, 0, 1], 0));
    options.session = session;
    let server = Server::bind(options).unwrap();
    let address = server.local_addr().unwrap();
    thread::spawn(move || {
        server
            .run(|| {
                Ok(Counter {
                    presses: 0,
                    woke: false,
                })
            })
            .unwrap();
    });
    address
}

/// Sends one request; returns the response's status line and its body.
fn ask(address: SocketAddr, request: &str) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let end = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(answer[..end].to_vec()).unwrap();
    (
        head.lines().next().unwrap().to_owned(),
        answer[end + 4..].to_vec(),
    )
}

fn get(address: SocketAddr, path: &str) -> (String, Vec<u8>) {
    ask(
        address,
        &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
}

fn post(address: SocketAddr, path: &str, body: &str) -> String {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nX-NoWayOS-Dev: 1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    ask(address, &request).0
}

/// What `/wait` says now: the instance, the frame's and the notice's
/// numbers, and the status.
fn state(address: SocketAddr, query: &str) -> (u64, u64, u64, String) {
    let (status, body) = get(address, &format!("/wait{query}"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    let text = String::from_utf8(body).unwrap();
    let value = |name: &str| {
        let line = text.lines().find(|line| line.starts_with(name)).unwrap();
        line[name.len()..].trim().to_owned()
    };
    (
        value("instance").parse().unwrap(),
        value("frame").parse().unwrap(),
        value("notice").parse().unwrap(),
        value("status"),
    )
}

/// Waits for the status to become `wanted`, asking as the page does.
fn until(address: SocketAddr, wanted: &str) -> (u64, u64, u64, String) {
    let mut now = state(address, "");
    for _ in 0..50 {
        if now.3 == wanted {
            return now;
        }
        let query = format!("?instance={}&frame={}&notice={}", now.0, now.1, now.2);
        now = state(address, &query);
    }
    panic!("the status never became {wanted:?}: {now:?}");
}

/// The red of the frame's first pixel.
fn red(address: SocketAddr) -> u8 {
    let (status, picture) = get(address, "/frame");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(&picture[..2], b"BM");
    // Blue, green, red, after the 54 bytes of headers.
    picture[56]
}

#[test]
fn the_page_is_served_with_its_title_made_safe() {
    let address = window(None);
    let (status, body) = get(address, "/");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains("<title>A &lt;test&gt; window</title>"));
    assert!(!page.contains("__TITLE__"));
    assert_eq!(get(address, "/nothing").0, "HTTP/1.1 404 Not Found");
}

#[test]
fn input_from_the_page_reaches_the_application_and_a_frame_follows() {
    let address = window(None);
    let first = until(address, "0 pressed");
    assert_eq!(red(address), 0);
    assert_eq!(
        post(address, "/input", "kd 40\nku 40\nkd 40;ku 40"),
        "HTTP/1.1 204 No Content"
    );
    let next = until(address, "2 pressed");
    assert_eq!(next.0, first.0, "the same run");
    assert!(next.1 > first.1, "a new frame");
    assert_eq!(red(address), 2);
    // What is not input is refused whole.
    assert_eq!(
        post(address, "/input", "kd 40;launch"),
        "HTTP/1.1 400 Bad Request"
    );
    assert_eq!(state(address, "").3, "2 pressed");
}

#[test]
fn only_this_machines_own_page_may_ask_or_send() {
    let address = window(None);
    until(address, "0 pressed");
    // Another name for this machine: refused, whatever it asks.
    let (status, _) = ask(
        address,
        "GET / HTTP/1.1\r\nHost: window.example.com\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    // Without the page's header: refused, and nothing is pressed.
    let request = "POST /input HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nkd 40";
    assert_eq!(ask(address, request).0, "HTTP/1.1 403 Forbidden");
    assert_eq!(
        ask(address, "PUT / HTTP/1.1\r\nHost: localhost\r\n\r\n").0,
        "HTTP/1.1 405 Method Not Allowed"
    );
    assert_eq!(
        ask(address, "nonsense\r\n\r\n").0,
        "HTTP/1.1 400 Bad Request"
    );
    let long = format!(
        "GET /{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
        "a".repeat(9000)
    );
    assert_eq!(ask(address, &long).0, "HTTP/1.1 413 Content Too Large");
    assert_eq!(state(address, "").3, "0 pressed");
}

#[test]
fn a_session_is_played_back_into_a_program_started_again_and_forgotten_on_start_again() {
    let file =
        std::env::temp_dir().join(format!("nowayos-dev-window-{}.session", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let first = window(Some(file.clone()));
    until(first, "0 pressed");
    post(first, "/input", "kd 40;ku 40");
    post(first, "/input", "kd 40;ku 40");
    post(first, "/input", "kd 40;ku 40");
    until(first, "3 pressed");
    assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 3);

    // The program again, as after a rebuild: where it was, and a new run.
    let second = window(Some(file.clone()));
    let now = state(second, "");
    assert_eq!(now.3, "3 pressed");
    assert_ne!(now.0, state(first, "").0);

    assert_eq!(post(second, "/restart", ""), "HTTP/1.1 204 No Content");
    until(second, "0 pressed");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
    let _ = std::fs::remove_file(&file);
}

#[test]
fn a_notice_posted_is_shown_and_cleared() {
    let address = window(None);
    let before = until(address, "0 pressed");
    assert_eq!(get(address, "/notice").1, b"");
    assert_eq!(
        post(address, "/notice", "error: it did not build\n"),
        "HTTP/1.1 204 No Content"
    );
    let after = state(address, "");
    assert_eq!(after.2, before.2 + 1);
    assert_eq!(get(address, "/notice").1, b"error: it did not build");
    post(address, "/notice", "");
    assert_eq!(get(address, "/notice").1, b"");
}

#[test]
fn time_passes_for_the_application_with_no_input() {
    let address = window(None);
    until(address, "0 pressed");
    until(address, "0 pressed, woke");
}
