use super::*;
use gpui::{Rgba, WindowAppearance, WindowBackgroundAppearance, rgb};
use shika_core::{Appearance, ThemeMode, ThemeSettings, Translucency};
use shika_terminal::ThemeUi;
use shika_terminal::{Palette, Rgb, Theme, catalog};

#[test]
fn usage_intensities_are_opaque_monotonic_in_every_catalog_theme() {
    for theme in catalog::THEMES {
        for opacity in [100, 60] {
            let appearance = Appearance {
                opacity,
                ..Appearance::default()
            };
            let chrome = chrome_for(&appearance, theme, false);
            let colors = chrome.usage_intensities();
            let toward_ink = |color: Rgba| {
                (color.r - chrome.overlay.r).abs()
                    + (color.g - chrome.overlay.g).abs()
                    + (color.b - chrome.overlay.b).abs()
            };
            for color in colors {
                assert_eq!(color.a, 1.);
            }
            for pair in colors.windows(2) {
                assert!(toward_ink(pair[0]) < toward_ink(pair[1]), "{}", theme.id);
            }
            assert_eq!(colors[0], mix(chrome.overlay, chrome.ink_1, 0.08));
            assert_eq!(colors[4], mix(chrome.overlay, chrome.ink_1, 0.86));
            // The empty square reads on the dialog without an outline.
            assert!(toward_ink(colors[0]) > 0.03, "{}", theme.id);
        }
    }
}

#[test]
fn sidebar_only_keeps_the_terminal_opaque() {
    let appearance = Appearance {
        opacity: 70,
        blur: 20,
        translucency: Translucency::Sidebar,
    };
    assert_eq!(sidebar_alpha(&appearance), 0.7);
    assert_eq!(terminal_alpha_for(&appearance, false), 1.0);
    assert_eq!(
        background_for(&appearance, false, false),
        WindowBackgroundAppearance::Transparent
    );
    // A background image replaces the desktop, so the window stays opaque
    // while the column keeps its alpha over the image.
    assert_eq!(
        background_for(&appearance, false, true),
        WindowBackgroundAppearance::Opaque
    );
    assert!(glass_active(&appearance, false));
}

#[test]
fn terminal_frost_follows_opacity_down_to_the_floor() {
    let mut appearance = Appearance {
        opacity: 10,
        blur: 0,
        translucency: Translucency::SidebarAndTerminal,
    };
    assert_eq!(terminal_alpha_for(&appearance, false), TERMINAL_MIN_ALPHA);
    appearance.opacity = 40;
    assert!((terminal_alpha_for(&appearance, false) - 0.4).abs() < 0.001);
    appearance.opacity = 92;
    assert!((terminal_alpha_for(&appearance, false) - 0.92).abs() < 0.001);
    appearance.opacity = 100;
    assert_eq!(terminal_alpha_for(&appearance, false), 1.0);
    assert_eq!(
        background_for(&appearance, false, false),
        WindowBackgroundAppearance::Opaque
    );
}

#[test]
fn reduce_transparency_paints_solid() {
    let appearance = Appearance {
        opacity: 58,
        blur: 44,
        translucency: Translucency::SidebarAndTerminal,
    };
    assert_eq!(terminal_alpha_for(&appearance, true), 1.0);
    assert_eq!(
        background_for(&appearance, true, false),
        WindowBackgroundAppearance::Opaque
    );
    let chrome = chrome_for(&appearance, shika_theme(false), true);
    assert_eq!(chrome.column.a, 1.0);
    assert_eq!(
        Rgba {
            a: 1.0,
            ..chrome.column
        },
        rgb(0xF1F2EC)
    );
}

#[test]
fn glass_uses_the_frosted_column_and_a_washed_selected_card() {
    let appearance = Appearance {
        opacity: 58,
        blur: 44,
        translucency: Translucency::Sidebar,
    };
    let chrome = chrome_for(&appearance, shika_theme(false), false);
    assert!((chrome.column.a - 0.58).abs() < 0.001);
    assert_eq!(
        Rgba {
            a: 1.0,
            ..chrome.column
        },
        rgb(0xF6F8F2)
    );
    assert!(chrome.card_selected.a > chrome.hover.a && chrome.card_selected.a < 0.25);
    let dark = chrome_for(&appearance, shika_theme(true), false);
    assert_eq!(
        Rgba {
            a: 1.0,
            ..dark.column
        },
        rgb(0x181A17)
    );
    assert!(dark.card_selected.a > dark.hover.a && dark.card_selected.a < 0.25);
}

