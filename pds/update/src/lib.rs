// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos_update_pd`: the update PD, the
//! only PD that writes a boot slot or the update state - thin over
//! `nowayos_update::updater::Updater`, which decides, with
//! `nowayos-signature`'s ECDSA P-256 and SHA-256 as its checks.
//!
//! **Bundles** arrive on a `nowayos-ipc` ring of chunk records
//! (`nowayos_update::Chunk`), from the TLS PD - in `update-test`, a test PD
//! in its place. Each notification on [`BUNDLES`] is one whole bundle: the
//! PD takes every record waiting, each copied out of the ring before it is
//! decoded, puts the trust list and the manifest back together, and gives
//! the logic each in order: the trust list, the manifest,
//! the image's chunks, then the bundle's end. It prints what became of the
//! bundle - staged for one trial, or refused and why - and notifies back,
//! for the next. A bundle holding the trust list the device holds already
//! goes on to its manifest; one holding an older list is refused.
//!
//! **Storage** is the storage PD's, through protected calls on [`STORAGE`]
//! and one shared page ([`storage`]): reading or writing either copy of the
//! update state, or writing image bytes to a slot - nothing else. What it
//! reads back is decoded like any input.
//!
//! **The trust anchors** are the root keys in the running image, in a
//! read-only region prefilled at build time (`test-trust-anchors.bin` in a
//! test system): the held trust list is checked again against them before
//! it is used.
//!
//! **How the device booted** - the slot running, and a slot's length - the
//! storage PD says, from the bootloader's record ([`storage::BootFacts`]).
//!
//! **The health verdict** on a trial boot: once it
//! has started on trial, it asks for the verdict with a protected call on
//! [`BUNDLES`] ([`HEALTH_ASK`]), whose reply's label is the verdict
//! ([`verdict`]): passed marks the trial slot good and raises the rollback
//! index; failed or late unmarks it, and the device falls back. In
//! `update-trial` the bundle server stands in for the supervisor's
//! heartbeats and the product's own check, and that system gives the
//! update PD's end of the channel `pp`; a system that does not is never
//! asked, as it starts no trial.
//!
//! It asks for a restart by saying so: its systems give it no supervisor
//! channel, so the run ends there. No device, interrupt
//! or physical address; no `unsafe` code; no heap.
//!
//! Channels: 0 to the bundle server, both ways, and called for the health
//! verdict where the system lets it (`pp`); 1 to the storage PD (`pp`).
//! Regions: `nowayos_rw_region_0` and `nowayos_ro_region_1`, the ring from
//! the bundle server (control read-write, messages read-only);
//! `nowayos_rw_region_1`, the storage page; `nowayos_ro_region_0`, the
//! trust anchors.

#![no_std]

pub mod storage;

use nowayos_ipc::Consumer;
use nowayos_region::{ReadWrite, RingRx, Words, read_only, read_write, ring_consumer};
use nowayos_signature::{PublicKey, Sha256, Signature, sha256, verify};
use nowayos_update::chunk::{Chunk, Part, RECORD_LEN as CHUNK_RECORD_LEN};
use nowayos_update::manifest::{self, Manifest};
use nowayos_update::state::{
    COPY_LEN, Slot, StateCopy, StateStorage, StateStore, StorageError, UpdateState,
};
use nowayos_update::trust_list::{self, TrustList};
use nowayos_update::updater::{
    Config, Crypto, ImageHash, Judged, Refusal, SlotStorage, StartError, Started, Updater, Verdict,
};
use nowayos_update::{DIGEST_LEN, KEY_ID_LEN, KEY_LEN, SIGNATURE_LEN};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, MessageInfo, debug_println, with_msg_regs,
    with_msg_regs_mut,
};

use crate::storage::{BootFacts, OK, Request};

/// The channel to the bundle server: "a bundle waits" one way, "taken" the
/// other; and, on a trial boot, the call that asks for the health verdict.
pub const BUNDLES: Channel = Channel::new(0);

/// The channel the PD calls the storage PD on.
pub const STORAGE: Channel = Channel::new(1);

/// The label of the call that asks for the health verdict on [`BUNDLES`].
pub const HEALTH_ASK: u64 = 1;

