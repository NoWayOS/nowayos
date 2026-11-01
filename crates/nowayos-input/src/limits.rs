// SPDX-License-Identifier: MIT OR Apache-2.0
//! Validation and normalisation of events against the declared screen and
//! device limits, and relative-to-absolute pointer tracking.

use core::fmt;

use crate::InputEvent;

/// Why an event was rejected by [`InputLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputError {
    /// A position lies outside the screen (strict validation only;
    /// normalisation clamps instead).
    OutsideScreen {
        /// Horizontal position.
        x: i32,
        /// Vertical position.
        y: i32,
    },
    /// A pointer button number the device does not have.
    UnknownButton {
        /// The button number.
        button: u8,
    },
    /// A touch contact number beyond the supported number of contacts.
    TouchIdOutOfRange {
        /// The contact number.
        id: u8,
    },
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideScreen { x, y } => write!(f, "position ({x}, {y}) is off-screen"),
            Self::UnknownButton { button } => write!(f, "unknown pointer button {button}"),
            Self::TouchIdOutOfRange { id } => write!(f, "touch contact {id} out of range"),
        }
    }
}

impl core::error::Error for InputError {}

/// The screen size and device limits events are checked against.
///
/// The input PD applies these to every event before forwarding it, so the
/// UI can rely on positions being on-screen and button and contact numbers
/// being in range.
///
/// ```
/// use nowayos_input::{InputEvent, InputLimits};
///
/// let limits = InputLimits::new(800, 480).unwrap();
/// let wild = InputEvent::PointerMove { x: 9000, y: -3 };
/// assert!(limits.validate(wild).is_err());
/// assert_eq!(
///     limits.normalise(wild),
///     Ok(InputEvent::PointerMove { x: 799, y: 0 })
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLimits {
    width: u16,
    height: u16,
    buttons: u8,
    touch_points: u8,
}

impl InputLimits {
    /// Limits for a `width` × `height` pixel screen, with 3 pointer buttons
    /// and 10 touch contacts. Returns `None` if either dimension is zero.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            width,
            height,
            buttons: 3,
            touch_points: 10,
        })
    }

    /// The same limits with `buttons` pointer buttons (numbered from 0).
    #[must_use]
    pub const fn with_buttons(self, buttons: u8) -> Self {
        Self { buttons, ..self }
    }

    /// The same limits with `touch_points` simultaneous contacts (numbered
    /// from 0).
    #[must_use]
    pub const fn with_touch_points(self, touch_points: u8) -> Self {
        Self {
            touch_points,
            ..self
        }
    }

    /// Screen width in pixels.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Screen height in pixels.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Number of pointer buttons.
    #[must_use]
    pub const fn buttons(&self) -> u8 {
        self.buttons
    }

    /// Number of simultaneous touch contacts.
    #[must_use]
    pub const fn touch_points(&self) -> u8 {
        self.touch_points
    }

    /// Accepts `event` only if it is already within limits.
    ///
    /// # Errors
    ///
    /// [`InputError`] naming the first limit the event breaks.
    pub fn validate(&self, event: InputEvent) -> Result<InputEvent, InputError> {
        self.check_ids(event)?;
        match event {
            InputEvent::PointerMove { x, y } | InputEvent::Touch { x, y, .. }
                if self.clamp(x, y) != (x, y) =>
            {
                Err(InputError::OutsideScreen { x, y })
            }
            _ => Ok(event),
        }
    }

    /// Clamps positions onto the screen and rejects what cannot be repaired.
    ///
    /// Clamping is right for positions: a mouse pushed against the edge of
    /// the screen, or a touch panel reporting slightly beyond its active
    /// area, still means "at the edge". A button or contact number out of
    /// range has no sensible repair, so it is rejected.
    ///
    /// # Errors
    ///
    /// [`InputError::UnknownButton`] or [`InputError::TouchIdOutOfRange`].
    pub fn normalise(&self, event: InputEvent) -> Result<InputEvent, InputError> {
        self.check_ids(event)?;
        Ok(match event {
            InputEvent::PointerMove { x, y } => {
                let (x, y) = self.clamp(x, y);
                InputEvent::PointerMove { x, y }
            }
            InputEvent::Touch { id, x, y, phase } => {
                let (x, y) = self.clamp(x, y);
                InputEvent::Touch { id, x, y, phase }
            }
            other => other,
        })
    }

    fn check_ids(&self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::PointerButton { button, .. } if button >= self.buttons => {
                Err(InputError::UnknownButton { button })
            }
            InputEvent::Touch { id, .. } if id >= self.touch_points => {
                Err(InputError::TouchIdOutOfRange { id })
            }
            _ => Ok(()),
        }
    }

    /// Clamps `(x, y)` into `0..width` × `0..height`.
    fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        let max_x = i32::from(self.width.saturating_sub(1));
        let max_y = i32::from(self.height.saturating_sub(1));
        (x.clamp(0, max_x), y.clamp(0, max_y))
    }
}

