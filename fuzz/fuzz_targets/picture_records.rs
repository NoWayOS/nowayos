// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the picture decoder PD's records (`nowayos-picture-wire`) -
//! any bytes decoded as a `Decode` or a `Decoded`.
//!
//! Checks: nothing panics; whatever decodes re-encodes to the bytes it came
//! from (`check_decode`); a request's length is within the encoded region,
//! and a picture an answer names fits the pixel region.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::check_decode;
use nowayos_picture_wire::{Decode, Decoded, MAX_ENCODED, PIXEL_BYTES, pixel_len};

fuzz_target!(|data: &[u8]| {
    if let Some(request) = check_decode::<Decode>(data) {
        assert!(request.len > 0 && request.len <= MAX_ENCODED);
    }
    if let Some(answer) = check_decode::<Decoded>(data)
        && let Ok((width, height)) = answer.outcome
    {
        assert!(pixel_len(width, height) <= PIXEL_BYTES);
    }
});