/// The verdicts the reply to [`HEALTH_ASK`] carries, as its label: passed,
/// failed, late; any other label is no verdict.
pub const VERDICT_PASSED: u64 = 1;
/// See [`VERDICT_PASSED`].
pub const VERDICT_FAILED: u64 = 2;
/// See [`VERDICT_PASSED`].
pub const VERDICT_LATE: u64 = 3;

/// The verdict a reply's label carries, if it is one.
#[must_use]
pub const fn verdict(label: u64) -> Option<Verdict> {
    match label {
        VERDICT_PASSED => Some(Verdict::Passed),
        VERDICT_FAILED => Some(Verdict::Failed),
        VERDICT_LATE => Some(Verdict::Late),
        _ => None,
    }
}

/// The ring's slots: a whole bundle's records fit, as each notification is
/// one bundle - a release image of up to 8,192 chunks less its trust list
/// and manifest, about 3.9 MB. The records' region holds the 4 MiB
/// of slots and the producer's two counters: 6 MiB, in 2 MiB pages.
pub const RING_SLOTS: usize = 8192;

/// The most trust anchors an image holds.
pub const MAX_ANCHORS: usize = 4;

/// What the running image says about the device.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// The product it runs, as manifests name it.
    pub product: &'static str,
}

/// Starts the PD: binds its regions and reads its anchors, then the update
/// state, and says how the device started.
pub fn init(settings: Settings) -> UpdatePd {
    debug_println!("NOWAYOS: update: starting");
    let state = match bind(settings) {
        Ok(state) => state,
        Err(error) => {
            debug_println!("NOWAYOS: update: gave up: {error}; nothing was written");
            return UpdatePd { state: None };
        }
    };
    let window = Window(&state.window);
    match Updater::start(state.config(), Real, StateStore::new(window), window) {
        Ok((updater, started)) => {
            report_state("starting", updater.state(), updater.held_list_refused());
            let how = match started {
                Started::Good => "from the slot marked good",
                Started::OnTrial => "on trial, awaiting the health verdict",
                Started::FellBack => {
                    "from the good slot: the update failed its trial, which is unmarked"
                }
            };
            debug_println!(
                "NOWAYOS: update: slot {} runs, {how}; {} trust anchor(s) in the image",
                slot_name(state.boot.slot),
                state.anchor_count
            );
            if started == Started::OnTrial {
                state.ask_health();
            }
        }
        Err(error) => {
            debug_println!("NOWAYOS: update: gave up: {}", start_error(error));
            return UpdatePd { state: None };
        }
    }
    UpdatePd { state: Some(state) }
}

/// Binds the regions and reads the anchors.
fn bind(settings: Settings) -> Result<State, &'static str> {
    let ring = read_write(0)
        .and_then(|control| ring_consumer(control, read_only(1)?))
        .map_err(|_| "the ring from the bundle server")?;
    // A page: the longest request, a copy of the state, fits (WINDOW_LEN).
    let window = read_write(1)
        .and_then(|region| region.into_words())
        .map_err(|_| "the storage page")?;
    let anchor_words = read_only(0)
        .and_then(|region| region.into_words())
        .map_err(|_| "the trust anchors")?;
    let count = anchor_words
        .load(0)
        .and_then(|count| usize::try_from(count).ok())
        .filter(|count| (1..=MAX_ANCHORS).contains(count))
        .ok_or("the trust anchors: no key, or more than the PD holds")?;
    let mut anchors = [[0u8; KEY_LEN]; MAX_ANCHORS];
    let mut bytes = [0u8; 4 + KEY_LEN * MAX_ANCHORS];
    if !load(&anchor_words, &mut bytes) {
        return Err("the trust anchors' region is too small");
    }
    for (anchor, key) in anchors
        .iter_mut()
        .zip(bytes.get(4..).unwrap_or_default().chunks_exact(KEY_LEN))
        .take(count)
    {
        anchor.copy_from_slice(key);
    }
    let boot = Window(&window)
        .boot()
        .ok_or("the storage PD did not say how the device booted")?;
    Ok(State {
        settings,
        boot,
        ring,
        window,
        anchors,
        anchor_count: count,
        bundle: 0,
    })
}

/// The PD; `None` only if it could not start.
pub struct UpdatePd {
    state: Option<State>,
}

