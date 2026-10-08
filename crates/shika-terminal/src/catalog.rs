//! The theme catalog: every theme a user can pick in Settings. A theme is a
//! terminal palette plus, optionally, the UI surfaces its own spec names.
//! Shika Light and Shika Dark have hand-tuned chrome in the app
//! (`appearance::chrome_for`); every other theme's chrome is derived from
//! its palette and `ui`.

use crate::theme::Palette;
use crate::types::Rgb;

pub const SHIKA_LIGHT: &str = "shika-light";
pub const SHIKA_DARK: &str = "shika-dark";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Stable id saved in `settings.json`, kebab-case: `catppuccin-mocha`.
    pub id: &'static str,
    /// Shown in Settings: `Catppuccin Mocha`.
    pub name: &'static str,
    pub dark: bool,
    pub palette: Palette,
    /// Surfaces from the theme's own spec, when it defines them. None means
    /// the app derives them from the palette.
    pub ui: Option<ThemeUi>,
}

/// UI surfaces taken from a theme's published palette, for themes that
/// define more than terminal colors (Catppuccin's mantle and surface0,
/// Rose Pine's surface and muted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeUi {
    /// A step away from the background, toward the frame: the terminal
    /// header, key caps, and segmented tracks.
    pub mantle: Rgb,
    /// A raised surface on the background: the selected card, dialogs, the
    /// picker.
    pub surface: Rgb,
    /// Secondary text that still meets 4.5:1 on the background.
    pub muted: Rgb,
}

pub static THEMES: &[Theme] = &[
    Theme {
        id: SHIKA_LIGHT,
        name: "Shika Light",
        dark: false,
        palette: Palette::shika_light(),
        ui: None,
    },
    Theme {
        id: SHIKA_DARK,
        name: "Shika Dark",
        dark: true,
        palette: Palette::shika_dark(),
        ui: None,
    },
    CATPPUCCIN_LATTE,
    CATPPUCCIN_FRAPPE,
    CATPPUCCIN_MACCHIATO,
    CATPPUCCIN_MOCHA,
    ROSE_PINE,
    ROSE_PINE_MOON,
    ROSE_PINE_DAWN,
    TOKYO_NIGHT,
    TOKYO_NIGHT_STORM,
    TOKYO_NIGHT_MOON,
    TOKYO_NIGHT_DAY,
    DRACULA,
    ALUCARD,
    GRUVBOX_DARK,
    GRUVBOX_LIGHT,
];

/// The theme with this id, if the catalog has it.
pub fn find(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// Light or dark themes, in catalog order.
pub fn themes(dark: bool) -> impl Iterator<Item = &'static Theme> {
    THEMES.iter().filter(move |theme| theme.dark == dark)
}

/// ANSI 0 to 15 written as hex, so each palette reads like its source file.
const fn ansi(hex: [u32; 16]) -> [Rgb; 16] {
    let mut colors = [Rgb::new(0, 0, 0); 16];
    let mut i = 0;
    while i < 16 {
        colors[i] = Rgb::hex(hex[i]);
        i += 1;
    }
    colors
}

const fn ui(mantle: u32, surface: u32, muted: u32) -> Option<ThemeUi> {
    Some(ThemeUi {
        mantle: Rgb::hex(mantle),
        surface: Rgb::hex(surface),
        muted: Rgb::hex(muted),
    })
}

// Catppuccin: catppuccin/palette v1.8.0 and its official Ghostty port,
// catppuccin/ghostty. The port repeats the normal colors as the bright
// ones, and so does catppuccin/alacritty; the palette's separate
// `ansiColors` brights are not used by either port. The port's selection is
// overlay2 mixed 20% into base, which the wash reproduces exactly. `ui` is
// mantle, surface0, and subtext0; Latte's subtext0 is 4.37:1 on base, so
// Latte uses subtext1.

const CATPPUCCIN_LATTE: Theme = Theme {
    id: "catppuccin-latte",
    name: "Catppuccin Latte",
    dark: false,
    palette: Palette {
        ansi: ansi([
            0x5C5F77, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0xACB0BE,
            0x6C6F85, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0xBCC0CC,
        ]),
        foreground: Rgb::hex(0x4C4F69),
        background: Rgb::hex(0xEFF1F5),
        cursor: Rgb::hex(0xDC8A78),
        cursor_text: Rgb::hex(0xEFF1F5),
        selection: Rgb::hex(0x7C7F93),
        selection_alpha: 0.2,
        bold_is_bright: false,
    },
    ui: ui(0xE6E9EF, 0xCCD0DA, 0x5C5F77),
};

