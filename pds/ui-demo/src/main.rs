// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-ui-demo`: the UI protection domain.
//!
//! It runs the clickable demo's screens (`nowayos-demo`) through
//! `nowayos-ui-loop`, which does everything else: the display contract, the
//! framebuffer it shares with the display PD, the renderer and the rings.
//! This file is all a product UI PD adds - its screens, and turning a
//! notification into a call to the loop. No `unsafe` code, no device, no
//! interrupt (its role in the grants file is `ui`).
//!
//! The demo's screens are built once the display has announced its layout
//! and device scale, laid out in its viewport with the theme's metrics at
//! that scale: the Pi 5's window, 1920 x 1080 at scale 2, in QEMU
//! and on the Pi.
//!
//! Input arrives from the input PD as canonical `InputEvent` records. After
//! each batch the UI draws only what changed and presents those rectangles;
//! once the display has shown them, the UI tells the input PD it is ready
//! for more. A malformed record is rejected - after it has been copied out
//! of the ring - and the rest of the batch is still handled. It prints how
//! long each frame took to draw (the counter; `nowayos-hw`).

#![no_std]
#![no_main]

use nowayos_demo::{Demo, action_name};
use nowayos_display::{DeviceScale, UiEvent};
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_render_eg::{DEFAULT_BYTES, DEFAULT_SLOTS, Glyphs, Slot};
use nowayos_ui_loop::{LoopError, UiChannels, UiLoop};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, NullHandler, debug_println, protection_domain,
};

/// The channel to the display PD.
const DISPLAY: Channel = Channel::new(0);

/// The channel to the input PD.
const INPUT: Channel = Channel::new(1);

#[protection_domain]
fn init() -> NullHandler {
    debug_println!("NOWAYOS: ui: starting");
    // The glyph store's storage lives in this frame, which runs the event
    // loop and never returns: a PD has no heap. The system gives the PD a
    // stack to hold it.
    let mut pixels = [0_u8; DEFAULT_BYTES];
    let mut slots = [Slot::EMPTY; DEFAULT_SLOTS];
    let glyphs = Glyphs::new(&mut pixels, &mut slots);
    let state = match start(glyphs) {
        Ok(state) => Some(state),
        Err(error) => {
            debug_println!("NOWAYOS: ui: gave up: {error}");
            None
        }
    };
    run_forever(&mut UiPd { state })
}

/// rust-sel4's event loop for `pd`, which never returns.
#[allow(
    unreachable_code,
    reason = "Handler::run never returns: its type, Result<Never, Infallible>, has no value"
)]
fn run_forever(pd: &mut UiPd<'_>) -> ! {
    match pd.run() {
        Ok(never) => match never {},
        Err(infallible) => match infallible {},
    }
}

fn start(glyphs: Glyphs<'_>) -> Result<State<'_>, LoopError> {
    let channels = UiChannels::bind()?;
    let ui = UiLoop::new(channels.frame_len());
    Ok(State {
        channels,
        ui,
        glyphs,
        demo: None,
    })
}

/// Everything the UI PD holds once started.
struct State<'g> {
    channels: UiChannels,
    ui: UiLoop,
    /// Where its text's glyphs come from: drawn from the outlines, kept.
    glyphs: Glyphs<'g>,
    /// The demo, once the display has announced its layout.
    demo: Option<Demo>,
}

impl State<'_> {
    /// Takes every record from the display and acts on it.
    fn serve(&mut self) -> Result<(), LoopError> {
        while let Some(record) = self.channels.next_from_display()? {
            match self.ui.receive(&record)? {
                UiEvent::Configured(layout) => {
                    let scale = self.ui.scale().unwrap_or(DeviceScale::ONE);
                    debug_println!(
                        "NOWAYOS: ui: the display is {}x{} {:?} at scale {scale}",
                        layout.width(),
                        layout.height(),
                        layout.format()
                    );
                    let viewport = self.ui.viewport().ok_or(LoopError::Ring)?;
                    self.demo =
                        Some(Demo::new(self.ui.metrics(), viewport).map_err(|_| LoopError::Ring)?);
                    self.present()?;
                }
                UiEvent::PresentDone(sequence) => {
                    debug_println!("NOWAYOS: ui: frame {sequence} shown");
                    INPUT.notify();
                }
            }
        }
        Ok(())
    }

    /// Takes every input record waiting, then draws and presents the
    /// result.
    fn take_input(&mut self) -> Result<(), LoopError> {
        let mut any = false;
        while let Some(record) = self.channels.next_input()? {
            any = true;
            match record {
                Ok(event) => {
                    let Some(demo) = self.demo.as_mut() else {
                        continue;
                    };
                    if let Some(action) = demo.handle(event).map_err(|_| LoopError::Ring)? {
                        debug_println!("NOWAYOS: ui: action {}", action_name(action));
                    }
                }
                Err(error) => debug_println!("NOWAYOS: ui: input record rejected: {error}"),
            }
        }
        if any {
            self.present()?;
        }
        Ok(())
    }

    /// Draws what changed - the whole screen, the first time - and
    /// presents it, saying how long drawing took.
    fn present(&mut self) -> Result<(), LoopError> {
        let Some(demo) = self.demo.as_mut() else {
            return Ok(());
        };
        let started = counter();
        let present =
            self.ui
                .draw_dirty(demo.tree_mut(), self.channels.frame(), &mut self.glyphs)?;
        let took = counter().saturating_sub(started);
        self.channels.send(&present)?;
        DISPLAY.notify();
        let pixels = present
            .damage()
            .iter()
            .fold(0_u64, |sum, rect| sum.saturating_add(rect.area()));
        debug_println!(
            "NOWAYOS: ui: frame {} drawn and presented: {} rectangles, {pixels} pixels in {} us",
            present.sequence(),
            present.damage().len(),
            took.saturating_mul(1_000_000)
                .checked_div(counter_frequency())
                .unwrap_or(0)
        );
        Ok(())
    }
}

/// The UI PD: `None` once it has given up.
struct UiPd<'g> {
    state: Option<State<'g>>,
}

impl Handler for UiPd<'_> {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let mut result = Ok(());
        if channels.contains(DISPLAY) {
            result = state.serve();
        }
        if result.is_ok() && channels.contains(INPUT) {
            result = state.take_input();
        }
        if let Err(error) = result {
            debug_println!("NOWAYOS: ui: gave up: {error}");
            self.state = None;
        }
        Ok(())
    }
}
