// SPDX-License-Identifier: MIT OR Apache-2.0
//! HID report descriptors (HID 1.11, sections 6.2.2 and 8.4) and the
//! absolute pointers they describe: a touchscreen, or a tablet such as
//! QEMU's.
//!
//! A keyboard or mouse in the boot protocol sends fixed reports
//! ([`hid`](crate::hid)). A touchscreen does not: its reports are laid out
//! as its **report descriptor** says, a program of short items - main items
//! (Input, Output, Feature, Collection), global items (usage page, logical
//! range, report size, count and ID, push and pop) and local items
//! (usages), which the host must interpret to find where in a report the X
//! and Y positions and the tip switch are.
//!
//! **A report descriptor is what a device says about itself, and a device is
//! not trusted.** [`parse`] walks a copy of it with every bound fixed in
//! advance: at most [`DESCRIPTOR_MAX`] bytes, [`MAX_USAGES`] usages an
//! item, a push stack [`MAX_PUSH`] deep and collections [`MAX_DEPTH`] deep,
//! input fields of 1 to 32 bits and input reports of at most
//! [`REPORT_BITS`] bits; a long item, a reserved item type, a value that
//! runs past the end or any bound exceeded is a typed [`ReportError`],
//! never a panic. It records the first [`MAX_FIELDS`] input fields, walks
//! the rest within the same bounds without recording them, and records
//! how long each input report is: so a multi-touch screen, whose
//! descriptor lays out ten contacts, still has its first contact found. A
//! descriptor longer than [`DESCRIPTOR_MAX`] is refused, not read in part:
//! what it lays out past the bytes read could make its reports longer than
//! the walk saw.
//!
//! [`Layout::find`] then picks out one absolute pointer - X and Y from the
//! Generic Desktop page, absolute and variable, with a range - and its tip
//! switch or buttons, with its report's length, which the USB host PD
//! checks fits one packet of the interface's endpoint
//! ([`Layout::report_bytes`]): a report longer than a packet would arrive
//! in pieces, and a later piece could decode as a report of its own. And
//! [`AbsolutePointer`] turns each report into canonical events: a touch's
//! start, moves and end for a touchscreen; pointer moves and buttons for a
//! tablet. Only one contact is followed; a multi-touch screen's first
//! contact drives it.

use core::fmt;

use nowayos_input::{InputEvent, InputLimits, TouchPhase};

/// The longest report descriptor read.
pub const DESCRIPTOR_MAX: usize = 512;

/// The most input fields recorded from one descriptor: the rest are walked
/// but not recorded.
pub const MAX_FIELDS: usize = 32;

/// The most usages one main item may name.
pub const MAX_USAGES: usize = 16;

/// How deep the global item stack may be pushed.
pub const MAX_PUSH: usize = 4;

/// How deep collections may nest.
pub const MAX_DEPTH: usize = 8;

/// The most bits of one report, its report ID excluded: a full-speed
/// interrupt packet less that byte.
pub const REPORT_BITS: u32 = 63 * 8;

/// Usage pages and usages (HID Usage Tables 1.5).
pub mod usage {
    /// The Generic Desktop page.
    pub const GENERIC_DESKTOP: u16 = 0x01;
    /// The Button page.
    pub const BUTTON: u16 = 0x09;
    /// The Digitizers page.
    pub const DIGITIZER: u16 = 0x0D;
    /// Generic Desktop X.
    pub const X: u16 = 0x30;
    /// Generic Desktop Y.
    pub const Y: u16 = 0x31;
    /// Digitizers Tip Switch: the finger or stylus touches.
    pub const TIP_SWITCH: u16 = 0x42;
}

/// Why a report descriptor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportError {
    /// Longer than [`DESCRIPTOR_MAX`].
    TooLong,
    /// An item's data runs past the end.
    Truncated {
        /// Where the item starts.
        at: usize,
    },
    /// A long item, or an item of the reserved type: not supported.
    Unsupported {
        /// Where it starts.
        at: usize,
    },
    /// A bound exceeded: too many fields, usages, pushes, collections, or a
    /// field or report too large.
    Bounds {
        /// Which bound.
        what: &'static str,
    },
    /// A Pop with nothing pushed, or an End Collection with nothing open.
    Unbalanced {
        /// Where it is.
        at: usize,
    },
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooLong => write!(f, "longer than {DESCRIPTOR_MAX} bytes"),
            Self::Truncated { at } => write!(f, "an item at byte {at} cut short"),
            Self::Unsupported { at } => write!(f, "a long or reserved item at byte {at}"),
            Self::Bounds { what } => write!(f, "too many or too large: {what}"),
            Self::Unbalanced { at } => write!(f, "an unbalanced pop or end at byte {at}"),
        }
    }
}

