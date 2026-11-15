// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: what the dev window reads from a browser
//! (`nowayos-dev-window`, a developer's tool on the host) - a request's
//! head, and a batch of input events as text.
//!
//! The fuzzer's bytes are read both ways. Checks: nothing panics; a head
//! that is read declares no body longer than the limit, and a head over the
//! limit is never read; a batch that is read holds no more events than the
//! limit, and written out again reads back as the same events.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_dev_window::events::{MAX_BATCH, format_batch, parse_batch};
use nowayos_dev_window::http::{MAX_BODY, MAX_HEAD, parse_head};

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = parse_head(data) {
        assert!(data.len() <= MAX_HEAD);
        assert!(request.length <= MAX_BODY);
        assert!(request.path.starts_with('/'));
        let _ = request.local();
        let _ = request.number("frame");
    }
    if let Ok(text) = core::str::from_utf8(data)
        && let Ok(events) = parse_batch(text)
    {
        assert!(events.len() <= MAX_BATCH);
        assert_eq!(parse_batch(&format_batch(&events)), Ok(events));
    }
});