struct State {
    settings: Settings,
    /// The slot running and a slot's length, as the storage PD said.
    boot: BootFacts,
    ring: RingRx<RING_SLOTS, CHUNK_RECORD_LEN>,
    window: Words<ReadWrite>,
    anchors: [[u8; KEY_LEN]; MAX_ANCHORS],
    anchor_count: usize,
    /// Bundles taken so far.
    bundle: u32,
}

impl State {
    fn config(&self) -> Config<'_> {
        Config {
            product: self.settings.product,
            running: self.boot.slot,
            slot_len: self.boot.slot_len,
            anchors: self.anchors.get(..self.anchor_count).unwrap_or_default(),
        }
    }

    /// Takes one bundle from the ring, and says what became of it.
    fn bundle(&mut self) {
        self.bundle = self.bundle.saturating_add(1);
        let number = self.bundle;
        let window = Window(&self.window);
        let (mut updater, _) =
            match Updater::start(self.config(), Real, StateStore::new(window), window) {
                Ok(started) => started,
                Err(error) => {
                    debug_println!(
                        "NOWAYOS: update: bundle {number}: not taken: {}",
                        start_error(error)
                    );
                    return;
                }
            };
        let mut parts = Parts::new();
        let mut outcome: Result<(), Outcome> = Ok(());
        let Ok(mut rx) = Consumer::new(&self.ring) else {
            debug_println!("NOWAYOS: update: bundle {number}: not taken: the ring is broken");
            return;
        };
        loop {
            let record = match rx.try_recv_record() {
                Ok(Some(record)) => *record.as_bytes(),
                Ok(None) => break,
                Err(_) => {
                    outcome = Err(Outcome::Malformed);
                    break;
                }
            };
            // Once refused, the rest of the bundle is drained unread.
            if outcome.is_ok() {
                outcome = parts.take(&mut updater, &record);
            }
        }
        let outcome = outcome.and_then(|()| updater.bundle_end().map_err(Outcome::Refused));
        match outcome {
            Ok(slot) => debug_println!(
                "NOWAYOS: update: bundle {number}: staged in slot {} for one trial; restart wanted",
                slot_name(slot)
            ),
            Err(outcome) => {
                let marked = if updater.state().trial.is_some() {
                    "a slot is marked"
                } else {
                    "no slot marked"
                };
                debug_println!(
                    "NOWAYOS: update: bundle {number}: refused: {}; {marked}",
                    reason(outcome)
                );
            }
        }
    }
}

impl State {
    /// Asks the health check for its verdict on this trial boot, and takes
    /// it.
    fn ask_health(&self) {
        debug_println!("NOWAYOS: update: asking the health check for its verdict");
        let reply = BUNDLES.pp_call(MessageInfo::new(HEALTH_ASK, 0));
        match verdict(reply.label()) {
            Some(verdict) => {
                self.health(verdict);
            }
            None => debug_println!(
                "NOWAYOS: update: the health check gave no verdict; the trial stays marked"
            ),
        }
    }

    /// Takes the health verdict on this boot; true if it was judged.
    fn health(&self, verdict: Verdict) -> bool {
        let window = Window(&self.window);
        let mut updater = match Updater::start(self.config(), Real, StateStore::new(window), window)
        {
            Ok((updater, _)) => updater,
            Err(error) => {
                debug_println!(
                    "NOWAYOS: update: health verdict not taken: {}",
                    start_error(error)
                );
                return false;
            }
        };
        let said = match verdict {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::Late => "came after the deadline",
        };
        let running = slot_name(self.boot.slot);
        match updater.health(verdict) {
            Ok(Judged::MarkedGood { rollback_index }) => {
                debug_println!(
                    "NOWAYOS: update: the health check {said}: slot {running} marked good; rollback index {rollback_index}"
                );
                true
            }
            Ok(Judged::FallBack) => {
                debug_println!(
                    "NOWAYOS: update: the health check {said}: the update failed; slot {running} unmarked; restart wanted, into slot {}",
                    slot_name(self.boot.slot.other())
                );
                true
            }
            Err(refusal) => {
                debug_println!(
                    "NOWAYOS: update: the health check {said}, but no verdict is due: {}",
                    reason(Outcome::Refused(refusal))
                );
                false
            }
        }
    }
}

/// A bundle's trust list and manifest, put back together from their
/// chunks.
struct Parts {
    list: [u8; trust_list::RECORD_LEN],
    list_len: usize,
    manifest: [u8; manifest::RECORD_LEN],
    manifest_len: usize,
}

