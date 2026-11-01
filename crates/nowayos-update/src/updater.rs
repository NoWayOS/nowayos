// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update PD's decisions: which trust list to take, which
//! manifest to accept, where the image goes and when it is marked good - in
//! one library, driven by events, so that every refusal is
//! shown over many inputs before a PD runs it.
//!
//! # Events, in order
//!
//! [`Updater::start`] reads the update state - and starts nothing if it is
//! lost - then checks the held trust list again against the trust anchors
//! in the running image: a list no anchor signed is not used, and is
//! reported. A trial slot that is not the one running failed its trial (the
//! bootloader fell back), and is unmarked. Then, for a bundle:
//!
//! 1. [`trust_list`](Updater::trust_list): a newer list is taken only if its
//!    version is higher and a root key of the list held signed it - and, if
//!    it names a root key that list does not, that new root signed it too.
//!    A staged update whose image key it removes is
//!    unmarked: the hook's *Withdrawn*.
//! 2. [`manifest`](Updater::manifest): refused, each with its own reason, if
//!    no image key in the held list signed it, if it is marked development
//!    and the held list is not, if its security version is below the
//!    rollback index, if it needs a newer trust list than the one held, if
//!    it names another product, or if its image is longer than a slot.
//! 3. [`image`](Updater::image): the image's chunks, in order, written to
//!    the slot that is **not** running - never the running one - and hashed
//!    as they are written.
//! 4. [`bundle_end`](Updater::bundle_end): the slot is marked for one trial
//!    boot only if the length and the SHA-256 match the manifest; then a
//!    restart is wanted.
//!
//! At a trial boot, [`health`](Updater::health): only a verdict that the
//! trial passed marks the slot good and raises the rollback index to the
//! manifest's security version; one that failed or came late unmarks it,
//! and the device falls back to the slot that was good.
//!
//! **Before the first byte goes into the other slot, the update state says
//! that slot is not marked**, so a reset at any point - mid-image, before
//! the mark, during a write of the state - leaves the running slot booting
//! and the other unmarked ([`crate::state`] keeps each write whole).
//!
//! # What it does not decide
//!
//! It says a restart is wanted; the PD asks the supervisor. It decides
//! nothing the fault policy leaves to the update design, such as telling a planned
//! restart apart in the restart count. Checking a signature and hashing are
//! the PD's [`Crypto`] - `nowayos-signature` in a PD - so this crate needs
//! no curve arithmetic; storage is the PD's too ([`StateStorage`],
//! [`SlotStorage`]).

use crate::chunk::{Chunk, Part};
use crate::manifest::Manifest;
use crate::state::{Slot, StateStorage, StateStore, StorageError, StoreError, Trial, UpdateState};
use crate::trust_list::{Purpose, TrustList};
use crate::{DIGEST_LEN, KEY_ID_LEN, KEY_LEN, NamedSignature, SIGNATURE_LEN};

/// The checks the update PD's logic needs: ECDSA P-256 over SHA-256, and
/// SHA-256 itself. A PD gives `nowayos-signature`'s.
pub trait Crypto {
    /// A SHA-256 being computed over an image as it arrives.
    type Hash: ImageHash;

