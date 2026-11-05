// SPDX-License-Identifier: MIT OR Apache-2.0
//! The UI protection domain's reusable loop: the display contract,
//! drawing into the framebuffer it shares with the display PD, and
//! presenting. A product UI PD adds only its screens - a `nowayos-ui` tree -
//! and the few lines that turn a notification into a call here (the loop
//! is NoWayOS's; the screens are the product's).
//!
//! The pieces it joins:
//!
//! * the display contract v0 (`nowayos-display`): the UI's [`UiEnd`] checks
//!   every record from the display - its layout must fit the framebuffer
//!   region - and allows one `Present` in flight. The display's layout comes
//!   with its device scale: screens are laid out in the layout's
//!   size divided by it, at the theme's metrics for it ([`UiLoop::viewport`],
//!   [`UiLoop::metrics`]), and drawn at the layout's own pixels - the Pi 5's
//!   1920 x 1080 window at scale 2, a 960 x 540 screen, crisp;
//! * the framebuffer, a region the UI maps read-write and the display PD
//!   read-only, viewed as atomic bytes (`nowayos-region`'s `Frame`);
//! * the renderer (`nowayos-render-eg`), drawing exactly the pixels of the
//!   golden images in CI;
//! * three `nowayos-ipc` rings: one to the display PD for `Present`, one
//!   from it for `Configure` and `PresentDone`, and one from the input PD
//!   carrying canonical `InputEvent` records, each copied out before it is
//!   decoded, so a malformed one is rejected and the next one read.
//!
//! [`UiLoop`] is the part that needs no seL4, tested on the host;
//! [`UiChannels`] binds the regions and rings in a PD.
//!
//! A frame is drawn whole ([`UiLoop::draw_all`]) or only where the tree
//! changed ([`UiLoop::draw_dirty`]), presenting just those
//! rectangles, so the display copies only them: a key press repaints one
//! field, not a screen of two million pixels. The renderer guarantees the
//! two give the same pixels.
//!
//! A loop made [`with_pointer`](UiLoop::with_pointer) draws the mouse
//! pointer over the tree where a mouse or a tablet last moved it:
//! a move repaints the old and new areas only, and the pointer is drawn
//! into each area repainted, after the tree - never into the tree, so never
//! into a golden image.
//!
//! Provisional.

#![cfg_attr(not(test), no_std)]

use core::fmt;

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::{
    ContractError, DeviceScale, DirtyRegion, FrameMemory, FramebufferLayout, MAX_DAMAGE_RECTS,
    MAX_RECORD_LEN, Point, Present, Rect, UiEnd, UiEvent,
};
use nowayos_input::InputEvent;
use nowayos_ipc::{Consumer, DecodeError, Producer, RecvError, RingError, SendError};
use nowayos_region::{
    Frame, ReadWrite, RegionError, RingRx, RingTx, read_only, read_write, ring_consumer,
    ring_producer,
};
use nowayos_render_eg::{
    Glyphs, RawFramebuffer, RawFramebufferError, Theme, pointer_area, render, render_dirty,
    render_pointer, to_eg,
};
use nowayos_ui::{Cursor, TextMetrics, Tree, Zoom};

/// Slots in each ring: one `Present` is in flight at a time, so a few are
/// plenty.
pub const RING_SLOTS: usize = 4;

/// Bytes per ring slot: the longest display-contract record.
pub const RING_SLOT: usize = MAX_RECORD_LEN;

/// Slots in the input ring: room for a burst of events between frames. The
/// input broker's side is the same constant (`nowayos_input::broker`).
pub const INPUT_SLOTS: usize = nowayos_input::broker::UI_RING_SLOTS;

/// Bytes per input-ring slot: an `InputEvent` record (an 8-byte header and
/// a 12-byte payload), rounded up.
pub const INPUT_SLOT: usize = nowayos_input::broker::UI_RING_SLOT;

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopError {
    /// The display broke the contract, or the UI did.
    Contract(ContractError),
    /// The framebuffer memory is shorter than the layout.
    Framebuffer(RawFramebufferError),
    /// A ring could not be used: its indices are impossible, or it is full.
    Ring,
    /// A region could not be bound or viewed.
    Region(RegionError),
}

impl fmt::Display for LoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(f, "display contract: {error}"),
            Self::Framebuffer(error) => write!(f, "framebuffer: {error}"),
            Self::Ring => write!(f, "a ring to the display PD failed"),
            Self::Region(error) => write!(f, "region: {error}"),
        }
    }
}

