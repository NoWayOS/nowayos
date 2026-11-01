// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Video Probe and Commit controls (UVC 1.5, section 4.3.1.1): how the
//! size and rate chosen are set.
//!
//! The host sets the probe - bmHint's dwFrameInterval bit, the format, the
//! frame and the interval it wants, every other field 0 for the camera to
//! fill - reads it back, and if the answer keeps its format and frame, sets
//! the commit to that answer; then it selects the streaming interface's
//! alternate setting. Each is a class request to the VideoStreaming
//! interface (section 4.1: bmRequestType 0x21 or 0xA1, wValue the control
//! selector in its high byte, wIndex the interface with entity 0), with the
//! request codes of table A-8 and the selectors of table A-16.
//!
//! **The answer is the camera's and is checked** ([`answer`]): long enough
//! for the fields of UVC 1.0, the same format and frame, an interval and
//! a frame size other than 0. Its payload size is reported, not refused: the
//! endpoint's packets bound every payload whatever the camera says.

use core::fmt;

use super::camera::Choice;
use crate::request::Setup;
use crate::{le16, le32};

/// SET_CUR (UVC 1.5, table A-8).
pub const SET_CUR: u8 = 0x01;

/// GET_CUR (UVC 1.5, table A-8).
pub const GET_CUR: u8 = 0x81;

/// VS_PROBE_CONTROL (UVC 1.5, table A-16).
pub const PROBE: u8 = 0x01;

/// VS_COMMIT_CONTROL (UVC 1.5, table A-16).
pub const COMMIT: u8 = 0x02;

/// The longest probe and commit: UVC 1.5's 48 bytes (table 4-75).
pub const PROBE_MAX: usize = 48;

/// The shortest answer read: UVC 1.0's 26 bytes, through
/// dwMaxPayloadTransferSize.
pub const PROBE_MIN: usize = 26;

/// bmRequestType: host to device, class, interface.
const OUT_CLASS_INTERFACE: u8 = 0x21;

/// bmRequestType: device to host, class, interface.
const IN_CLASS_INTERFACE: u8 = 0xA1;

/// bmHint's dwFrameInterval bit: keep the interval asked for.
const HINT_INTERVAL: u16 = 0x0001;

/// SET_CUR of `control` (the probe or the commit) on VideoStreaming
/// interface `interface`, `len` bytes.
#[must_use]
pub const fn set(control: u8, interface: u8, len: u16) -> Setup {
    Setup {
        request_type: OUT_CLASS_INTERFACE,
        request: SET_CUR,
        value: u16::from_be_bytes([control, 0]),
        index: interface as u16,
        length: len,
    }
}

/// GET_CUR of `control` on VideoStreaming interface `interface`, `len`
/// bytes.
#[must_use]
pub const fn get(control: u8, interface: u8, len: u16) -> Setup {
    Setup {
        request_type: IN_CLASS_INTERFACE,
        request: GET_CUR,
        value: u16::from_be_bytes([control, 0]),
        index: interface as u16,
        length: len,
    }
}

/// The probe the host sets first: bmHint's interval bit, `choice`'s format,
/// frame and interval, and 0 in every field the camera fills.
#[must_use]
pub fn first(choice: &Choice) -> [u8; PROBE_MAX] {
    let mut out = [0u8; PROBE_MAX];
    let [h0, h1] = HINT_INTERVAL.to_le_bytes();
    let [i0, i1, i2, i3] = choice.interval.to_le_bytes();
    for (slot, value) in out
        .iter_mut()
        .zip([h0, h1, choice.format, choice.frame, i0, i1, i2, i3])
    {
        *slot = value;
    }
    out
}

/// The commit: the probe's answer as the camera gave it, `len` bytes of it
/// - 0 past what it gave.
#[must_use]
pub fn commit(answer: &[u8], len: usize) -> [u8; PROBE_MAX] {
    let mut out = [0u8; PROBE_MAX];
    for (slot, byte) in out.iter_mut().zip(answer).take(len) {
        *slot = *byte;
    }
    out
}

/// What the camera's answer to the probe says, checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    /// bmHint, as it came back.
    pub hint: u16,
    /// dwFrameInterval: the interval it will stream at, at least 1.
    pub interval: u32,
    /// wDelay: its latency, in milliseconds.
    pub delay: u16,
    /// dwMaxVideoFrameSize: the most bytes of one frame, at least 1.
    pub max_frame: u32,
    /// dwMaxPayloadTransferSize: the most bytes of one payload transfer.
    pub max_payload: u32,
}

/// Why an answer to the probe is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerError {
    /// Fewer bytes than UVC 1.0's fields.
    Short {
        /// The bytes there were.
        len: usize,
    },
    /// Another format or frame than the one asked for.
    Other {
        /// The format it answered with.
        format: u8,
        /// The frame it answered with.
        frame: u8,
    },
    /// An interval of 0.
    NoInterval,
    /// A frame size of 0.
    NoFrameSize,
}

impl fmt::Display for AnswerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short { len } => write!(f, "{len} bytes, fewer than {PROBE_MIN}"),
            Self::Other { format, frame } => {
                write!(f, "format {format}, frame {frame}: not the one asked for")
            }
            Self::NoInterval => f.write_str("a frame interval of 0"),
            Self::NoFrameSize => f.write_str("a frame size of 0"),
        }
    }
}

