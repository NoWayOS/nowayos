// SPDX-License-Identifier: MIT OR Apache-2.0
//! The security PD's signing records, so that "the
//! device's key signs nothing but a TLS 1.3 client `CertificateVerify`
//! input" is code with tests and proofs before any key exists.
//!
//! * [`recognise`]: the one input the security PD signs, by its fixed form
//!   (RFC 8446 section 4.4.3) - 64 spaces, the context string
//!   [`CLIENT_CONTEXT`], a zero byte, then a transcript hash of 32 or 48
//!   bytes, 130 or 146 bytes in all, as rustls hands its `Signer` the input
//!   to sign. Anything else - the server's context string, a hash of another
//!   length, any byte changed, the input cut short or extended - is refused
//!   with a [`NotClientInput`] saying which part differs.
//! * [`SignRequest`], TLS PD to security PD: the whole input to sign.
//! * [`SignAnswer`], security PD to TLS PD: a 64-byte P-256 signature, r then
//!   s, or a [`Refusal`] with its reason.
//! * [`KeyCode`], security PD to App PD: the key's approval code - the first
//!   64 bits of the SHA-256 of its public key, public - or
//!   that there is no key yet.
//!
//! Each is a fixed-size `nowayos-ipc` [`Message`] of its own kind and layout
//! version, unused bytes zero; a record that is malformed, truncated, of
//! another version or kind, or not zero where it must be, is refused with a
//! typed error, never a panic. Making the key and signing come with the
//! security PD itself, which needs the entropy PD's seeds.
//!
//! Pure logic: no heap, no `unsafe`, no panics; arithmetic is checked.
//! Provisional: the first cut of these records.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(kani)]
mod proofs;

use core::fmt;

use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer};

/// Message kinds: a block of their own.
pub const SIGN_REQUEST_KIND: u16 = 0x0701;
/// See [`SIGN_REQUEST_KIND`].
pub const SIGN_ANSWER_KIND: u16 = 0x0702;
/// See [`SIGN_REQUEST_KIND`].
pub const KEY_CODE_KIND: u16 = 0x0781;

/// The spaces a `CertificateVerify` input starts with.
pub const PADDING_LEN: usize = 64;

/// The client's context string (RFC 8446 section 4.4.3).
pub const CLIENT_CONTEXT: &[u8; 33] = b"TLS 1.3, client CertificateVerify";

/// The server's, which the security PD never signs.
pub const SERVER_CONTEXT: &[u8; 33] = b"TLS 1.3, server CertificateVerify";

/// The bytes before the transcript hash: spaces, context, zero.
pub const PREFIX_LEN: usize = PADDING_LEN + CLIENT_CONTEXT.len() + 1;

/// The longest input: a 48-byte (SHA-384) transcript hash.
pub const INPUT_MAX: usize = PREFIX_LEN + 48;

/// A P-256 signature: r then s, 32 bytes each.
pub const SIGNATURE_LEN: usize = 64;

/// An approval code: the first 64 bits of the SHA-256 of the public key.
pub const CODE_LEN: usize = 8;

/// The transcript hash a recognised input carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TranscriptHash {
    /// SHA-256: a 130-byte input.
    Sha256([u8; 32]),
    /// SHA-384: a 146-byte input.
    Sha384([u8; 48]),
}

impl TranscriptHash {
    /// The hash's bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Sha256(bytes) => bytes,
            Self::Sha384(bytes) => bytes,
        }
    }
}

/// Why an input is not a TLS 1.3 client `CertificateVerify` input: the
/// first part that differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotClientInput {
    /// Neither 130 nor 146 bytes: a hash of another length, or an input cut
    /// short or extended.
    Length {
        /// The input's length.
        len: usize,
    },
    /// The first 64 bytes are not all spaces.
    Padding,
    /// The server's context string: the security PD signs only the
    /// client's.
    ServerContext,
    /// Another context string.
    Context,
    /// No zero byte after the context string.
    Separator,
}

impl fmt::Display for NotClientInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { len } => write!(f, "{len} bytes, not 130 or 146"),
            Self::Padding => f.write_str("not 64 spaces first"),
            Self::ServerContext => f.write_str("the server's context string"),
            Self::Context => f.write_str("not the client's context string"),
            Self::Separator => f.write_str("no zero byte after the context string"),
        }
    }
}

