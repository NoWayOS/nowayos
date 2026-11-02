// SPDX-License-Identifier: MIT OR Apache-2.0
//! The UI-to-display message contract, v0: the records the UI protection
//! domain and a display protection domain exchange.
//!
//! The UI never holds a device capability.
//! It draws into a framebuffer region it owns, mapped read-write
//! into the UI PD and **read-only** into the display PD, and tells the
//! display which parts changed. The display PD shows them — by copying them
//! to the device's scanout buffer, or by flushing them — and acknowledges.
//! QEMU's display now and the Pi's later sit behind the same three messages.
//!
//! # The system
//!
//! | Resource | UI PD | Display PD |
//! | --- | --- | --- |
//! | Framebuffer region | read-write | read-only |
//! | UI → display ring: [`Present`] | producer | consumer |
//! | Display → UI ring: [`Configure`], [`PresentDone`] | consumer | producer |
//! | One channel between them | notifies after sending | notifies after sending |
//!
//! # The exchange
//!
//! 1. The display sends [`Configure`]: the [`FramebufferLayout`] it shows,
//!    and its device scale - how many of its pixels make one layout unit
//!    each way. The UI checks that the layout fits its region and
//!    refuses to draw otherwise ([`UiEnd::receive`](crate::UiEnd::receive)),
//!    and lays its screens out in the layout's size divided by the scale:
//!    the Pi 5's 1920 x 1080 window at scale 2 is a 960 x 540 screen, drawn
//!    at the monitor's own pixels.
//! 2. The UI draws, then sends [`Present`]: a sequence number and up to
//!    [`MAX_DAMAGE_RECTS`] rectangles that changed.
//! 3. The display checks the sequence and that every rectangle lies inside
//!    the layout ([`DisplayEnd::receive`](crate::DisplayEnd::receive)). Only
//!    an accepted Present says which framebuffer bytes to read; anything
//!    else is a typed error and nothing is read or copied.
//! 4. The display sends [`PresentDone`] with the same sequence. The UI does
//!    not draw while a Present is in flight, so the display never reads a
//!    half-drawn frame. A UI that draws anyway garbles only its own pixels:
//!    the display copies them and never interprets them.
//!
//! One Present is in flight at a time, so the UI → display ring never holds
//! more than one record and the display → UI ring never more than two. The
//! first Present carries sequence 0; sequences wrap at 2³².
//!
//! # v0 limits
//!
//! * One [`Configure`] per session: the layout does not change. A mode
//!   change needs a new session, which v0 does not define. Neither PD
//!   restarts on its own. In a product, the fault policy
//!   restarts the whole system on a fault, so the session starts again with
//!   it; in today's demo and test systems the faulted PD stays stopped.
//! * A rejected Present is not acknowledged. Only a faulty UI sends one; the
//!   display reports it and keeps waiting for the expected sequence.
//! * Dimensions are at most [`MAX_DIMENSION`] pixels, as for input
//!   (`nowayos-input`'s limits).
//!
//! Every record is a fixed-size `nowayos-ipc` [`Message`], validated field
//! by field on receipt; the wire layout of each is in its documentation.

use core::fmt;

use nowayos_bounded::BoundedVec;
use nowayos_ipc::{DecodeError, EncodeError, Message, Reader, Writer, encoded_len};

use crate::{FramebufferLayout, LayoutError, PixelFormat, Rect};

/// Message kind of [`Configure`] on the display → UI ring.
pub const CONFIGURE_KIND: u16 = 0x0201;
/// Message kind of [`Present`] on the UI → display ring.
pub const PRESENT_KIND: u16 = 0x0202;
/// Message kind of [`PresentDone`] on the display → UI ring.
pub const PRESENT_DONE_KIND: u16 = 0x0203;

/// The most damage rectangles one [`Present`] carries. The UI keeps its
/// changes in a [`DirtyRegion`](crate::DirtyRegion) of this size, which
/// merges rectangles rather than exceed it.
pub const MAX_DAMAGE_RECTS: usize = 8;