impl From<ContractError> for LoopError {
    fn from(error: ContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<RegionError> for LoopError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

/// The UI's side of the display contract, and drawing: no seL4, so it runs
/// on the host too.
#[derive(Debug)]
pub struct UiLoop {
    end: UiEnd,
    theme: Theme<Rgb888>,
    /// Whether the mouse pointer is drawn.
    pointer: bool,
    /// The area the pointer was last drawn in, and its shape, while it
    /// shows.
    pointer_drawn: Option<(Rect, Cursor)>,
    /// The scale the person zoomed to (#10083), when not the display's.
    zoom: Option<DeviceScale>,
}

impl UiLoop {
    /// The loop for a framebuffer region of `region_len` bytes, drawing in
    /// the light theme.
    #[must_use]
    pub fn new(region_len: usize) -> Self {
        Self::with_theme(region_len, Theme::light())
    }

    /// The loop for a framebuffer region of `region_len` bytes, drawing in
    /// `theme`.
    #[must_use]
    pub const fn with_theme(region_len: usize, theme: Theme<Rgb888>) -> Self {
        Self {
            end: UiEnd::new(region_len),
            theme,
            pointer: false,
            pointer_drawn: None,
            zoom: None,
        }
    }

    /// The same loop, drawing the mouse pointer over the screens where a
    /// mouse or a tablet last moved it: for a UI with a mouse. A
    /// touch hides it ([`Tree::pointer_shown`]).
    #[must_use]
    pub const fn with_pointer(mut self) -> Self {
        self.pointer = true;
        self
    }

    /// Where the pointer is drawn for `tree`, if it is: its hot spot, its
    /// shape - what a press there will do (#10082) - and its area.
    fn pointer_for<const N: usize, const T: usize>(
        &self,
        tree: &Tree<N, T>,
    ) -> Option<(Point, Cursor, Rect)> {
        if !self.pointer {
            return None;
        }
        let at = tree.pointer_shown()?;
        let cursor = tree.cursor();
        Some((at, cursor, pointer_area(at, cursor, tree.scale())))
    }

    /// The theme, whose metrics the screens are laid out with.
    #[must_use]
    pub const fn theme(&self) -> &Theme<Rgb888> {
        &self.theme
    }

    /// Draws in `theme` from now on: a product's brand, or its
    /// high-contrast version, as the screen showing asks. The screens are
    /// then laid out again with the new [`metrics`](Self::metrics), and the
    /// next frame drawn whole ([`draw_all`](Self::draw_all)), as every
    /// pixel may change. Any theme draws at any device scale: its text
    /// comes from the glyph outlines.
    pub fn set_theme(&mut self, theme: Theme<Rgb888>) {
        self.theme = theme;
    }

    /// The layout the display announced, once it has.
    #[must_use]
    pub const fn layout(&self) -> Option<FramebufferLayout> {
        self.end.layout()
    }

    /// The display's device scale, once it has announced its layout: its
    /// pixels each way for each layout unit, 1 to 4 in quarter steps.
    #[must_use]
    pub const fn scale(&self) -> Option<DeviceScale> {
        self.end.scale()
    }

    /// The viewport to lay screens out in, once the display has announced
    /// its layout: the whole layout, in pixels. Laid out with
    /// [`metrics`](Self::metrics), a screen takes the layout's size divided
    /// by the scale in layout units - 960 x 540 on the Pi 5 at scale 2.
    #[must_use]
    pub fn viewport(&self) -> Option<Rect> {
        self.end.layout().map(|layout| layout.bounds())
    }

    /// The metrics to lay screens out with: the theme's at the display's
    /// scale ([`Theme::metrics_at`]) - or the one zoomed to
    /// ([`zoom`](Self::zoom)) - or at scale 1 before the display has
    /// announced its layout. The loop draws at the same scale.
    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.theme
            .metrics_at(self.zoom.or(self.end.scale()).unwrap_or(DeviceScale::ONE))
    }

    /// Steps the scale the screens are laid out and drawn at by `zoom`
    /// (#10083; Ctrl and plus, minus or zero, [`UiEvent::Zoom`]): a quarter
    /// larger or smaller, within 1 to 4, or back to the display's own. The
    /// new scale if it changed - then lay the screens out again with
    /// [`metrics`](Self::metrics), which keeps focus and each scroll view's
    /// place, and draw them whole; `None` if it did not, or before the
    /// display has announced its layout.
    ///
    /// [`UiEvent::Zoom`]: nowayos_ui::UiEvent::Zoom
    pub fn zoom(&mut self, zoom: Zoom) -> Option<DeviceScale> {
        let base = self.end.scale()?;
        let now = self.zoom.unwrap_or(base);
        let next = zoom.apply(now, base);
        if next == now {
            return None;
        }
        self.zoom = (next != base).then_some(next);
        Some(next)
    }

    /// Sets the scale the screens are laid out and drawn at, as the
    /// application asks (#10083): `None` for the display's own.
    pub const fn set_zoom(&mut self, scale: Option<DeviceScale>) {
        self.zoom = scale;
    }

    /// Whether the UI may draw: configured, and no `Present` in flight.
    #[must_use]
    pub const fn can_draw(&self) -> bool {
        self.end.can_draw()
    }

    /// Checks and applies one record from the display, already copied out of
    /// its ring.
    ///
    /// # Errors
    ///
    /// As [`UiEnd::receive`]. A refused record changes nothing.
    pub fn receive(&mut self, record: &[u8]) -> Result<UiEvent, LoopError> {
        Ok(self.end.receive(record)?)
    }

    /// Draws `tree` whole into `frame`, its text's glyphs from `glyphs`, and
    /// returns the `Present` announcing the whole screen.
    ///
    /// # Errors
    ///
    /// [`ContractError::NotConfigured`] or [`ContractError::PresentInFlight`] if the UI
    /// may not draw yet; [`LoopError::Framebuffer`] if `frame` is shorter
    /// than the layout.
    pub fn draw_all<M: FrameMemory, const N: usize, const T: usize>(
        &mut self,
        tree: &Tree<N, T>,
        frame: M,
        glyphs: &mut Glyphs<'_>,
    ) -> Result<Present, LoopError> {
        let layout = self.end.layout().ok_or(ContractError::NotConfigured)?;
        if !self.end.can_draw() {
            return Err(ContractError::PresentInFlight.into());
        }
        let mut target = RawFramebuffer::new(layout, frame).map_err(LoopError::Framebuffer)?;
        match render(tree, &mut target, &self.theme, glyphs) {
            Ok(()) => {}
            Err(never) => match never {},
        }
        let pointer = self.pointer_for(tree);
        if let Some((at, cursor, _)) = pointer {
            match render_pointer(&mut target, at, cursor, tree.scale(), layout.bounds()) {
                Ok(()) => {}
                Err(never) => match never {},
            }
        }
        let present = self.end.present(&[layout.bounds()])?;
        self.pointer_drawn = pointer.map(|(_, cursor, area)| (area, cursor));
        Ok(present)
    }

    /// The runtime's frame clock (#10086): while anything in `tree` moves
    /// (a transition, an animation, a fling), lets `elapsed` milliseconds
    /// pass ([`Tree::advance`]) and draws what moved, giving the frame to
    /// present; `None` while nothing moves. A PD calls it each time the
    /// display has shown a frame, with the time since the last, so motion
    /// runs at the display's rate.
    ///
    /// # Errors
    ///
    /// As [`draw_dirty`](Self::draw_dirty).
    pub fn tick<M: FrameMemory, const N: usize, const T: usize>(
        &mut self,
        tree: &mut Tree<N, T>,
        frame: M,
        glyphs: &mut Glyphs<'_>,
        elapsed: u32,
    ) -> Result<Option<Present>, LoopError> {
        if !tree.is_moving() || !self.end.can_draw() {
            return Ok(None);
        }
        tree.advance(elapsed);
        self.draw_dirty(tree, frame, glyphs).map(Some)
    }

    /// Draws only the areas `tree` has marked dirty into `frame`, its text's
    /// glyphs from `glyphs`, marks
    /// them drawn ([`Tree::clear_dirty`]), and returns the `Present`
    /// announcing just those rectangles - none, if nothing changed,
    /// so the display shows the same frame and acknowledges it. The pixels
    /// are those a whole frame would have ([`render_dirty`]), so long as
    /// `frame` holds the last frame drawn: the UI's own framebuffer, which
    /// only it writes.
    ///
    /// # Errors
    ///
    /// As [`draw_all`](Self::draw_all); on an error the tree stays dirty.
    pub fn draw_dirty<M: FrameMemory, const N: usize, const T: usize>(
        &mut self,
        tree: &mut Tree<N, T>,
        frame: M,
        glyphs: &mut Glyphs<'_>,
    ) -> Result<Present, LoopError> {
        let layout = self.end.layout().ok_or(ContractError::NotConfigured)?;
        if !self.end.can_draw() {
            return Err(ContractError::PresentInFlight.into());
        }
        // A pointer that moved, showed, hid or changed shape: its old
        // area repainted without it, its new one with it.
        let pointer = self.pointer_for(tree);
        let drawn = pointer.map(|(_, cursor, area)| (area, cursor));
        if drawn != self.pointer_drawn {
            for (moved, _) in [self.pointer_drawn, drawn].into_iter().flatten() {
                tree.mark_area_dirty(moved);
            }
        }
        let mut target = RawFramebuffer::new(layout, frame).map_err(LoopError::Framebuffer)?;
        // What scrolls moved of what is drawn moves first, in order; the
        // pointer drawn over it is drawn again where it was, and where its
        // pixels went is repainted. Memory that cannot move is repainted.
        let mut moves = [nowayos_ui::scroll::Move::default(); nowayos_ui::scroll::MAX_MOVES];
        let count = moves.len().min(tree.moves().len());
        for (slot, made) in moves.iter_mut().zip(tree.moves()) {
            *slot = *made;
        }
        for made in moves.iter().take(count) {
            if !target.move_area(&to_eg(made.area), made.dx, made.dy) {
                tree.mark_area_dirty(made.area);
            }
            if let Some(drawn) = self
                .pointer_drawn
                .and_then(|(drawn, _)| drawn.intersection(&made.area))
            {
                tree.mark_area_dirty(drawn);
                tree.mark_area_dirty(Rect::from_edges(
                    drawn.left().saturating_add(made.dx),
                    drawn.top().saturating_add(made.dy),
                    drawn.right().saturating_add(made.dx),
                    drawn.bottom().saturating_add(made.dy),
                ));
            }
        }
        match render_dirty(tree, &mut target, &self.theme, glyphs) {
            Ok(()) => {}
            Err(never) => match never {},
        }
        // Each repainted area covers the pointer's pixels there: draw it
        // again, over the tree.
        if let Some((at, cursor, _)) = pointer {
            for &rect in tree.dirty_rects() {
                match render_pointer(&mut target, at, cursor, tree.scale(), rect) {
                    Ok(()) => {}
                    Err(never) => match never {},
                }
            }
        }
        // The display copies what changed: what was drawn, and every area
        // whose pixels moved - merged, as there may be more than it takes.
        let bounds = layout.bounds();
        let mut damage: DirtyRegion<MAX_DAMAGE_RECTS> = DirtyRegion::new(bounds);
        for &rect in tree.dirty_rects() {
            damage.add(rect);
        }
        for made in moves.iter().take(count) {
            damage.add(made.area);
        }
        let present = self.end.present(damage.rects())?;
        tree.clear_dirty();
        self.pointer_drawn = drawn;
        Ok(present)
    }
}

/// The UI PD's regions and rings, bound from the system description:
///
/// | Symbol | Region | Map |
/// | --- | --- | --- |
/// | `nowayos_rw_region_0` | the `Present` ring's messages | `rw` |
/// | `nowayos_ro_region_0` | the `Present` ring's control | `r` |
/// | `nowayos_rw_region_1` | the display ring's control | `rw` |
/// | `nowayos_ro_region_1` | the display ring's messages | `r` |
/// | `nowayos_rw_region_2` | the framebuffer | `rw` |
/// | `nowayos_rw_region_3` | the input ring's control | `rw` |
/// | `nowayos_ro_region_2` | the input ring's messages | `r` |
pub struct UiChannels {
    to_display: RingTx<RING_SLOTS, RING_SLOT>,
    from_display: RingRx<RING_SLOTS, RING_SLOT>,
    from_input: RingRx<INPUT_SLOTS, INPUT_SLOT>,
    frame: Frame<ReadWrite>,
}

impl UiChannels {
    /// Binds the regions above.
    ///
    /// # Errors
    ///
    /// [`LoopError::Region`] if a region is not mapped as the table says.
    pub fn bind() -> Result<Self, LoopError> {
        let to_display = ring_producer(read_write(0)?, read_only(0)?)?;
        let from_display = ring_consumer(read_write(1)?, read_only(1)?)?;
        let from_input = ring_consumer(read_write(3)?, read_only(2)?)?;
        let frame = read_write(2)?;
        let len = frame.size();
        Ok(Self {
            to_display,
            from_display,
            from_input,
            frame: frame.into_frame(len)?,
        })
    }

