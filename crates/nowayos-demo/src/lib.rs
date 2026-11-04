// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small synthetic form demo, and the scripted session that drives it.
//!
//! Two generic screens built with `nowayos-ui` - a form asking for a name,
//! with Help and Continue, then a thank-you screen with Start again - and
//! [`SESSION`]: steps of canonical input events (taps, clicks and keys), the
//! same events an input PD sends. The crate is `no_std` and allocates
//! nothing, so the very same screens and session run on the host (`make
//! demo`, every frame checked against a golden image) and in a UI
//! protection domain in QEMU.
//!
//! The session includes input the UI must ignore without panicking: a tap
//! outside every control, keys with nothing focused, and typing into a full
//! text input.
//!
//! The screen is the Raspberry Pi 5's HDMI monitor - all 1920 × 1080 of its
//! pixels - drawn at its own pixels at device scale 2:
//! a 960 × 540 screen in layout units, its text from the atlases twice the
//! size: 36 px for the light theme's 18 px body text, in which the
//! session's frames are drawn, and 48 px for the large-text theme's 24.
//! QEMU's runs show the same screen. Every tap and click lands on the same
//! control in both themes.
//!
//! Everything here is synthetic and generic: no product, client or person.
//! Provisional.

#![no_std]

use nowayos_display::Rect;
use nowayos_input::{InputEvent, KeyCode, TouchPhase};
use nowayos_ui::{ActionId, Align, FieldId, Style, TextMetrics, Tree, UiError, UiEvent};

/// Screen width, in pixels: the Pi 5's HDMI window.
pub const WIDTH: u32 = 1920;
/// Screen height, in pixels: the Pi 5's window, every row of the monitor.
pub const HEIGHT: u32 = 1080;
/// The device scale the demo is drawn at: 2 pixels each way for each
/// layout unit, so the screen is 960 x 540 units.
pub const SCALE: nowayos_display::DeviceScale = nowayos_display::DeviceScale::TWO;
/// The screen, in pixels: what the session's taps and clicks are placed on.
pub const SCREEN: Rect = Rect::new(0, 0, WIDTH, HEIGHT);

/// Space around the screen's edge, in layout units.
const MARGIN: u32 = 24;
/// Space between the screen's lines, and between its buttons, in layout
/// units.
const GAP: u32 = 16;

/// The form's Continue button.
pub const CONTINUE: ActionId = ActionId(1);
/// The form's Help button.
pub const HELP: ActionId = ActionId(2);
/// The thank-you screen's Start again button.
pub const START_AGAIN: ActionId = ActionId(3);
/// The form's name input.
pub const NAME: FieldId = FieldId(1);

/// Nodes the demo's tree holds: more than either screen needs.
pub const NODES: usize = 16;

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The form asking for a name.
    Form,
    /// The thank-you screen.
    Done,
}

/// The demo: its tree, laid out in the display's viewport - for its
/// session, [`SCREEN`] at [`SCALE`].
#[derive(Debug)]
pub struct Demo {
    tree: Tree<NODES>,
    screen: Screen,
    metrics: TextMetrics,
    viewport: Rect,
}

impl Demo {
    /// The demo on its form screen, laid out in `viewport` with `metrics`
    /// (the theme's font at the display's device scale: the session's
    /// frames are [`SCREEN`] with the light theme's metrics at [`SCALE`]).
    ///
    /// # Errors
    ///
    /// A [`UiError`] if the screen cannot be built - never for these
    /// screens in a viewport layout accepts.
    pub fn new(metrics: TextMetrics, viewport: Rect) -> Result<Self, UiError> {
        let mut demo = Self {
            tree: Tree::new(),
            screen: Screen::Form,
            metrics,
            viewport,
        };
        demo.show(Screen::Form)?;
        Ok(demo)
    }

    /// The tree, to draw only what changed and then mark it drawn
    /// (`Tree::clear_dirty`).
    pub const fn tree_mut(&mut self) -> &mut Tree<NODES> {
        &mut self.tree
    }