/// The largest width or height, in pixels, the contract can describe.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

/// The largest device scale a [`Configure`] may carry: 2, the Pi 5's. A
/// theme names an atlas for each size of its type scale at every scale up
/// to this one, and its tests check it does, so a scale with no atlas to
/// draw it is refused at the contract rather than drawn smaller.
pub const MAX_SCALE: u32 = 2;

/// The longest record in the contract (a [`Present`]): ring slots on both
/// rings must be at least this long.
pub const MAX_RECORD_LEN: usize = encoded_len::<Present>();

/// Why a contract message or exchange was refused. Nothing in the contract
/// panics instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
    /// A record is malformed: see the [`DecodeError`].
    Decode(DecodeError),
    /// A well-formed record of a kind this end never receives.
    UnexpectedKind {
        /// The kind in the record's header.
        kind: u16,
    },
    /// A layout wider or taller than [`MAX_DIMENSION`].
    LayoutTooLarge {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// The display's layout needs more bytes than the UI's framebuffer
    /// region holds, so the UI will not draw.
    ExceedsRegion {
        /// Bytes the layout needs.
        needed: usize,
        /// Bytes in the region.
        region: usize,
    },
    /// A device scale of 0 or above [`MAX_SCALE`], or one that does not
    /// divide the layout's width and height into whole layout units.
    BadScale {
        /// The scale given.
        scale: u32,
    },
    /// A second [`Configure`] arrived; v0 has one per session.
    AlreadyConfigured,
    /// The UI was asked to present before any layout was accepted.
    NotConfigured,
    /// The UI was asked to present while a Present is still in flight.
    PresentInFlight,
    /// A [`PresentDone`] arrived with no Present in flight.
    NoPresentInFlight {
        /// The sequence it acknowledged.
        found: u32,
    },
    /// A sequence number other than the one expected.
    OutOfSequence {
        /// The sequence expected.
        expected: u32,
        /// The sequence received.
        found: u32,
    },
    /// More damage rectangles than [`MAX_DAMAGE_RECTS`].
    TooManyRects {
        /// How many were given.
        count: usize,
    },
    /// A damage rectangle that covers no pixels.
    EmptyRect {
        /// Its position in the list.
        index: usize,
    },
    /// A damage rectangle with a negative corner or a dimension beyond
    /// [`MAX_DIMENSION`], which the wire cannot carry.
    NotRepresentable {
        /// Its position in the list.
        index: usize,
    },
    /// A damage rectangle not wholly inside the layout.
    OutsideLayout {
        /// Its position in the list.
        index: usize,
    },
}

impl From<DecodeError> for ContractError {
    fn from(err: DecodeError) -> Self {
        Self::Decode(err)
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "malformed record: {err}"),
            Self::UnexpectedKind { kind } => write!(f, "unexpected message kind {kind:#06x}"),
            Self::LayoutTooLarge { width, height } => {
                write!(f, "layout {width}x{height} exceeds {MAX_DIMENSION} pixels")
            }
            Self::ExceedsRegion { needed, region } => {
                write!(f, "layout needs {needed} bytes, region holds {region}")
            }
            Self::BadScale { scale } => write!(
                f,
                "device scale {scale} is not 1 to {MAX_SCALE}, or does not divide the layout"
            ),
            Self::AlreadyConfigured => f.write_str("display already configured"),
            Self::NotConfigured => f.write_str("no display layout accepted"),
            Self::PresentInFlight => f.write_str("a present is already in flight"),
            Self::NoPresentInFlight { found } => {
                write!(f, "present-done {found} with no present in flight")
            }
            Self::OutOfSequence { expected, found } => {
                write!(f, "sequence {found} out of order: expected {expected}")
            }
            Self::TooManyRects { count } => {
                write!(f, "{count} damage rectangles exceed {MAX_DAMAGE_RECTS}")
            }
            Self::EmptyRect { index } => write!(f, "damage rectangle {index} is empty"),
            Self::NotRepresentable { index } => {
                write!(f, "damage rectangle {index} cannot be encoded")
            }
            Self::OutsideLayout { index } => {
                write!(f, "damage rectangle {index} lies outside the layout")
            }
        }
    }
}