/// Turns relative pointer motion into absolute, on-screen
/// [`InputEvent::PointerMove`] events.
///
/// Mice report movement, not position — as does the `POINTER_DELTA` message
/// of the sanitising-microcontroller design. The
/// input PD keeps one of these per pointer and forwards absolute positions.
///
/// ```
/// use nowayos_input::{InputEvent, InputLimits, PointerTracker};
///
/// let mut pointer = PointerTracker::new(InputLimits::new(100, 50).unwrap());
/// assert_eq!(pointer.position(), (50, 25)); // starts in the centre
/// assert_eq!(pointer.apply_delta(500, -5), InputEvent::PointerMove { x: 99, y: 20 });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerTracker {
    limits: InputLimits,
    x: i32,
    y: i32,
}

impl PointerTracker {
    /// A tracker for the screen in `limits`, starting at the centre.
    #[must_use]
    pub fn new(limits: InputLimits) -> Self {
        Self {
            limits,
            x: i32::from(limits.width / 2),
            y: i32::from(limits.height / 2),
        }
    }

    /// The current position.
    #[must_use]
    pub const fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Moves by `(dx, dy)`, clamped to the screen, and returns the new
    /// position as an event.
    pub fn apply_delta(&mut self, dx: i32, dy: i32) -> InputEvent {
        let (x, y) = self
            .limits
            .clamp(self.x.saturating_add(dx), self.y.saturating_add(dy));
        self.x = x;
        self.y = y;
        InputEvent::PointerMove { x, y }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use crate::{KeyCode, TouchPhase};

    fn limits() -> InputLimits {
        InputLimits::new(320, 240)
            .unwrap()
            .with_buttons(2)
            .with_touch_points(2)
    }

    #[test]
    fn zero_sized_screens_are_refused() {
        assert_eq!(InputLimits::new(0, 10), None);
        assert_eq!(InputLimits::new(10, 0), None);
    }

    #[test]
    fn validate_is_strict() {
        let l = limits();
        let inside = InputEvent::PointerMove { x: 319, y: 239 };
        assert_eq!(l.validate(inside), Ok(inside));
        assert_eq!(
            l.validate(InputEvent::PointerMove { x: 320, y: 0 }),
            Err(InputError::OutsideScreen { x: 320, y: 0 })
        );
        assert_eq!(
            l.validate(InputEvent::PointerButton {
                button: 2,
                down: true
            }),
            Err(InputError::UnknownButton { button: 2 })
        );
        let key = InputEvent::KeyDown(KeyCode::ENTER);
        assert_eq!(l.validate(key), Ok(key));
    }

    #[test]
    fn normalise_clamps_positions_and_rejects_ids() {
        let l = limits();
        assert_eq!(
            l.normalise(InputEvent::Touch {
                id: 1,
                x: i32::MIN,
                y: i32::MAX,
                phase: TouchPhase::Start
            }),
            Ok(InputEvent::Touch {
                id: 1,
                x: 0,
                y: 239,
                phase: TouchPhase::Start
            })
        );
        assert_eq!(
            l.normalise(InputEvent::Touch {
                id: 2,
                x: 0,
                y: 0,
                phase: TouchPhase::End
            }),
            Err(InputError::TouchIdOutOfRange { id: 2 })
        );
    }

    #[test]
    fn tracker_clamps_and_saturates() {
        let mut p = PointerTracker::new(limits());
        assert_eq!(p.position(), (160, 120));
        p.apply_delta(i32::MAX, i32::MAX);
        assert_eq!(p.position(), (319, 239));
        p.apply_delta(i32::MIN, i32::MIN);
        assert_eq!(p.position(), (0, 0));
        assert_eq!(p.apply_delta(3, 4), InputEvent::PointerMove { x: 3, y: 4 });
    }

    #[test]
    fn one_pixel_screen() {
        let l = InputLimits::new(1, 1).unwrap();
        assert_eq!(
            l.normalise(InputEvent::PointerMove { x: 7, y: -7 }),
            Ok(InputEvent::PointerMove { x: 0, y: 0 })
        );
    }
}