/// One element of an input report: where it is and what it means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// The report it is in (0 when the device uses no report IDs).
    pub report_id: u8,
    /// Its first bit in the report, after the report ID byte.
    pub bit: u16,
    /// Its size in bits, 1 to 32.
    pub bits: u8,
    /// Its usage page.
    pub page: u16,
    /// Its usage.
    pub usage: u16,
    /// Its logical range.
    pub min: i32,
    /// Its logical range.
    pub max: i32,
    /// Whether it is a variable (not an array) holding an absolute value.
    pub absolute_variable: bool,
}

impl Field {
    /// Its value in `report` (the bytes after the report ID), little-endian
    /// bit order, sign-extended when its logical minimum is negative;
    /// `None` if the report is too short to hold it.
    #[must_use]
    pub fn value(&self, report: &[u8]) -> Option<i32> {
        let mut raw: u64 = 0;
        for index in 0..u32::from(self.bits) {
            let at = u32::from(self.bit).checked_add(index)?;
            let byte = report.get(usize::try_from(at / 8).ok()?)?;
            if (byte >> (at % 8)) & 1 != 0 {
                raw |= 1u64.checked_shl(index)?;
            }
        }
        let bits = u32::from(self.bits);
        if self.min < 0 && bits < 64 && raw & (1u64 << bits.saturating_sub(1).min(63)) != 0 {
            // Sign-extend from `bits`.
            raw |= u64::MAX.checked_shl(bits).unwrap_or(0);
            return i32::try_from(raw as i64).ok();
        }
        i32::try_from(raw).ok().or(Some(i32::MAX))
    }
}

/// The input fields a report descriptor declares, in order, and how long
/// each of its input reports is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    fields: [Field; MAX_FIELDS],
    len: usize,
    /// Each input report's bits, its report ID's byte excluded, by ID: at
    /// most [`REPORT_BITS`].
    report_bits: [u16; 256],
}

impl Fields {
    /// The fields.
    #[must_use]
    pub fn as_slice(&self) -> &[Field] {
        self.fields.get(..self.len).unwrap_or_default()
    }

    /// The bytes of input report `report_id`, as the device sends it: its
    /// report ID's byte, unless it is 0 (no IDs), then its fields, padded
    /// to a whole byte.
    #[must_use]
    pub fn report_bytes(&self, report_id: u8) -> u16 {
        let bits = self
            .report_bits
            .get(usize::from(report_id))
            .copied()
            .unwrap_or(0);
        bits.div_ceil(8).saturating_add(u16::from(report_id != 0))
    }

    /// Records `field`; false, recording nothing, once [`MAX_FIELDS`] are.
    fn push(&mut self, field: Field) -> bool {
        let Some(slot) = self.fields.get_mut(self.len) else {
            return false;
        };
        *slot = field;
        self.len = self.len.saturating_add(1);
        true
    }
}

/// The global items' state (section 6.2.2.7).
#[derive(Clone, Copy, Debug, Default)]
struct Globals {
    page: u16,
    min: i32,
    max: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

/// The local items' state for the next main item (section 6.2.2.8).
#[derive(Clone, Copy, Debug, Default)]
struct Locals {
    usages: [(u16, u16); MAX_USAGES],
    len: usize,
    minimum: Option<(u16, u16)>,
    maximum: Option<(u16, u16)>,
}

impl Locals {
    fn add(&mut self, page: u16, usage: u16) -> Result<(), ReportError> {
        let slot = self
            .usages
            .get_mut(self.len)
            .ok_or(ReportError::Bounds { what: "usages" })?;
        *slot = (page, usage);
        self.len = self.len.saturating_add(1);
        Ok(())
    }

