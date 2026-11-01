// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-usb-host`: the USB host PD. It drives one
//! xHCI controller through `nowayos-hw` - RP1's on the Raspberry Pi 5,
//! QEMU's `qemu-xhci` in QEMU - and has no `unsafe` code of its own.
//!
//! It binds device 0, the controller's register window, and DMA region 0,
//! its rings and contexts, with the board's facts about the controller
//! (read-only region 5). Where the facts say the driver places the
//! controller itself - QEMU, which has no firmware to enumerate PCI - it
//! binds device 1, the controller's PCI configuration page, and places its
//! BAR 0 where the board file says first (`nowayos_hw::pci`). On the Pi 5,
//! U-Boot's enumeration has placed RP1 already.
//!
//! **Start-up.** It identifies the controller from its capability
//! registers, takes it from the firmware - which on the Pi 5 leaves it
//! running - halts and resets it, sets it up in its own DMA region at the
//! bus addresses the facts' DMA offset gives and runs it, and sends a No Op
//! command, whose completion event shows that the controller reads the
//! command ring and writes the event ring by DMA. Then it reports each root
//! port, powering any that is not.
//!
//! **Devices.** On a timer from the timer PD (channel 0), every
//! [`TICK_MS`] milliseconds - the controller's interrupts do not reach a PD
//! on the Pi 5, so it polls - it drains the controller's events and
//! enumerates each device attached to a root port, one at a time
//! (`nowayos_usb::enumerate`): it resets the port, waits out USB 2.0's
//! reset recovery time, enables a slot, addresses the device, reads its
//! device and configuration descriptors and reports its vendor, product
//! and class, then sets up the interrupt IN endpoint of each HID interface
//! it can use - a boot keyboard or mouse, or an interface whose report
//! descriptor lays out an absolute pointer - and starts its
//! reports. A hub, a device with no such interface, or a step that fails
//! or takes more than [`STEP_MS`] milliseconds ends that device's
//! enumeration, and its slot is given back; a command that does not
//! complete in time is aborted, and each completion is matched with its
//! own command, so one that completes late is never taken for a later
//! one. A device unplugged is forgotten, and one plugged in - or unplugged
//! and plugged back between two ticks - is enumerated afresh once its
//! connection has held for [`DEBOUNCE_MS`] milliseconds, USB 2.0's attach
//! debounce, so a connection that flaps is never enumerated; and a device
//! is enumerated at most [`MAX_ATTEMPTS`] times each time it is plugged
//! in. It reads each root port's status on every tick rather than waiting
//! for Port Status Change Events: the controller raises one only when a
//! change bit sets while none was set, so a change left set - as RP1's
//! controller leaves a connection made before it runs - would hide every
//! later one.
//!
//! **Input**. Each boot keyboard's and mouse's reports become
//! canonical [`InputEvent`]s (`nowayos_usb::hid`): a key down or
//! up for each key a keyboard report changes, releases first; the pointer
//! moved by a mouse report's displacement, clamped to the screen the system
//! gives it (read-only region 7), and each button that changed. They go to
//! the input broker through one ring (read-write region 0 and read-only
//! region 0, channel 1), so the broker holds this controller's devices to
//! one rate; what does not fit waits in a small queue for the broker to
//! drain the ring - one notification and the broker, above this PD, has
//! drained it - with pointer moves, and a touch's moves, not yet sent
//! folded into the last, and places in it kept for releases. A device
//! unplugged, or whose reports stop - enumerated again then, while it has
//! attempts left, but on a USB 3 port left alone - has every key and button
//! it held released.
//! Reports the boot protocol does not have - a wrong length, a rollover, an
//! undefined key - and events that do not fit the queue are dropped and
//! counted, and the counts reported when they change, at most once a
//! second, as every other kind of trouble is. A report's contents are what
//! a person entered and never go into a log: a device's first report is
//! logged by its length alone.
//!
//! **Isochronous streams**. On a controller whose DMA region holds
//! `nowayos_hw::xhci::DMA_SIZE_STREAMING` bytes, the PD streams USB audio
//! interfaces (`nowayos_usb::audio`): of each, the best alternate setting
//! with an isochronous endpoint - one it supports, then 16-bit samples,
//! then the rate it wants - selects that setting, sets the rate where the
//! endpoint has a choice, and keeps [`STREAM_AHEAD_US`] of
//! transfers queued ahead of the controller, refilled on every tick. **A
//! missed interval is counted, never fatal** (`nowayos_usb::isoch`): every
//! [`STREAM_REPORT_MS`] each stream's line says how long it has run and how
//! many intervals it missed - "no interval missed", or "N intervals missed
//! so far" - and another its intervals served and bytes moved. A stream it
//! does not support - not PCM, more than two channels, no rate its packets
//! hold - is refused, and the PD says why.
//!
//! **A call's sound**. Where the system gives it a client's audio
//! rings and channel (`client`, `nowayos_audio`), a microphone's sound goes
//! to the client as 16-bit PCM at the device's rate, on a lossy ring - a
//! client that falls behind loses the oldest sound - and a speaker plays
//! the 16-bit PCM the client sends in the speaker's format, and silence
//! when it has sent none: the PD never waits for its client. An
//! asynchronous speaker's feedback endpoint sets how many frames each
//! packet carries. One speaker and one microphone, the first found. Started
//! instead with [`init_test_tone`] - a test instrument's program, never a
//! product's - the PD plays a [`TONE_HZ`] tone to a speaker and counts a
//! microphone's packets, keeping none.
//!
//! **A camera's video**. Started with [`init_camera`] - a test
//! instrument's program too, for now - on a controller whose DMA region
//! holds `nowayos_hw::xhci::DMA_SIZE_CAMERA` bytes, the PD also finds a
//! USB camera (`nowayos_usb::video`): lists its Motion JPEG sizes and rates,
//! refuses one with none, chooses 1280 x 720 at 30 a second where offered,
//! sets it by the probe and commit, selects the alternate setting with the
//! most bandwidth, and streams its isochronous video endpoint from a pool of
//! buffers, a microphone's way. Each payload's header is checked and
//! removed and whole frames go into a ring of [`FRAME_SLOTS`] slots for a
//! client PD (`camera`): a frame that starts with no slot free is dropped,
//! never queued, and a damaged one is dropped, each counted; every
//! [`STREAM_REPORT_MS`] a line says how many went, their sizes and the
//! drops. The frames are never decoded.
//!
//! **Nothing a device sends is trusted:** `nowayos-hw` copies every event,
//! descriptor and report out of DMA memory before anything reads it, and
//! `nowayos-usb` checks each before it is used; nothing it refuses can
//! panic the PD (`make fuzz`).
//!
//! Every line it prints starts with its PD name, so two controllers' lines
//! are told apart; what differs by board - the controller's version and
//! ports, what is attached - is printed but checked only where the system's
//! QEMU devices fix it.
//!
//! A library and a program, as the timer and display PDs are:
//! NoWayOS's systems run the program; a product's system runs the PD from a
//! program of its own whose `init` returns [`init`]'s, with the same regions
//! and channels.

#![no_std]

mod camera;
mod client;

use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_hw::pci::PciFunction;
use nowayos_hw::xhci::{
    CONTROL_BUFFER_LARGE, CommandId, MAX_DEVICES, MAX_STREAMS, Xhci, XhciEvent,
};
use nowayos_hw::{HwError, device, dma_region};
use nowayos_input::{InputEvent, InputLimits, TouchPhase};
use nowayos_ipc::Producer;
use nowayos_region::{RingTx, device_facts, read_only, read_write, ring_producer, screen};
use nowayos_timer_api::Client as Timer;
use nowayos_usb::audio::{self, Pcm, Stream};
use nowayos_usb::capture::{self, Ask, Gate};
use nowayos_usb::context::{interval_micros, isoch_interval};
use nowayos_usb::descriptor::{BootDevice, BootInterface, MAX_BOOT_INTERFACES};
use nowayos_usb::enumerate::{Enumeration, Outcome, Step, Video};
use nowayos_usb::hid::{Dropped, KeyRepeat, Keyboard, Mouse};
use nowayos_usb::isoch::{FrameClock, Tone};
use nowayos_usb::port::{PortWrite, Speed};
use nowayos_usb::report::{AbsolutePointer, Field, PointerKind};
use nowayos_usb::trb::{Completion, ISOCH_MAX, kind};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println, pd_name};

pub use camera::FRAME_SLOTS;

/// A stream's place before one is copied in: never configured.
const EMPTY_STREAM: Stream = Stream {
    interface: 0,
    alternate: 0,
    endpoint: 0,
    max_packet: 0,
    transactions: 1,
    interval: 1,
    sync: 0,
    format: None,
    max_packets_only: false,
    rate: 0,
    rate_control: false,
    feedback: None,
    refusal: None,
};

/// The device number of the controller's register window.
const CONTROLLER: usize = 0;

/// The device number of its PCI configuration page, where the driver places
/// the controller itself.
const CONFIGURATION: usize = 1;

/// The DMA region of its rings and contexts.
const MEMORY: usize = 0;

/// The channel to the timer PD: the PD calls it, and it notifies the PD
/// when the tick is due.
const TIMER: Timer = Timer::new(Channel::new(0));

/// The tick's id with the timer PD.
const TICK: u64 = 1;

/// The channel to the input broker, on which only this PD notifies.
const BROKER: Channel = Channel::new(1);

/// The ring to the broker: the slots and slot size every input source's
/// ring has (`nowayos-input-broker`'s).
const RING_SLOTS: usize = 4;
const RING_SLOT: usize = 32;

/// Events waiting for room in the ring.
const QUEUE: usize = 128;

/// Of those, the places only a release may take - a key or button up, a
/// touch's end - so a flood of presses and moves never leaves a key or
/// button down.
const RESERVED: usize = 32;

/// The most times one tick fills the ring and notifies the broker.
const FLUSHES: usize = 16;

/// The largest screen a pointer is clamped to.
const MAX_SIDE: u32 = 8192;

/// How often the PD drains the controller's events.
const TICK_MS: u64 = 8;

/// How long after its start-up the PD first drains events and enumerates:
/// long enough for the PDs below it to print their own start-up lines
/// first - at 115200 baud a line takes up to 8 ms, and lines from two PDs
/// mix if one is held mid-line (`pds/README.md`, "When several PDs
/// print").
const FIRST_TICK_MS: u64 = 500;

