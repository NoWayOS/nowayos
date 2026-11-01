// SPDX-License-Identifier: MIT OR Apache-2.0
//! nowayos-rng-health's tests: the cut-offs computed from SP 800-90B
//! section 4.4's formulas, each test failing at its cut-off and latching,
//! the start-up test, the seeds, and SHA-256 against NIST's test vectors.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    reason = "test code: computes on known data, and a failed check panics"
)]

use nowayos_rng_health::{Conditioner, Entropy, Failure, HealthTests, STARTUP_BYTES, WINDOW};
use proptest::prelude::*;
use sha2::{Digest, Sha256};

const ALL: [Entropy; 4] = [Entropy::H8, Entropy::H4, Entropy::H2, Entropy::H1];

/// CRITBINOM(n, p, 1 - alpha): the least k with P(X > k) <= alpha for X
/// binomial(n, p), from the upper tail summed term by term in log space.
fn critbinom(n: u32, p: f64, alpha: f64) -> u32 {
    let ln_choose = |k: u32| -> f64 {
        (0..k)
            .map(|i| f64::from(n - i).ln() - f64::from(i + 1).ln())
            .sum()
    };
    let pmf: Vec<f64> = (0..=n)
        .map(|k| (ln_choose(k) + f64::from(k) * p.ln() + f64::from(n - k) * (1.0 - p).ln()).exp())
        .collect();
    let mut tail = 0.0;
    for k in (0..=n).rev() {
        // tail = P(X > k)
        if tail > alpha {
            return k + 1;
        }
        tail += pmf[k as usize];
    }
    0
}

fn adaptive_cutoff(entropy: Entropy, alpha: f64) -> u16 {
    let p = 2f64.powi(-i32::from(entropy.bits()));
    u16::try_from(1 + critbinom(u32::from(WINDOW), p, alpha)).unwrap()
}

#[test]
fn the_cut_offs_are_section_4_4s_at_alpha_2_to_the_minus_40() {
    let alpha = 2f64.powi(-40);
    for entropy in ALL {
        let h = entropy.bits();
        assert_eq!(
            entropy.repetition_cutoff(),
            1 + 40_u16.div_ceil(h),
            "{entropy:?}"
        );
        assert_eq!(
            entropy.adaptive_cutoff(),
            adaptive_cutoff(entropy, alpha),
            "{entropy:?}"
        );
        assert_eq!(entropy.bytes_per_seed(), 320_u16.div_ceil(h), "{entropy:?}");
    }
    // The table's values.
    let table: Vec<_> = ALL
        .iter()
        .map(|e| (e.repetition_cutoff(), e.adaptive_cutoff()))
        .collect();
    assert_eq!(table, [(6, 19), (11, 78), (21, 201), (41, 336)]);
}

#[test]
fn at_alpha_2_to_the_minus_20_the_formula_gives_the_standards_table_2() {
    // SP 800-90B Table 2, the adaptive proportion test's cut-offs for a
    // window of 512 at alpha = 2^-20: H = 8, 4, 2, 1.
    let alpha = 2f64.powi(-20);
    let computed: Vec<_> = ALL.iter().map(|&e| adaptive_cutoff(e, alpha)).collect();
    assert_eq!(computed, [13, 62, 177, 311]);
}

#[test]
fn a_stuck_source_fails_the_repetition_count_test_at_its_cut_off_and_latches() {
    for entropy in ALL {
        let mut tests = HealthTests::new(entropy);
        let cutoff = entropy.repetition_cutoff();
        for _ in 1..cutoff {
            assert_eq!(tests.test(0x5A), Ok(()), "{entropy:?}");
        }
        assert_eq!(
            tests.test(0x5A),
            Err(Failure::RepetitionCount),
            "{entropy:?}"
        );
        // Latched: a good byte is refused too.
        assert_eq!(tests.test(0x00), Err(Failure::RepetitionCount));
        assert_eq!(tests.failed(), Some(Failure::RepetitionCount));
    }
}

#[test]
fn a_biased_source_fails_the_adaptive_proportion_test_at_its_cut_off() {
    // The window's first value two bytes in three: never long runs, so only
    // the adaptive proportion test can catch it, inside one 512-byte window
    // for every claimed entropy.
    for entropy in ALL {
        let mut tests = HealthTests::new(entropy);
        let mut occurrences = 0;
        let mut other = 0u8;
        let failed = 'feed: loop {
            for _ in 0..2 {
                occurrences += 1;
                let result = tests.test(0xFF);
                if result.is_err() {
                    break 'feed result;
                }
            }
            other = (other + 1) % 0xFE;
            assert_eq!(tests.test(other), Ok(()), "{entropy:?}");
        };
        assert_eq!(failed, Err(Failure::AdaptiveProportion), "{entropy:?}");
        assert_eq!(occurrences, entropy.adaptive_cutoff(), "{entropy:?}");
    }
}