    /// The usage of element `index` of a main item: the listed usages in
    /// turn, then a minimum-to-maximum range, the last one repeating.
    fn usage(&self, index: u32) -> (u16, u16) {
        let listed = self.usages.get(..self.len).unwrap_or_default();
        if let Some(found) = usize::try_from(index).ok().and_then(|i| listed.get(i)) {
            return *found;
        }
        if let (Some((page, low)), Some((_, high))) = (self.minimum, self.maximum) {
            let offset = u16::try_from(index).unwrap_or(u16::MAX);
            return (page, low.saturating_add(offset).min(high));
        }
        listed.last().copied().unwrap_or((0, 0))
    }
}

/// An item's unsigned data, little-endian.
fn unsigned(data: &[u8]) -> u32 {
    data.iter()
        .rev()
        .fold(0u32, |value, byte| (value << 8) | u32::from(*byte))
}

/// An item's signed data, sign-extended from its size.
fn signed(data: &[u8]) -> i32 {
    match *data {
        [] => 0,
        [a] => i32::from(a as i8),
        [a, b] => i32::from(i16::from_le_bytes([a, b])),
        [a, b, c, d] => i32::from_le_bytes([a, b, c, d]),
        _ => 0,
    }
}

/// A usage item's page and usage: a 4-byte usage carries its own page
/// (section 6.2.2.8).
fn usage_of(data: &[u8], page: u16) -> (u16, u16) {
    let value = unsigned(data);
    if data.len() == 4 {
        ((value >> 16) as u16, (value & 0xFFFF) as u16)
    } else {
        (page, (value & 0xFFFF) as u16)
    }
}

/// Walks a copy of a whole report descriptor and returns its first
/// [`MAX_FIELDS`] input fields and each input report's length.
///
/// # Errors
///
/// [`ReportError`] for a descriptor that breaks a bound or is malformed.
pub fn parse(descriptor: &[u8]) -> Result<Fields, ReportError> {
    if descriptor.len() > DESCRIPTOR_MAX {
        return Err(ReportError::TooLong);
    }
    let mut fields = Fields {
        fields: [Field::default(); MAX_FIELDS],
        len: 0,
        report_bits: [0; 256],
    };
    let mut globals = Globals::default();
    let mut stack = [Globals::default(); MAX_PUSH];
    let mut pushed = 0usize;
    let mut locals = Locals::default();
    let mut depth = 0usize;
    // Bits so far in each report: one counter per report ID.
    let mut offsets = [0u32; 256];
    let mut at = 0usize;
    while let Some(&prefix) = descriptor.get(at) {
        if prefix == 0xFE {
            return Err(ReportError::Unsupported { at });
        }
        let size = match prefix & 0x3 {
            3 => 4,
            n => usize::from(n),
        };
        let start = at.checked_add(1).ok_or(ReportError::Truncated { at })?;
        let end = start
            .checked_add(size)
            .ok_or(ReportError::Truncated { at })?;
        let data = descriptor
            .get(start..end)
            .ok_or(ReportError::Truncated { at })?;
        let tag = prefix >> 4;
        match (prefix >> 2) & 0x3 {
            // Main items.
            0 => {
                match tag {
                    // Input.
                    0x8 => {
                        let flags = unsigned(data);
                        if globals.count > 0 && !(1..=32).contains(&globals.size) {
                            return Err(ReportError::Bounds {
                                what: "report field size",
                            });
                        }
                        let constant = flags & 1 != 0;
                        let variable = flags & 2 != 0;
                        let relative = flags & 4 != 0;
                        let offset = offsets
                            .get_mut(usize::from(globals.report_id))
                            .ok_or(ReportError::Bounds { what: "report IDs" })?;
                        let total =
                            globals
                                .size
                                .checked_mul(globals.count)
                                .ok_or(ReportError::Bounds {
                                    what: "report size",
                                })?;
                        let next = offset
                            .checked_add(total)
                            .filter(|next| *next <= REPORT_BITS)
                            .ok_or(ReportError::Bounds {
                                what: "report size",
                            })?;
                        if !constant && fields.len < MAX_FIELDS {
                            for index in 0..globals.count {
                                let (page, usage) = locals.usage(index);
                                let bit = globals
                                    .size
                                    .checked_mul(index)
                                    .and_then(|bits| bits.checked_add(*offset))
                                    .and_then(|bit| u16::try_from(bit).ok())
                                    .ok_or(ReportError::Bounds {
                                        what: "report size",
                                    })?;
                                if !fields.push(Field {
                                    report_id: globals.report_id,
                                    bit,
                                    bits: u8::try_from(globals.size).unwrap_or(32),
                                    page,
                                    usage,
                                    min: globals.min,
                                    max: globals.max,
                                    absolute_variable: variable && !relative,
                                }) {
                                    break;
                                }
                            }
                        }
                        *offset = next;
                    }
                    // Output, Feature: not inputs; their bits are their own
                    // reports'.
                    0x9 | 0xB => {}
                    // Collection.
                    0xA => {
                        depth = depth
                            .checked_add(1)
                            .filter(|depth| *depth <= MAX_DEPTH)
                            .ok_or(ReportError::Bounds {
                                what: "collection depth",
                            })?;
                    }
                    // End Collection.
                    0xC => {
                        depth = depth.checked_sub(1).ok_or(ReportError::Unbalanced { at })?;
                    }
                    _ => return Err(ReportError::Unsupported { at }),
                }
                locals = Locals::default();
            }
            // Global items.
            1 => match tag {
                0x0 => globals.page = (unsigned(data) & 0xFFFF) as u16,
                0x1 => globals.min = signed(data),
                0x2 => {
                    // A maximum is signed only if the minimum is negative.
                    globals.max = if globals.min < 0 {
                        signed(data)
                    } else {
                        i32::try_from(unsigned(data)).unwrap_or(i32::MAX)
                    };
                }
                0x7 => {
                    globals.size = unsigned(data);
                    if !(1..=32).contains(&globals.size) {
                        return Err(ReportError::Bounds {
                            what: "report field size",
                        });
                    }
                }
                0x8 => {
                    globals.report_id = u8::try_from(unsigned(data))
                        .ok()
                        .filter(|id| *id != 0)
                        .ok_or(ReportError::Bounds { what: "report ID" })?;
                }
                // Report Count: checked where an Input item uses it, as the
                // size of the report it makes.
                0x9 => globals.count = unsigned(data),
                0xA => {
                    let slot = stack
                        .get_mut(pushed)
                        .ok_or(ReportError::Bounds { what: "push depth" })?;
                    *slot = globals;
                    pushed = pushed.saturating_add(1);
                }
                0xB => {
                    pushed = pushed
                        .checked_sub(1)
                        .ok_or(ReportError::Unbalanced { at })?;
                    globals = stack.get(pushed).copied().unwrap_or_default();
                }
                // Physical range, units: not needed here.
                0x3..=0x6 => {}
                _ => return Err(ReportError::Unsupported { at }),
            },
            // Local items.
            2 => match tag {
                0x0 => {
                    let (page, usage) = usage_of(data, globals.page);
                    locals.add(page, usage)?;
                }
                0x1 => locals.minimum = Some(usage_of(data, globals.page)),
                0x2 => locals.maximum = Some(usage_of(data, globals.page)),
                // Designators, strings, delimiters: not needed here.
                _ => {}
            },
            _ => return Err(ReportError::Unsupported { at }),
        }
        at = end;
    }
    if depth != 0 {
        return Err(ReportError::Unbalanced { at });
    }
    for (bits, offset) in fields.report_bits.iter_mut().zip(offsets) {
        // At most REPORT_BITS, so it fits.
        *bits = u16::try_from(offset).unwrap_or(u16::MAX);
    }
    Ok(fields)
}

/// What kind of absolute pointer a layout is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    /// A touchscreen: a tip switch, so touches.
    Touch,
    /// A tablet or absolute mouse: buttons, so pointer moves and clicks.
    Pointer,
}

