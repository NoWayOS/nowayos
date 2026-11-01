// SPDX-License-Identifier: MIT OR Apache-2.0
//! The two ends of the UI-to-display contract: [`UiEnd`] in the UI PD and
//! [`DisplayEnd`] in the display PD. Each keeps the sequence state and
//! checks every record it receives, so neither PD trusts the other.

use core::ops::Range;

use nowayos_bounded::BoundedVec;
use nowayos_ipc::{Header, decode};

use crate::contract::{
    CONFIGURE_KIND, Configure, ContractError, MAX_DAMAGE_RECTS, PRESENT_DONE_KIND, PRESENT_KIND,
    Present, PresentDone,
};
use crate::{FramebufferLayout, Rect};

/// What the UI end accepted from a display record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    /// The display's layout fits the region: the UI may draw in it.
    Configured(FramebufferLayout),
    /// The Present with this sequence has been shown: the UI may draw again.
    PresentDone(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiState {
    Unconfigured,
    Ready(FramebufferLayout),
    Presenting {
        layout: FramebufferLayout,
        sequence: u32,
    },
}

/// The UI PD's end of the contract.
///
/// It holds the size of the UI's framebuffer region, accepts the display's
/// layout only if it fits, and allows drawing only between a layout and the
/// next [`present`](Self::present), or between a [`PresentDone`] and the
/// next present.
///
/// ```
/// use nowayos_display::{
///     DisplayEnd, FramebufferLayout, PixelFormat, Rect, UiEnd, UiEvent,
/// };
/// use nowayos_ipc::encode;
///
/// let layout = FramebufferLayout::packed(320, 240, PixelFormat::Xrgb8888).unwrap();
/// let mut display = DisplayEnd::new(layout).unwrap();
/// let mut ui = UiEnd::new(layout.byte_len());
/// let mut slot = [0u8; nowayos_display::MAX_RECORD_LEN];
///
/// encode(&display.configure(), &mut slot).unwrap();
/// assert_eq!(ui.receive(&slot), Ok(UiEvent::Configured(layout)));
///
/// assert!(ui.can_draw());
/// let present = ui.present(&[Rect::new(10, 10, 50, 20)]).unwrap();
/// assert!(!ui.can_draw()); // until the display is done
///
/// encode(&present, &mut slot).unwrap();
/// let shown = display.receive(&slot).unwrap();
/// assert_eq!(shown.spans().count(), 20); // one byte range per damaged row
///
/// encode(&shown.done(), &mut slot).unwrap();
/// assert_eq!(ui.receive(&slot), Ok(UiEvent::PresentDone(0)));
/// assert!(ui.can_draw());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiEnd {
    region_len: usize,
    state: UiState,
    /// The display's device scale, once its layout is accepted.
    scale: u32,
    next_sequence: u32,
}

impl UiEnd {
    /// The UI's end, drawing into a framebuffer region of `region_len`
    /// bytes. It cannot draw until a layout arrives.
    #[must_use]
    pub const fn new(region_len: usize) -> Self {
        Self {
            region_len,
            state: UiState::Unconfigured,
            scale: 0,
            next_sequence: 0,
        }
    }

    /// Checks and applies one record from the display, already copied out
    /// of the ring.
    ///
    /// # Errors
    ///
    /// [`ContractError::Decode`] for a malformed record — including an
    /// unknown pixel format — and [`ContractError::UnexpectedKind`] for a
    /// kind the display never sends; otherwise as for
    /// [`configure`](Self::configure) or [`present_done`](Self::present_done).
    /// A refused record changes nothing.
    pub fn receive(&mut self, record: &[u8]) -> Result<UiEvent, ContractError> {
        match Header::parse(record)?.kind {
            CONFIGURE_KIND => self.configure(decode(record)?).map(UiEvent::Configured),
            PRESENT_DONE_KIND => self.present_done(decode(record)?).map(UiEvent::PresentDone),
            kind => Err(ContractError::UnexpectedKind { kind }),
        }
    }

