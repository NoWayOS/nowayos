// SPDX-License-Identifier: MIT OR Apache-2.0
//! The records `tools/update-bundles` signs with OpenSSL, read by
//! nowayos-update's decoders and checked with NoWayOS's RustCrypto P-256:
//! every record decodes, every OpenSSL signature verifies - so a
//! signature from another implementation, as an HSM's will be, is shown to
//! verify - and each deliberately bad bundle is refused by the update PD's
//! logic for its intended reason.
//!
//! The tool runs first, making keys in a temporary folder deleted when it
//! ends; this test reads only public keys and signed records. It needs
//! python3 and openssl, as the dev image has.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use nowayos_signature::{PublicKey, Sha256, Signature, sha256, verify};
use nowayos_update::chunk::RECORD_LEN as CHUNK_RECORD_LEN;
use nowayos_update::state::{
    COPY_LEN, Slot, StateCopy, StateStorage, StateStore, StorageError, UpdateState, encode_copy,
};
use nowayos_update::updater::{Config, Crypto, ImageHash, Refusal, SlotStorage, Updater};
use nowayos_update::{
    Chunk, DIGEST_LEN, KEY_ID_LEN, KEY_LEN, Manifest, Part, Purpose, SIGNATURE_LEN, TrustList,
};

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

/// The state's copies, in memory.
struct States([[u8; COPY_LEN]; 2]);

impl StateStorage for States {
    fn read(&mut self, copy: StateCopy, out: &mut [u8; COPY_LEN]) -> Result<(), StorageError> {
        *out = self.0[usize::from(copy == StateCopy::B)];
        Ok(())
    }

    fn write(&mut self, copy: StateCopy, bytes: &[u8; COPY_LEN]) -> Result<(), StorageError> {
        self.0[usize::from(copy == StateCopy::B)] = *bytes;
        Ok(())
    }
}

/// The slots, which only count what is written.
#[derive(Default)]
struct Slots(Vec<(Slot, u64, usize)>);

impl SlotStorage for Slots {
    fn write(&mut self, slot: Slot, offset: u64, bytes: &[u8]) -> Result<(), StorageError> {
        self.0.push((slot, offset, bytes.len()));
        Ok(())
    }
}