/// The transcript hash, if `input` is a TLS 1.3 client `CertificateVerify`
/// input: the only thing the security PD signs.
///
/// # Errors
///
/// The [`NotClientInput`] naming the first part that differs.
pub fn recognise(input: &[u8]) -> Result<TranscriptHash, NotClientInput> {
    let len = input.len();
    let (prefix, hash) = input
        .split_at_checked(PREFIX_LEN)
        .ok_or(NotClientInput::Length { len })?;
    let hash = match hash.len() {
        32 => TranscriptHash::Sha256(
            hash.try_into()
                .map_err(|_| NotClientInput::Length { len })?,
        ),
        48 => TranscriptHash::Sha384(
            hash.try_into()
                .map_err(|_| NotClientInput::Length { len })?,
        ),
        _ => return Err(NotClientInput::Length { len }),
    };
    let (padding, rest) = prefix
        .split_at_checked(PADDING_LEN)
        .ok_or(NotClientInput::Length { len })?;
    if padding.iter().any(|byte| *byte != b' ') {
        return Err(NotClientInput::Padding);
    }
    let (context, separator) = rest
        .split_at_checked(CLIENT_CONTEXT.len())
        .ok_or(NotClientInput::Length { len })?;
    if context != CLIENT_CONTEXT {
        return Err(if context == SERVER_CONTEXT {
            NotClientInput::ServerContext
        } else {
            NotClientInput::Context
        });
    }
    if separator != [0] {
        return Err(NotClientInput::Separator);
    }
    Ok(hash)
}

/// TLS PD to security PD: an input to sign, as rustls hands it over, up to
/// [`INPUT_MAX`] bytes. The record carries its length and the input,
/// zero-padded; the security PD signs it only if [`recognise`] accepts it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SignRequest {
    len: u8,
    input: [u8; INPUT_MAX],
}

impl SignRequest {
    /// A request to sign `input`, or `None` if it is longer than
    /// [`INPUT_MAX`]. Not checked against [`recognise`]: the security PD
    /// does that.
    #[must_use]
    pub fn new(input: &[u8]) -> Option<Self> {
        let len = u8::try_from(input.len())
            .ok()
            .filter(|len| usize::from(*len) <= INPUT_MAX)?;
        let mut padded = [0u8; INPUT_MAX];
        padded.get_mut(..input.len())?.copy_from_slice(input);
        Some(Self { len, input: padded })
    }

    /// The input to sign.
    #[must_use]
    pub fn input(&self) -> &[u8] {
        self.input.get(..usize::from(self.len)).unwrap_or_default()
    }

    /// The transcript hash, if the input is the one thing the security PD
    /// signs.
    ///
    /// # Errors
    ///
    /// As for [`recognise`].
    pub fn recognise(&self) -> Result<TranscriptHash, NotClientInput> {
        recognise(self.input())
    }
}

impl Message for SignRequest {
    const KIND: u16 = SIGN_REQUEST_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 1 + INPUT_MAX;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u8(self.len)?;
        out.write_bytes(&self.input)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let len = input.read_u8()?;
        let bytes: [u8; INPUT_MAX] = input.read_array()?;
        let used = usize::from(len);
        if used > INPUT_MAX {
            return Err(DecodeError::InvalidValue {
                field: "input length",
            });
        }
        if bytes
            .get(used..)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
        {
            return Err(DecodeError::NonZeroPadding);
        }
        Ok(Self { len, input: bytes })
    }
}

/// Why the security PD signed nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The input is not a TLS 1.3 client `CertificateVerify` input.
    NotClientInput,
    /// There is no key yet: no seed, or none made.
    NoKey,
    /// The request did not decode.
    BadRequest,
}

impl Refusal {
    const fn code(self) -> u8 {
        match self {
            Self::NotClientInput => 1,
            Self::NoKey => 2,
            Self::BadRequest => 3,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::NotClientInput),
            2 => Some(Self::NoKey),
            3 => Some(Self::BadRequest),
            _ => None,
        }
    }
}

/// Security PD to TLS PD: the signature, or why there is none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignAnswer {
    /// A P-256 signature, r then s, neither zero.
    Signed([u8; SIGNATURE_LEN]),
    /// Nothing signed.
    Refused(Refusal),
}