/// Where one absolute pointer's values are in its reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The report its fields are in (0: the device uses no IDs).
    pub report_id: u8,
    /// Its X position.
    pub x: Field,
    /// Its Y position.
    pub y: Field,
    /// Its tip switch, for a touchscreen.
    pub tip: Option<Field>,
    /// Its first three buttons, for a tablet.
    pub buttons: [Option<Field>; 3],
    /// The bytes of its report as the device sends it, its report ID's
    /// included ([`Fields::report_bytes`]): a report longer than a packet
    /// of its endpoint would arrive in pieces, so the USB host PD uses the
    /// layout only if it fits one.
    pub report_bytes: u16,
}

impl Layout {
    /// The first absolute pointer `fields` hold: absolute variable X and Y
    /// from the Generic Desktop page, in one report, each with a range, and
    /// that report's tip switch or buttons 1 to 3. `None` if there is none.
    #[must_use]
    pub fn find(fields: &Fields) -> Option<Self> {
        let all = fields;
        let fields = fields.as_slice();
        let axis = |usage: u16, report: Option<u8>| {
            fields.iter().copied().find(|field| {
                field.page == usage::GENERIC_DESKTOP
                    && field.usage == usage
                    && field.absolute_variable
                    && field.max > field.min
                    && report.is_none_or(|id| field.report_id == id)
            })
        };
        let x = axis(usage::X, None)?;
        let y = axis(usage::Y, Some(x.report_id))?;
        let in_report = |page: u16, usage: u16| {
            fields.iter().copied().find(|field| {
                field.report_id == x.report_id && field.page == page && field.usage == usage
            })
        };
        Some(Self {
            report_id: x.report_id,
            x,
            y,
            tip: in_report(usage::DIGITIZER, usage::TIP_SWITCH),
            buttons: [
                in_report(usage::BUTTON, 1),
                in_report(usage::BUTTON, 2),
                in_report(usage::BUTTON, 3),
            ],
            report_bytes: all.report_bytes(x.report_id),
        })
    }

    /// A touchscreen if it has a tip switch, otherwise a pointer.
    #[must_use]
    pub const fn kind(&self) -> PointerKind {
        if self.tip.is_some() {
            PointerKind::Touch
        } else {
            PointerKind::Pointer
        }
    }
}

/// An absolute pointer's state: its layout, the screen it maps onto, and
/// where it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsolutePointer {
    layout: Layout,
    limits: InputLimits,
    at: Option<(i32, i32)>,
    touching: bool,
    buttons: u8,
    /// Reports refused: too short, or of another report ID.
    refused: u32,
}

/// The most events one report makes: a move or touch, and three buttons.
pub const MAX_ABSOLUTE_EVENTS: usize = 4;

impl AbsolutePointer {
    /// A pointer laid out as `layout`, mapped onto the screen `limits`
    /// gives.
    #[must_use]
    pub const fn new(layout: Layout, limits: InputLimits) -> Self {
        Self {
            layout,
            limits,
            at: None,
            touching: false,
            buttons: 0,
            refused: 0,
        }
    }

    /// The layout.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Reports refused so far.
    #[must_use]
    pub const fn refused(&self) -> u32 {
        self.refused
    }

