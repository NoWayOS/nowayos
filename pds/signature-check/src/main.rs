// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-signature-check`: a test instrument, in
//! `systems/signature-test.system`.
//!
//! It checks, with `nowayos-signature`, the signature OpenSSL made once over
//! a message, its private key discarded (`nowayos_signature::known_answer`);
//! then the same with one bit changed in the message, in the signature and
//! in the public key, and a signature whose r is zero. It prints each
//! result, and how long each check took on the counter. The times vary from
//! run to run, so the expect file checks only that the slowest was under a
//! second.
//!
//! No heap - no global allocator is declared, so a crate in its tree that
//! used `alloc` would fail to link - and no randomness: it links no
//! `nowayos-entropy`, and `pds/deny.toml` admits `getrandom` only under it. No `unsafe` of its own; it reads the counter through
//! `nowayos-hw`. A test instrument: never in a demo or product system.

#![no_std]
#![no_main]

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_signature::{PUBLIC_KEY_LEN, SIGNATURE_LEN, check, known_answer};
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The most the slowest check may take, in microseconds, for the expect
/// file's last line: generous, as QEMU emulates the CPU.
const BOUND_US: u64 = 1_000_000;

/// The message's length.
const MESSAGE_LEN: usize = known_answer::MESSAGE.len();

/// The bytes whose lowest bit is changed: the message's first, and the last
/// of the signature (s) and of the public key (y).
const MESSAGE_BYTE: usize = 0;
const SIGNATURE_BYTE: usize = SIGNATURE_LEN - 1;
const KEY_BYTE: usize = PUBLIC_KEY_LEN - 1;

#[protection_domain]
fn init() -> SignatureCheckPd {
    debug_println!("NOWAYOS: signature-check: starting");
    let key = known_answer::PUBLIC_KEY;
    let signature = known_answer::SIGNATURE;
    let mut message = [0u8; MESSAGE_LEN];
    message.copy_from_slice(known_answer::MESSAGE);

    let mut changed_message = message;
    flip(&mut changed_message, MESSAGE_BYTE);
    let mut changed_signature = signature;
    flip(&mut changed_signature, SIGNATURE_BYTE);
    let mut changed_key = key;
    flip(&mut changed_key, KEY_BYTE);
    let mut zero_r = signature;
    if let Some(r) = zero_r.get_mut(..32) {
        r.fill(0);
    }

    let mut slowest = 0;
    for (name, key, message, signature) in [
        ("OpenSSL's signature", &key, &message, &signature),
        (
            "one bit of the message changed",
            &key,
            &changed_message,
            &signature,
        ),
        (
            "one bit of the signature changed",
            &key,
            &message,
            &changed_signature,
        ),
        (
            "one bit of the public key changed",
            &changed_key,
            &message,
            &signature,
        ),
        ("r of zero", &key, &message, &zero_r),
    ] {
        let start = counter();
        let result = check(key, message, signature);
        let took = micros(counter().saturating_sub(start));
        match result {
            Ok(()) => debug_println!("NOWAYOS: signature-check: {name}: accepted"),
            Err(error) => debug_println!("NOWAYOS: signature-check: {name}: refused: {error}"),
        }
        debug_println!("NOWAYOS: signature-check: {name}: took {took} us");
        slowest = slowest.max(took);
    }
    if slowest < BOUND_US {
        debug_println!("NOWAYOS: signature-check: every check took under 1 s");
    } else {
        debug_println!("NOWAYOS: signature-check: the slowest check took {slowest} us, over 1 s");
    }
    debug_println!("NOWAYOS: signature-check: done");
    SignatureCheckPd
}

/// Changes the lowest bit of byte `at`.
fn flip(bytes: &mut [u8], at: usize) {
    if let Some(byte) = bytes.get_mut(at) {
        *byte ^= 1;
    }
}

/// Counter ticks as whole microseconds, saturating.
fn micros(ticks: u64) -> u64 {
    u128::from(ticks)
        .checked_mul(1_000_000)
        .and_then(|scaled| scaled.checked_div(u128::from(counter_frequency())))
        .and_then(|micros| u64::try_from(micros).ok())
        .unwrap_or(u64::MAX)
}

/// The PD after its checks: it has no channels, so nothing more to do.
struct SignatureCheckPd;

impl Handler for SignatureCheckPd {
    type Error = Infallible;
}