    /// The tree to render.
    #[must_use]
    pub const fn tree(&self) -> &Tree<NODES> {
        &self.tree
    }

    /// The screen showing.
    #[must_use]
    pub const fn screen(&self) -> Screen {
        self.screen
    }

    /// Feeds one input event to the UI. Continue moves to the thank-you
    /// screen and Start again back to an empty form; returns the action a
    /// button reported, if any.
    ///
    /// # Errors
    ///
    /// A [`UiError`] if the next screen cannot be built - never for these
    /// screens.
    pub fn handle(&mut self, event: InputEvent) -> Result<Option<ActionId>, UiError> {
        let Some(UiEvent::Action(action)) = self.tree.handle(event) else {
            return Ok(None);
        };
        match action {
            CONTINUE => self.show(Screen::Done)?,
            START_AGAIN => self.show(Screen::Form)?,
            _ => {}
        }
        Ok(Some(action))
    }

    /// Replaces the tree with `screen`, wiping what was typed.
    fn show(&mut self, screen: Screen) -> Result<(), UiError> {
        self.tree.clear();
        let tree = &mut self.tree;
        let column = tree.column(Style::new().padding(MARGIN).gap(GAP))?;
        tree.set_root(column)?;
        let (title, hint) = match screen {
            Screen::Form => ("Welcome", "Please enter your name"),
            Screen::Done => ("Thank you", "That is all for now"),
        };
        for text in [title, hint] {
            let line = tree.text(text)?;
            tree.append(column, line)?;
        }
        if screen == Screen::Form {
            let name = tree.text_input(NAME, "Your name")?;
            tree.append(column, name)?;
        }
        let push_down = tree.spacer()?;
        tree.append(column, push_down)?;
        let row = tree.row(Style::new().gap(GAP).align(Align::Center))?;
        tree.append(column, row)?;
        let buttons: &[(&'static str, ActionId)] = match screen {
            Screen::Form => &[("Help", HELP), ("Continue", CONTINUE)],
            Screen::Done => &[("Start again", START_AGAIN)],
        };
        for (index, &(label, action)) in buttons.iter().enumerate() {
            if index > 0 {
                let push_right = tree.spacer()?;
                tree.append(row, push_right)?;
            }
            let button = tree.button(label, action)?;
            tree.append(row, button)?;
        }
        tree.layout(self.viewport, &self.metrics)?;
        self.screen = screen;
        Ok(())
    }
}

/// The name of an action, for the session's log.
#[must_use]
pub const fn action_name(action: ActionId) -> &'static str {
    match action {
        CONTINUE => "Continue",
        HELP => "Help",
        START_AGAIN => "Start again",
        _ => "unknown",
    }
}

/// One step of the session: its name and the input events it sends. A frame
/// is checked after each step.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    /// A short name, used for the step's golden image.
    pub name: &'static str,
    /// The input events, in order.
    pub events: &'static [InputEvent],
}

/// The key for a letter, at compile time.
const fn key(letter: char) -> KeyCode {
    match KeyCode::letter(letter) {
        Some(key) => key,
        None => KeyCode::SPACE,
    }
}

/// A finger touching `(x, y)` and lifting again.
const fn tap(x: i32, y: i32) -> [InputEvent; 2] {
    [
        InputEvent::Touch {
            id: 0,
            x,
            y,
            phase: TouchPhase::Start,
        },
        InputEvent::Touch {
            id: 0,
            x,
            y,
            phase: TouchPhase::End,
        },
    ]
}

/// The pointer moving to `(x, y)` and clicking.
const fn click(x: i32, y: i32) -> [InputEvent; 3] {
    [
        InputEvent::PointerMove { x, y },
        InputEvent::PointerButton {
            button: 0,
            down: true,
        },
        InputEvent::PointerButton {
            button: 0,
            down: false,
        },
    ]
}

