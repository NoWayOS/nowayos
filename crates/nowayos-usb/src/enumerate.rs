// SPDX-License-Identifier: MIT OR Apache-2.0
//! One device's enumeration, from a connected port to its boot interfaces'
//! reports flowing (xHCI 1.2, section 4.3; USB 2.0, section 9.1.2; HID 1.11,
//! section 7.2), as a state machine with no hardware in it.
//!
//! The driver asks [`Enumeration`] what to do next ([`Step`]), does it
//! through the controller, and feeds back what the controller reported
//! ([`Outcome`]); every outcome moves the machine on or ends it. The order
//! is section 4.3's:
//!
//! 1. reset the port, if it is a USB 2.0 port (a USB 3 port trains and
//!    enables itself);
//! 2. Enable Slot, then Address Device with the default control endpoint's
//!    packet size for the port's speed;
//! 3. GET_DESCRIPTOR(Device), its first 8 bytes for the true packet size,
//!    an Evaluate Context if it differs, then all 18 bytes;
//! 4. GET_DESCRIPTOR(Configuration), its 9-byte header for the total
//!    length, then the whole configuration (up to [`CONFIG_MAX`] bytes),
//!    walked for HID interfaces that speak the boot protocol;
//! 5. Configure Endpoint with their interrupt IN endpoints - and, for a
//!    machine that streams ([`Enumeration::start_streaming`]), the
//!    isochronous endpoint of each audio stream ([`audio`]) - then
//!    SET_CONFIGURATION;
//! 6. for each boot interface, SET_PROTOCOL(boot), and for a keyboard
//!    SET_IDLE(0) - a device that stalls either is reset and carried on
//!    with, since some do;
//! 7. for each audio stream it supports, SET_INTERFACE to its alternate
//!    setting, after the Configure Endpoint that set its endpoint up (xHCI
//!    1.2, section 4.6.6), then, where its endpoint has the control or its
//!    format several rates, the audio class's SET_CUR of its sampling
//!    frequency (Audio 1.0, section 5.2.3.2.3.1) - a stream whose
//!    SET_INTERFACE fails, or whose rate cannot be set when it has a
//!    choice, is dropped, its control endpoint reset, and the rest carried
//!    on with;
//! 8. for a camera's video, for a machine that looks for one ([`video`]) -
//!    its endpoint set up with the rest at step 5 - the probe's
//!    SET_CUR and GET_CUR, the commit's
//!    SET_CUR, then SET_INTERFACE to its alternate setting; a request that
//!    fails, or an answer refused, drops the video alone ([`Video`]);
//! 9. poll, and stream.
//!
//! A hub, a device with nothing to use or a failure ends it
//! ([`Step::Stop`]): the driver disables the slot and leaves the port.
//! What the device says is decoded by [`descriptor`] and
//! never trusted.

use core::fmt;

use crate::audio::{self, Stream, Streams};
use crate::descriptor::{
    self, BootDevice, BootInterfaces, CLASS_HUB, ConfigHeader, DescriptorError, DeviceDescriptor,
};
use crate::hid;
use crate::port::Speed;
use crate::report::{self, Layout};
use crate::request::{OutData, Setup, descriptor_type};
use crate::trb::Completion;
use crate::video::camera::{Camera, Choice, Refused};
use crate::video::probe::{self, Answer, PROBE_MAX};
use crate::video::{self};

/// The most bytes of a configuration read: a keyboard's or mouse's is a few
/// dozen.
pub const CONFIG_MAX: u16 = 512;

/// The most a machine that streams reads: a camera lists its video
/// formats, often kilobytes of them, before its microphone's interface.
pub const CONFIG_MAX_STREAMING: u16 = 8192;

/// What the driver must do next for the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Reset the port, then report [`Outcome::PortEnabled`] when the reset
    /// has finished and the port is enabled.
    ResetPort,
    /// Issue Enable Slot; report [`Outcome::SlotEnabled`].
    EnableSlot,
    /// Issue Address Device for the slot, the default control endpoint's
    /// packet size `max_packet`; report [`Outcome::CommandDone`].
    AddressDevice {
        /// Bytes per packet on the default control endpoint.
        max_packet: u16,
    },
    /// Issue Evaluate Context: the default control endpoint's packet size
    /// is `max_packet`.
    EvaluateControl {
        /// Bytes per packet.
        max_packet: u16,
    },
    /// Make the control request `Setup`, with a Data Stage from the device
    /// if it has a length; report [`Outcome::ControlData`] with the bytes
    /// that came, or [`Outcome::ControlDone`] when there is no Data Stage.
    Control(Setup),
    /// Make the control request `Setup` with a Data Stage to the device
    /// carrying the bytes given - an audio stream's sampling frequency
    /// or a camera's probe or commit; report
    /// [`Outcome::ControlDone`].
    ControlOut(Setup, OutData),
    /// Issue Reset Endpoint for the default control endpoint, halted by a
    /// stall; report [`Outcome::CommandDone`].
    ResetControl,
    /// Issue Configure Endpoint for the boot interfaces' endpoints
    /// ([`Enumeration::boot`]) and the audio streams' isochronous ones
    /// ([`Enumeration::streams`]), and a camera's video endpoint
    /// ([`Video::endpoint`]); report [`Outcome::CommandDone`].
    ConfigureEndpoints,
    /// Queue reports on each boot interface's endpoint, and keep them
    /// queued; start each audio stream [`Enumeration::selected`]: the
    /// device is ready.
    Poll,
    /// Give up on the device: disable its slot, if it has one.
    Stop(Stopped),
}

/// What the controller reported for the last step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// The port reset finished and the port is enabled at this speed.
    PortEnabled(Speed),
    /// Enable Slot gave this slot.
    SlotEnabled(u8),
    /// The command completed successfully.
    CommandDone,
    /// The command failed.
    CommandFailed(Completion),
    /// The control request's Data Stage brought these bytes, copied out of
    /// DMA memory.
    ControlData(&'a [u8]),
    /// The control request without a Data Stage completed.
    ControlDone,
    /// The control request failed.
    ControlFailed(Completion),
    /// The step did not complete in time.
    TimedOut,
}

/// Why enumeration stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// A command failed.
    Command(Completion),
    /// A control request failed.
    Control(Completion),
    /// A descriptor was refused.
    Descriptor(DescriptorError),
    /// A hub: not supported yet.
    Hub,
    /// No HID interface speaks the boot protocol.
    NoBootInterface,
    /// Neither a HID interface this driver uses nor an audio stream, for a
    /// machine that streams.
    NothingToUse,
    /// A slot number of 0 from Enable Slot.
    NoSlot,
    /// A step did not complete in time: what it waited for.
    TimedOut(&'static str),
    /// An outcome that does not answer the step.
    Unexpected,
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Command(code) => write!(f, "a command failed: {code}"),
            Self::Control(code) => write!(f, "a control request failed: {code}"),
            Self::Descriptor(error) => write!(f, "a descriptor was refused: {error}"),
            Self::Hub => f.write_str("a hub, not supported yet"),
            Self::NoBootInterface => {
                f.write_str("no keyboard or mouse boot interface, and no absolute pointer")
            }
            Self::NothingToUse => f.write_str(
                "no keyboard or mouse boot interface, no absolute pointer and no audio stream",
            ),
            Self::NoSlot => f.write_str("no slot"),
            Self::TimedOut(step) => write!(f, "no answer in time to {step}"),
            Self::Unexpected => f.write_str("an answer to another step"),
        }
    }
}