/// How long the No Op command may take to come back at start-up.
const COMMAND_MS: u64 = 100;

/// How long any one enumeration step may take: a USB 2.0 port reset is 50
/// ms, a control transfer a few frames.
const STEP_MS: u64 = 1000;

/// How long a device is left after its port's reset ends before it is
/// addressed: USB 2.0's reset recovery time, TRSTRCY (section 7.1.7.5).
const RECOVERY_MS: u64 = 10;

/// How long a connection must hold before its device is enumerated: USB
/// 2.0's attach debounce interval, TATTDB (section 7.1.7.3). A connection
/// that flaps never holds that long, so it is never enumerated.
const DEBOUNCE_MS: u64 = 100;

/// How many times a device is enumerated once plugged in - again after its
/// reports stop, or after it is replugged before it is ready - before it is
/// left alone until its port has been empty for [`DEBOUNCE_MS`].
const MAX_ATTEMPTS: u8 = 4;

/// The least time between two lines of one kind of trouble: a device or a
/// controller that misbehaves cannot flood the serial line from this PD,
/// which runs above the UI.
const WARN_MS: u64 = 1000;

/// The most events drained on one tick: as many as the event ring holds,
/// so a tick's isochronous completions - one a millisecond for each audio
/// stream, eight for a high-speed stream every microframe - never pile up.
const EVENTS_PER_TICK: usize = 256;

/// How far ahead of the controller each isochronous stream's transfers are
/// queued: two ticks, and the 8 ms or so a long line takes to
/// print at 115200 baud, a few times over.
pub const STREAM_AHEAD_US: u32 = 48_000;

/// How often each stream's counts are reported.
pub const STREAM_REPORT_MS: u64 = 10_000;

/// The test tone a speaker plays: 440 Hz, a quarter of full
/// scale.
pub const TONE_HZ: u32 = 440;
const TONE_AMPLITUDE: u16 = 0x2000;

/// The root ports this driver keeps track of; a controller with more has
/// the rest ignored.
const MAX_PORTS: usize = 16;

/// What the enumeration in flight waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    /// Its port's reset to finish.
    PortReset,
    /// This command to complete.
    Command(CommandId),
    /// Reset Endpoint, then Set TR Dequeue Pointer, on the control
    /// endpoint: the first's failure or the second's completion ends it.
    ControlReset {
        reset: CommandId,
        dequeue: CommandId,
    },
    /// A control transfer on its slot to end, which brings data or not.
    Control { data: bool },
}

/// The counter as microseconds, for key repeats: in 128 bits, as
/// the counter times a million passes 64 bits within minutes.
fn micros() -> u64 {
    u128::from(counter())
        .saturating_mul(1_000_000)
        .checked_div(u128::from(counter_frequency().max(1)))
        .and_then(|us| u64::try_from(us).ok())
        .unwrap_or(u64::MAX)
}

/// The one enumeration in flight.
#[derive(Clone, Copy, Debug)]
struct Active {
    machine: Enumeration,
    waiting: Waiting,
    deadline: u64,
    reported: bool,
    /// When its port's reset was first seen to have ended.
    reset_ended: Option<u64>,
}

/// What a root port holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PortUse {
    /// Nothing, or nothing seen yet.
    #[default]
    Empty,
    /// A device connected, until its connection has held for
    /// [`DEBOUNCE_MS`].
    Settling,
    /// A device waiting to be enumerated.
    Waiting,
    /// The device being enumerated.
    Enumerating,
    /// A keyboard or mouse whose reports flow, in this slot.
    Polled(u8),
    /// A device enumeration gave up on: left until it is unplugged.
    Unused,
}

/// What turns one HID interface's reports into events.
#[derive(Clone, Copy, Debug)]
enum Decoder {
    /// A boot keyboard, and its held key's repeats.
    Keyboard(Keyboard, KeyRepeat),
    Mouse(Mouse),
    /// A touchscreen or tablet, laid out as its report descriptor says.
    Absolute(AbsolutePointer),
}

impl Decoder {
    /// What it is, for a log line.
    const fn name(&self) -> &'static str {
        match self {
            Self::Keyboard(..) => "keyboard",
            Self::Mouse(_) => "mouse",
            Self::Absolute(pointer) => match pointer.layout().kind() {
                PointerKind::Touch => "touchscreen",
                PointerKind::Pointer => "tablet",
            },
        }
    }

    fn dropped(&self) -> Dropped {
        match self {
            Self::Keyboard(keyboard, _) => keyboard.dropped(),
            Self::Mouse(mouse) => mouse.dropped(),
            Self::Absolute(pointer) => Dropped {
                wrong_length: pointer.refused(),
                ..Dropped::default()
            },
        }
    }

    /// One report, its events passed to `emit`; a keyboard's also start or
    /// stop its repeats, `now` microseconds on the counter.
    fn report(&mut self, report: &[u8], now: u64, mut emit: impl FnMut(InputEvent)) {
        match self {
            Self::Keyboard(keyboard, repeat) => keyboard.report(report, |event| {
                repeat.saw(event, now);
                emit(event);
            }),
            Self::Mouse(mouse) => mouse.report(report, emit),
            Self::Absolute(pointer) => pointer.report(report, emit),
        }
    }

    fn release_all(&mut self, emit: impl FnMut(InputEvent)) {
        match self {
            Self::Keyboard(keyboard, repeat) => {
                repeat.stop();
                keyboard.release_all(emit);
            }
            Self::Mouse(mouse) => mouse.release_all(emit),
            Self::Absolute(pointer) => pointer.release_all(emit),
        }
    }
}

/// What an isochronous stream carries.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A speaker's, for a test instrument: the test tone, packet by packet
    /// at its rate.
    Tone {
        tone: Tone,
        clock: FrameClock,
        format: Pcm,
    },
    /// A microphone's, for a test instrument: packets counted, never kept.
    Counted,
    /// A speaker's: what the client sends, packet by packet at
    /// its rate - or its device's pace - and silence when it has sent
    /// nothing.
    Speaker {
        clock: FrameClock,
        format: Pcm,
        max_packets_only: bool,
    },
    /// A microphone's: its sound to the client.
    Microphone {
        clock: FrameClock,
        format: Pcm,
        max_packets_only: bool,
    },
    /// An asynchronous speaker's feedback endpoint: what it reads
    /// paces stream `paces`, whose packets come every `interval_us` and
    /// carry `nominal` 65536ths of a frame at its rate.
    Feedback {
        paces: usize,
        speed: Speed,
        interval_us: u32,
        nominal: u64,
    },
}

/// An isochronous stream the PD keeps fed, by its stream number
/// in `nowayos_hw::xhci`.
#[derive(Clone, Copy, Debug)]
struct Playing {
    port: u8,
    slot: u8,
    endpoint: u8,
    /// The TDs kept queued.
    depth: usize,
    source: Source,
    /// When it started, on the counter.
    started: u64,
    /// The reports made, one every [`STREAM_REPORT_MS`].
    reports: u64,
}

impl Playing {
    const fn direction(&self) -> &'static str {
        if self.endpoint & 0x80 != 0 {
            "in"
        } else {
            "out"
        }
    }
}

/// A field's place in a report, for a log line.
struct Place(Field);

impl core::fmt::Display for Place {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "bit {}, {} bits, {} to {}",
            self.0.bit, self.0.bits, self.0.min, self.0.max
        )
    }
}

/// A device whose reports flow: a decoder for each of its boot interfaces.
#[derive(Clone, Copy, Debug)]
struct Attached {
    port: u8,
    decoders: [Option<Decoder>; MAX_BOOT_INTERFACES],
    reports: [u32; MAX_BOOT_INTERFACES],
}

/// Events waiting for room in the ring to the broker, oldest first.
#[derive(Clone, Copy, Debug)]
struct Outbox {
    events: [Option<InputEvent>; QUEUE],
    head: usize,
    len: usize,
    /// Events that did not fit.
    dropped: u32,
}

