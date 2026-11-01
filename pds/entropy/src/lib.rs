// SPDX-License-Identifier: MIT OR Apache-2.0
//! A protection domain's own random generator, and the `getrandom` back end
//! that reads it. Every
//! crate in the PD that calls `getrandom` - version 0.2, 0.3 or 0.4,
//! directly or through `rand_core` - gets bytes from that one generator, or
//! an error: never zeros, never another source.
//!
//! * The generator is ChaCha20 (RustCrypto's `ChaCha20Rng`), seeded with 256
//!   bits. After every request it re-keys itself from its own next 32 bytes
//!   (fast key erasure), so the state it keeps cannot reproduce anything it
//!   has handed out, and the replaced state is wiped.
//! * Until [`seed`] runs, and after [`forget`], every request is refused,
//!   and the back ends write nothing into the caller's buffer. A request
//!   over [`MAX_REQUEST`] bytes is refused, and so is every request after
//!   the [`RESEED_INTERVAL`]th since the last seed, until the next.
//! * The seed comes from the entropy PD; until that PD exists, a
//!   test instrument seeds it with a fixed, public test value
//!   (`systems/getrandom-test.system`).
//!
//! Every PD workspace builds with `--cfg getrandom_backend="custom"`
//! (`pds/.cargo/config.toml`), so getrandom 0.3 and 0.4 call this crate's
//! back end; getrandom 0.2 calls it through its `custom` feature. A build
//! with no back end does not compile, and one with the back end selected
//! but not linked does not link: link this crate into every PD that draws
//! randomness.
//!
//! For single-threaded protection domains only: the generator lives in a
//! static that assumes one thread (`cell`). Unsafe code is confined to two
//! modules: `cell`, which keeps the PD's one generator in that static, and
//! `backend`, the symbols `getrandom` calls (`tools/unsafe-allowlist.toml`;
//! Miri runs both, `make pds-miri`).

#![cfg_attr(not(test), no_std)]

#[allow(
    unsafe_code,
    reason = "a static that holds the PD's generator needs `unsafe impl Sync`"
)]
mod cell;
mod generator;

#[allow(
    unsafe_code,
    reason = "the back ends are #[no_mangle] functions over a raw pointer and a length, which getrandom calls"
)]
mod backend;

pub use generator::{Generator, MAX_REQUEST, RESEED_INTERVAL, Refusal, SEED_LEN};

/// The PD's one generator: none until [`seed`].
static GENERATOR: cell::PdCell<Option<Generator>> = cell::PdCell::new(None);

/// Seeds the PD's generator from `seed`, replacing (and wiping) any it had.
/// The seed is wiped too: it is used once.
///
/// # Errors
///
/// [`Refusal::Busy`] if called from inside the generator, which no PD does.
pub fn seed(seed: &mut [u8; SEED_LEN]) -> Result<(), Refusal> {
    GENERATOR
        .with(|slot| *slot = Some(Generator::from_seed(seed)))
        .ok_or(Refusal::Busy)
}

/// Drops the PD's generator, wiping its state: every request is refused
/// until the next [`seed`]. A PD's restart does the same, since its restart
/// image holds no generator.
///
/// # Errors
///
/// [`Refusal::Busy`] if called from inside the generator.
pub fn forget() -> Result<(), Refusal> {
    GENERATOR.with(|slot| *slot = None).ok_or(Refusal::Busy)
}

/// Whether a request for `len` bytes would be served now. The back ends ask
/// before they touch the caller's buffer.
///
/// # Errors
///
/// The [`Refusal`] the request would get.
pub fn ready_for(len: usize) -> Result<(), Refusal> {
    GENERATOR
        .with(|slot| {
            slot.as_ref().map_or(Err(Refusal::NotSeeded), |generator| {
                generator.ready_for(len)
            })
        })
        .unwrap_or(Err(Refusal::Busy))
}

/// Fills `dest` from the PD's generator; `dest` is untouched when refused.
///
/// # Errors
///
/// [`Refusal::NotSeeded`] before the first [`seed`] and after [`forget`];
/// otherwise as [`Generator::generate`].
pub fn fill(dest: &mut [u8]) -> Result<(), Refusal> {
    GENERATOR
        .with(|slot| {
            slot.as_mut().map_or(Err(Refusal::NotSeeded), |generator| {
                generator.generate(dest)
            })
        })
        .unwrap_or(Err(Refusal::Busy))
}

/// The tests that use the PD's one generator take this lock first, and
/// start from no generator: a PD has one thread, the test harness several.
#[cfg(test)]
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(forget(), Ok(()));
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SEED: [u8; SEED_LEN] = *b"nowayos-entropy test seed, 32 b.";

    #[test]
    fn refuses_before_the_first_seed_and_after_forget_writing_nothing() {
        let _only = exclusive();
        let mut buf = [0xa5u8; 16];
        assert_eq!(fill(&mut buf), Err(Refusal::NotSeeded));
        assert_eq!(ready_for(1), Err(Refusal::NotSeeded));
        assert_eq!(buf, [0xa5; 16]);
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        assert_eq!(fill(&mut buf), Ok(()));
        assert_eq!(forget(), Ok(()));
        let before = buf;
        assert_eq!(fill(&mut buf), Err(Refusal::NotSeeded));
        assert_eq!(buf, before);
    }

    #[test]
    fn seeding_wipes_the_seed_and_serves_what_a_generator_of_that_seed_gives() {
        let _only = exclusive();
        let mut once = TEST_SEED;
        assert_eq!(seed(&mut once), Ok(()));
        assert_eq!(once, [0; SEED_LEN]);
        let mut reference = Generator::from_seed(&mut TEST_SEED.clone());
        for len in [1, 32, 64, 1000] {
            let (mut got, mut want) = (vec![0u8; len], vec![0u8; len]);
            assert_eq!(fill(&mut got), Ok(()));
            assert_eq!(reference.generate(&mut want), Ok(()));
            assert_eq!(got, want);
        }
    }

    #[test]
    fn a_fresh_seed_replaces_the_generator() {
        let _only = exclusive();
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        let mut first = [0u8; 32];
        assert_eq!(fill(&mut first), Ok(()));
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        let mut again = [0u8; 32];
        assert_eq!(fill(&mut again), Ok(()));
        assert_eq!(first, again, "a second seed starts a generator afresh");
    }

    #[test]
    fn a_request_from_inside_the_generator_is_refused() {
        let _only = exclusive();
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        let inner = GENERATOR.with(|_| {
            let mut buf = [0xa5u8; 4];
            (
                fill(&mut buf),
                ready_for(4),
                seed(&mut [1; SEED_LEN]),
                forget(),
                buf,
            )
        });
        assert_eq!(
            inner,
            Some((
                Err(Refusal::Busy),
                Err(Refusal::Busy),
                Err(Refusal::Busy),
                Err(Refusal::Busy),
                [0xa5; 4]
            ))
        );
        assert_eq!(
            fill(&mut [0u8; 4]),
            Ok(()),
            "and the generator is free again"
        );
    }
}