impl core::error::Error for ContractError {}

/// The wire code of each pixel format. Zero and anything unlisted are
/// refused.
const fn format_to_wire(format: PixelFormat) -> u8 {
    match format {
        PixelFormat::Rgb565 => 1,
        PixelFormat::Rgb888 => 2,
        PixelFormat::Bgr888 => 3,
        PixelFormat::Xrgb8888 => 4,
        PixelFormat::Xbgr8888 => 5,
    }
}

const fn format_from_wire(code: u8) -> Option<PixelFormat> {
    match code {
        1 => Some(PixelFormat::Rgb565),
        2 => Some(PixelFormat::Rgb888),
        3 => Some(PixelFormat::Bgr888),
        4 => Some(PixelFormat::Xrgb8888),
        5 => Some(PixelFormat::Xbgr8888),
        _ => None,
    }
}

/// Display → UI: the framebuffer layout the display shows, and its device
/// scale. The UI draws in exactly this layout, or not at all, and lays its
/// screens out in the layout's size divided by the scale.
///
/// Wire layout, 12 bytes, little-endian (version 2; version 1, had
/// no scale and zero in byte 9):
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–1 | width (`u16`) | not zero |
/// | 2–3 | height (`u16`) | not zero |
/// | 4–7 | stride in bytes (`u32`) | at least width × bytes per pixel |
/// | 8 | pixel format | 1 RGB565, 2 RGB888, 3 BGR888, 4 XRGB8888, 5 XBGR8888 |
/// | 9 | device scale (`u8`) | 1 to [`MAX_SCALE`] (2), dividing width and height |
/// | 10–11 | — | zero |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Configure {
    layout: FramebufferLayout,
    scale: u32,
}

impl Configure {
    /// The message announcing `layout`, at scale 1: one pixel a layout
    /// unit.
    ///
    /// # Errors
    ///
    /// [`ContractError::LayoutTooLarge`] if a dimension exceeds
    /// [`MAX_DIMENSION`].
    pub const fn new(layout: FramebufferLayout) -> Result<Self, ContractError> {
        Self::scaled(layout, 1)
    }

    /// The message announcing `layout` at device scale `scale`: `scale`
    /// pixels each way for each layout unit.
    ///
    /// # Errors
    ///
    /// [`ContractError::LayoutTooLarge`] if a dimension exceeds
    /// [`MAX_DIMENSION`]; [`ContractError::BadScale`] for a scale of 0 or
    /// above [`MAX_SCALE`], or one that does not divide the width and the
    /// height.
    pub const fn scaled(layout: FramebufferLayout, scale: u32) -> Result<Self, ContractError> {
        if layout.width() > MAX_DIMENSION || layout.height() > MAX_DIMENSION {
            return Err(ContractError::LayoutTooLarge {
                width: layout.width(),
                height: layout.height(),
            });
        }
        if !scale_fits(layout.width(), layout.height(), scale) {
            return Err(ContractError::BadScale { scale });
        }
        Ok(Self { layout, scale })
    }

    /// The layout announced.
    #[must_use]
    pub const fn layout(&self) -> FramebufferLayout {
        self.layout
    }

    /// The device scale: pixels each way for each layout unit, 1 to
    /// [`MAX_SCALE`].
    #[must_use]
    pub const fn scale(&self) -> u32 {
        self.scale
    }
}

/// Whether `scale` is 1 to [`MAX_SCALE`] and divides `width` and `height`
/// into whole layout units.
const fn scale_fits(width: u32, height: u32, scale: u32) -> bool {
    scale >= 1 && scale <= MAX_SCALE && width.is_multiple_of(scale) && height.is_multiple_of(scale)
}

