// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-picture-show`: the test instrument of
//! `systems/picture-decode-test.system` (#10084) - the picture decoder PD's
//! client and its parent.
//!
//! Once the display has announced its layout, it sends the decoder five
//! pictures, one at a time, through the encoded region and the request
//! ring: NoWayOS's sample JPEG, a PNG whose header claims 100000 x 100000
//! pixels, the JPEG cut in half, a 1024 x 1024 PNG larger than the
//! decoder's heap, and the sample PNG. A picture decoded is copied out of
//! the pixel region into this PD's own heap - checked against the size it
//! was told - and lent to the renderer by its `LiveId` at each frame; a
//! refusal is shown as such. When the decoder faults - the large picture
//! stops it - this PD, its parent, restarts it from its restart image; the
//! decoder's new restart epoch says the picture in flight went unanswered,
//! and it is not sent again: what stopped the decoder once would again.
//! The screen shows both pictures and what became of each of the five.
//!
//! Regions: the UI loop's (`nowayos_ui_loop::UiChannels`: read-write 0 to
//! 3, read-only 0 to 2), then
//!
//! | Symbol | Region | Map |
//! | --- | --- | --- |
//! | `nowayos_rw_region_4` | the request ring's messages | `rw` |
//! | `nowayos_ro_region_3` | the request ring's control | `r` |
//! | `nowayos_rw_region_5` | the answer ring's control | `rw` |
//! | `nowayos_ro_region_4` | the answer ring's messages | `r` |
//! | `nowayos_rw_region_6` | the encoded picture | `rw` |
//! | `nowayos_ro_region_5` | the pixels | `r` |
//! | `nowayos_ro_region_6` | the decoder's restart image | `r` |
//! | `nowayos_heap_region` | its heap: the pictures' copies | `rw` |
//!
//! A test instrument: never in a product image.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::fmt::Write as _;

use nowayos_bounded::BoundedStr;
use nowayos_display::{FrameMemory, UiEvent};
use nowayos_ipc::{Consumer, EpochChange, EpochWatch, Producer};
use nowayos_picture_wire::{Decode, Decoded, Format, MAX_ENCODED, PIXEL_BYTES, Refusal, pixel_len};
use nowayos_region::{
    Frame, ReadOnly, ReadWrite, RingRx, RingTx, read_only, read_write, ring_consumer, ring_producer,
};
use nowayos_render_eg::{DEFAULT_BYTES, DEFAULT_SLOTS, Glyphs, Live, Slot};
use nowayos_restart::image::HEADER_LEN;
use nowayos_restart::{Header, restart};
use nowayos_ui::builder::UiBuilder;
use nowayos_ui::pictures::{Fit, View};
use nowayos_ui::{LiveId, SlotId, Style, TextRole, TextStyle, Tree};
use nowayos_ui_loop::{LoopError, UiChannels, UiLoop};
use sel4_microkit::{
    Channel, ChannelSet, Child, Handler, Infallible, MessageInfo, NullHandler, debug_println,
    protection_domain,
};

nowayos_heap::declare_region_heap!(HEAP, Some(report_refusal));

/// Start of every line this program prints.
const TAG: &str = "NOWAYOS: picture-show:";

/// The channel to the display PD.
const DISPLAY: Channel = Channel::new(0);

/// The channel to the decoder.
const DECODER: Channel = Channel::new(1);

/// The decoder's child id.
const DECODER_CHILD: usize = 0;

/// How many times it restarts the decoder.
const MAX_RESTARTS: u32 = 3;

/// The decoder's rings: slots, and bytes per slot.
const SLOTS: usize = 4;
const SLOT: usize = 32;

/// The tree's nodes.
const NODES: usize = 16;

/// NoWayOS's sample pictures, drawn for NoWayOS; and the test's hostile
/// ones, made for it (`crates/nowayos-picture-decode/tests/pictures`).
const LANDSCAPE: &[u8] = include_bytes!("../../../crates/nowayos-pictures/pictures/landscape.jpg");
const BADGE: &[u8] = include_bytes!("../../../crates/nowayos-pictures/pictures/badge.png");
const HUGE: &[u8] =
    include_bytes!("../../../crates/nowayos-picture-decode/tests/pictures/huge.png");
const BIG: &[u8] = include_bytes!("../../../crates/nowayos-picture-decode/tests/pictures/big.png");

