// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: `nowayos-update`'s update state - any bytes decoded
//! as the state record, as the update PD decodes one read back from
//! storage; and any bytes as a whole copy of the state, as read, and again
//! with its CRC made good so that its record and trust list are read too.
//!
//! Checks: nothing panics; `StateRecord::decode` and `nowayos-ipc`'s
//! `decode_exact` agree on every input, and whatever decodes encodes back to
//! exactly the bytes it came from (`nowayos_fuzz::check_decode`, whose
//! prefix decoder also takes a record with bytes after it), keeping its
//! rules - a trial never in the
//! slot that boots, never below the rollback index; and a copy that decodes
//! encodes back to exactly the copy.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::{FRAME_MAX, Input, check_decode, check_header, frame};
use nowayos_ipc::decode_exact;
use nowayos_update::state::{COPY_LEN, StateRecord, crc32, decode_copy, encode_copy};

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let mut buf = [0u8; FRAME_MAX];
    let choice = input.byte() % 3;
    if choice < 2 {
        let record: &[u8] = match choice {
            1 => frame::<StateRecord>(input.rest(), &mut buf),
            _ => input.rest(),
        };
        check_header(record);
        // The prefix decoder's record, which takes bytes after it too.
        let kept = check_decode::<StateRecord>(record);
        let decoded = StateRecord::decode(record);
        assert_eq!(
            decoded.as_ref().ok(),
            decode_exact::<StateRecord>(record).as_ref().ok(),
            "the two decoders disagree on {record:02x?}"
        );
        if let Ok(record) = decoded {
            assert_eq!(kept, Some(record));
            if let Some(trial) = record.trial {
                assert_ne!(trial.slot, record.boots, "{record:?}");
                assert!(
                    trial.security_version >= record.rollback_index,
                    "{record:?}"
                );
            }
        }
        return;
    }
    let mut copy = [0u8; COPY_LEN];
    for (slot, byte) in copy.iter_mut().zip(input.rest()) {
        *slot = *byte;
    }
    for sealed in [false, true] {
        if sealed {
            let (body, crc) = copy.split_at_mut(COPY_LEN - 4);
            crc.copy_from_slice(&crc32(body).to_le_bytes());
        }
        if let Ok((generation, state)) = decode_copy(&copy) {
            assert_eq!(
                encode_copy(generation, &state).ok(),
                Some(copy),
                "{state:?}"
            );
        }
    }
});
