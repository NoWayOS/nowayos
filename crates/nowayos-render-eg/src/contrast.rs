// SPDX-License-Identifier: MIT OR Apache-2.0
//! The contrast every theme must have, as a check any theme can be put
//! through: NoWayOS's own themes in its tests, and a
//! product's - a brand's - in the product's, so that a brand that would
//! ship unreadable fails its build.
//!
//! [`pairs`] lists every pair of a theme's colours the renderer draws one
//! against the other: text against what it is drawn on, and a control's
//! boundary, a choice control's marks and the focus ring against what lies
//! beside them - on the background and on every surface, for every text
//! role and every button variant. [`failures`] gives each pair below WCAG
//! 2's minimum - 4.5:1 for text (success criterion 1.4.3, for text
//! of every size), 3:1 for the rest (1.4.11) - both as
//! the theme specifies it - what a 32-bit screen shows, as the Pi 5's and
//! QEMU's do - and as a 16-bit screen shows it, after `r5g6b5`
//! ([`as_shown`]).
//!
//! A surface's own edge is not checked: it groups content, and no control
//! is known by it (1.4.11). Nor is a quiet button's boundary: it has none,
//! and its label alone shows it.
//!
//! `no_std`: the arithmetic is `f64`, and the one power WCAG's formula
//! needs is worked out without `powf`, which `core` lacks
//! ([`luminance`]).

use core::fmt;

use embedded_graphics::pixelcolor::{Rgb565, Rgb888, RgbColor};
use nowayos_ui::{ButtonVariant, NodeId, NodeKind, Surface, TextRole, Tone, Tree};

use crate::Theme;

/// What a pair of colours drawn together must meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Text against what it is drawn on (WCAG 2, 1.4.3): 4.5:1, for text
    /// of every size. WCAG 2 asks only 3:1 of large text, but the type
    /// scale's smaller roles draw below it, and the check stays as strict
    /// for all.
    Text,
    /// A control's boundary, a choice control's mark or the focus ring
    /// against a colour beside it (1.4.11): 3:1.
    Graphic,
}

impl Need {
    /// The least ratio it asks for.
    #[must_use]
    pub const fn minimum(self) -> f64 {
        match self {
            Self::Text => 4.5,
            Self::Graphic => 3.0,
        }
    }
}

/// One pair of a theme's colours that the renderer draws together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    /// What is drawn, against what.
    pub what: &'static str,
    /// The surface it stands on, or `None` for the background.
    pub on: Option<Surface>,
    /// What it must meet.
    pub need: Need,
    /// The colour drawn.
    pub fore: Rgb888,
    /// The colour it is drawn against.
    pub back: Rgb888,
}

/// A pair below its minimum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Failure {
    /// The pair.
    pub pair: Pair,
    /// Whether it falls short as the screen shows it, after `r5g6b5`,
    /// rather than as the theme specifies it.
    pub shown: bool,
    /// The colour drawn, as specified or as shown.
    pub fore: Rgb888,
    /// The colour it is drawn against, likewise.
    pub back: Rgb888,
    /// Their contrast ratio.
    pub ratio: f64,
}

impl fmt::Display for Failure {
    /// One line: the pair, where it stands, whether after `r5g6b5`, its
    /// ratio, the minimum and the two colours.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.pair.what)?;
        if let Some(surface) = self.pair.on {
            write!(f, ", on {surface:?}")?;
        }
        if self.shown {
            f.write_str(", after r5g6b5")?;
        }
        write!(
            f,
            ": {:.2}:1, below {}:1 ({} on {})",
            self.ratio,
            self.pair.need.minimum(),
            Hex(self.fore),
            Hex(self.back)
        )
    }
}

/// A colour as `#RRGGBB`.
struct Hex(Rgb888);

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.0.r(), self.0.g(), self.0.b())
    }
}

/// `x` to the power 2.4, for `x` from 0 to 1: `x²` times the fifth root of
/// `x²`, found by Newton's method. From 1, each step comes down towards the
/// root and never past it, and never reaches 0, so nothing divides by 0;
/// sixty steps are many more than the 8-bit channels' values need.
fn pow_2_4(x: f64) -> f64 {
    let square = x * x;
    let mut root = 1.0;
    for _ in 0..60 {
        let fourth = root * root * root * root;
        root -= (fourth * root - square) / (5.0 * fourth);
    }
    square * root
}

/// WCAG 2's relative luminance of an sRGB colour, from 0 to 1.
#[must_use]
pub fn luminance(colour: Rgb888) -> f64 {
    let channel = |value: u8| {
        let c = f64::from(value) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            pow_2_4((c + 0.055) / 1.055)
        }
    };
    0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
}

/// WCAG 2's contrast ratio between two colours, from 1 to 21.
#[must_use]
pub fn ratio(a: Rgb888, b: Rgb888) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la >= lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// A colour as a 16-bit screen shows it: in `r5g6b5`, widened back to 8
/// bits as a display does. A 32-bit screen shows the colour itself.
#[must_use]
pub fn as_shown(colour: Rgb888) -> Rgb888 {
    Rgb888::from(Rgb565::from(colour))
}