    /// Whether `signature` (r then s) is a good signature by `key` (x then
    /// y) over `message`.
    fn verify(&self, key: &[u8; KEY_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool;

    /// `key`'s key ID: the SHA-256 of its SEC1 uncompressed encoding.
    fn key_id(&self, key: &[u8; KEY_LEN]) -> [u8; KEY_ID_LEN];

    /// A new SHA-256.
    fn hash(&self) -> Self::Hash;
}

/// A SHA-256 over bytes given in order.
pub trait ImageHash {
    /// Hashes `bytes`, after those before.
    fn update(&mut self, bytes: &[u8]);

    /// The digest of everything given.
    fn finish(self) -> [u8; DIGEST_LEN];
}

/// Where images are written: the two boot slots, which a storage PD serves,
/// or a test's memory.
pub trait SlotStorage {
    /// Writes `bytes` at `offset` in `slot`.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if they were not written whole.
    fn write(&mut self, slot: Slot, offset: u64, bytes: &[u8]) -> Result<(), StorageError>;
}

/// What the device is and holds, from its running image.
#[derive(Clone, Copy, Debug)]
pub struct Config<'a> {
    /// The product it runs, as a manifest names it.
    pub product: &'a str,
    /// The slot running now, as the bootloader says.
    pub running: Slot,
    /// A slot's length: no longer image fits.
    pub slot_len: u64,
    /// The root keys in the running image: the trust anchors a
    /// held trust list is checked against.
    pub anchors: &'a [[u8; KEY_LEN]],
}

/// How the device started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// From the slot marked good, with no trial pending.
    Good,
    /// On trial: awaiting the health verdict ([`Updater::health`]).
    OnTrial,
    /// A trial was pending for the other slot and the device runs the good
    /// one: the trial failed, and is unmarked.
    FellBack,
}

/// Why the logic cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The update state could not be read: lost, or storage failed. No
    /// update starts.
    State(StoreError),
    /// The running slot is neither the good one nor the trial: writing
    /// the other could overwrite the slot that boots.
    UnexpectedSlot,
}

/// Why an event was refused. Refused, nothing was written, and a staged
/// update in progress is dropped where the reason says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A trust list whose version is not higher than the one held.
    TrustListNotNewer {
        /// The version held.
        held: u32,
        /// The version offered.
        offered: u32,
    },
    /// A trust list no root key held signed, or naming a new root key that
    /// did not sign it too.
    TrustListNotSigned,
    /// A manifest no image key in the held list signed.
    ManifestNotSigned,
    /// A development manifest, and the held list is not development.
    Development,
    /// A security version below the rollback index.
    Rollback {
        /// The rollback index.
        index: u32,
        /// The manifest's security version.
        offered: u32,
    },
    /// A manifest needing a newer trust list than the one held.
    NeedsNewerTrustList {
        /// The version held.
        held: u32,
        /// The version needed.
        needed: u32,
    },
    /// A manifest for another product.
    OtherProduct,
    /// An image longer than a slot.
    TooLong,
    /// An image chunk with no manifest accepted, or not the image's part.
    NotExpected,
    /// A chunk not where the image has got to, or past its end; the staged
    /// update is dropped.
    OutOfOrder,
    /// The image's length or SHA-256 does not match the manifest; the slot
    /// is not marked.
    Mismatch,
    /// Awaiting the trial's verdict: no update is taken during a trial; or
    /// a verdict with no trial running.
    Trial,
    /// Storage failed; nothing is marked.
    Storage(StoreError),
}

/// A trust list taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taken {
    /// Its version.
    pub version: u32,
    /// A staged update whose image key it removes was unmarked: the hook's
    /// *Withdrawn*.
    pub withdrawn: bool,
}

/// The health check's verdict on a trial boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every PD started, each heartbeat came, the product's check passed,
    /// within the deadline.
    Passed,
    /// Something failed.
    Failed,
    /// The deadline passed first.
    Late,
}

/// What a verdict did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
    /// The trial slot is good now, and the rollback index this.
    MarkedGood {
        /// The new rollback index.
        rollback_index: u32,
    },
    /// The trial slot is unmarked: restart into the slot that was good.
    FallBack,
}

/// A staged update in progress: its manifest's facts, and how far its
/// image has got.
struct Staging<H> {
    slot: Slot,
    image_len: u64,
    sha256: [u8; DIGEST_LEN],
    security_version: u32,
    image_key: [u8; KEY_ID_LEN],
    written: u64,
    hash: H,
}

