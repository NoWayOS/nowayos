// SPDX-License-Identifier: MIT OR Apache-2.0
//! The update PD's logic, with real ECDSA P-256 signatures by keys
//! made at test time (never stored): each trust list, manifest, image and
//! verdict rule, the root key's rule, resets at any point, and a
//! property test over random event sequences - the running slot is never
//! written, the rollback index never falls, and no slot is marked whose
//! bytes do not match a signed manifest.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::collections::HashMap;

use nowayos_signature::{PublicKey, Sha256, Signature, sha256, verify};
use nowayos_update::chunk::chunks;
use nowayos_update::manifest::Version;
use nowayos_update::state::{
    COPY_LEN, Slot, StateCopy, StateStorage, StateStore, StorageError, UpdateState, encode_copy,
};
use nowayos_update::updater::{
    Config, Crypto, ImageHash, Judged, Refusal, SlotStorage, StartError, Started, Updater, Verdict,
};
use nowayos_update::{
    DIGEST_LEN, KEY_ID_LEN, KEY_LEN, ListedKey, Manifest, ManifestContents, ManifestFields,
    NamedSignature, Part, Purpose, SIGNATURE_LEN, TrustList, TrustListContents,
};
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer;
use proptest::prelude::*;

const PRODUCT: &str = "kiosk";
const SLOT_LEN: u64 = 4_096;

/// `nowayos-signature`, as a PD gives it.
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
        let (Ok(key), Ok(signature)) = (
            PublicKey::from_coordinates(key),
            Signature::from_bytes(signature),
        ) else {
            return false;
        };
        verify(&key, message, &signature).is_ok()
    }

    fn key_id(&self, key: &[u8; KEY_LEN]) -> [u8; KEY_ID_LEN] {
        let mut sec1 = [0x04; 65];
        sec1[1..].copy_from_slice(key);
        sha256(&sec1)
    }

    fn hash(&self) -> RealHash {
        RealHash(Sha256::new())
    }
}

/// A key made now, from `seed`: never stored.
struct Key {
    signing: SigningKey,
    public: [u8; KEY_LEN],
}

impl Key {
    /// A new key, from the process's own randomness: made at test time,
    /// never stored, different at every run.
    fn fresh() -> Self {
        use std::hash::{BuildHasher, Hasher};
        loop {
            let mut bytes = [0u8; 32];
            for chunk in bytes.chunks_mut(8) {
                let random = std::collections::hash_map::RandomState::new()
                    .build_hasher()
                    .finish();
                chunk.copy_from_slice(&random.to_le_bytes());
            }
            // Any scalar from 1 to n - 1; the rare other draw, again.
            if let Ok(signing) = SigningKey::from_slice(&bytes) {
                return Self::new(signing);
            }
        }
    }

    fn new(signing: SigningKey) -> Self {
        let point = signing.verifying_key().to_sec1_point(false);
        let public = point.as_bytes()[1..].try_into().unwrap();
        Self { signing, public }
    }

    fn id(&self) -> [u8; KEY_ID_LEN] {
        Real.key_id(&self.public)
    }

    fn sign(&self, message: &[u8]) -> NamedSignature {
        let signature: p256::ecdsa::Signature = self.signing.sign(message);
        NamedSignature {
            key_id: self.id(),
            signature: signature.to_bytes().into(),
        }
    }
}

/// A trust list of `version` naming `roots` and `images`, signed by
/// `signers`.
fn list(
    version: u32,
    development: bool,
    roots: &[&Key],
    images: &[&Key],
    signers: &[&Key],
) -> TrustList {
    let keys: Vec<ListedKey> = roots
        .iter()
        .map(|key| ListedKey {
            purpose: Purpose::TrustList,
            key: key.public,
        })
        .chain(images.iter().map(|key| ListedKey {
            purpose: Purpose::Image,
            key: key.public,
        }))
        .collect();
    let contents = TrustListContents::new(version, development, &keys).unwrap();
    let signed = contents.signed_bytes();
    let signatures: Vec<NamedSignature> = signers.iter().map(|key| key.sign(&signed)).collect();
    TrustList::new(contents, &signatures).unwrap()
}

