// SPDX-License-Identifier: MIT OR Apache-2.0
//! Property tests for `RawFramebuffer`: drawing anywhere - off every edge, at
//! extreme coordinates - writes exactly the in-layout pixels, never a byte
//! outside the layout or in a row's padding, and never panics; and a solid
//! fill, stored in runs a row at a time, stores exactly the bytes a
//! pixel at a time would.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]

use embedded_graphics::Pixel;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, Point, Size};
use embedded_graphics::primitives::Rectangle;
use nowayos_display::{FramebufferLayout, PixelFormat};
use nowayos_render_eg::RawFramebuffer;
use proptest::prelude::*;

const FORMATS: [PixelFormat; 5] = [
    PixelFormat::Rgb565,
    PixelFormat::Rgb888,
    PixelFormat::Bgr888,
    PixelFormat::Xrgb8888,
    PixelFormat::Xbgr8888,
];

fn coordinate() -> impl Strategy<Value = i32> {
    prop_oneof![-20..40_i32, any::<i32>(), Just(i32::MIN), Just(i32::MAX)]
}

proptest! {
    #[test]
    fn only_pixels_inside_the_layout_are_written(
        width in 1..24_u32,
        height in 1..24_u32,
        format in 0..FORMATS.len(),
        padding in 0..8_u32,
        pixels in proptest::collection::vec((coordinate(), coordinate(), any::<(u8, u8, u8)>()), 0..64),
    ) {
        let format = FORMATS[format];
        let stride = width * u32::from(format.bytes_per_pixel()) + padding;
        let layout = FramebufferLayout::new(width, height, stride, format).unwrap();
        let mut memory = vec![0xA5_u8; layout.byte_len() + 8];
        let mut expected = memory.clone();
        for &(x, y, (r, g, b)) in &pixels {
            if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
                layout.write_pixel(&mut expected, x, y, (r, g, b));
            }
        }
        let mut fb = RawFramebuffer::new(layout, &mut memory[..]).unwrap();
        fb.draw_iter(pixels.iter().map(|&(x, y, (r, g, b))| Pixel(Point::new(x, y), Rgb888::new(r, g, b)))).unwrap();
        prop_assert_eq!(&memory, &expected);
        let row = (width * u32::from(format.bytes_per_pixel())) as usize;
        for y in 0..height as usize {
            let start = y * stride as usize;
            prop_assert!(memory[start + row..start + stride as usize].iter().all(|&b| b == 0xA5));
        }
        prop_assert!(memory[layout.byte_len()..].iter().all(|&b| b == 0xA5));
    }
}

proptest! {
    #[test]
    fn a_solid_fill_stores_what_each_pixel_would(
        width in 1..700_u32,
        height in 1..6_u32,
        format in 0..FORMATS.len(),
        padding in 0..8_u32,
        x in coordinate(),
        y in coordinate(),
        w in prop_oneof![0..800_u32, any::<u32>()],
        h in prop_oneof![0..8_u32, any::<u32>()],
        (r, g, b) in any::<(u8, u8, u8)>(),
    ) {
        let format = FORMATS[format];
        let stride = width * u32::from(format.bytes_per_pixel()) + padding;
        let layout = FramebufferLayout::new(width, height, stride, format).unwrap();
        let mut memory = vec![0xA5_u8; layout.byte_len() + 8];
        let mut expected = memory.clone();
        for py in 0..height {
            for px in 0..width {
                let inside = i64::from(px) >= i64::from(x)
                    && i64::from(px) < i64::from(x) + i64::from(w)
                    && i64::from(py) >= i64::from(y)
                    && i64::from(py) < i64::from(y) + i64::from(h);
                if inside {
                    layout.write_pixel(&mut expected, px, py, (r, g, b));
                }
            }
        }
        let mut fb = RawFramebuffer::new(layout, &mut memory[..]).unwrap();
        fb.fill_solid(&Rectangle::new(Point::new(x, y), Size::new(w, h)), Rgb888::new(r, g, b)).unwrap();
        prop_assert_eq!(&memory, &expected);
    }
}
