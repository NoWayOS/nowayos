// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-display-ramfb`: the display PD of the interactive demo in QEMU.
//! It plays the display's side of the display contract, as the
//! stub in `ui-demo` does, but shows each frame on QEMU's screen through
//! the `ramfb` device instead of only hashing it.
//!
//! At start-up it reads the screen's size from its configuration region
//! (`nowayos_region::screen`), gives QEMU a scan-out buffer of that size -
//! a DMA region whose physical address it learns, which the Microkit tool
//! therefore places contiguously (`nowayos_hw::ramfb`) - and announces the
//! layout to the UI PD: the screen at its own pixels, at the device scale
//! `nowayos_display::device_scale` gives it - 1920 x 1080 at 2,
//! the Pi 5's window - in Xrgb8888, the Pi 5's format, so the UI draws
//! exactly the frames the Pi shows. ramfb shows XRGB8888 too, so each row
//! is copied as it is. For each `Present` the UI sends, it checks the
//! record (`nowayos-display`'s `DisplayEnd`: in sequence, inside the
//! layout), copies the rectangles it names out of the framebuffer it shares
//! read-only with the UI into the scan-out buffer, prints the whole frame's
//! hash - the hash the stub prints, which a host test computes from the
//! approved golden images - and acknowledges it.
//!
//! It holds fw_cfg's registers and two DMA regions (role `display`); no
//! interrupt. Everything raw is in `nowayos-hw`; this PD has no `unsafe`
//! code. QEMU only: fw_cfg is QEMU's own device.

#![no_std]

use nowayos_display::{
    DisplayEnd, FrameHash, FramebufferLayout, PixelFormat, Rect, Size, device_scale, scale_row,
};
use nowayos_hw::fw_cfg::FwCfg;
use nowayos_hw::{DmaRegion, device, dma_region, ramfb};
use nowayos_ipc::{Consumer, Producer};
use nowayos_region::{
    Frame, ReadOnly, RegionError, RingRx, RingTx, read_only, read_write, ring_consumer,
    ring_producer, screen,
};
use nowayos_ui_loop::{RING_SLOT, RING_SLOTS};
use sel4_microkit::{Channel, ChannelSet, Handler, Infallible, debug_println};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: display:";

/// The channel to the UI PD.
const UI: Channel = Channel::new(0);

/// The device number the system description binds fw_cfg to.
const FW_CFG: usize = 0;

/// The DMA region for fw_cfg's requests, its file directory and ramfb's
/// configuration.
const REQUESTS: usize = 0;

/// The DMA region QEMU scans out.
const SCAN_OUT: usize = 1;

/// The widest screen this PD shows: the bytes of one row must fit its row
/// buffers.
const MAX_WIDTH: u32 = 1920;
const MAX_ROW: usize = 7680;

/// The format the UI draws in: the Pi 5's framebuffer's.
const FRAME_FORMAT: PixelFormat = PixelFormat::Xrgb8888;

/// Starts the display PD: what its program's `#[protection_domain]` entry
/// returns, for NoWayOS's systems and a product's own program alike. It
/// prints every frame's hash, for the golden checks.
#[must_use]
pub fn init() -> DisplayPd {
    start_pd(false)
}

/// As [`init`], but quiet: it prints no frame's hash, and so spends no time
/// hashing - for a system whose frames are not golden, such as one showing
/// a remote screen.
#[must_use]
pub fn init_quiet() -> DisplayPd {
    start_pd(true)
}

fn start_pd(quiet: bool) -> DisplayPd {
    debug_println!("{TAG} starting");
    let state = match start(quiet) {
        Ok(state) => Some(state),
        Err(why) => {
            debug_println!("{TAG} gave up: {why}");
            None
        }
    };
    DisplayPd { state }
}

/// What the display PD holds. Its regions:
///
/// | Symbol | Region | Map |
/// | --- | --- | --- |
/// | `nowayos_rw_region_0` | the `Present` ring's control | `rw` |
/// | `nowayos_ro_region_0` | the `Present` ring's messages | `r` |
/// | `nowayos_rw_region_1` | the display ring's messages | `rw` |
/// | `nowayos_ro_region_1` | the display ring's control | `r` |
/// | `nowayos_ro_region_2` | the framebuffer the UI draws into | `r` |
/// | `nowayos_ro_region_7` | the screen's size | `r` |
/// | `nowayos_device_0` | fw_cfg's registers | `rw`, uncached |
/// | `nowayos_dma_0`, `nowayos_dma_1` | fw_cfg's requests; the scan-out buffer | `rw`, uncached |
struct State {
    from_ui: RingRx<RING_SLOTS, RING_SLOT>,
    to_ui: RingTx<RING_SLOTS, RING_SLOT>,
    frame: Frame<ReadOnly>,
    scan_out: DmaRegion,
    /// The scan-out buffer's layout: the screen in XRGB8888.
    shown: FramebufferLayout,
    end: DisplayEnd,
    /// Whether to leave out each frame's hash.
    quiet: bool,
}

