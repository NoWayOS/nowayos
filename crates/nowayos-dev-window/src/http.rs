// SPDX-License-Identifier: MIT OR Apache-2.0
//! As much of HTTP/1.1 as the page needs: a request's head read into a
//! [`Request`], and a response written.
//!
//! The head is bounded before it is parsed ([`MAX_HEAD`]), a body by its
//! declared length ([`MAX_BODY`]); anything else is refused with a typed
//! error, never a panic. Every response closes its connection.

use std::io::{self, Write};

/// The longest request head, in bytes.
pub const MAX_HEAD: usize = 8 * 1024;
/// The longest request body, in bytes.
pub const MAX_BODY: usize = 16 * 1024;

/// The header a request that changes anything must carry. A page on another
/// site cannot send it without asking first, and is never told yes.
pub const OURS: &str = "x-nowayos-dev";

/// A request's method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
}

/// A request's head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Its method.
    pub method: Method,
    /// Its path, before any `?`.
    pub path: String,
    /// What follows the `?`, or nothing.
    pub query: String,
    /// The `Host` header's name or address, without its port and in lower
    /// case; empty if there was none.
    pub host: String,
    /// The body's length, from `Content-Length`; 0 without one.
    pub length: usize,
    /// Whether it carries the [`OURS`] header.
    pub ours: bool,
}

/// Why a request's head was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The head is longer than [`MAX_HEAD`] or the body than [`MAX_BODY`].
    TooLong,
    /// Not a request this server reads.
    Malformed,
    /// A method other than `GET` and `POST`.
    Method,
}

impl Request {
    /// Whether the request names this machine as its host: `localhost`,
    /// `127.0.0.1` or `[::1]`. A page elsewhere that points a name of its
    /// own at this machine is refused by it.
    #[must_use]
    pub fn local(&self) -> bool {
        matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    }

    /// The number a query gives `name`, as in `after=3&instance=7`.
    #[must_use]
    pub fn number(&self, name: &str) -> Option<u64> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            if key == name {
                value.parse().ok()
            } else {
                None
            }
        })
    }
}

/// Reads a request's head: everything before the blank line.
///
/// # Errors
///
/// A [`RequestError`] if it is too long or is not a request this server
/// reads.
pub fn parse_head(head: &[u8]) -> Result<Request, RequestError> {
    if head.len() > MAX_HEAD {
        return Err(RequestError::TooLong);
    }
    let text = core::str::from_utf8(head).map_err(|_| RequestError::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or(RequestError::Malformed)?.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (first.next(), first.next(), first.next(), first.next())
    else {
        return Err(RequestError::Malformed);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(RequestError::Malformed);
    }
    let method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return Err(RequestError::Method),
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        host: String::new(),
        length: 0,
        ours: false,
    };
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(RequestError::Malformed)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            request.host = host_name(value).to_ascii_lowercase();
        } else if name.eq_ignore_ascii_case("content-length") {
            let length: usize = value.parse().map_err(|_| RequestError::Malformed)?;
            if length > MAX_BODY {
                return Err(RequestError::TooLong);
            }
            request.length = length;
        } else if name.eq_ignore_ascii_case(OURS) {
            request.ours = true;
        }
    }
    Ok(request)
}

/// A `Host` header's value without its port.
fn host_name(value: &str) -> &str {
    if value.starts_with('[') {
        // An IPv6 address in brackets, then perhaps a port.
        return value.split_inclusive(']').next().unwrap_or(value);
    }
    value.split(':').next().unwrap_or(value)
}

/// Writes a whole response and asks for the connection to be closed.
///
/// # Errors
///
/// Whatever writing to `to` reports.
pub fn respond(to: &mut impl Write, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    write!(
        to,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    to.write_all(body)?;
    to.flush()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed step fails the test"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_get_with_a_query_reads() {
        let request = parse_head(
            b"GET /wait?frame=3&instance=17 HTTP/1.1\r\nHost: LocalHost:8017\r\nAccept: */*",
        )
        .unwrap();
        assert_eq!(request.method, Method::Get);
        assert_eq!(request.path, "/wait");
        assert_eq!(request.number("frame"), Some(3));
        assert_eq!(request.number("instance"), Some(17));
        assert_eq!(request.number("notice"), None);
        assert!(request.local());
        assert!(!request.ours);
        assert_eq!(request.length, 0);
    }

    #[test]
    fn a_post_carries_its_length_and_our_header() {
        let request = parse_head(
            b"POST /input HTTP/1.1\r\nhost: 127.0.0.1:8017\r\nContent-Length: 12\r\nX-NoWayOS-Dev: 1",
        )
        .unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.length, 12);
        assert!(request.ours && request.local());
    }

    #[test]
    fn only_this_machine_is_local() {
        for (host, local) in [
            ("localhost", true),
            ("localhost:1", true),
            ("[::1]:8017", true),
            ("127.0.0.1", true),
            ("example.com", false),
            ("localhost.example.com", false),
            ("127.0.0.1.example.com:8017", false),
            ("", false),
        ] {
            let head = format!("GET / HTTP/1.1\r\nHost: {host}");
            assert_eq!(
                parse_head(head.as_bytes()).unwrap().local(),
                local,
                "{host}"
            );
        }
        assert!(!parse_head(b"GET / HTTP/1.1").unwrap().local());
    }

    #[test]
    fn what_is_not_a_request_is_refused() {
        assert_eq!(parse_head(b""), Err(RequestError::Malformed));
        assert_eq!(parse_head(b"GET /"), Err(RequestError::Malformed));
        assert_eq!(
            parse_head(b"GET / HTTP/1.1 x"),
            Err(RequestError::Malformed)
        );
        assert_eq!(parse_head(b"GET x HTTP/1.1"), Err(RequestError::Malformed));
        assert_eq!(parse_head(b"GET / SPDY/3"), Err(RequestError::Malformed));
        assert_eq!(parse_head(b"PUT / HTTP/1.1"), Err(RequestError::Method));
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nno colon"),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\n\xff: 1"),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nContent-Length: many"),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nContent-Length: 16385"),
            Err(RequestError::TooLong)
        );
        assert_eq!(
            parse_head(&vec![b'a'; MAX_HEAD + 1]),
            Err(RequestError::TooLong)
        );
    }

    #[test]
    fn a_response_says_its_length_and_closes() {
        let mut out = Vec::new();
        respond(&mut out, "200 OK", "text/plain", b"hello").unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));
    }

    proptest! {
        #[test]
        fn any_bytes_are_read_or_refused(head in proptest::collection::vec(any::<u8>(), 0..600)) {
            let _ = parse_head(&head);
        }

        #[test]
        fn any_request_shaped_text_is_read_or_refused(
            method in "[A-Z]{0,5}", target in "[ -~]{0,40}", headers in "([ -~]{0,30}\r\n){0,6}",
        ) {
            let head = format!("{method} {target} HTTP/1.1\r\n{headers}");
            if let Ok(request) = parse_head(head.as_bytes()) {
                prop_assert!(request.length <= MAX_BODY);
                let _ = request.local();
                let _ = request.number("frame");
            }
        }
    }
}
