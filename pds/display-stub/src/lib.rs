// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-display-stub`: the display PD of `ui-demo`, which shows
//! each frame on the board's framebuffer when it has one, at the
//! monitor's own pixels.
//!
//! It plays the display's side of the contract: announces a layout and its
//! device scale, checks each `Present` it receives (`nowayos-display`'s
//! `DisplayEnd`: in sequence, inside the layout), shows the rectangles it
//! names, and acknowledges it. Unless it is quiet ([`init_quiet`]) it also
//! hashes the whole frame and prints the hash, which
//! `systems/ui-demo.expect` holds for each approved golden image, so the
//! test fails if one pixel the UI PD drew differs; and how long showing it
//! took.
//!
//! **The screen it asks for.** Where its system grants it a framebuffer the
//! firmware scans out (device 0 and the board's facts about it, read-only
//! region 5: the Pi 5's `firmware_fb`, `systems/rpi5b_2gb/ui-demo.system`),
//! it announces the framebuffer's window at the window's own pixels and
//! format: on the Pi 5, 1920 x 1064 r5g6b5 from the monitor's row 16, at
//! device scale 2 (`nowayos_display::device_scale`), so the UI lays out a
//! 960 x 532 screen and draws it crisp, filling the monitor with no border
//! and nothing doubled. Granted none (QEMU's `ui-demo`), it announces the
//! same screen, the Pi 5's window, and only hashes: QEMU runs exactly the
//! frames the Pi shows. It never needs to know the board: only whether it
//! was given a framebuffer, and its facts.
//!
//! A system built for the kiosk's 800 x 480 frames keeps working: when the
//! framebuffer region its system shares with the UI is too small for a
//! frame at the screen's own pixels, the stub announces 800 x 480
//! `Xrgb8888` at scale 1, as an older one does, and shows each frame at
//! the largest whole-number scale that fits below the window's first row,
//! centred - 2x on the Pi 5. It says which it chose. The choice is
//! `nowayos_display::frame_for`'s, whose host tests take both branches;
//! `nowayos-fb`'s show the 2x placement and copies.
//!
//! It shows only the rectangles each `Present` names (`nowayos-fb`'s
//! `Screen::show_rect`), so a key press copies one field to the screen,
//! not a whole screen. Unless quiet, it then reads back what the window
//! holds where the frame is shown and prints its hash - the scan-out hash,
//! which `systems/rpi5b_2gb/ui-demo.expect` holds too.
//!
//! **Clear of the scan**. Where the system also grants where the
//! display hardware is in its scan (device 1: the Pi 5's `hvs`, read-only),
//! each rectangle is copied only while the scan is off its rows, so a
//! moving window does not tear ([`scan`]).
//!
//! It maps the UI's framebuffer read-only, the board's framebuffer and the
//! scan position only if it is granted them; no interrupt (role `display`).
//! No `unsafe` code.
//!
//! A library and a program, as the timer and clock PDs are: NoWayOS's
//! systems run the program; a product's system runs the PD from a program of
//! its own whose `init` returns [`init`]'s, with the same regions and
//! channel - or [`init_quiet`]'s, which prints no frame's hash or timing,
//! for screens that show what a person enters.

#![no_std]

mod scan;

use nowayos_display::{
    DisplayEnd, FrameHash, FramebufferLayout, KIOSK_FRAME, MAX_DAMAGE_RECTS, PixelFormat, Rect,
    Size, ValidPresent, frame_for,
};
use nowayos_fb::{FbError, Screen, bind_window, format_of, window_size};
use nowayos_hw::Scanout;
use nowayos_hw::cpu::{counter, counter_frequency};
use nowayos_ipc::{Consumer, Producer};
use nowayos_region::{
    Frame, ReadOnly, RegionError, RingRx, RingTx, read_only, read_write, ring_consumer,
    ring_producer,
};
use nowayos_ui_loop::{RING_SLOT, RING_SLOTS};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

use crate::scan::Scan;

/// The channel to the UI PD.
const UI: Channel = Channel::new(0);

/// The screen announced when the system grants no framebuffer: the Pi 5's
/// window, 1920 x 1064 r5g6b5, so QEMU runs the frames the Pi shows.
pub const STAND_IN: Size = Size::new(1920, 1064);

/// The stand-in screen's format: the Pi 5's framebuffer's.
pub const STAND_IN_FORMAT: PixelFormat = PixelFormat::Rgb565;

