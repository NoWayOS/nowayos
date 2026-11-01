// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PD's generator: ChaCha20 with fast key erasure.

use chacha20::ChaCha20Rng;
use chacha20::rand_core::{Rng as _, SeedableRng as _};
use zeroize::Zeroize as _;

/// Bytes in a seed: 256 bits, ChaCha20's key.
pub const SEED_LEN: usize = 32;

/// The most one request may take, in bytes. No PD needs more at once.
pub const MAX_REQUEST: usize = 1 << 16;

/// Requests one seed serves before the PD must take a fresh one.
pub const RESEED_INTERVAL: u32 = 1 << 16;

/// Why a request was refused. The back ends report it to getrandom's callers
/// as a custom error code ([`Refusal::code`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No seed yet, or the generator was dropped: the PD is not ready.
    NotSeeded,
    /// More than [`MAX_REQUEST`] bytes in one request.
    TooLong,
    /// [`RESEED_INTERVAL`] requests since the last seed: a fresh one is due.
    ReseedDue,
    /// A request from inside the generator (re-entry), which no PD makes.
    Busy,
}

impl Refusal {
    /// The code the back ends report, in getrandom's custom-error range.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::NotSeeded => 1,
            Self::TooLong => 2,
            Self::ReseedDue => 3,
            Self::Busy => 4,
        }
    }
}

/// ChaCha20 with fast key erasure: after every request the generator re-keys
/// itself from its own next 32 bytes, and the old state is wiped (`chacha20`
/// built with `zeroize` wipes its key and buffer on drop, which the build
/// checks below).
pub struct Generator {
    rng: ChaCha20Rng,
    requests: u32,
}

/// The build fails unless the generator's state wipes itself when dropped.
const _: fn() = || {
    fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    wiped_on_drop::<ChaCha20Rng>();
};

impl Generator {
    /// A generator keyed with `seed`, which is then wiped.
    #[must_use]
    pub fn from_seed(seed: &mut [u8; SEED_LEN]) -> Self {
        let rng = ChaCha20Rng::from_seed(*seed);
        seed.zeroize();
        Self { rng, requests: 0 }
    }

    /// Whether a request for `len` bytes would be served.
    ///
    /// # Errors
    ///
    /// [`Refusal::TooLong`] or [`Refusal::ReseedDue`].
    pub fn ready_for(&self, len: usize) -> Result<(), Refusal> {
        if len > MAX_REQUEST {
            Err(Refusal::TooLong)
        } else if self.requests >= RESEED_INTERVAL {
            Err(Refusal::ReseedDue)
        } else {
            Ok(())
        }
    }