/// What a manifest says, for [`manifest`].
#[derive(Clone, Copy, Debug)]
struct Release {
    product: &'static str,
    security_version: u32,
    trust_list_version: u32,
    development: bool,
    image_len: u64,
    image_sha256: [u8; DIGEST_LEN],
}

impl Release {
    fn of(image: &[u8], security_version: u32) -> Self {
        Self {
            product: PRODUCT,
            security_version,
            trust_list_version: 1,
            development: false,
            image_len: u64::try_from(image.len()).unwrap(),
            image_sha256: sha256(image),
        }
    }
}

fn manifest(release: Release, signer: &Key) -> Manifest {
    let fields = ManifestFields {
        product: release.product,
        version: Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        security_version: release.security_version,
        trust_list_version: release.trust_list_version,
        development: release.development,
        image_len: release.image_len,
        image_sha256: release.image_sha256,
        reference_values: &[],
    };
    let contents = ManifestContents::new(&fields).unwrap();
    let signature = signer.sign(&contents.signed_bytes());
    Manifest::new(contents, &signature)
}

/// The state's two copies and the two slots, in memory; the next state
/// write may be cut short.
#[derive(Clone, Default)]
struct Device {
    copies: Vec<[u8; COPY_LEN]>,
    cut: Option<usize>,
    slots: HashMap<Slot, Vec<u8>>,
    /// Every slot written, in order.
    written: Vec<Slot>,
}

impl Device {
    fn with(state: &UpdateState) -> Self {
        let copy = encode_copy(1, state).unwrap();
        let mut slots = HashMap::new();
        slots.insert(Slot::A, b"the factory image".to_vec());
        slots.insert(Slot::B, Vec::new());
        Self {
            copies: vec![copy, [0; COPY_LEN]],
            cut: None,
            slots,
            written: Vec::new(),
        }
    }
}

/// The device's state copies, as [`StateStorage`].
struct States<'d>(&'d mut Device);

impl StateStorage for States<'_> {
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError> {
        *out = self.0.copies[usize::from(copy == StateCopy::B)];
        Ok(())
    }

    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError> {
        let target = &mut self.0.copies[usize::from(copy == StateCopy::B)];
        match self.0.cut.take() {
            Some(cut) => {
                target[..cut].copy_from_slice(&bytes[..cut]);
                Err(StorageError)
            }
            None => {
                *target = *bytes;
                Ok(())
            }
        }
    }
}

/// The device's slots, as [`SlotStorage`].
struct Slots<'d>(&'d mut Device);

