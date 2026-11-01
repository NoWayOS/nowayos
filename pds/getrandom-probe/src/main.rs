// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-getrandom-probe`: a test instrument, in
//! `systems/getrandom-test.system`.
//!
//! It shows, in a PD built for `aarch64-sel4` with
//! `--cfg getrandom_backend="custom"`, that every way into `getrandom` -
//! versions 0.2, 0.3 and 0.4, `rand_core` 0.9's `OsRng` and getrandom 0.4's
//! `SysRng` - reaches `nowayos-entropy`'s one generator: each is refused
//! before the PD seeds it (0.3 and 0.4, and the generators on them, writing
//! nothing), each draws from it, in turn, once seeded, and each is refused
//! again once the generator is dropped, as a restart drops it. Each check
//! prints one line, `ok` or `FAILED`, which the expect file forbids.
//!
//! The seed is [`TEST_SEED`], a fixed value for this test only, and public;
//! a PD's seed comes from the entropy PD, which does not exist
//! yet. The draws themselves are never printed. A test instrument: never in
//! a demo or product system.

#![no_std]
#![no_main]

use getrandom04::rand_core::TryRng as _;
use nowayos_entropy::{Generator, Refusal, SEED_LEN};
use rand_core09::TryRngCore as _;
use sel4_microkit::{Handler, Infallible, debug_println, protection_domain};

/// The seed for this test only, and public: in a PD, a seed from the
/// entropy PD.
const TEST_SEED: [u8; SEED_LEN] = *b"nowayos getrandom-test seed, 32b";

/// What a buffer holds before a request that must be refused: a refused
/// request must leave it so (getrandom 0.2 aside, which zeroes it first).
const UNTOUCHED: [u8; 32] = [0xa5; 32];

#[protection_domain]
fn init() -> ProbePd {
    debug_println!("NOWAYOS: getrandom-probe: starting");
    let mut failed = refused("not seeded");
    failed = failed.saturating_add(seeded());
    let dropped = nowayos_entropy::forget().is_ok();
    failed = failed.saturating_add(report(
        dropped,
        "dropped the generator, as a restart drops it",
    ));
    failed = failed.saturating_add(refused("after the drop"));
    if failed == 0 {
        debug_println!("NOWAYOS: getrandom-probe: every check passed");
    } else {
        debug_println!("NOWAYOS: getrandom-probe: {failed} checks FAILED");
    }
    ProbePd
}

struct ProbePd;

impl Handler for ProbePd {
    type Error = Infallible;
}

/// Prints one check's result; 1 if it failed.
fn report(ok: bool, what: &str) -> u32 {
    if ok {
        debug_println!("NOWAYOS: getrandom-probe: ok: {what}");
        0
    } else {
        debug_println!("NOWAYOS: getrandom-probe: FAILED: {what}");
        1
    }
}

/// Every way in, refused with `nowayos-entropy`'s "not seeded" error, as
/// before the first seed and after the generator is dropped (`when`); the
/// number of checks that failed.
fn refused(when: &str) -> u32 {
    let not_seeded = Refusal::NotSeeded.code();
    let v02_code = getrandom02::Error::CUSTOM_START.saturating_add(u32::from(not_seeded));
    let v03_error = getrandom03::Error::new_custom(not_seeded);
    let v04_error = getrandom04::Error::new_custom(not_seeded);
    let mut failed = 0u32;

    let mut buf = UNTOUCHED;
    let result = getrandom02::getrandom(&mut buf);
    let ok = matches!(result, Err(error) if error.code().get() == v02_code) && buf == [0; 32];
    debug_println!("NOWAYOS: getrandom-probe: {when}:");
    failed = failed.saturating_add(report(
        ok,
        "getrandom 0.2.17 refused with the back end's error (it zeroes the buffer itself first)",
    ));

    let mut buf = UNTOUCHED;
    let ok = getrandom03::fill(&mut buf) == Err(v03_error) && buf == UNTOUCHED;
    failed = failed.saturating_add(report(ok, "getrandom 0.3.4 refused, nothing written"));

    let mut buf = UNTOUCHED;
    let ok = getrandom04::fill(&mut buf) == Err(v04_error) && buf == UNTOUCHED;
    failed = failed.saturating_add(report(
        ok,
        "getrandom 0.4.3 refused with the same error, made as 0.3's and received as 0.4's, nothing written",
    ));

    let mut buf = UNTOUCHED;
    let ok = rand_core09::OsRng.try_fill_bytes(&mut buf).is_err() && buf == UNTOUCHED;
    failed = failed.saturating_add(report(
        ok,
        "rand_core 0.9.5's OsRng refused, nothing written",
    ));

    let mut buf = UNTOUCHED;
    let ok = getrandom04::SysRng.try_fill_bytes(&mut buf) == Err(v04_error) && buf == UNTOUCHED;
    failed = failed.saturating_add(report(
        ok,
        "getrandom 0.4.3's SysRng refused, nothing written",
    ));
    failed
}

/// Seeds the PD's generator, then draws through every way in and checks
/// each draw against a second generator with the same seed: the number of
/// checks that failed.
fn seeded() -> u32 {
    let mut failed = 0u32;
    let mut seed = TEST_SEED;
    let ok = nowayos_entropy::seed(&mut seed).is_ok() && seed == [0; SEED_LEN];
    failed = failed.saturating_add(report(
        ok,
        "seeded the generator with the test seed, which was wiped",
    ));

    let mut draws = [[0u8; 32]; 5];
    let [a, b, c, d, e] = &mut draws;
    let results = [
        getrandom02::getrandom(a).is_ok(),
        getrandom03::fill(b).is_ok(),
        getrandom04::fill(c).is_ok(),
        rand_core09::OsRng.try_fill_bytes(d).is_ok(),
        getrandom04::SysRng.try_fill_bytes(e).is_ok(),
    ];

    // The same five requests to a generator of the same seed: what the PD's
    // one generator must have given, request by request.
    let mut reference = Generator::from_seed(&mut TEST_SEED.clone());
    let mut expected = [[0u8; 32]; 5];
    let mut reference_ok = true;
    for draw in &mut expected {
        reference_ok &= reference.generate(draw).is_ok();
    }
    let names = [
        "getrandom 0.2.17 drew request 1 from the one generator",
        "getrandom 0.3.4 drew request 2 from it",
        "getrandom 0.4.3 drew request 3 from it",
        "rand_core 0.9.5's OsRng drew request 4 from it",
        "getrandom 0.4.3's SysRng drew request 5 from it",
    ];
    for (((draw, want), ok), name) in draws.iter().zip(&expected).zip(results).zip(names) {
        failed = failed.saturating_add(report(reference_ok && ok && draw == want, name));
    }
    let distinct = draws
        .iter()
        .enumerate()
        .all(|(i, x)| *x != [0u8; 32] && draws.iter().skip(i.saturating_add(1)).all(|y| x != y));
    failed.saturating_add(report(
        distinct,
        "the five draws differ from each other and from zeros",
    ))
}