    /// Scales `value` of `field` onto `0..pixels`.
    fn scale(field: &Field, value: i32, pixels: u16) -> i32 {
        let value = i64::from(value.clamp(field.min, field.max));
        let span = i64::from(field.max)
            .saturating_sub(i64::from(field.min))
            .max(1);
        let last = i64::from(pixels).saturating_sub(1);
        let scaled = value
            .saturating_sub(i64::from(field.min))
            .saturating_mul(last)
            .checked_div(span)
            .unwrap_or(0);
        i32::try_from(scaled.clamp(0, last)).unwrap_or(0)
    }

    /// Takes a copy of one report and passes `emit` the events it makes:
    /// for a touchscreen a touch's start, its moves and its end; for a
    /// tablet the pointer's new position if it moved, then each button that
    /// changed. A report of another report ID, or too short to hold the
    /// fields, changes nothing and is counted.
    pub fn report(&mut self, report: &[u8], mut emit: impl FnMut(InputEvent)) {
        let body = if self.layout.report_id == 0 {
            Some(report)
        } else {
            match report.split_first() {
                Some((id, body)) if *id == self.layout.report_id => Some(body),
                _ => None,
            }
        };
        let Some(body) = body else {
            self.refused = self.refused.saturating_add(1);
            return;
        };
        let (Some(x), Some(y)) = (self.layout.x.value(body), self.layout.y.value(body)) else {
            self.refused = self.refused.saturating_add(1);
            return;
        };
        let at = (
            Self::scale(&self.layout.x, x, self.limits.width()),
            Self::scale(&self.layout.y, y, self.limits.height()),
        );
        let moved = self.at != Some(at);
        match self.layout.tip {
            Some(tip) => {
                let down = tip.value(body).is_some_and(|value| value != 0);
                let phase = match (self.touching, down) {
                    (false, true) => Some(TouchPhase::Start),
                    (true, true) if moved => Some(TouchPhase::Move),
                    (true, false) => Some(TouchPhase::End),
                    _ => None,
                };
                self.touching = down;
                // A touch ends where it was last seen down.
                let (x, y) = if phase == Some(TouchPhase::End) {
                    self.at.unwrap_or(at)
                } else {
                    at
                };
                if let Some(phase) = phase {
                    emit(InputEvent::Touch { id: 0, x, y, phase });
                }
                if down {
                    self.at = Some(at);
                }
            }
            None => {
                if moved {
                    emit(InputEvent::PointerMove { x: at.0, y: at.1 });
                }
                self.at = Some(at);
                let mut buttons = 0u8;
                for (index, field) in (0u8..).zip(self.layout.buttons) {
                    if field
                        .and_then(|field| field.value(body))
                        .is_some_and(|value| value != 0)
                    {
                        buttons |= 1 << index;
                    }
                }
                self.set_buttons(buttons, &mut emit);
            }
        }
    }

    /// Ends a touch in progress and releases every button - when the
    /// device goes.
    pub fn release_all(&mut self, mut emit: impl FnMut(InputEvent)) {
        if self.touching {
            self.touching = false;
            let (x, y) = self.at.unwrap_or((0, 0));
            emit(InputEvent::Touch {
                id: 0,
                x,
                y,
                phase: TouchPhase::End,
            });
        }
        self.set_buttons(0, &mut emit);
    }

    fn set_buttons(&mut self, buttons: u8, emit: &mut impl FnMut(InputEvent)) {
        for button in 0..3u8 {
            let mask = 1u8 << button;
            if (self.buttons ^ buttons) & mask != 0 {
                emit(InputEvent::PointerButton {
                    button,
                    down: buttons & mask != 0,
                });
            }
        }
        self.buttons = buttons;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A single-touch touchscreen, invented from the HID Usage Tables'
    /// digitizer examples: report ID 2, a tip switch and in-range bit, six
    /// bits of padding, a contact ID, then X and Y, 0 to 4095, 16 bits
    /// each.
    pub(crate) const TOUCHSCREEN: &[u8] = &[
        0x05, 0x0D, // Usage Page (Digitizers)
        0x09, 0x04, // Usage (Touch Screen)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x02, //   Report ID (2)
        0x09, 0x22, //   Usage (Finger)
        0xA1, 0x02, //   Collection (Logical)
        0x09, 0x42, //     Usage (Tip Switch)
        0x09, 0x32, //     Usage (In Range)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x75, 0x01, //     Report Size (1)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x95, 0x06, //     Report Count (6)
        0x81, 0x03, //     Input (Constant)
        0x09, 0x51, //     Usage (Contact Identifier)
        0x25, 0x0A, //     Logical Maximum (10)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x26, 0xFF, 0x0F, // Logical Maximum (4095)
        0x75, 0x10, //     Report Size (16)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0xC0, //         End Collection
        0xC0, //       End Collection
    ];

    /// A tablet: three buttons, five bits of padding, X and Y from 0 to
    /// 32767, 16 bits each, and a relative wheel - no report ID. These are
    /// the bytes QEMU's `usb-tablet` gave over USB when `usb-input-test`'s
    /// USB host PD read its report descriptor (2026-10-07; read from the
    /// device, never from QEMU's source).
    pub(crate) const TABLET: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x03, //     Usage Maximum (3)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x95, 0x03, //     Report Count (3)
        0x75, 0x01, //     Report Size (1)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x95, 0x01, //     Report Count (1)
        0x75, 0x05, //     Report Size (5)
        0x81, 0x01, //     Input (Constant)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x15, 0x00, //     Logical Minimum (0)
        0x26, 0xFF, 0x7F, // Logical Maximum (32767)
        0x35, 0x00, //     Physical Minimum (0)
        0x46, 0xFF, 0x7F, // Physical Maximum (32767)
        0x75, 0x10, //     Report Size (16)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x38, //     Usage (Wheel)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7F, //     Logical Maximum (127)
        0x35, 0x00, //     Physical Minimum (0)
        0x45, 0x00, //     Physical Maximum (0)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0xC0, //         End Collection
        0xC0, //       End Collection
    ];