    /// Accepts the display's layout if it fits the region.
    ///
    /// # Errors
    ///
    /// [`ContractError::ExceedsRegion`] if the layout needs more bytes than
    /// the region holds, or [`ContractError::AlreadyConfigured`]. Either
    /// way the UI stays as it was, so with no layout it does not draw.
    pub fn configure(&mut self, message: Configure) -> Result<FramebufferLayout, ContractError> {
        if self.state != UiState::Unconfigured {
            return Err(ContractError::AlreadyConfigured);
        }
        let layout = message.layout();
        if layout.byte_len() > self.region_len {
            return Err(ContractError::ExceedsRegion {
                needed: layout.byte_len(),
                region: self.region_len,
            });
        }
        self.state = UiState::Ready(layout);
        self.scale = message.scale();
        Ok(layout)
    }

    /// Accepts the display's acknowledgement of the Present in flight,
    /// returning its sequence.
    ///
    /// # Errors
    ///
    /// [`ContractError::OutOfSequence`] if it acknowledges another
    /// sequence, or [`ContractError::NoPresentInFlight`].
    pub fn present_done(&mut self, message: PresentDone) -> Result<u32, ContractError> {
        match self.state {
            UiState::Presenting { layout, sequence } if sequence == message.sequence => {
                self.state = UiState::Ready(layout);
                Ok(sequence)
            }
            UiState::Presenting { sequence, .. } => Err(ContractError::OutOfSequence {
                expected: sequence,
                found: message.sequence,
            }),
            UiState::Unconfigured | UiState::Ready(_) => Err(ContractError::NoPresentInFlight {
                found: message.sequence,
            }),
        }
    }

    /// The accepted layout, if any.
    #[must_use]
    pub const fn layout(&self) -> Option<FramebufferLayout> {
        match self.state {
            UiState::Unconfigured => None,
            UiState::Ready(layout) | UiState::Presenting { layout, .. } => Some(layout),
        }
    }

    /// The display's device scale - its pixels each way for each layout
    /// unit, 1 to [`MAX_SCALE`](crate::MAX_SCALE) - once its layout is
    /// accepted. The UI lays its screens out in the layout's size
    /// divided by it, and draws them at the layout's own pixels.
    #[must_use]
    pub const fn scale(&self) -> Option<u32> {
        match self.state {
            UiState::Unconfigured => None,
            UiState::Ready(_) | UiState::Presenting { .. } => Some(self.scale),
        }
    }

    /// Whether the UI may write to the framebuffer now: a layout has been
    /// accepted and no Present is in flight.
    #[must_use]
    pub const fn can_draw(&self) -> bool {
        matches!(self.state, UiState::Ready(_))
    }

    /// Ends a frame: returns the [`Present`] to send for `damage`, and
    /// stops drawing until the display acknowledges it. Pass the rectangles
    /// of a [`DirtyRegion`](crate::DirtyRegion) of
    /// [`MAX_DAMAGE_RECTS`] whose bounds are the layout's.
    ///
    /// # Errors
    ///
    /// [`ContractError::NotConfigured`], [`ContractError::PresentInFlight`],
    /// [`ContractError::OutsideLayout`], or as for [`Present::new`]. A
    /// refused call changes nothing.
    pub fn present(&mut self, damage: &[Rect]) -> Result<Present, ContractError> {
        let layout = match self.state {
            UiState::Ready(layout) => layout,
            UiState::Unconfigured => return Err(ContractError::NotConfigured),
            UiState::Presenting { .. } => return Err(ContractError::PresentInFlight),
        };
        let present = Present::new(self.next_sequence, damage)?;
        check_inside(&layout, present.damage())?;
        self.state = UiState::Presenting {
            layout,
            sequence: present.sequence(),
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Ok(present)
    }
}

/// Fails on the first rectangle not wholly inside `layout`.
fn check_inside(layout: &FramebufferLayout, damage: &[Rect]) -> Result<(), ContractError> {
    let bounds = layout.bounds();
    match damage.iter().position(|rect| !bounds.contains_rect(rect)) {
        Some(index) => Err(ContractError::OutsideLayout { index }),
        None => Ok(()),
    }
}

/// The display PD's end of the contract.
///
/// It announces the layout and accepts only the Present it expects next
/// whose rectangles all lie inside that layout. An accepted Present is the
/// only thing that says which framebuffer bytes to read, so a refused one
/// reads and copies nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayEnd {
    configure: Configure,
    expected: u32,
}

