// SPDX-License-Identifier: MIT OR Apache-2.0
//! The getrandom back ends: one function, three ways in.
//!
//! * getrandom 0.2.17, with its `custom` feature, calls `__getrandom_custom`
//!   on a target it does not support itself (target_os "none"). Its
//!   `register_custom_getrandom!` macro defines that symbol around a safe
//!   function. getrandom 0.2 zeroes the caller's buffer before it calls.
//!   The macro writes the `#[no_mangle]` function itself, and the
//!   `unsafe_code` lint does not look inside another crate's macro: a crate
//!   that forbids unsafe code can still register one, which is
//!   why the entropy design adds a source check and a link check.
//! * getrandom 0.3.4 and 0.4.3, built with `--cfg getrandom_backend="custom"`,
//!   both call `__getrandom_v03_custom`: the same name in both
//!   (`src/backends/custom.rs` in each), so one definition serves both. Each
//!   declares it as returning its own `Error`. The two types have the same
//!   shape - one `NonZeroI32` field - but neither is `#[repr(transparent)]`,
//!   so sharing the symbol rests on a layout Rust does not promise. The tests
//!   check it: an error made as 0.3's must reach a 0.4 caller with its code
//!   intact. Run them again for every getrandom upgrade and toolchain change.

getrandom02::register_custom_getrandom!(fill_v02);

/// getrandom 0.2's way in.
fn fill_v02(dest: &mut [u8]) -> Result<(), getrandom02::Error> {
    crate::fill(dest).map_err(|refusal| {
        let code = getrandom02::Error::CUSTOM_START.saturating_add(u32::from(refusal.code()));
        core::num::NonZeroU32::new(code)
            .map_or(getrandom02::Error::UNEXPECTED, getrandom02::Error::from)
    })
}

/// getrandom 0.3's and 0.4's way in.
///
/// # Safety
///
/// `dest` must be valid for writes of `len` bytes, and nothing else may use
/// them during the call. getrandom passes its caller's buffer, which may be
/// uninitialised.
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom03::Error> {
    let refused = |refusal: crate::Refusal| getrandom03::Error::new_custom(refusal.code());
    // Refuse before touching the buffer: a refused request writes nothing.
    crate::ready_for(len).map_err(refused)?;
    // SAFETY: getrandom passes a pointer valid for writes of `len` bytes
    // (this function's contract); this initialises every one of them.
    unsafe { core::ptr::write_bytes(dest, 0, len) };
    // SAFETY: the same `len` bytes, valid for writes and now initialised, and
    // used by nothing else during the call (this function's contract), so
    // they may be viewed as a `&mut [u8]` for the generator to fill.
    let buf = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    crate::fill(buf).map_err(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Generator, Refusal, SEED_LEN, exclusive, seed};

    const TEST_SEED: [u8; SEED_LEN] = *b"nowayos-entropy test seed, 32 b.";

    /// getrandom 0.3's way in, as getrandom calls it.
    fn v03(buf: &mut [u8]) -> Result<(), getrandom03::Error> {
        // SAFETY: `buf` is valid for writes of its length, and only the call
        // uses it.
        unsafe { __getrandom_v03_custom(buf.as_mut_ptr(), buf.len()) }
    }

    #[test]
    fn v03_refuses_before_the_seed_writing_nothing() {
        let _only = exclusive();
        let mut buf = [0xa5u8; 16];
        assert_eq!(
            v03(&mut buf),
            Err(getrandom03::Error::new_custom(Refusal::NotSeeded.code()))
        );
        assert_eq!(buf, [0xa5; 16]);
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        let mut over = vec![0xa5u8; crate::MAX_REQUEST.saturating_add(1)];
        assert_eq!(
            v03(&mut over),
            Err(getrandom03::Error::new_custom(Refusal::TooLong.code()))
        );
        assert!(over.iter().all(|&byte| byte == 0xa5));
    }

    #[test]
    fn both_ways_in_draw_from_the_one_generator_in_turn() {
        let _only = exclusive();
        assert_eq!(seed(&mut TEST_SEED.clone()), Ok(()));
        let (mut a, mut b, mut c) = ([0u8; 32], [0u8; 32], [0u8; 0]);
        assert_eq!(fill_v02(&mut a), Ok(()));
        assert_eq!(v03(&mut b), Ok(()));
        assert_eq!(v03(&mut c), Ok(()), "an empty request is served too");
        let mut reference = Generator::from_seed(&mut TEST_SEED.clone());
        let (mut want_a, mut want_b) = ([0u8; 32], [0u8; 32]);
        assert_eq!(reference.generate(&mut want_a), Ok(()));
        assert_eq!(reference.generate(&mut want_b), Ok(()));
        assert_eq!((a, b), (want_a, want_b));
    }

    #[test]
    fn v02_reports_the_refusal_as_a_custom_code() {
        let _only = exclusive();
        let code =
            getrandom02::Error::CUSTOM_START.saturating_add(u32::from(Refusal::NotSeeded.code()));
        assert_eq!(
            fill_v02(&mut [0u8; 4]).map_err(|e| e.code().get()),
            Err(code)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "calls the symbol as 0.4 declares it, a layout Rust does not promise (which Miri rightly refuses): this checks the toolchain in use"
    )]
    fn an_error_made_as_0_3s_arrives_intact_as_0_4s() {
        unsafe extern "Rust" {
            // getrandom 0.4.3's own declaration of the shared symbol
            // (src/backends/custom.rs).
            #[link_name = "__getrandom_v03_custom"]
            fn as_v04(dest: *mut u8, len: usize) -> Result<(), getrandom04::Error>;
        }
        let _only = exclusive();
        let mut buf = [0xa5u8; 8];
        // SAFETY: as `v03`; the declaration is getrandom 0.4's, whose `Error`
        // has the shape 0.3's has - the assumption this test checks.
        let result = unsafe { as_v04(buf.as_mut_ptr(), buf.len()) };
        assert_eq!(
            result,
            Err(getrandom04::Error::new_custom(Refusal::NotSeeded.code()))
        );
        assert_eq!(buf, [0xa5; 8]);
    }
}