// Where the session taps and clicks, in the screen's pixels at scale 2. In
// the light theme (18 px body text, 36 px at 2x, lines 47 high) the
// form's name input spans y 206-272; its buttons, 56 units high,
// y 920-1031, Help x 48-222 and Continue x 1625-1871; Start again, x
// 48-330. In the large-text theme (24 px, 48 px at 2x, lines 63 high) the
// input spans y 238-320 and the buttons y 920-1031, Help x 48-248 and
// Continue x 1575-1871; Start again, x 48-392. Each point below lies
// inside its control in both.

/// Below the name input and above the buttons: in the gap where the
/// spacer is, nowhere a control is.
const TAP_OUTSIDE: [InputEvent; 2] = tap(960, 600);
const UNFOCUSED_KEYS: [InputEvent; 2] = [
    InputEvent::KeyDown(key('q')),
    InputEvent::KeyDown(KeyCode::ENTER),
];
const TAP_NAME: [InputEvent; 2] = tap(400, 260);
const TYPE_NAME: [InputEvent; 5] = [
    InputEvent::KeyDown(KeyCode::LEFT_SHIFT),
    InputEvent::KeyDown(key('a')),
    InputEvent::KeyUp(KeyCode::LEFT_SHIFT),
    InputEvent::KeyDown(key('d')),
    InputEvent::KeyDown(key('a')),
];
/// 62 more letters: 61 fill the 64-byte input after "Ada", and the last is
/// refused because the input is full.
const FILL_NAME: [InputEvent; 62] = [InputEvent::KeyDown(key('x')); 62];
const CLICK_HELP: [InputEvent; 3] = click(120, 970);
const CLICK_CONTINUE: [InputEvent; 3] = click(1750, 970);
const TAP_START_AGAIN: [InputEvent; 2] = tap(180, 970);

/// The scripted session: each step's input, then a frame to check.
pub const SESSION: &[Step] = &[
    Step {
        name: "start",
        events: &[],
    },
    Step {
        name: "tap-outside",
        events: &TAP_OUTSIDE,
    },
    Step {
        name: "keys-unfocused",
        events: &UNFOCUSED_KEYS,
    },
    Step {
        name: "tap-name",
        events: &TAP_NAME,
    },
    Step {
        name: "type-name",
        events: &TYPE_NAME,
    },
    Step {
        name: "type-into-full",
        events: &FILL_NAME,
    },
    Step {
        name: "click-help",
        events: &CLICK_HELP,
    },
    Step {
        name: "click-continue",
        events: &CLICK_CONTINUE,
    },
    Step {
        name: "keys-on-done",
        events: &UNFOCUSED_KEYS,
    },
    Step {
        name: "tap-start-again",
        events: &TAP_START_AGAIN,
    },
];

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed check panics, and tests index known data"
)]
mod tests {
    extern crate std;

    use super::*;
    use embedded_graphics::pixelcolor::Rgb888;
    use nowayos_display::Rect;
    use nowayos_render_eg::Theme;

    /// The light theme's metrics at the demo's scale: the session's frames
    /// are drawn in it.
    fn metrics() -> TextMetrics {
        Theme::<Rgb888>::light().metrics_at(SCALE)
    }

    /// Where each tap and click of the session lands, as a point.
    fn point(event: &InputEvent) -> Option<(i32, i32)> {
        match *event {
            InputEvent::Touch { x, y, .. } | InputEvent::PointerMove { x, y } => Some((x, y)),
            _ => None,
        }
    }

    /// The rectangle of the node at `index` in paint order.
    fn rect_at(demo: &Demo, index: usize) -> Rect {
        demo.tree().paint_order().nth(index).unwrap().1.rect()
    }