#[test]
fn each_window_counts_afresh() {
    // One short of the cut-off in each of many windows never fails.
    let entropy = Entropy::H8;
    let mut tests = HealthTests::new(entropy);
    for _ in 0..8 {
        let mut window = vec![0xFF_u8; usize::from(entropy.adaptive_cutoff() - 1)];
        let mut other = 0u8;
        while window.len() < usize::from(WINDOW) {
            other = (other + 1) % 0xFE;
            window.push(other);
        }
        // Spread the 0xFF bytes out so no run reaches the other cut-off.
        let (ff, rest) = window.split_at(usize::from(entropy.adaptive_cutoff() - 1));
        let mut spread = Vec::new();
        for (i, &byte) in rest.iter().enumerate() {
            if let Some(&f) = ff.get(i) {
                spread.push(f);
            }
            spread.push(byte);
        }
        assert_eq!(spread.len(), usize::from(WINDOW));
        for byte in spread {
            assert_eq!(tests.test(byte), Ok(()));
        }
    }
}

#[test]
fn the_first_1024_bytes_are_tested_and_discarded_then_seeds_are_sha_256_of_disjoint_runs() {
    for entropy in ALL {
        let mut conditioner = Conditioner::new(entropy);
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i * 7 + i / 256) as u8).collect();
        let (startup, rest) = bytes.split_at(usize::from(STARTUP_BYTES));
        for &byte in startup {
            assert_eq!(conditioner.feed(byte), Ok(None));
        }
        assert!(conditioner.started());
        let per_seed = usize::from(entropy.bytes_per_seed());
        let mut seeds = Vec::new();
        for &byte in rest {
            if let Some(seed) = conditioner.feed(byte).unwrap() {
                seeds.push(seed);
            }
        }
        let expected: Vec<[u8; 32]> = rest
            .chunks_exact(per_seed)
            .map(|run| Sha256::digest(run).into())
            .collect();
        assert_eq!(seeds, expected, "{entropy:?}");
    }
}

#[test]
fn a_failure_in_the_start_up_test_means_no_seed_ever() {
    let mut conditioner = Conditioner::new(Entropy::H8);
    for _ in 0..5 {
        assert_eq!(conditioner.feed(7), Ok(None));
    }
    assert_eq!(conditioner.feed(7), Err(Failure::RepetitionCount));
    for i in 0..4096u32 {
        assert_eq!(conditioner.feed(i as u8), Err(Failure::RepetitionCount));
    }
    assert_eq!(conditioner.failed(), Some(Failure::RepetitionCount));
}

#[test]
fn sha_256_matches_nists_test_vectors() {
    // FIPS 180-2 / NIST's SHA-256 examples: "abc", the empty message and the
    // 448-bit message.
    let hex = |digest: [u8; 32]| {
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    assert_eq!(
        hex(Sha256::digest(b"abc").into()),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(Sha256::digest(b"").into()),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").into()),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

proptest! {
    /// Random bytes from a source as good as claimed pass (a false alarm is
    /// one window in 2^40).
    #[test]
    fn random_input_passes(bytes in prop::collection::vec(any::<u8>(), 2048..4096)) {
        let mut conditioner = Conditioner::new(Entropy::H8);
        let mut seeds = 0;
        for byte in bytes.iter().copied() {
            if conditioner.feed(byte).unwrap().is_some() {
                seeds += 1;
            }
        }
        prop_assert_eq!(seeds, (bytes.len() - usize::from(STARTUP_BYTES)) / 40);
    }

    /// Any bytes, at any claimed entropy: no panic, no overflow; once a test
    /// fails, nothing but that failure, and no seed.
    #[test]
    fn no_input_panics_and_a_failure_latches(
        entropy in prop::sample::select(ALL.to_vec()),
        bytes in prop::collection::vec(prop_oneof![Just(0u8), Just(1u8), any::<u8>()], 0..6000),
    ) {
        let mut conditioner = Conditioner::new(entropy);
        let mut failed = None;
        for byte in bytes {
            let result = conditioner.feed(byte);
            match failed {
                Some(failure) => prop_assert_eq!(result, Err(failure)),
                None => if let Err(failure) = result { failed = Some(failure) },
            }
        }
        prop_assert_eq!(conditioner.failed(), failed);
    }
}