/// The pictures it sends, in order: name, format, bytes, and where a
/// decoded one shows.
const PICTURES: [(&str, Format, &[u8], Option<LiveId>); 5] = [
    ("landscape.jpg", Format::Jpeg, LANDSCAPE, Some(LiveId(1))),
    ("huge.png", Format::Png, HUGE, None),
    (
        "truncated.jpg",
        Format::Jpeg,
        LANDSCAPE.split_at(LANDSCAPE.len() / 2).0,
        None,
    ),
    ("big.png", Format::Png, BIG, None),
    ("badge.png", Format::Png, BADGE, Some(LiveId(2))),
];

/// The heap refused a request: say so; the allocation error stops the PD.
fn report_refusal(layout: Layout, in_use: usize) {
    debug_println!(
        "{TAG} its heap refused {} bytes with {in_use} in use",
        layout.size()
    );
}

#[protection_domain]
fn init() -> NullHandler {
    debug_println!("{TAG} starting");
    let mut pixels = [0_u8; DEFAULT_BYTES];
    let mut slots = [Slot::EMPTY; DEFAULT_SLOTS];
    let glyphs = Glyphs::new(&mut pixels, &mut slots);
    let state = match start(glyphs) {
        Ok(state) => Some(state),
        Err(why) => {
            debug_println!("{TAG} gave up: {why}");
            None
        }
    };
    run_forever(&mut ShowPd { state })
}

/// rust-sel4's event loop for `pd`, which never returns.
#[allow(
    unreachable_code,
    reason = "Handler::run never returns: its type, Result<Never, Infallible>, has no value"
)]
fn run_forever(pd: &mut ShowPd<'_>) -> ! {
    match pd.run() {
        Ok(never) => match never {},
        Err(infallible) => match infallible {},
    }
}

fn start(glyphs: Glyphs<'_>) -> Result<State<'_>, &'static str> {
    nowayos_heap::give_region(&HEAP).map_err(|_| "its heap is not mapped as it expects")?;
    let channels = UiChannels::bind().map_err(|_| "the UI loop's regions are not mapped")?;
    let ui = UiLoop::new(channels.frame_len());
    let requests = read_write(4)
        .and_then(|messages| ring_producer(messages, read_only(3)?))
        .map_err(|_| "the request ring is not mapped")?;
    let answers = read_write(5)
        .and_then(|control| ring_consumer(control, read_only(4)?))
        .map_err(|_| "the answer ring is not mapped")?;
    let encoded = read_write(6)
        .and_then(|region| region.into_frame(MAX_ENCODED as usize))
        .map_err(|_| "the encoded region is not mapped")?;
    let pixels = read_only(5)
        .and_then(|region| region.into_frame(PIXEL_BYTES))
        .map_err(|_| "the pixel region is not mapped")?;
    let image = read_only(6)
        .and_then(|region| region.into_frame(HEADER_LEN))
        .ok()
        .and_then(|frame| {
            let mut bytes = [0_u8; HEADER_LEN];
            frame.load(0, &mut bytes).then_some(bytes)
        })
        .and_then(|bytes| Header::parse(&bytes).ok());
    let (Ok(mut tx), Ok(mut rx)) = (Producer::new(&requests), Consumer::new(&answers)) else {
        return Err("a ring is broken");
    };
    tx.begin_epoch();
    rx.begin_epoch();
    let _ = rx.skip_in_flight();
    let mut decoder = EpochWatch::new();
    let seen = tx.peer_epoch();
    decoder.observe(seen);
    Ok(State {
        channels,
        ui,
        glyphs,
        tree: None,
        requests,
        answers,
        encoded,
        pixels,
        decoder,
        image,
        restarts: 0,
        decoder_up: seen != 0,
        next: 0,
        waiting: None,
        shown: Shown::default(),
        lines: [None; 5],
        redraw: false,
    })
}

/// The pictures decoded, in this PD's own heap: lent to each frame.
#[derive(Default)]
struct Shown {
    pictures: Vec<(LiveId, u32, u32, Vec<u8>)>,
}

impl Live for Shown {
    fn view(&self, id: LiveId) -> Option<View<'_>> {
        let (_, width, height, rgba) = self.pictures.iter().find(|(shown, ..)| *shown == id)?;
        View::new(*width, *height, rgba)
    }
}

/// What became of a picture.
#[derive(Clone, Copy)]
enum Fate {
    Decoded(u16, u16),
    Refused(Refusal),
    Stopped,
}