impl Outbox {
    const fn new() -> Self {
        Self {
            events: [None; QUEUE],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }

    fn at(&self, offset: usize) -> usize {
        self.head
            .saturating_add(offset)
            .checked_rem(QUEUE)
            .unwrap_or(0)
    }

    /// Queues `event`. A pointer move folds into a move still waiting at
    /// the end, and a touch's move into a move of the same contact there,
    /// so a flood of motion costs one place; a touch's start and end never
    /// fold. A release may take any place; anything else leaves
    /// [`RESERVED`] places for releases.
    fn push(&mut self, event: InputEvent) {
        let last = self
            .len
            .checked_sub(1)
            .map(|last| self.at(last))
            .and_then(|last| self.events.get_mut(last));
        if let Some(slot @ Some(_)) = last {
            let folds = match (*slot, event) {
                (Some(InputEvent::PointerMove { .. }), InputEvent::PointerMove { .. }) => true,
                (
                    Some(InputEvent::Touch {
                        id: queued,
                        phase: TouchPhase::Move,
                        ..
                    }),
                    InputEvent::Touch {
                        id,
                        phase: TouchPhase::Move,
                        ..
                    },
                ) => queued == id,
                _ => false,
            };
            if folds {
                *slot = Some(event);
                return;
            }
        }
        let release = matches!(
            event,
            InputEvent::KeyUp(_)
                | InputEvent::PointerButton { down: false, .. }
                | InputEvent::Touch {
                    phase: TouchPhase::End | TouchPhase::Cancel,
                    ..
                }
        );
        let room = if release {
            QUEUE
        } else {
            QUEUE.saturating_sub(RESERVED)
        };
        if self.len >= room {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        let end = self.at(self.len);
        if let Some(slot) = self.events.get_mut(end) {
            *slot = Some(event);
            self.len = self.len.saturating_add(1);
        }
    }

    fn front(&self) -> Option<InputEvent> {
        if self.len == 0 {
            return None;
        }
        self.events.get(self.head).copied().flatten()
    }

    fn pop(&mut self) {
        if self.len > 0 {
            if let Some(slot) = self.events.get_mut(self.head) {
                *slot = None;
            }
            self.head = self.at(1);
            self.len = self.len.saturating_sub(1);
        }
    }
}

/// Starts the USB host PD: binds its controller, its DMA memory, the
/// board's facts about the controller and its ring to the broker, and asks
/// the timer for its first tick.
///
/// Where the system also gives it a client's audio rings (the
/// regions and channel `nowayos_audio` describes) and a DMA region of
/// `nowayos_hw::xhci::DMA_SIZE_STREAMING` bytes, it streams a USB speaker
/// and microphone for that client, each at the rate it takes nearest
/// [`audio::DEFAULT_RATE`].
#[must_use]
pub fn init() -> UsbHost {
    start_pd(false, audio::DEFAULT_RATE)
}

/// As [`init`], its audio streams set to the rate nearest `rate` each takes
/// and its packets hold: a product whose client needs 44.1 kHz
/// asks for it here, so a device that takes it needs no resampling.
#[must_use]
pub fn init_with_rate(rate: u32) -> UsbHost {
    start_pd(false, rate)
}

/// As [`init`], for a test instrument: the PD also streams every
/// USB audio interface it finds - a [`TONE_HZ`] tone to a speaker, a
/// microphone's packets counted and dropped - and reports each stream's
/// intervals served and missed.
#[must_use]
pub fn init_test_tone() -> UsbHost {
    start_pd(true, audio::DEFAULT_RATE)
}

/// As [`init_test_tone`], for a system with a client for a camera's frames:
/// the PD also streams the first USB camera's Motion JPEG video it
/// finds into the ring of frames the system gives it - the frames region and
/// its page read-write (regions 1 and 2), the client's page read-only
/// (region 1) - and notifies the client (channel 2) of each whole frame.
#[must_use]
pub fn init_camera() -> UsbHost {
    let mut pd = start_pd(true, audio::DEFAULT_RATE);
    // Before the first tick, so before any device is enumerated.
    pd.frames = camera::frame_ring(pd.name, camera::ALONE);
    pd
}

/// As [`init_with_rate`], for a station's calls: the PD serves its
/// audio client - the rings at regions 1 and 2, channel 2 - and beside it
/// streams the first USB camera's Motion JPEG video it finds into the ring
/// of frames the system gives it there: the frames region and its page
/// read-write (regions 3 and 4), the client's page read-only (region 3),
/// the same client notified of each whole frame on the same channel, 2. A
/// product whose client needs 44.1 kHz asks for it as `rate`.
#[must_use]
pub fn init_calls(rate: u32) -> UsbHost {
    let mut pd = start_pd(false, rate);
    pd.frames = camera::frame_ring(pd.name, camera::BESIDE_AUDIO);
    pd.streaming = pd.streaming || pd.frames.is_some();
    pd
}

fn start_pd(test_tone: bool, wanted_rate: u32) -> UsbHost {
    let name = pd_name().unwrap_or("usb");
    debug_println!("NOWAYOS: {name}: starting");
    // A client's audio rings, where the system maps them.
    let client = if test_tone {
        None
    } else {
        client::bind().unwrap_or_else(|error| {
            debug_println!("NOWAYOS: {name}: no audio client: {error}");
            None
        })
    };
    let mut pd = UsbHost {
        streaming: test_tone || client.is_some(),
        test_tone,
        client,
        wanted_rate,
        playing: [None; MAX_STREAMS],
        name,
        xhci: None,
        ports: [PortUse::Empty; MAX_PORTS],
        active: None,
        attached: [None; MAX_DEVICES],
        refused: 0,
        ring: None,
        limits: None,
        outbox: Outbox::new(),
        reported: (Dropped::default(), 0, 0),
        warned: Warned::default(),
        changed_at: [0; MAX_PORTS],
        attempts: [0; MAX_PORTS],
        frames: None,
        camera: None,
        capture: None,
    };
    // The ring to the broker and the screen, before anything else: without
    // them no report has anywhere to go.
    match read_write(0).and_then(|messages| ring_producer(messages, read_only(0)?)) {
        Ok(ring) => pd.ring = Some(ring),
        Err(error) => debug_println!("NOWAYOS: {name}: no ring to the input broker: {error}"),
    }
    pd.limits = match screen() {
        Ok((width, height))
            if (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height) =>
        {
            u16::try_from(width)
                .ok()
                .zip(u16::try_from(height).ok())
                .and_then(|(width, height)| InputLimits::new(width, height))
        }
        Ok((width, height)) => {
            debug_println!("NOWAYOS: {name}: a {width}x{height} screen: no pointer");
            None
        }
        Err(error) => {
            debug_println!("NOWAYOS: {name}: the screen's size: {error}: no pointer");
            None
        }
    };
    match start(name) {
        Ok(xhci) => {
            pd.xhci = Some(xhci);
            debug_println!("NOWAYOS: {name}: ready");
            pd.scan();
            if let Err(error) = TIMER.set_timeout(TICK, FIRST_TICK_MS.saturating_mul(1_000_000)) {
                debug_println!("NOWAYOS: {name}: could not set the timer: {error:?}");
            }
        }
        Err(error) => debug_println!("NOWAYOS: {name}: gave up: {error}"),
    }
    pd
}

/// Identifies, resets and runs the controller, checks a command comes back
/// through DMA, and powers its ports.
fn start(name: &str) -> Result<Xhci, HwError> {
    let facts = match device_facts() {
        Ok(facts) => facts,
        Err(error) => {
            debug_println!("NOWAYOS: {name}: the board's facts: {error}");
            return Err(HwError::Xhci {
                reason: "no facts about the controller",
            });
        }
    };
    if let Some(address) = facts.pci_bar0() {
        let mut function = PciFunction::new(device(CONFIGURATION)?)?;
        let identity = function.identity()?;
        let size = function.place_bar0(address, facts.registers())?;
        debug_println!(
            "NOWAYOS: {name}: PCI function {:04x}:{:04x}, class {:06x}: its {size:#x}-byte window placed at bus address {address:#x}",
            identity.vendor,
            identity.device,
            identity.class
        );
    }
    let mut xhci = Xhci::new(device(CONTROLLER)?, dma_region(MEMORY)?, facts.dma_offset())?;
    let caps = *xhci.capabilities();
    debug_println!(
        "NOWAYOS: {name}: xHCI {}.{:x}, {} device slots, {} interrupters, {} ports, {}-byte contexts, {} scratchpad buffers",
        caps.version >> 8,
        caps.version & 0xFF,
        caps.max_slots,
        caps.max_interrupters,
        caps.max_ports,
        if caps.context_64 { 64 } else { 32 },
        caps.scratchpads
    );
    for protocol in xhci.protocols() {
        debug_println!(
            "NOWAYOS: {name}: USB {}.{:x} on ports {} to {}",
            protocol.major,
            protocol.minor >> 4,
            protocol.first_port,
            protocol
                .first_port
                .saturating_add(protocol.ports.saturating_sub(1))
        );
    }
    debug_println!(
        "NOWAYOS: {name}: it sees memory at bus address physical + {:#x}",
        facts.dma_offset()
    );
    debug_println!("NOWAYOS: {name}: controller identified");
    let (command, status) = xhci.command_and_status()?;
    debug_println!(
        "NOWAYOS: {name}: handed over {} (USBCMD {command:#x}, USBSTS {status:#x})",
        if command & 1 != 0 {
            "running"
        } else {
            "stopped"
        }
    );
    if xhci.take_from_firmware()? {
        debug_println!("NOWAYOS: {name}: the firmware let go of it");
    }
    xhci.halt_and_reset()?;
    debug_println!("NOWAYOS: {name}: halted and reset");
    xhci.start()?;
    xhci.no_op()?;
    let ticks = counter_frequency().saturating_mul(COMMAND_MS) / 1000;
    let begun = counter();
    loop {
        let event = xhci.next_event();
        if let (Err(_), Some((pointer, ring))) = (&event, xhci.last_refused()) {
            debug_println!(
                "NOWAYOS: {name}: a completion names TRB {pointer:#x}; the command ring is at {ring:#x}"
            );
        }
        match event? {
            Some(XhciEvent::Command {
                kind: kind::NO_OP_COMMAND,
                code: Completion::SUCCESS,
                ..
            }) => break,
            Some(XhciEvent::Command { kind, code, .. }) => {
                debug_println!("NOWAYOS: {name}: command {kind} completed: {code}");
                return Err(HwError::Xhci {
                    reason: "the No Op command failed",
                });
            }
            Some(_) | None => {}
        }
        if counter().wrapping_sub(begun) > ticks {
            let (_, status) = xhci.command_and_status()?;
            let (crcr, physical, bus) = xhci.command_ring_state()?;
            debug_println!(
                "NOWAYOS: {name}: USBSTS {status:#x}, CRCR {crcr:#x}; memory at physical {physical:#x}, bus {bus:#x}"
            );
            return Err(HwError::XhciTimeout {
                what: "a No Op command (no event came back through DMA)",
            });
        }
        core::hint::spin_loop();
    }
    debug_println!("NOWAYOS: {name}: running: a No Op command came back through DMA");
    for port in 1..=caps.max_ports {
        let status = xhci.port_status(port)?;
        if caps.port_power && !status.powered() {
            xhci.port_write(port, PortWrite::Power)?;
        }
        debug_println!(
            "NOWAYOS: {name}: port {port} (USB {}): {}, {}",
            xhci.major_of(port).unwrap_or(0),
            if status.powered() {
                "powered"
            } else {
                "powering"
            },
            match status.speed() {
                Some(speed) => speed.name(),
                None if status.connected() => "a device of unknown speed",
                None => "nothing attached",
            }
        );
    }
    Ok(xhci)
}

/// The USB host PD: its controller, its ports, the enumeration in flight
/// and the devices whose reports flow.
pub struct UsbHost {
    /// Whether it streams audio interfaces: as a test instrument
    /// ([`init_test_tone`]), or to a client.
    streaming: bool,
    /// Whether it is the test instrument.
    test_tone: bool,
    /// The client its audio streams go to and come from.
    client: Option<client::Client>,
    /// The rate its audio streams are set to where they have a choice.
    wanted_rate: u32,
    /// The isochronous streams it keeps fed, by stream number.
    playing: [Option<Playing>; MAX_STREAMS],
    name: &'static str,
    xhci: Option<Xhci>,
    ports: [PortUse; MAX_PORTS],
    active: Option<Active>,
    /// By slot: slot `n` at index `n - 1`.
    attached: [Option<Attached>; MAX_DEVICES],
    /// Events refused so far.
    refused: u32,
    /// The ring to the input broker.
    ring: Option<RingTx<RING_SLOTS, RING_SLOT>>,
    /// The screen pointers are clamped to.
    limits: Option<InputLimits>,
    /// Events waiting for room in the ring.
    outbox: Outbox,
    /// The drop counts last reported: the decoders', the queue's, and the
    /// events refused.
    reported: (Dropped, u32, u32),
    /// When each kind of trouble was last reported.
    warned: Warned,
    /// By port: when its connection last changed, on the counter.
    changed_at: [u64; MAX_PORTS],
    /// By port: the enumerations started since its device was plugged in.
    attempts: [u8; MAX_PORTS],
    /// The ring of frames to a camera's client ([`init_camera`]).
    frames: Option<camera::FrameRing>,
    /// The camera's video, while it streams.
    camera: Option<camera::CameraStream>,
    /// The client's microphone's capture gate: it captures only
    /// while the client asks.
    capture: Option<Gating>,
}

/// A stream's capture gate - the client's microphone's or the
/// camera's - and its requests on its device's control endpoint.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Gating {
    /// Its stream, by its number in `nowayos_hw::xhci`.
    pub(crate) stream: usize,
    pub(crate) port: u8,
    pub(crate) slot: u8,
    pub(crate) gate: Gate,
    /// A request of the gate's is in flight.
    asked: bool,
    /// A request waiting for the other gate's to complete: a webcam's
    /// microphone and camera share its control endpoint, one request at a
    /// time.
    pending: Option<Ask>,
}

impl Gating {
    pub(crate) const fn new(stream: usize, port: u8, slot: u8, gate: Gate) -> Self {
        Self {
            stream,
            port,
            slot,
            gate,
            asked: false,
            pending: None,
        }
    }
}

/// Which gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gated {
    Microphone,
    Camera,
}