/// The device number the system description binds the board's framebuffer
/// to, when it grants one.
const SCREEN: usize = 0;

/// The longest frame row the stub reads at once, in bytes: a 1920-pixel
/// row of `Xrgb8888`, rounded up.
const MAX_ROW: usize = 8192;

/// Starts the display PD: binds its rings, the UI's framebuffer and, if its
/// system grants one, the board's framebuffer, and announces the layout.
/// It prints the hash of every frame it shows, and the time showing it
/// took - how NoWayOS's systems are checked against their approved images
/// and measured.
#[must_use]
pub fn init() -> StubPd {
    start_with(false)
}

/// As [`init`], but printing no frame's hash or timing: for a
/// system whose screens show what a person enters. A frame's hash is the
/// same whenever the screen is, so a log of one hash per keystroke would
/// let whoever holds it recover an answer, character by character, by
/// trying each. Nothing else it prints names a frame's
/// contents.
#[must_use]
pub fn init_quiet() -> StubPd {
    start_with(true)
}

fn start_with(quiet: bool) -> StubPd {
    debug_println!("NOWAYOS: display-stub: starting");
    let state = match start(quiet) {
        Ok(state) => Some(state),
        Err(why) => {
            debug_println!("NOWAYOS: display-stub: gave up: {why}");
            None
        }
    };
    StubPd { state }
}

/// What the stub holds. Its regions:
///
/// | Symbol | Region | Map |
/// | --- | --- | --- |
/// | `nowayos_rw_region_0` | the `Present` ring's control | `rw` |
/// | `nowayos_ro_region_0` | the `Present` ring's messages | `r` |
/// | `nowayos_rw_region_1` | the display ring's messages | `rw` |
/// | `nowayos_ro_region_1` | the display ring's control | `r` |
/// | `nowayos_ro_region_2` | the framebuffer | `r` |
/// | `nowayos_ro_region_5` | the board's facts about its framebuffer, if granted | `r` |
/// | `nowayos_device_0` | the board's framebuffer's window, if granted | `rw`, cached or not (`Scanout` cleans each write to memory) |
/// | `nowayos_device_1` | where the display is in its scan, if granted (the Pi 5's `hvs`) | `r` |
struct State {
    from_ui: RingRx<RING_SLOTS, RING_SLOT>,
    to_ui: RingTx<RING_SLOTS, RING_SLOT>,
    frame: Frame<ReadOnly>,
    end: DisplayEnd,
    screen: Option<Screen<Scanout>>,
    /// The display's scan, which copies wait to be clear of.
    scan: Option<Scan>,
    /// Print no frame's hash or timing ([`init_quiet`]).
    quiet: bool,
}

fn start(quiet: bool) -> Result<State, &'static str> {
    let region = |_: RegionError| "a region is not mapped as the stub expects";
    let from_ui = ring_consumer(
        read_write(0).map_err(region)?,
        read_only(0).map_err(region)?,
    )
    .map_err(region)?;
    let to_ui = ring_producer(
        read_write(1).map_err(region)?,
        read_only(1).map_err(region)?,
    )
    .map_err(region)?;
    let shared = read_only(2).map_err(region)?;
    let window = board_window();
    // The screen at its own pixels: the board's window, or the stand-in -
    // or, in a system whose region is too small for it, the kiosk's frame
    // (`nowayos_display::frame_for`, host-tested).
    let (size, format) = match &window {
        Some((_, mode)) => (window_size(mode), format_of(mode)),
        None => (STAND_IN, STAND_IN_FORMAT),
    };
    let choice = frame_for(size, format, shared.size())
        .ok_or("the framebuffer region holds no frame the stub can ask for")?;
    if !choice.native {
        debug_println!(
            "NOWAYOS: display-stub: the framebuffer region holds {} bytes, too few for the screen at its own pixels: asking for the kiosk's {}x{} frames",
            shared.size(),
            KIOSK_FRAME.width,
            KIOSK_FRAME.height
        );
    }
    let (layout, scale) = (choice.layout, choice.scale);
    let frame = shared.into_frame(layout.byte_len()).map_err(region)?;
    let end = DisplayEnd::scaled(layout, scale).map_err(|_| "the layout is impossible")?;
    let screen = window.and_then(|(window, mode)| board_screen(window, mode, &layout));
    let scan = screen
        .as_ref()
        .and_then(|screen| Scan::bind(screen.mode().width(), screen.mode().height()));
    let mut tx = Producer::new(&to_ui).map_err(|_| "the ring to the UI is broken")?;
    tx.try_send(&end.configure())
        .map_err(|_| "could not send the layout")?;
    UI.notify();
    debug_println!(
        "NOWAYOS: display-stub: announced {}x{} {:?} at scale {scale}",
        layout.width(),
        layout.height(),
        layout.format()
    );
    Ok(State {
        from_ui,
        to_ui,
        frame,
        end,
        screen,
        scan,
        quiet,
    })
}