#[test]
fn popups_stay_solid_in_glass() {
    let appearance = Appearance {
        opacity: 40,
        blur: 30,
        translucency: Translucency::SidebarAndTerminal,
    };
    for theme in every_theme() {
        let chrome = chrome_for(&appearance, theme, false);
        assert!(chrome.column.a < 1.0, "{}", theme.id);
        assert_eq!(chrome.overlay.a, 1.0, "{}", theme.id);
        assert_eq!(chrome.toast_bg.a, 1.0, "{}", theme.id);
    }
}

/// Source-over with straight alpha, returning premultiplied color and alpha.
fn over(top: Rgba, bottom: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let keep = 1.0 - top.a;
    (
        top.r * top.a + bottom.0 * keep,
        top.g * top.a + bottom.1 * keep,
        top.b * top.a + bottom.2 * keep,
        top.a + bottom.3 * keep,
    )
}

#[test]
fn the_active_tab_matches_the_terminal_in_every_mode() {
    for (opacity, translucency) in [
        (100, Translucency::SidebarAndTerminal),
        (40, Translucency::Sidebar),
        (0, Translucency::SidebarAndTerminal),
        (20, Translucency::SidebarAndTerminal),
        (40, Translucency::SidebarAndTerminal),
        (90, Translucency::SidebarAndTerminal),
    ] {
        let appearance = Appearance {
            opacity,
            blur: 30,
            translucency,
        };
        for theme in every_theme() {
            let chrome = chrome_for(&appearance, theme, false);
            let header = over(
                with_alpha(chrome.term_header, chrome.term_header_alpha),
                (0.0, 0.0, 0.0, 0.0),
            );
            let tab = over(chrome.term_tab, header);
            let terminal = over(chrome.term_surface, (0.0, 0.0, 0.0, 0.0));
            for (got, want) in [
                (tab.0, terminal.0),
                (tab.1, terminal.1),
                (tab.2, terminal.2),
                (tab.3, terminal.3),
            ] {
                assert!(
                    (got - want).abs() < 1e-4,
                    "{} {opacity}: {got} {want}",
                    theme.id
                );
            }
        }
    }
}

#[test]
fn the_terminal_shares_the_column_background() {
    for (opacity, translucency) in [
        (100, Translucency::SidebarAndTerminal),
        (60, Translucency::Sidebar),
        (60, Translucency::SidebarAndTerminal),
    ] {
        let appearance = Appearance {
            opacity,
            blur: 30,
            translucency,
        };
        for theme in every_theme() {
            let chrome = chrome_for(&appearance, theme, false);
            let surface = Rgba {
                a: 1.0,
                ..chrome.term_surface
            };
            let column = Rgba {
                a: 1.0,
                ..chrome.column
            };
            assert_eq!(surface, column, "{} {opacity}", theme.id);
        }
    }
}

fn palette(background: u32, foreground: u32, ansi: [u32; 16]) -> Palette {
    Palette {
        ansi: ansi.map(Rgb::hex),
        foreground: Rgb::hex(foreground),
        background: Rgb::hex(background),
        cursor: Rgb::hex(foreground),
        cursor_text: Rgb::hex(background),
        selection: Rgb::hex(foreground),
        selection_alpha: 0.2,
        bold_is_bright: true,
    }
}

/// A Catppuccin Mocha lookalike: dark, with its own UI surfaces.
const MOCHA: Theme = Theme {
    id: "test-mocha",
    name: "Test Mocha",
    dark: true,
    palette: Palette {
        ansi: [
            Rgb::hex(0x45475A),
            Rgb::hex(0xF38BA8),
            Rgb::hex(0xA6E3A1),
            Rgb::hex(0xF9E2AF),
            Rgb::hex(0x89B4FA),
            Rgb::hex(0xF5C2E7),
            Rgb::hex(0x94E2D5),
            Rgb::hex(0xBAC2DE),
            Rgb::hex(0x585B70),
            Rgb::hex(0xF38BA8),
            Rgb::hex(0xA6E3A1),
            Rgb::hex(0xF9E2AF),
            Rgb::hex(0x89B4FA),
            Rgb::hex(0xF5C2E7),
            Rgb::hex(0x94E2D5),
            Rgb::hex(0xA6ADC8),
        ],
        foreground: Rgb::hex(0xCDD6F4),
        background: Rgb::hex(0x1E1E2E),
        cursor: Rgb::hex(0xF5E0DC),
        cursor_text: Rgb::hex(0x1E1E2E),
        selection: Rgb::hex(0x585B70),
        selection_alpha: 0.5,
        bold_is_bright: true,
    },
    ui: Some(ThemeUi {
        mantle: Rgb::hex(0x181825),
        surface: Rgb::hex(0x313244),
        muted: Rgb::hex(0xA6ADC8),
    }),
};

