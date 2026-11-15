// SPDX-License-Identifier: MIT OR Apache-2.0
//! The usual application: one `nowayos-ui` tree, as a UI protection domain
//! holds it.
//!
//! A product implements [`UiApplication`] for the value its UI PD runs -
//! its tree, and what it does with an input event - and wraps it in [`Ui`],
//! which is an [`Application`]: it draws the tree with NoWayOS's renderer in
//! the application's theme, keeping the glyphs it has drawn from frame to
//! frame as a UI PD does.
//!
//! ```no_run
//! # use nowayos_dev_window::ui::{Ui, UiApplication};
//! # use nowayos_dev_window::{Options, Server};
//! # use nowayos_input::InputEvent;
//! # use nowayos_ui::Tree;
//! # struct App(Tree<16>);
//! # impl App { fn new() -> Result<Self, String> { Ok(Self(Tree::new())) } }
//! impl UiApplication<16, 0> for App {
//!     fn tree(&self) -> &Tree<16> {
//!         &self.0
//!     }
//!     fn handle(&mut self, event: InputEvent) {
//!         let _ = self.0.handle(event);
//!     }
//! }
//!
//! let server = Server::bind(Options::new("My application"))?;
//! server.run(|| Ok(Ui::new(App::new()?, 1920, 1080)))?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Drawing is generic over the tree's sizes, so it is compiled in the
//! product's own dev program: build that crate optimised (a
//! `[profile.NAME.package.PROGRAM] opt-level = 3`), or each frame takes
//! tens of milliseconds where it could take a few.

use nowayos_input::InputEvent;
use nowayos_render_eg::{Glyphs, Rgb888, Theme, render};
use nowayos_testkit::Framebuffer;
use nowayos_ui::Tree;

use crate::Application;

/// An application that is one tree of `N` nodes and `T` bytes of long text.
pub trait UiApplication<const N: usize, const T: usize> {
    /// The tree to draw, laid out for the screen.
    fn tree(&self) -> &Tree<N, T>;

    /// One input event, as an input PD delivers it.
    fn handle(&mut self, event: InputEvent);

    /// A batch of input has ended: whatever the application does then.
    fn end_input(&mut self) {}

    /// Time has reached `now`, in milliseconds from
    /// [`START`](crate::START): returns whether what is shown may have
    /// changed. An application with no clock of its own leaves this alone.
    fn advance(&mut self, now: u64) -> bool {
        let _ = now;
        false
    }

    /// The theme the tree is drawn in: NoWayOS's light theme unless the
    /// application has its own.
    fn theme(&self) -> Theme<Rgb888> {
        Theme::light()
    }

    /// A word for where the application is - its screen's name.
    fn status(&self) -> String {
        String::new()
    }
}

/// A [`UiApplication`] as the window shows it.
pub struct Ui<A, const N: usize, const T: usize> {
    app: A,
    width: u32,
    height: u32,
    /// The glyphs drawn so far. Their storage is never given back: a
    /// megabyte or so each time the application is started again.
    glyphs: Glyphs<'static>,
}

impl<A: UiApplication<N, T>, const N: usize, const T: usize> Ui<A, N, T> {
    /// `app`, drawn on a screen of `width` by `height` pixels: the size its
    /// tree was laid out for.
    #[must_use]
    pub fn new(app: A, width: u32, height: u32) -> Self {
        let (pixels, slots) = nowayos_testkit::glyph_storage();
        let glyphs = Glyphs::new(
            Box::leak(pixels.into_boxed_slice()),
            Box::leak(slots.into_boxed_slice()),
        );
        Self {
            app,
            width,
            height,
            glyphs,
        }
    }

    /// The application.
    pub const fn app(&self) -> &A {
        &self.app
    }
}

impl<A: UiApplication<N, T>, const N: usize, const T: usize> Application for Ui<A, N, T> {
    fn input(&mut self, events: &[InputEvent], _now: u64) {
        for event in events {
            self.app.handle(*event);
        }
        self.app.end_input();
    }

    fn advance(&mut self, now: u64) -> bool {
        self.app.advance(now)
    }

    fn draw(&mut self) -> Result<Framebuffer, String> {
        let mut frame = Framebuffer::new(self.width, self.height).map_err(|err| err.to_string())?;
        render(
            self.app.tree(),
            &mut frame,
            &self.app.theme(),
            &mut self.glyphs,
        )
        .map_err(|err| format!("{err:?}"))?;
        Ok(frame)
    }

    fn status(&self) -> String {
        self.app.status()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed step fails the test"
)]
mod tests {
    use super::*;
    use nowayos_display::Rect;
    use nowayos_input::KeyCode;
    use nowayos_ui::{ActionId, UiEvent};

    /// One button, and how often it has been pressed.
    struct Presses {
        tree: Tree<8>,
        pressed: u32,
    }

    impl Presses {
        fn new() -> Self {
            let mut tree = Tree::new();
            let button = tree.button("Press", ActionId(1)).unwrap();
            tree.set_root(button).unwrap();
            tree.layout(
                Rect::new(0, 0, 320, 240),
                &Theme::<Rgb888>::light().metrics(),
            )
            .unwrap();
            tree.set_focus(Some(button)).unwrap();
            Self { tree, pressed: 0 }
        }
    }

    impl UiApplication<8, 0> for Presses {
        fn tree(&self) -> &Tree<8> {
            &self.tree
        }

        fn handle(&mut self, event: InputEvent) {
            if let Some(UiEvent::Action(_)) = self.tree.handle(event) {
                self.pressed = self.pressed.saturating_add(1);
            }
        }

        fn status(&self) -> String {
            format!("{} pressed", self.pressed)
        }
    }

    #[test]
    fn the_tree_is_drawn_at_its_size_and_input_reaches_it() {
        let mut ui = Ui::new(Presses::new(), 320, 240);
        let frame = ui.draw().unwrap();
        assert_eq!((frame.width(), frame.height()), (320, 240));
        let first = frame.pixel(0, 0).unwrap();
        assert!(frame.pixels().iter().any(|&pixel| pixel != first));
        assert_eq!(ui.status(), "0 pressed");
        ui.input(
            &[
                InputEvent::KeyDown(KeyCode::ENTER),
                InputEvent::KeyUp(KeyCode::ENTER),
            ],
            crate::START,
        );
        assert_eq!(ui.status(), "1 pressed");
        assert!(!ui.advance(crate::START + 1_000));
    }
}