impl Message for Configure {
    const KIND: u16 = CONFIGURE_KIND;
    const VERSION: u16 = 2;
    const PAYLOAD_LEN: usize = 12;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let width = u16::try_from(self.layout.width())
            .map_err(|_| EncodeError::InvalidValue { field: "width" })?;
        let height = u16::try_from(self.layout.height())
            .map_err(|_| EncodeError::InvalidValue { field: "height" })?;
        let scale =
            u8::try_from(self.scale).map_err(|_| EncodeError::InvalidValue { field: "scale" })?;
        out.write_u16(width)?;
        out.write_u16(height)?;
        out.write_u32(self.layout.stride())?;
        out.write_u8(format_to_wire(self.layout.format()))?;
        out.write_u8(scale)?;
        out.write_zeros(2)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let width = input.read_u16()?;
        let height = input.read_u16()?;
        let stride = input.read_u32()?;
        let format = format_from_wire(input.read_u8()?)
            .ok_or(DecodeError::InvalidValue { field: "format" })?;
        let scale = u32::from(input.read_u8()?);
        input.read_zeros(2)?;
        let layout =
            FramebufferLayout::new(width.into(), height.into(), stride, format).map_err(|err| {
                match err {
                    LayoutError::Empty | LayoutError::TooLarge => {
                        DecodeError::InvalidValue { field: "size" }
                    }
                    LayoutError::StrideTooSmall { .. } => {
                        DecodeError::InvalidValue { field: "stride" }
                    }
                }
            })?;
        if !scale_fits(layout.width(), layout.height(), scale) {
            return Err(DecodeError::InvalidValue { field: "scale" });
        }
        Ok(Self { layout, scale })
    }
}

/// UI → display: the UI has drawn, and these rectangles changed.
///
/// Built by [`UiEnd::present`](crate::UiEnd::present), which also checks
/// the rectangles against the layout; [`DisplayEnd`](crate::DisplayEnd)
/// checks them again on receipt, since the UI is not trusted to.
///
/// Wire layout, 72 bytes, little-endian:
///
/// | Bytes | Field | Rule |
/// | --- | --- | --- |
/// | 0–3 | sequence (`u32`) | the next one expected |
/// | 4 | count (`u8`) | at most [`MAX_DAMAGE_RECTS`] |
/// | 5–7 | — | zero |
/// | 8–71 | 8 rectangles of `x`, `y`, `width`, `height` (`u16` each) | the first `count` non-empty; the rest all zero |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Present {
    sequence: u32,
    damage: BoundedVec<Rect, MAX_DAMAGE_RECTS>,
}

impl Present {
    /// A Present of `damage` with sequence number `sequence`.
    ///
    /// # Errors
    ///
    /// [`ContractError::TooManyRects`], [`ContractError::EmptyRect`] or
    /// [`ContractError::NotRepresentable`].
    pub fn new(sequence: u32, damage: &[Rect]) -> Result<Self, ContractError> {
        if damage.len() > MAX_DAMAGE_RECTS {
            return Err(ContractError::TooManyRects {
                count: damage.len(),
            });
        }
        let mut rects = BoundedVec::new();
        for (index, rect) in damage.iter().enumerate() {
            if rect.is_empty() {
                return Err(ContractError::EmptyRect { index });
            }
            if wire_rect(rect).is_none() {
                return Err(ContractError::NotRepresentable { index });
            }
            rects.push(*rect).map_err(|_| ContractError::TooManyRects {
                count: damage.len(),
            })?;
        }
        Ok(Self {
            sequence,
            damage: rects,
        })
    }

    /// The sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// The rectangles that changed.
    #[must_use]
    pub fn damage(&self) -> &[Rect] {
        self.damage.as_slice()
    }
}

/// A rectangle's wire fields, if each fits a `u16` (so the corner is not
/// negative).
fn wire_rect(rect: &Rect) -> Option<[u16; 4]> {
    Some([
        u16::try_from(rect.origin.x).ok()?,
        u16::try_from(rect.origin.y).ok()?,
        u16::try_from(rect.size.width).ok()?,
        u16::try_from(rect.size.height).ok()?,
    ])
}

/// Bytes one rectangle takes on the wire.
const RECT_LEN: usize = 8;

