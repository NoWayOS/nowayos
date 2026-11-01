// SPDX-License-Identifier: MIT OR Apache-2.0
//! USB video: a webcam's own Motion JPEG frames, whole, for a
//! client PD - never decoded here (the station sends the camera's
//! frames as they are; the far end decodes them).
//!
//! From usb.org's *Universal Serial Bus Device Class Definition for Video
//! Devices*, revision 1.5 (UVC 1.5: the stream's payload transfers and
//! headers of section 2.4.3, the VideoControl and VideoStreaming interface
//! descriptors of sections 3.7.2, 3.9.2.1 and 3.10.1, the class requests of
//! section 4.1, the Video Probe and Commit controls of section 4.3.1.1, and
//! the codes of appendix A) and its *Motion-JPEG Payload*, revision 1.5 (the
//! format and frame descriptors of section 3.1, the payload transfers of
//! section 3.2 and the frame's markers of section 3.3). No driver source of
//! any kind was read.
//!
//! * [`camera`](mod@camera) - a configuration walked within fixed bounds for its first
//!   VideoStreaming interface: its Motion JPEG frame sizes and rates, the
//!   alternate settings of its isochronous video endpoint, and the size and
//!   rate chosen ([`Camera::choose`]: 1280 x 720 at 30 a second where
//!   offered); a camera with no Motion JPEG frame is refused;
//! * [`probe`] - the Video Probe and Commit controls that set the size and
//!   rate chosen: SET_CUR and GET_CUR of the probe, then SET_CUR of the
//!   commit, each answer checked;
//! * [`frames`] - each payload transfer's header checked and removed, and
//!   whole frames assembled from what follows into a slot a client reads;
//!   a frame with no free slot (the client behind) is dropped, never
//!   queued, and a damaged one - its error bit, an interval missed, a bad
//!   header, too large for a slot, or not a whole JPEG - is dropped;
//! * [`ring`] - the bounded ring of frame slots between the USB host PD
//!   and its client: each slot's record, and each side's checks of the
//!   other's index.
//!
//! **Nothing a camera sends is trusted.** Descriptors, the probe's answer
//! and every payload are copies already out of DMA memory, each checked
//! before it is used; `make fuzz` runs them on arbitrary bytes
//! (`usb_video`). The frames themselves are opaque: only their first and
//! last markers are looked at, never their image.
//!
//! Provisional, and left out: the first VideoStreaming interface's first
//! Motion JPEG format only (uncompressed, frame-based and H.264 formats are
//! not used); its isochronous endpoint at the alternate setting with
//! the most bandwidth, which carries any size the camera offers (choosing
//! the smallest that carries the negotiated payload needs a second Configure
//! Endpoint); a bulk video endpoint is listed but not streamed (bulk
//! transfers come later); still images, the status interrupt endpoint,
//! camera and processing unit controls, and the payload headers' clock
//! fields (read past, never used) are not.

pub mod camera;
pub mod frames;
pub mod probe;
pub mod ring;

pub use camera::{Camera, Choice, Refused, camera};

/// The video interface class (UVC 1.5, table A-1).
pub const CLASS_VIDEO: u8 = 0x0E;

/// The VideoControl interface subclass (UVC 1.5, table A-2).
pub const SUBCLASS_CONTROL: u8 = 0x01;

/// The VideoStreaming interface subclass (UVC 1.5, table A-2).
pub const SUBCLASS_STREAMING: u8 = 0x02;

/// The class-specific interface descriptor type (UVC 1.5, table A-4).
pub const CS_INTERFACE: u8 = 0x24;

/// A frame interval of 1/30 s, in the 100 ns units UVC gives intervals in.
pub const INTERVAL_30: u32 = 333_333;

/// The 100 ns units in a second.
const UNITS: u32 = 10_000_000;

/// A frame interval as a rate, for a log line: frames a second to a tenth
/// - "30", "7.5" - or "?" for an interval of 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate(pub u32);