const CATPPUCCIN_FRAPPE: Theme = Theme {
    id: "catppuccin-frappe",
    name: "Catppuccin Frappé",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x51576D, 0xE78284, 0xA6D189, 0xE5C890, 0x8CAAEE, 0xF4B8E4, 0x81C8BE, 0xA5ADCE,
            0x626880, 0xE78284, 0xA6D189, 0xE5C890, 0x8CAAEE, 0xF4B8E4, 0x81C8BE, 0xB5BFE2,
        ]),
        foreground: Rgb::hex(0xC6D0F5),
        background: Rgb::hex(0x303446),
        cursor: Rgb::hex(0xF2D5CF),
        cursor_text: Rgb::hex(0x232634),
        selection: Rgb::hex(0x949CBB),
        selection_alpha: 0.2,
        bold_is_bright: true,
    },
    ui: ui(0x292C3C, 0x414559, 0xA5ADCE),
};

const CATPPUCCIN_MACCHIATO: Theme = Theme {
    id: "catppuccin-macchiato",
    name: "Catppuccin Macchiato",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x494D64, 0xED8796, 0xA6DA95, 0xEED49F, 0x8AADF4, 0xF5BDE6, 0x8BD5CA, 0xA5ADCB,
            0x5B6078, 0xED8796, 0xA6DA95, 0xEED49F, 0x8AADF4, 0xF5BDE6, 0x8BD5CA, 0xB8C0E0,
        ]),
        foreground: Rgb::hex(0xCAD3F5),
        background: Rgb::hex(0x24273A),
        cursor: Rgb::hex(0xF4DBD6),
        cursor_text: Rgb::hex(0x181926),
        selection: Rgb::hex(0x939AB7),
        selection_alpha: 0.2,
        bold_is_bright: true,
    },
    ui: ui(0x1E2030, 0x363A4F, 0xA5ADCB),
};

const CATPPUCCIN_MOCHA: Theme = Theme {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x45475A, 0xF38BA8, 0xA6E3A1, 0xF9E2AF, 0x89B4FA, 0xF5C2E7, 0x94E2D5, 0xA6ADC8,
            0x585B70, 0xF38BA8, 0xA6E3A1, 0xF9E2AF, 0x89B4FA, 0xF5C2E7, 0x94E2D5, 0xBAC2DE,
        ]),
        foreground: Rgb::hex(0xCDD6F4),
        background: Rgb::hex(0x1E1E2E),
        cursor: Rgb::hex(0xF5E0DC),
        cursor_text: Rgb::hex(0x11111B),
        selection: Rgb::hex(0x9399B2),
        selection_alpha: 0.2,
        bold_is_bright: true,
    },
    ui: ui(0x181825, 0x313244, 0xA6ADC8),
};

// Rosé Pine: rose-pine/rose-pine-palette and its official Ghostty port,
// rose-pine/ghostty. The port's selection is highlight-med; a wash of the
// text color reproduces it within 4 RGB units on Rosé Pine, 2 on Moon, and
// 1 on Dawn. The palette has no tone darker than base, so `ui` follows its
// documented layering: surface (panels) is the mantle, overlay (popovers,
// dialogs) is the surface, and subtle is the muted text. Dawn's subtle is
// 4.02:1 on its base and no other secondary text color passes, so Dawn has
// no `ui`.

const ROSE_PINE: Theme = Theme {
    id: "rose-pine",
    name: "Rosé Pine",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x26233A, 0xEB6F92, 0x31748F, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEBBCBA, 0xE0DEF4,
            0x6E6A86, 0xEB6F92, 0x31748F, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEBBCBA, 0xE0DEF4,
        ]),
        foreground: Rgb::hex(0xE0DEF4),
        background: Rgb::hex(0x191724),
        cursor: Rgb::hex(0xE0DEF4),
        cursor_text: Rgb::hex(0x191724),
        selection: Rgb::hex(0xE0DEF4),
        selection_alpha: 0.2,
        bold_is_bright: true,
    },
    ui: ui(0x1F1D2E, 0x26233A, 0x908CAA),
};