    /// The next input event from the input PD: `None` if there is none, and
    /// `Some(Err(..))` for a malformed record - already copied out and
    /// consumed, so the next call reads the record after it.
    ///
    /// # Errors
    ///
    /// [`LoopError::Ring`] if the ring's indices are impossible.
    pub fn next_input(&mut self) -> Result<Option<Result<InputEvent, DecodeError>>, LoopError> {
        let mut rx = Consumer::new(&self.from_input).map_err(|_: RingError| LoopError::Ring)?;
        match rx.try_recv::<InputEvent>() {
            Ok(event) => Ok(event.map(Ok)),
            Err(RecvError::Decode(error)) => Ok(Some(Err(error))),
            Err(_) => Err(LoopError::Ring),
        }
    }

    /// The framebuffer region's size in bytes.
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        self.frame.len()
    }

    /// The framebuffer, to draw into.
    pub const fn frame(&mut self) -> &mut Frame<ReadWrite> {
        &mut self.frame
    }

    /// The next record from the display, copied out of its ring, or `None`.
    ///
    /// # Errors
    ///
    /// [`LoopError::Ring`] if the ring's indices are impossible.
    pub fn next_from_display(&mut self) -> Result<Option<[u8; RING_SLOT]>, LoopError> {
        let mut rx = Consumer::new(&self.from_display).map_err(|_: RingError| LoopError::Ring)?;
        match rx.try_recv_record() {
            Ok(record) => Ok(record.map(|record| *record.as_bytes())),
            Err(_) => Err(LoopError::Ring),
        }
    }