/// Why a bundle was refused.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    /// A record that is no chunk, or a part not where it belongs.
    Malformed,
    /// The logic refused it.
    Refused(Refusal),
}

impl Parts {
    const fn new() -> Self {
        Self {
            list: [0; trust_list::RECORD_LEN],
            list_len: 0,
            manifest: [0; manifest::RECORD_LEN],
            manifest_len: 0,
        }
    }

    /// Takes one record of the bundle, copied out of the ring.
    fn take<C: Crypto, S: StateStorage, B: SlotStorage>(
        &mut self,
        updater: &mut Updater<'_, C, S, B>,
        record: &[u8],
    ) -> Result<(), Outcome> {
        let chunk = Chunk::decode(record).map_err(|_| Outcome::Malformed)?;
        match chunk.part() {
            Part::TrustList => {
                if !append(&mut self.list, &mut self.list_len, &chunk) {
                    return Err(Outcome::Malformed);
                }
                if self.list_len == trust_list::RECORD_LEN {
                    let list = TrustList::decode(&self.list).map_err(|_| Outcome::Malformed)?;
                    // The list held already: on to the manifest.
                    if updater.trust_list() != Some(&list) {
                        updater.take_trust_list(&list).map_err(Outcome::Refused)?;
                    }
                }
                Ok(())
            }
            Part::Manifest => {
                if !append(&mut self.manifest, &mut self.manifest_len, &chunk) {
                    return Err(Outcome::Malformed);
                }
                if self.manifest_len == manifest::RECORD_LEN {
                    let manifest =
                        Manifest::decode(&self.manifest).map_err(|_| Outcome::Malformed)?;
                    updater.manifest(&manifest).map_err(Outcome::Refused)?;
                }
                Ok(())
            }
            Part::Image => updater.image(&chunk).map_err(Outcome::Refused),
        }
    }
}

/// Puts `chunk`'s bytes where they go in `buf`, if it is the next piece.
fn append(buf: &mut [u8], len: &mut usize, chunk: &Chunk) -> bool {
    let bytes = chunk.bytes();
    let next = u64::try_from(*len).ok();
    let end = len.checked_add(bytes.len());
    match (
        next == Some(chunk.offset()),
        end.and_then(|end| buf.get_mut(*len..end)),
    ) {
        (true, Some(to)) => {
            to.copy_from_slice(bytes);
            *len = end.unwrap_or(*len);
            true
        }
        _ => false,
    }
}

impl Handler for UpdatePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if let Some(state) = self.state.as_mut()
            && channels.contains(BUNDLES)
        {
            state.bundle();
            BUNDLES.notify();
        }
        Ok(())
    }
}

/// `nowayos-signature`'s checks, as the logic asks for them.
struct Real;

struct RealHash(Sha256);

impl ImageHash for RealHash {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(self) -> [u8; DIGEST_LEN] {
        self.0.finish()
    }
}

impl Crypto for Real {
    type Hash = RealHash;

    fn verify(&self, key: &[u8; KEY_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
        match (
            PublicKey::from_coordinates(key),
            Signature::from_bytes(signature),
        ) {
            (Ok(key), Ok(signature)) => verify(&key, message, &signature).is_ok(),
            _ => false,
        }
    }

    fn key_id(&self, key: &[u8; KEY_LEN]) -> [u8; KEY_ID_LEN] {
        let mut sec1 = [0x04; 1 + KEY_LEN];
        if let Some(xy) = sec1.get_mut(1..) {
            xy.copy_from_slice(key);
        }
        sha256(&sec1)
    }

    fn hash(&self) -> RealHash {
        RealHash(Sha256::new())
    }
}

/// The storage PD, through the shared page: both the update state's
/// storage and the slots'.
#[derive(Clone, Copy)]
struct Window<'w>(&'w Words<ReadWrite>);

impl Window<'_> {
    /// Calls the storage PD with `request`; true if it did as asked.
    fn call(self, request: Request) -> bool {
        let words = request.words();
        with_msg_regs_mut(|regs| {
            for (reg, word) in regs.iter_mut().zip(words) {
                *reg = word;
            }
        });
        let reply = STORAGE.pp_call(MessageInfo::new(request.label(), words.len()));
        reply.label() == OK
    }
}