const ROSE_PINE_MOON: Theme = Theme {
    id: "rose-pine-moon",
    name: "Rosé Pine Moon",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x393552, 0xEB6F92, 0x3E8FB0, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEA9A97, 0xE0DEF4,
            0x6E6A86, 0xEB6F92, 0x3E8FB0, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEA9A97, 0xE0DEF4,
        ]),
        foreground: Rgb::hex(0xE0DEF4),
        background: Rgb::hex(0x232136),
        cursor: Rgb::hex(0xE0DEF4),
        cursor_text: Rgb::hex(0x232136),
        selection: Rgb::hex(0xE0DEF4),
        selection_alpha: 0.18,
        bold_is_bright: true,
    },
    ui: ui(0x2A273F, 0x393552, 0x908CAA),
};

const ROSE_PINE_DAWN: Theme = Theme {
    id: "rose-pine-dawn",
    name: "Rosé Pine Dawn",
    dark: false,
    palette: Palette {
        ansi: ansi([
            0xF2E9E1, 0xB4637A, 0x286983, 0xEA9D34, 0x56949F, 0x907AA9, 0xD7827E, 0x575279,
            0x9893A5, 0xB4637A, 0x286983, 0xEA9D34, 0x56949F, 0x907AA9, 0xD7827E, 0x575279,
        ]),
        foreground: Rgb::hex(0x575279),
        background: Rgb::hex(0xFAF4ED),
        cursor: Rgb::hex(0x575279),
        cursor_text: Rgb::hex(0xFAF4ED),
        selection: Rgb::hex(0x575279),
        selection_alpha: 0.16,
        bold_is_bright: false,
    },
    ui: None,
};

// Tokyo Night: folke/tokyonight.nvim, its official Ghostty and WezTerm
// extras (`extras/ghostty`, `extras/wezterm`), which agree. The cursor text
// is the background, from the WezTerm extra. The selection is bg_visual,
// which the theme computes as blue0 blended 40% into bg, so the wash
// reproduces it exactly. `ui` is bg_dark, bg_highlight, and fg_dark, from
// `extras/lua`. Day's fg_dark is 3.57:1 on its bg and its only passing text
// color is fg itself, so Day has no `ui`.

const TOKYO_NIGHT: Theme = Theme {
    id: "tokyo-night",
    name: "Tokyo Night",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x15161E, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6,
            0x414868, 0xFF899D, 0x9FE044, 0xFABA4A, 0x8DB0FF, 0xC7A9FF, 0xA4DAFF, 0xC0CAF5,
        ]),
        foreground: Rgb::hex(0xC0CAF5),
        background: Rgb::hex(0x1A1B26),
        cursor: Rgb::hex(0xC0CAF5),
        cursor_text: Rgb::hex(0x1A1B26),
        selection: Rgb::hex(0x3D59A1),
        selection_alpha: 0.4,
        bold_is_bright: true,
    },
    ui: ui(0x16161E, 0x292E42, 0xA9B1D6),
};

const TOKYO_NIGHT_STORM: Theme = Theme {
    id: "tokyo-night-storm",
    name: "Tokyo Night Storm",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x1D202F, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6,
            0x414868, 0xFF899D, 0x9FE044, 0xFABA4A, 0x8DB0FF, 0xC7A9FF, 0xA4DAFF, 0xC0CAF5,
        ]),
        foreground: Rgb::hex(0xC0CAF5),
        background: Rgb::hex(0x24283B),
        cursor: Rgb::hex(0xC0CAF5),
        cursor_text: Rgb::hex(0x24283B),
        selection: Rgb::hex(0x3D59A1),
        selection_alpha: 0.4,
        bold_is_bright: true,
    },
    ui: ui(0x1F2335, 0x292E42, 0xA9B1D6),
};

const TOKYO_NIGHT_MOON: Theme = Theme {
    id: "tokyo-night-moon",
    name: "Tokyo Night Moon",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x1B1D2B, 0xFF757F, 0xC3E88D, 0xFFC777, 0x82AAFF, 0xC099FF, 0x86E1FC, 0x828BB8,
            0x444A73, 0xFF8D94, 0xC7FB6D, 0xFFD8AB, 0x9AB8FF, 0xCAABFF, 0xB2EBFF, 0xC8D3F5,
        ]),
        foreground: Rgb::hex(0xC8D3F5),
        background: Rgb::hex(0x222436),
        cursor: Rgb::hex(0xC8D3F5),
        cursor_text: Rgb::hex(0x222436),
        selection: Rgb::hex(0x3E68D7),
        selection_alpha: 0.4,
        bold_is_bright: true,
    },
    ui: ui(0x1E2030, 0x2F334D, 0x828BB8),
};