    /// Fills `dest`, then re-keys.
    ///
    /// # Errors
    ///
    /// As [`Generator::ready_for`]; `dest` is untouched when refused.
    pub fn generate(&mut self, dest: &mut [u8]) -> Result<(), Refusal> {
        self.ready_for(dest.len())?;
        self.rng.fill_bytes(dest);
        let mut next = [0u8; SEED_LEN];
        self.rng.fill_bytes(&mut next);
        // The old generator is dropped here, and wipes itself.
        self.rng = ChaCha20Rng::from_seed(next);
        next.zeroize();
        self.requests = self.requests.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SEED: [u8; SEED_LEN] = *b"nowayos-entropy test seed, 32 b.";

    /// RFC 8439, appendix A.1, test vector 1: the ChaCha20 block function
    /// with an all-zero key and nonce, block counter 0.
    const RFC8439_A1_TV1: [u8; 64] = [
        0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90, 0x40, 0x5d, 0x6a, 0xe5, 0x53, 0x86, 0xbd,
        0x28, 0xbd, 0xd2, 0x19, 0xb8, 0xa0, 0x8d, 0xed, 0x1a, 0xa8, 0x36, 0xef, 0xcc, 0x8b, 0x77,
        0x0d, 0xc7, 0xda, 0x41, 0x59, 0x7c, 0x51, 0x57, 0x48, 0x8d, 0x77, 0x24, 0xe0, 0x3f, 0xb8,
        0xd8, 0x4a, 0x37, 0x6a, 0x43, 0xb8, 0xf4, 0x15, 0x18, 0xa1, 0x1c, 0xc3, 0x87, 0xb6, 0x69,
        0xb2, 0xee, 0x65, 0x86,
    ];

    /// RFC 8439, appendix A.1, test vector 2: the same key and nonce, block 1.
    const RFC8439_A1_TV2: [u8; 64] = [
        0x9f, 0x07, 0xe7, 0xbe, 0x55, 0x51, 0x38, 0x7a, 0x98, 0xba, 0x97, 0x7c, 0x73, 0x2d, 0x08,
        0x0d, 0xcb, 0x0f, 0x29, 0xa0, 0x48, 0xe3, 0x65, 0x69, 0x12, 0xc6, 0x53, 0x3e, 0x32, 0xee,
        0x7a, 0xed, 0x29, 0xb7, 0x21, 0x76, 0x9c, 0xe6, 0x4e, 0x43, 0xd5, 0x71, 0x33, 0xb0, 0x74,
        0xd8, 0x39, 0xd5, 0x31, 0xed, 0x1f, 0x28, 0x51, 0x0a, 0xfb, 0x45, 0xac, 0xe1, 0x0a, 0x1f,
        0x4b, 0x79, 0x4d, 0x6f,
    ];

    #[test]
    fn chacha20_matches_rfc_8439_test_vectors_1_and_2() {
        let mut rng = ChaCha20Rng::from_seed([0u8; 32]);
        let mut block = [0u8; 64];
        rng.fill_bytes(&mut block);
        assert_eq!(block, RFC8439_A1_TV1);
        rng.fill_bytes(&mut block);
        assert_eq!(block, RFC8439_A1_TV2);
    }

    #[test]
    fn the_first_request_is_the_seeds_chacha20_stream_and_the_seed_is_wiped() {
        let mut seed = TEST_SEED;
        let mut generator = Generator::from_seed(&mut seed);
        assert_eq!(seed, [0; SEED_LEN]);
        let mut got = [0u8; 100];
        assert_eq!(generator.generate(&mut got), Ok(()));
        let mut want = [0u8; 100];
        ChaCha20Rng::from_seed(TEST_SEED).fill_bytes(&mut want);
        assert_eq!(got, want);
    }

    #[test]
    fn it_re_keys_from_its_own_next_32_bytes_after_every_request() {
        let mut generator = Generator::from_seed(&mut TEST_SEED.clone());
        // The model: each request is the current key's stream, then the
        // next 32 bytes of that stream are the next key.
        let mut key = TEST_SEED;
        for len in [1usize, 16, 64, 65, 300] {
            let mut model = ChaCha20Rng::from_seed(key);
            let mut want = vec![0u8; len];
            model.fill_bytes(&mut want);
            model.fill_bytes(&mut key);
            let mut got = vec![0u8; len];
            assert_eq!(generator.generate(&mut got), Ok(()));
            assert_eq!(got, want, "request of {len} bytes");
        }
        // So no request continues the one before it.
        let mut stream = ChaCha20Rng::from_seed(TEST_SEED);
        let (mut first, mut next) = ([0u8; 16], [0u8; 16]);
        stream.fill_bytes(&mut first);
        stream.fill_bytes(&mut next);
        let mut again = Generator::from_seed(&mut TEST_SEED.clone());
        let (mut one, mut two) = ([0u8; 16], [0u8; 16]);
        assert_eq!(
            (again.generate(&mut one), again.generate(&mut two)),
            (Ok(()), Ok(()))
        );
        assert_eq!(one, first);
        assert_ne!(two, next);
    }

    #[test]
    fn a_request_over_max_request_bytes_is_refused_untouched() {
        let mut generator = Generator::from_seed(&mut TEST_SEED.clone());
        let mut over = vec![0xa5u8; MAX_REQUEST.saturating_add(1)];
        assert_eq!(generator.ready_for(over.len()), Err(Refusal::TooLong));
        assert_eq!(generator.generate(&mut over), Err(Refusal::TooLong));
        assert!(over.iter().all(|&byte| byte == 0xa5));
        let mut most = vec![0u8; MAX_REQUEST];
        assert_eq!(generator.generate(&mut most), Ok(()));
    }

    #[test]
    fn after_reseed_interval_requests_every_request_is_refused() {
        let mut generator = Generator::from_seed(&mut TEST_SEED.clone());
        generator.requests = RESEED_INTERVAL.saturating_sub(1);
        let mut buf = [0xa5u8; 8];
        assert_eq!(
            generator.generate(&mut buf),
            Ok(()),
            "the last request a seed serves"
        );
        let served = buf;
        for _ in 0..3 {
            assert_eq!(generator.ready_for(1), Err(Refusal::ReseedDue));
            assert_eq!(generator.generate(&mut buf), Err(Refusal::ReseedDue));
            assert_eq!(buf, served, "nothing written");
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "65,536 ChaCha20 re-keys: too slow under Miri, which runs the test above"
    )]
    fn one_seed_serves_exactly_reseed_interval_requests() {
        let mut generator = Generator::from_seed(&mut TEST_SEED.clone());
        let mut byte = [0u8; 1];
        for request in 0..RESEED_INTERVAL {
            assert_eq!(generator.generate(&mut byte), Ok(()), "request {request}");
        }
        assert_eq!(generator.generate(&mut byte), Err(Refusal::ReseedDue));
    }
}