/// A signature's two halves, r and s: an ECDSA signature has neither zero.
fn halves_nonzero(signature: &[u8; SIGNATURE_LEN]) -> bool {
    let (r, s) = signature.split_at(SIGNATURE_LEN / 2);
    r.iter().any(|byte| *byte != 0) && s.iter().any(|byte| *byte != 0)
}

impl SignAnswer {
    /// A signed answer, or `None` if r or s is zero, which no ECDSA
    /// signature has.
    #[must_use]
    pub fn signed(signature: [u8; SIGNATURE_LEN]) -> Option<Self> {
        halves_nonzero(&signature).then_some(Self::Signed(signature))
    }
}

impl Message for SignAnswer {
    const KIND: u16 = SIGN_ANSWER_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 1 + SIGNATURE_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        match self {
            Self::Signed(signature) => {
                if !halves_nonzero(signature) {
                    return Err(EncodeError::InvalidValue { field: "signature" });
                }
                out.write_u8(0)?;
                out.write_bytes(signature)
            }
            Self::Refused(refusal) => {
                out.write_u8(1)?;
                out.write_u8(refusal.code())?;
                out.write_bytes(&[0u8; SIGNATURE_LEN - 1])
            }
        }
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let outcome = input.read_u8()?;
        let body: [u8; SIGNATURE_LEN] = input.read_array()?;
        match outcome {
            0 => Self::signed(body).ok_or(DecodeError::InvalidValue { field: "signature" }),
            1 => {
                let (reason, padding) = body
                    .split_first()
                    .ok_or(DecodeError::InvalidValue { field: "reason" })?;
                let refusal = Refusal::from_code(*reason)
                    .ok_or(DecodeError::InvalidValue { field: "reason" })?;
                if padding.iter().any(|byte| *byte != 0) {
                    return Err(DecodeError::NonZeroPadding);
                }
                Ok(Self::Refused(refusal))
            }
            _ => Err(DecodeError::InvalidValue { field: "outcome" }),
        }
    }
}

/// Security PD to App PD: the key's approval code, or no key yet. The code
/// is public: the out-of-service screen shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCode {
    /// No key: the kiosk stays out of service.
    NoKey,
    /// The key is ready; its code.
    Ready([u8; CODE_LEN]),
}

impl KeyCode {
    /// The code as four groups of four hexadecimal digits, as the screen
    /// shows it; `None` with no key.
    #[must_use]
    pub fn groups(&self) -> Option<[u16; 4]> {
        let Self::Ready(code) = self else {
            return None;
        };
        let [a, b, c, d, e, f, g, h] = *code;
        Some([
            u16::from_be_bytes([a, b]),
            u16::from_be_bytes([c, d]),
            u16::from_be_bytes([e, f]),
            u16::from_be_bytes([g, h]),
        ])
    }
}