/// Where the machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Resetting,
    EnablingSlot,
    Addressing,
    ReadingPrefix,
    EvaluatingControl,
    ReadingDevice,
    ReadingConfigHeader,
    ReadingConfig,
    /// The report descriptor of interface `n`.
    ReadingReport(usize),
    Configuring,
    SettingConfiguration,
    /// SET_PROTOCOL on boot interface `n`.
    SettingProtocol(usize),
    /// SET_IDLE on boot interface `n`.
    SettingIdle(usize),
    /// SET_INTERFACE for audio stream `n`.
    SettingInterface(usize),
    /// SET_CUR of audio stream `n`'s sampling frequency.
    SettingRate(usize),
    /// The camera's probe set, then read back, its commit set and its
    /// alternate setting selected.
    SettingProbe,
    GettingProbe,
    SettingCommit,
    SettingVideoInterface,
    /// Resetting the control endpoint after a request on boot interface
    /// `interface` failed, then carrying on as `then` says.
    ResettingControl {
        interface: usize,
        then: Resume,
    },
    Polling,
    Stopped,
}

/// Where an enumeration carries on once its control endpoint is reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resume {
    /// SET_PROTOCOL stalled: on to the interface's SET_IDLE.
    Idle,
    /// SET_IDLE stalled: on to the next interface's SET_PROTOCOL.
    NextProtocol,
    /// A report descriptor's read failed: no layout for the interface, and
    /// on to the next report descriptor.
    NextReport,
    /// SET_INTERFACE failed: the stream dropped, and on to the next.
    NextInterface,
    /// SET_CUR of a sampling frequency failed: the stream kept if its
    /// format takes only the one rate, and on to the next.
    RateRefused,
    /// A camera's request failed: its video dropped, and on.
    NoVideo,
}

/// A camera's video, as far as its enumeration has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Video {
    /// No camera, or a machine that does not stream.
    None,
    /// Chosen, its endpoint to set up; not yet set.
    Chosen(Choice),
    /// Its probe answered; the commit next.
    Answered(Choice, Answer),
    /// Committed and its alternate setting selected: to stream.
    Streaming(Choice, Answer),
    /// Not streamed: why.
    Refused(Refused),
}

impl Video {
    /// The choice, while the video is chosen or streams.
    #[must_use]
    pub const fn choice(&self) -> Option<Choice> {
        match self {
            Self::Chosen(choice) | Self::Answered(choice, _) | Self::Streaming(choice, _) => {
                Some(*choice)
            }
            Self::None | Self::Refused(_) => None,
        }
    }

    /// The video endpoint to set up with the device's others, while the
    /// video is chosen or streams.
    #[must_use]
    pub const fn endpoint(&self) -> Option<Stream> {
        match self.choice() {
            Some(choice) => Some(choice.stream()),
            None => None,
        }
    }
}

/// One device's enumeration.
#[derive(Clone, Copy, Debug)]
pub struct Enumeration {
    state: State,
    port: u8,
    speed: Option<Speed>,
    slot: u8,
    max_packet: u16,
    device: Option<DeviceDescriptor>,
    configuration: u8,
    boot: BootInterfaces,
    /// Each interface's absolute pointer, by index in [`Self::boot`], for
    /// an interface whose report descriptor lays one out.
    layouts: [Option<Layout>; descriptor::MAX_BOOT_INTERFACES],
    /// Whether the driver streams audio: only then are streams
    /// looked for.
    streaming: bool,
    /// Its audio streams, once its configuration is read.
    streams: Streams,
    /// By index in [`Self::streams`]: whether SET_INTERFACE selected it,
    /// and its rate is set.
    selected: [bool; audio::MAX_STREAMS],
    /// The rate its audio streams are set to where they have a choice: the
    /// one they take nearest it.
    wanted_rate: u32,
    /// Whether it looks for a camera too ([`Self::start_with_camera`]).
    cameras: bool,
    /// Its camera, once its configuration is read, for a machine that
    /// looks for one.
    camera: Option<Camera>,
    /// Its camera's video.
    video: Video,
    /// Its camera's commit, as made.
    commit: Option<(Setup, OutData)>,
}

impl Enumeration {
    /// Starts enumerating the device on `port`: a USB 2.0 port is reset
    /// first; a USB 3 port, enabled at `speed`, goes straight to its slot.
    #[must_use]
    pub fn start(port: u8, usb3_enabled: Option<Speed>) -> (Self, Step) {
        Self::begin(port, usb3_enabled, false)
    }

    /// As [`Self::start`], for a driver that streams audio: the
    /// device's audio streams are set up too, and a device with only audio
    /// streams is used.
    #[must_use]
    pub fn start_streaming(port: u8, usb3_enabled: Option<Speed>) -> (Self, Step) {
        Self::begin(port, usb3_enabled, true)
    }

    /// As [`Self::start_streaming`], for a driver that also streams a
    /// camera's video: a camera's Motion JPEG frame size and rate
    /// are chosen, its video endpoint set up with the rest, its probe and
    /// commit set and its alternate setting selected ([`Video`]).
    #[must_use]
    pub fn start_with_camera(port: u8, usb3_enabled: Option<Speed>) -> (Self, Step) {
        let (mut machine, step) = Self::begin(port, usb3_enabled, true);
        machine.cameras = true;
        (machine, step)
    }

    fn begin(port: u8, usb3_enabled: Option<Speed>, streaming: bool) -> (Self, Step) {
        let mut machine = Self {
            state: State::Resetting,
            port,
            speed: None,
            slot: 0,
            max_packet: 0,
            device: None,
            configuration: 0,
            boot: BootInterfaces::default(),
            layouts: [None; descriptor::MAX_BOOT_INTERFACES],
            streaming,
            streams: Streams::default(),
            selected: [false; audio::MAX_STREAMS],
            wanted_rate: audio::DEFAULT_RATE,
            cameras: false,
            camera: None,
            video: Video::None,
            commit: None,
        };
        let step = match usb3_enabled {
            Some(speed) => machine.enabled(speed),
            None => Step::ResetPort,
        };
        (machine, step)
    }

    /// The port, from 1.
    #[must_use]
    pub const fn port(&self) -> u8 {
        self.port
    }

    /// The device's speed, once its port is enabled.
    #[must_use]
    pub const fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// Its slot, once Enable Slot gave one (0 before).
    #[must_use]
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Its device descriptor, once read.
    #[must_use]
    pub const fn device(&self) -> Option<DeviceDescriptor> {
        self.device
    }