/// Checks the camera's answer to the probe, `bytes` already copied out of
/// DMA memory, against `choice`.
///
/// # Errors
///
/// [`AnswerError`] for an answer shorter than [`PROBE_MIN`], with another
/// format or frame, or with an interval or a frame size of 0.
pub fn answer(bytes: &[u8], choice: &Choice) -> Result<Answer, AnswerError> {
    if bytes.len() < PROBE_MIN {
        return Err(AnswerError::Short { len: bytes.len() });
    }
    let byte = |at: usize| bytes.get(at).copied().unwrap_or(0);
    let (format, frame) = (byte(2), byte(3));
    if (format, frame) != (choice.format, choice.frame) {
        return Err(AnswerError::Other { format, frame });
    }
    let interval = le32(bytes, 4).unwrap_or(0);
    let max_frame = le32(bytes, 18).unwrap_or(0);
    if interval == 0 {
        return Err(AnswerError::NoInterval);
    }
    if max_frame == 0 {
        return Err(AnswerError::NoFrameSize);
    }
    Ok(Answer {
        hint: le16(bytes, 0).unwrap_or(0),
        interval,
        delay: le16(bytes, 16).unwrap_or(0),
        max_frame,
        max_payload: le32(bytes, 22).unwrap_or(0),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod tests {
    use super::*;
    use crate::video::camera::{camera, tests::webcam};
    use proptest::prelude::*;

    fn choice() -> Choice {
        camera(&webcam()).unwrap().unwrap().choose().unwrap()
    }

    #[test]
    fn requests_on_the_wire() {
        assert_eq!(
            set(PROBE, 1, 26).to_bytes(),
            [0x21, 0x01, 0x00, 0x01, 1, 0, 26, 0]
        );
        assert_eq!(
            get(PROBE, 1, 48).to_bytes(),
            [0xA1, 0x81, 0x00, 0x01, 1, 0, 48, 0]
        );
        assert_eq!(
            set(COMMIT, 3, 34).to_bytes(),
            [0x21, 0x01, 0x00, 0x02, 3, 0, 34, 0]
        );
        assert!(get(PROBE, 1, 26).inbound() && !set(COMMIT, 1, 26).inbound());
    }

    #[test]
    fn the_first_probe_asks_for_the_choice_and_leaves_the_rest_to_the_camera() {
        let probe = first(&choice());
        assert_eq!(probe[..8], [0x01, 0x00, 2, 2, 0x15, 0x16, 0x05, 0x00]);
        assert!(probe[8..].iter().all(|byte| *byte == 0));
    }

    /// An invented camera's answer: the frame asked for at 30 a second,
    /// 40 ms of latency, frames up to 1,843,200 bytes, payloads up to 3072.
    fn reply() -> [u8; 26] {
        let mut out = [0u8; 26];
        out[..8].copy_from_slice(&[0x01, 0x00, 2, 2, 0x15, 0x16, 0x05, 0x00]);
        out[16..18].copy_from_slice(&40u16.to_le_bytes());
        out[18..22].copy_from_slice(&1_843_200u32.to_le_bytes());
        out[22..26].copy_from_slice(&3072u32.to_le_bytes());
        out
    }

    #[test]
    fn an_answer_is_checked_and_committed_as_given() {
        let choice = choice();
        assert_eq!(
            answer(&reply(), &choice),
            Ok(Answer {
                hint: 1,
                interval: 333_333,
                delay: 40,
                max_frame: 1_843_200,
                max_payload: 3072,
            })
        );
        let committed = commit(&reply(), 26);
        assert_eq!(committed[..26], reply());
        assert!(committed[26..].iter().all(|byte| *byte == 0));
        // A UVC 1.5 commit of a 26-byte answer: 0 past it.
        assert_eq!(commit(&reply(), 48)[..26], reply());

        assert_eq!(
            answer(&reply()[..25], &choice),
            Err(AnswerError::Short { len: 25 })
        );
        let mut other = reply();
        other[3] = 1;
        assert_eq!(
            answer(&other, &choice),
            Err(AnswerError::Other {
                format: 2,
                frame: 1
            })
        );
        let mut no_interval = reply();
        no_interval[4..8].fill(0);
        assert_eq!(answer(&no_interval, &choice), Err(AnswerError::NoInterval));
        let mut no_size = reply();
        no_size[18..22].fill(0);
        assert_eq!(answer(&no_size, &choice), Err(AnswerError::NoFrameSize));
        assert_eq!(
            std::format!("{}", AnswerError::Short { len: 3 }),
            "3 bytes, fewer than 26"
        );
    }

    proptest! {
        #[test]
        fn any_answer_is_checked_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Ok(found) = answer(&bytes, &choice()) {
                prop_assert!(found.interval != 0 && found.max_frame != 0);
                prop_assert!(bytes.len() >= PROBE_MIN);
            }
            let committed = commit(&bytes, PROBE_MAX);
            prop_assert_eq!(&committed[..bytes.len().min(PROBE_MAX)], &bytes[..bytes.len().min(PROBE_MAX)]);
        }
    }
}