const TOKYO_NIGHT_DAY: Theme = Theme {
    id: "tokyo-night-day",
    name: "Tokyo Night Day",
    dark: false,
    palette: Palette {
        ansi: ansi([
            0xB4B5B9, 0xF52A65, 0x587539, 0x8C6C3E, 0x2E7DE9, 0x9854F1, 0x007197, 0x6172B0,
            0xA1A6C5, 0xFF4774, 0x5C8524, 0xA27629, 0x358AFF, 0xA463FF, 0x007EA8, 0x3760BF,
        ]),
        foreground: Rgb::hex(0x3760BF),
        background: Rgb::hex(0xE1E2E7),
        cursor: Rgb::hex(0x3760BF),
        cursor_text: Rgb::hex(0xE1E2E7),
        selection: Rgb::hex(0x7890DD),
        selection_alpha: 0.4,
        bold_is_bright: false,
    },
    ui: None,
};

// Dracula: the official specification (draculatheme.com/spec) and the
// official Ghostty port, dracula/ghostty, which agree with dracula/alacritty.
// The port's selection is the opaque Selection color #44475A (dracula/wezterm
// draws it at 50% instead); a wash of the foreground at 16% lands within 6
// RGB units of it. Comment, the secondary text color, is 3.03:1 on the
// background, so Dracula has no `ui`.
//
// Alucard is the light variant the specification names Alucard Classic,
// distinct from Dracula Pro's Alucard. No terminal emulator port exists yet,
// so the colors are the specification's ANSI palette and the terminal
// colors of its reference implementation, dracula/cursor (`alucard.yml`):
// the selection is Purple at alpha 0x60, exactly. That file also gives the
// cursor's text the foreground, which would hide the glyph, so the cursor
// text is the background. `ui` is the specification's Background Dark
// (sidebar, status bar), Background Light (menus, dropdowns), and Comment.

const DRACULA: Theme = Theme {
    id: "dracula",
    name: "Dracula",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x21222C, 0xFF5555, 0x50FA7B, 0xF1FA8C, 0xBD93F9, 0xFF79C6, 0x8BE9FD, 0xF8F8F2,
            0x6272A4, 0xFF6E6E, 0x69FF94, 0xFFFFA5, 0xD6ACFF, 0xFF92DF, 0xA4FFFF, 0xFFFFFF,
        ]),
        foreground: Rgb::hex(0xF8F8F2),
        background: Rgb::hex(0x282A36),
        cursor: Rgb::hex(0xF8F8F2),
        cursor_text: Rgb::hex(0x282A36),
        selection: Rgb::hex(0xF8F8F2),
        selection_alpha: 0.16,
        bold_is_bright: true,
    },
    ui: None,
};

const ALUCARD: Theme = Theme {
    id: "alucard",
    name: "Alucard",
    dark: false,
    palette: Palette {
        ansi: ansi([
            0xFFFBEB, 0xCB3A2A, 0x14710A, 0x846E15, 0x644AC9, 0xA3144D, 0x036A96, 0x1F1F1F,
            0x6C664B, 0xD74C3D, 0x198D0C, 0x9E841A, 0x7862D0, 0xBF185A, 0x047FB4, 0x2C2B31,
        ]),
        foreground: Rgb::hex(0x1F1F1F),
        background: Rgb::hex(0xFFFBEB),
        cursor: Rgb::hex(0x1F1F1F),
        cursor_text: Rgb::hex(0xFFFBEB),
        selection: Rgb::hex(0x644AC9),
        selection_alpha: 0x60 as f32 / 255.0,
        bold_is_bright: false,
    },
    ui: ui(0xCECCC0, 0xDEDCCF, 0x6C664B),
};

// Gruvbox: the original medium-contrast palette from morhetz/gruvbox
// (`colors/gruvbox.vim`) and the terminal ports in morhetz/gruvbox-contrib
// (termite, st, xfce4-terminal, Xresources). Those ports agree, except that
// the older Xresources and xfce4 files give Light's ANSI 0 the retired light0
// #FDF4C1; this uses the current light0 #FBF1C7, as termite and st do. The
// ports set no selection, so the target is bg3, the Visual background in
// gruvbox.vim; a wash of the foreground lands within 4 RGB units of it.
// `ui` follows gruvbox.vim: bg1 (tab line, sign column) is the mantle and bg2
// (popup menu) the surface. Gray is 4.02:1 on Dark and 3.24:1 on Light, so
// the muted text is the nearest passing tone: fg4 on Dark, fg3 on Light.