    /// Its configuration value, once read.
    #[must_use]
    pub const fn configuration(&self) -> u8 {
        self.configuration
    }

    /// Its boot interfaces, once its configuration is read.
    #[must_use]
    pub const fn boot(&self) -> &BootInterfaces {
        &self.boot
    }

    /// The absolute pointer interface `index` of [`Self::boot`] lays out,
    /// if its report descriptor was read and found one.
    #[must_use]
    pub fn layout(&self, index: usize) -> Option<Layout> {
        self.layouts.get(index).copied().flatten()
    }

    /// Its audio streams, once its configuration is read: every one found,
    /// those refused among them.
    #[must_use]
    pub const fn streams(&self) -> &Streams {
        &self.streams
    }

    /// Its audio streams that are not refused: those to set up and select.
    pub fn usable(&self) -> impl Iterator<Item = &Stream> {
        self.streams
            .iter()
            .filter(|stream| stream.refusal.is_none())
    }

    /// Sets the rate its audio streams are set to where they have a choice:
    /// of the rates each takes and its packets hold, the nearest;
    /// [`audio::DEFAULT_RATE`] unless set. Takes effect when the
    /// configuration is read.
    pub const fn want_rate(&mut self, rate: u32) {
        self.wanted_rate = rate;
    }

    /// The audio streams SET_INTERFACE selected, once ready: those to
    /// start.
    pub fn selected(&self) -> impl Iterator<Item = &Stream> {
        self.streams
            .iter()
            .zip(self.selected)
            .filter_map(|(stream, selected)| selected.then_some(stream))
    }

    /// Its camera - what it offers - once its configuration is read.
    #[must_use]
    pub const fn camera(&self) -> Option<&Camera> {
        self.camera.as_ref()
    }

    /// Its camera's video: chosen, streaming or refused.
    #[must_use]
    pub const fn video(&self) -> Video {
        self.video
    }

    /// Its camera's commit, once its video streams: the request to make
    /// again before the video starts again.
    #[must_use]
    pub const fn video_commit(&self) -> Option<(Setup, OutData)> {
        match self.video {
            Video::Streaming(..) => self.commit,
            _ => None,
        }
    }

    /// Whether the device is ready and polled.
    #[must_use]
    pub fn polling(&self) -> bool {
        self.state == State::Polling
    }

    /// Whether enumeration has ended, ready or not.
    #[must_use]
    pub fn finished(&self) -> bool {
        matches!(self.state, State::Polling | State::Stopped)
    }

