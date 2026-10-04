//! Box drawing and block characters drawn as shapes instead of font glyphs.
//! The design's line height is 1.52, so font glyphs for `│` or `█` leave
//! gaps between rows. Drawing them to the cell edges keeps Claude Code's
//! input frame and logo joined, the way native terminals do.
//!
//! Dashed, double, and diagonal lines are left to the font.

/// Stroke weight of one arm: 0 none, 1 light, 2 heavy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arms {
    pub left: u8,
    pub right: u8,
    pub up: u8,
    pub down: u8,
}

/// Which two arms a rounded corner joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Corner {
    DownRight,
    DownLeft,
    UpLeft,
    UpRight,
}

/// A rectangle in cell fractions: left, top, right, bottom, from 0 to 1.
pub(crate) type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BoxGlyph {
    Lines(Arms),
    Arc(Corner),
    /// Filled rectangles at the given opacity of the text color.
    Blocks(&'static [Rect], f32),
}

pub(crate) fn lookup(ch: char) -> Option<BoxGlyph> {
    let code = ch as u32;
    match code {
        0x2500..=0x257F => {
            match ch {
                '\u{256D}' => return Some(BoxGlyph::Arc(Corner::DownRight)),
                '\u{256E}' => return Some(BoxGlyph::Arc(Corner::DownLeft)),
                '\u{256F}' => return Some(BoxGlyph::Arc(Corner::UpLeft)),
                '\u{2570}' => return Some(BoxGlyph::Arc(Corner::UpRight)),
                _ => {}
            }
            LINES
                .iter()
                .find(|(c, _)| *c == ch)
                .map(|(_, [left, right, up, down])| {
                    BoxGlyph::Lines(Arms {
                        left: *left,
                        right: *right,
                        up: *up,
                        down: *down,
                    })
                })
        }
        0x2580..=0x259F => block(ch),
        _ => None,
    }
}

fn block(ch: char) -> Option<BoxGlyph> {
    const E: f32 = 1.0 / 8.0;
    let rects: &'static [Rect] = match ch {
        '\u{2580}' => &[(0.0, 0.0, 1.0, 0.5)],
        '\u{2581}' => &[(0.0, 1.0 - E, 1.0, 1.0)],
        '\u{2582}' => &[(0.0, 1.0 - 2.0 * E, 1.0, 1.0)],
        '\u{2583}' => &[(0.0, 1.0 - 3.0 * E, 1.0, 1.0)],
        '\u{2584}' => &[(0.0, 0.5, 1.0, 1.0)],
        '\u{2585}' => &[(0.0, 1.0 - 5.0 * E, 1.0, 1.0)],
        '\u{2586}' => &[(0.0, 1.0 - 6.0 * E, 1.0, 1.0)],
        '\u{2587}' => &[(0.0, 1.0 - 7.0 * E, 1.0, 1.0)],
        '\u{2588}' => &[(0.0, 0.0, 1.0, 1.0)],
        '\u{2589}' => &[(0.0, 0.0, 7.0 * E, 1.0)],
        '\u{258A}' => &[(0.0, 0.0, 6.0 * E, 1.0)],
        '\u{258B}' => &[(0.0, 0.0, 5.0 * E, 1.0)],
        '\u{258C}' => &[(0.0, 0.0, 0.5, 1.0)],
        '\u{258D}' => &[(0.0, 0.0, 3.0 * E, 1.0)],
        '\u{258E}' => &[(0.0, 0.0, 2.0 * E, 1.0)],
        '\u{258F}' => &[(0.0, 0.0, E, 1.0)],
        '\u{2590}' => &[(0.5, 0.0, 1.0, 1.0)],
        '\u{2591}' => return Some(BoxGlyph::Blocks(&[(0.0, 0.0, 1.0, 1.0)], 0.25)),
        '\u{2592}' => return Some(BoxGlyph::Blocks(&[(0.0, 0.0, 1.0, 1.0)], 0.5)),
        '\u{2593}' => return Some(BoxGlyph::Blocks(&[(0.0, 0.0, 1.0, 1.0)], 0.75)),
        '\u{2594}' => &[(0.0, 0.0, 1.0, E)],
        '\u{2595}' => &[(1.0 - E, 0.0, 1.0, 1.0)],
        '\u{2596}' => &[LL],
        '\u{2597}' => &[LR],
        '\u{2598}' => &[UL],
        '\u{2599}' => &[UL, LL, LR],
        '\u{259A}' => &[UL, LR],
        '\u{259B}' => &[UL, UR, LL],
        '\u{259C}' => &[UL, UR, LR],
        '\u{259D}' => &[UR],
        '\u{259E}' => &[UR, LL],
        '\u{259F}' => &[UR, LL, LR],
        _ => return None,
    };
    Some(BoxGlyph::Blocks(rects, 1.0))
}

const UL: Rect = (0.0, 0.0, 0.5, 0.5);
const UR: Rect = (0.5, 0.0, 1.0, 0.5);
const LL: Rect = (0.0, 0.5, 0.5, 1.0);
const LR: Rect = (0.5, 0.5, 1.0, 1.0);