    fn limits() -> InputLimits {
        InputLimits::new(800, 480).unwrap()
    }

    fn events(pointer: &mut AbsolutePointer, report: &[u8]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        pointer.report(report, |event| events.push(event));
        events
    }

    #[test]
    fn a_touchscreens_layout() {
        let fields = parse(TOUCHSCREEN).unwrap();
        let layout = Layout::find(&fields).unwrap();
        assert_eq!(layout.kind(), PointerKind::Touch);
        assert_eq!(layout.report_id, 2);
        assert_eq!((layout.x.bit, layout.x.bits, layout.x.max), (16, 16, 4095));
        assert_eq!((layout.y.bit, layout.y.bits), (32, 16));
        assert_eq!(layout.tip.map(|tip| (tip.bit, tip.bits)), Some((0, 1)));
        // Report 2: its ID's byte, then 48 bits.
        assert_eq!(layout.report_bytes, 7);
        assert_eq!(fields.report_bytes(2), 7);
        assert_eq!(fields.report_bytes(3), 1, "no fields: only an ID");
    }

    #[test]
    fn a_touch_starts_moves_and_ends() {
        let layout = Layout::find(&parse(TOUCHSCREEN).unwrap()).unwrap();
        let mut touch = AbsolutePointer::new(layout, limits());
        // Report 2: tip down, contact 0, at (2047, 0).
        let down = [2, 0b01, 0, 0xFF, 0x07, 0x00, 0x00];
        assert_eq!(
            events(&mut touch, &down),
            [InputEvent::Touch {
                id: 0,
                x: 399,
                y: 0,
                phase: TouchPhase::Start
            }]
        );
        // The same place again: nothing.
        assert!(events(&mut touch, &down).is_empty());
        let moved = [2, 0b01, 0, 0xFF, 0x0F, 0xFF, 0x0F];
        assert_eq!(
            events(&mut touch, &moved),
            [InputEvent::Touch {
                id: 0,
                x: 799,
                y: 479,
                phase: TouchPhase::Move
            }]
        );
        // Lifted: the touch ends where it was last down.
        let up = [2, 0b00, 0, 0, 0, 0, 0];
        assert_eq!(
            events(&mut touch, &up),
            [InputEvent::Touch {
                id: 0,
                x: 799,
                y: 479,
                phase: TouchPhase::End
            }]
        );
        // Another report ID, and a short report: refused.
        assert!(events(&mut touch, &[3, 1, 0, 0, 0, 0, 0]).is_empty());
        assert!(events(&mut touch, &[2, 1, 0]).is_empty());
        assert_eq!(touch.refused(), 2);
        // Unplugged while touching: the touch ends.
        events(&mut touch, &down);
        let mut gone = Vec::new();
        touch.release_all(|event| gone.push(event));
        assert_eq!(
            gone,
            [InputEvent::Touch {
                id: 0,
                x: 399,
                y: 0,
                phase: TouchPhase::End
            }]
        );
    }

    #[test]
    fn a_tablet_moves_and_clicks() {
        let layout = Layout::find(&parse(TABLET).unwrap()).unwrap();
        assert_eq!(layout.kind(), PointerKind::Pointer);
        assert_eq!(layout.report_id, 0);
        assert_eq!((layout.x.bit, layout.y.bit), (8, 24));
        // No report IDs: 48 bits, as QEMU's tablet sends them.
        assert_eq!(layout.report_bytes, 6);
        let mut tablet = AbsolutePointer::new(layout, limits());
        // The middle: 16384 on each axis.
        assert_eq!(
            events(&mut tablet, &[0, 0x00, 0x40, 0x00, 0x40, 0]),
            [InputEvent::PointerMove { x: 399, y: 239 }]
        );
        assert_eq!(
            events(&mut tablet, &[1, 0x00, 0x40, 0x00, 0x40, 0]),
            [InputEvent::PointerButton {
                button: 0,
                down: true
            }]
        );
        assert_eq!(
            events(&mut tablet, &[0, 0xFF, 0x7F, 0, 0, 0]),
            [
                InputEvent::PointerMove { x: 799, y: 0 },
                InputEvent::PointerButton {
                    button: 0,
                    down: false
                }
            ]
        );
    }