/// Synthetic themes for derivation: Mocha, a Latte lookalike, a light
/// theme without `ui` whose yellow is faint, and a dark theme whose own
/// foreground does not reach 4.5:1.
fn synthetic_themes() -> Vec<Theme> {
    let latte = Theme {
        id: "test-latte",
        name: "Test Latte",
        dark: false,
        palette: palette(
            0xEFF1F5,
            0x4C4F69,
            [
                0x5C5F77, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0xACB0BE,
                0x6C6F85, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0xBCC0CC,
            ],
        ),
        ui: Some(ThemeUi {
            mantle: Rgb::hex(0xE6E9EF),
            surface: Rgb::hex(0xCCD0DA),
            muted: Rgb::hex(0x6C6F85),
        }),
    };
    let paper = Theme {
        id: "test-paper",
        name: "Test Paper",
        dark: false,
        palette: palette(
            0xFDF6E3,
            0x657B83,
            [
                0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5,
                0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
            ],
        ),
        ui: None,
    };
    let murk = Theme {
        id: "test-murk",
        name: "Test Murk",
        dark: true,
        palette: palette(
            0x3A3A3A,
            0x6A6A6A,
            [
                0x2A2A2A, 0x6A4A4A, 0x4A5A4A, 0x5A5A40, 0x4A4A6A, 0x5A4A5A, 0x4A5A5A, 0x5A5A5A,
                0x444444, 0x7A5A5A, 0x5A6A5A, 0x6A6A50, 0x5A5A7A, 0x6A5A6A, 0x5A6A6A, 0x6A6A6A,
            ],
        ),
        ui: None,
    };
    vec![MOCHA, latte, paper, murk]
}

/// The catalog, whatever it holds, plus the synthetic themes.
fn every_theme() -> Vec<&'static Theme> {
    static SYNTHETIC: std::sync::OnceLock<Vec<Theme>> = std::sync::OnceLock::new();
    let synthetic = SYNTHETIC.get_or_init(synthetic_themes);
    catalog::THEMES.iter().chain(synthetic).collect()
}

fn modes() -> Vec<(Appearance, bool)> {
    let mut modes = vec![];
    for (opacity, translucency) in [
        (100, Translucency::Sidebar),
        (60, Translucency::Sidebar),
        (40, Translucency::SidebarAndTerminal),
    ] {
        for reduce in [false, true] {
            let appearance = Appearance {
                opacity,
                blur: 30,
                translucency,
            };
            modes.push((appearance, reduce));
        }
    }
    modes
}

#[test]
fn shika_chrome_keeps_its_hand_tuned_values() {
    let solid = Appearance::default();
    let glass = Appearance {
        opacity: 60,
        blur: 30,
        translucency: Translucency::SidebarAndTerminal,
    };
    let light = chrome_for(&solid, shika_theme(false), false);
    assert_eq!(light.column, rgb(0xF1F2EC));
    assert_eq!(light.ink_1, rgb(0x262824));
    assert_eq!(light.ink_3, rgb(0x6C7166));
    assert_eq!(light.ready.text, rgb(0x21763C));
    assert_eq!(light.term_header, rgb(0xE7E9E1));
    assert_eq!(light.toast_bg, rgb(0x252823));
    let dark = chrome_for(&solid, shika_theme(true), false);
    assert_eq!(dark.column, rgb(0x1A1C19));
    assert_eq!(dark.ink_1, rgb(0xE8EBE3));
    assert_eq!(dark.working.text, rgb(0x8CC4F4));
    assert_eq!(dark.overlay, rgb(0x242722));
    assert_eq!(dark.term_header, rgb(0x151714));
    assert_eq!(dark.hairline, rgb(0x2A2D27));
    let glass_dark = chrome_for(&glass, shika_theme(true), false);
    assert_eq!(glass_dark.column, tint(0x181A17, 0.6));
    assert_eq!(glass_dark.ink_3, rgb(0xA6AB9E));
    assert_eq!(glass_dark.term_header, rgb(0x181A17));
    assert!((glass_dark.term_header_alpha - 0.5).abs() < 1e-6);
    let glass_light = chrome_for(&glass, shika_theme(false), false);
    assert_eq!(glass_light.hairline, tint(0x000000, 0.10));
    assert_eq!(
        terminal_palette(&glass, shika_theme(false), false).background,
        Rgb::hex(0xF6F8F2)
    );
}