impl SlotStorage for Slots<'_> {
    fn write(&mut self, slot: Slot, offset: u64, bytes: &[u8]) -> Result<(), StorageError> {
        self.0.written.push(slot);
        let data = self.0.slots.entry(slot).or_default();
        let offset = usize::try_from(offset).unwrap();
        if data.len() < offset + bytes.len() {
            data.resize(offset + bytes.len(), 0);
        }
        data[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

/// One shared device: a `RefCell` so the state and the slots can both be
/// borrowed by the logic.
struct Shared(std::rc::Rc<std::cell::RefCell<Device>>);

impl StateStorage for Shared {
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError> {
        States(&mut self.0.borrow_mut()).read(copy, out)
    }

    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError> {
        States(&mut self.0.borrow_mut()).write(copy, bytes)
    }
}

impl SlotStorage for Shared {
    fn write(&mut self, slot: Slot, offset: u64, bytes: &[u8]) -> Result<(), StorageError> {
        Slots(&mut self.0.borrow_mut()).write(slot, offset, bytes)
    }
}

type Logic<'a> = Updater<'a, Real, Shared, Shared>;

/// The world: a root key in the image, two image keys, and the device.
struct World {
    root: Key,
    image: Key,
    other_image: Key,
    stranger: Key,
    anchors: [[u8; KEY_LEN]; 1],
    device: std::rc::Rc<std::cell::RefCell<Device>>,
}

impl World {
    /// A device running slot A, rollback index 2, holding a list of
    /// version 1 that names `image` - unless `held` is false.
    fn new(held: bool) -> Self {
        let root = Key::fresh();
        let image = Key::fresh();
        let other_image = Key::fresh();
        let stranger = Key::fresh();
        let anchors = [root.public];
        let state = UpdateState {
            boots: Slot::A,
            trial: None,
            rollback_index: 2,
            trust_list: held.then(|| list(1, false, &[&root], &[&image], &[&root])),
        };
        Self {
            root,
            image,
            other_image,
            stranger,
            anchors,
            device: std::rc::Rc::new(std::cell::RefCell::new(Device::with(&state))),
        }
    }

    /// The bootloader's choice: the trial slot if one is marked, else the
    /// good one.
    fn booted(&self) -> Slot {
        let state = self.state();
        state.trial.map_or(state.boots, |trial| trial.slot)
    }

    fn state(&self) -> UpdateState {
        StateStore::new(Shared(self.device.clone())).read().unwrap()
    }

    fn start(&self, running: Slot) -> Result<(Logic<'_>, Started), StartError> {
        let config = Config {
            product: PRODUCT,
            running,
            slot_len: SLOT_LEN,
            anchors: &self.anchors,
        };
        Updater::start(
            config,
            Real,
            StateStore::new(Shared(self.device.clone())),
            Shared(self.device.clone()),
        )
    }

    fn slot(&self, slot: Slot) -> Vec<u8> {
        self.device.borrow().slots[&slot].clone()
    }
}

/// Sends `image` in chunks, then the bundle's end.
fn send_image(logic: &mut Logic<'_>, image: &[u8]) -> Result<Slot, Refusal> {
    for chunk in chunks(Part::Image, image) {
        logic.image(&chunk)?;
    }
    logic.bundle_end()
}

/// A small image: one chunk.
const IMAGE: &[u8] = b"release 1.1 of the kiosk image, a stand-in for a system image";

#[test]
fn a_newer_list_signed_by_the_held_root_is_taken_and_others_refused() {
    let world = World::new(true);
    let (mut logic, started) = world.start(Slot::A).unwrap();
    assert_eq!(started, Started::Good);
    let held = logic.trust_list().cloned();
    // Not newer.
    let same = list(1, false, &[&world.root], &[&world.image], &[&world.root]);
    assert_eq!(
        logic.take_trust_list(&same),
        Err(Refusal::TrustListNotNewer {
            held: 1,
            offered: 1
        })
    );
    // Signed by a key that is no root.
    let forged = list(
        2,
        false,
        &[&world.root],
        &[&world.image],
        &[&world.stranger],
    );
    assert_eq!(
        logic.take_trust_list(&forged),
        Err(Refusal::TrustListNotSigned)
    );
    // A new root, signed by the old one alone.
    let half = list(
        2,
        false,
        &[&world.stranger],
        &[&world.image],
        &[&world.root],
    );
    assert_eq!(
        logic.take_trust_list(&half),
        Err(Refusal::TrustListNotSigned)
    );
    assert_eq!(logic.trust_list().cloned(), held);
    assert_eq!(world.state().trust_list, held);
    // Newer, signed by the root.
    let newer = list(2, false, &[&world.root], &[&world.image], &[&world.root]);
    assert!(logic.take_trust_list(&newer).is_ok());
    assert_eq!(world.state().trust_list, Some(newer));
    // A rotation: the new root and the old both sign.
    let rotated = list(
        3,
        false,
        &[&world.stranger],
        &[&world.image],
        &[&world.root, &world.stranger],
    );
    assert!(logic.take_trust_list(&rotated).is_ok());
    assert_eq!(world.state().trust_list_version(), 3);
}

#[test]
fn a_list_removing_the_staged_updates_key_withdraws_it() {
    let world = World::new(true);
    let (mut logic, _) = world.start(Slot::A).unwrap();
    logic
        .manifest(&manifest(Release::of(IMAGE, 3), &world.image))
        .unwrap();
    assert_eq!(send_image(&mut logic, IMAGE), Ok(Slot::B));
    assert!(world.state().trial.is_some());
    let without = list(
        2,
        false,
        &[&world.root],
        &[&world.other_image],
        &[&world.root],
    );
    let taken = logic.take_trust_list(&without).unwrap();
    assert!(taken.withdrawn);
    assert_eq!(world.state().trial, None);
}

#[test]
fn each_manifest_rule_is_refused_with_its_own_reason() {
    let world = World::new(true);
    let (mut logic, _) = world.start(Slot::A).unwrap();
    let good = Release::of(IMAGE, 3);
    let cases: Vec<(Manifest, Refusal)> = vec![
        (
            manifest(good, &world.other_image),
            Refusal::ManifestNotSigned,
        ),
        (manifest(good, &world.root), Refusal::ManifestNotSigned),
        (
            manifest(
                Release {
                    development: true,
                    ..good
                },
                &world.image,
            ),
            Refusal::Development,
        ),
        (
            manifest(
                Release {
                    security_version: 1,
                    ..good
                },
                &world.image,
            ),
            Refusal::Rollback {
                index: 2,
                offered: 1,
            },
        ),
        (
            manifest(
                Release {
                    trust_list_version: 2,
                    ..good
                },
                &world.image,
            ),
            Refusal::NeedsNewerTrustList { held: 1, needed: 2 },
        ),
        (
            manifest(
                Release {
                    product: "pump",
                    ..good
                },
                &world.image,
            ),
            Refusal::OtherProduct,
        ),
        (
            manifest(
                Release {
                    image_len: SLOT_LEN + 1,
                    ..good
                },
                &world.image,
            ),
            Refusal::TooLong,
        ),
    ];
    for (offered, refusal) in cases {
        assert_eq!(logic.manifest(&offered), Err(refusal));
        assert_eq!(
            logic.bundle_end(),
            Err(Refusal::NotExpected),
            "nothing staged"
        );
    }
    // A signature over other bytes.
    let mut tampered = manifest(good, &world.image).signature();
    tampered.signature[10] ^= 1;
    let contents = ManifestContents::new(&ManifestFields {
        product: PRODUCT,
        version: Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        security_version: 3,
        trust_list_version: 1,
        development: false,
        image_len: u64::try_from(IMAGE.len()).unwrap(),
        image_sha256: sha256(IMAGE),
        reference_values: &[],
    })
    .unwrap();
    assert_eq!(
        logic.manifest(&Manifest::new(contents, &tampered)),
        Err(Refusal::ManifestNotSigned)
    );
    assert!(world.device.borrow().written.is_empty(), "no slot written");
}

#[test]
fn only_the_other_slot_is_written_and_marked_only_if_the_image_matches() {
    let world = World::new(true);
    let (mut logic, _) = world.start(Slot::A).unwrap();
    // A hash that does not match: written, but not marked.
    let wrong = Release {
        image_sha256: [7; DIGEST_LEN],
        ..Release::of(IMAGE, 3)
    };
    logic.manifest(&manifest(wrong, &world.image)).unwrap();
    assert_eq!(send_image(&mut logic, IMAGE), Err(Refusal::Mismatch));
    assert_eq!(world.state().trial, None);
    // Short by a byte: not marked.
    logic
        .manifest(&manifest(Release::of(IMAGE, 3), &world.image))
        .unwrap();
    assert_eq!(
        send_image(&mut logic, &IMAGE[..IMAGE.len() - 1]),
        Err(Refusal::Mismatch)
    );
    assert_eq!(world.state().trial, None);
    // Whole and matching: marked for one trial.
    logic
        .manifest(&manifest(Release::of(IMAGE, 3), &world.image))
        .unwrap();
    assert_eq!(send_image(&mut logic, IMAGE), Ok(Slot::B));
    let trial = world.state().trial.unwrap();
    assert_eq!((trial.slot, trial.security_version), (Slot::B, 3));
    assert_eq!(world.slot(Slot::B), IMAGE);
    assert!(
        world
            .device
            .borrow()
            .written
            .iter()
            .all(|&slot| slot == Slot::B)
    );
    assert_eq!(world.slot(Slot::A), b"the factory image");
}

#[test]
fn a_trial_passes_and_raises_the_index_or_fails_and_falls_back() {
    for verdict in [Verdict::Passed, Verdict::Failed, Verdict::Late] {
        let world = World::new(true);
        {
            let (mut logic, _) = world.start(Slot::A).unwrap();
            logic
                .manifest(&manifest(Release::of(IMAGE, 5), &world.image))
                .unwrap();
            send_image(&mut logic, IMAGE).unwrap();
        }
        // The restart: the bootloader boots the trial slot once.
        let (mut logic, started) = world.start(world.booted()).unwrap();
        assert_eq!(started, Started::OnTrial);
        // No update is taken during a trial.
        assert_eq!(
            logic.manifest(&manifest(Release::of(IMAGE, 6), &world.image)),
            Err(Refusal::Trial)
        );
        let judged = logic.health(verdict).unwrap();
        let state = world.state();
        assert_eq!(state.trial, None);
        if verdict == Verdict::Passed {
            assert_eq!(judged, Judged::MarkedGood { rollback_index: 5 });
            assert_eq!((state.boots, state.rollback_index), (Slot::B, 5));
        } else {
            assert_eq!(judged, Judged::FallBack);
            assert_eq!((state.boots, state.rollback_index), (Slot::A, 2));
        }
    }
}

#[test]
fn a_trial_the_bootloader_fell_back_from_is_unmarked() {
    let world = World::new(true);
    {
        let (mut logic, _) = world.start(Slot::A).unwrap();
        logic
            .manifest(&manifest(Release::of(IMAGE, 5), &world.image))
            .unwrap();
        send_image(&mut logic, IMAGE).unwrap();
    }
    // The trial boot hung; the bootloader booted the good slot.
    let (_, started) = world.start(Slot::A).unwrap();
    assert_eq!(started, Started::FellBack);
    assert_eq!(world.state().trial, None);
    assert_eq!(world.state().rollback_index, 2);
}

#[test]
fn a_held_list_no_anchor_signed_is_not_used_and_reported() {
    let world = World::new(false);
    let forged = list(
        1,
        false,
        &[&world.stranger],
        &[&world.other_image],
        &[&world.stranger],
    );
    let state = UpdateState {
        boots: Slot::A,
        trial: None,
        rollback_index: 0,
        trust_list: Some(forged),
    };
    *world.device.borrow_mut() = Device::with(&state);
    let (mut logic, _) = world.start(Slot::A).unwrap();
    assert!(logic.held_list_refused());
    assert!(logic.trust_list().is_none());
    // Its image key is no use.
    assert_eq!(
        logic.manifest(&manifest(Release::of(IMAGE, 3), &world.other_image)),
        Err(Refusal::ManifestNotSigned)
    );
}

#[test]
fn a_lost_state_or_an_unexpected_slot_starts_nothing() {
    let world = World::new(true);
    assert!(matches!(
        world.start(Slot::B),
        Err(StartError::UnexpectedSlot)
    ));
    world.device.borrow_mut().copies = vec![[0; COPY_LEN]; 2];
    assert!(matches!(world.start(Slot::A), Err(StartError::State(_))));
}

#[test]
fn a_reset_at_any_point_leaves_the_running_slot_booting_and_the_other_unmarked() {
    let image: Vec<u8> = (0..1_500u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let chunk_count = chunks(Part::Image, &image).count();
    // A reset after each chunk, and during the state write that marks it.
    for stop in 0..=chunk_count {
        let world = World::new(true);
        {
            let (mut logic, _) = world.start(Slot::A).unwrap();
            logic
                .manifest(&manifest(Release::of(&image, 3), &world.image))
                .unwrap();
            for chunk in chunks(Part::Image, &image).take(stop) {
                logic.image(&chunk).unwrap();
            }
            if stop == chunk_count {
                world.device.borrow_mut().cut = Some(COPY_LEN / 2);
                assert!(matches!(logic.bundle_end(), Err(Refusal::Storage(_))));
            }
        }
        // The device restarts.
        assert_eq!(world.booted(), Slot::A);
        let (_, started) = world.start(Slot::A).unwrap();
        assert_eq!(started, Started::Good);
        assert_eq!(world.state().trial, None);
    }
}

// The property test: random events over a small world.

#[derive(Clone, Debug)]
enum Event {
    List(usize),
    Manifest(usize),
    /// A manifest, then the whole image it names.
    Release(usize),
    Image {
        image: usize,
        corrupt: bool,
    },
    Chunk {
        image: usize,
        at: usize,
    },
    End,
    Restart,
    Health(Verdict),
    CutNextStateWrite(usize),
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        2 => (0usize..4).prop_map(Event::List),
        2 => (0usize..6).prop_map(Event::Manifest),
        4 => (0usize..6).prop_map(Event::Release),
        3 => ((0usize..2), any::<bool>()).prop_map(|(image, corrupt)| Event::Image { image, corrupt }),
        1 => ((0usize..2), 0usize..4).prop_map(|(image, at)| Event::Chunk { image, at }),
        2 => Just(Event::End),
        3 => Just(Event::Restart),
        3 => prop_oneof![Just(Verdict::Passed), Just(Verdict::Failed), Just(Verdict::Late)].prop_map(Event::Health),
        1 => (0..COPY_LEN).prop_map(Event::CutNextStateWrite),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_events_never_write_the_running_slot_lower_the_index_or_mark_a_bad_image(
        events in prop::collection::vec(event(), 5..40),
    ) {
        let world = World::new(true);
        let images: [Vec<u8>; 2] = [IMAGE.to_vec(), (0..1_200u32).map(|i| u8::try_from(i % 253).unwrap()).collect()];
        let lists = [
            list(2, false, &[&world.root], &[&world.image, &world.other_image], &[&world.root]),
            list(3, false, &[&world.root], &[&world.other_image], &[&world.root]),
            list(4, true, &[&world.root], &[&world.image], &[&world.stranger]),
            list(1, false, &[&world.root], &[&world.image], &[&world.root]),
        ];
        let manifests = [
            manifest(Release::of(&images[0], 3), &world.image),
            manifest(Release::of(&images[1], 4), &world.other_image),
            manifest(Release::of(&images[0], 1), &world.image),
            manifest(Release::of(&images[1], 9), &world.stranger),
            manifest(Release { development: true, ..Release::of(&images[1], 5) }, &world.image),
            manifest(Release { trust_list_version: 3, ..Release::of(&images[0], 6) }, &world.other_image),
        ];
        // The image each manifest names.
        const RELEASE_IMAGE: [usize; 6] = [0, 1, 0, 1, 1, 0];
        // What each signed image hashes to.
        let good: Vec<[u8; DIGEST_LEN]> = images.iter().map(|image| sha256(image)).collect();
        let mut running = Slot::A;
        let mut index = world.state().rollback_index;
        let mut logic = world.start(running).unwrap().0;
        for event in events {
            let written_before = world.device.borrow().written.len();
            match event {
                Event::List(i) => { let _ = logic.take_trust_list(&lists[i]); }
                Event::Manifest(i) => { let _ = logic.manifest(&manifests[i]); }
                Event::Release(i) => {
                    if logic.manifest(&manifests[i]).is_ok() {
                        let _ = send_image(&mut logic, &images[RELEASE_IMAGE[i]]);
                    }
                }
                Event::Image { image, corrupt } => {
                    let mut bytes = images[image].clone();
                    if corrupt { bytes[7] ^= 0x40; }
                    let _ = send_image(&mut logic, &bytes);
                }
                Event::Chunk { image, at } => {
                    if let Some(chunk) = chunks(Part::Image, &images[image]).nth(at) {
                        let _ = logic.image(&chunk);
                    }
                }
                Event::End => { let _ = logic.bundle_end(); }
                Event::Restart => {
                    drop(logic);
                    running = world.booted();
                    logic = match world.start(running) {
                        Ok((logic, _)) => logic,
                        Err(error) => panic!("restart refused: {error:?}"),
                    };
                }
                Event::Health(verdict) => { let _ = logic.health(verdict); }
                Event::CutNextStateWrite(cut) => world.device.borrow_mut().cut = Some(cut),
            }
            // The running slot is never written.
            let device = world.device.borrow();
            prop_assert!(device.written[written_before..].iter().all(|&slot| slot != running));
            drop(device);
            // The rollback index never falls.
            let state = world.state();
            prop_assert!(state.rollback_index >= index);
            index = state.rollback_index;
            // A marked trial slot holds exactly an image a signed manifest
            // named.
            if let Some(trial) = state.trial {
                let bytes = world.slot(trial.slot);
                let marked = good.iter().zip(&images).any(|(hash, image)| {
                    bytes.len() >= image.len() && sha256(&bytes[..image.len()]) == *hash
                });
                prop_assert!(marked, "slot {:?} marked with bytes no manifest signed", trial.slot);
            }
        }
    }
}