/// The board's framebuffer's window and mode, if the system grants one;
/// `None`, saying why, if it grants one that cannot be used; `None`,
/// silently, if it grants none.
fn board_window() -> Option<(Scanout, nowayos_region::FramebufferMode)> {
    match bind_window(SCREEN) {
        Ok(window) => Some(window),
        Err(FbError::Facts(RegionError::NotMapped { .. })) => None,
        Err(error) => {
            debug_println!(
                "NOWAYOS: display-stub: cannot show frames on the board's framebuffer: {error}"
            );
            None
        }
    }
}

/// The board's framebuffer set to show frames of `layout`, cleared; `None`,
/// saying why, if it cannot.
fn board_screen(
    window: Scanout,
    mode: nowayos_region::FramebufferMode,
    layout: &FramebufferLayout,
) -> Option<Screen<Scanout>> {
    let screen =
        Screen::new(window, mode, layout.size(), layout.format()).and_then(|mut screen| {
            screen.clear()?;
            Ok(screen)
        });
    let screen = match screen {
        Ok(screen) => screen,
        Err(error) => {
            debug_println!(
                "NOWAYOS: display-stub: cannot show frames on the board's framebuffer: {error}"
            );
            return None;
        }
    };
    let placement = screen.placement();
    if placement.scale() == 1 && placement.size() == window_size(&mode) {
        debug_println!(
            "NOWAYOS: display-stub: showing each frame on the board's {}x{} r5g6b5 framebuffer at its own pixels, from row {}",
            mode.width(),
            mode.height(),
            placement.y()
        );
    } else {
        debug_println!(
            "NOWAYOS: display-stub: showing each frame on the board's {}x{} r5g6b5 framebuffer, at {}x at ({}, {})",
            mode.width(),
            mode.height(),
            placement.scale(),
            placement.x(),
            placement.y()
        );
    }
    Some(screen)
}

impl State {
    /// Takes every record from the UI: checks it, shows the rectangles it
    /// names on the board's framebuffer, if there is one, hashes the frame
    /// and what the window shows unless quiet, and acknowledges it.
    fn serve(&mut self) -> Result<(), &'static str> {
        let Self {
            from_ui,
            to_ui,
            frame,
            end,
            screen,
            scan,
            quiet,
        } = self;
        let mut rx = Consumer::new(&*from_ui).map_err(|_| "the ring from the UI is broken")?;
        while let Some(record) = rx
            .try_recv_record()
            .map_err(|_| "the ring from the UI is broken")?
        {
            let shown = match end.receive(record.as_bytes()) {
                Ok(shown) => shown,
                Err(error) => {
                    debug_println!("NOWAYOS: display-stub: refused a record: {error}");
                    continue;
                }
            };
            let layout = end.layout();
            let started = counter();
            let pixels = show(layout, frame, screen, scan, &shown);
            let took = counter().saturating_sub(started);
            if !*quiet {
                report(layout, frame, screen, &shown, pixels, took)?;
            }
            let mut tx = Producer::new(&*to_ui).map_err(|_| "the ring to the UI is broken")?;
            tx.try_send(&shown.done())
                .map_err(|_| "could not acknowledge the frame")?;
            UI.notify();
        }
        Ok(())
    }
}

