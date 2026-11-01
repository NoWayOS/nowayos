// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every theme meets WCAG 2's contrast minimums for every pair of colours
//! the renderer draws together, and every focusable control the UI core
//! lays out is at least the minimum target size, 24 by 24 pixels -
//! the token tests. A theme change that breaks a minimum fails
//! here, naming the theme, the pair and its ratio, or the control and its
//! size.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::mono_font::iso_8859_1::FONT_6X10;
use embedded_graphics::pixelcolor::Rgb888;
use nowayos_display::Rect;
use nowayos_render_eg::{Font, Theme, TypeScale, contrast};
use nowayos_ui::{ActionId, Align, FieldId, MIN_TARGET, NodeKind, Style, Tree};

/// Every pair of `theme`'s colours drawn together below its minimum, as
/// specified and after `r5g6b5` (`nowayos_render_eg::contrast`), one
/// line each naming the theme, the pair, where it stands, its ratio and
/// its colours.
fn contrast_failures(name: &str, theme: &Theme<Rgb888>) -> Vec<String> {
    contrast::failures(theme)
        .map(|failure| format!("{name}: {failure}"))
        .collect()
}

/// Every theme meets WCAG 2's minimums for every pair of colours drawn
/// together - 4.5:1 for text, 3:1 for a control's boundary, a choice
/// control's marks and the focus ring - on the background and on every
/// surface, for every text role and button variant, as specified and as
/// the screen shows it after `r5g6b5`.
#[test]
fn every_theme_meets_the_contrast_minimums_for_every_pair_drawn_together() {
    let failures: Vec<String> = Theme::<Rgb888>::all()
        .iter()
        .flat_map(|(name, theme)| contrast_failures(name, theme))
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    // Every pair is checked: a text input's, the error marker's and a
    // progress bar's seven, and on each of the six grounds the six text
    // roles, the choice controls' label and marks, the error, the focus
    // ring, a progress bar's fill and three button variants' four pairs -
    // less a quiet button's boundary, which it has none of.
    for (name, theme) in Theme::<Rgb888>::all() {
        assert_eq!(contrast::pairs(&theme).count(), 7 + 6 * 22, "{name}");
    }
}

/// The light theme's earlier colours fail, each by name and ratio:
/// the check catches a theme change that breaks a minimum.
#[test]
fn a_theme_below_a_minimum_fails_naming_the_theme_pair_and_ratio() {
    let before = Theme {
        background: Rgb888::new(0xF4, 0xF5, 0xF7),
        placeholder: Rgb888::new(0x86, 0x8E, 0x96),
        input_border: Rgb888::new(0x9A, 0xA1, 0xA9),
        focus: Rgb888::new(0xE0, 0x7A, 0x00),
        ..Theme::<Rgb888>::light()
    };
    let failures = contrast_failures("light, earlier", &before);
    for expected in [
        "light, earlier: placeholder on the text input: 3.32:1, below 4.5:1 (#868E96 on #FFFFFF)",
        "light, earlier: text input border against the background: 2.39:1, below 3:1 (#9AA1A9 on #F4F5F7)",
        "light, earlier: text input border against the input's fill: 2.61:1, below 3:1 (#9AA1A9 on #FFFFFF)",
        "light, earlier: the focus ring: 2.77:1, below 3:1 (#E07A00 on #F4F5F7)",
        // As the screen shows them, after r5g6b5, too; and the focus ring
        // on each notice it stands on.
        "light, earlier: placeholder on the text input, after r5g6b5: 3.35:1, below 4.5:1 (#848E94 on #FFFFFF)",
        "light, earlier: the focus ring, on Notice(Caution): 2.59:1, below 3:1 (#E07A00 on #FBEBDD)",
    ] {
        assert!(
            failures.iter().any(|failure| failure == expected),
            "{expected}\nnot among\n{failures:#?}"
        );
    }
}

const OPTIONS: &[&str] = &["", "A"];
const LONG_OPTIONS: &[&str] = &["Morning", "Afternoon", "Evening"];

/// A screen holding every focusable control the UI core has, at its
/// smallest - no label, no placeholder, one-letter options - and as usual,
/// laid out in `viewport` with `theme`, at natural size: in a column or a
/// row aligned to the start, so nothing is stretched.
fn every_control(theme: &Theme<Rgb888>, row: bool, viewport: Rect) -> Tree<24> {
    let mut tree: Tree<24> = Tree::new();
    let style = Style::new().padding(4).gap(4).align(Align::Start);
    let root = if row {
        tree.row(style).unwrap()
    } else {
        tree.column(style).unwrap()
    };
    tree.set_root(root).unwrap();
    let mut field = 0;
    let mut next_field = || {
        field += 1;
        FieldId(field)
    };
    for label in ["", "OK", "Continue"] {
        let button = tree.button(label, ActionId(1)).unwrap();
        tree.append(root, button).unwrap();
    }
    for placeholder in ["", "Your name"] {
        let input = tree.text_input(next_field(), placeholder).unwrap();
        tree.append(root, input).unwrap();
    }
    for label in ["", "Send me a copy"] {
        let checkbox = tree.checkbox(next_field(), label).unwrap();
        tree.append(root, checkbox).unwrap();
    }
    for options in [OPTIONS, LONG_OPTIONS] {
        let group = tree.radio_group(next_field(), options).unwrap();
        tree.append(root, group).unwrap();
    }
    tree.layout(viewport, &theme.metrics()).unwrap();
    tree
}

