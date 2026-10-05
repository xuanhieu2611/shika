//! Terminal colors. `Palette::shika` is the terminal under light chrome.
//! `Palette::shika_dark` is the same palette with the darker background.
//! The values are specified in `design/DESIGN.md`: `--term-bg`, `--term-fg`,
//! `--term-cursor`, `--term-selection`, and the soft ANSI 16.

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
    /// The terminal under the light app chrome.
    pub fn shika() -> Self {
        Self {
            ansi: ANSI,
            foreground: Rgb::hex(0xD5D9CF),
            background: Rgb::hex(0x131512),
            cursor: Rgb::hex(0xD5D9CF),
            cursor_text: Rgb::hex(0x131512),
            selection: Rgb::hex(0xD5D9CF),
            selection_alpha: 0.18,
            bold_is_bright: true,
        }
    }

    /// The terminal under the dark app chrome. Only the background changes.
    pub fn shika_dark() -> Self {
        Self {
            background: Rgb::hex(0x10120F),
            cursor_text: Rgb::hex(0x10120F),
            ..Self::shika()
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
        Self::shika()
    }
}

const ANSI: [Rgb; 16] = [
    Rgb::hex(0x1E1E1C),
    Rgb::hex(0xE0786B),
    Rgb::hex(0x8CC98A),
    Rgb::hex(0xE2BE6A),
    Rgb::hex(0x7FA8D9),
    Rgb::hex(0xC793C2),
    Rgb::hex(0x7CC0C4),
    Rgb::hex(0xD9D7D1),
    Rgb::hex(0x5F5D58),
    Rgb::hex(0xF08F82),
    Rgb::hex(0xA5DBA2),
    Rgb::hex(0xF0D08A),
    Rgb::hex(0x9DBCE6),
    Rgb::hex(0xD8ABD3),
    Rgb::hex(0x98D3D6),
    Rgb::hex(0xF4F2ED),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_colors_follow_the_xterm_cube_and_ramp() {
        let palette = Palette::shika();
        assert_eq!(palette.indexed(1), Rgb::hex(0xE0786B));
        assert_eq!(palette.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(palette.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(palette.indexed(255), Rgb::new(238, 238, 238));
    }
}