    /// The layout the comment above the session's points describes, in
    /// both themes, so the points are where it says.
    #[test]
    fn the_controls_are_where_the_session_taps() {
        for (theme, input, help, carry_on, again) in [
            (
                Theme::<Rgb888>::light(),
                Rect::new(48, 206, 1824, 67),
                Rect::new(48, 920, 175, 112),
                Rect::new(1625, 920, 247, 112),
                Rect::new(48, 920, 283, 112),
            ),
            (
                Theme::<Rgb888>::high_contrast(),
                Rect::new(48, 238, 1824, 83),
                Rect::new(48, 920, 201, 112),
                Rect::new(1575, 920, 297, 112),
                Rect::new(48, 920, 345, 112),
            ),
        ] {
            let mut demo = Demo::new(theme.metrics_at(SCALE), SCREEN).unwrap();
            // The column, two lines, the input, the spacer, the row, Help,
            // the spacer between, Continue.
            assert_eq!(rect_at(&demo, 3), input);
            assert_eq!(rect_at(&demo, 6), help);
            assert_eq!(rect_at(&demo, 8), carry_on);
            for step in &SESSION[..8] {
                for event in step.events {
                    demo.handle(*event).unwrap();
                }
            }
            assert_eq!(demo.screen(), Screen::Done);
            assert_eq!(rect_at(&demo, 5), again);
        }
        let inside = |area: Rect, events: &[InputEvent]| {
            events
                .iter()
                .filter_map(point)
                .all(|(x, y)| area.contains(nowayos_display::Point::new(x, y)))
        };
        // Inside the narrower of each control in the two themes.
        assert!(inside(Rect::new(48, 238, 1824, 35), &TAP_NAME));
        assert!(inside(Rect::new(48, 945, 123, 71), &CLICK_HELP));
        assert!(inside(Rect::new(1682, 945, 190, 71), &CLICK_CONTINUE));
        assert!(inside(Rect::new(48, 945, 221, 71), &TAP_START_AGAIN));
        // Between the large-text input's bottom and the large-text buttons'
        // top.
        assert!(inside(Rect::new(48, 353, 1824, 544), &TAP_OUTSIDE));
    }

    /// Runs the session, returning each step's actions and the screen after
    /// it.
    fn run() -> std::vec::Vec<(&'static str, std::vec::Vec<ActionId>, Screen)> {
        let mut demo = Demo::new(metrics(), SCREEN).unwrap();
        SESSION
            .iter()
            .map(|step| {
                let actions = step
                    .events
                    .iter()
                    .filter_map(|event| demo.handle(*event).unwrap())
                    .collect();
                (step.name, actions, demo.screen())
            })
            .collect()
    }

    #[test]
    fn the_session_clicks_through_both_screens() {
        let steps = run();
        let actions: std::vec::Vec<_> = steps
            .iter()
            .flat_map(|(name, actions, _)| actions.iter().map(move |a| (*name, action_name(*a))))
            .collect();
        assert_eq!(
            actions,
            [
                ("click-help", "Help"),
                ("click-continue", "Continue"),
                ("tap-start-again", "Start again")
            ]
        );
        let screens: std::vec::Vec<_> = steps.iter().map(|(_, _, screen)| *screen).collect();
        assert_eq!(screens[..7], [Screen::Form; 7]);
        assert_eq!(screens[7..9], [Screen::Done; 2]);
        assert_eq!(screens[9], Screen::Form);
    }

    #[test]
    fn the_name_fills_and_then_refuses_more() {
        let mut demo = Demo::new(metrics(), SCREEN).unwrap();
        for step in &SESSION[..6] {
            for event in step.events {
                demo.handle(*event).unwrap();
            }
        }
        let value = demo.tree().value(NAME).unwrap();
        assert_eq!(value.len(), nowayos_ui::INPUT_CAPACITY);
        assert!(value.starts_with("Adaxxx"));
    }

    #[test]
    fn start_again_shows_an_empty_form() {
        let mut demo = Demo::new(metrics(), SCREEN).unwrap();
        for step in SESSION {
            for event in step.events {
                demo.handle(*event).unwrap();
            }
        }
        assert_eq!(demo.screen(), Screen::Form);
        assert_eq!(demo.tree().value(NAME), Some(""));
    }
}