impl core::fmt::Display for Rate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Tenths of a frame a second, rounded: 10^8 / interval.
        let Some(tenths) =
            (UNITS.saturating_mul(10).saturating_add(self.0 / 2)).checked_div(self.0)
        else {
            return f.write_str("?");
        };
        let (whole, tenth) = (tenths / 10, tenths % 10);
        if tenth == 0 {
            write!(f, "{whole}")
        } else {
            write!(f, "{whole}.{tenth}")
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
    //! A camera's enumeration, end to end (`enumerate`'s steps 5 to 9).

    use super::*;
    use crate::descriptor::tests::KEYBOARD_DEVICE;
    use crate::enumerate::{Enumeration, Outcome, Step, Stopped, Video};
    use crate::port::Speed;
    use crate::request::{OutData, Setup};
    use crate::trb::Completion;
    use std::vec::Vec;

    /// The invented webcam of `camera`'s tests with a microphone: an audio
    /// streaming interface (2) whose alternate setting 1 has 16 kHz mono
    /// PCM on IN endpoint 0x84.
    fn webcam_with_microphone() -> Vec<u8> {
        let mut config = camera::tests::webcam();
        config[4] = 3;
        config.extend_from_slice(&[9, 4, 2, 0, 0, 1, 2, 0, 0]);
        config.extend_from_slice(&[9, 4, 2, 1, 1, 1, 2, 0, 0]);
        config.extend_from_slice(&[7, 0x24, 1, 1, 1, 0x01, 0x00]);
        config.extend_from_slice(&[11, 0x24, 2, 1, 1, 2, 16, 1, 0x80, 0x3E, 0x00]);
        config.extend_from_slice(&[9, 5, 0x84, 0x05, 32, 0, 4, 0, 0]);
        camera::tests::finish(config)
    }

    /// A high-speed device's enumeration up to its configuration read, by
    /// a machine that looks for a camera or one that streams audio alone.
    fn enumerate(config: &[u8], cameras: bool) -> (Enumeration, Step) {
        let (mut machine, _) = if cameras {
            Enumeration::start_with_camera(1, None)
        } else {
            Enumeration::start_streaming(1, None)
        };
        let mut device = KEYBOARD_DEVICE;
        device[7] = 64;
        for outcome in [
            Outcome::PortEnabled(Speed::High),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&device[..8]),
            Outcome::ControlData(&device),
            Outcome::ControlData(&config[..9]),
        ] {
            machine.on(outcome);
        }
        let step = machine.on(Outcome::ControlData(config));
        (machine, step)
    }

    /// The camera's answer to the probe: the frame asked for (format 2,
    /// frame 2) at 30 a second, frames up to 1,843,200 bytes, payloads up
    /// to 3072.
    fn reply() -> [u8; 26] {
        let mut out = [0u8; 26];
        out[..8].copy_from_slice(&[0x01, 0x00, 2, 2, 0x15, 0x16, 0x05, 0x00]);
        out[18..22].copy_from_slice(&1_843_200u32.to_le_bytes());
        out[22..26].copy_from_slice(&3072u32.to_le_bytes());
        out
    }

    #[test]
    fn a_webcam_is_set_up_probed_committed_and_selected() {
        let (mut machine, step) = enumerate(&webcam_with_microphone(), true);
        assert_eq!(step, Step::ConfigureEndpoints);
        let choice = machine.video().choice().unwrap();
        assert_eq!(
            (choice.width, choice.height, choice.interval),
            (1280, 720, INTERVAL_30)
        );
        assert_eq!(machine.video().endpoint(), Some(choice.stream()));
        assert_eq!(machine.camera().unwrap().frames().count(), 3);
        assert_eq!(machine.streams().len(), 1, "its microphone");
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::set_configuration(1))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_interface(2, 1)),
            "the microphone first"
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::ControlOut(
                probe::set(probe::PROBE, 1, 26),
                OutData::new(&probe::first(&choice)[..26]).unwrap(),
            )
        );
        assert_eq!(machine.waiting_for(), "the probe's SET_CUR");
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(probe::get(probe::PROBE, 1, 26))
        );
        assert_eq!(
            machine.on(Outcome::ControlData(&reply())),
            Step::ControlOut(
                probe::set(probe::COMMIT, 1, 26),
                OutData::new(&probe::commit(&reply(), 26)[..26]).unwrap(),
            )
        );
        assert!(matches!(machine.video(), Video::Answered(..)));
        assert_eq!(machine.video_commit(), None, "not streaming yet");
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_interface(1, 2))
        );
        assert_eq!(machine.on(Outcome::ControlDone), Step::Poll);
        assert!(matches!(
            machine.video(),
            Video::Streaming(_, answer) if (answer.max_frame, answer.max_payload) == (1_843_200, 3072)
        ));
        assert_eq!(machine.selected().count(), 1);
        // The commit kept, to make again before each start.
        assert_eq!(
            machine.video_commit(),
            Some((
                probe::set(probe::COMMIT, 1, 26),
                OutData::new(&probe::commit(&reply(), 26)[..26]).unwrap(),
            ))
        );
    }

    #[test]
    fn a_machine_for_audio_alone_leaves_the_camera() {
        let (machine, step) = enumerate(&webcam_with_microphone(), false);
        assert_eq!(step, Step::ConfigureEndpoints);
        assert_eq!((machine.camera(), machine.video()), (None, Video::None));
    }

    #[test]
    fn a_failed_request_or_a_refused_answer_drops_the_video_alone() {
        let to_probe = |config: &[u8]| {
            let (mut machine, _) = enumerate(config, true);
            machine.on(Outcome::CommandDone);
            let mut step = machine.on(Outcome::ControlDone);
            // Past the audio streams' requests, a rate's SET_CUR among them,
            // to the probe's.
            while machine.waiting_for() != "the probe's SET_CUR" {
                assert!(!matches!(step, Step::Poll | Step::Stop(_)), "no probe");
                step = machine.on(Outcome::ControlDone);
            }
            machine
        };
        // The probe's GET_CUR stalls: the control endpoint is reset, and
        // the microphone streams without the video.
        let mut machine = to_probe(&webcam_with_microphone());
        machine.on(Outcome::ControlDone);
        assert_eq!(
            machine.on(Outcome::ControlFailed(Completion::STALL)),
            Step::ResetControl
        );
        assert_eq!(machine.on(Outcome::CommandDone), Step::Poll);
        assert_eq!(
            machine.video(),
            Video::Refused(Refused::Request("the probe's GET_CUR"))
        );
        // A camera alone whose commit stalls: nothing left to use.
        let mut machine = to_probe(&camera::tests::webcam());
        machine.on(Outcome::ControlDone);
        machine.on(Outcome::ControlData(&reply()));
        machine.on(Outcome::ControlFailed(Completion::STALL));
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Stop(Stopped::NothingToUse)
        );
        // An answer for another frame: refused, with nothing to reset.
        let mut machine = to_probe(&webcam_with_microphone());
        machine.on(Outcome::ControlDone);
        let mut other = reply();
        other[3] = 3;
        assert_eq!(machine.on(Outcome::ControlData(&other)), Step::Poll);
        assert!(matches!(
            machine.video(),
            Video::Refused(Refused::Answer(_))
        ));
    }

    #[test]
    fn a_camera_with_no_motion_jpeg_and_nothing_else_is_left() {
        let mut config = camera::tests::webcam();
        let at = config
            .windows(4)
            .position(|w| w == [11, 0x24, 0x06, 2])
            .unwrap();
        config[at + 2] = 0x10;
        let (machine, step) = enumerate(&config, true);
        assert_eq!(step, Step::Stop(Stopped::NothingToUse));
        assert_eq!(machine.video(), Video::Refused(Refused::NoMotionJpeg));
    }

    #[test]
    fn intervals_read_as_rates() {
        let show = |interval| std::format!("{}", Rate(interval));
        assert_eq!(show(INTERVAL_30), "30");
        assert_eq!(show(400_000), "25");
        assert_eq!(show(1_333_333), "7.5");
        assert_eq!(show(166_666), "60");
        assert_eq!(show(10_000_000), "1");
        assert_eq!(show(0), "?");
        assert_eq!(show(u32::MAX), "0");
    }
}