struct State<'g> {
    channels: UiChannels,
    ui: UiLoop,
    glyphs: Glyphs<'g>,
    tree: Option<Tree<NODES>>,
    requests: RingTx<SLOTS, SLOT>,
    answers: RingRx<SLOTS, SLOT>,
    encoded: Frame<ReadWrite>,
    pixels: Frame<ReadOnly>,
    decoder: EpochWatch,
    image: Option<Header>,
    restarts: u32,
    /// The decoder has started: its first restart epoch is seen. Nothing
    /// is sent before, or its start would drop it as in flight.
    decoder_up: bool,
    /// The next picture to send.
    next: usize,
    /// The picture sent and not yet answered.
    waiting: Option<usize>,
    shown: Shown,
    lines: [Option<Fate>; 5],
    /// Something changed while a frame was being shown.
    redraw: bool,
}

impl State<'_> {
    fn serve_display(&mut self) -> Result<(), LoopError> {
        while let Some(record) = self.channels.next_from_display()? {
            match self.ui.receive(&record)? {
                UiEvent::Configured(_) => {
                    let viewport = self.ui.viewport().ok_or(LoopError::Ring)?;
                    self.tree =
                        Some(screen(viewport, &self.ui.metrics()).map_err(|_| LoopError::Ring)?);
                    self.present()?;
                    if self.decoder_up {
                        self.send_next();
                    }
                }
                UiEvent::PresentDone(_) => {
                    if self.redraw {
                        self.redraw = false;
                        self.present()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Sends the next picture, if any is left.
    fn send_next(&mut self) {
        let Some(&(name, format, bytes, _)) = PICTURES.get(self.next) else {
            debug_println!("{TAG} every picture done");
            return;
        };
        let index = self.next;
        self.next = self.next.saturating_add(1);
        let Ok(len) = u32::try_from(bytes.len()) else {
            return;
        };
        let request = Decode {
            id: u16::try_from(index).unwrap_or(0),
            format,
            len,
        };
        if self.encoded.store(0, bytes)
            && Producer::new(&self.requests).is_ok_and(|mut tx| tx.try_send(&request).is_ok())
        {
            self.waiting = Some(index);
            debug_println!("{TAG} sent {name}, {len} bytes");
            DECODER.notify();
        } else {
            debug_println!("{TAG} could not send {name}");
        }
    }

    fn serve_decoder(&mut self) -> Result<(), LoopError> {
        let Ok(epoch) = Producer::new(&self.requests).map(|tx| tx.peer_epoch()) else {
            return Ok(());
        };
        // Restarted: the picture in flight went unanswered, and is not
        // sent again - it stopped the decoder once.
        match self.decoder.observe(epoch) {
            // Its first start: now pictures can be sent.
            EpochChange::Changed { from: 0, .. } => {
                self.decoder_up = true;
                if self.tree.is_some() && self.next == 0 {
                    self.send_next();
                }
            }
            EpochChange::Changed { from, to } => {
                debug_println!("{TAG} the decoder restarted (epoch {from} -> {to})");
                if let Some(index) = self.waiting.take() {
                    self.settle(index, Fate::Stopped)?;
                }
            }
            _ => {}
        }
        let mut answers: Vec<Decoded> = Vec::new();
        if let Ok(mut rx) = Consumer::new(&self.answers) {
            while let Ok(Some(answer)) = rx.try_recv::<Decoded>() {
                answers.push(answer);
            }
        }
        for answer in answers {
            let index = usize::from(answer.id);
            if self.waiting != Some(index) {
                continue;
            }
            self.waiting = None;
            let fate = match answer.outcome {
                Ok((width, height)) => self.copy(index, width, height),
                Err(refusal) => Fate::Refused(refusal),
            };
            self.settle(index, fate)?;
        }
        Ok(())
    }

    /// Copies a decoded picture's pixels out of the region into this PD's
    /// heap, lent to the renderer from there.
    fn copy(&mut self, index: usize, width: u16, height: u16) -> Fate {
        let mut rgba = vec![0_u8; pixel_len(width, height)];
        let Some(&(_, _, _, Some(id))) = PICTURES.get(index) else {
            return Fate::Decoded(width, height);
        };
        if !self.pixels.load(0, &mut rgba) {
            return Fate::Refused(Refusal::TooLarge);
        }
        self.shown
            .pictures
            .push((id, u32::from(width), u32::from(height), rgba));
        if let Some(tree) = self.tree.as_mut() {
            tree.live_changed(id);
        }
        Fate::Decoded(width, height)
    }

    /// Records picture `index`'s fate, shows it, and sends the next.
    fn settle(&mut self, index: usize, fate: Fate) -> Result<(), LoopError> {
        let name = PICTURES.get(index).map_or("?", |picture| picture.0);
        let mut line: BoundedStr<64> = BoundedStr::new();
        let _ = match fate {
            Fate::Decoded(width, height) => write!(line, "{name}: decoded, {width} x {height}"),
            Fate::Refused(Refusal::TooLarge) => write!(line, "{name}: refused, too large"),
            Fate::Refused(Refusal::Malformed) => write!(line, "{name}: refused, malformed"),
            Fate::Refused(why) => write!(line, "{name}: refused, {why:?}"),
            Fate::Stopped => write!(line, "{name}: the decoder stopped; restarted"),
        };
        debug_println!("{TAG} {}", line.as_str());
        if let (Some(tree), Some(slot)) = (self.tree.as_mut(), self.lines.get_mut(index)) {
            *slot = Some(fate);
            let id = SlotId(u16::try_from(index).unwrap_or(0).saturating_add(1));
            let _ = tree.set_slot(id, line.as_str());
        }
        self.present()?;
        self.send_next();
        Ok(())
    }

    /// Draws what changed and presents it, or once the frame being shown
    /// is done.
    fn present(&mut self) -> Result<(), LoopError> {
        let Some(tree) = self.tree.as_mut() else {
            return Ok(());
        };
        if !self.ui.can_draw() {
            self.redraw = true;
            return Ok(());
        }
        let present =
            self.ui
                .draw_dirty_live(tree, self.channels.frame(), &mut self.glyphs, &self.shown)?;
        self.channels.send(&present)?;
        DISPLAY.notify();
        Ok(())
    }

    /// The decoder faulted: restarted from its restart image, at most
    /// [`MAX_RESTARTS`] times.
    fn fault(&mut self, child: Child, label: u64) {
        if child.index() != DECODER_CHILD {
            return;
        }
        let Some(image) = self.image else {
            debug_println!("{TAG} the decoder faulted (label {label}); no restart image");
            return;
        };
        if self.restarts >= MAX_RESTARTS {
            debug_println!("{TAG} the decoder faulted again: left stopped");
            return;
        }
        self.restarts = self.restarts.saturating_add(1);
        match restart(child, &image) {
            Ok(()) => debug_println!(
                "{TAG} the decoder stopped by a fault; restarted it (restart {})",
                self.restarts
            ),
            Err(error) => debug_println!("{TAG} cannot restart the decoder: {error}"),
        }
    }
}

/// The screen: a title, the two pictures side by side, and a line for each
/// of the five.
fn screen(
    viewport: nowayos_display::Rect,
    metrics: &nowayos_ui::TextMetrics,
) -> Result<Tree<NODES>, nowayos_ui::UiError> {
    let mut tree: Tree<NODES> = Tree::new();
    let column = tree.column(Style::new().padding(32).gap(16))?;
    tree.set_root(column)?;
    let title = UiBuilder::text_styled(
        &mut tree,
        "Pictures decoded in a PD of their own",
        TextStyle::new(TextRole::Title),
    )?;
    tree.append(column, title)?;
    let row = tree.row(Style::new().gap(32))?;
    tree.append(column, row)?;
    let landscape = tree.live_picture(LiveId(1), (320, 200), Fit::Contain, "A landscape")?;
    tree.append(row, landscape)?;
    let badge = tree.live_picture(LiveId(2), (96, 96), Fit::Contain, "A badge")?;
    tree.append(row, badge)?;
    for line in 1..=5 {
        let slot = tree.slot(SlotId(line))?;
        tree.append(column, slot)?;
    }
    tree.layout(viewport, metrics)?;
    Ok(tree)
}

/// The PD: `None` once it has given up.
struct ShowPd<'g> {
    state: Option<State<'g>>,
}

impl Handler for ShowPd<'_> {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let mut result = Ok(());
        if channels.contains(DISPLAY) {
            result = state.serve_display();
        }
        if result.is_ok() && channels.contains(DECODER) {
            result = state.serve_decoder();
        }
        if let Err(error) = result {
            debug_println!("{TAG} gave up: {error}");
            self.state = None;
        }
        Ok(())
    }

    fn fault(
        &mut self,
        child: Child,
        msg_info: MessageInfo,
    ) -> Result<Option<MessageInfo>, Self::Error> {
        if let Some(state) = self.state.as_mut() {
            state.fault(child, msg_info.label());
        }
        Ok(None)
    }
}
