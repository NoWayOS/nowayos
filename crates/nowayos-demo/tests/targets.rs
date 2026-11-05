// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every focusable control on the demo's screens, after every step of its
//! session, is at least WCAG 2.2's minimum target size, 24 by 24 layout
//! units - 48 by 48 pixels at the demo's scale, 2 - in every theme.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests compute on known data"
)]

use embedded_graphics::pixelcolor::Rgb888;
use nowayos_demo::{Demo, SCALE, SCREEN, SESSION, Screen};
use nowayos_render_eg::Theme;
use nowayos_ui::MIN_TARGET;

#[test]
fn every_control_of_the_demo_meets_the_minimum_target_size_in_every_theme() {
    let mut failures = Vec::new();
    for (name, theme) in Theme::<Rgb888>::all() {
        let mut demo = Demo::new(theme.metrics_at(SCALE), SCREEN).unwrap();
        let mut screens = Vec::new();
        for step in SESSION {
            for event in step.events {
                demo.handle(*event).unwrap();
            }
            screens.push(demo.screen());
            for (_, node) in demo.tree().paint_order() {
                let size = node.rect().size;
                let least = SCALE.px(MIN_TARGET);
                if node.is_focusable() && (size.width < least || size.height < least) {
                    failures.push(format!(
                        "{name}, after {}: {:?} is {}x{}, below {least}x{least}",
                        step.name,
                        node.kind(),
                        size.width,
                        size.height
                    ));
                }
            }
        }
        assert!(
            screens.contains(&Screen::Form) && screens.contains(&Screen::Done),
            "{name}: the session shows both screens"
        );
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