/// What a person taps in `kind`, by name: the control, or one option of a
/// radio group.
fn control(kind: &NodeKind, option: Option<usize>) -> String {
    match (kind, option) {
        (NodeKind::Button { label, .. }, _) => format!("button {label:?}"),
        (NodeKind::TextInput { placeholder, .. }, _) => format!("text input {placeholder:?}"),
        (NodeKind::Checkbox { label, .. }, _) => format!("checkbox {label:?}"),
        (NodeKind::RadioGroup { options, .. }, Some(index)) => {
            format!("radio option {:?}", options[index])
        }
        (other, _) => format!("{other:?}"),
    }
}

/// Every target in `tree` - each focusable control, and each option of a
/// radio group - smaller than `MIN_TARGET` either way, one line each naming
/// `name`, the control and its size.
fn small_targets<const N: usize>(name: &str, tree: &Tree<N>) -> Vec<String> {
    let mut found = Vec::new();
    let mut check = |what: String, rect: Rect| {
        let size = rect.size;
        if size.width < MIN_TARGET || size.height < MIN_TARGET {
            found.push(format!(
                "{name}: {what} is {}x{}, below {MIN_TARGET}x{MIN_TARGET}",
                size.width, size.height
            ));
        }
    };
    for (_, node) in tree.paint_order() {
        if !node.is_focusable() {
            continue;
        }
        check(control(node.kind(), None), node.rect());
        if let NodeKind::RadioGroup { options, .. } = node.kind() {
            for index in 0..options.len() {
                check(
                    control(node.kind(), Some(index)),
                    node.option_rect(index).unwrap(),
                );
            }
        }
    }
    found
}