impl Message for Present {
    const KIND: u16 = PRESENT_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 8 + MAX_DAMAGE_RECTS * RECT_LEN;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        let count = u8::try_from(self.damage.len())
            .map_err(|_| EncodeError::InvalidValue { field: "count" })?;
        out.write_u32(self.sequence)?;
        out.write_u8(count)?;
        out.write_zeros(3)?;
        for index in 0..MAX_DAMAGE_RECTS {
            match self.damage.get(index) {
                Some(rect) => {
                    let fields =
                        wire_rect(rect).ok_or(EncodeError::InvalidValue { field: "rect" })?;
                    for field in fields {
                        out.write_u16(field)?;
                    }
                }
                None => out.write_zeros(RECT_LEN)?,
            }
        }
        Ok(())
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let sequence = input.read_u32()?;
        let count = usize::from(input.read_u8()?);
        if count > MAX_DAMAGE_RECTS {
            return Err(DecodeError::InvalidValue { field: "count" });
        }
        input.read_zeros(3)?;
        let mut damage = BoundedVec::new();
        for index in 0..MAX_DAMAGE_RECTS {
            if index >= count {
                input.read_zeros(RECT_LEN)?;
                continue;
            }
            let x = input.read_u16()?;
            let y = input.read_u16()?;
            let width = input.read_u16()?;
            let height = input.read_u16()?;
            if width == 0 || height == 0 {
                return Err(DecodeError::InvalidValue { field: "rect" });
            }
            damage
                .push(Rect::new(x.into(), y.into(), width.into(), height.into()))
                .map_err(|_| DecodeError::InvalidValue { field: "count" })?;
        }
        Ok(Self { sequence, damage })
    }
}

/// Display → UI: the Present with this sequence has been shown and its
/// pixels read; the UI may draw again.
///
/// Wire layout, 4 bytes: the sequence (`u32`, little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresentDone {
    /// The sequence of the Present acknowledged.
    pub sequence: u32,
}

impl Message for PresentDone {
    const KIND: u16 = PRESENT_DONE_KIND;
    const VERSION: u16 = 1;
    const PAYLOAD_LEN: usize = 4;

    fn encode_payload(&self, out: &mut Writer<'_>) -> Result<(), EncodeError> {
        out.write_u32(self.sequence)
    }