impl Gated {
    const fn name(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Camera => "camera",
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Microphone => Self::Camera,
            Self::Camera => Self::Microphone,
        }
    }
}

/// When each kind of trouble was last reported, on the counter.
#[derive(Clone, Copy, Debug, Default)]
struct Warned {
    refused: Option<u64>,
    controller: Option<u64>,
    endpoint: Option<u64>,
    drops: Option<u64>,
    ports: Option<u64>,
}

/// Whether a line of a kind last reported at `last` is due - [`WARN_MS`]
/// since - and if so, notes it is reported now.
fn due(last: &mut Option<u64>) -> bool {
    let now = counter();
    let gap = counter_frequency().saturating_mul(WARN_MS) / 1000;
    let due = last.is_none_or(|at| now.wrapping_sub(at) >= gap);
    if due {
        *last = Some(now);
    }
    due
}

impl UsbHost {
    /// Marks every root port with a device attached to settle, then be
    /// enumerated.
    fn scan(&mut self) {
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let now = counter();
        for port in 1..=xhci.capabilities().max_ports {
            let index = usize::from(port).saturating_sub(1);
            if let (Ok(status), Some(state), Some(at)) = (
                xhci.port_status(port),
                self.ports.get_mut(index),
                self.changed_at.get_mut(index),
            ) && status.connected()
                && *state == PortUse::Empty
            {
                *state = PortUse::Settling;
                *at = now;
            }
        }
    }

    /// One tick: every event drained, the enumeration in flight moved on or
    /// timed out, the next one started, and the next tick asked for.
    fn tick(&mut self) {
        let began = counter();
        for _ in 0..EVENTS_PER_TICK {
            let Some(xhci) = self.xhci.as_mut() else {
                return;
            };
            match xhci.next_event() {
                Ok(Some(event)) => self.event(event),
                Ok(None) => break,
                Err(error) => {
                    self.refused = self.refused.saturating_add(1);
                    if due(&mut self.warned.refused) {
                        debug_println!(
                            "NOWAYOS: {}: an event refused ({} so far): {error}",
                            self.name,
                            self.refused
                        );
                    }
                }
            }
        }
        self.poll_ports();
        self.check_reset();
        self.settle_ports();
        if let Some(active) = self.active
            && counter() > active.deadline
        {
            self.report_stall(&active);
            // A command that does not complete holds up the command ring
            // behind it: it is aborted, and completes late, if at all, with
            // an id nothing waits for.
            if matches!(
                active.waiting,
                Waiting::Command(_) | Waiting::ControlReset { .. }
            ) && let Some(Err(error)) = self.xhci.as_mut().map(Xhci::abort_command)
            {
                debug_println!("NOWAYOS: {}: a command not aborted: {error}", self.name);
            }
            self.outcome(Outcome::TimedOut);
        }
        if self.active.is_none() {
            self.next_enumeration();
        }
        self.gate_tick(Gated::Microphone);
        self.gate_tick(Gated::Camera);
        self.repeat_keys();
        self.feed_streams();
        if let Some(client) = self.client.as_mut() {
            client.end_tick();
        }
        self.flush();
        self.report_drops();
        self.report_streams();
        self.report_camera(began);
        if let Some(Ok(Some(status))) = self.xhci.as_mut().map(Xhci::failed)
            && due(&mut self.warned.controller)
        {
            debug_println!(
                "NOWAYOS: {}: the controller reports an error (USBSTS {status:#x})",
                self.name
            );
        }
        if let Err(error) = TIMER.set_timeout(TICK, TICK_MS.saturating_mul(1_000_000)) {
            debug_println!("NOWAYOS: {}: could not set the timer: {error:?}", self.name);
        }
    }

    /// Reads every root port's status, and acts on each one that changed:
    /// the driver's only sight of ports, as a change left set stops the
    /// controller raising Port Status Change Events.
    fn poll_ports(&mut self) {
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let ports = xhci.capabilities().max_ports;
        let mut changed = [false; MAX_PORTS];
        for (port, flag) in (1..=ports).zip(changed.iter_mut()) {
            *flag = xhci
                .port_status(port)
                .is_ok_and(|status| status.changes() != 0);
        }
        for (port, flag) in (1..=ports).zip(changed) {
            if flag {
                self.port_changed(port);
            }
        }
    }

    /// Moves on each port whose connection has held for [`DEBOUNCE_MS`]: a
    /// device settled is queued to be enumerated, while it has attempts
    /// left, and a port empty that long has its count of attempts cleared.
    fn settle_ports(&mut self) {
        let now = counter();
        let debounce = counter_frequency().saturating_mul(DEBOUNCE_MS) / 1000;
        for index in 0..MAX_PORTS {
            let (Some(state), Some(at)) = (
                self.ports.get(index).copied(),
                self.changed_at.get(index).copied(),
            ) else {
                continue;
            };
            if now.wrapping_sub(at) < debounce {
                continue;
            }
            match state {
                PortUse::Empty => {
                    if let Some(attempts) = self.attempts.get_mut(index) {
                        *attempts = 0;
                    }
                }
                PortUse::Settling => {
                    if let Ok(port) = u8::try_from(index.saturating_add(1)) {
                        self.requeue(port);
                    }
                }
                _ => {}
            }
        }
    }

    /// Queues the device on `port` to be enumerated, if it has attempts
    /// left; otherwise leaves it until its port has been empty for
    /// [`DEBOUNCE_MS`].
    fn requeue(&mut self, port: u8) {
        let attempts = self
            .attempts
            .get(usize::from(port).saturating_sub(1))
            .copied()
            .unwrap_or(u8::MAX);
        if attempts < MAX_ATTEMPTS {
            self.set_port(port, PortUse::Waiting);
        } else {
            if due(&mut self.warned.ports) {
                debug_println!(
                    "NOWAYOS: {}: port {port}: left alone: enumerated {attempts} times since it was plugged in",
                    self.name
                );
            }
            self.set_port(port, PortUse::Unused);
        }
    }

    /// Whether the port being reset has finished: enabled, its reset over,
    /// and USB 2.0's reset recovery time past since that was first seen;
    /// then its speed goes to the enumeration.
    fn check_reset(&mut self) {
        let Some(mut active) = self.active else {
            return;
        };
        if active.waiting != Waiting::PortReset {
            return;
        }
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let Ok(status) = xhci.port_status(active.machine.port()) else {
            return;
        };
        if !status.connected() || !status.enabled() || status.resetting() {
            return;
        }
        let now = counter();
        let Some(ended) = active.reset_ended else {
            active.reset_ended = Some(now);
            self.active = Some(active);
            return;
        };
        if now.wrapping_sub(ended) < counter_frequency().saturating_mul(RECOVERY_MS) / 1000 {
            return;
        }
        match status.speed() {
            Some(speed) => self.outcome(Outcome::PortEnabled(speed)),
            None => self.outcome(Outcome::TimedOut),
        }
    }

    /// What the controller says when a step gets no answer in time: the
    /// port's status, the controller's, and the slot's state.
    fn report_stall(&mut self, active: &Active) {
        let name = self.name;
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let port = active.machine.port();
        let portsc = xhci.port_status(port).map_or(0, |status| status.0);
        let (command, status) = xhci.command_and_status().unwrap_or((0, 0));
        let slot = active.machine.slot();
        let state = if slot == 0 {
            None
        } else {
            xhci.slot_report(slot).ok().flatten()
        };
        debug_println!(
            "NOWAYOS: {name}: port {port}: waiting for {}: PORTSC {portsc:#x}, USBCMD {command:#x}, USBSTS {status:#x}, slot {slot} {state:?}",
            active.machine.waiting_for()
        );
    }

