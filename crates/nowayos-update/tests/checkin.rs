// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for the check-in record and the deployment record: every
//! byte string either decodes and encodes back to the same bytes, or is
//! refused with a typed error - each rule with its own - and nothing
//! panics; the crate's decoders and `nowayos-ipc`'s agree on every input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_ipc::{DecodeError, HEADER_LEN, Message, decode_exact, encode};
use nowayos_update::deployment::SIGNED_LEN;
use nowayos_update::manifest::{is_board_character, is_product_character};
use nowayos_update::{
    CheckIn, CheckInError, CheckInFields, Deployment, DeploymentContents, DeploymentError,
    DeploymentFields, Instruction, NamedSignature, Version,
};
use proptest::prelude::*;

const CHECKIN_LEN: usize = HEADER_LEN + CheckIn::PAYLOAD_LEN;
const DEPLOYMENT_LEN: usize = HEADER_LEN + Deployment::PAYLOAD_LEN;

/// A name of 1 to 32 of `alphabet`'s characters.
fn name(alphabet: &'static [u8]) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(alphabet), 1..=32)
        .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

const PRODUCT_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";
const BOARD_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-_";

fn checkin() -> impl Strategy<Value = CheckIn> {
    (
        name(PRODUCT_CHARACTERS),
        name(BOARD_CHARACTERS),
        any::<(u16, u16, u16)>(),
        any::<(u32, u32, u16)>(),
        any::<([u8; 32], [u8; 32], [u8; 32])>(),
    )
        .prop_map(
            |(
                product,
                board,
                (major, minor, patch),
                (security, list, last),
                (manifest, nonce, device),
            )| {
                CheckIn::new(&CheckInFields {
                    product: &product,
                    board: &board,
                    version: Version {
                        major,
                        minor,
                        patch,
                    },
                    security_version: security,
                    trust_list_version: list,
                    last_result: last,
                    running_manifest: manifest,
                    nonce,
                    device,
                })
                .unwrap()
            },
        )
}

fn deployment() -> impl Strategy<Value = Deployment> {
    (
        name(PRODUCT_CHARACTERS),
        name(BOARD_CHARACTERS),
        any::<[u8; 32]>(),
        any::<u32>(),
        prop_oneof![
            Just(Instruction::Current),
            any::<[u8; 32]>().prop_map(Instruction::Install)
        ],
        any::<([u8; 32], [u8; 32], [u8; 32])>(),
    )
        .prop_map(
            |(product, board, nonce, cohort, instruction, (key_id, r, s))| {
                let contents = DeploymentContents::new(&DeploymentFields {
                    product: &product,
                    board: &board,
                    nonce,
                    cohort,
                    instruction,
                })
                .unwrap();
                let mut signature = [0u8; 64];
                signature[..32].copy_from_slice(&r);
                signature[32..].copy_from_slice(&s);
                Deployment::new(contents, &NamedSignature { key_id, signature })
            },
        )
}

fn encoded<M: Message>(message: &M) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN + M::PAYLOAD_LEN];
    assert_eq!(encode(message, &mut out), Ok(out.len()));
    out
}

fn checkin_both(bytes: &[u8]) -> Result<CheckIn, CheckInError> {
    let decoded = CheckIn::decode(bytes);
    assert_eq!(
        decoded.clone().map_err(CheckInError::to_decode_error),
        decode_exact::<CheckIn>(bytes),
        "the two decoders disagree"
    );
    if let Ok(checkin) = &decoded {
        assert_eq!(
            encoded(checkin),
            bytes,
            "a decoded check-in encodes differently"
        );
        assert!(checkin.product().bytes().all(is_product_character));
        assert!(checkin.board().bytes().all(is_board_character));
        assert!(!checkin.product().is_empty() && !checkin.board().is_empty());
    }
    decoded
}