    fn decode_payload(input: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            sequence: input.read_u32()?,
        })
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
    use nowayos_ipc::{HEADER_LEN, decode_exact, encode};

    fn layout(width: u32, height: u32, stride: u32, format: PixelFormat) -> FramebufferLayout {
        FramebufferLayout::new(width, height, stride, format).unwrap()
    }

    fn record<M: Message>(message: &M) -> [u8; MAX_RECORD_LEN] {
        let mut buf = [0u8; MAX_RECORD_LEN];
        encode(message, &mut buf).unwrap();
        buf
    }

    fn configure_payload(mutate: impl FnOnce(&mut [u8])) -> Result<Configure, DecodeError> {
        let message = Configure::new(layout(640, 480, 2560, PixelFormat::Xrgb8888)).unwrap();
        let mut buf = record(&message);
        mutate(&mut buf[HEADER_LEN..HEADER_LEN + Configure::PAYLOAD_LEN]);
        nowayos_ipc::decode(&buf)
    }

    fn present_payload(mutate: impl FnOnce(&mut [u8])) -> Result<Present, DecodeError> {
        let message = Present::new(1, &[Rect::new(1, 2, 3, 4)]).unwrap();
        let mut buf = record(&message);
        mutate(&mut buf[HEADER_LEN..]);
        nowayos_ipc::decode(&buf)
    }

    #[test]
    fn present_is_the_longest_record() {
        assert_eq!(MAX_RECORD_LEN, 80);
        assert!(encoded_len::<Configure>() <= MAX_RECORD_LEN);
        assert!(encoded_len::<PresentDone>() <= MAX_RECORD_LEN);
    }

    #[test]
    fn configure_layout_matches_the_table() {
        let message =
            Configure::new(layout(0x0102, 0x0304, 0x0506_0708, PixelFormat::Rgb565)).unwrap();
        let buf = record(&message);
        assert_eq!(buf[..HEADER_LEN], [0x01, 0x02, 2, 0, 12, 0, 0, 0]);
        assert_eq!(
            buf[HEADER_LEN..HEADER_LEN + 12],
            [0x02, 0x01, 0x04, 0x03, 0x08, 0x07, 0x06, 0x05, 1, 1, 0, 0]
        );
        let len = encoded_len::<Configure>();
        assert_eq!(decode_exact::<Configure>(&buf[..len]), Ok(message));
    }

    #[test]
    fn every_format_round_trips() {
        for format in [
            PixelFormat::Rgb565,
            PixelFormat::Rgb888,
            PixelFormat::Bgr888,
            PixelFormat::Xrgb8888,
            PixelFormat::Xbgr8888,
        ] {
            let message = Configure::new(FramebufferLayout::packed(8, 2, format).unwrap()).unwrap();
            assert_eq!(nowayos_ipc::decode(&record(&message)), Ok(message));
        }
    }

    #[test]
    fn configure_rejects_each_bad_field() {
        let bad = |field| Err(DecodeError::InvalidValue { field });
        assert_eq!(configure_payload(|p| p[8] = 0), bad("format"));
        assert_eq!(configure_payload(|p| p[8] = 6), bad("format"));
        assert_eq!(configure_payload(|p| p[8] = 0xFF), bad("format"));
        assert_eq!(configure_payload(|p| p[0..2].fill(0)), bad("size"));
        assert_eq!(configure_payload(|p| p[2..4].fill(0)), bad("size"));
        // 640 pixels of XRGB8888 need a stride of at least 2560.
        assert_eq!(
            configure_payload(|p| p[4..8].copy_from_slice(&2559u32.to_le_bytes())),
            bad("stride")
        );
        assert_eq!(
            configure_payload(|p| p[11] = 1),
            Err(DecodeError::NonZeroPadding)
        );
        // The scale: 1 or 2, dividing 640 x 480.
        assert_eq!(configure_payload(|p| p[9] = 0), bad("scale"));
        assert_eq!(configure_payload(|p| p[9] = 3), bad("scale"));
        assert_eq!(
            configure_payload(|p| p[9] = 4),
            bad("scale"),
            "4 divides 640 x 480, but no theme has atlases for it"
        );
        assert_eq!(configure_payload(|p| p[9] = 0xFF), bad("scale"));
        for scale in [1, 2] {
            let configure = configure_payload(|p| p[9] = scale).unwrap();
            assert_eq!(configure.scale(), u32::from(scale));
        }
    }

    #[test]
    fn the_pi_5s_window_at_scale_2_is_a_960_by_540_screen() {
        let window = layout(1920, 1080, 3840, PixelFormat::Rgb565);
        let message = Configure::scaled(window, 2).unwrap();
        assert_eq!((message.layout(), message.scale()), (window, 2));
        assert_eq!(nowayos_ipc::decode(&record(&message)), Ok(message));
        let buf = record(&message);
        assert_eq!(buf[HEADER_LEN + 9], 2);
        assert_eq!(Configure::new(window).unwrap().scale(), 1);
        for scale in [0, 3, 4, u32::MAX] {
            assert_eq!(
                Configure::scaled(window, scale),
                Err(ContractError::BadScale { scale })
            );
        }
    }

    #[test]
    fn configure_refuses_layouts_the_wire_cannot_carry() {
        let wide = FramebufferLayout::packed(MAX_DIMENSION + 1, 1, PixelFormat::Rgb565).unwrap();
        assert_eq!(
            Configure::new(wide),
            Err(ContractError::LayoutTooLarge {
                width: MAX_DIMENSION + 1,
                height: 1
            })
        );
        let edge = FramebufferLayout::packed(MAX_DIMENSION, 1, PixelFormat::Rgb565).unwrap();
        assert!(Configure::new(edge).is_ok());
    }

    #[test]
    fn present_layout_matches_the_table() {
        let message = Present::new(
            0x0A0B_0C0D,
            &[
                Rect::new(1, 2, 3, 4),
                Rect::new(0x0102, 0x0304, 0x0506, 0x0708),
            ],
        )
        .unwrap();
        let buf = record(&message);
        assert_eq!(buf[..HEADER_LEN], [0x02, 0x02, 1, 0, 72, 0, 0, 0]);
        let payload = &buf[HEADER_LEN..];
        assert_eq!(payload[..8], [0x0D, 0x0C, 0x0B, 0x0A, 2, 0, 0, 0]);
        assert_eq!(payload[8..16], [1, 0, 2, 0, 3, 0, 4, 0]);
        assert_eq!(payload[16..24], [2, 1, 4, 3, 6, 5, 8, 7]);
        assert!(payload[24..].iter().all(|&b| b == 0));
        assert_eq!(decode_exact::<Present>(&buf), Ok(message));
    }

    #[test]
    fn present_rejects_each_bad_field() {
        let bad = |field| Err(DecodeError::InvalidValue { field });
        // Over the rectangle bound.
        assert_eq!(present_payload(|p| p[4] = 9), bad("count"));
        assert_eq!(present_payload(|p| p[4] = 0xFF), bad("count"));
        // A counted rectangle that is empty.
        assert_eq!(present_payload(|p| p[12..14].fill(0)), bad("rect"));
        assert_eq!(present_payload(|p| p[14..16].fill(0)), bad("rect"));
        // Padding, and an uncounted rectangle that is not all zero.
        assert_eq!(
            present_payload(|p| p[6] = 1),
            Err(DecodeError::NonZeroPadding)
        );
        assert_eq!(
            present_payload(|p| p[16] = 1),
            Err(DecodeError::NonZeroPadding)
        );
        assert_eq!(
            present_payload(|p| p[71] = 1),
            Err(DecodeError::NonZeroPadding)
        );
        // The count can say less than is there, never more.
        assert_eq!(present_payload(|p| p[4] = 2), bad("rect"));
        assert_eq!(
            present_payload(|p| p[4] = 0),
            Err(DecodeError::NonZeroPadding)
        );
    }

    #[test]
    fn present_new_checks_count_emptiness_and_range() {
        let r = Rect::new(0, 0, 1, 1);
        assert!(Present::new(0, &[r; MAX_DAMAGE_RECTS]).is_ok());
        assert!(Present::new(0, &[]).is_ok());
        assert_eq!(
            Present::new(0, &[r; MAX_DAMAGE_RECTS + 1]),
            Err(ContractError::TooManyRects { count: 9 })
        );
        assert_eq!(
            Present::new(0, &[r, Rect::new(4, 4, 0, 3)]),
            Err(ContractError::EmptyRect { index: 1 })
        );
        assert_eq!(
            Present::new(0, &[Rect::new(-1, 0, 1, 1)]),
            Err(ContractError::NotRepresentable { index: 0 })
        );
        assert_eq!(
            Present::new(0, &[r, r, Rect::new(0, 0, MAX_DIMENSION + 1, 1)]),
            Err(ContractError::NotRepresentable { index: 2 })
        );
    }

    #[test]
    fn present_done_round_trips() {
        let buf = record(&PresentDone {
            sequence: 0x0102_0304,
        });
        assert_eq!(
            buf[..HEADER_LEN + 4],
            [0x03, 0x02, 1, 0, 4, 0, 0, 0, 4, 3, 2, 1]
        );
        assert_eq!(
            nowayos_ipc::decode::<PresentDone>(&buf),
            Ok(PresentDone {
                sequence: 0x0102_0304
            })
        );
    }

    #[test]
    fn kinds_are_distinct_and_checked() {
        let buf = record(&PresentDone { sequence: 1 });
        assert_eq!(
            nowayos_ipc::decode::<Configure>(&buf),
            Err(DecodeError::WrongKind {
                expected: CONFIGURE_KIND,
                found: PRESENT_DONE_KIND
            })
        );
    }
}
