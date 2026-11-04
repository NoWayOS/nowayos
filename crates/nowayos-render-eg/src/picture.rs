// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pictures over what lies beneath them: a premultiplied pixel blended
//! over a colour, in 8 bits a channel, then narrowed to the target's depth.

use embedded_graphics::pixelcolor::{Rgb888, RgbColor};

use crate::text::widen;

/// `rgba`, premultiplied, over `ground`: each channel the picture's plus
/// the ground's times what the picture leaves showing, `(255 - alpha) / 255`,
/// to the nearest whole value.
pub(crate) fn over<C>(rgba: [u8; 4], ground: C) -> C
where
    C: RgbColor + From<Rgb888>,
{
    let [r, g, b, alpha] = rgba;
    if alpha == u8::MAX {
        return C::from(Rgb888::new(r, g, b));
    }
    let showing = u32::from(u8::MAX.saturating_sub(alpha));
    let channel = |fore: u8, back: u8| {
        let back = u32::from(back).saturating_mul(showing).saturating_add(127) / 255;
        u8::try_from(u32::from(fore).saturating_add(back)).unwrap_or(u8::MAX)
    };
    C::from(Rgb888::new(
        channel(r, widen(ground.r(), C::MAX_R)),
        channel(g, widen(ground.g(), C::MAX_G)),
        channel(b, widen(ground.b(), C::MAX_B)),
    ))
}

#[cfg(test)]
mod tests {
    use embedded_graphics::pixelcolor::{Rgb565, Rgb888};

    use super::over;

    #[test]
    fn opaque_is_itself_transparent_the_ground_and_between_a_blend() {
        let ground = Rgb888::new(200, 100, 0);
        assert_eq!(over([1, 2, 3, 255], ground), Rgb888::new(1, 2, 3));
        assert_eq!(over([0, 0, 0, 0], ground), ground);
        // Half-covering white, premultiplied: 128 + ground x 127/255.
        assert_eq!(
            over([128, 128, 128, 128], ground),
            Rgb888::new(228, 178, 128)
        );
        // On a 16-bit target, the ground widened, then narrowed again.
        let ground = Rgb565::new(31, 0, 31);
        assert_eq!(over([0, 0, 0, 0], ground), ground);
    }
}