impl DisplayEnd {
    /// The display's end, showing `layout` at scale 1.
    ///
    /// # Errors
    ///
    /// As for [`Configure::new`].
    pub const fn new(layout: FramebufferLayout) -> Result<Self, ContractError> {
        Self::scaled(layout, 1)
    }

    /// The display's end, showing `layout` at device scale `scale`: what
    /// the UI lays out in units of `scale` pixels each way.
    ///
    /// # Errors
    ///
    /// As for [`Configure::scaled`].
    pub const fn scaled(layout: FramebufferLayout, scale: u32) -> Result<Self, ContractError> {
        match Configure::scaled(layout, scale) {
            Ok(configure) => Ok(Self {
                configure,
                expected: 0,
            }),
            Err(err) => Err(err),
        }
    }

    /// The [`Configure`] message to send the UI first.
    #[must_use]
    pub const fn configure(&self) -> Configure {
        self.configure
    }

    /// The layout shown.
    #[must_use]
    pub const fn layout(&self) -> FramebufferLayout {
        self.configure.layout()
    }

    /// The device scale announced.
    #[must_use]
    pub const fn scale(&self) -> u32 {
        self.configure.scale()
    }

    /// The sequence the next Present must carry.
    #[must_use]
    pub const fn expected_sequence(&self) -> u32 {
        self.expected
    }

    /// Checks one record from the UI, already copied out of the ring.
    ///
    /// # Errors
    ///
    /// [`ContractError::Decode`] for a malformed record — including one
    /// over the rectangle bound — [`ContractError::UnexpectedKind`] for any
    /// kind but a Present, or as for [`accept`](Self::accept).
    pub fn receive(&mut self, record: &[u8]) -> Result<ValidPresent, ContractError> {
        match Header::parse(record)?.kind {
            PRESENT_KIND => self.accept(&decode(record)?),
            kind => Err(ContractError::UnexpectedKind { kind }),
        }
    }

    /// Checks a decoded Present against the expected sequence and the
    /// layout.
    ///
    /// # Errors
    ///
    /// [`ContractError::OutOfSequence`] or [`ContractError::OutsideLayout`].
    /// A refused Present changes nothing: the same sequence is still
    /// expected.
    pub fn accept(&mut self, present: &Present) -> Result<ValidPresent, ContractError> {
        if present.sequence() != self.expected {
            return Err(ContractError::OutOfSequence {
                expected: self.expected,
                found: present.sequence(),
            });
        }
        let layout = self.layout();
        check_inside(&layout, present.damage())?;
        let damage =
            BoundedVec::from_slice(present.damage()).map_err(|_| ContractError::TooManyRects {
                count: present.damage().len(),
            })?;
        self.expected = self.expected.wrapping_add(1);
        Ok(ValidPresent {
            layout,
            sequence: present.sequence(),
            damage,
        })
    }
}

/// A Present the display has accepted: its rectangles lie inside the
/// layout, so the byte ranges they cover lie inside the framebuffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidPresent {
    layout: FramebufferLayout,
    sequence: u32,
    damage: BoundedVec<Rect, MAX_DAMAGE_RECTS>,
}

impl ValidPresent {
    /// The sequence accepted.
    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// The rectangles to show, each inside the layout.
    #[must_use]
    pub fn damage(&self) -> &[Rect] {
        self.damage.as_slice()
    }

