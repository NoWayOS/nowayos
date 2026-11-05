// SPDX-License-Identifier: MIT OR Apache-2.0
//! Images (builder contract v15): a picture's node is its own size in
//! layout units at the device scale, to the nearest whole pixel, until a
//! style gives it another; its words for anyone who cannot see it are
//! checked as any text is.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use nowayos_display::{DeviceScale, Rect};
use nowayos_ui::builder::{self as ui, UiBuilder};
use nowayos_ui::pictures::{Fit, samples};
use nowayos_ui::{Align, Length, NodeKind, Style, TextMetrics, Tree, UiError};

fn laid_out(scale: DeviceScale, style: Option<Style>) -> Rect {
    let mut tree: Tree<4> = Tree::new();
    let root = tree.column(Style::new().align(Align::Start)).unwrap();
    tree.set_root(root).unwrap();
    let image = tree
        .image(&samples::LANDSCAPE, Fit::Cover, "A lake at sunset")
        .unwrap();
    if let Some(style) = style {
        tree.set_style(image, style).unwrap();
    }
    tree.append(root, image).unwrap();
    let metrics = TextMetrics::new(8, 16).with_scale(scale);
    tree.layout(Rect::new(0, 0, 4000, 4000), &metrics).unwrap();
    tree.get(image).unwrap().rect()
}

#[test]
fn a_picture_is_its_own_size_at_every_scale_until_a_style_says_otherwise() {
    for (quarters, size) in [
        (4, (160, 100)),
        (5, (200, 125)),
        (8, (320, 200)),
        (10, (400, 250)),
        (16, (640, 400)),
    ] {
        let rect = laid_out(DeviceScale::from_quarters(quarters).unwrap(), None);
        assert_eq!(
            (rect.size.width, rect.size.height),
            size,
            "{quarters} quarters"
        );
    }
    let fixed = Style::new()
        .width(Length::Fixed(50))
        .height(Length::Fixed(30));
    let rect = laid_out(DeviceScale::TWO, Some(fixed));
    assert_eq!((rect.size.width, rect.size.height), (100, 60));
}

#[test]
fn generated_screens_build_an_image_through_the_builder() {
    fn build<B: UiBuilder>(b: &mut B) -> Result<B::Node, B::Error> {
        let picture = b.image(
            &samples::BADGE,
            ui::Fit::Contain,
            "",
            ui::Layout::new().width(60),
        )?;
        let title = b.text("Done")?;
        b.column(ui::Layout::new().gap(8), &[picture, title])
    }
    let mut tree: Tree<4> = Tree::new();
    let root = build(&mut tree).unwrap();
    tree.set_root(root).unwrap();
    let first = tree.children(root).next().unwrap();
    assert_eq!(
        tree.get(first).unwrap().style().width,
        nowayos_ui::Length::Fixed(60)
    );
    assert!(matches!(
        tree.get(first).unwrap().kind(),
        NodeKind::Image {
            fit: Fit::Contain,
            alt: "",
            ..
        }
    ));
}

#[test]
fn its_words_are_checked_as_any_text_is() {
    let mut tree: Tree<4> = Tree::new();
    assert_eq!(
        tree.image(&samples::LANDSCAPE, Fit::Fill, "two\nlines"),
        Err(UiError::ControlCharacter)
    );
    assert_eq!(
        tree.image(&samples::LANDSCAPE, Fit::Fill, "zero\u{200B}width"),
        Err(UiError::InvisibleCharacter)
    );
}