/// The update PD's logic; see the [module](self).
pub struct Updater<'a, C: Crypto, S, B> {
    config: Config<'a>,
    crypto: C,
    store: StateStore<S>,
    slots: B,
    state: UpdateState,
    /// Whether the held list checked against the anchors: if not, it is not
    /// used, and the anchors alone are trusted.
    list_used: bool,
    staging: Option<Staging<C::Hash>>,
}

impl<'a, C: Crypto, S: StateStorage, B: SlotStorage> Updater<'a, C, S, B> {
    /// Starts the logic: reads the state, checks the held trust list
    /// against the anchors, and settles a trial that failed.
    ///
    /// # Errors
    ///
    /// [`StartError`]: the state lost or unreadable, or the running slot
    /// unexpected.
    pub fn start(
        config: Config<'a>,
        crypto: C,
        mut store: StateStore<S>,
        slots: B,
    ) -> Result<(Self, Started), StartError> {
        let mut state = store.read().map_err(StartError::State)?;
        let started = match state.trial {
            Some(trial) if trial.slot == config.running => Started::OnTrial,
            Some(_) if state.boots == config.running => {
                state.trial = None;
                store.write(&state).map_err(StartError::State)?;
                Started::FellBack
            }
            None if state.boots == config.running => Started::Good,
            _ => return Err(StartError::UnexpectedSlot),
        };
        let list_used = state
            .trust_list
            .as_ref()
            .is_some_and(|list| signed_by_any(&crypto, list, config.anchors.iter()));
        let updater = Self {
            config,
            crypto,
            store,
            slots,
            state,
            list_used,
            staging: None,
        };
        Ok((updater, started))
    }

    /// The update state as last read or written.
    #[must_use]
    pub const fn state(&self) -> &UpdateState {
        &self.state
    }

    /// The trust list in use: the held one, if it checked against the
    /// anchors.
    #[must_use]
    pub fn trust_list(&self) -> Option<&TrustList> {
        self.state.trust_list.as_ref().filter(|_| self.list_used)
    }

    /// Whether the held trust list was read back and did not check against
    /// the anchors: it is not used (reported at start).
    #[must_use]
    pub fn held_list_refused(&self) -> bool {
        self.state.trust_list.is_some() && !self.list_used
    }

    /// The storage, back.
    pub fn into_parts(self) -> (StateStore<S>, B) {
        (self.store, self.slots)
    }

    /// Offers a trust list; see the [module](self).
    ///
    /// # Errors
    ///
    /// A [`Refusal`]: not newer, not signed as its trust list requires, on
    /// trial, or storage failed. Refused, the held list is unchanged.
    pub fn take_trust_list(&mut self, list: &TrustList) -> Result<Taken, Refusal> {
        self.not_on_trial()?;
        if let Some(held) = self.state.trust_list.as_ref()
            && list.version() <= held.version()
        {
            return Err(Refusal::TrustListNotNewer {
                held: held.version(),
                offered: list.version(),
            });
        }
        let crypto = &self.crypto;
        let anchors = self.config.anchors;
        let held_roots = self.trust_list().map(roots);
        let signed_by_held = match &held_roots {
            Some(keys) => signed_by_any(crypto, list, keys.iter().flatten()),
            None => signed_by_any(crypto, list, anchors.iter()),
        };
        // A root the list names that the held list (or the anchors) does
        // not must have signed it too.
        let new_roots_signed = roots(list).iter().flatten().all(|key| {
            let known = match &held_roots {
                Some(keys) => keys.iter().flatten().any(|held| held == key),
                None => anchors.contains(key),
            };
            known || signed_by_any(crypto, list, core::iter::once(key))
        });
        if !(signed_by_held && new_roots_signed) {
            return Err(Refusal::TrustListNotSigned);
        }
        let mut state = self.state.clone();
        state.trust_list = Some(list.clone());
        let mut withdrawn = false;
        if let Some(trial) = state.trial
            && !lists_image_key(crypto, list, &trial.image_key)
        {
            state.trial = None;
            withdrawn = true;
        }
        self.write(state)?;
        self.list_used = true;
        if self
            .staging
            .as_ref()
            .is_some_and(|staging| !lists_image_key(&self.crypto, list, &staging.image_key))
        {
            self.staging = None;
            withdrawn = true;
        }
        Ok(Taken {
            version: list.version(),
            withdrawn,
        })
    }