    /// Sends `present` to the display. Notify it afterwards.
    ///
    /// # Errors
    ///
    /// [`LoopError::Ring`] if the ring is full or broken.
    pub fn send(&mut self, present: &Present) -> Result<(), LoopError> {
        let mut tx = Producer::new(&self.to_display).map_err(|_: RingError| LoopError::Ring)?;
        tx.try_send(present).map_err(|_: SendError| LoopError::Ring)
    }
}

impl fmt::Debug for UiChannels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UiChannels")
            .field("frame_len", &self.frame.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod tests {
    use nowayos_display::Rect;
    use nowayos_display::{
        Configure, DisplayEnd, FrameHash, PixelFormat, PresentDone, UiEvent as DisplayUiEvent,
    };
    use nowayos_ipc::encode;
    use nowayos_ui::Style;

    use super::*;

    /// Runs `draw` with a glyph store over fresh storage on the heap.
    fn with_glyphs<R>(draw: impl FnOnce(&mut Glyphs<'_>) -> R) -> R {
        let (mut pixels, mut slots) = (
            vec![0; nowayos_render_eg::DEFAULT_BYTES],
            vec![nowayos_render_eg::Slot::EMPTY; nowayos_render_eg::DEFAULT_SLOTS],
        );
        draw(&mut Glyphs::new(&mut pixels, &mut slots))
    }

    fn layout() -> FramebufferLayout {
        FramebufferLayout::packed(32, 16, PixelFormat::Xrgb8888).unwrap()
    }

    fn tree() -> Tree<4> {
        let mut tree = Tree::new();
        let column = tree.column(Style::new().padding(2)).unwrap();
        tree.set_root(column).unwrap();
        let text = tree.text("Hi").unwrap();
        tree.append(column, text).unwrap();
        // Laid out as the loop's theme draws it.
        let metrics = Theme::<Rgb888>::light().metrics();
        tree.layout(Rect::new(0, 0, 32, 16), &metrics).unwrap();
        tree
    }

    fn record<M: nowayos_ipc::Message>(message: &M) -> [u8; RING_SLOT] {
        let mut slot = [0; RING_SLOT];
        encode(message, &mut slot).unwrap();
        slot
    }

    #[test]
    fn configure_draw_present_and_done() {
        let mut ui = UiLoop::new(layout().byte_len());
        let mut memory = vec![0_u8; layout().byte_len()];
        assert_eq!(
            with_glyphs(|glyphs| ui.draw_all(&tree(), &mut memory[..], glyphs)).unwrap_err(),
            LoopError::Contract(ContractError::NotConfigured)
        );
        let mut display = DisplayEnd::new(layout()).unwrap();
        let configure: Configure = display.configure();
        assert_eq!(
            ui.receive(&record(&configure)).unwrap(),
            DisplayUiEvent::Configured(layout())
        );
        let present = with_glyphs(|glyphs| ui.draw_all(&tree(), &mut memory[..], glyphs)).unwrap();
        assert!(!ui.can_draw(), "one Present in flight");
        assert_eq!(
            with_glyphs(|glyphs| ui.draw_all(&tree(), &mut memory[..], glyphs)).unwrap_err(),
            LoopError::Contract(ContractError::PresentInFlight)
        );
        let shown = display.accept(&present).unwrap();
        assert_eq!(shown.spans().count(), 16, "every row of the screen");
        assert_ne!(
            FrameHash::of(&memory),
            FrameHash::of(&vec![0; layout().byte_len()])
        );
        let done: PresentDone = shown.done();
        assert_eq!(
            ui.receive(&record(&done)).unwrap(),
            DisplayUiEvent::PresentDone(0)
        );
        assert!(ui.can_draw());
    }

    /// The display's scale reaches the screens - the viewport is its
    /// layout, the metrics the theme's at its scale - and a dirty frame
    /// presents only what changed, with the pixels of a whole frame.
    #[test]
    fn a_scaled_display_lays_out_and_presents_only_what_changed() {
        let layout = FramebufferLayout::packed(64, 32, PixelFormat::Rgb565).unwrap();
        let mut ui = UiLoop::new(layout.byte_len());
        assert_eq!((ui.scale(), ui.viewport()), (None, None));
        assert_eq!(ui.metrics().scale(), DeviceScale::ONE);
        let mut display = DisplayEnd::scaled(layout, DeviceScale::TWO).unwrap();
        ui.receive(&record(&display.configure())).unwrap();
        assert_eq!(ui.scale(), Some(DeviceScale::TWO));
        assert_eq!(ui.viewport(), Some(Rect::new(0, 0, 64, 32)));
        let metrics = ui.metrics();
        assert_eq!(metrics.scale(), DeviceScale::TWO);
        assert_eq!(
            metrics,
            Theme::<Rgb888>::light().metrics_at(DeviceScale::TWO)
        );

        let mut tree: Tree<4> = Tree::new();
        let column = tree.column(Style::new().padding(2)).unwrap();
        tree.set_root(column).unwrap();
        let slot = tree.slot(nowayos_ui::SlotId::new(1)).unwrap();
        tree.append(column, slot).unwrap();
        tree.layout(ui.viewport().unwrap(), &metrics).unwrap();
        let mut memory = vec![0_u8; layout.byte_len()];
        // The first frame: the whole screen is dirty.
        let present =
            with_glyphs(|glyphs| ui.draw_dirty(&mut tree, &mut memory[..], glyphs)).unwrap();
        assert_eq!(present.damage(), &[layout.bounds()]);
        assert!(tree.dirty_rects().is_empty());
        let done = display.accept(&present).unwrap().done();
        ui.receive(&record(&done)).unwrap();
        // Nothing changed: an empty Present, acknowledged as any other.
        let present =
            with_glyphs(|glyphs| ui.draw_dirty(&mut tree, &mut memory[..], glyphs)).unwrap();
        assert!(present.damage().is_empty());
        let done = display.accept(&present).unwrap().done();
        ui.receive(&record(&done)).unwrap();
        // One slot changed: only it is drawn and presented, and the frame
        // is what a whole frame would be.
        tree.set_slot(nowayos_ui::SlotId::new(1), "Hi").unwrap();
        let changed = tree.dirty_rects().to_vec();
        let present =
            with_glyphs(|glyphs| ui.draw_dirty(&mut tree, &mut memory[..], glyphs)).unwrap();
        assert_eq!(present.damage(), &changed[..]);
        let done = display.accept(&present).unwrap().done();
        ui.receive(&record(&done)).unwrap();
        let mut whole = vec![0_u8; layout.byte_len()];
        let mut fresh = UiLoop::new(layout.byte_len());
        fresh
            .receive(&record(
                &DisplayEnd::scaled(layout, DeviceScale::TWO)
                    .unwrap()
                    .configure(),
            ))
            .unwrap();
        with_glyphs(|glyphs| fresh.draw_all(&tree, &mut whole[..], glyphs)).unwrap();
        assert_eq!(memory, whole);
    }

    /// Zoom (#10083) steps the scale the screens are laid out at a quarter
    /// at a time, within 1 to 4, and back to the display's own.
    #[test]
    fn zoom_steps_the_scale_by_quarters_and_back() {
        let layout = FramebufferLayout::packed(64, 32, PixelFormat::Rgb565).unwrap();
        let mut ui = UiLoop::new(layout.byte_len());
        assert_eq!(
            ui.zoom(Zoom::In),
            None,
            "nothing to zoom before the display"
        );
        let display = DisplayEnd::scaled(layout, DeviceScale::TWO).unwrap();
        ui.receive(&record(&display.configure())).unwrap();
        assert_eq!(ui.zoom(Zoom::In), DeviceScale::from_quarters(9));
        assert_eq!(ui.metrics().scale(), DeviceScale::from_quarters(9).unwrap());
        assert_eq!(ui.zoom(Zoom::Reset), Some(DeviceScale::TWO));
        assert_eq!(ui.zoom(Zoom::Reset), None);
        for _ in 0..4 {
            ui.zoom(Zoom::Out);
        }
        assert_eq!(ui.metrics().scale(), DeviceScale::ONE);
        assert_eq!(ui.zoom(Zoom::Out), None, "1 is the least");
        ui.set_zoom(Some(DeviceScale::MAX));
        assert_eq!(ui.zoom(Zoom::In), None, "4 is the most");
        assert_eq!(ui.scale(), Some(DeviceScale::TWO), "the display's own");
    }

    /// Draws what changed, as the display shows it, and returns the damage.
    fn shown(
        ui: &mut UiLoop,
        display: &mut DisplayEnd,
        tree: &mut Tree<4>,
        memory: &mut [u8],
    ) -> Vec<Rect> {
        let present = with_glyphs(|glyphs| ui.draw_dirty(tree, memory, glyphs)).unwrap();
        let damage = present.damage().to_vec();
        let done = display.accept(&present).unwrap().done();
        ui.receive(&record(&done)).unwrap();
        damage
    }

    /// `tree` drawn whole by a fresh loop, with the pointer or without.
    fn whole(tree: &Tree<4>, layout: FramebufferLayout, pointer: bool) -> Vec<u8> {
        let mut ui = UiLoop::new(layout.byte_len());
        if pointer {
            ui = ui.with_pointer();
        }
        ui.receive(&record(&DisplayEnd::new(layout).unwrap().configure()))
            .unwrap();
        let mut memory = vec![0_u8; layout.byte_len()];
        with_glyphs(|glyphs| ui.draw_all(tree, &mut memory[..], glyphs)).unwrap();
        memory
    }

    /// The frame clock (#10086): nothing to draw while nothing moves; while
    /// a fade runs, a frame each tick, the last as a whole frame would be.
    #[test]
    fn the_frame_clock_draws_while_something_moves() {
        let layout = FramebufferLayout::packed(64, 48, PixelFormat::Xrgb8888).unwrap();
        let mut display = DisplayEnd::new(layout).unwrap();
        let mut ui = UiLoop::new(layout.byte_len());
        ui.receive(&record(&display.configure())).unwrap();
        let mut tree: Tree<4> = Tree::new();
        let column = tree.column(Style::new()).unwrap();
        tree.set_root(column).unwrap();
        tree.layout(layout.bounds(), &ui.metrics()).unwrap();
        let mut memory = vec![0_u8; layout.byte_len()];
        shown(&mut ui, &mut display, &mut tree, &mut memory);
        let idle = with_glyphs(|glyphs| ui.tick(&mut tree, &mut memory[..], glyphs, 16)).unwrap();
        assert!(idle.is_none(), "nothing moves");
        tree.set_motion(nowayos_ui::motion::MotionSetting::Full);
        tree.animate(column, nowayos_ui::motion::Animation::fade_in(48))
            .unwrap();
        let mut frames = 0;
        while let Some(present) =
            with_glyphs(|glyphs| ui.tick(&mut tree, &mut memory[..], glyphs, 16)).unwrap()
        {
            let done = display.accept(&present).unwrap().done();
            ui.receive(&record(&done)).unwrap();
            frames += 1;
        }
        assert_eq!(frames, 3, "48 ms at 16 a frame");
        assert_eq!(memory, whole(&tree, layout, false));
    }

    /// The pointer is drawn where the mouse last moved, a move
    /// repaints only its old and new areas, and the frame is what a whole
    /// frame with the pointer would be; a touch hides it.
    #[test]
    fn the_pointer_follows_the_mouse_and_a_touch_hides_it() {
        let layout = FramebufferLayout::packed(64, 48, PixelFormat::Xrgb8888).unwrap();
        let mut display = DisplayEnd::new(layout).unwrap();
        let mut ui = UiLoop::new(layout.byte_len()).with_pointer();
        ui.receive(&record(&display.configure())).unwrap();
        let mut tree: Tree<4> = Tree::new();
        let column = tree.column(Style::new()).unwrap();
        tree.set_root(column).unwrap();
        tree.layout(layout.bounds(), &ui.metrics()).unwrap();
        let mut memory = vec![0_u8; layout.byte_len()];

        // No move yet: no pointer.
        assert_eq!(
            shown(&mut ui, &mut display, &mut tree, &mut memory),
            [layout.bounds()]
        );
        assert_eq!(memory, whole(&tree, layout, false));

        // A move: only the pointer's area is drawn, and it shows.
        tree.handle(InputEvent::PointerMove { x: 10, y: 5 });
        assert_eq!(
            shown(&mut ui, &mut display, &mut tree, &mut memory),
            [Rect::new(10, 5, 12, 19)]
        );
        assert_eq!(memory, whole(&tree, layout, true));
        assert_ne!(memory, whole(&tree, layout, false));

        // Another: the old and new areas, merged as they overlap.
        tree.handle(InputEvent::PointerMove { x: 14, y: 9 });
        assert_eq!(
            shown(&mut ui, &mut display, &mut tree, &mut memory),
            [Rect::new(10, 5, 16, 23)]
        );
        assert_eq!(memory, whole(&tree, layout, true));

        // Nothing moved: nothing drawn.
        assert!(shown(&mut ui, &mut display, &mut tree, &mut memory).is_empty());

        // A touch: the pointer's area repainted without it.
        tree.handle(InputEvent::Touch {
            id: 0,
            x: 40,
            y: 30,
            phase: nowayos_input::TouchPhase::Start,
        });
        assert_eq!(
            shown(&mut ui, &mut display, &mut tree, &mut memory),
            [Rect::new(14, 9, 12, 19)]
        );
        assert_eq!(memory, whole(&tree, layout, false));
    }

    /// Any theme draws at any device scale the contract allows - its text
    /// from the glyph outlines - so a display at 2.5 is accepted and its
    /// frames drawn.
    #[test]
    fn every_scale_the_contract_allows_draws() {
        let layout = FramebufferLayout::packed(80, 40, PixelFormat::Xrgb8888).unwrap();
        for quarters in 4..=16 {
            let scale = DeviceScale::from_quarters(quarters).unwrap();
            let mut ui = UiLoop::new(layout.byte_len());
            let display = DisplayEnd::scaled(layout, scale).unwrap();
            ui.receive(&record(&display.configure())).unwrap();
            assert_eq!(ui.scale(), Some(scale));
            assert_eq!(ui.metrics(), Theme::<Rgb888>::light().metrics_at(scale));
            let mut memory = vec![0_u8; layout.byte_len()];
            assert!(with_glyphs(|glyphs| ui.draw_all(&tree(), &mut memory[..], glyphs)).is_ok());
        }
    }

    /// A theme switched at run time gives its metrics at the
    /// display's scale, and draws a whole frame in its own colours: the
    /// pixels a loop started in it draws.
    #[test]
    fn a_theme_switched_at_run_time_lays_out_and_draws_in_its_own() {
        let layout = FramebufferLayout::packed(64, 32, PixelFormat::Rgb565).unwrap();
        let mut ui = UiLoop::new(layout.byte_len());
        let mut display = DisplayEnd::scaled(layout, DeviceScale::TWO).unwrap();
        ui.receive(&record(&display.configure())).unwrap();
        let large = Theme::<Rgb888>::high_contrast();
        ui.set_theme(large);
        assert_eq!(ui.metrics(), large.metrics_at(DeviceScale::TWO));
        let mut tree = tree();
        tree.layout(ui.viewport().unwrap(), &ui.metrics()).unwrap();
        let mut memory = vec![0_u8; layout.byte_len()];
        let present = with_glyphs(|glyphs| ui.draw_all(&tree, &mut memory[..], glyphs)).unwrap();
        ui.receive(&record(&display.accept(&present).unwrap().done()))
            .unwrap();
        let mut fresh = UiLoop::with_theme(layout.byte_len(), large);
        fresh
            .receive(&record(
                &DisplayEnd::scaled(layout, DeviceScale::TWO)
                    .unwrap()
                    .configure(),
            ))
            .unwrap();
        let mut whole = vec![0_u8; layout.byte_len()];
        with_glyphs(|glyphs| fresh.draw_all(&tree, &mut whole[..], glyphs)).unwrap();
        assert_eq!(memory, whole);
        // Black, the large-text theme's background, where the light
        // theme's is a warm white.
        assert_eq!(memory.get(..2), Some(&[0, 0][..]));
    }

    #[test]
    fn a_layout_bigger_than_the_region_is_refused() {
        let mut ui = UiLoop::new(layout().byte_len() - 1);
        let display = DisplayEnd::new(layout()).unwrap();
        assert!(matches!(
            ui.receive(&record(&display.configure())),
            Err(LoopError::Contract(ContractError::ExceedsRegion { .. }))
        ));
        assert_eq!(ui.layout(), None);
    }
}