fn hex(color: Rgba) -> u32 {
    let byte = |c: f32| (c * 255.0).round() as u32;
    byte(color.r) << 16 | byte(color.g) << 8 | byte(color.b)
}

#[test]
fn diff_colors_keep_the_design_values() {
    let solid = Appearance::default();
    let glass = Appearance {
        opacity: 60,
        blur: 30,
        translucency: Translucency::Sidebar,
    };
    let cases = [
        (solid, false, [0xE3E8DE, 0x3A7439, 0xECE4DE, 0xAE4539]),
        (glass, false, [0xE7EEE3, 0x3B763A, 0xF1EAE3, 0xB2463A]),
        (solid, true, [0x252D24, 0x8CC98A, 0x2E2521, 0xE0786B]),
        (glass, true, [0x242C23, 0x8CC98A, 0x2C231F, 0xE0786B]),
    ];
    for (appearance, dark, expected) in cases {
        let chrome = chrome_for(&appearance, shika_theme(dark), false);
        let got = [
            hex(chrome.diff_added.tint),
            hex(chrome.diff_added.text),
            hex(chrome.diff_removed.tint),
            hex(chrome.diff_removed.text),
        ];
        assert_eq!(
            got.map(|c| format!("{c:06X}")),
            expected.map(|c| format!("{c:06X}")),
            "dark {dark} opacity {}",
            appearance.opacity
        );
    }
}

#[test]
fn diff_colors_are_opaque_and_readable_in_every_theme() {
    for theme in every_theme() {
        for (appearance, reduce) in modes() {
            let chrome = chrome_for(&appearance, theme, reduce);
            for colors in [chrome.diff_added, chrome.diff_removed] {
                assert_eq!(colors.tint.a, 1.0, "{}", theme.id);
                let ratio = contrast(colors.text, colors.tint);
                assert!(ratio >= TEXT_CONTRAST, "{}: {ratio}", theme.id);
            }
            let header = chrome.diff_file_header();
            assert_eq!(header.a, 1.0, "{}", theme.id);
            for ink in [
                chrome.term_dim,
                chrome.term_white,
                chrome.diff_added.text,
                chrome.diff_removed.text,
            ] {
                assert!(
                    contrast(chrome.diff_file_header_text(ink), header) >= TEXT_CONTRAST,
                    "{}",
                    theme.id
                );
            }
            assert_eq!(
                chrome.term_text,
                rgba_of(theme.palette.foreground),
                "{}",
                theme.id
            );
        }
    }
}

#[test]
fn diff_counts_are_readable_on_the_column() {
    // A ready card colors its `+N` and `−N` with the Changes panel's
    // diff text, so that text must also read on the column.
    for theme in every_theme() {
        for (appearance, reduce) in modes() {
            let chrome = chrome_for(&appearance, theme, reduce);
            let column = with_alpha(chrome.column, 1.0);
            for (name, color) in [
                ("added", chrome.diff_added.text),
                ("removed", chrome.diff_removed.text),
            ] {
                let ratio = contrast(color, column);
                assert!(ratio >= TEXT_CONTRAST, "{} {name}: {ratio}", theme.id);
            }
        }
    }
}

#[test]
fn derived_chrome_is_readable_on_the_column() {
    for theme in every_theme().into_iter().filter(|theme| !is_shika(theme)) {
        for (appearance, reduce) in modes() {
            let chrome = chrome_for(&appearance, theme, reduce);
            let column = with_alpha(chrome.column, 1.0);
            assert_eq!(column, rgba_of(theme.palette.background), "{}", theme.id);
            assert_eq!(
                chrome.ink_1,
                rgba_of(theme.palette.foreground),
                "{}",
                theme.id
            );
            let readable = [
                ("ink_3", chrome.ink_3),
                ("ready", chrome.ready.text),
                ("asking", chrome.asking.text),
                ("working", chrome.working.text),
                ("waiting", chrome.waiting.text),
                ("failed", chrome.failed.text),
                ("term_dim", chrome.term_dim),
            ];
            for (name, color) in readable {
                let ratio = contrast(color, column);
                assert!(ratio >= TEXT_CONTRAST, "{} {name}: {ratio}", theme.id);
            }
            assert_eq!(chrome.overlay.a, 1.0);
            assert_eq!(chrome.primary_bg, chrome.ink_1);
            assert_eq!(chrome.primary_fg, column);
        }
    }
}