/// Light and heavy lines: [left, right, up, down].
const LINES: &[(char, [u8; 4])] = &[
    ('\u{2500}', [1, 1, 0, 0]),
    ('\u{2501}', [2, 2, 0, 0]),
    ('\u{2502}', [0, 0, 1, 1]),
    ('\u{2503}', [0, 0, 2, 2]),
    ('\u{250C}', [0, 1, 0, 1]),
    ('\u{250D}', [0, 2, 0, 1]),
    ('\u{250E}', [0, 1, 0, 2]),
    ('\u{250F}', [0, 2, 0, 2]),
    ('\u{2510}', [1, 0, 0, 1]),
    ('\u{2511}', [2, 0, 0, 1]),
    ('\u{2512}', [1, 0, 0, 2]),
    ('\u{2513}', [2, 0, 0, 2]),
    ('\u{2514}', [0, 1, 1, 0]),
    ('\u{2515}', [0, 2, 1, 0]),
    ('\u{2516}', [0, 1, 2, 0]),
    ('\u{2517}', [0, 2, 2, 0]),
    ('\u{2518}', [1, 0, 1, 0]),
    ('\u{2519}', [2, 0, 1, 0]),
    ('\u{251A}', [1, 0, 2, 0]),
    ('\u{251B}', [2, 0, 2, 0]),
    ('\u{251C}', [0, 1, 1, 1]),
    ('\u{251D}', [0, 2, 1, 1]),
    ('\u{251E}', [0, 1, 2, 1]),
    ('\u{251F}', [0, 1, 1, 2]),
    ('\u{2520}', [0, 1, 2, 2]),
    ('\u{2521}', [0, 2, 2, 1]),
    ('\u{2522}', [0, 2, 1, 2]),
    ('\u{2523}', [0, 2, 2, 2]),
    ('\u{2524}', [1, 0, 1, 1]),
    ('\u{2525}', [2, 0, 1, 1]),
    ('\u{2526}', [1, 0, 2, 1]),
    ('\u{2527}', [1, 0, 1, 2]),
    ('\u{2528}', [1, 0, 2, 2]),
    ('\u{2529}', [2, 0, 2, 1]),
    ('\u{252A}', [2, 0, 1, 2]),
    ('\u{252B}', [2, 0, 2, 2]),
    ('\u{252C}', [1, 1, 0, 1]),
    ('\u{252D}', [2, 1, 0, 1]),
    ('\u{252E}', [1, 2, 0, 1]),
    ('\u{252F}', [2, 2, 0, 1]),
    ('\u{2530}', [1, 1, 0, 2]),
    ('\u{2531}', [2, 1, 0, 2]),
    ('\u{2532}', [1, 2, 0, 2]),
    ('\u{2533}', [2, 2, 0, 2]),
    ('\u{2534}', [1, 1, 1, 0]),
    ('\u{2535}', [2, 1, 1, 0]),
    ('\u{2536}', [1, 2, 1, 0]),
    ('\u{2537}', [2, 2, 1, 0]),
    ('\u{2538}', [1, 1, 2, 0]),
    ('\u{2539}', [2, 1, 2, 0]),
    ('\u{253A}', [1, 2, 2, 0]),
    ('\u{253B}', [2, 2, 2, 0]),
    ('\u{253C}', [1, 1, 1, 1]),
    ('\u{253D}', [2, 1, 1, 1]),
    ('\u{253E}', [1, 2, 1, 1]),
    ('\u{253F}', [2, 2, 1, 1]),
    ('\u{2540}', [1, 1, 2, 1]),
    ('\u{2541}', [1, 1, 1, 2]),
    ('\u{2542}', [1, 1, 2, 2]),
    ('\u{2543}', [2, 1, 2, 1]),
    ('\u{2544}', [1, 2, 2, 1]),
    ('\u{2545}', [2, 1, 1, 2]),
    ('\u{2546}', [1, 2, 1, 2]),
    ('\u{2547}', [2, 2, 2, 1]),
    ('\u{2548}', [2, 2, 1, 2]),
    ('\u{2549}', [2, 1, 2, 2]),
    ('\u{254A}', [1, 2, 2, 2]),
    ('\u{254B}', [2, 2, 2, 2]),
    ('\u{2574}', [1, 0, 0, 0]),
    ('\u{2575}', [0, 0, 1, 0]),
    ('\u{2576}', [0, 1, 0, 0]),
    ('\u{2577}', [0, 0, 0, 1]),
    ('\u{2578}', [2, 0, 0, 0]),
    ('\u{2579}', [0, 0, 2, 0]),
    ('\u{257A}', [0, 2, 0, 0]),
    ('\u{257B}', [0, 0, 0, 2]),
    ('\u{257C}', [1, 2, 0, 0]),
    ('\u{257D}', [0, 0, 1, 2]),
    ('\u{257E}', [2, 1, 0, 0]),
    ('\u{257F}', [0, 0, 2, 1]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_frame_characters_are_drawn() {
        assert_eq!(
            lookup('─'),
            Some(BoxGlyph::Lines(Arms {
                left: 1,
                right: 1,
                up: 0,
                down: 0
            }))
        );
        assert_eq!(
            lookup('│'),
            Some(BoxGlyph::Lines(Arms {
                left: 0,
                right: 0,
                up: 1,
                down: 1
            }))
        );
        assert_eq!(lookup('╭'), Some(BoxGlyph::Arc(Corner::DownRight)));
        assert_eq!(lookup('╯'), Some(BoxGlyph::Arc(Corner::UpLeft)));
        assert!(matches!(lookup('█'), Some(BoxGlyph::Blocks(_, a)) if a == 1.0));
        assert!(matches!(lookup('▛'), Some(BoxGlyph::Blocks(r, _)) if r.len() == 3));
    }

    #[test]
    fn dashed_double_and_plain_text_use_the_font() {
        assert_eq!(lookup('┄'), None);
        assert_eq!(lookup('═'), None);
        assert_eq!(lookup('╱'), None);
        assert_eq!(lookup('a'), None);
    }
}