    /// Acts on one event the controller wrote.
    fn event(&mut self, event: XhciEvent) {
        match event {
            XhciEvent::Command {
                kind,
                code,
                slot,
                id,
            } => {
                let outcome = if !code.ok() {
                    Outcome::CommandFailed(code)
                } else if kind == kind::ENABLE_SLOT {
                    Outcome::SlotEnabled(slot)
                } else {
                    Outcome::CommandDone
                };
                match self.active.map(|active| active.waiting) {
                    Some(
                        Waiting::Command(wanted)
                        | Waiting::ControlReset {
                            dequeue: wanted, ..
                        },
                    ) if wanted == id => {
                        self.outcome(outcome);
                    }
                    // Reset Endpoint goes before Set TR Dequeue Pointer:
                    // only its failure ends the step.
                    Some(Waiting::ControlReset { reset, .. }) if reset == id && !code.ok() => {
                        self.outcome(outcome);
                    }
                    // Nothing waits for it: a command given up on, completed
                    // late. A slot it enabled goes back.
                    _ => {
                        if kind == kind::ENABLE_SLOT && code.ok() && slot != 0 {
                            self.give_back(slot);
                        }
                    }
                }
            }
            XhciEvent::Port { port } => self.port_changed(port),
            XhciEvent::Control { slot, code, len } => self.control(slot, code, len),
            XhciEvent::Report {
                slot,
                endpoint,
                bytes,
                len,
            } => self.report(slot, endpoint, &bytes, len),
            XhciEvent::ReportFailed {
                slot,
                endpoint,
                code,
            } => self.report_failed(slot, endpoint, code),
            // Counted by the driver as they come; the PD reports the counts,
            // and takes what a microphone or a feedback endpoint sent, and a
            // camera's payloads go to its frames.
            XhciEvent::Stream {
                stream,
                code,
                len,
                buffer,
            } => {
                // Each takes only its own streams.
                self.stream_done(stream, code, len, buffer);
                self.camera_event(stream, Some((buffer, len, code)));
            }
            XhciEvent::StreamEmpty { stream } => {
                if let Some(client) = self.client.as_mut()
                    && client.is_microphone(stream)
                {
                    client.lost();
                }
                self.camera_event(stream, None);
            }
            XhciEvent::HostController { code } => {
                if due(&mut self.warned.controller) {
                    debug_println!("NOWAYOS: {}: the controller reports {code}", self.name);
                }
            }
            XhciEvent::Ignored => {}
        }
    }

    /// A control transfer on `slot` ended.
    fn control(&mut self, slot: u8, code: Completion, len: usize) {
        // Not the enumeration's: a capture gate's.
        let enumerating = self.active.is_some_and(|active| {
            active.machine.slot() == slot && matches!(active.waiting, Waiting::Control { .. })
        });
        if !enumerating {
            for which in [Gated::Microphone, Gated::Camera] {
                if self
                    .gating(which)
                    .is_some_and(|gating| gating.slot == slot && gating.asked)
                {
                    self.gate_done(which, code.ok());
                    break;
                }
            }
            return;
        }
        let Some(active) = self.active else {
            return;
        };
        let Waiting::Control { data } = active.waiting else {
            return;
        };
        if active.machine.slot() != slot {
            return;
        }
        if !code.ok() {
            self.outcome(Outcome::ControlFailed(code));
        } else if data {
            // Copied out of DMA memory here; the machine checks the copy.
            // As much as the large buffer holds: a configuration read by a
            // PD that streams can be that long.
            let mut bytes = [0u8; CONTROL_BUFFER_LARGE];
            let copied = self
                .xhci
                .as_ref()
                .and_then(|xhci| xhci.control_data(slot, &mut bytes).ok())
                .unwrap_or(0)
                .min(len);
            self.outcome(Outcome::ControlData(
                bytes.get(..copied).unwrap_or_default(),
            ));
        } else {
            self.outcome(Outcome::ControlDone);
        }
    }