    /// Offers a manifest; see the [module](self). Accepted, a slot marked
    /// for a trial is unmarked before its image is written over.
    ///
    /// # Errors
    ///
    /// A [`Refusal`], each rule with its own; refused, nothing changes.
    pub fn manifest(&mut self, manifest: &Manifest) -> Result<(), Refusal> {
        self.not_on_trial()?;
        let list = self.trust_list();
        let signature = manifest.signature();
        let signer = list.and_then(|list| {
            image_keys(list)
                .into_iter()
                .flatten()
                .find(|key| self.crypto.key_id(key) == signature.key_id)
        });
        let signed = signer.is_some_and(|key| {
            self.crypto
                .verify(&key, &manifest.signed_bytes(), &signature.signature)
        });
        let Some((list, key)) = list.zip(signer).filter(|_| signed) else {
            return Err(Refusal::ManifestNotSigned);
        };
        if manifest.is_development() && !list.is_development() {
            return Err(Refusal::Development);
        }
        if manifest.security_version() < self.state.rollback_index {
            return Err(Refusal::Rollback {
                index: self.state.rollback_index,
                offered: manifest.security_version(),
            });
        }
        if manifest.trust_list_version() > list.version() {
            return Err(Refusal::NeedsNewerTrustList {
                held: list.version(),
                needed: manifest.trust_list_version(),
            });
        }
        if manifest.product() != self.config.product {
            return Err(Refusal::OtherProduct);
        }
        if manifest.image_len() > self.config.slot_len {
            return Err(Refusal::TooLong);
        }
        let image_key = self.crypto.key_id(&key);
        if self.state.trial.is_some() {
            let mut state = self.state.clone();
            state.trial = None;
            self.write(state)?;
        }
        self.staging = Some(Staging {
            slot: self.config.running.other(),
            image_len: manifest.image_len(),
            sha256: manifest.image_sha256(),
            security_version: manifest.security_version(),
            image_key,
            written: 0,
            hash: self.crypto.hash(),
        });
        Ok(())
    }

    /// One chunk of the image, written to the slot not running and hashed.
    ///
    /// # Errors
    ///
    /// [`Refusal::NotExpected`] with no manifest accepted or a chunk of
    /// another part; [`Refusal::OutOfOrder`] for one not where the image
    /// has got to or past its end, and [`Refusal::Storage`] - each of which
    /// drops the staged update.
    pub fn image(&mut self, chunk: &Chunk) -> Result<(), Refusal> {
        let Some(staging) = self.staging.as_mut() else {
            return Err(Refusal::NotExpected);
        };
        if chunk.part() != Part::Image {
            return Err(Refusal::NotExpected);
        }
        if chunk.offset() != staging.written || chunk.end() > staging.image_len {
            self.staging = None;
            return Err(Refusal::OutOfOrder);
        }
        if let Err(error) = self
            .slots
            .write(staging.slot, chunk.offset(), chunk.bytes())
        {
            self.staging = None;
            return Err(Refusal::Storage(StoreError::Storage(error)));
        }
        staging.hash.update(chunk.bytes());
        staging.written = chunk.end();
        Ok(())
    }

    /// The bundle's end: the slot is marked for one trial boot if the image
    /// is whole and its SHA-256 matches; then a restart is wanted. Returns
    /// the slot marked.
    ///
    /// # Errors
    ///
    /// [`Refusal::NotExpected`] with no manifest accepted;
    /// [`Refusal::Mismatch`] if the length or hash differ; or
    /// [`Refusal::Storage`]. Refused, nothing is marked.
    pub fn bundle_end(&mut self) -> Result<Slot, Refusal> {
        let staging = self.staging.take().ok_or(Refusal::NotExpected)?;
        if staging.written != staging.image_len || staging.hash.finish() != staging.sha256 {
            return Err(Refusal::Mismatch);
        }
        let mut state = self.state.clone();
        state.trial = Some(Trial {
            slot: staging.slot,
            security_version: staging.security_version,
            image_key: staging.image_key,
        });
        self.write(state)?;
        Ok(staging.slot)
    }