/// Shows `shown`'s rectangles of `frame`, laid out as `layout`, on the
/// board's framebuffer, if there is one, and returns how many pixels of
/// the frame they cover. Where the display's scan is granted, the PD first
/// waits for the scan to be where it cannot overtake a copy of every row
/// the rectangles cover, then copies them from the top down, so the whole
/// frame shows in one refresh ([`scan`]). A failure to show is
/// reported once, and the framebuffer given up.
fn show(
    layout: FramebufferLayout,
    frame: &Frame<ReadOnly>,
    screen: &mut Option<Screen<Scanout>>,
    scan: &mut Option<Scan>,
    shown: &ValidPresent,
) -> u64 {
    let damage = shown.damage();
    let pixels = damage
        .iter()
        .fold(0_u64, |pixels, rect| pixels.saturating_add(rect.area()));
    let mut rects: [Option<Rect>; MAX_DAMAGE_RECTS] =
        core::array::from_fn(|index| damage.get(index).copied());
    rects.sort_unstable_by_key(|rect| rect.map_or(i32::MAX, |rect| rect.origin.y));
    if let (Some(board), Some(scan)) = (screen.as_ref(), scan.as_mut()) {
        let rows = rects
            .iter()
            .flatten()
            .filter_map(|rect| board.rows_of(*rect))
            .reduce(|(top, bottom), (next_top, next_bottom)| {
                (top.min(next_top), bottom.max(next_bottom))
            });
        if let Some(rows) = rows {
            scan.wait_for(rows);
        }
    }
    let started = counter();
    let mut copied = 0_u64;
    for rect in rects.iter().flatten() {
        let Some(board) = screen.as_mut() else {
            break;
        };
        let load = |y: u32, x: u32, out: &mut [u8]| {
            layout
                .pixel_offset(x, y)
                .is_some_and(|offset| frame.load(offset, out))
        };
        let scale = u64::from(board.placement().scale());
        let on_screen = rect.area().saturating_mul(scale.saturating_mul(scale));
        if let Err(error) = board.show_rect(*rect, load) {
            debug_println!(
                "NOWAYOS: display-stub: cannot show frames on the board's framebuffer: {error}"
            );
            *screen = None;
        }
        copied = copied.saturating_add(on_screen);
    }
    if let Some(scan) = scan.as_mut()
        && screen.is_some()
    {
        scan.copied(copied, counter().saturating_sub(started));
    }
    pixels
}

/// Prints the frame's hash, the hash of what the window shows where the
/// frame is, and how long showing it took.
fn report(
    layout: FramebufferLayout,
    frame: &Frame<ReadOnly>,
    screen: &mut Option<Screen<Scanout>>,
    shown: &ValidPresent,
    pixels: u64,
    took: u64,
) -> Result<(), &'static str> {
    let sequence = shown.sequence();
    let hash = frame_hash(layout, frame)?;
    debug_println!("NOWAYOS: display-stub: frame {sequence} hash {hash:#018x}");
    if let Some(board) = screen.as_mut() {
        let mut scanned = FrameHash::new();
        match board.read_back(|row| scanned.update(row)) {
            Ok(()) => debug_println!(
                "NOWAYOS: display-stub: frame {sequence} on the framebuffer, scan-out hash {:#018x}",
                scanned.finish()
            ),
            Err(error) => {
                debug_println!(
                    "NOWAYOS: display-stub: cannot show frames on the board's framebuffer: {error}"
                );
                *screen = None;
            }
        }
    }
    if screen.is_some() {
        debug_println!(
            "NOWAYOS: display-stub: frame {sequence}: {} rectangles, {pixels} pixels shown in {} us",
            shown.damage().len(),
            microseconds(took)
        );
    }
    Ok(())
}

/// `ticks` of the counter in whole microseconds.
fn microseconds(ticks: u64) -> u64 {
    ticks
        .saturating_mul(1_000_000)
        .checked_div(counter_frequency())
        .unwrap_or(0)
}

/// The hash of the whole frame the layout covers, copied out row by row.
fn frame_hash(layout: FramebufferLayout, frame: &Frame<ReadOnly>) -> Result<u64, &'static str> {
    let mut row = [0_u8; MAX_ROW];
    let mut hash = FrameHash::new();
    let stride = usize::try_from(layout.stride()).map_err(|_| "stride")?;
    let bytes = row
        .get_mut(..stride)
        .ok_or("a row is wider than the stub's buffer")?;
    for y in 0..layout.height() {
        let offset = layout.pixel_offset(0, y).ok_or("row outside the layout")?;
        if !frame.load(offset, bytes) {
            return Err("the frame is shorter than the layout");
        }
        hash.update(bytes);
    }
    Ok(hash.finish())
}

/// The display PD, as Microkit runs it: it serves the UI's `Present`
/// records when notified, until it gives up.
pub struct StubPd {
    state: Option<State>,
}

impl Handler for StubPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(UI)
            && let Some(state) = self.state.as_mut()
            && let Err(error) = state.serve()
        {
            debug_println!("NOWAYOS: display-stub: gave up: {error}");
            self.state = None;
        }
        Ok(())
    }
}