fn deployment_both(bytes: &[u8]) -> Result<Deployment, DeploymentError> {
    let decoded = Deployment::decode(bytes);
    assert_eq!(
        decoded.clone().map_err(DeploymentError::to_decode_error),
        decode_exact::<Deployment>(bytes),
        "the two decoders disagree"
    );
    if let Ok(record) = &decoded {
        assert_eq!(
            encoded(record),
            bytes,
            "a decoded record encodes differently"
        );
        assert_eq!(record.signed_bytes()[..], bytes[..SIGNED_LEN]);
        assert!(record.product().bytes().all(is_product_character));
        assert!(record.board().bytes().all(is_board_character));
        if record.instruction() == Instruction::Current {
            assert!(
                bytes[HEADER_LEN + 104..HEADER_LEN + 136]
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
    }
    decoded
}

#[test]
fn the_layouts_are_as_documented() {
    let checkin = CheckIn::new(&CheckInFields {
        product: "kiosk",
        board: "rpi5b_2gb",
        version: Version {
            major: 1,
            minor: 2,
            patch: 3,
        },
        security_version: 0x0A0B_0C0D,
        trust_list_version: 2,
        last_result: 16,
        running_manifest: [0xA1; 32],
        nonce: [0xB2; 32],
        device: [0xC3; 32],
    })
    .unwrap();
    let bytes = encoded(&checkin);
    assert_eq!(bytes.len(), 192);
    assert_eq!(bytes[..8], [5, 0, 1, 0, 184, 0, 0, 0]);
    let payload = &bytes[HEADER_LEN..];
    assert_eq!(payload[..8], [5, 9, 1, 0, 2, 0, 3, 0]);
    assert_eq!(&payload[8..13], b"kiosk");
    assert_eq!(&payload[40..49], b"rpi5b_2gb");
    assert_eq!(payload[72..82], [0x0D, 0x0C, 0x0B, 0x0A, 2, 0, 0, 0, 16, 0]);
    assert_eq!(payload[82..88], [0; 6]);
    assert_eq!(payload[88..120], [0xA1; 32]);
    assert_eq!(payload[120..152], [0xB2; 32]);
    assert_eq!(payload[152..184], [0xC3; 32]);
    assert_eq!(checkin.nonce(), [0xB2; 32]);
    assert_eq!(checkin.last_result(), 16);

    let contents = DeploymentContents::new(&DeploymentFields {
        product: "kiosk",
        board: "rpi5b_2gb",
        nonce: [0xB2; 32],
        cohort: 0x0102_0304,
        instruction: Instruction::Install([0xD4; 32]),
    })
    .unwrap();
    let record = Deployment::new(
        contents,
        &NamedSignature {
            key_id: [0x51; 32],
            signature: [0x52; 64],
        },
    );
    let bytes = encoded(&record);
    assert_eq!(bytes.len(), 240);
    assert_eq!(bytes[..8], [6, 0, 1, 0, 232, 0, 0, 0]);
    let payload = &bytes[HEADER_LEN..];
    assert_eq!(payload[..8], [5, 9, 2, 0, 4, 3, 2, 1]);
    assert_eq!(&payload[8..13], b"kiosk");
    assert_eq!(&payload[40..49], b"rpi5b_2gb");
    assert_eq!(payload[72..104], [0xB2; 32]);
    assert_eq!(payload[104..136], [0xD4; 32]);
    assert_eq!(payload[136..168], [0x51; 32]);
    assert_eq!(payload[168..232], [0x52; 64]);
    assert_eq!(record.instruction(), Instruction::Install([0xD4; 32]));
    assert_eq!(record.cohort(), 0x0102_0304);
}

proptest! {
    #[test]
    fn checkins_round_trip(checkin in checkin()) {
        prop_assert_eq!(checkin_both(&encoded(&checkin)), Ok(checkin));
    }

    #[test]
    fn deployments_round_trip(record in deployment()) {
        prop_assert_eq!(deployment_both(&encoded(&record)), Ok(record));
    }

    #[test]
    fn any_bytes_decode_or_are_refused(bytes in prop::collection::vec(any::<u8>(), 0..2 * DEPLOYMENT_LEN)) {
        let _ = checkin_both(&bytes);
        let _ = deployment_both(&bytes);
    }

    #[test]
    fn any_payload_behind_a_header_decodes_or_is_refused(
        checkin in checkin(),
        record in deployment(),
        a in prop::collection::vec(any::<u8>(), CHECKIN_LEN - HEADER_LEN),
        b in prop::collection::vec(any::<u8>(), DEPLOYMENT_LEN - HEADER_LEN),
    ) {
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN..].copy_from_slice(&a);
        let _ = checkin_both(&bytes);
        let mut bytes = encoded(&record);
        bytes[HEADER_LEN..].copy_from_slice(&b);
        let _ = deployment_both(&bytes);
    }

    #[test]
    fn changed_records_decode_or_are_refused(
        checkin in checkin(),
        record in deployment(),
        changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        let mut a = encoded(&checkin);
        let mut b = encoded(&record);
        for (at, value) in changes {
            let i = at.index(a.len());
            a[i] = value;
            let i = at.index(b.len());
            b[i] = value;
        }
        let _ = checkin_both(&a);
        let _ = deployment_both(&b);
    }

    #[test]
    fn each_rule_is_refused_with_its_own_error(checkin in checkin(), record in deployment(), len in 33u8..) {
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN] = 0;
        prop_assert_eq!(checkin_both(&bytes), Err(CheckInError::ProductNameLength { len: 0 }));
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN + 1] = len;
        prop_assert_eq!(checkin_both(&bytes), Err(CheckInError::BoardNameLength { len: usize::from(len) }));
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN + 8] = b'K';
        prop_assert_eq!(checkin_both(&bytes), Err(CheckInError::ProductNameCharacter { at: 0, byte: b'K' }));
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN + 40] = b'.';
        prop_assert_eq!(checkin_both(&bytes), Err(CheckInError::BoardNameCharacter { at: 0, byte: b'.' }));
        let mut bytes = encoded(&checkin);
        bytes[HEADER_LEN + 82] = 1;
        prop_assert_eq!(checkin_both(&bytes), Err(CheckInError::ReservedNotZero));

        let mut bytes = encoded(&record);
        bytes[HEADER_LEN + 2] = 3;
        prop_assert_eq!(deployment_both(&bytes), Err(DeploymentError::UnknownInstruction { code: 3 }));
        let mut bytes = encoded(&record);
        bytes[HEADER_LEN + 3] = 1;
        prop_assert_eq!(deployment_both(&bytes), Err(DeploymentError::ReservedNotZero));
        let mut bytes = encoded(&record);
        bytes[HEADER_LEN + 1] = 0;
        prop_assert_eq!(deployment_both(&bytes), Err(DeploymentError::BoardNameLength { len: 0 }));
        // "current" with a manifest hash that is not zeros.
        let mut bytes = encoded(&record);
        bytes[HEADER_LEN + 2] = 1;
        bytes[HEADER_LEN + 104] = 1;
        prop_assert_eq!(deployment_both(&bytes), Err(DeploymentError::ReservedNotZero));
        let mut bytes = encoded(&record);
        bytes[2] = 2;
        prop_assert_eq!(
            deployment_both(&bytes),
            Err(DeploymentError::Frame(DecodeError::UnsupportedVersion { expected: 1, found: 2 }))
        );
    }
}