    /// `slot`'s interrupt endpoint `endpoint` stopped - a stall, a
    /// transaction error: every key and button its device held is released,
    /// its slot given back, and it is enumerated again while it has
    /// attempts left ([`MAX_ATTEMPTS`]). On a USB 3 port, which enables
    /// itself and is not reset before enumeration, the device - still
    /// configured - could not be addressed again: it is left alone until it
    /// is unplugged.
    fn report_failed(&mut self, slot: u8, endpoint: usize, code: Completion) {
        let name = self.name;
        let Some(port) = self
            .attached
            .get(usize::from(slot).saturating_sub(1))
            .copied()
            .flatten()
            .map(|attached| attached.port)
        else {
            return;
        };
        if due(&mut self.warned.endpoint) {
            debug_println!(
                "NOWAYOS: {name}: port {port}: slot {slot}'s endpoint {endpoint} stopped: {code}"
            );
        }
        self.detach(slot);
        if self.xhci.as_ref().and_then(|xhci| xhci.major_of(port)) == Some(3) {
            if due(&mut self.warned.ports) {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: left alone: its reports stopped, on a USB 3 port"
                );
            }
            self.set_port(port, PortUse::Unused);
        } else {
            self.requeue(port);
        }
    }

    /// A report on `slot`'s interrupt endpoint `endpoint`, already copied
    /// out of DMA memory: decoded, and its events queued for the broker.
    fn report(&mut self, slot: u8, endpoint: usize, bytes: &[u8], len: usize) {
        let name = self.name;
        let Some(Some(attached)) = self.attached.get_mut(usize::from(slot).saturating_sub(1))
        else {
            return;
        };
        let (Some(count), Some(Some(decoder))) = (
            attached.reports.get_mut(endpoint),
            attached.decoders.get_mut(endpoint),
        ) else {
            return;
        };
        *count = count.saturating_add(1);
        let report = bytes.get(..len).unwrap_or_default();
        // Its length only: a report's contents are what a person entered -
        // a key, a touch's place - and never go into a log (Security
        // Checklist, Kiosk).
        if *count == 1 {
            debug_println!(
                "NOWAYOS: {name}: port {}: the {}'s first report: {len} bytes",
                attached.port,
                decoder.name()
            );
        }
        let outbox = &mut self.outbox;
        decoder.report(report, micros(), |event| outbox.push(event));
    }

    /// Each tick: every keyboard's held key pressed again when its
    /// repeat is due.
    fn repeat_keys(&mut self) {
        let now = micros();
        let outbox = &mut self.outbox;
        for attached in self.attached.iter_mut().flatten() {
            for decoder in attached.decoders.iter_mut().flatten() {
                if let Decoder::Keyboard(_, repeat) = decoder {
                    repeat.due(now, |event| outbox.push(event));
                }
            }
        }
    }

    /// Sends what waits in the outbox through the ring, notifying the
    /// broker after each fill: it runs above this PD, so each notification
    /// finds the ring drained.
    fn flush(&mut self) {
        let Some(ring) = self.ring.as_ref() else {
            return;
        };
        for _ in 0..FLUSHES {
            let Some(_) = self.outbox.front() else {
                return;
            };
            let Ok(mut tx) = Producer::new(ring) else {
                return;
            };
            let mut sent = false;
            while let Some(event) = self.outbox.front() {
                if tx.try_send::<InputEvent>(&event).is_err() {
                    break;
                }
                self.outbox.pop();
                sent = true;
            }
            if !sent {
                return;
            }
            BROKER.notify();
        }
    }

    /// Reports the drop counts, when they changed.
    fn report_drops(&mut self) {
        let mut dropped = Dropped::default();
        for decoder in self
            .attached
            .iter()
            .flatten()
            .flat_map(|attached| attached.decoders.iter().flatten())
        {
            let counts = decoder.dropped();
            dropped.wrong_length = dropped.wrong_length.saturating_add(counts.wrong_length);
            dropped.rollover = dropped.rollover.saturating_add(counts.rollover);
            dropped.undefined_usage = dropped
                .undefined_usage
                .saturating_add(counts.undefined_usage);
        }
        let now = (dropped, self.outbox.dropped, self.refused);
        if now != self.reported && due(&mut self.warned.drops) {
            self.reported = now;
            debug_println!(
                "NOWAYOS: {}: dropped so far: {} reports of a wrong length, {} rollovers, {} undefined keys, {} events with no room",
                self.name,
                dropped.wrong_length,
                dropped.rollover,
                dropped.undefined_usage,
                self.outbox.dropped
            );
        }
    }

    /// Port `port`'s status changed. A change of its connection - a device
    /// gone, or gone and another come between two looks - ends whatever
    /// the port held: an enumeration in flight stops, its command aborted
    /// and its slot given back, and a device's keys and buttons are
    /// released; a device connected settles for [`DEBOUNCE_MS`] before it
    /// is enumerated. The end of a reset is [`Self::check_reset`]'s.
    fn port_changed(&mut self, port: u8) {
        let name = self.name;
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let Ok(status) = xhci.port_status(port) else {
            return;
        };
        if xhci.port_write(port, PortWrite::ClearChanges).is_err() {
            debug_println!("NOWAYOS: {name}: port {port}: could not clear its changes");
        }
        let index = usize::from(port).saturating_sub(1);
        let state = self.ports.get(index).copied().unwrap_or_default();
        let settle = if !status.connected() || status.connect_changed() {
            if let Some(what) = self.end_port(port, state)
                && due(&mut self.warned.ports)
            {
                debug_println!("NOWAYOS: {name}: port {port}: {what}");
            }
            true
        } else {
            // Connected, with no change of connection seen: as at start-up.
            state == PortUse::Empty
        };
        if settle {
            if let Some(at) = self.changed_at.get_mut(index) {
                *at = counter();
            }
            self.set_port(
                port,
                if status.connected() {
                    PortUse::Settling
                } else {
                    PortUse::Empty
                },
            );
        }
    }

    /// Ends whatever `port`, in `state`, held when its connection changed;
    /// says what to report, if anything was there.
    fn end_port(&mut self, port: u8, state: PortUse) -> Option<&'static str> {
        if let Some(active) = self.active
            && active.machine.port() == port
        {
            if matches!(
                active.waiting,
                Waiting::Command(_) | Waiting::ControlReset { .. }
            ) && let Some(Err(error)) = self.xhci.as_mut().map(Xhci::abort_command)
            {
                debug_println!("NOWAYOS: {}: a command not aborted: {error}", self.name);
            }
            self.active = None;
            self.give_back(active.machine.slot());
            return Some("unplugged while being enumerated");
        }
        match state {
            PortUse::Polled(slot) => {
                self.detach(slot);
                Some("unplugged")
            }
            PortUse::Unused => Some("unplugged"),
            _ => None,
        }
    }

    fn set_port(&mut self, port: u8, state: PortUse) {
        if let Some(entry) = self.ports.get_mut(usize::from(port).saturating_sub(1)) {
            *entry = state;
        }
    }

    /// Forgets the device in `slot` - every key and button it held
    /// released, its streams stopped - and gives its slot back.
    fn detach(&mut self, slot: u8) {
        for (id, entry) in self.playing.iter_mut().enumerate() {
            if entry.is_some_and(|playing| playing.slot == slot) {
                *entry = None;
                if let Some(client) = self.client.as_mut() {
                    client.release(id);
                }
            }
        }
        if self.camera.is_some_and(|camera| camera.gating.slot == slot) {
            self.camera = None;
            if let Some(frames) = self.frames.as_ref() {
                frames.withdraw();
            }
        }
        if self.capture.is_some_and(|mic| mic.slot == slot) {
            self.capture = None;
        }
        if let Some(entry) = self.attached.get_mut(usize::from(slot).saturating_sub(1))
            && let Some(mut attached) = entry.take()
        {
            let outbox = &mut self.outbox;
            for decoder in attached.decoders.iter_mut().flatten() {
                decoder.release_all(|event| outbox.push(event));
            }
        }
        self.give_back(slot);
    }

    /// Gives `slot` back to the controller.
    fn give_back(&mut self, slot: u8) {
        if slot != 0
            && let Some(xhci) = self.xhci.as_mut()
            && let Err(error) = xhci.disable_slot(slot)
        {
            debug_println!(
                "NOWAYOS: {}: slot {slot}: could not disable it: {error}",
                self.name
            );
        }
    }

    /// Starts enumerating the lowest port waiting, if any.
    fn next_enumeration(&mut self) {
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let Some(index) = self
            .ports
            .iter()
            .position(|state| *state == PortUse::Waiting)
        else {
            return;
        };
        let Ok(port) = u8::try_from(index.saturating_add(1)) else {
            return;
        };
        let Ok(status) = xhci.port_status(port) else {
            return;
        };
        if !status.connected() {
            self.set_port(port, PortUse::Empty);
            return;
        }
        // A USB 3 port trains and enables itself; a USB 2.0 port is reset.
        let usb3 = (xhci.major_of(port) == Some(3) && status.enabled())
            .then(|| status.speed())
            .flatten();
        let (mut machine, step) = if self.streaming && xhci.streams_available() > 0 {
            if self.frames.is_some() {
                Enumeration::start_with_camera(port, usb3)
            } else {
                Enumeration::start_streaming(port, usb3)
            }
        } else {
            Enumeration::start(port, usb3)
        };
        machine.want_rate(self.wanted_rate);
        self.set_port(port, PortUse::Enumerating);
        if let Some(attempts) = self.attempts.get_mut(index) {
            *attempts = attempts.saturating_add(1);
        }
        self.active = Some(Active {
            machine,
            waiting: Waiting::PortReset,
            deadline: 0,
            reported: false,
            reset_ended: None,
        });
        self.execute(step);
    }

    /// Feeds `outcome` to the enumeration in flight, and does what it says
    /// next.
    fn outcome(&mut self, outcome: Outcome<'_>) {
        let Some(mut active) = self.active else {
            return;
        };
        let step = active.machine.on(outcome);
        if !active.reported
            && let Some(device) = active.machine.device()
        {
            active.reported = true;
            debug_println!(
                "NOWAYOS: {}: port {}: device {:04x}:{:04x}, class {}/{}/{}, USB {}.{}, {}",
                self.name,
                active.machine.port(),
                device.vendor,
                device.product,
                device.class,
                device.subclass,
                device.protocol,
                device.usb >> 8,
                (device.usb >> 4) & 0xF,
                active
                    .machine
                    .speed()
                    .map_or("speed unknown", |speed| speed.name())
            );
        }
        self.active = Some(active);
        self.execute(step);
    }

    /// Does `step` for the enumeration in flight, and notes what it waits
    /// for.
    fn execute(&mut self, step: Step) {
        let name = self.name;
        let Some(mut active) = self.active else {
            return;
        };
        let machine = active.machine;
        let port = machine.port();
        let slot = machine.slot();
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let speed = machine.speed().ok_or(HwError::Xhci {
            reason: "no speed for the device",
        });
        let result = match step {
            // Its change bits cleared first, so none left from before can be
            // read as this reset's.
            Step::ResetPort => xhci
                .port_write(port, PortWrite::ClearChanges)
                .and_then(|()| xhci.port_write(port, PortWrite::Reset))
                .map(|()| Waiting::PortReset),
            Step::EnableSlot => xhci.enable_slot().map(Waiting::Command),
            Step::AddressDevice { max_packet } => speed
                .and_then(|speed| xhci.address_device(slot, port, speed, max_packet))
                .map(Waiting::Command),
            Step::EvaluateControl { max_packet } => xhci
                .evaluate_control(slot, max_packet)
                .map(Waiting::Command),
            Step::Control(setup) => xhci.control(slot, setup).map(|()| Waiting::Control {
                data: setup.length > 0,
            }),
            Step::ControlOut(setup, data) => xhci
                .control_out(slot, setup, data.bytes())
                .map(|()| Waiting::Control { data: false }),
            Step::ResetControl => xhci
                .reset_control(slot)
                .map(|(reset, dequeue)| Waiting::ControlReset { reset, dequeue }),
            Step::ConfigureEndpoints => {
                let mut list = [BootInterface {
                    interface: 0,
                    device: BootDevice::Keyboard,
                    endpoint: 0x81,
                    max_packet: 8,
                    interval: 10,
                    report_len: 0,
                }; MAX_BOOT_INTERFACES];
                let mut count = 0usize;
                for (to, from) in list.iter_mut().zip(machine.boot().iter()) {
                    *to = *from;
                    count = count.saturating_add(1);
                }
                // As many of its streams - those not refused, each with
                // its feedback endpoint - as the driver has room
                // for, then a camera's video endpoint if room is left;
                // a stream left out is never started.
                let mut streams = [EMPTY_STREAM; MAX_STREAMS];
                let mut taken = 0usize;
                let room = xhci.streams_available();
                for stream in machine.usable() {
                    let feedback = stream
                        .feedback
                        .zip(machine.speed())
                        .map(|(feedback, speed)| {
                            feedback.as_stream(stream.interface, stream.alternate, speed)
                        });
                    let needed = 1usize.saturating_add(usize::from(feedback.is_some()));
                    if taken.saturating_add(needed) > room {
                        break;
                    }
                    for endpoint in core::iter::once(*stream).chain(feedback) {
                        if let Some(to) = streams.get_mut(taken) {
                            *to = endpoint;
                            taken = taken.saturating_add(1);
                        }
                    }
                }
                if let Some(video) = machine.video().endpoint()
                    && taken < room
                    && let Some(to) = streams.get_mut(taken)
                {
                    *to = video;
                    taken = taken.saturating_add(1);
                }
                speed
                    .and_then(|speed| {
                        xhci.configure_endpoints(
                            slot,
                            port,
                            speed,
                            machine.configuration(),
                            list.get(..count).unwrap_or_default(),
                            streams.get(..taken).unwrap_or_default(),
                        )
                    })
                    .map(Waiting::Command)
            }
            Step::Poll => {
                self.active = None;
                match xhci.start_reports(slot) {
                    Ok(()) => self.attach(&machine),
                    Err(error) => {
                        debug_println!("NOWAYOS: {name}: port {port}: its reports: {error}");
                        self.give_up(port, slot);
                    }
                }
                return;
            }
            Step::Stop(why) => {
                self.active = None;
                self.report_refused(&machine);
                if let Video::Refused(refused) = machine.video() {
                    debug_println!("NOWAYOS: {name}: port {port}: camera: not streamed: {refused}");
                }
                debug_println!("NOWAYOS: {name}: port {port}: left alone: {why}");
                self.give_up(port, slot);
                return;
            }
        };
        match result {
            Ok(waiting) => {
                active.waiting = waiting;
                active.deadline =
                    counter().saturating_add(counter_frequency().saturating_mul(STEP_MS) / 1000);
                self.active = Some(active);
            }
            Err(error) => {
                debug_println!("NOWAYOS: {name}: port {port}: {error}");
                self.active = None;
                self.give_up(port, slot);
            }
        }
    }

    /// The device `machine` enumerated is ready: its reports flow.
    fn attach(&mut self, machine: &Enumeration) {
        let (port, slot) = (machine.port(), machine.slot());
        let mut decoders = [None; MAX_BOOT_INTERFACES];
        for (index, (to, from)) in decoders.iter_mut().zip(machine.boot().iter()).enumerate() {
            *to = match from.device {
                BootDevice::Keyboard => Some(Decoder::Keyboard(Keyboard::new(), KeyRepeat::new())),
                BootDevice::Mouse => self.limits.map(|limits| Decoder::Mouse(Mouse::new(limits))),
                BootDevice::Absolute => {
                    machine
                        .layout(index)
                        .zip(self.limits)
                        .map(|(layout, limits)| {
                            Decoder::Absolute(AbsolutePointer::new(layout, limits))
                        })
                }
            };
        }
        if let Some(entry) = self.attached.get_mut(usize::from(slot).saturating_sub(1)) {
            *entry = Some(Attached {
                port,
                decoders,
                reports: [0; MAX_BOOT_INTERFACES],
            });
        }
        self.set_port(port, PortUse::Polled(slot));
        self.start_streams(machine);
        self.start_camera(machine);
        for decoder in decoders.iter().flatten() {
            match decoder {
                Decoder::Absolute(pointer) => {
                    let layout = pointer.layout();
                    debug_println!(
                        "NOWAYOS: {}: port {port}: {} ready: report ID {}; X at {}; Y at {}",
                        self.name,
                        decoder.name(),
                        layout.report_id,
                        Place(layout.x),
                        Place(layout.y)
                    );
                    if let Some(tip) = layout.tip {
                        debug_println!(
                            "NOWAYOS: {}: port {port}: its tip switch at {}",
                            self.name,
                            Place(tip)
                        );
                    }
                }
                _ => debug_println!(
                    "NOWAYOS: {}: port {port}: boot {} ready",
                    self.name,
                    decoder.name()
                ),
            }
        }
    }

    /// Starts each audio stream `machine` selected and the driver set up:
    /// its source chosen, [`STREAM_AHEAD_US`] of transfers queued, its
    /// doorbell rung - and an asynchronous speaker's feedback
    /// endpoint with it. Says why of each stream it refused.
    fn start_streams(&mut self, machine: &Enumeration) {
        let name = self.name;
        let (port, slot) = (machine.port(), machine.slot());
        self.report_refused(machine);
        let Some(speed) = machine.speed() else {
            return;
        };
        for stream in machine.selected() {
            let direction = if stream.inbound() { "in" } else { "out" };
            let Some(xhci) = self.xhci.as_ref() else {
                return;
            };
            let Some((id, (_, capacity), td_bytes)) = xhci
                .stream_of(slot, stream.endpoint)
                .and_then(|id| Some((id, xhci.stream_queued(id)?, xhci.stream_td_bytes(id)?)))
            else {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {:#04x} {direction}: no room to stream it",
                    stream.endpoint
                );
                continue;
            };
            let interval_us = interval_micros(isoch_interval(speed, stream.interval));
            let depth = usize::try_from(STREAM_AHEAD_US.checked_div(interval_us).unwrap_or(1))
                .unwrap_or(1)
                .clamp(2, capacity.max(2));
            let source = match self.source(stream, id, interval_us) {
                Ok(source) => source,
                Err(why) => {
                    debug_println!(
                        "NOWAYOS: {name}: port {port}: endpoint {:#04x} {direction}: not streamed: {why}",
                        stream.endpoint
                    );
                    continue;
                }
            };
            let (channels, bits) = stream
                .format
                .map_or((0, 0), |format| (format.channels, format.bits));
            let what = match source {
                Source::Tone { .. } => "a 440 Hz tone",
                Source::Counted => "counted, never kept",
                Source::Speaker { .. } => "the client's speaker",
                Source::Microphone { .. } => "the client's microphone",
                Source::Feedback { .. } => "",
            };
            if matches!(source, Source::Counted) {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {:#04x} {direction}: interface {}, alternate {}: up to {td_bytes} bytes every {interval_us} us, {depth} transfers ahead: {what}",
                    stream.endpoint,
                    stream.interface,
                    stream.alternate
                );
            } else {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {:#04x} {direction}: interface {}, alternate {}: {} Hz, {channels} channels of {bits} bits, up to {td_bytes} bytes every {interval_us} us, {depth} transfers ahead: {what}",
                    stream.endpoint,
                    stream.interface,
                    stream.alternate,
                    stream.rate
                );
            }
            if stream.sets_rate() {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {:#04x} {direction}: its rate set to {} Hz by request",
                    stream.endpoint,
                    stream.rate
                );
            }
            self.play(id, port, slot, stream.endpoint, depth, source);
            // A client's microphone captures only while its client asks:
            // first, back to alternate setting 0.
            if matches!(source, Source::Microphone { .. }) {
                let (gate, ask) = Gate::microphone(*stream);
                self.capture = Some(Gating::new(id, port, slot, gate));
                self.gate_ask(Gated::Microphone, ask);
            }
            if let Some(feedback) = stream.feedback {
                self.start_feedback(machine, stream, &feedback, id, interval_us);
            }
        }
        self.feed_streams();
    }

    /// Records stream `id` as fed from `source`, `depth` TDs ahead.
    fn play(&mut self, id: usize, port: u8, slot: u8, endpoint: u8, depth: usize, source: Source) {
        if let Some(entry) = self.playing.get_mut(id) {
            *entry = Some(Playing {
                port,
                slot,
                endpoint,
                depth,
                source,
                started: counter(),
                reports: 0,
            });
        }
    }

    /// Starts the feedback endpoint of `stream`, streamed as `paces`, whose
    /// packets come every `interval_us`: what it reads sets the speaker's
    /// pace.
    fn start_feedback(
        &mut self,
        machine: &Enumeration,
        stream: &Stream,
        feedback: &audio::Feedback,
        paces: usize,
        interval_us: u32,
    ) {
        let (name, port, slot) = (self.name, machine.port(), machine.slot());
        let Some(speed) = machine.speed() else {
            return;
        };
        let Some(xhci) = self.xhci.as_ref() else {
            return;
        };
        let Some((id, capacity)) = xhci
            .stream_of(slot, feedback.endpoint)
            .and_then(|id| Some((id, xhci.stream_queued(id)?.1)))
        else {
            debug_println!(
                "NOWAYOS: {name}: port {port}: endpoint {:#04x} in: no room for its feedback: the speaker runs at its nominal rate",
                feedback.endpoint
            );
            return;
        };
        let every = feedback.as_stream(stream.interface, stream.alternate, speed);
        let every_us = interval_micros(isoch_interval(speed, every.interval));
        let depth = usize::try_from(STREAM_AHEAD_US.checked_div(every_us).unwrap_or(1))
            .unwrap_or(1)
            .clamp(2, capacity.max(2));
        debug_println!(
            "NOWAYOS: {name}: port {port}: endpoint {:#04x} in: feedback for endpoint {:#04x}, every {every_us} us",
            feedback.endpoint,
            stream.endpoint
        );
        self.play(
            id,
            port,
            slot,
            feedback.endpoint,
            depth,
            Source::Feedback {
                paces,
                speed,
                interval_us,
                nominal: audio::nominal_q16(stream.rate, interval_us),
            },
        );
    }

    /// Says why of each stream `machine` found and did not select: refused,
    /// or its setting or rate refused by the device.
    fn report_refused(&self, machine: &Enumeration) {
        for stream in machine.streams().iter() {
            let direction = if stream.inbound() { "in" } else { "out" };
            if let Some(why) = stream.refusal {
                debug_println!(
                    "NOWAYOS: {}: port {}: endpoint {:#04x} {direction}: not streamed: {why}",
                    self.name,
                    machine.port(),
                    stream.endpoint
                );
            } else if machine.polling()
                && !machine
                    .selected()
                    .any(|selected| selected.endpoint == stream.endpoint)
            {
                debug_println!(
                    "NOWAYOS: {}: port {}: endpoint {:#04x} {direction}: not streamed: the device refused its alternate setting or its rate",
                    self.name,
                    machine.port(),
                    stream.endpoint
                );
            }
        }
    }

    /// What `stream`, streamed as `id`, carries. For a test instrument
    /// ([`init_test_tone`]): a microphone's packets are counted, a speaker
    /// plays the tone. Otherwise, with a client: a speaker plays what the
    /// client sends, a microphone's sound goes to it - one of each, the
    /// first found. Or why not.
    fn source(
        &mut self,
        stream: &Stream,
        id: usize,
        interval_us: u32,
    ) -> Result<Source, &'static str> {
        let format = stream.format.filter(|_| stream.refusal.is_none());
        if self.test_tone {
            if stream.inbound() {
                return Ok(Source::Counted);
            }
            let format = format.ok_or("no PCM format this driver reads")?;
            if !(2..=4).contains(&format.subframe) {
                return Err("8-bit samples");
            }
            let tone = Tone::new(TONE_HZ, stream.rate, TONE_AMPLITUDE)
                .ok_or("a rate too low for the tone")?;
            return Ok(Source::Tone {
                tone,
                clock: FrameClock::new(stream.rate, interval_us),
                format,
            });
        }
        let format = format.ok_or("no PCM format this driver reads")?;
        let client = self.client.as_mut().ok_or("no client to stream it to")?;
        let pcm = nowayos_audio::Format::new(stream.rate, format.channels)
            .ok_or("a format no client record carries")?;
        let clock = FrameClock::new(stream.rate, interval_us);
        if stream.inbound() {
            if !client.take_microphone(id, pcm) {
                return Err("the client has a microphone already");
            }
            Ok(Source::Microphone {
                clock,
                format,
                max_packets_only: stream.max_packets_only,
            })
        } else {
            if !client.take_speaker(id, pcm) {
                return Err("the client has a speaker already");
            }
            Ok(Source::Speaker {
                clock,
                format,
                max_packets_only: stream.max_packets_only,
            })
        }
    }

    /// Tops each stream up to its depth, and rings its doorbell: the
    /// controller takes a transfer each interval, so between two ticks
    /// it never runs out.
    fn feed_streams(&mut self) {
        let name = self.name;
        let Self {
            xhci,
            playing,
            client,
            warned,
            capture,
            camera,
            ..
        } = self;
        let capture = *capture;
        let camera = camera.map(|camera| camera.gating);
        let Some(xhci) = xhci.as_mut() else {
            return;
        };
        let mut packet = [0u8; ISOCH_MAX as usize];
        for (id, entry) in playing.iter_mut().enumerate() {
            let Some(playing) = entry.as_mut() else {
                continue;
            };
            // A client's microphone only while its gate lets it.
            if matches!(playing.source, Source::Microphone { .. })
                && !capture.is_some_and(|mic| mic.stream == id && mic.gate.feeding())
            {
                continue;
            }
            // The camera's only while its gate lets it.
            if camera.is_some_and(|camera| camera.stream == id && !camera.gate.feeding()) {
                continue;
            }
            let Some((queued, _)) = xhci.stream_queued(id) else {
                continue;
            };
            let td_bytes = xhci.stream_td_bytes(id).unwrap_or(0);
            let mut added = false;
            for _ in queued..playing.depth {
                let room = packet
                    .get_mut(..td_bytes.min(ISOCH_MAX as usize))
                    .unwrap_or_default();
                let len = match &mut playing.source {
                    Source::Tone {
                        tone,
                        clock,
                        format,
                    } => {
                        let frame = format.frame_bytes().max(1);
                        // At most a packet's worth: a device's wMaxPacketSize
                        // holds a packet's frames, rounded up.
                        let frames = clock
                            .next_packet()
                            .min(td_bytes.checked_div(frame).unwrap_or(0));
                        tone.fill(room, frames, format)
                    }
                    Source::Speaker {
                        clock,
                        format,
                        max_packets_only,
                    } => speaker_packet(
                        room,
                        clock.next_packet(),
                        format,
                        *max_packets_only,
                        client.as_mut(),
                    ),
                    Source::Counted | Source::Microphone { .. } | Source::Feedback { .. } => 0,
                };
                if let Err(error) = xhci.queue_isoch(id, packet.get(..len).unwrap_or_default()) {
                    if due(&mut warned.endpoint) {
                        debug_println!(
                            "NOWAYOS: {name}: port {}: endpoint {:#04x}: {error}",
                            playing.port,
                            playing.endpoint
                        );
                    }
                    break;
                }
                added = true;
            }
            if added && let Err(error) = xhci.ring_stream(id) {
                debug_println!("NOWAYOS: {name}: port {}: {error}", playing.port);
            }
        }
    }

    /// A gate, as it stands.
    fn gating(&self, which: Gated) -> Option<Gating> {
        match which {
            Gated::Microphone => self.capture,
            Gated::Camera => self.camera.map(|camera| camera.gating),
        }
    }

    /// A gate, changed. A camera that starts streaming awaits a new frame:
    /// any it was writing when it stopped is given up.
    fn set_gating(&mut self, which: Gated, gating: Gating) {
        match which {
            Gated::Microphone => {
                if let Some(mic) = self.capture.as_mut() {
                    *mic = gating;
                }
            }
            Gated::Camera => {
                if let Some(camera) = self.camera.as_mut() {
                    if !camera.gating.gate.feeding() && gating.gate.feeding() {
                        camera.assembler.restart();
                    }
                    camera.gating = gating;
                }
            }
        }
    }

    /// Each tick: what a gate's client last asked given to it - the audio
    /// client's capture record, the camera's client's
    /// frames-wanted word - and a stream drained of its last
    /// transfer put back at alternate setting 0.
    fn gate_tick(&mut self, which: Gated) {
        let Some(mut gating) = self.gating(which) else {
            return;
        };
        let want = match which {
            Gated::Microphone => self.client.as_ref().is_some_and(client::Client::capture),
            Gated::Camera => self.frames_wanted(),
        };
        let before = gating.gate.state();
        let mut ask = None;
        if gating.gate.wanted() != want {
            ask = gating.gate.want(want);
        }
        if ask.is_none() && gating.gate.state() == capture::State::Draining {
            let queued = self
                .xhci
                .as_ref()
                .and_then(|xhci| xhci.stream_queued(gating.stream))
                .map_or(0, |(queued, _)| queued);
            if queued == 0 {
                ask = gating.gate.drained();
            }
        }
        self.set_gating(which, gating);
        self.gate_said(which, before);
        if let Some(ask) = ask {
            self.gate_ask(which, ask);
        }
    }

    /// A gate's request made on its device's control endpoint - or kept
    /// until the other gate's request there completes; one that cannot be
    /// made fails the gate.
    fn gate_ask(&mut self, which: Gated, ask: Ask) {
        let Some(mut gating) = self.gating(which) else {
            return;
        };
        if self
            .gating(which.other())
            .is_some_and(|other| other.slot == gating.slot && other.asked)
        {
            gating.pending = Some(ask);
            self.set_gating(which, gating);
            return;
        }
        let Some(xhci) = self.xhci.as_mut() else {
            return;
        };
        let made = match ask {
            Ask::Control(setup) => xhci.control(gating.slot, setup),
            Ask::ControlOut(setup, data) => xhci.control_out(gating.slot, setup, data.bytes()),
        };
        match made {
            Ok(()) => {
                gating.asked = true;
                self.set_gating(which, gating);
            }
            Err(error) => {
                debug_println!(
                    "NOWAYOS: {}: port {}: {}: a request not made: {error}",
                    self.name,
                    gating.port,
                    which.name()
                );
                self.gate_done(which, false);
            }
        }
    }

    /// A gate's request completed, `ok` or not: a request of the other
    /// gate's waiting on the same control endpoint made first, then this
    /// gate's next, if any.
    fn gate_done(&mut self, which: Gated, ok: bool) {
        let Some(mut gating) = self.gating(which) else {
            return;
        };
        gating.asked = false;
        let before = gating.gate.state();
        let next = gating.gate.done(ok);
        self.set_gating(which, gating);
        self.gate_said(which, before);
        let other = which.other();
        if let Some(mut waiting) = self.gating(other)
            && waiting.slot == gating.slot
            && let Some(ask) = waiting.pending.take()
        {
            self.set_gating(other, waiting);
            self.gate_ask(other, ask);
        }
        if let Some(ask) = next {
            self.gate_ask(which, ask);
        }
    }

    /// Says where a gate has gone, from `before`: streaming, stopped or
    /// failed - never what it takes in.
    fn gate_said(&self, which: Gated, before: capture::State) {
        let Some(gating) = self.gating(which) else {
            return;
        };
        let (name, port, now) = (self.name, gating.port, gating.gate.state());
        if now == before {
            return;
        }
        let (on, off, failed) = match which {
            Gated::Microphone => ("capturing", "not capturing", "not captured"),
            Gated::Camera => ("streaming", "not streaming", "not streamed"),
        };
        let what = which.name();
        match now {
            capture::State::On => {
                debug_println!("NOWAYOS: {name}: port {port}: {what}: {on}, as its client asked");
            }
            capture::State::Off => {
                debug_println!("NOWAYOS: {name}: port {port}: {what}: {off}: alternate setting 0");
            }
            capture::State::Failed => debug_println!(
                "NOWAYOS: {name}: port {port}: {what}: {failed}: a request to it failed"
            ),
            _ => {}
        }
    }

    /// One of stream `stream`'s TDs completed: a microphone's
    /// sound goes to the client, a feedback value paces its speaker, and a
    /// TD missed is sound lost.
    fn stream_done(&mut self, stream: usize, code: Completion, len: usize, buffer: usize) {
        let Self {
            xhci,
            playing,
            client,
            ..
        } = self;
        let (Some(xhci), Some(source)) = (
            xhci.as_ref(),
            playing
                .get(stream)
                .copied()
                .flatten()
                .map(|playing| playing.source),
        ) else {
            return;
        };
        // Only what came in is copied out of DMA memory.
        let mut bytes = [0u8; ISOCH_MAX as usize];
        let inbound = matches!(source, Source::Microphone { .. } | Source::Feedback { .. });
        let copied = if code.ok() && inbound {
            xhci.stream_data(stream, buffer, len, &mut bytes)
                .unwrap_or(0)
        } else {
            0
        };
        let data = bytes.get(..copied).unwrap_or_default();
        match source {
            Source::Microphone {
                mut clock,
                format,
                max_packets_only,
            } => {
                let Some(client) = client.as_mut() else {
                    return;
                };
                if !code.ok() {
                    client.lost();
                    return;
                }
                // A device that sends only whole packets pads them: only
                // the frames its rate gives are sound.
                let frames = if max_packets_only {
                    clock.next_packet()
                } else {
                    usize::MAX
                };
                let mut samples = [0i16; audio::MAX_CHANNELS as usize];
                for frame in data.chunks_exact(format.frame_bytes().max(1)).take(frames) {
                    let channels = format.read_frame(frame, &mut samples);
                    client.captured(samples.get(..channels).unwrap_or_default());
                }
                if let Some(Some(entry)) = playing.get_mut(stream) {
                    entry.source = Source::Microphone {
                        clock,
                        format,
                        max_packets_only,
                    };
                }
            }
            Source::Feedback {
                paces,
                speed,
                interval_us,
                nominal,
            } => {
                if let Some(q16) = audio::feedback_q16(data, speed, interval_us, nominal)
                    && let Some(Some(entry)) = playing.get_mut(paces)
                    && let Source::Speaker { clock, .. } | Source::Tone { clock, .. } =
                        &mut entry.source
                {
                    clock.pace(q16);
                }
            }
            Source::Tone { .. } | Source::Counted | Source::Speaker { .. } => {}
        }
    }

    /// Every [`STREAM_REPORT_MS`], each stream's time, its intervals missed
    /// and its intervals served.
    fn report_streams(&mut self) {
        let name = self.name;
        let Some(xhci) = self.xhci.as_ref() else {
            return;
        };
        let now = counter();
        let period = counter_frequency().saturating_mul(STREAM_REPORT_MS) / 1000;
        for (id, entry) in self.playing.iter_mut().enumerate() {
            let Some(playing) = entry.as_mut() else {
                continue;
            };
            let due_at = playing
                .reports
                .saturating_add(1)
                .saturating_mul(period)
                .saturating_add(playing.started);
            if period == 0 || now < due_at {
                continue;
            }
            playing.reports = playing.reports.saturating_add(1);
            let Some(stats) = xhci.stream_stats(id) else {
                continue;
            };
            let seconds = playing
                .reports
                .saturating_mul(STREAM_REPORT_MS)
                .checked_div(1000)
                .unwrap_or(0);
            let (port, endpoint, direction) = (playing.port, playing.endpoint, playing.direction());
            if stats.missed == 0 {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {endpoint:#04x} {direction}: {seconds} s, no interval missed"
                );
            } else {
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {endpoint:#04x} {direction}: {seconds} s, {} intervals missed so far, {} of them with its ring empty",
                    stats.missed,
                    stats.empty
                );
            }
            debug_println!(
                "NOWAYOS: {name}: port {port}: endpoint {endpoint:#04x} {direction}: {} intervals served, {} bytes",
                stats.served,
                stats.bytes
            );
            if let Source::Speaker { .. } = playing.source
                && let Some(client) = self.client.as_ref()
                && client.speaker_counts() != (0, 0)
            {
                let (silent, dropped) = client.speaker_counts();
                debug_println!(
                    "NOWAYOS: {name}: port {port}: endpoint {endpoint:#04x} out: the client sent too little: {silent} frames of silence, {dropped} records dropped so far"
                );
            }
        }
    }

    /// Gives back `slot`, if the enumeration got one, and leaves the device
    /// on `port` until it is unplugged.
    fn give_up(&mut self, port: u8, slot: u8) {
        self.give_back(slot);
        self.set_port(port, PortUse::Unused);
    }
}