/// Every ground a control or text stands on: the background, then each
/// surface.
const GROUNDS: [Option<Surface>; 6] = [
    None,
    Some(Surface::Card),
    Some(Surface::Header),
    Some(Surface::Notice(Tone::Success)),
    Some(Surface::Notice(Tone::Caution)),
    Some(Surface::Notice(Tone::Warning)),
];

/// The pairs a text input, a field's error marker and a progress bar's
/// fill and track draw, on the background: those no ground below covers.
fn fields(theme: &Theme<Rgb888>) -> [Pair; 8] {
    let pair = |what, need, fore, back| Pair {
        what,
        on: None,
        need,
        fore,
        back,
    };
    [
        pair(
            "a typed value on the text input",
            Need::Text,
            theme.text,
            theme.input,
        ),
        pair(
            "placeholder on the text input",
            Need::Text,
            theme.placeholder,
            theme.input,
        ),
        pair(
            "a typed value on its selection",
            Need::Text,
            theme.text,
            theme.selection,
        ),
        pair(
            "the error marker's `!` (the background) on the marker",
            Need::Text,
            theme.background,
            theme.error,
        ),
        pair(
            "text input border against the background",
            Need::Graphic,
            theme.input_border,
            theme.background,
        ),
        pair(
            "text input border against the input's fill",
            Need::Graphic,
            theme.input_border,
            theme.input,
        ),
        pair(
            "focus ring and caret against the text input's fill",
            Need::Graphic,
            theme.focus,
            theme.input,
        ),
        pair(
            "a progress bar's fill against its track",
            Need::Graphic,
            theme.button,
            theme.progress_track,
        ),
    ]
}

/// Text in `role`, by name.
const fn role_text(role: TextRole) -> &'static str {
    match role {
        TextRole::Display => "display text",
        TextRole::Title => "title text",
        TextRole::Heading => "heading text",
        TextRole::Body => "body text",
        TextRole::Label => "label text",
        TextRole::Caption => "caption text (muted on the background and on a card)",
    }
}

/// The four things each button variant is checked for, by name: its
/// label, its label pressed, its boundary and its pressed fill.
const fn button_names(variant: ButtonVariant) -> [&'static str; 4] {
    match variant {
        ButtonVariant::Primary => [
            "a primary button's label",
            "a pressed primary button's label",
            "a primary button's fill",
            "a pressed primary button's fill",
        ],
        ButtonVariant::Secondary => [
            "a secondary button's label",
            "a pressed secondary button's label",
            "a secondary button's outline",
            "a pressed secondary button's fill",
        ],
        ButtonVariant::Quiet => [
            "a quiet button's label",
            "a pressed quiet button's label",
            "a quiet button's fill",
            "a pressed quiet button's fill",
        ],
    }
}

/// Every pair drawn on `on`: text of each role, the choice controls'
/// labels and marks, a field's error, the focus ring, each button variant
/// and a progress bar's fill against the ground. A quiet button has
/// no boundary to check, so its third pair is `None`.
fn on_ground(theme: &Theme<Rgb888>, on: Option<Surface>) -> [Option<Pair>; 24] {
    let ground = theme.ground(on);
    let pair = |what, need, fore| {
        Some(Pair {
            what,
            on,
            need,
            fore,
            back: ground,
        })
    };
    let role = |role| pair(role_text(role), Need::Text, theme.text_on(on, role));
    let button = |variant| {
        let colours = theme.button_on(on, variant);
        let [label, pressed_label, boundary, pressed] = button_names(variant);
        let fill = colours.fill.unwrap_or(ground);
        [
            Some(Pair {
                what: label,
                on,
                need: Need::Text,
                fore: colours.label,
                back: fill,
            }),
            Some(Pair {
                what: pressed_label,
                on,
                need: Need::Text,
                fore: colours.pressed_label,
                back: colours.pressed,
            }),
            // Its boundary: its outline, or its own fill, against what
            // lies beneath it.
            colours
                .outline
                .or(colours.fill)
                .and_then(|edge| pair(boundary, Need::Graphic, edge)),
            pair(pressed, Need::Graphic, colours.pressed),
        ]
    };
    let [p0, p1, p2, p3] = button(ButtonVariant::Primary);
    let [s0, s1, s2, s3] = button(ButtonVariant::Secondary);
    let [q0, q1, q2, q3] = button(ButtonVariant::Quiet);
    [
        role(TextRole::Display),
        role(TextRole::Title),
        role(TextRole::Heading),
        role(TextRole::Body),
        role(TextRole::Label),
        role(TextRole::Caption),
        pair(
            "a checkbox's or radio option's label",
            Need::Text,
            theme.ink_on(on),
        ),
        pair(
            "a checkbox's box, a radio option's ring",
            Need::Graphic,
            theme.ink_on(on),
        ),
        pair("a field's error", Need::Text, theme.error_on(on)),
        pair("the focus ring", Need::Graphic, theme.focus_on(on)),
        pair(
            "a progress bar's fill",
            Need::Graphic,
            theme.progress_on(on).0,
        ),
        pair("a scrollbar's thumb", Need::Graphic, theme.scrollbar_on(on)),
        p0,
        p1,
        p2,
        p3,
        s0,
        s1,
        s2,
        s3,
        q0,
        q1,
        q2,
        q3,
    ]
}

