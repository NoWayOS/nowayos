// SPDX-License-Identifier: MIT OR Apache-2.0
//! rustls's hash trait over RustCrypto's SHA-2 (`sha2` 0.10): SHA-256 and
//! SHA-384, for the handshake transcript.

use alloc::boxed::Box;
use core::marker::PhantomData;

use rustls::crypto::hash::{Context, Hash, HashAlgorithm, Output};
use sha2_010::Digest;

/// A SHA-2 function as rustls's [`Hash`]. `PhantomData<fn() -> D>` keeps it
/// `Send + Sync` whatever `D` is, and constructible in a `static`.
pub(crate) struct Sha2<D>(PhantomData<fn() -> D>, HashAlgorithm);

pub(crate) static SHA256: Sha2<sha2_010::Sha256> = Sha2(PhantomData, HashAlgorithm::SHA256);
pub(crate) static SHA384: Sha2<sha2_010::Sha384> = Sha2(PhantomData, HashAlgorithm::SHA384);

impl<D> Hash for Sha2<D>
where
    D: Digest + Clone + Send + Sync + 'static,
{
    fn start(&self) -> Box<dyn Context> {
        Box::new(Sha2Context(D::new()))
    }

    fn hash(&self, data: &[u8]) -> Output {
        Output::new(&D::digest(data))
    }

    fn algorithm(&self) -> HashAlgorithm {
        self.1
    }

    fn output_len(&self) -> usize {
        <D as Digest>::output_size()
    }
}

/// A hash in progress.
struct Sha2Context<D>(D);

impl<D> Context for Sha2Context<D>
where
    D: Digest + Clone + Send + Sync + 'static,
{
    fn fork_finish(&self) -> Output {
        Output::new(&self.0.clone().finalize())
    }

    fn fork(&self) -> Box<dyn Context> {
        Box::new(Self(self.0.clone()))
    }

    fn finish(self: Box<Self>) -> Output {
        Output::new(&self.0.finalize())
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}