#[test]
fn derived_chrome_uses_the_theme_surfaces_and_hues() {
    let chrome = chrome_for(&Appearance::default(), &MOCHA, false);
    let ui = MOCHA.ui.unwrap();
    assert_eq!(chrome.overlay, rgba_of(ui.surface));
    assert_eq!(chrome.sunken, rgba_of(ui.mantle));
    assert_eq!(chrome.term_header, rgba_of(ui.mantle));
    // Mocha's own muted text and hues already meet 4.5:1, so they pass
    // through unchanged.
    assert_eq!(chrome.ink_3, rgba_of(ui.muted));
    assert_eq!(chrome.ready.text, rgba_of(MOCHA.palette.ansi[2]));
    assert_eq!(chrome.working.dot, rgba_of(MOCHA.palette.ansi[4]));
    // Hierarchy: each ink step is further from the foreground.
    let fg = chrome.ink_1;
    let steps = [chrome.ink_2, chrome.ink_3, chrome.ink_4, chrome.ink_5];
    for pair in steps.windows(2) {
        assert!(contrast(pair[0], fg) < contrast(pair[1], fg));
    }
    // In frost the header keeps the terminal's color.
    let frost = Appearance {
        opacity: 50,
        blur: 30,
        translucency: Translucency::SidebarAndTerminal,
    };
    let chrome = chrome_for(&frost, &MOCHA, false);
    assert_eq!(chrome.term_header, rgba_of(MOCHA.palette.background));
    assert_eq!(
        terminal_palette(&frost, &MOCHA, false).background,
        MOCHA.palette.background
    );
}

#[test]
fn legible_reaches_the_ratio_even_past_the_foreground() {
    assert!((contrast(rgb(0x000000), rgb(0xFFFFFF)) - 21.0).abs() < 0.01);
    let background = rgb(0x777777);
    let color = legible(rgb(0x808080), background, rgb(0x8A8A8A), TEXT_CONTRAST);
    assert!(contrast(color, background) >= TEXT_CONTRAST);
    // Already readable stays as it is.
    let ink = rgb(0x111111);
    assert_eq!(legible(ink, rgb(0xFFFFFF), ink, TEXT_CONTRAST), ink);
}

#[test]
fn unknown_and_wrong_side_ids_paint_the_shika_theme() {
    assert_eq!(shika_core::DEFAULT_LIGHT_THEME, catalog::SHIKA_LIGHT);
    assert_eq!(shika_core::DEFAULT_DARK_THEME, catalog::SHIKA_DARK);
    let mut choice = ThemeSettings::default();
    assert_eq!(resolve_theme(&choice, false).id, catalog::SHIKA_LIGHT);
    assert_eq!(resolve_theme(&choice, true).id, catalog::SHIKA_DARK);
    choice.light = catalog::SHIKA_DARK.into();
    choice.dark = "no-such-theme".into();
    assert_eq!(resolve_theme(&choice, false).id, catalog::SHIKA_LIGHT);
    assert_eq!(resolve_theme(&choice, true).id, catalog::SHIKA_DARK);
    for theme in catalog::THEMES {
        let choice = ThemeSettings {
            light: theme.id.into(),
            dark: theme.id.into(),
            ..ThemeSettings::default()
        };
        assert_eq!(resolve_theme(&choice, theme.dark).id, theme.id);
        assert!(is_shika(resolve_theme(&choice, !theme.dark)));
    }
}

#[test]
fn stepping_a_theme_wraps_within_its_side() {
    for dark in [false, true] {
        let side: Vec<&Theme> = catalog::themes(dark).collect();
        let first = side[0];
        let last = side[side.len() - 1];
        assert_eq!(step_theme(last, 1).id, first.id);
        assert_eq!(step_theme(first, -1).id, last.id);
        let mut at = first;
        for _ in 0..side.len() {
            at = step_theme(at, 1);
            assert_eq!(at.dark, dark);
        }
        assert_eq!(at.id, first.id);
    }
}

#[test]
fn a_forced_mode_wins_over_macos() {
    assert!(is_dark_for(ThemeMode::Dark, WindowAppearance::Light));
    assert!(!is_dark_for(
        ThemeMode::Light,
        WindowAppearance::VibrantDark
    ));
    assert!(is_dark_for(ThemeMode::System, WindowAppearance::Dark));
    assert!(!is_dark_for(ThemeMode::System, WindowAppearance::Light));
}

#[test]
fn tint_keeps_the_color_and_sets_alpha() {
    let color = tint(0xF1F2EC, 0.5);
    assert_eq!(Rgba { a: 1.0, ..color }, rgb(0xF1F2EC));
    assert_eq!(color.a, 0.5);
}