    /// What the machine waits for now, for a report when it gives up.
    #[must_use]
    pub const fn waiting_for(&self) -> &'static str {
        match self.state {
            State::Resetting => "the port's reset",
            State::EnablingSlot => "Enable Slot",
            State::Addressing => "Address Device",
            State::ReadingPrefix => "the device descriptor's first 8 bytes",
            State::EvaluatingControl => "Evaluate Context",
            State::ReadingDevice => "the device descriptor",
            State::ReadingConfigHeader => "the configuration descriptor's header",
            State::ReadingConfig => "the configuration descriptor",
            State::ReadingReport(_) => "a report descriptor",
            State::Configuring => "Configure Endpoint",
            State::SettingConfiguration => "SET_CONFIGURATION",
            State::SettingProtocol(_) => "SET_PROTOCOL",
            State::SettingIdle(_) => "SET_IDLE",
            State::SettingInterface(_) | State::SettingVideoInterface => "SET_INTERFACE",
            State::SettingRate(_) => "SET_CUR of a sampling frequency",
            State::SettingProbe => "the probe's SET_CUR",
            State::GettingProbe => "the probe's GET_CUR",
            State::SettingCommit => "the commit's SET_CUR",
            State::ResettingControl { .. } => "the control endpoint's reset",
            State::Polling | State::Stopped => "nothing",
        }
    }

    /// Why a device with nothing to use stops.
    const fn nothing(&self) -> Stopped {
        if self.streaming {
            Stopped::NothingToUse
        } else {
            Stopped::NoBootInterface
        }
    }

    /// SET_INTERFACE for the next audio stream from `n` that is not
    /// refused, or, when none is left, the camera's probe - or
    /// polling, or a stop if nothing is left to use.
    fn next_interface(&mut self, from: usize) -> Step {
        let next = self
            .streams
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, stream)| stream.refusal.is_none())
            .map(|(n, stream)| (n, *stream));
        if let Some((n, stream)) = next {
            self.state = State::SettingInterface(n);
            return Step::Control(Setup::set_interface(stream.interface, stream.alternate));
        }
        if let Video::Chosen(choice) = self.video {
            self.state = State::SettingProbe;
            let setup = probe::set(probe::PROBE, choice.interface, choice.probe_len);
            return self.video_out(setup, &probe::first(&choice));
        }
        self.ready()
    }

    /// Polling, or a stop if nothing is left to use.
    fn ready(&mut self) -> Step {
        let video = matches!(self.video, Video::Streaming(..));
        if self.boot.is_empty() && !self.selected.contains(&true) && !video {
            return self.stop(self.nothing());
        }
        self.state = State::Polling;
        Step::Poll
    }

    /// Selects audio stream `n` - its setting selected and its rate set -
    /// and moves on to the next.
    fn select(&mut self, n: usize) -> Step {
        if let Some(selected) = self.selected.get_mut(n) {
            *selected = true;
        }
        self.next_interface(n.saturating_add(1))
    }

    /// A camera's probe or commit, `setup`, sent with the first
    /// `setup.length` bytes of `data` - or, should they not make a Data
    /// Stage, the video dropped before any request is made.
    fn video_out(&mut self, setup: Setup, data: &[u8; PROBE_MAX]) -> Step {
        match data.get(..usize::from(setup.length)).and_then(OutData::new) {
            Some(data) => Step::ControlOut(setup, data),
            None => {
                self.video = Video::Refused(Refused::Request(self.waiting_for()));
                self.ready()
            }
        }
    }

    /// The camera's video dropped, `why`; its control endpoint, halted by
    /// the failure, reset first.
    fn drop_video(&mut self, why: Refused) -> Step {
        self.video = Video::Refused(why);
        self.state = State::ResettingControl {
            interface: 0,
            then: Resume::NoVideo,
        };
        Step::ResetControl
    }

    fn stop(&mut self, why: Stopped) -> Step {
        self.state = State::Stopped;
        Step::Stop(why)
    }

    fn enabled(&mut self, speed: Speed) -> Step {
        self.speed = Some(speed);
        self.max_packet = speed.initial_max_packet();
        self.state = State::EnablingSlot;
        Step::EnableSlot
    }

    fn read(&mut self, state: State, kind: u8, length: u16) -> Step {
        self.state = state;
        Step::Control(Setup::get_descriptor(kind, 0, length))
    }

    /// The next SET_PROTOCOL or SET_IDLE from boot interface `from`, or
    /// polling when none is left: keyboards get SET_IDLE after their
    /// SET_PROTOCOL.
    fn class_request(&mut self, from: usize, idle_next: bool) -> Step {
        let interfaces: [Option<_>; descriptor::MAX_BOOT_INTERFACES] = {
            let mut all = [None; descriptor::MAX_BOOT_INTERFACES];
            for (slot, interface) in all.iter_mut().zip(self.boot.iter()) {
                *slot = Some(*interface);
            }
            all
        };
        if let Some(Some(interface)) = interfaces.get(from) {
            if idle_next {
                if interface.device == BootDevice::Keyboard {
                    self.state = State::SettingIdle(from);
                    return Step::Control(Setup::set_idle(interface.interface));
                }
            } else if interface.device != BootDevice::Absolute {
                self.state = State::SettingProtocol(from);
                return Step::Control(Setup::set_boot_protocol(interface.interface));
            }
            // A mouse: no SET_IDLE; an absolute pointer: neither, as it is
            // no boot device. On to the next interface.
            return self.class_request(from.saturating_add(1), false);
        }
        self.next_interface(0)
    }

    /// Boot interface `n`, in order.
    fn interface(&self, n: usize) -> Option<descriptor::BootInterface> {
        self.boot.iter().nth(n).copied()
    }

    /// Reads the report descriptor of the next interface from `n` that
    /// needs one - an absolute pointer's - or, when none is left, goes on
    /// to configure the device's endpoints; or stops if no interface is
    /// left to use.
    fn next_report(&mut self, from: usize) -> Step {
        let mut n = from;
        while let Some(interface) = self.interface(n) {
            if interface.device == BootDevice::Absolute {
                self.state = State::ReadingReport(n);
                let length = interface
                    .report_len
                    .min(u16::try_from(report::DESCRIPTOR_MAX).unwrap_or(u16::MAX));
                return Step::Control(Setup::get_report_descriptor(interface.interface, length));
            }
            n = n.saturating_add(1);
        }
        // Every report descriptor is read: keep the interfaces that are a
        // boot device or lay out an absolute pointer, in order, and their
        // layouts with them.
        let mut keep = [false; descriptor::MAX_BOOT_INTERFACES];
        let mut layouts = [None; descriptor::MAX_BOOT_INTERFACES];
        let mut kept = 0usize;
        for ((index, interface), wanted) in self.boot.iter().enumerate().zip(keep.iter_mut()) {
            let layout = self.layouts.get(index).copied().flatten();
            if interface.device != BootDevice::Absolute || layout.is_some() {
                *wanted = true;
                if let Some(slot) = layouts.get_mut(kept) {
                    *slot = layout;
                }
                kept = kept.saturating_add(1);
            }
        }
        self.boot.keep(keep);
        self.layouts = layouts;
        if self.boot.is_empty() && self.usable().next().is_none() && self.video.choice().is_none() {
            return self.stop(self.nothing());
        }
        self.state = State::Configuring;
        Step::ConfigureEndpoints
    }

    /// Feeds what the controller reported for the last step, and returns
    /// the next.
    pub fn on(&mut self, outcome: Outcome<'_>) -> Step {
        if outcome == Outcome::TimedOut {
            let step = self.waiting_for();
            return self.stop(Stopped::TimedOut(step));
        }
        match (self.state, outcome) {
            (State::Resetting, Outcome::PortEnabled(speed)) => self.enabled(speed),
            (State::EnablingSlot, Outcome::SlotEnabled(0)) => self.stop(Stopped::NoSlot),
            (State::EnablingSlot, Outcome::SlotEnabled(slot)) => {
                self.slot = slot;
                self.state = State::Addressing;
                Step::AddressDevice {
                    max_packet: self.max_packet,
                }
            }
            (State::Addressing, Outcome::CommandDone) => {
                self.read(State::ReadingPrefix, descriptor_type::DEVICE, 8)
            }
            (State::ReadingPrefix, Outcome::ControlData(bytes)) => {
                match descriptor::max_packet0(bytes) {
                    Ok(size) if size == self.max_packet => self.read(
                        State::ReadingDevice,
                        descriptor_type::DEVICE,
                        descriptor::DEVICE_DESCRIPTOR_LEN as u16,
                    ),
                    Ok(size) => {
                        self.max_packet = size;
                        self.state = State::EvaluatingControl;
                        Step::EvaluateControl { max_packet: size }
                    }
                    Err(error) => self.stop(Stopped::Descriptor(error)),
                }
            }
            (State::EvaluatingControl, Outcome::CommandDone) => self.read(
                State::ReadingDevice,
                descriptor_type::DEVICE,
                descriptor::DEVICE_DESCRIPTOR_LEN as u16,
            ),
            (State::ReadingDevice, Outcome::ControlData(bytes)) => {
                match DeviceDescriptor::decode(bytes) {
                    Ok(device) if device.class == CLASS_HUB => {
                        self.device = Some(device);
                        self.stop(Stopped::Hub)
                    }
                    Ok(device) => {
                        self.device = Some(device);
                        self.read(
                            State::ReadingConfigHeader,
                            descriptor_type::CONFIGURATION,
                            descriptor::CONFIG_HEADER_LEN as u16,
                        )
                    }
                    Err(error) => self.stop(Stopped::Descriptor(error)),
                }
            }
            (State::ReadingConfigHeader, Outcome::ControlData(bytes)) => {
                match ConfigHeader::decode(bytes) {
                    Ok(header) => {
                        self.configuration = header.value;
                        let most = if self.streaming {
                            CONFIG_MAX_STREAMING
                        } else {
                            CONFIG_MAX
                        };
                        self.read(
                            State::ReadingConfig,
                            descriptor_type::CONFIGURATION,
                            header.total_len.min(most),
                        )
                    }
                    Err(error) => self.stop(Stopped::Descriptor(error)),
                }
            }
            (State::ReadingConfig, Outcome::ControlData(bytes)) => {
                let streams = if self.streaming {
                    audio::streams_at(bytes, self.speed.unwrap_or(Speed::Full), self.wanted_rate)
                } else {
                    Ok(Streams::default())
                };
                let camera = if self.cameras {
                    video::camera(bytes)
                } else {
                    Ok(None)
                };
                match (descriptor::boot_interfaces(bytes), streams, camera) {
                    (Ok(boot), Ok(streams), Ok(camera)) => {
                        self.camera = camera;
                        self.video = match camera.map(|camera| camera.choose()) {
                            Some(Ok(choice)) => Video::Chosen(choice),
                            Some(Err(why)) => Video::Refused(why),
                            None => Video::None,
                        };
                        if boot.is_empty()
                            && streams.iter().all(|stream| stream.refusal.is_some())
                            && self.video.choice().is_none()
                        {
                            self.streams = streams;
                            return self.stop(self.nothing());
                        }
                        self.boot = boot;
                        self.streams = streams;
                        self.next_report(0)
                    }
                    (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                        self.stop(Stopped::Descriptor(error))
                    }
                }
            }
            (State::ReadingReport(n), Outcome::ControlData(bytes)) => {
                // Nothing this driver can use leaves no layout, and the
                // interface goes once every descriptor is read: a
                // descriptor not read whole, as its reports could be longer
                // than the walk saw, and a layout whose report does not fit
                // one packet of its endpoint, as it would arrive in pieces.
                let layout = self.interface(n).and_then(|interface| {
                    let room = usize::from(interface.max_packet).min(hid::REPORT_MAX);
                    (usize::from(interface.report_len) <= bytes.len())
                        .then(|| report::parse(bytes).ok())
                        .flatten()
                        .and_then(|fields| Layout::find(&fields))
                        .filter(|layout| usize::from(layout.report_bytes) <= room)
                });
                if let Some(slot) = self.layouts.get_mut(n) {
                    *slot = layout;
                }
                self.next_report(n.saturating_add(1))
            }
            (State::Configuring, Outcome::CommandDone) => {
                self.state = State::SettingConfiguration;
                Step::Control(Setup::set_configuration(self.configuration))
            }
            (State::SettingConfiguration, Outcome::ControlDone) => self.class_request(0, false),
            (State::SettingProtocol(n), Outcome::ControlDone) => self.class_request(n, true),
            (State::SettingIdle(n), Outcome::ControlDone) => {
                self.class_request(n.saturating_add(1), false)
            }
            (State::SettingInterface(n), Outcome::ControlDone) => {
                match self.streams.get(n).filter(Stream::sets_rate) {
                    Some(stream) => {
                        self.state = State::SettingRate(n);
                        let (setup, data) =
                            Setup::set_sampling_frequency(stream.endpoint, stream.rate);
                        Step::ControlOut(setup, data)
                    }
                    None => self.select(n),
                }
            }
            (State::SettingRate(n), Outcome::ControlDone) => self.select(n),
            // The rate not set: the control endpoint, halted, is reset
            // first.
            (State::SettingRate(n), Outcome::ControlFailed(_)) => {
                self.state = State::ResettingControl {
                    interface: n,
                    then: Resume::RateRefused,
                };
                Step::ResetControl
            }
            (State::SettingProbe, Outcome::ControlDone) => match self.video.choice() {
                Some(choice) => {
                    self.state = State::GettingProbe;
                    Step::Control(probe::get(probe::PROBE, choice.interface, choice.probe_len))
                }
                None => self.stop(Stopped::Unexpected),
            },
            (State::GettingProbe, Outcome::ControlData(bytes)) => {
                let Some(choice) = self.video.choice() else {
                    return self.stop(Stopped::Unexpected);
                };
                match probe::answer(bytes, &choice) {
                    Ok(answer) => {
                        self.video = Video::Answered(choice, answer);
                        self.state = State::SettingCommit;
                        let setup = probe::set(probe::COMMIT, choice.interface, choice.probe_len);
                        let step = self
                            .video_out(setup, &probe::commit(bytes, usize::from(choice.probe_len)));
                        if let Step::ControlOut(setup, data) = step {
                            self.commit = Some((setup, data));
                        }
                        step
                    }
                    // Refused, but the request worked: nothing to reset.
                    Err(error) => {
                        self.video = Video::Refused(Refused::Answer(error));
                        self.ready()
                    }
                }
            }
            (State::SettingCommit, Outcome::ControlDone) => match self.video.choice() {
                Some(choice) => {
                    self.state = State::SettingVideoInterface;
                    Step::Control(Setup::set_interface(
                        choice.interface,
                        choice.alternate.alternate,
                    ))
                }
                None => self.stop(Stopped::Unexpected),
            },
            (State::SettingVideoInterface, Outcome::ControlDone) => {
                if let Video::Answered(choice, answer) = self.video {
                    self.video = Video::Streaming(choice, answer);
                }
                self.ready()
            }
            (State::SettingProbe, Outcome::ControlFailed(_)) => {
                self.drop_video(Refused::Request("the probe's SET_CUR"))
            }
            (State::GettingProbe, Outcome::ControlFailed(_)) => {
                self.drop_video(Refused::Request("the probe's GET_CUR"))
            }
            (State::SettingCommit, Outcome::ControlFailed(_)) => {
                self.drop_video(Refused::Request("the commit's SET_CUR"))
            }
            (State::SettingVideoInterface, Outcome::ControlFailed(_)) => {
                self.drop_video(Refused::Request("SET_INTERFACE"))
            }
            // A stream the device will not select is dropped; its endpoint,
            // set up but never started, stays idle. The failure halted the
            // control endpoint, so it is reset first.
            (State::SettingInterface(n), Outcome::ControlFailed(_)) => {
                self.state = State::ResettingControl {
                    interface: n,
                    then: Resume::NextInterface,
                };
                Step::ResetControl
            }
            (State::SettingProtocol(n), Outcome::ControlFailed(Completion::STALL)) => {
                self.state = State::ResettingControl {
                    interface: n,
                    then: Resume::Idle,
                };
                Step::ResetControl
            }
            (State::SettingIdle(n), Outcome::ControlFailed(Completion::STALL)) => {
                self.state = State::ResettingControl {
                    interface: n,
                    then: Resume::NextProtocol,
                };
                Step::ResetControl
            }
            // A report descriptor that cannot be read lays out nothing: the
            // interface goes, the device does not. Any failure halts the
            // control endpoint, so it is reset first.
            (State::ReadingReport(n), Outcome::ControlFailed(_)) => {
                if let Some(slot) = self.layouts.get_mut(n) {
                    *slot = None;
                }
                self.state = State::ResettingControl {
                    interface: n,
                    then: Resume::NextReport,
                };
                Step::ResetControl
            }
            (State::ResettingControl { interface, then }, Outcome::CommandDone) => match then {
                Resume::Idle => self.class_request(interface, true),
                Resume::NextProtocol => self.class_request(interface.saturating_add(1), false),
                Resume::NextReport => self.next_report(interface.saturating_add(1)),
                Resume::NextInterface => self.next_interface(interface.saturating_add(1)),
                // A stream that takes one rate runs at it whatever the
                // request did; one with a choice is dropped.
                Resume::RateRefused => {
                    if self
                        .streams
                        .get(interface)
                        .and_then(|stream| stream.format)
                        .is_some_and(|format| !format.rates.several())
                    {
                        self.select(interface)
                    } else {
                        self.next_interface(interface.saturating_add(1))
                    }
                }
                Resume::NoVideo => self.ready(),
            },
            (
                State::Addressing
                | State::EvaluatingControl
                | State::Configuring
                | State::ResettingControl { .. },
                Outcome::CommandFailed(code),
            )
            | (State::EnablingSlot, Outcome::CommandFailed(code)) => {
                self.stop(Stopped::Command(code))
            }
            (
                State::ReadingPrefix
                | State::ReadingDevice
                | State::ReadingConfigHeader
                | State::ReadingConfig
                | State::SettingConfiguration
                | State::SettingProtocol(_)
                | State::SettingIdle(_),
                Outcome::ControlFailed(code),
            ) => self.stop(Stopped::Control(code)),
            _ => self.stop(Stopped::Unexpected),
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
    use super::*;
    use crate::descriptor::tests::{KEYBOARD_CONFIG, KEYBOARD_DEVICE};
    use proptest::prelude::*;

    #[test]
    fn a_low_speed_keyboard_on_a_usb2_port() {
        let (mut machine, step) = Enumeration::start(2, None);
        assert_eq!(step, Step::ResetPort);
        assert_eq!(
            machine.on(Outcome::PortEnabled(Speed::Low)),
            Step::EnableSlot
        );
        assert_eq!(
            machine.on(Outcome::SlotEnabled(1)),
            Step::AddressDevice { max_packet: 8 }
        );
        assert_eq!(machine.slot(), 1);
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::get_descriptor(1, 0, 8))
        );
        assert_eq!(
            machine.on(Outcome::ControlData(&KEYBOARD_DEVICE[..8])),
            Step::Control(Setup::get_descriptor(1, 0, 18))
        );
        assert_eq!(
            machine.on(Outcome::ControlData(&KEYBOARD_DEVICE)),
            Step::Control(Setup::get_descriptor(2, 0, 9))
        );
        assert_eq!(machine.device().unwrap().vendor, 0x1234);
        assert_eq!(
            machine.on(Outcome::ControlData(&KEYBOARD_CONFIG[..9])),
            Step::Control(Setup::get_descriptor(2, 0, 34))
        );
        assert_eq!(
            machine.on(Outcome::ControlData(&KEYBOARD_CONFIG)),
            Step::ConfigureEndpoints
        );
        assert_eq!(machine.boot().len(), 1);
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::set_configuration(1))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_boot_protocol(0))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_idle(0))
        );
        assert!(!machine.finished());
        assert_eq!(machine.on(Outcome::ControlDone), Step::Poll);
        assert!(machine.polling() && machine.finished());
    }

    #[test]
    fn a_tablet_has_its_report_descriptor_read_and_no_class_requests() {
        use crate::descriptor::tests::TABLET_CONFIG;
        use crate::report::tests::{TABLET, TOUCHSCREEN};
        let (mut machine, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&TABLET_CONFIG[..9]),
        ] {
            machine.on(outcome);
        }
        assert_eq!(
            machine.on(Outcome::ControlData(&TABLET_CONFIG)),
            Step::Control(Setup::get_report_descriptor(0, 74))
        );
        assert_eq!(
            machine.on(Outcome::ControlData(TABLET)),
            Step::ConfigureEndpoints
        );
        assert_eq!(
            machine.layout(0).map(|layout| layout.kind()),
            Some(crate::report::PointerKind::Pointer)
        );
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::set_configuration(1))
        );
        assert_eq!(machine.on(Outcome::ControlDone), Step::Poll);

        // A touchscreen's descriptor, 57 bytes as its HID descriptor says: a
        // touch layout.
        let mut touch_config = TABLET_CONFIG;
        touch_config[25] = 57;
        let (mut touch, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&touch_config[..9]),
            Outcome::ControlData(&touch_config),
        ] {
            touch.on(outcome);
        }
        assert_eq!(
            touch.on(Outcome::ControlData(TOUCHSCREEN)),
            Step::ConfigureEndpoints
        );
        assert_eq!(
            touch.layout(0).map(|layout| layout.kind()),
            Some(crate::report::PointerKind::Touch)
        );

        // A tablet whose endpoint's packets (4 bytes) are shorter than its
        // report (6): its reports would arrive in pieces, so no layout.
        let mut small = TABLET_CONFIG;
        small[31] = 4;
        let (mut pieces, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&small[..9]),
            Outcome::ControlData(&small),
        ] {
            pieces.on(outcome);
        }
        assert_eq!(
            pieces.on(Outcome::ControlData(TABLET)),
            Step::Stop(Stopped::NoBootInterface)
        );
        // A descriptor shorter than its HID descriptor said: not read
        // whole, so no layout.
        let (mut short, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&TABLET_CONFIG[..9]),
            Outcome::ControlData(&TABLET_CONFIG),
        ] {
            short.on(outcome);
        }
        assert_eq!(
            short.on(Outcome::ControlData(&TABLET[..70])),
            Step::Stop(Stopped::NoBootInterface)
        );

        // A report descriptor with no absolute pointer: nothing to use.
        let (mut other, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&TABLET_CONFIG[..9]),
            Outcome::ControlData(&TABLET_CONFIG),
        ] {
            other.on(outcome);
        }
        assert_eq!(
            other.on(Outcome::ControlData(&[
                0x05, 0x07, 0x75, 8, 0x95, 6, 0x81, 0x00
            ])),
            Step::Stop(Stopped::NoBootInterface)
        );
    }

    #[test]
    fn a_report_descriptor_that_cannot_be_read_leaves_the_keyboard() {
        use crate::descriptor::tests::COMPOSITE_CONFIG;
        // A keyboard and a HID interface outside the boot subclass.
        let mut config = [0u8; 59];
        config.copy_from_slice(&COMPOSITE_CONFIG[..59]);
        config[2] = 59;
        config[4] = 2;
        let (mut machine, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&config[..9]),
        ] {
            machine.on(outcome);
        }
        assert_eq!(
            machine.on(Outcome::ControlData(&config)),
            Step::Control(Setup::get_report_descriptor(1, 40))
        );
        assert_eq!(machine.waiting_for(), "a report descriptor");
        assert_eq!(
            machine.on(Outcome::ControlFailed(Completion::STALL)),
            Step::ResetControl
        );
        assert_eq!(machine.on(Outcome::CommandDone), Step::ConfigureEndpoints);
        assert_eq!(
            machine
                .boot()
                .iter()
                .map(|found| found.device)
                .collect::<Vec<_>>(),
            [BootDevice::Keyboard]
        );
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::set_configuration(1))
        );
        // A transaction error does the same.
        let (mut other, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(2),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&config[..9]),
            Outcome::ControlData(&config),
        ] {
            other.on(outcome);
        }
        assert_eq!(
            other.on(Outcome::ControlFailed(Completion::TRANSACTION)),
            Step::ResetControl
        );
        // A reset that fails stops the device.
        assert_eq!(
            other.on(Outcome::CommandFailed(Completion::TRB)),
            Step::Stop(Stopped::Command(Completion::TRB))
        );
    }

    #[test]
    fn a_full_speed_device_with_a_bigger_control_packet_is_evaluated() {
        let (mut machine, _) = Enumeration::start(1, None);
        machine.on(Outcome::PortEnabled(Speed::Full));
        machine.on(Outcome::SlotEnabled(3));
        machine.on(Outcome::CommandDone);
        let mut prefix = KEYBOARD_DEVICE;
        prefix[7] = 64;
        assert_eq!(
            machine.on(Outcome::ControlData(&prefix[..8])),
            Step::EvaluateControl { max_packet: 64 }
        );
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::get_descriptor(1, 0, 18))
        );
    }

    #[test]
    fn a_usb3_port_skips_the_reset() {
        let (machine, step) = Enumeration::start(3, Some(Speed::Super));
        assert_eq!(step, Step::EnableSlot);
        assert_eq!(machine.speed(), Some(Speed::Super));
    }

    #[test]
    fn a_stalled_set_idle_is_reset_and_carried_on_with() {
        let (mut machine, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Low),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&KEYBOARD_CONFIG[..9]),
            Outcome::ControlData(&KEYBOARD_CONFIG),
            Outcome::CommandDone,
            Outcome::ControlDone,
            Outcome::ControlDone,
        ] {
            machine.on(outcome);
        }
        assert_eq!(
            machine.on(Outcome::ControlFailed(Completion::STALL)),
            Step::ResetControl
        );
        assert_eq!(machine.on(Outcome::CommandDone), Step::Poll);
    }

    #[test]
    fn hubs_failures_and_stray_outcomes_stop_it() {
        let (mut machine, _) = Enumeration::start(1, None);
        machine.on(Outcome::PortEnabled(Speed::High));
        assert_eq!(
            machine.on(Outcome::SlotEnabled(0)),
            Step::Stop(Stopped::NoSlot)
        );
        assert!(machine.finished() && !machine.polling());

        let (mut machine, _) = Enumeration::start(1, None);
        machine.on(Outcome::PortEnabled(Speed::High));
        machine.on(Outcome::SlotEnabled(1));
        machine.on(Outcome::CommandDone);
        let mut hub = KEYBOARD_DEVICE;
        hub[4] = 9;
        hub[7] = 64;
        machine.on(Outcome::ControlData(&hub[..8]));
        assert_eq!(
            machine.on(Outcome::ControlData(&hub)),
            Step::Stop(Stopped::Hub)
        );

        let (mut machine, _) = Enumeration::start(1, None);
        machine.on(Outcome::PortEnabled(Speed::Low));
        assert_eq!(
            machine.on(Outcome::CommandFailed(Completion::NO_SLOTS)),
            Step::Stop(Stopped::Command(Completion::NO_SLOTS))
        );

        let (mut machine, _) = Enumeration::start(1, None);
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Stop(Stopped::Unexpected)
        );
        let (mut machine, _) = Enumeration::start(1, None);
        assert_eq!(
            machine.on(Outcome::TimedOut),
            Step::Stop(Stopped::TimedOut("the port's reset"))
        );
        // A timeout names the step it ended.
        let (mut machine, _) = Enumeration::start(1, None);
        machine.on(Outcome::PortEnabled(Speed::Full));
        machine.on(Outcome::SlotEnabled(1));
        assert_eq!(machine.waiting_for(), "Address Device");
        assert_eq!(
            machine.on(Outcome::TimedOut),
            Step::Stop(Stopped::TimedOut("Address Device"))
        );
        assert_eq!(
            Stopped::TimedOut("Address Device").to_string(),
            "no answer in time to Address Device"
        );

        // A configuration with no boot interface.
        let (mut machine, _) = Enumeration::start(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Low),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&KEYBOARD_CONFIG[..9]),
        ] {
            machine.on(outcome);
        }
        let mut not_boot = KEYBOARD_CONFIG;
        not_boot[14] = 0xFF;
        assert_eq!(
            machine.on(Outcome::ControlData(&not_boot)),
            Step::Stop(Stopped::NoBootInterface)
        );
    }

    /// A speaker's enumeration, up to its configuration read, by a
    /// machine that streams or one that does not.
    fn speaker(streaming: bool) -> (Enumeration, Step) {
        use crate::audio::tests::SPEAKER_CONFIG;
        let (mut machine, _) = if streaming {
            Enumeration::start_streaming(5, None)
        } else {
            Enumeration::start(5, None)
        };
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&SPEAKER_CONFIG[..9]),
        ] {
            machine.on(outcome);
        }
        let step = machine.on(Outcome::ControlData(&SPEAKER_CONFIG));
        (machine, step)
    }

    #[test]
    fn a_speakers_stream_is_set_up_and_selected() {
        let (mut machine, step) = speaker(true);
        assert_eq!(step, Step::ConfigureEndpoints);
        assert!(machine.boot().is_empty());
        assert_eq!(machine.streams().len(), 1);
        assert_eq!(machine.selected().count(), 0, "not selected yet");
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Control(Setup::set_configuration(1))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_interface(1, 1))
        );
        assert_eq!(machine.waiting_for(), "SET_INTERFACE");
        assert_eq!(machine.on(Outcome::ControlDone), Step::Poll);
        assert!(machine.polling());
        assert_eq!(
            machine
                .selected()
                .map(|stream| stream.endpoint)
                .collect::<Vec<_>>(),
            [0x01]
        );
    }

    #[test]
    fn a_machine_that_streams_reads_a_longer_configuration() {
        // A configuration of 3000 bytes, as a camera's may be: a machine
        // that streams reads it whole, one that does not its first 512.
        let mut header = KEYBOARD_CONFIG;
        header[2..4].copy_from_slice(&3000u16.to_le_bytes());
        for (streaming, expected) in [(true, 3000), (false, CONFIG_MAX)] {
            let (mut machine, _) = if streaming {
                Enumeration::start_streaming(1, None)
            } else {
                Enumeration::start(1, None)
            };
            for outcome in [
                Outcome::PortEnabled(Speed::High),
                Outcome::SlotEnabled(1),
                Outcome::CommandDone,
            ] {
                machine.on(outcome);
            }
            let mut prefix = KEYBOARD_DEVICE;
            prefix[7] = 64;
            machine.on(Outcome::ControlData(&prefix[..8]));
            machine.on(Outcome::ControlData(&prefix));
            assert_eq!(
                machine.on(Outcome::ControlData(&header[..9])),
                Step::Control(Setup::get_descriptor(2, 0, expected))
            );
        }
    }

    #[test]
    fn a_driver_that_does_not_stream_leaves_a_speaker() {
        let (machine, step) = speaker(false);
        assert_eq!(step, Step::Stop(Stopped::NoBootInterface));
        assert!(machine.streams().is_empty());
    }

    #[test]
    fn a_stream_the_device_will_not_select_is_dropped() {
        let (mut machine, _) = speaker(true);
        machine.on(Outcome::CommandDone);
        machine.on(Outcome::ControlDone);
        assert_eq!(
            machine.on(Outcome::ControlFailed(Completion::STALL)),
            Step::ResetControl
        );
        // Nothing else to use: the device is left.
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Stop(Stopped::NothingToUse)
        );
        assert_eq!(machine.selected().count(), 0);
        assert_eq!(
            Stopped::NothingToUse.to_string(),
            "no keyboard or mouse boot interface, no absolute pointer and no audio stream"
        );
    }

    #[test]
    fn a_keyboard_with_a_microphone_gets_both() {
        use crate::audio::tests::SPEAKER_CONFIG;
        // The keyboard's configuration with the speaker's streaming
        // interface after it.
        let mut config = KEYBOARD_CONFIG.to_vec();
        config.extend_from_slice(&SPEAKER_CONFIG[48..]);
        let total = u16::try_from(config.len()).unwrap();
        config[2..4].copy_from_slice(&total.to_le_bytes());
        let (mut machine, _) = Enumeration::start_streaming(1, None);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&config[..9]),
        ] {
            machine.on(outcome);
        }
        assert_eq!(
            machine.on(Outcome::ControlData(&config)),
            Step::ConfigureEndpoints
        );
        assert_eq!((machine.boot().len(), machine.streams().len()), (1, 1));
        machine.on(Outcome::CommandDone);
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_boot_protocol(0))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_idle(0))
        );
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_interface(1, 1))
        );
        // The stream refused: the keyboard is still used.
        machine.on(Outcome::ControlFailed(Completion::STALL));
        assert_eq!(machine.on(Outcome::CommandDone), Step::Poll);
        assert_eq!(machine.selected().count(), 0);
    }

    /// A machine that streams, fed a device's descriptors up to its
    /// configuration `config` read, wanting `rate`.
    fn streaming_device(config: &[u8], rate: u32) -> (Enumeration, Step) {
        let (mut machine, _) = Enumeration::start_streaming(2, None);
        machine.want_rate(rate);
        for outcome in [
            Outcome::PortEnabled(Speed::Full),
            Outcome::SlotEnabled(1),
            Outcome::CommandDone,
            Outcome::ControlData(&KEYBOARD_DEVICE[..8]),
            Outcome::ControlData(&KEYBOARD_DEVICE),
            Outcome::ControlData(&config[..9]),
        ] {
            machine.on(outcome);
        }
        let step = machine.on(Outcome::ControlData(config));
        (machine, step)
    }

    #[test]
    fn a_speaker_with_a_choice_of_rates_has_its_rate_set() {
        use crate::audio::tests::ASYNC_SPEAKER_CONFIG;
        let (mut machine, step) = streaming_device(&ASYNC_SPEAKER_CONFIG, 44_100);
        assert_eq!(step, Step::ConfigureEndpoints);
        assert_eq!(machine.usable().count(), 1);
        machine.on(Outcome::CommandDone);
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::Control(Setup::set_interface(1, 2))
        );
        let (setup, data) = Setup::set_sampling_frequency(0x01, 44_100);
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::ControlOut(setup, data)
        );
        assert_eq!(machine.waiting_for(), "SET_CUR of a sampling frequency");
        assert_eq!(machine.selected().count(), 0, "not until its rate is set");
        assert_eq!(machine.on(Outcome::ControlDone), Step::Poll);
        let selected: Vec<_> = machine.selected().map(|stream| stream.rate).collect();
        assert_eq!(selected, [44_100]);
    }

    #[test]
    fn a_rate_refused_drops_a_stream_with_a_choice_and_keeps_one_without() {
        use crate::audio::tests::{ASYNC_SPEAKER_CONFIG, SPEAKER_CONFIG};
        let (mut machine, _) = streaming_device(&ASYNC_SPEAKER_CONFIG, 48_000);
        machine.on(Outcome::CommandDone);
        machine.on(Outcome::ControlDone);
        assert!(matches!(
            machine.on(Outcome::ControlDone),
            Step::ControlOut(..)
        ));
        assert_eq!(
            machine.on(Outcome::ControlFailed(Completion::STALL)),
            Step::ResetControl
        );
        assert_eq!(
            machine.on(Outcome::CommandDone),
            Step::Stop(Stopped::NothingToUse)
        );
        // The speaker of one rate, its endpoint given the control: the
        // request refused, it runs at that rate.
        let mut config = SPEAKER_CONFIG;
        config[96] = 0x01;
        let (mut machine, _) = streaming_device(&config, 44_100);
        machine.on(Outcome::CommandDone);
        machine.on(Outcome::ControlDone);
        let (setup, data) = Setup::set_sampling_frequency(0x01, 48_000);
        assert_eq!(
            machine.on(Outcome::ControlDone),
            Step::ControlOut(setup, data)
        );
        machine.on(Outcome::ControlFailed(Completion::STALL));
        assert_eq!(machine.on(Outcome::CommandDone), Step::Poll);
        assert_eq!(machine.selected().count(), 1);
    }

    #[test]
    fn a_device_whose_every_stream_is_refused_is_left() {
        use crate::audio::tests::SPEAKER_CONFIG;
        // Three channels.
        let mut config = SPEAKER_CONFIG;
        config[77] = 3;
        let (machine, step) = streaming_device(&config, 48_000);
        assert_eq!(step, Step::Stop(Stopped::NothingToUse));
        let refused: Vec<_> = machine
            .streams()
            .iter()
            .map(|stream| stream.refusal)
            .collect();
        assert_eq!(refused, [Some(crate::audio::Refusal::Channels(3))]);
        assert_eq!(machine.usable().count(), 0);
    }

    fn outcome_strategy() -> impl Strategy<Value = (u8, u8, Vec<u8>)> {
        (
            0u8..9,
            any::<u8>(),
            proptest::collection::vec(any::<u8>(), 0..40),
        )
    }

    proptest! {
        #[test]
        fn any_sequence_of_outcomes_ends_or_waits_without_panicking(
            outcomes in proptest::collection::vec(outcome_strategy(), 0..30),
            streaming: bool,
        ) {
            let (mut machine, _) = if streaming {
                Enumeration::start_streaming(1, None)
            } else {
                Enumeration::start(1, None)
            };
            for (kind, value, bytes) in &outcomes {
                let outcome = match kind {
                    0 => Outcome::PortEnabled(Speed::from_id(value % 6).unwrap_or(Speed::Full)),
                    1 => Outcome::SlotEnabled(*value),
                    2 => Outcome::CommandDone,
                    3 => Outcome::CommandFailed(Completion(*value)),
                    4 => Outcome::ControlData(bytes),
                    5 => Outcome::ControlDone,
                    6 => Outcome::ControlFailed(Completion(*value)),
                    7 => Outcome::TimedOut,
                    _ => Outcome::ControlData(&KEYBOARD_CONFIG),
                };
                let step = machine.on(outcome);
                if let Step::Stop(_) = step {
                    prop_assert!(machine.finished() && !machine.polling());
                }
                if step == Step::Poll {
                    prop_assert!(machine.polling() && machine.slot() != 0);
                    prop_assert!(!machine.boot().is_empty() || machine.selected().count() > 0);
                    if !streaming {
                        prop_assert!(!machine.boot().is_empty() && machine.streams().is_empty());
                    }
                }
            }
        }
    }
}