const GRUVBOX_DARK: Theme = Theme {
    id: "gruvbox-dark",
    name: "Gruvbox Dark",
    dark: true,
    palette: Palette {
        ansi: ansi([
            0x282828, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0xA89984,
            0x928374, 0xFB4934, 0xB8BB26, 0xFABD2F, 0x83A598, 0xD3869B, 0x8EC07C, 0xEBDBB2,
        ]),
        foreground: Rgb::hex(0xEBDBB2),
        background: Rgb::hex(0x282828),
        cursor: Rgb::hex(0xEBDBB2),
        cursor_text: Rgb::hex(0x282828),
        selection: Rgb::hex(0xEBDBB2),
        selection_alpha: 0.3,
        bold_is_bright: true,
    },
    ui: ui(0x3C3836, 0x504945, 0xA89984),
};

const GRUVBOX_LIGHT: Theme = Theme {
    id: "gruvbox-light",
    name: "Gruvbox Light",
    dark: false,
    palette: Palette {
        ansi: ansi([
            0xFBF1C7, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0x7C6F64,
            0x928374, 0x9D0006, 0x79740E, 0xB57614, 0x076678, 0x8F3F71, 0x427B58, 0x3C3836,
        ]),
        foreground: Rgb::hex(0x3C3836),
        background: Rgb::hex(0xFBF1C7),
        cursor: Rgb::hex(0x3C3836),
        cursor_text: Rgb::hex(0xFBF1C7),
        selection: Rgb::hex(0x3C3836),
        selection_alpha: 0.34,
        bold_is_bright: false,
    },
    ui: ui(0xEBDBB2, 0xD5C4A1, 0x665C54),
};

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x contrast ratio between two opaque colors.
    fn contrast(a: Rgb, b: Rgb) -> f64 {
        fn luminance(color: Rgb) -> f64 {
            let channel = |value: u8| {
                let c = value as f64 / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn luminance_order(theme: &Theme) -> bool {
        let fg = theme.palette.foreground;
        let bg = theme.palette.background;
        let sum = |c: Rgb| c.r as u32 * 2126 + c.g as u32 * 7152 + c.b as u32 * 722;
        sum(bg) > sum(fg)
    }

    #[test]
    fn ids_are_unique_kebab_case() {
        for (i, theme) in THEMES.iter().enumerate() {
            let id = theme.id;
            assert!(!id.is_empty(), "empty id");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{id} is not kebab-case"
            );
            assert!(
                !id.starts_with('-') && !id.ends_with('-') && !id.contains("--"),
                "{id} is not kebab-case"
            );
            assert!(
                THEMES[i + 1..].iter().all(|other| other.id != id),
                "{id} is listed twice"
            );
        }
    }

    #[test]
    fn find_returns_every_theme() {
        for theme in THEMES {
            assert_eq!(find(theme.id), Some(theme));
        }
        assert_eq!(find("no-such-theme"), None);
    }

    #[test]
    fn shika_leads_each_appearance() {
        assert_eq!(themes(false).next().map(|t| t.id), Some(SHIKA_LIGHT));
        assert_eq!(themes(true).next().map(|t| t.id), Some(SHIKA_DARK));
    }

    #[test]
    fn foreground_meets_wcag_aa_on_background() {
        for theme in THEMES {
            let ratio = contrast(theme.palette.foreground, theme.palette.background);
            assert!(ratio >= 4.5, "{}: foreground is {ratio:.2}:1", theme.id);
        }
    }

    #[test]
    fn muted_text_meets_wcag_aa_on_background() {
        for theme in THEMES {
            if let Some(ui) = theme.ui {
                let ratio = contrast(ui.muted, theme.palette.background);
                assert!(ratio >= 4.5, "{}: muted is {ratio:.2}:1", theme.id);
            }
        }
    }

    #[test]
    fn dark_flag_matches_the_palette() {
        for theme in THEMES {
            assert_eq!(
                luminance_order(theme),
                !theme.dark,
                "{}: background and foreground do not match dark = {}",
                theme.id,
                theme.dark
            );
        }
    }

    #[test]
    fn selection_alpha_is_a_wash() {
        for theme in THEMES {
            let alpha = theme.palette.selection_alpha;
            assert!(alpha > 0.0 && alpha < 1.0, "{}: alpha {alpha}", theme.id);
        }
    }
}