/// Every focusable control kind, laid out by the UI core in each theme, is
/// at least 24 pixels each way, and so is each radio option; and no
/// theme's token asks for less.
#[test]
fn every_focusable_control_meets_the_minimum_target_size_in_every_theme() {
    let mut failures = Vec::new();
    for (name, theme) in Theme::<Rgb888>::all() {
        if theme.min_target < MIN_TARGET {
            failures.push(format!(
                "{name}: minimum target size {} is below {MIN_TARGET}",
                theme.min_target
            ));
        }
        for row in [false, true] {
            let tree = every_control(&theme, row, Rect::new(0, 0, 1600, 800));
            let focusable = tree.paint_order().filter(|(_, n)| n.is_focusable());
            assert_eq!(focusable.count(), 9, "{name}: every control laid out");
            failures.extend(small_targets(name, &tree));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A theme whose token asks for less than the minimum is caught by its
/// controls: the 6x10 bring-up font gives a checkbox 22 pixels, a text
/// input 20 and a radio option 16, below 24, unless the token raises them.
/// A button, with its padding of 8 above and below and a theme's button
/// height, is never below it.
#[test]
fn a_theme_below_the_minimum_target_size_fails_naming_each_control_and_its_size() {
    let small = Theme {
        type_scale: TypeScale::uniform(Font::Mono(&FONT_6X10)),
        min_target: 16,
        ..Theme::<Rgb888>::light()
    };
    let tree = every_control(&small, false, Rect::new(0, 0, 400, 800));
    assert_eq!(
        small_targets("light at 16 pixels", &tree),
        [
            "light at 16 pixels: text input \"\" is 108x20, below 24x24",
            "light at 16 pixels: text input \"Your name\" is 108x20, below 24x24",
            "light at 16 pixels: checkbox \"\" is 28x22, below 24x24",
            "light at 16 pixels: checkbox \"Send me a copy\" is 112x22, below 24x24",
            "light at 16 pixels: radio option \"\" is 22x16, below 24x24",
            "light at 16 pixels: radio option \"A\" is 22x16, below 24x24",
            "light at 16 pixels: radio option \"Morning\" is 70x16, below 24x24",
            "light at 16 pixels: radio option \"Afternoon\" is 70x16, below 24x24",
            "light at 16 pixels: radio option \"Evening\" is 70x16, below 24x24",
        ]
    );
}

/// Every theme draws its text from glyph atlases, each role at its type
/// scale's size - the light theme display 96, title 28, heading 20, body
/// 18, label 16 and caption 14; the large-text theme no text below 24 px -
/// and from the atlas twice that size at device scale 2. The bitmap fonts are for bring-up
/// and debug screens only.
#[test]
fn every_theme_draws_each_role_from_an_atlas_at_its_size_at_every_scale() {
    use nowayos_ui::{TextRole, TextStyle};
    let sizes = |theme: &Theme<Rgb888>| -> Vec<(u16, u16)> {
        TextRole::ALL
            .iter()
            .map(|&role| {
                let font = theme.type_scale.font(TextStyle::new(role));
                match (font, font.atlas(1), font.atlas(2)) {
                    (Font::Atlas(_), Some(one), Some(two)) => (one.size(), two.size()),
                    _ => panic!("{role:?}: a bitmap font, or no atlas at scale 1 or 2"),
                }
            })
            .collect()
    };
    let [(_, light), (_, large)] = Theme::<Rgb888>::all();
    assert_eq!(
        sizes(&light),
        [(96, 192), (28, 56), (20, 40), (18, 36), (16, 32), (14, 28)]
    );
    assert_eq!(
        sizes(&large),
        [(96, 192), (36, 72), (28, 56), (24, 48), (24, 48), (24, 48)]
    );
    // Every scale the display contract allows draws each role at its own
    // size, from the atlas rasterised at it - so the large-text theme never
    // falls below 24 px a unit - and layout measures with that atlas; the
    // scale past it is refused.
    for (name, theme) in Theme::<Rgb888>::all() {
        for (style, font) in theme.type_scale.styles() {
            let one = font.atlas(1).unwrap().size();
            if name == "high contrast" {
                assert!(one >= 24, "{style:?} below 24 px");
            }
            for scale in 1..=nowayos_display::MAX_SCALE {
                let atlas = font.atlas(scale).unwrap();
                assert_eq!(
                    u32::from(atlas.size()),
                    u32::from(one) * scale,
                    "{name}: {style:?} at {scale}x"
                );
                let metrics = theme.metrics_at(scale).for_style(style);
                assert!(
                    core::ptr::eq(metrics.atlas().unwrap(), atlas),
                    "{name}: {style:?} at {scale}x measured from another atlas"
                );
            }
        }
        for scale in 1..=nowayos_display::MAX_SCALE {
            assert!(theme.draws_at(scale), "{name} at {scale}x");
            let metrics = theme.metrics_at(scale);
            assert_eq!(metrics.scale(), scale, "{name}");
            assert_eq!(metrics.min_target(), theme.min_target * scale, "{name}");
        }
        assert!(!theme.draws_at(0), "{name}");
        assert!(!theme.draws_at(nowayos_display::MAX_SCALE + 1), "{name}");
    }
}

/// At device scale 2 every focusable control is at least the minimum
/// target size in layout units - twice as many pixels.
#[test]
fn every_focusable_control_meets_the_minimum_target_size_at_scale_2() {
    let mut failures = Vec::new();
    for (name, theme) in Theme::<Rgb888>::all() {
        for row in [false, true] {
            let mut tree = every_control(&theme, row, Rect::new(0, 0, 1600, 800));
            tree.layout(Rect::new(0, 0, 3200, 1600), &theme.metrics_at(2))
                .unwrap();
            for (_, node) in tree.paint_order().filter(|(_, n)| n.is_focusable()) {
                let size = node.rect().size;
                if size.width < 2 * MIN_TARGET || size.height < 2 * MIN_TARGET {
                    failures.push(format!("{name} at 2x: {}x{}", size.width, size.height));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The check fails a surface whose text is too faint, and a secondary
/// button's outline too light against a card, naming each.
#[test]
fn a_faint_surface_or_outline_fails_naming_each() {
    let mut faint = Theme::<Rgb888>::light();
    faint.surfaces.caution.text = Rgb888::new(0xB0, 0x80, 0x50);
    faint.button = Rgb888::new(0xA0, 0xB0, 0xD0);
    let failures = contrast_failures("faint", &faint);
    assert!(
        failures
            .iter()
            .any(|f| f.starts_with("faint: body text, on Notice(Caution): ")),
        "{failures:#?}"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.starts_with("faint: a secondary button's outline, on Card: ")),
        "{failures:#?}"
    );
}

/// Every theme's buttons are at least 48 layout units high, and
/// layout gives every button that height at every scale.
#[test]
fn every_theme_makes_buttons_at_least_48_units_high() {
    for (name, theme) in Theme::<Rgb888>::all() {
        assert!(theme.shape.button_height >= 48, "{name}");
        for scale in 1..=nowayos_display::MAX_SCALE {
            let mut tree: Tree<4> = Tree::new();
            let root = tree.column(Style::new().align(Align::Start)).unwrap();
            tree.set_root(root).unwrap();
            let button = tree.button("A", ActionId(1)).unwrap();
            tree.append(root, button).unwrap();
            tree.layout(
                Rect::new(0, 0, 400 * scale, 400 * scale),
                &theme.metrics_at(scale),
            )
            .unwrap();
            let height = tree.get(button).unwrap().rect().size.height;
            assert!(height >= 48 * scale, "{name} at {scale}x: {height}");
        }
    }
}
