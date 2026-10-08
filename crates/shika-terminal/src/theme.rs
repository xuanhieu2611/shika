//! Terminal colors. `Palette::shika_dark` and `Palette::shika_light` are
//! the terminal halves of the Shika Dark and Shika Light themes: the
//! terminal shares the agent column's background, so the window reads as
//! one surface. The values are specified in `design/DESIGN.md`: `--term-bg`,
//! `--term-fg`, `--term-cursor`, `--term-selection`, and the soft ANSI 16.

use crate::types::Rgb;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// ANSI 0 to 15: black, red, green, yellow, blue, magenta, cyan, white,
    /// then the bright versions in the same order.
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Text drawn on top of a block cursor.
    pub cursor_text: Rgb,
    pub selection: Rgb,
    /// Selection is drawn as a wash over the cell background.
    pub selection_alpha: f32,
    /// Bold text in ANSI 0 to 7 is drawn with the bright color, like
    /// xterm.js `drawBoldTextInBrightColors`.
    pub bold_is_bright: bool,
}

impl Palette {
    /// Shika Dark: the column's warm charcoal.
    pub const fn shika_dark() -> Self {
        Self {
            ansi: ANSI_DARK,
            foreground: Rgb::hex(0xD5D9CF),
            background: Rgb::hex(0x1A1C19),
            cursor: Rgb::hex(0xD5D9CF),
            cursor_text: Rgb::hex(0x1A1C19),
            selection: Rgb::hex(0xD5D9CF),
            selection_alpha: 0.18,
            bold_is_bright: true,
        }
    }

    /// Shika Light: the column's sage-tinted paper. Bold keeps its color,
    /// because the bright colors are the lighter, lower-contrast set here.
    pub const fn shika_light() -> Self {
        Self {
            ansi: ANSI_LIGHT,
            foreground: Rgb::hex(0x262824),
            background: Rgb::hex(0xF1F2EC),
            cursor: Rgb::hex(0x262824),
            cursor_text: Rgb::hex(0xF1F2EC),
            selection: Rgb::hex(0x262824),
            selection_alpha: 0.14,
            bold_is_bright: false,
        }
    }

    /// The same colors on another background, such as the frosted column
    /// tint. The block cursor's text follows it.
    pub fn with_background(self, background: Rgb) -> Self {
        Self {
            background,
            cursor_text: background,
            ..self
        }
    }

    /// Any of the 256 indexed colors: the 16 from the palette, then the
    /// 6x6x6 cube, then the 24 step gray ramp.
    pub fn indexed(&self, index: u8) -> Rgb {
        let index = index as usize;
        if index < 16 {
            return self.ansi[index];
        }
        if index < 232 {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
            return Rgb::new(level(i / 36), level((i / 6) % 6), level(i % 6));
        }
        let gray = (8 + (index - 232) * 10) as u8;
        Rgb::new(gray, gray, gray)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::shika_dark()
    }
}

const ANSI_DARK: [Rgb; 16] = [
    Rgb::hex(0x2E312B),
    Rgb::hex(0xE0786B),
    Rgb::hex(0x8CC98A),
    Rgb::hex(0xE2BE6A),
    Rgb::hex(0x7FA8D9),
    Rgb::hex(0xC793C2),
    Rgb::hex(0x7CC0C4),
    Rgb::hex(0xD9D7D1),
    Rgb::hex(0x6E6C66),
    Rgb::hex(0xF08F82),
    Rgb::hex(0xA5DBA2),
    Rgb::hex(0xF0D08A),
    Rgb::hex(0x9DBCE6),
    Rgb::hex(0xD8ABD3),
    Rgb::hex(0x98D3D6),
    Rgb::hex(0xF4F2ED),
];

/// Every normal color meets 4.5:1 on `#F1F2EC`; the bright set meets 3.5:1
/// except bright white, which stays a light grey as on other light themes.
const ANSI_LIGHT: [Rgb; 16] = [
    Rgb::hex(0x262824),
    Rgb::hex(0xB2463A),
    Rgb::hex(0x3D7A3C),
    Rgb::hex(0x86630F),
    Rgb::hex(0x35659F),
    Rgb::hex(0x8A4A86),
    Rgb::hex(0x2B7579),
    Rgb::hex(0x6C7166),
    Rgb::hex(0x7A7F73),
    Rgb::hex(0xC4594B),
    Rgb::hex(0x4C8A49),
    Rgb::hex(0x9A741A),
    Rgb::hex(0x4A78B3),
    Rgb::hex(0x9D5C98),
    Rgb::hex(0x3A878B),
    Rgb::hex(0x868B7E),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_colors_follow_the_xterm_cube_and_ramp() {
        let palette = Palette::shika_dark();
        assert_eq!(palette.indexed(1), Rgb::hex(0xE0786B));
        assert_eq!(palette.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(palette.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(palette.indexed(255), Rgb::new(238, 238, 238));
    }
}