impl Window<'_> {
    /// Asks the storage PD how the device booted.
    fn boot(self) -> Option<BootFacts> {
        if !self.call(Request::Boot) {
            return None;
        }
        let words = with_msg_regs(|regs| {
            let mut words = [0u64; 2];
            for (word, reg) in words.iter_mut().zip(regs.iter()) {
                *word = *reg;
            }
            words
        });
        BootFacts::received(words)
    }
}

impl StateStorage for Window<'_> {
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError> {
        if !self.call(Request::ReadState(copy)) {
            return Err(StorageError);
        }
        // Copied out of the shared page before anything reads it.
        if load(self.0, out) {
            Ok(())
        } else {
            Err(StorageError)
        }
    }

    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError> {
        if store(self.0, bytes) && self.call(Request::WriteState(copy)) {
            Ok(())
        } else {
            Err(StorageError)
        }
    }
}

impl SlotStorage for Window<'_> {
    fn write(&mut self, slot: Slot, offset: u64, bytes: &[u8]) -> Result<(), StorageError> {
        let len = u32::try_from(bytes.len()).map_err(|_| StorageError)?;
        if store(self.0, bytes) && self.call(Request::WriteSlot { slot, offset, len }) {
            Ok(())
        } else {
            Err(StorageError)
        }
    }
}

/// Copies `out.len()` bytes from the start of `words`, little-endian words;
/// false if they do not fit.
pub fn load<A: nowayos_region::Access>(words: &Words<A>, out: &mut [u8]) -> bool {
    for (index, chunk) in out.chunks_mut(4).enumerate() {
        let Some(word) = words.load(index) else {
            return false;
        };
        for (to, from) in chunk.iter_mut().zip(word.to_le_bytes()) {
            *to = from;
        }
    }
    true
}

/// Copies `bytes` to the start of `words`, little-endian words; false if
/// they do not fit.
pub fn store(words: &Words<ReadWrite>, bytes: &[u8]) -> bool {
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        for (to, from) in word.iter_mut().zip(chunk) {
            *to = *from;
        }
        if !words.store(index, u32::from_le_bytes(word)) {
            return false;
        }
    }
    true
}

/// A slot's name, for the log.
const fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "A",
        Slot::B => "B",
    }
}

/// Prints the update state, for the log and the expect file.
fn report_state(when: &str, state: &UpdateState, held_refused: bool) {
    let list = match (&state.trust_list, held_refused) {
        (None, _) => "none held",
        (Some(_), true) => "held but signed by no anchor: not used",
        (Some(_), false) => "held, checked against the anchors",
    };
    let trial = state.trial.map_or("none", |trial| slot_name(trial.slot));
    debug_println!(
        "NOWAYOS: update: {when}: slot {} boots; trial: {trial}; rollback index {}; trust list v{} ({list})",
        slot_name(state.boots),
        state.rollback_index,
        state.trust_list_version()
    );
}

/// Why the logic could not start, for the log.
const fn start_error(error: StartError) -> &'static str {
    match error {
        StartError::State(_) => "the update state could not be read (lost, or storage failed)",
        StartError::UnexpectedSlot => "the running slot is neither the good one nor the trial",
    }
}

/// Why a bundle was refused, for the log.
fn reason(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Malformed => "a record that is no chunk, or a part out of place",
        Outcome::Refused(refusal) => match refusal {
            Refusal::TrustListNotNewer { .. } => "its trust list is older than the one held",
            Refusal::TrustListNotSigned => "its trust list is not signed by a root key held",
            Refusal::ManifestNotSigned => {
                "its manifest is not signed by an image key the trust list names"
            }
            Refusal::Development => "a development manifest under a production trust list",
            Refusal::Rollback { .. } => "its security version is below the rollback index",
            Refusal::NeedsNewerTrustList { .. } => "its manifest needs a newer trust list",
            Refusal::OtherProduct => "its manifest names another product",
            Refusal::TooLong => "its image is longer than a slot",
            Refusal::NotExpected | Refusal::OutOfOrder => "a chunk out of place",
            Refusal::Mismatch => "its image does not match the manifest's length and SHA-256",
            Refusal::Trial => "a trial is running",
            Refusal::Storage(_) => "storage failed",
        },
    }
}