/// Fills `room` with `frames` frames for a speaker in `format` - the
/// client's, silence where it has sent none - as far as `room` holds whole
/// frames; for a speaker that takes only whole packets, padded with zeros
/// to all of `room` (Audio 1.0, section 4.6.1.2). Returns its bytes.
fn speaker_packet(
    room: &mut [u8],
    frames: usize,
    format: &Pcm,
    max_packets_only: bool,
    mut client: Option<&mut client::Client>,
) -> usize {
    let frame_bytes = format.frame_bytes();
    let channels = usize::from(format.channels).min(usize::from(audio::MAX_CHANNELS));
    let mut written = 0usize;
    if frame_bytes != 0 {
        for out in room.chunks_exact_mut(frame_bytes).take(frames) {
            let mut samples = [0i16; audio::MAX_CHANNELS as usize];
            let frame = samples.get_mut(..channels).unwrap_or_default();
            if !client
                .as_deref_mut()
                .is_some_and(|client| client.next_frame(frame))
            {
                frame.fill(0);
            }
            written = written.saturating_add(format.write_frame(frame, out));
        }
    }
    if max_packets_only {
        room.get_mut(written..).unwrap_or_default().fill(0);
        written = room.len();
    }
    written
}

impl Handler for UsbHost {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(TIMER.channel()) {
            while let Ok(Some(_)) = TIMER.expired() {}
            self.tick();
        }
        Ok(())
    }
}