/// Runs the tool into a fresh folder, and returns it.
fn made() -> PathBuf {
    let out = std::env::temp_dir().join(format!("nowayos-update-bundles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let tool = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/update-bundles");
    let status = Command::new("python3")
        .arg(&tool)
        .arg("--out")
        .arg(&out)
        .status()
        .expect("python3 runs tools/update-bundles");
    assert!(status.success(), "tools/update-bundles failed");
    out
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// A bundle's chunks, decoded, and its three parts put back together.
fn parts(bundle: &[u8]) -> (TrustList, Manifest, Vec<Chunk>) {
    assert_eq!(bundle.len() % CHUNK_RECORD_LEN, 0);
    let chunks: Vec<Chunk> = bundle
        .chunks(CHUNK_RECORD_LEN)
        .map(|record| Chunk::decode(record).unwrap())
        .collect();
    let whole = |part: Part| {
        let mut bytes = Vec::new();
        for chunk in chunks.iter().filter(|chunk| chunk.part() == part) {
            assert_eq!(
                chunk.offset(),
                u64::try_from(bytes.len()).unwrap(),
                "in order"
            );
            bytes.extend_from_slice(chunk.bytes());
        }
        bytes
    };
    let list = TrustList::decode(&whole(Part::TrustList)).unwrap();
    let manifest = Manifest::decode(&whole(Part::Manifest)).unwrap();
    let image = chunks
        .into_iter()
        .filter(|chunk| chunk.part() == Part::Image)
        .collect();
    (list, manifest, image)
}

/// Whether a key the list names, or the anchor, made `list`'s every
/// signature - by OpenSSL - and NoWayOS's check says so.
fn list_signatures_verify(list: &TrustList, anchor: &[u8; KEY_LEN]) -> bool {
    let signed = list.signed_bytes();
    list.signatures().all(|signature| {
        let key = list
            .keys()
            .map(|listed| listed.key)
            .chain(std::iter::once(*anchor))
            .find(|key| Real.key_id(key) == signature.key_id)
            .expect("each signature names a key the test knows");
        Real.verify(&key, &signed, &signature.signature)
    })
}

#[test]
fn every_record_decodes_every_openssl_signature_verifies_and_each_bad_bundle_is_refused_for_its_reason()
 {
    let dir = made();
    let index = String::from_utf8(read(&dir, "bundles.json")).unwrap();
    let anchor: [u8; KEY_LEN] = read(&dir, "anchors.bin").try_into().unwrap();

    // Every trust list decodes and its OpenSSL signature verifies.
    let mut lists = Vec::new();
    for name in ["list-v0.rec", "list-v1.rec", "list-v2.rec"] {
        let list = TrustList::decode(&read(&dir, name)).unwrap();
        assert!(list_signatures_verify(&list, &anchor), "{name}");
        lists.push(list);
    }
    let held = lists[1].clone();
    assert_eq!(held.version(), 1);
    // The image keys the lists name, all of them.
    let image_keys: Vec<[u8; KEY_LEN]> = lists
        .iter()
        .flat_map(|list| {
            list.keys()
                .filter(|key| key.purpose == Purpose::Image)
                .map(|key| key.key)
                .collect::<Vec<_>>()
        })
        .collect();

    let cases: [(&str, Option<Refusal>); 8] = [
        ("good", None),
        ("unsigned", Some(Refusal::ManifestNotSigned)),
        ("unknown-key", Some(Refusal::ManifestNotSigned)),
        ("removed-key", Some(Refusal::ManifestNotSigned)),
        (
            "list-version",
            Some(Refusal::TrustListNotNewer {
                held: 1,
                offered: 0,
            }),
        ),
        ("development", Some(Refusal::Development)),
        (
            "security-version",
            Some(Refusal::Rollback {
                index: 3,
                offered: 1,
            }),
        ),
        ("hash", Some(Refusal::Mismatch)),
    ];
    for (name, expected) in cases {
        assert!(
            index.contains(&format!("\"name\": \"{name}\"")),
            "{name} in bundles.json"
        );
        let (list, manifest, image) = parts(&read(&dir, &format!("{name}.bundle")));
        assert!(list_signatures_verify(&list, &anchor), "{name}'s list");
        // The manifest's OpenSSL signature verifies whenever a key a list
        // names made it - all but the zeros and the stranger's.
        let signature = manifest.signature();
        if let Some(key) = image_keys
            .iter()
            .find(|key| Real.key_id(key) == signature.key_id)
            && name != "unsigned"
        {
            assert!(
                Real.verify(key, &manifest.signed_bytes(), &signature.signature),
                "{name}'s manifest"
            );
        }

        // A device holding list v1 and rollback index 3, running slot A.
        let state = UpdateState {
            boots: Slot::A,
            trial: None,
            rollback_index: 3,
            trust_list: Some(held.clone()),
        };
        let store = StateStore::new(States([encode_copy(1, &state).unwrap(), [0; COPY_LEN]]));
        let anchors = [anchor];
        let config = Config {
            product: "kiosk",
            running: Slot::A,
            slot_len: 1 << 20,
            anchors: &anchors,
        };
        let (mut logic, _) = Updater::start(config, Real, store, Slots::default()).unwrap();
        assert!(!logic.held_list_refused());
        let result = logic
            .take_trust_list(&list)
            .map(|_| ())
            .and_then(|()| logic.manifest(&manifest))
            .and_then(|()| image.iter().try_for_each(|chunk| logic.image(chunk)))
            .and_then(|()| logic.bundle_end());
        match expected {
            None => assert_eq!(result, Ok(Slot::B), "{name}"),
            Some(refusal) => assert_eq!(result, Err(refusal), "{name}"),
        }
    }
    // The provisioned state update-test's storage stand-in starts with,
    // as the tool writes it: one copy that decodes.
    let copy: [u8; COPY_LEN] = read(&dir, "state-initial.bin").try_into().unwrap();
    assert_eq!(
        nowayos_update::state::decode_copy(&copy),
        Ok((
            1,
            UpdateState {
                boots: Slot::A,
                trial: None,
                rollback_index: 3,
                trust_list: Some(held),
            }
        ))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