    /// The health check's verdict on this trial boot: passed marks the slot
    /// good and raises the rollback index; failed or late unmarks it.
    ///
    /// # Errors
    ///
    /// [`Refusal::Trial`] with no trial running; [`Refusal::Storage`].
    pub fn health(&mut self, verdict: Verdict) -> Result<Judged, Refusal> {
        let trial = self
            .state
            .trial
            .filter(|trial| trial.slot == self.config.running)
            .ok_or(Refusal::Trial)?;
        let mut state = self.state.clone();
        state.trial = None;
        let judged = match verdict {
            Verdict::Passed => {
                state.boots = trial.slot;
                state.rollback_index = state.rollback_index.max(trial.security_version);
                Judged::MarkedGood {
                    rollback_index: state.rollback_index,
                }
            }
            Verdict::Failed | Verdict::Late => Judged::FallBack,
        };
        self.write(state)?;
        Ok(judged)
    }

    /// Refuses updates while a trial runs, awaiting its verdict.
    fn not_on_trial(&self) -> Result<(), Refusal> {
        match self.state.trial {
            Some(trial) if trial.slot == self.config.running => Err(Refusal::Trial),
            _ => Ok(()),
        }
    }

    /// Writes `state`, and keeps it as the state.
    fn write(&mut self, state: UpdateState) -> Result<(), Refusal> {
        self.store.write(&state).map_err(Refusal::Storage)?;
        self.state = state;
        Ok(())
    }
}

/// The list's root keys: those for the next trust list.
fn roots(list: &TrustList) -> [Option<[u8; KEY_LEN]>; crate::trust_list::MAX_KEYS] {
    keys_for(list, Purpose::TrustList)
}

/// The list's image keys.
fn image_keys(list: &TrustList) -> [Option<[u8; KEY_LEN]>; crate::trust_list::MAX_KEYS] {
    keys_for(list, Purpose::Image)
}

fn keys_for(
    list: &TrustList,
    purpose: Purpose,
) -> [Option<[u8; KEY_LEN]>; crate::trust_list::MAX_KEYS] {
    let mut keys = [None; crate::trust_list::MAX_KEYS];
    for (slot, key) in keys
        .iter_mut()
        .zip(list.keys().filter(|key| key.purpose == purpose))
    {
        *slot = Some(key.key);
    }
    keys
}

/// Whether `list` names an image key whose ID is `id`.
fn lists_image_key<C: Crypto>(crypto: &C, list: &TrustList, id: &[u8; KEY_ID_LEN]) -> bool {
    image_keys(list)
        .iter()
        .flatten()
        .any(|key| crypto.key_id(key) == *id)
}

/// Whether one of `keys` made a good signature over `list` that the list
/// carries.
fn signed_by_any<'k, C: Crypto>(
    crypto: &C,
    list: &TrustList,
    keys: impl Iterator<Item = &'k [u8; KEY_LEN]>,
) -> bool {
    let signed = list.signed_bytes();
    let signatures: [Option<NamedSignature>; crate::trust_list::MAX_SIGNATURES] = {
        let mut all = [None; crate::trust_list::MAX_SIGNATURES];
        for (slot, signature) in all.iter_mut().zip(list.signatures()) {
            *slot = Some(signature);
        }
        all
    };
    keys.into_iter().any(|key| {
        let id = crypto.key_id(key);
        signatures.iter().flatten().any(|signature| {
            signature.key_id == id && crypto.verify(key, &signed, &signature.signature)
        })
    })
}