impl Message for KeyCode {
    const KIND: u16 = KEY_CODE_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 1 + CODE_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        match self {
            Self::NoKey => {
                out.write_u8(0)?;
                out.write_bytes(&[0u8; CODE_LEN])
            }
            Self::Ready(code) => {
                out.write_u8(1)?;
                out.write_bytes(code)
            }
        }
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let state = input.read_u8()?;
        let code: [u8; CODE_LEN] = input.read_array()?;
        match state {
            0 if code == [0; CODE_LEN] => Ok(Self::NoKey),
            0 => Err(DecodeError::NonZeroPadding),
            1 => Ok(Self::Ready(code)),
            _ => Err(DecodeError::InvalidValue { field: "state" }),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use nowayos_ipc::{HEADER_LEN, decode, decode_exact, encode, encoded_len};
    use proptest::prelude::*;

    use super::*;

    /// A client `CertificateVerify` input with `hash`.
    fn client_input(hash: &[u8]) -> Vec<u8> {
        let mut input = vec![b' '; PADDING_LEN];
        input.extend_from_slice(CLIENT_CONTEXT);
        input.push(0);
        input.extend_from_slice(hash);
        input
    }

    fn round_trip<M: Message + PartialEq + core::fmt::Debug>(message: &M) {
        let mut record = vec![0u8; encoded_len::<M>()];
        assert_eq!(encode(message, &mut record), Ok(record.len()));
        assert_eq!(&decode_exact::<M>(&record).unwrap(), message);
    }

    #[test]
    fn the_lengths_are_rfc_8446s() {
        assert_eq!(PREFIX_LEN, 98);
        assert_eq!(client_input(&[7; 32]).len(), 130);
        assert_eq!(INPUT_MAX, 146);
        assert_eq!(encoded_len::<SignRequest>(), HEADER_LEN + 147);
        assert_eq!(encoded_len::<SignAnswer>(), HEADER_LEN + 65);
        assert_eq!(encoded_len::<KeyCode>(), HEADER_LEN + 9);
    }

    #[test]
    fn a_client_input_is_recognised_with_either_hash() {
        assert_eq!(
            recognise(&client_input(&[7; 32])),
            Ok(TranscriptHash::Sha256([7; 32]))
        );
        assert_eq!(
            recognise(&client_input(&[9; 48])),
            Ok(TranscriptHash::Sha384([9; 48]))
        );
        assert_eq!(TranscriptHash::Sha384([9; 48]).as_bytes(), &[9; 48]);
    }

    #[test]
    fn anything_else_is_refused_naming_the_part() {
        let good = client_input(&[7; 32]);
        let mut server = good.clone();
        server[PADDING_LEN..PADDING_LEN + 33].copy_from_slice(SERVER_CONTEXT);
        assert_eq!(recognise(&server), Err(NotClientInput::ServerContext));
        let mut padding = good.clone();
        padding[10] = b'\t';
        assert_eq!(recognise(&padding), Err(NotClientInput::Padding));
        let mut context = good.clone();
        context[PADDING_LEN] = b't';
        assert_eq!(recognise(&context), Err(NotClientInput::Context));
        let mut separator = good.clone();
        separator[PREFIX_LEN - 1] = 1;
        assert_eq!(recognise(&separator), Err(NotClientInput::Separator));
        for len in [0, 97, 98, 129, 131, 145, 147, 200] {
            let mut input = good.clone();
            input.resize(len, 7);
            assert_eq!(
                recognise(&input),
                Err(NotClientInput::Length { len }),
                "{len} bytes"
            );
        }
        assert_eq!(
            recognise(&client_input(&[7; 33])),
            Err(NotClientInput::Length { len: 131 })
        );
    }

    #[test]
    fn each_record_round_trips_with_its_own_kind_and_version() {
        round_trip(&SignRequest::new(&client_input(&[7; 32])).unwrap());
        round_trip(&SignRequest::new(&client_input(&[7; 48])).unwrap());
        round_trip(&SignRequest::new(b"anything short").unwrap());
        round_trip(&SignAnswer::signed([5; 64]).unwrap());
        for refusal in [Refusal::NotClientInput, Refusal::NoKey, Refusal::BadRequest] {
            round_trip(&SignAnswer::Refused(refusal));
        }
        round_trip(&KeyCode::NoKey);
        round_trip(&KeyCode::Ready([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(
            KeyCode::Ready([0x1a, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70, 0x81]).groups(),
            Some([0x1a2b, 0x3c4d, 0x5e6f, 0x7081])
        );
        assert_eq!(KeyCode::NoKey.groups(), None);
        let mut record = vec![0u8; encoded_len::<KeyCode>()];
        encode(&KeyCode::NoKey, &mut record).unwrap();
        assert!(matches!(
            decode::<SignAnswer>(&record),
            Err(DecodeError::WrongKind { .. })
        ));
    }

    #[test]
    fn a_request_too_long_or_padded_with_nonzero_is_refused() {
        assert!(SignRequest::new(&[0x20; INPUT_MAX + 1]).is_none());
        let request = SignRequest::new(&client_input(&[7; 32])).unwrap();
        assert_eq!(request.recognise(), Ok(TranscriptHash::Sha256([7; 32])));
        let mut record = vec![0u8; encoded_len::<SignRequest>()];
        encode(&request, &mut record).unwrap();
        let mut padded = record.clone();
        *padded.last_mut().unwrap() = 1;
        assert_eq!(
            decode::<SignRequest>(&padded),
            Err(DecodeError::NonZeroPadding)
        );
        let mut long = record.clone();
        long[HEADER_LEN] = 147;
        assert_eq!(
            decode::<SignRequest>(&long),
            Err(DecodeError::InvalidValue {
                field: "input length"
            })
        );
        let mut version = record.clone();
        version[2] = 2;
        assert!(matches!(
            decode::<SignRequest>(&version),
            Err(DecodeError::UnsupportedVersion { .. })
        ));
        assert!(matches!(
            decode::<SignRequest>(&record[..record.len() - 1]),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn an_answer_is_a_whole_signature_or_a_known_reason() {
        assert!(SignAnswer::signed([0; 64]).is_none());
        let mut half = [5u8; 64];
        half[32..].fill(0);
        assert!(SignAnswer::signed(half).is_none(), "s zero");
        let mut record = vec![0u8; encoded_len::<SignAnswer>()];
        assert_eq!(
            encode(&SignAnswer::Signed(half), &mut record),
            Err(EncodeError::InvalidValue { field: "signature" })
        );
        encode(&SignAnswer::Refused(Refusal::NoKey), &mut record).unwrap();
        let mut reason = record.clone();
        reason[HEADER_LEN + 1] = 9;
        assert_eq!(
            decode::<SignAnswer>(&reason),
            Err(DecodeError::InvalidValue { field: "reason" })
        );
        let mut padding = record.clone();
        padding[HEADER_LEN + 2] = 1;
        assert_eq!(
            decode::<SignAnswer>(&padding),
            Err(DecodeError::NonZeroPadding)
        );
        let mut outcome = record.clone();
        outcome[HEADER_LEN] = 2;
        assert_eq!(
            decode::<SignAnswer>(&outcome),
            Err(DecodeError::InvalidValue { field: "outcome" })
        );
    }

    #[test]
    fn a_code_record_with_no_key_is_zeros() {
        let mut record = vec![0u8; encoded_len::<KeyCode>()];
        encode(&KeyCode::NoKey, &mut record).unwrap();
        let mut stray = record.clone();
        stray[HEADER_LEN + 3] = 1;
        assert_eq!(decode::<KeyCode>(&stray), Err(DecodeError::NonZeroPadding));
        let mut state = record.clone();
        state[HEADER_LEN] = 2;
        assert_eq!(
            decode::<KeyCode>(&state),
            Err(DecodeError::InvalidValue { field: "state" })
        );
    }

    proptest! {
        #[test]
        fn a_client_input_with_any_hash_is_recognised(hash in proptest::collection::vec(any::<u8>(), 32..=48)) {
            let input = client_input(&hash);
            match hash.len() {
                32 | 48 => {
                    let recognised = recognise(&input).unwrap();
                    prop_assert_eq!(recognised.as_bytes(), &hash[..]);
                }
                len => prop_assert_eq!(recognise(&input), Err(NotClientInput::Length { len: len + PREFIX_LEN })),
            }
        }

        /// Any one byte of the prefix changed: refused; changed in the
        /// hash: still the client's form, with that hash.
        #[test]
        fn a_changed_byte_is_refused_unless_it_is_in_the_hash(
            sha384 in any::<bool>(), at in 0usize..146, delta in 1u8..=255,
        ) {
            let hash = vec![3u8; if sha384 { 48 } else { 32 }];
            let mut input = client_input(&hash);
            let at = at % input.len();
            input[at] = input[at].wrapping_add(delta);
            match recognise(&input) {
                Ok(hash) => {
                    prop_assert!(at >= PREFIX_LEN);
                    prop_assert_eq!(hash.as_bytes(), &input[PREFIX_LEN..]);
                }
                Err(_) => prop_assert!(at < PREFIX_LEN),
            }
        }

        #[test]
        fn recognising_any_bytes_never_panics_and_keeps_the_form(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            if let Ok(hash) = recognise(&bytes) {
                prop_assert!(bytes.len() == 130 || bytes.len() == 146);
                prop_assert!(bytes[..PADDING_LEN].iter().all(|byte| *byte == b' '));
                prop_assert_eq!(&bytes[PADDING_LEN..PREFIX_LEN - 1], &CLIENT_CONTEXT[..]);
                prop_assert_eq!(bytes[PREFIX_LEN - 1], 0);
                prop_assert_eq!(hash.as_bytes(), &bytes[PREFIX_LEN..]);
            }
        }

        #[test]
        fn decoding_any_bytes_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..170)) {
            if let Ok(request) = decode::<SignRequest>(&bytes) {
                prop_assert!(request.input().len() <= INPUT_MAX);
                round_trip(&request);
            }
            if let Ok(answer) = decode::<SignAnswer>(&bytes) {
                round_trip(&answer);
            }
            if let Ok(code) = decode::<KeyCode>(&bytes) {
                round_trip(&code);
            }
        }
    }
}