fn start(quiet: bool) -> Result<State, &'static str> {
    let region = |_: RegionError| "a region is not mapped as this PD expects";
    let (width, height) = screen().map_err(|_| "no screen size")?;
    if width > MAX_WIDTH {
        return Err("the screen is wider than this PD shows");
    }
    let shown = FramebufferLayout::packed(width, height, PixelFormat::Xrgb8888)
        .map_err(|_| "the screen's layout is impossible")?;
    let layout = FramebufferLayout::packed(width, height, FRAME_FORMAT)
        .map_err(|_| "the screen's layout is impossible")?;
    let scale = device_scale(Size::new(width, height));
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
    let frame = read_only(2)
        .map_err(region)?
        .into_frame(layout.byte_len())
        .map_err(region)?;

    let granted = |_| "fw_cfg or a DMA region is not granted as this PD expects";
    let mut fw_cfg = FwCfg::new(device(FW_CFG).map_err(granted)?).map_err(granted)?;
    let mut requests = dma_region(REQUESTS).map_err(granted)?;
    let scan_out = dma_region(SCAN_OUT).map_err(granted)?;
    let file = fw_cfg
        .file(ramfb::FILE_NAME, &mut requests, 0)
        .map_err(|_| "could not read fw_cfg's file directory")?
        .ok_or("fw_cfg has no etc/ramfb - QEMU needs -device ramfb")?;
    ramfb::show(
        &mut fw_cfg,
        file,
        &mut requests,
        0,
        &scan_out,
        (width, height, shown.stride()),
    )
    .map_err(|_| "could not give QEMU the scan-out buffer")?;

    let end = DisplayEnd::scaled(layout, scale).map_err(|_| "the layout is impossible")?;
    let mut tx = Producer::new(&to_ui).map_err(|_| "the ring to the UI is broken")?;
    tx.try_send(&end.configure())
        .map_err(|_| "could not send the layout")?;
    UI.notify();
    debug_println!(
        "{TAG} showing a {width}x{height} Xrgb8888 screen on QEMU's ramfb, from {width}x{height} {FRAME_FORMAT:?} frames at scale {scale}"
    );
    Ok(State {
        from_ui,
        to_ui,
        frame,
        scan_out,
        shown,
        end,
        quiet,
    })
}

impl State {
    /// Takes every record from the UI: checks it, shows the frame it
    /// presents, acknowledges it.
    fn serve(&mut self) -> Result<(), &'static str> {
        let Self {
            from_ui,
            to_ui,
            frame,
            scan_out,
            shown: screen,
            end,
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
                    debug_println!("{TAG} refused a record: {error}");
                    continue;
                }
            };
            for rect in shown.damage() {
                show(end.layout(), *screen, *rect, frame, scan_out)?;
            }
            if !*quiet {
                let hash = hash(end.layout(), frame)?;
                debug_println!("{TAG} showing {hash:#018x}");
            }
            let mut tx = Producer::new(&*to_ui).map_err(|_| "the ring to the UI is broken")?;
            tx.try_send(&shown.done())
                .map_err(|_| "could not acknowledge the frame")?;
            UI.notify();
        }
        Ok(())
    }
}

/// Copies the part of the frame inside `rect` from `frame`, laid out as
/// `layout`, to `scan_out`, laid out as `screen`, row by row, converting
/// each to the screen's format.
fn show(
    layout: FramebufferLayout,
    screen: FramebufferLayout,
    rect: Rect,
    frame: &Frame<ReadOnly>,
    scan_out: &mut DmaRegion,
) -> Result<(), &'static str> {
    let mut from = [0_u8; MAX_ROW];
    let mut to = [0_u8; MAX_ROW];
    let left = u32::try_from(rect.origin.x).map_err(|_| "a rectangle off the screen")?;
    let top = u32::try_from(rect.origin.y).map_err(|_| "a rectangle off the screen")?;
    let len = usize::try_from(rect.size.width)
        .ok()
        .and_then(|width| width.checked_mul(usize::from(layout.format().bytes_per_pixel())))
        .ok_or("a rectangle wider than the screen")?;
    let from = from
        .get_mut(..len)
        .ok_or("a rectangle wider than this PD's buffer")?;
    for y in top..top.saturating_add(rect.size.height) {
        let source = layout
            .pixel_offset(left, y)
            .ok_or("a rectangle outside the layout")?;
        if !frame.load(source, from) {
            return Err("the frame is shorter than the layout");
        }
        let written = scale_row(layout.format(), from, screen.format(), 1, &mut to)
            .ok_or("a row could not be converted")?;
        let target = screen
            .pixel_offset(left, y)
            .ok_or("a rectangle outside the screen")?;
        scan_out
            .write(
                target,
                to.get(..written).ok_or("a row could not be converted")?,
            )
            .map_err(|_| "the frame does not fit the scan-out buffer")?;
    }
    Ok(())
}

/// The hash of the whole frame `layout` covers, copied out row by row.
fn hash(layout: FramebufferLayout, frame: &Frame<ReadOnly>) -> Result<u64, &'static str> {
    let mut row = [0_u8; MAX_ROW];
    let mut hash = FrameHash::new();
    let stride = usize::try_from(layout.stride()).map_err(|_| "stride")?;
    let bytes = row
        .get_mut(..stride)
        .ok_or("a row is wider than this PD's buffer")?;
    for y in 0..layout.height() {
        let offset = layout.pixel_offset(0, y).ok_or("row outside the layout")?;
        if !frame.load(offset, bytes) {
            return Err("the frame is shorter than the layout");
        }
        hash.update(bytes);
    }
    Ok(hash.finish())
}

/// The display PD: `None` once it has given up.
pub struct DisplayPd {
    state: Option<State>,
}

impl Handler for DisplayPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if channels.contains(UI)
            && let Some(state) = self.state.as_mut()
            && let Err(why) = state.serve()
        {
            debug_println!("{TAG} gave up: {why}");
            self.state = None;
        }
        Ok(())
    }
}
