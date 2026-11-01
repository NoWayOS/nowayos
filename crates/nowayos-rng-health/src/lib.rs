// SPDX-License-Identifier: MIT OR Apache-2.0
//! A hardware random source's bytes, health-tested and conditioned into
//! seeds, for the entropy PD.
//!
//! Every byte from the source passes NIST SP 800-90B's two approved
//! continuous health tests (section 4.4) before anything else is done with
//! it, at a false-alarm probability of α = 2⁻⁴⁰:
//!
//! * the **repetition count test** (4.4.1) fails when one value occurs
//!   C = 1 + ⌈40 / H⌉ times in a row;
//! * the **adaptive proportion test** (4.4.2) fails when, in a window of
//!   512 bytes, the window's first value occurs C times, where
//!   C = 1 + CRITBINOM(512, 2⁻ᴴ, 1 − 2⁻⁴⁰).
//!
//! H is the min-entropy per byte claimed for the source ([`Entropy`]). The
//! first 1,024 bytes are tested and discarded (the start-up test, 4.3).
//! Then every ⌈320 / H⌉ tested bytes become one 256-bit seed: SHA-256 over
//! them, a vetted conditioning function (3.1.5.1.1), crediting more than
//! 255.99999999 bits (3.1.5.1.2). No byte goes into two seeds.
//!
//! **A failure latches**: once either test fails, [`Conditioner::feed`]
//! refuses every byte and makes no seed again; the entropy PD reports it
//! and faults. What the tests show is a source that has
//! stopped or is badly stuck, never that a source is good.
//!
//! Pure logic: no heap, no `unsafe`, no panics; arithmetic is checked or
//! saturating.

#![no_std]

use sha2::{Digest, Sha256};

/// The bytes the start-up test runs over and discards (SP 800-90B 4.3).
pub const STARTUP_BYTES: u16 = 1024;

/// The adaptive proportion test's window, in bytes (4.4.2, non-binary).
pub const WINDOW: u16 = 512;

/// The min-entropy per byte claimed for a source: what its documentation
/// or an SP 800-90B assessment supports, set in the board file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entropy {
    /// 8 bits: QEMU's virtio-rng, fed from the host's own generator.
    H8,
    /// 4 bits.
    H4,
    /// 2 bits.
    H2,
    /// 1 bit: the Pi 5's and the i.MX8's sources until each is assessed.
    H1,
}

impl Entropy {
    /// The bits per byte claimed.
    #[must_use]
    pub const fn bits(self) -> u16 {
        match self {
            Self::H8 => 8,
            Self::H4 => 4,
            Self::H2 => 2,
            Self::H1 => 1,
        }
    }

    /// The repetition count test's cut-off at α = 2⁻⁴⁰: 1 + ⌈40 / H⌉.
    #[must_use]
    pub const fn repetition_cutoff(self) -> u16 {
        match self {
            Self::H8 => 6,
            Self::H4 => 11,
            Self::H2 => 21,
            Self::H1 => 41,
        }
    }

    /// The adaptive proportion test's cut-off at α = 2⁻⁴⁰ for a window of
    /// 512: 1 + CRITBINOM(512, 2⁻ᴴ, 1 − 2⁻⁴⁰) (the tests compute it).
    #[must_use]
    pub const fn adaptive_cutoff(self) -> u16 {
        match self {
            Self::H8 => 19,
            Self::H4 => 78,
            Self::H2 => 201,
            Self::H1 => 336,
        }
    }

    /// The tested bytes one seed is made from: ⌈320 / H⌉, 64 bits more
    /// claimed entropy than the seed's 256.
    #[must_use]
    pub const fn bytes_per_seed(self) -> u16 {
        320_u16.div_ceil(self.bits())
    }
}

/// Which health test failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// One value occurred the repetition count test's cut-off times in a
    /// row.
    RepetitionCount,
    /// A window's first value occurred the adaptive proportion test's
    /// cut-off times in the window.
    AdaptiveProportion,
}