    #[test]
    fn bad_descriptors_are_refused() {
        assert_eq!(
            parse(&[0xFE, 2, 0, 0, 0]),
            Err(ReportError::Unsupported { at: 0 })
        );
        assert_eq!(parse(&[0x26, 0xFF]), Err(ReportError::Truncated { at: 0 }));
        assert_eq!(parse(&[0xC0]), Err(ReportError::Unbalanced { at: 0 }));
        assert_eq!(parse(&[0xB4]), Err(ReportError::Unbalanced { at: 0 }));
        assert_eq!(parse(&[0xA1, 0x01]), Err(ReportError::Unbalanced { at: 2 }));
        assert_eq!(
            parse(&[0x75, 33]),
            Err(ReportError::Bounds {
                what: "report field size"
            })
        );
        assert_eq!(
            parse(&[0x85, 0]),
            Err(ReportError::Bounds { what: "report ID" })
        );
        assert_eq!(parse(&[0x0C]), Err(ReportError::Unsupported { at: 0 }));
        assert_eq!(parse(&[0u8; DESCRIPTOR_MAX + 1]), Err(ReportError::TooLong));
        // 64 bytes of 8-bit data: more than a report holds.
        assert_eq!(
            parse(&[0x75, 8, 0x95, 64, 0x81, 0x02]),
            Err(ReportError::Bounds {
                what: "report size"
            })
        );
        // More than MAX_FIELDS fields: the first MAX_FIELDS are recorded.
        assert_eq!(
            parse(&[0x75, 1, 0x95, 40, 0x81, 0x02])
                .unwrap()
                .as_slice()
                .len(),
            MAX_FIELDS
        );
        // Collections nested too deep, pushes too deep.
        assert!(parse(&[0xA1, 0x00].repeat(MAX_DEPTH + 1)).is_err());
        assert!(parse(&[0xA4].repeat(MAX_PUSH + 1)).is_err());
        // An Input item before any Report Size (found by make fuzz):
        // fields of no bits are refused, never made.
        assert_eq!(
            parse(&[0x95, 0x02, 0x81, 0x02]),
            Err(ReportError::Bounds {
                what: "report field size"
            })
        );
        // A keyboard's descriptor has no absolute pointer.
        let keys = [0x05, 0x07, 0x75, 8, 0x95, 6, 0x81, 0x00];
        assert_eq!(Layout::find(&parse(&keys).unwrap()), None);
    }

    /// A multi-touch screen laid out for `contacts` contacts in report 1:
    /// each a Finger collection - its tip switch, seven bits of padding, a
    /// contact identifier, then X and Y, 0 to 4095, 16 bits each - and then
    /// the contact count. Invented for these tests, on the Digitizers page
    /// of the HID Usage Tables.
    fn multi_touch(contacts: usize) -> Vec<u8> {
        let mut descriptor = vec![
            0x05, 0x0D, // Usage Page (Digitizers)
            0x09, 0x04, // Usage (Touch Screen)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x01, //   Report ID (1)
        ];
        for _ in 0..contacts {
            descriptor.extend_from_slice(&[
                0x05, 0x0D, // Usage Page (Digitizers)
                0x09, 0x22, // Usage (Finger)
                0xA1, 0x02, // Collection (Logical)
                0x09, 0x42, //   Usage (Tip Switch)
                0x15, 0x00, //   Logical Minimum (0)
                0x25, 0x01, //   Logical Maximum (1)
                0x75, 0x01, //   Report Size (1)
                0x95, 0x01, //   Report Count (1)
                0x81, 0x02, //   Input (Data, Variable, Absolute)
                0x95, 0x07, //   Report Count (7)
                0x81, 0x03, //   Input (Constant)
                0x09, 0x51, //   Usage (Contact Identifier)
                0x25, 0x0F, //   Logical Maximum (15)
                0x75, 0x08, //   Report Size (8)
                0x95, 0x01, //   Report Count (1)
                0x81, 0x02, //   Input (Data, Variable, Absolute)
                0x05, 0x01, //   Usage Page (Generic Desktop)
                0x09, 0x30, //   Usage (X)
                0x09, 0x31, //   Usage (Y)
                0x26, 0xFF, 0x0F, // Logical Maximum (4095)
                0x75, 0x10, //   Report Size (16)
                0x95, 0x02, //   Report Count (2)
                0x81, 0x02, //   Input (Data, Variable, Absolute)
                0xC0, //       End Collection
            ]);
        }
        descriptor.extend_from_slice(&[
            0x05, 0x0D, //   Usage Page (Digitizers)
            0x09, 0x54, //   Usage (Contact Count)
            0x25, 0x0A, //   Logical Maximum (10)
            0x75, 0x08, //   Report Size (8)
            0x95, 0x01, //   Report Count (1)
            0x81, 0x02, //   Input (Data, Variable, Absolute)
            0xC0, //       End Collection
        ]);
        descriptor
    }