/// Every pair of `theme`'s colours the renderer draws together: a text
/// input's and the error marker's, then those on the background and on
/// each surface.
pub fn pairs(theme: &Theme<Rgb888>) -> impl Iterator<Item = Pair> + '_ {
    fields(theme).into_iter().chain(
        GROUNDS
            .into_iter()
            .flat_map(move |on| on_ground(theme, on).into_iter().flatten()),
    )
}

/// Every pair of `theme` below its minimum, as specified and after
/// `r5g6b5` ([`as_shown`]): none, for a theme that may ship.
pub fn failures(theme: &Theme<Rgb888>) -> impl Iterator<Item = Failure> + '_ {
    pairs(theme).flat_map(|pair| {
        [
            (false, pair.fore, pair.back),
            (true, as_shown(pair.fore), as_shown(pair.back)),
        ]
        .into_iter()
        .filter_map(move |(shown, fore, back)| {
            let ratio = ratio(fore, back);
            (ratio < pair.need.minimum()).then_some(Failure {
                pair,
                shown,
                fore,
                back,
                ratio,
            })
        })
    })
}

/// Each node of `tree` that draws text - a text, a slot or a button's label -
/// with the colour its text is drawn in and the colour beneath it, as
/// [`crate::render`] draws them in `theme`: the colours a screen's
/// style names, or its roles' and surfaces' own. A screen whose style names
/// colours is checked by these, in every theme, against [`Need::Text`]
/// ([`as_shown`] each), since the theme's own pairs ([`pairs`]) cannot
/// cover what a screen combines.
pub fn text_colours<'a, const N: usize, const T: usize>(
    tree: &'a Tree<N, T>,
    theme: &'a Theme<Rgb888>,
) -> impl Iterator<Item = (NodeId, Rgb888, Rgb888)> + 'a {
    tree.paint_order().filter_map(move |(id, node)| {
        let (surface, fill) = crate::under(tree, node);
        let ground = fill.map_or_else(|| theme.ground(surface), |fill| theme.colour(fill));
        let style = node.style();
        match node.kind() {
            NodeKind::Text(_) | NodeKind::Slot { .. } => {
                let text = style.text.colour.map_or_else(
                    || theme.text_on(surface, style.text.role),
                    |colour| theme.colour(colour),
                );
                Some((id, text, ground))
            }
            NodeKind::Button { .. } => {
                let colours = theme.button_on(surface, style.variant);
                let fill = style
                    .look
                    .fill
                    .map_or(colours.fill.unwrap_or(ground), |colour| {
                        theme.colour(colour)
                    });
                let label = style
                    .look
                    .text
                    .map_or(colours.label, |colour| theme.colour(colour));
                Some((id, label, fill))
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "test code"
    )]

    use super::*;

    /// The power without `powf` is `powf`'s, for every value an 8-bit
    /// channel takes past the linear part.
    #[test]
    fn the_power_matches_powf_for_every_channel_value() {
        for value in 11..=255_u8 {
            let x = (f64::from(value) / 255.0 + 0.055) / 1.055;
            let (ours, std) = (pow_2_4(x), x.powf(2.4));
            assert!((ours - std).abs() < 1e-15, "{value}: {ours} against {std}");
        }
    }

    #[test]
    fn the_contrast_formula_is_wcag_2s() {
        assert!((ratio(Rgb888::BLACK, Rgb888::WHITE) - 21.0).abs() < 1e-9);
        assert!((ratio(Rgb888::WHITE, Rgb888::WHITE) - 1.0).abs() < 1e-9);
        // #767676 on white: the lightest grey that passes 4.5:1, at 4.54:1.
        let grey = Rgb888::new(0x76, 0x76, 0x76);
        assert!((ratio(grey, Rgb888::WHITE) - 4.54).abs() < 0.005);
        assert!(ratio(Rgb888::new(0x77, 0x77, 0x77), Rgb888::WHITE) < 4.5);
    }

    /// `r5g6b5` keeps 5, 6 and 5 bits of a colour, so most colours come
    /// back a little different, and once shown, a colour shows the same;
    /// black and white come back as they were.
    #[test]
    fn a_colour_as_shown_has_lost_its_low_bits() {
        let ink = Rgb888::new(0x16, 0x19, 0x1D);
        assert_ne!(as_shown(ink), ink);
        assert_eq!(as_shown(as_shown(ink)), as_shown(ink));
        assert_eq!(as_shown(Rgb888::BLACK), Rgb888::BLACK);
        assert_eq!(as_shown(Rgb888::WHITE), Rgb888::WHITE);
    }
}