/// SP 800-90B's two continuous health tests over a source's bytes, the
/// failure latched.
#[derive(Clone, Debug)]
pub struct HealthTests {
    entropy: Entropy,
    /// The repetition count test's value and how often it has occurred in
    /// a row; none before the first byte.
    run: Option<(u8, u16)>,
    /// The adaptive proportion test's window: its first value, how often it
    /// has occurred, and how many bytes the window has seen.
    window: Option<(u8, u16, u16)>,
    failed: Option<Failure>,
}

impl HealthTests {
    /// Tests for a source claimed to give `entropy` bits per byte.
    #[must_use]
    pub const fn new(entropy: Entropy) -> Self {
        Self {
            entropy,
            run: None,
            window: None,
            failed: None,
        }
    }

    /// The failure, if a test has failed: it latches.
    #[must_use]
    pub const fn failed(&self) -> Option<Failure> {
        self.failed
    }

    /// Tests one byte.
    ///
    /// # Errors
    ///
    /// The [`Failure`] of the test this byte failed, or of the one that
    /// failed before: once failed, every byte is refused.
    pub fn test(&mut self, byte: u8) -> Result<(), Failure> {
        if let Some(failure) = self.failed {
            return Err(failure);
        }
        let run = match self.run {
            Some((value, count)) if value == byte => count.saturating_add(1),
            _ => 1,
        };
        self.run = Some((byte, run));
        if run >= self.entropy.repetition_cutoff() {
            return Err(*self.failed.insert(Failure::RepetitionCount));
        }
        let (first, count, seen) = match self.window {
            Some((first, count, seen)) if seen < WINDOW => {
                let count = if byte == first {
                    count.saturating_add(1)
                } else {
                    count
                };
                (first, count, seen.saturating_add(1))
            }
            // The window's first byte, or a new window after a full one.
            _ => (byte, 1, 1),
        };
        self.window = Some((first, count, seen));
        if count >= self.entropy.adaptive_cutoff() {
            return Err(*self.failed.insert(Failure::AdaptiveProportion));
        }
        Ok(())
    }
}

/// A source's bytes in, seeds out: every byte health-tested, the first
/// [`STARTUP_BYTES`] discarded, then each [`Entropy::bytes_per_seed`]
/// tested bytes conditioned into one 256-bit seed with SHA-256.
#[derive(Clone, Debug)]
pub struct Conditioner {
    tests: HealthTests,
    /// Start-up bytes still to test and discard.
    startup: u16,
    /// The bytes of the seed being made, hashed as they come.
    pool: Sha256,
    pooled: u16,
}

impl Conditioner {
    /// A conditioner for a source claimed to give `entropy` bits per byte,
    /// at the start of its start-up test.
    #[must_use]
    pub fn new(entropy: Entropy) -> Self {
        Self {
            tests: HealthTests::new(entropy),
            startup: STARTUP_BYTES,
            pool: Sha256::new(),
            pooled: 0,
        }
    }

    /// Whether the start-up test is over.
    #[must_use]
    pub const fn started(&self) -> bool {
        self.startup == 0
    }

    /// The failure, if a test has failed: no seed is made after it.
    #[must_use]
    pub const fn failed(&self) -> Option<Failure> {
        self.tests.failed()
    }

    /// Takes one byte from the source: tests it, then discards it during
    /// the start-up test, or pools it; returns a seed when this byte
    /// completes one.
    ///
    /// # Errors
    ///
    /// The [`Failure`] of the test this byte failed, or of the one that
    /// failed before. The seed being pooled is dropped.
    pub fn feed(&mut self, byte: u8) -> Result<Option<[u8; 32]>, Failure> {
        if let Err(failure) = self.tests.test(byte) {
            self.pool = Sha256::new();
            self.pooled = 0;
            return Err(failure);
        }
        if self.startup > 0 {
            self.startup = self.startup.saturating_sub(1);
            return Ok(None);
        }
        self.pool.update([byte]);
        self.pooled = self.pooled.saturating_add(1);
        if self.pooled < self.tests.entropy.bytes_per_seed() {
            return Ok(None);
        }
        self.pooled = 0;
        let pool = core::mem::replace(&mut self.pool, Sha256::new());
        Ok(Some(pool.finalize().into()))
    }
}