    #[test]
    fn a_ten_contact_screen_is_driven_by_its_first_contact() {
        let descriptor = multi_touch(10);
        assert_eq!(descriptor.len(), 501);
        // 41 input fields: the first 32 are recorded, the rest walked.
        let fields = parse(&descriptor).unwrap();
        assert_eq!(fields.as_slice().len(), MAX_FIELDS);
        let layout = Layout::find(&fields).unwrap();
        assert_eq!(layout.kind(), PointerKind::Touch);
        assert_eq!(layout.report_id, 1);
        assert_eq!(layout.tip.map(|tip| (tip.bit, tip.bits)), Some((0, 1)));
        assert_eq!((layout.x.bit, layout.y.bit, layout.x.max), (16, 32, 4095));
        // Its report: the ID, ten contacts of 6 bytes, the contact count -
        // known though most of its fields are not recorded.
        assert_eq!(layout.report_bytes, 62);
        // Its first contact down at (4095, 0), the second contact elsewhere.
        let mut report = [0u8; 62];
        report[0] = 1;
        report[1] = 0b1;
        report[3..5].copy_from_slice(&4095u16.to_le_bytes());
        report[7] = 0b1;
        let mut touch = AbsolutePointer::new(layout, limits());
        assert_eq!(
            events(&mut touch, &report),
            [InputEvent::Touch {
                id: 0,
                x: 799,
                y: 0,
                phase: TouchPhase::Start
            }]
        );
    }

    #[test]
    fn a_feature_reports_count_is_not_an_inputs() {
        // A Feature item of 4096 bytes - a vendor blob - before a
        // touchscreen: only an Input item's count is checked.
        let mut descriptor = vec![0x75, 0x08, 0x96, 0x00, 0x10, 0xB1, 0x02];
        descriptor.extend_from_slice(TOUCHSCREEN);
        let layout = Layout::find(&parse(&descriptor).unwrap()).unwrap();
        assert_eq!(layout.kind(), PointerKind::Touch);
        // As an Input item it is too big for a report.
        descriptor[5] = 0x81;
        assert_eq!(
            parse(&descriptor),
            Err(ReportError::Bounds {
                what: "report size"
            })
        );
    }

    #[test]
    fn an_eleven_contact_screen_is_refused() {
        // 549 bytes: longer than is read, so refused whole - what lies past
        // the bytes read could lengthen its report.
        let descriptor = multi_touch(11);
        assert_eq!(descriptor.len(), 549);
        assert_eq!(parse(&descriptor), Err(ReportError::TooLong));
        // Its first 512 bytes, all the driver reads: cut mid-item, and no
        // layout - its report, 68 bytes, would not fit a 64-byte packet.
        assert!(parse(&descriptor[..DESCRIPTOR_MAX]).is_err());
    }

    #[test]
    fn values_sign_extend_only_with_a_negative_minimum() {
        let field = Field {
            report_id: 0,
            bit: 4,
            bits: 8,
            page: 1,
            usage: 0x38,
            min: -127,
            max: 127,
            absolute_variable: false,
        };
        assert_eq!(field.value(&[0xF0, 0x0F]), Some(-1));
        assert_eq!(field.value(&[0x10, 0x00]), Some(1));
        assert_eq!(field.value(&[0x10]), None);
        let unsigned = Field {
            min: 0,
            max: 255,
            ..field
        };
        assert_eq!(unsigned.value(&[0xF0, 0x0F]), Some(255));
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_keeps_its_bounds(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            if let Ok(fields) = parse(&bytes) {
                prop_assert!(fields.as_slice().len() <= MAX_FIELDS);
                for field in fields.as_slice() {
                    prop_assert!((1..=32).contains(&field.bits));
                    prop_assert!(u32::from(field.bit) + u32::from(field.bits) <= REPORT_BITS);
                }
                if let Some(layout) = Layout::find(&fields) {
                    let mut pointer = AbsolutePointer::new(layout, limits());
                    for report in [&bytes[..bytes.len().min(64)], &[0u8; 64][..], &[0xFF; 64][..]] {
                        let mut count = 0;
                        pointer.report(report, |event| {
                            count += 1;
                            match event {
                                InputEvent::PointerMove { x, y } | InputEvent::Touch { x, y, .. } => {
                                    assert!((0..800).contains(&x) && (0..480).contains(&y));
                                }
                                InputEvent::PointerButton { button, .. } => assert!(button < 3),
                                other => panic!("{other:?}"),
                            }
                        });
                        prop_assert!(count <= MAX_ABSOLUTE_EVENTS);
                    }
                }
            }
        }
    }
}