    /// The acknowledgement to send once the pixels have been read.
    #[must_use]
    pub const fn done(&self) -> PresentDone {
        PresentDone {
            sequence: self.sequence,
        }
    }

    /// The framebuffer byte ranges to read: one per damaged row of each
    /// rectangle, in order. Every range lies within the layout's
    /// [`byte_len`](FramebufferLayout::byte_len). Overlapping rectangles
    /// give overlapping ranges.
    #[must_use]
    pub const fn spans(&self) -> Spans<'_> {
        Spans {
            present: self,
            rect: 0,
            row: 0,
        }
    }
}

/// The byte ranges of a [`ValidPresent`]; see [`ValidPresent::spans`].
#[derive(Debug, Clone)]
pub struct Spans<'a> {
    present: &'a ValidPresent,
    rect: usize,
    row: u32,
}

impl Iterator for Spans<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let layout = &self.present.layout;
        loop {
            let rect = self.present.damage.get(self.rect)?;
            if self.row < rect.size.height {
                // The rectangle is inside the layout, so none of these
                // checked steps can fail.
                let x = u32::try_from(rect.origin.x).ok()?;
                let y = u32::try_from(rect.origin.y).ok()?.checked_add(self.row)?;
                let start = layout.pixel_offset(x, y)?;
                let len = u64::from(rect.size.width)
                    .checked_mul(u64::from(layout.format().bytes_per_pixel()))?;
                let end = start.checked_add(usize::try_from(len).ok()?)?;
                self.row = self.row.checked_add(1)?;
                return Some(start..end);
            }
            self.rect = self.rect.checked_add(1)?;
            self.row = 0;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::PixelFormat;
    use crate::contract::{MAX_DIMENSION, MAX_RECORD_LEN};
    use nowayos_ipc::{DecodeError, HEADER_LEN, Message, encode};

    fn layout() -> FramebufferLayout {
        // 16 x 8 pixels of RGB565 with 4 bytes of row padding.
        FramebufferLayout::new(16, 8, 36, PixelFormat::Rgb565).unwrap()
    }

    fn record<M: Message>(message: &M) -> [u8; MAX_RECORD_LEN] {
        let mut buf = [0u8; MAX_RECORD_LEN];
        encode(message, &mut buf).unwrap();
        buf
    }

    fn configured() -> (UiEnd, DisplayEnd) {
        let display = DisplayEnd::new(layout()).unwrap();
        let mut ui = UiEnd::new(layout().byte_len());
        ui.receive(&record(&display.configure())).unwrap();
        (ui, display)
    }

    /// What a display PD does with a record: copy the accepted spans from
    /// the UI's framebuffer to its own, and nothing otherwise.
    fn show(
        display: &mut DisplayEnd,
        record: &[u8],
        source: &[u8],
        target: &mut [u8],
    ) -> Result<PresentDone, ContractError> {
        let present = display.receive(record)?;
        for span in present.spans() {
            target[span.clone()].copy_from_slice(&source[span]);
        }
        Ok(present.done())
    }

    #[test]
    fn a_full_exchange_copies_only_the_damage() {
        let (mut ui, mut display) = configured();
        let source: Vec<u8> = (0..layout().byte_len())
            .map(|i| (i % 251) as u8 + 1)
            .collect();
        let mut target = vec![0u8; layout().byte_len()];
        let damage = Rect::new(2, 3, 4, 2);
        let present = ui.present(&[damage]).unwrap();
        let done = show(&mut display, &record(&present), &source, &mut target).unwrap();
        assert_eq!(ui.receive(&record(&done)), Ok(UiEvent::PresentDone(0)));
        for y in 0..8 {
            for x in 0..16 {
                let at = layout().pixel_offset(x, y).unwrap();
                let inside = damage.contains(crate::Point::new(x as i32, y as i32));
                let copied = target[at..at + 2] == source[at..at + 2];
                assert_eq!(copied, inside, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn spans_follow_the_stride() {
        let (mut ui, mut display) = configured();
        let present = ui
            .present(&[Rect::new(1, 2, 3, 2), Rect::new(15, 7, 1, 1)])
            .unwrap();
        let accepted = display.receive(&record(&present)).unwrap();
        let spans: Vec<_> = accepted.spans().collect();
        assert_eq!(spans, [74..80, 110..116, 282..284]);
        assert_eq!(spans.last().unwrap().end, layout().byte_len() - 4);
    }

    #[test]
    fn rejected_presents_copy_nothing_and_keep_the_sequence() {
        let (_, mut display) = configured();
        let source = vec![0xAAu8; layout().byte_len()];
        let mut target = vec![0u8; layout().byte_len()];
        let rect = Rect::new(0, 0, 1, 1);
        let mut over = record(&Present::new(0, &[]).unwrap());
        over[HEADER_LEN + 4] = 9;
        let refused = [
            // Out of sequence.
            (
                record(&Present::new(5, &[rect]).unwrap()),
                ContractError::OutOfSequence {
                    expected: 0,
                    found: 5,
                },
            ),
            // Outside the layout: off the right edge, then below the bottom.
            (
                record(&Present::new(0, &[rect, Rect::new(10, 0, 7, 1)]).unwrap()),
                ContractError::OutsideLayout { index: 1 },
            ),
            (
                record(&Present::new(0, &[Rect::new(0, 8, 1, 1)]).unwrap()),
                ContractError::OutsideLayout { index: 0 },
            ),
            // Over the rectangle bound.
            (
                over,
                ContractError::Decode(DecodeError::InvalidValue { field: "count" }),
            ),
            // Not a Present at all.
            (
                record(&PresentDone { sequence: 0 }),
                ContractError::UnexpectedKind {
                    kind: PRESENT_DONE_KIND,
                },
            ),
        ];
        for (bytes, error) in refused {
            assert_eq!(show(&mut display, &bytes, &source, &mut target), Err(error));
        }
        assert!(target.iter().all(|&b| b == 0), "nothing was copied");
        assert_eq!(display.expected_sequence(), 0);
        // The expected Present is still accepted afterwards.
        let good = record(&Present::new(0, &[Rect::new(0, 0, 16, 8)]).unwrap());
        assert!(show(&mut display, &good, &source, &mut target).is_ok());
        assert!(target[..32].iter().all(|&b| b == 0xAA));
        assert_eq!(display.expected_sequence(), 1);
    }

    #[test]
    fn a_layout_larger_than_the_region_is_refused_and_nothing_is_drawn() {
        let display = DisplayEnd::new(layout()).unwrap();
        let mut ui = UiEnd::new(layout().byte_len() - 1);
        assert_eq!(
            ui.receive(&record(&display.configure())),
            Err(ContractError::ExceedsRegion {
                needed: 288,
                region: 287
            })
        );
        assert_eq!(ui.layout(), None);
        assert!(!ui.can_draw());
        assert_eq!(ui.present(&[]), Err(ContractError::NotConfigured));
    }

    #[test]
    fn an_unknown_pixel_format_is_refused_and_nothing_is_drawn() {
        let display = DisplayEnd::new(layout()).unwrap();
        let mut ui = UiEnd::new(1 << 20);
        let mut bytes = record(&display.configure());
        bytes[HEADER_LEN + 8] = 0x2A;
        assert_eq!(
            ui.receive(&bytes),
            Err(ContractError::Decode(DecodeError::InvalidValue {
                field: "format"
            }))
        );
        assert!(!ui.can_draw());
        assert_eq!(ui.present(&[]), Err(ContractError::NotConfigured));
    }

    #[test]
    fn the_ui_end_enforces_one_present_in_flight() {
        let (mut ui, _) = configured();
        let display = DisplayEnd::new(layout()).unwrap();
        assert_eq!(
            ui.receive(&record(&display.configure())),
            Err(ContractError::AlreadyConfigured)
        );
        assert_eq!(
            ui.receive(&record(&PresentDone { sequence: 0 })),
            Err(ContractError::NoPresentInFlight { found: 0 })
        );
        assert_eq!(ui.present(&[Rect::new(0, 0, 1, 1)]).unwrap().sequence(), 0);
        assert_eq!(ui.present(&[]), Err(ContractError::PresentInFlight));
        assert_eq!(
            ui.receive(&record(&PresentDone { sequence: 1 })),
            Err(ContractError::OutOfSequence {
                expected: 0,
                found: 1
            })
        );
        assert!(!ui.can_draw());
        assert_eq!(
            ui.receive(&record(&PresentDone { sequence: 0 })),
            Ok(UiEvent::PresentDone(0))
        );
        assert_eq!(ui.present(&[]).unwrap().sequence(), 1);
    }

    #[test]
    fn the_ui_end_refuses_damage_outside_the_layout_without_sending() {
        let (mut ui, _) = configured();
        assert_eq!(
            ui.present(&[Rect::new(0, 0, 17, 1)]),
            Err(ContractError::OutsideLayout { index: 0 })
        );
        assert_eq!(
            ui.present(&[Rect::new(0, 0, 0, 1)]),
            Err(ContractError::EmptyRect { index: 0 })
        );
        assert!(ui.can_draw());
        assert_eq!(ui.present(&[]).unwrap().sequence(), 0);
    }

    #[test]
    fn the_ui_end_refuses_kinds_the_display_never_sends() {
        let (mut ui, _) = configured();
        let present = Present::new(0, &[]).unwrap();
        assert_eq!(
            ui.receive(&record(&present)),
            Err(ContractError::UnexpectedKind { kind: PRESENT_KIND })
        );
        assert!(matches!(
            ui.receive(&[0u8; 4]),
            Err(ContractError::Decode(DecodeError::Truncated { .. }))
        ));
    }

    #[test]
    fn sequences_wrap() {
        let (mut ui, mut display) = configured();
        ui.next_sequence = u32::MAX;
        display.expected = u32::MAX;
        for expected in [u32::MAX, 0] {
            let present = ui.present(&[]).unwrap();
            assert_eq!(present.sequence(), expected);
            let done = display.receive(&record(&present)).unwrap().done();
            assert_eq!(
                ui.receive(&record(&done)),
                Ok(UiEvent::PresentDone(expected))
            );
        }
    }

    #[test]
    fn the_ui_end_learns_the_displays_scale_with_its_layout() {
        let display = DisplayEnd::scaled(layout(), 2).unwrap();
        assert_eq!(display.scale(), 2);
        let mut ui = UiEnd::new(layout().byte_len());
        assert_eq!(ui.scale(), None, "no scale before a layout");
        assert_eq!(
            ui.receive(&record(&display.configure())),
            Ok(UiEvent::Configured(layout()))
        );
        assert_eq!(ui.scale(), Some(2));
        assert_eq!(ui.present(&[]).unwrap().sequence(), 0);
        assert_eq!(ui.scale(), Some(2), "kept while a Present is in flight");
        // A refused layout leaves no scale behind.
        let mut small = UiEnd::new(layout().byte_len() - 1);
        assert!(small.receive(&record(&display.configure())).is_err());
        assert_eq!(small.scale(), None);
        assert_eq!(
            DisplayEnd::scaled(layout(), 3),
            Err(ContractError::BadScale { scale: 3 })
        );
    }

    #[test]
    fn the_display_end_refuses_layouts_the_wire_cannot_carry() {
        let wide = FramebufferLayout::packed(MAX_DIMENSION + 1, 1, PixelFormat::Rgb565).unwrap();
        assert!(DisplayEnd::new(wide).is_err());
    }
}
