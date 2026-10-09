//! Window translucency. GPUI makes the window transparent. The blur comes
//! from the macOS window server, because GPUI's own blurred background has
//! one fixed strength. Ghostty, WezTerm, and winit set the radius the same
//! way.

use crate::model::Status;
use gpui::{Rgba, Window, WindowAppearance, WindowBackgroundAppearance, rgb};
use shika_core::{Appearance, ThemeMode, ThemeSettings, Translucency};
use shika_terminal::{Palette, Rgb, Theme, catalog};

/// Make the window transparent, or opaque again, and set the blur behind it.
/// macOS Reduce transparency forces a solid window even when opacity is lower.
pub fn apply(appearance: &Appearance, window: &mut Window) {
    apply_for(appearance, reduce_transparency(), window);
}

pub fn apply_for(appearance: &Appearance, reduce_transparency: bool, window: &mut Window) {
    window.set_background_appearance(background_for(appearance, reduce_transparency));
    let radius = if appearance.is_opaque() || reduce_transparency {
        0
    } else {
        appearance.blur
    };
    set_blur_radius(window, radius);
}

pub fn background(appearance: &Appearance) -> WindowBackgroundAppearance {
    background_for(appearance, reduce_transparency())
}

pub fn background_for(
    appearance: &Appearance,
    reduce_transparency: bool,
) -> WindowBackgroundAppearance {
    if appearance.is_opaque() || reduce_transparency {
        WindowBackgroundAppearance::Opaque
    } else {
        WindowBackgroundAppearance::Transparent
    }
}

pub fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// True when Settings asked for frost and macOS is not reducing transparency.
pub fn glass_active(appearance: &Appearance, reduce_transparency: bool) -> bool {
    !appearance.is_opaque() && !reduce_transparency
}

/// Alpha for the sidebar background.
pub fn sidebar_alpha(appearance: &Appearance) -> f32 {
    f32::from(appearance.opacity.min(100)) / 100.0
}

/// The lowest alpha the terminal side takes when frost covers it. It only
/// keeps the terminal from vanishing; readability is the user's choice.
pub const TERMINAL_MIN_ALPHA: f32 = 0.2;

/// Alpha for the terminal side: its header, its empty state, and the
/// terminal's default background. Sidebar-only frost keeps the terminal
/// solid. When frost covers the terminal, it follows the Settings opacity
/// down to `TERMINAL_MIN_ALPHA`.
pub fn terminal_alpha_for(appearance: &Appearance, reduce_transparency: bool) -> f32 {
    if !glass_active(appearance, reduce_transparency) {
        return 1.0;
    }
    match appearance.translucency {
        Translucency::Sidebar => 1.0,
        Translucency::SidebarAndTerminal => sidebar_alpha(appearance).max(TERMINAL_MIN_ALPHA),
    }
}

/// macOS "Reduce transparency". False off macOS, or if AppKit is unavailable.
pub fn reduce_transparency() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_reduce_transparency()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn macos_reduce_transparency() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    let Some(cls) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: NSWorkspace is AppKit's shared workspace, already loaded by GPUI.
    unsafe {
        let workspace: *mut AnyObject = msg_send![cls, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: Bool = msg_send![workspace, accessibilityDisplayShouldReduceTransparency];
        reduce.as_bool()
    }
}

/// One status hue: the dot, and text that meets contrast on the column.
#[derive(Clone, Copy, Debug)]
pub struct StatusColors {
    pub dot: Rgba,
    pub text: Rgba,
}

/// Colors for one paint of the window. Solid at 100% opacity, and whenever
/// macOS Reduce transparency is on. Glass uses the frosted tints. The names
/// follow `design/DESIGN.md`: `ink_1` is `--ink-1`, `line_2` is `--line-2`.
#[derive(Clone, Copy, Debug)]
pub struct Chrome {
    pub glass: bool,
    pub column: Rgba,
    /// `--line-1`: the column edge.
    pub hairline: Rgba,
    /// `--line-2`: the footer and picker footer rules.
    pub line_2: Rgba,
    /// `--shadow-card`: the 1px ring on a resting card.
    pub line_subtle: Rgba,
    /// The ring on the selected card while the terminal has focus.
    pub line_selected_dim: Rgba,
    /// Borders of buttons and number fields.
    pub line_control: Rgba,
    /// The empty project box.
    pub dashed: Rgba,
    pub ink_1: Rgba,
    pub ink_2: Rgba,
    pub ink_3: Rgba,
    pub ink_4: Rgba,
    /// Meta separators (`·`).
    pub ink_5: Rgba,
    pub focus: Rgba,
    pub card_rest: Rgba,
    pub card_selected: Rgba,
    pub card_ready: Rgba,
    pub card_asking: Rgba,
    pub card_highlight: Rgba,
    /// The soft drop under the selected card, below its focus ring.
    pub card_shadow: Rgba,
    pub card_shadow_blur: f32,
    pub row_selected: Rgba,
    pub overlay: Rgba,
    pub toast_bg: Rgba,
    pub toast_fg: Rgba,
    pub toast_shadow: Rgba,
    /// Key caps and segmented tracks.
    pub sunken: Rgba,
    pub raised: Rgba,
    pub raised_hover: Rgba,
    /// `--surface-hover`, a translucent ink wash.
    pub hover: Rgba,
    /// `--shadow-control`, light mode only. Transparent in dark.
    pub control_shadow: Rgba,
    pub dialog_ring: Rgba,
    pub dialog_shadow: Rgba,
    pub primary_bg: Rgba,
    pub primary_hover: Rgba,
    pub primary_fg: Rgba,
    /// Key cap text on a primary button.
    pub kbd_inverse: Rgba,
    pub ready: StatusColors,
    pub asking: StatusColors,
    pub working: StatusColors,
    pub waiting: StatusColors,
    /// A PR's checks failed. Not an agent status: it colors only the PR
    /// mark on the card's second line.
    pub failed: StatusColors,
    pub term_header: Rgba,
    pub term_header_alpha: f32,
    /// The terminal's default background with its alpha: the metadata row
    /// under the header shares it.
    pub term_surface: Rgba,
    /// The active tab's fill. Painted over the header, it looks exactly like
    /// `term_surface`, so the tab opens into the terminal below it.
    pub term_tab: Rgba,
    pub term_line: Rgba,
    pub term_seg: Rgba,
    pub term_seg_active: Rgba,
    pub term_hover: Rgba,
    /// `--term-fg-ui`: chrome text on the terminal side.
    pub term_fg: Rgba,
    /// The active segment's text.
    pub term_white: Rgba,
    pub term_dim: Rgba,
    /// `--term-faint-ui`: hints and the empty state line.
    pub term_faint: Rgba,
    /// `--term-faint`: the empty state's key row.
    pub term_fainter: Rgba,
    pub term_empty: Rgba,
    pub term_empty_alpha: f32,
}

impl Chrome {
    pub fn status(&self, status: Status) -> StatusColors {
        match status {
            Status::Ready => self.ready,
            Status::Asking => self.asking,
            Status::Working => self.working,
            Status::Waiting => self.waiting,
        }
    }
}

/// The chrome for one paint of the window in `theme`. Shika Light and
/// Shika Dark use their hand-tuned tables; every other theme derives its
/// chrome from its palette and `ui`, keeping Shika's layout and hierarchy.
pub fn chrome_for(appearance: &Appearance, theme: &Theme, reduce_transparency: bool) -> Chrome {
    let frame = Frame::new(appearance, theme, reduce_transparency);
    if is_shika(theme) {
        shika_chrome(&frame, theme.dark)
    } else {
        derived_chrome(&frame, theme)
    }
}

/// True for the two themes whose chrome is hand-tuned.
pub fn is_shika(theme: &Theme) -> bool {
    theme.id == catalog::SHIKA_LIGHT || theme.id == catalog::SHIKA_DARK
}

/// What one paint knows about the window, shared by both kinds of chrome.
struct Frame {
    glass: bool,
    /// The column's alpha: the Settings opacity in glass, otherwise 1.
    alpha: f32,
    term_alpha: f32,
    /// Frost covers the terminal side too.
    frost_term: bool,
    column_base: Rgb,
    palette: Palette,
}

impl Frame {
    fn new(appearance: &Appearance, theme: &Theme, reduce_transparency: bool) -> Self {
        let glass = glass_active(appearance, reduce_transparency);
        Self {
            glass,
            alpha: if glass {
                sidebar_alpha(appearance)
            } else {
                1.0
            },
            term_alpha: terminal_alpha_for(appearance, reduce_transparency),
            frost_term: glass
                && matches!(appearance.translucency, Translucency::SidebarAndTerminal),
            column_base: column_base(theme, glass),
            palette: terminal_palette(appearance, theme, reduce_transparency),
        }
    }

    /// The terminal side shares the column's background. Solid, the header
    /// is `solid_header`, a step away so the active tab opens into the
    /// terminal below it. In frost the header keeps the terminal's color and
    /// the step is its alpha, so the active tab can always match the
    /// terminal.
    fn term_surfaces(&self, solid_header: Rgba) -> TermSurfaces {
        let background = rgba_of(self.palette.background);
        let header = if self.frost_term {
            background
        } else {
            solid_header
        };
        let header_alpha = if self.frost_term {
            (self.term_alpha - 0.1).max(self.term_alpha * 0.5)
        } else {
            1.0
        };
        let surface = with_alpha(background, self.term_alpha);
        TermSurfaces {
            header,
            header_alpha,
            surface,
            tab: over_to_match(surface, with_alpha(header, header_alpha)),
            empty: background,
        }
    }
}

struct TermSurfaces {
    header: Rgba,
    header_alpha: f32,
    surface: Rgba,
    tab: Rgba,
    /// The empty state's color before its alpha.
    empty: Rgba,
}

/// Shika Light and Shika Dark, hand-tuned for solid and glass.
fn shika_chrome(frame: &Frame, dark: bool) -> Chrome {
    let glass = frame.glass;
    // Pick by mode: solid light, solid dark, glass light, glass dark.
    let pick =
        |light: Rgba, dark_solid: Rgba, glass_light: Rgba, glass_dark: Rgba| match (glass, dark) {
            (false, false) => light,
            (false, true) => dark_solid,
            (true, false) => glass_light,
            (true, true) => glass_dark,
        };
    let column = with_alpha(rgba_of(frame.column_base), frame.alpha);
    let hover = pick(
        tint(0x141E0A, 0.045),
        tint(0xFFFFFF, 0.05),
        tint(0x28341E, 0.06),
        tint(0xFFFFFF, 0.06),
    );
    let ink_3 = pick(rgb(0x6C7166), rgb(0x8D9286), rgb(0x5F6459), rgb(0xA6AB9E));
    let term = frame.term_surfaces(rgb(if dark { 0x151714 } else { 0xE7E9E1 }));
    // Terminal-side lines and washes are translucent so they read the same
    // over solid and glass.
    let (term_line, term_seg, term_seg_active, term_hover) = if dark {
        (
            tint(0xFFFFFF, 0.07),
            tint(0xFFFFFF, 0.06),
            tint(0xFFFFFF, 0.14),
            tint(0xFFFFFF, 0.06),
        )
    } else {
        (
            tint(0x000000, 0.10),
            tint(0x28341E, 0.07),
            tint(0x000000, 0.18),
            tint(0x141E0A, 0.06),
        )
    };
    Chrome {
        glass,
        column,
        hairline: pick(
            rgb(0xDADDD3),
            rgb(0x2A2D27),
            tint(0x000000, 0.10),
            tint(0xFFFFFF, 0.08),
        ),
        line_2: pick(
            rgb(0xE0E3D9),
            rgb(0x262923),
            tint(0x000000, 0.08),
            tint(0xFFFFFF, 0.07),
        ),
        line_subtle: if dark {
            tint(0xFFFFFF, 0.055)
        } else {
            tint(0x141E0A, 0.05)
        },
        line_selected_dim: if dark { rgb(0x3A3E36) } else { rgb(0xD6DACE) },
        line_control: pick(
            rgb(0xD3D7CC),
            rgb(0x383C35),
            tint(0x000000, 0.10),
            tint(0xFFFFFF, 0.12),
        ),
        dashed: pick(
            rgb(0xCFD3C7),
            rgb(0x383C35),
            tint(0x28341E, 0.22),
            tint(0xFFFFFF, 0.16),
        ),
        ink_1: if dark { rgb(0xE8EBE3) } else { rgb(0x262824) },
        ink_2: if dark { rgb(0xB6BBAE) } else { rgb(0x4E524A) },
        ink_3,
        ink_4: pick(rgb(0x9EA296), rgb(0x6B7065), rgb(0x868B7E), rgb(0x9A9F92)),
        ink_5: pick(rgb(0xC3C6BC), rgb(0x464A42), rgb(0xADB1A5), rgb(0x5E6359)),
        focus: if dark { rgb(0xE8EBE3) } else { rgb(0x2F332C) },
        card_rest: pick(
            rgb(0xF8F9F5),
            tint(0xFFFFFF, 0.03),
            tint(0xFFFFFF, 0.48),
            tint(0xFFFFFF, 0.045),
        ),
        card_selected: if dark { rgb(0x272A25) } else { rgb(0xFFFFFF) },
        card_ready: pick(
            rgb(0xEEFBF0),
            rgb(0x18241A),
            tint(0xE7FEEB, 0.78),
            tint(0x1C3422, 0.62),
        ),
        // The existing asking tokens from the archived design/demo, converted
        // from oklch to sRGB, like the ready and working tokens below.
        card_asking: pick(
            rgb(0xFFF5E7),
            rgb(0x2B1F11),
            tint(0xFFF4DA, 0.78),
            tint(0x3E290F, 0.62),
        ),
        card_highlight: if glass {
            tint(0xFFFFFF, if dark { 0.05 } else { 0.75 })
        } else {
            tint(0xFFFFFF, 0.0)
        },
        card_shadow: if dark {
            tint(0x000000, 0.30)
        } else {
            tint(0x141E0A, 0.07)
        },
        card_shadow_blur: if dark { 8. } else { 6. },
        row_selected: if dark { rgb(0x363A33) } else { rgb(0xE8EBE2) },
        // Solid in glass too. The window-server blur only reaches the desktop
        // and GPUI has no backdrop blur, so a translucent popup would show the
        // cards and terminal text under it, unblurred.
        overlay: if dark { rgb(0x242722) } else { rgb(0xFAFAF7) },
        toast_bg: if dark { rgb(0x363A33) } else { rgb(0x252823) },
        toast_fg: if dark { rgb(0xF1F4EC) } else { rgb(0xE9ECE3) },
        toast_shadow: tint(0x000000, 0.35),
        sunken: pick(
            rgb(0xE3E6DD),
            rgb(0x30342E),
            tint(0x28341E, 0.08),
            tint(0xFFFFFF, 0.09),
        ),
        raised: if dark { rgb(0x272A25) } else { rgb(0xFFFFFF) },
        raised_hover: if dark { rgb(0x2E322C) } else { rgb(0xFAFBF8) },
        hover,
        control_shadow: tint(0x141E0A, if dark { 0.0 } else { 0.04 }),
        dialog_ring: if dark {
            tint(0xFFFFFF, 0.10)
        } else {
            tint(0x000000, 0.20)
        },
        dialog_shadow: if dark {
            tint(0x000000, 0.55)
        } else {
            tint(0x0A0E08, 0.30)
        },
        primary_bg: if dark { rgb(0xE8EBE3) } else { rgb(0x262824) },
        primary_hover: if dark { rgb(0xFFFFFF) } else { rgb(0x353A31) },
        primary_fg: if dark { rgb(0x1A1C19) } else { rgb(0xF3F5EE) },
        kbd_inverse: if dark {
            tint(0x1A1C19, 0.70)
        } else {
            tint(0xF3F5EE, 0.72)
        },
        ready: if dark {
            StatusColors {
                dot: rgb(0x68CA80),
                text: rgb(0x89DA9B),
            }
        } else {
            StatusColors {
                dot: rgb(0x45B164),
                text: rgb(0x21763C),
            }
        },
        asking: if dark {
            StatusColors {
                dot: rgb(0xF8A13F),
                text: rgb(0xF6B669),
            }
        } else {
            StatusColors {
                dot: rgb(0xED8725),
                text: rgb(0xAB5200),
            }
        },
        working: if dark {
            StatusColors {
                dot: rgb(0x66ABE5),
                text: rgb(0x8CC4F4),
            }
        } else {
            StatusColors {
                dot: rgb(0x4493D0),
                text: rgb(0x266EA4),
            }
        },
        waiting: StatusColors {
            dot: if dark { rgb(0x6B7065) } else { rgb(0xA3A79B) },
            text: ink_3,
        },
        failed: if dark {
            StatusColors {
                dot: rgb(0xF07A6B),
                text: rgb(0xF59B8F),
            }
        } else {
            StatusColors {
                dot: rgb(0xDD4B3E),
                text: rgb(0xB42318),
            }
        },
        term_header: term.header,
        term_header_alpha: term.header_alpha,
        term_surface: term.surface,
        term_tab: term.tab,
        term_line,
        term_seg,
        term_seg_active,
        term_hover,
        term_fg: if dark { rgb(0xC5CABE) } else { rgb(0x4E524A) },
        term_white: if dark { rgb(0xF2F5EC) } else { rgb(0x262824) },
        term_dim: if dark { rgb(0x878C80) } else { rgb(0x6C7166) },
        term_faint: if dark { rgb(0x757A6E) } else { rgb(0x868B7E) },
        term_fainter: if dark { rgb(0x5A5F55) } else { rgb(0xA3A79B) },
        term_empty: term.empty,
        term_empty_alpha: frame.term_alpha,
    }
}

/// Text on the column meets this contrast ratio (WCAG AA for body text).
pub const TEXT_CONTRAST: f32 = 4.5;

/// Chrome for any theme but Shika's, from its palette and `ui`:
///
/// - The theme's background is the column and the terminal, solid and in
///   glass. `ui.surface` is the selected card, raised controls, and popups;
///   `ui.mantle` is the solid terminal header, key caps, and sunken tracks.
///   Without `ui`, the surface is a step toward white (light) or the
///   foreground (dark), and the mantle a step toward the frame.
/// - `ink_1` is the foreground. `ink_3` is `ui.muted`, or the foreground
///   42% of the way to the background, pushed until it meets 4.5:1 on the
///   column. `ink_2` sits halfway between them; `ink_4` and `ink_5` fade
///   `ink_3` toward the background.
/// - Lines, washes, and rings are the foreground at a low alpha, so they
///   read the same over solid and glass.
/// - Status hues are ANSI green (ready), yellow (asking), and blue
///   (working); failed PR checks are ANSI red. Text darkens the hue toward black on a light theme, or
///   lightens it toward white on a dark one, until it meets 4.5:1; that
///   keeps the hue where mixing toward a tinted foreground would grey it.
///   Waiting is the ink scale's grey. Card tints mix the hue lightly into
///   the background.
/// - The primary button is the foreground with background text; the toast
///   is inverted the same way. Popups stay solid.
fn derived_chrome(frame: &Frame, theme: &Theme) -> Chrome {
    let glass = frame.glass;
    let dark = theme.dark;
    let palette = &theme.palette;
    let side = |light: f32, dark_value: f32| if dark { dark_value } else { light };
    let bg = rgba_of(palette.background);
    let fg = rgba_of(palette.foreground);
    let black = rgb(0x000000);
    let white = rgb(0xFFFFFF);
    let ink = |alpha: f32| with_alpha(fg, alpha);
    let ui = theme.ui;
    let mantle = match ui {
        Some(ui) => rgba_of(ui.mantle),
        None if dark => mix(bg, black, 0.2),
        None => mix(bg, fg, 0.06),
    };
    let surface = match ui {
        Some(ui) => rgba_of(ui.surface),
        None if dark => mix(bg, fg, 0.07),
        None => mix(bg, white, 0.7),
    };
    let muted = ui.map_or_else(|| mix(fg, bg, 0.42), |ui| rgba_of(ui.muted));
    let ink_3 = legible(muted, bg, fg, TEXT_CONTRAST);
    let ink_2 = mix(fg, ink_3, 0.5);
    let ink_4 = mix(ink_3, bg, 0.33);
    let ink_5 = mix(ink_3, bg, 0.65);
    let status = |index: usize| {
        let hue = rgba_of(palette.ansi[index]);
        StatusColors {
            dot: hue,
            text: legible(hue, bg, extreme(bg), TEXT_CONTRAST),
        }
    };
    let card_tint = |index: usize| {
        let tinted = mix(bg, rgba_of(palette.ansi[index]), side(0.12, 0.10));
        if glass {
            with_alpha(tinted, side(0.78, 0.62))
        } else {
            tinted
        }
    };
    let term = frame.term_surfaces(mantle);
    Chrome {
        glass,
        column: with_alpha(rgba_of(frame.column_base), frame.alpha),
        hairline: ink(side(0.12, 0.08)),
        line_2: ink(side(0.10, 0.07)),
        line_subtle: ink(side(0.06, 0.055)),
        line_selected_dim: mix(bg, fg, side(0.13, 0.16)),
        line_control: ink(side(0.14, 0.12)),
        dashed: ink(side(0.24, 0.16)),
        ink_1: fg,
        ink_2,
        ink_3,
        ink_4,
        ink_5,
        focus: fg,
        card_rest: with_alpha(surface, 0.4),
        card_selected: surface,
        card_ready: card_tint(2),
        card_asking: card_tint(3),
        card_highlight: if glass {
            with_alpha(white, side(0.75, 0.05))
        } else {
            with_alpha(white, 0.0)
        },
        card_shadow: with_alpha(black, side(0.07, 0.30)),
        card_shadow_blur: side(6., 8.),
        row_selected: mix(surface, fg, 0.09),
        // Solid in glass too, like Shika's: GPUI has no backdrop blur.
        overlay: surface,
        toast_bg: fg,
        toast_fg: bg,
        toast_shadow: with_alpha(black, 0.35),
        sunken: mantle,
        raised: surface,
        raised_hover: mix(surface, fg, 0.05),
        hover: ink(side(0.05, 0.06)),
        control_shadow: with_alpha(black, side(0.04, 0.0)),
        dialog_ring: ink(side(0.20, 0.10)),
        dialog_shadow: with_alpha(black, side(0.30, 0.55)),
        primary_bg: fg,
        primary_hover: mix(fg, bg, 0.12),
        primary_fg: bg,
        kbd_inverse: with_alpha(bg, 0.70),
        ready: status(2),
        asking: status(3),
        working: status(4),
        waiting: StatusColors {
            dot: ink_4,
            text: ink_3,
        },
        failed: status(1),
        term_header: term.header,
        term_header_alpha: term.header_alpha,
        term_surface: term.surface,
        term_tab: term.tab,
        term_line: ink(side(0.10, 0.07)),
        term_seg: ink(side(0.07, 0.06)),
        term_seg_active: ink(side(0.18, 0.14)),
        term_hover: ink(0.06),
        term_fg: mix(fg, ink_3, 0.3),
        term_white: fg,
        term_dim: ink_3,
        term_faint: mix(ink_3, bg, 0.2),
        term_fainter: mix(ink_3, bg, 0.45),
        term_empty: term.empty,
        term_empty_alpha: frame.term_alpha,
    }
}

/// The column's base color before its alpha. Shika Dark's charcoal and
/// Shika Light's paper are a touch lighter in glass; any other theme keeps
/// its own background in both.
fn column_base(theme: &Theme, glass: bool) -> Rgb {
    if !is_shika(theme) {
        return theme.palette.background;
    }
    Rgb::hex(match (glass, theme.dark) {
        (false, false) => 0xF1F2EC,
        (false, true) => 0x1A1C19,
        (true, false) => 0xF6F8F2,
        (true, true) => 0x181A17,
    })
}

/// The terminal palette for this paint. Its background is the column's, so
/// the agent column and the terminal are one surface split by a hairline.
pub fn terminal_palette(
    appearance: &Appearance,
    theme: &Theme,
    reduce_transparency: bool,
) -> Palette {
    let glass = glass_active(appearance, reduce_transparency);
    theme.palette.with_background(column_base(theme, glass))
}

/// The light or dark pick in `choice`. An id the catalog does not know, or
/// one from the other side, paints that side's Shika theme.
pub fn resolve_theme(choice: &ThemeSettings, dark: bool) -> &'static Theme {
    let id = if dark { &choice.dark } else { &choice.light };
    catalog::find(id)
        .filter(|theme| theme.dark == dark)
        .unwrap_or_else(|| shika_theme(dark))
}

/// Shika Light or Shika Dark from the catalog.
pub fn shika_theme(dark: bool) -> &'static Theme {
    let id = if dark {
        catalog::SHIKA_DARK
    } else {
        catalog::SHIKA_LIGHT
    };
    catalog::find(id).expect("the catalog has the Shika themes")
}

/// The theme `delta` steps from `from` among that side's themes, in catalog
/// order, wrapping at the ends.
pub fn step_theme(from: &Theme, delta: i64) -> &'static Theme {
    let side: Vec<&'static Theme> = catalog::themes(from.dark).collect();
    let at = side
        .iter()
        .position(|theme| theme.id == from.id)
        .unwrap_or(0) as i64;
    let len = side.len() as i64;
    side[(at + delta).rem_euclid(len) as usize]
}

/// Whether this paint is dark: forced by the theme mode, or macOS's
/// appearance in System mode.
pub fn is_dark_for(mode: ThemeMode, appearance: WindowAppearance) -> bool {
    match mode {
        ThemeMode::System => is_dark(appearance),
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
    }
}

/// Force the app's appearance to match the theme mode, so the traffic
/// lights, menus, and `window.appearance()` agree with the paint. System
/// clears it, so the app follows macOS again.
pub fn apply_mode(mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    macos_set_app_appearance(mode);
    #[cfg(not(target_os = "macos"))]
    let _ = mode;
}

#[cfg(target_os = "macos")]
fn macos_set_app_appearance(mode: ThemeMode) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSAppearanceNameAqua: *const AnyObject;
        static NSAppearanceNameDarkAqua: *const AnyObject;
    }

    let (Some(app_class), Some(appearance_class)) = (
        AnyClass::get(c"NSApplication"),
        AnyClass::get(c"NSAppearance"),
    ) else {
        return;
    };
    // SAFETY: AppKit is loaded by GPUI, this runs on the main thread, and
    // the appearance names are AppKit's own constants. A nil appearance
    // makes the app inherit the system's again.
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let appearance: *mut AnyObject = match mode {
            ThemeMode::System => std::ptr::null_mut(),
            ThemeMode::Light => msg_send![appearance_class, appearanceNamed: NSAppearanceNameAqua],
            ThemeMode::Dark => {
                msg_send![appearance_class, appearanceNamed: NSAppearanceNameDarkAqua]
            }
        };
        let _: () = msg_send![app, setAppearance: appearance];
    }
}

pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// The fill that, painted over `under`, gives the same pixels as `target`
/// painted over nothing, whatever shows through the window. `target` must be
/// at least as opaque as `under`; otherwise `target` is returned as is.
pub fn over_to_match(target: Rgba, under: Rgba) -> Rgba {
    if under.a >= 1.0 || target.a <= under.a {
        return target;
    }
    let a = (target.a - under.a) / (1.0 - under.a);
    let channel = |t: f32, u: f32| ((t * target.a - u * under.a * (1.0 - a)) / a).clamp(0.0, 1.0);
    Rgba {
        r: channel(target.r, under.r),
        g: channel(target.g, under.g),
        b: channel(target.b, under.b),
        a,
    }
}

/// An `rgb` color with an alpha.
pub fn tint(hex: u32, alpha: f32) -> Rgba {
    Rgba {
        a: alpha,
        ..rgb(hex)
    }
}

/// An opaque GPUI color from a terminal color.
pub fn rgba_of(color: Rgb) -> Rgba {
    Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: 1.0,
    }
}

/// `from` moved `amount` (0 to 1) of the way to `to`, in sRGB. Opaque.
pub fn mix(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let t = amount.clamp(0.0, 1.0);
    let channel = |a: f32, b: f32| a + (b - a) * t;
    Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 1.0,
    }
}

/// WCAG relative luminance of an opaque color.
pub fn luminance(color: Rgba) -> f32 {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// Black or white, whichever contrasts more with `background`.
pub fn extreme(background: Rgba) -> Rgba {
    let (black, white) = (rgb(0x000000), rgb(0xFFFFFF));
    if contrast(black, background) > contrast(white, background) {
        black
    } else {
        white
    }
}

/// WCAG contrast ratio between two opaque colors, from 1 to 21.
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `color`, or the first step from it toward `toward` that meets `min`
/// contrast on `background`. If even `toward` falls short, as on a
/// low-contrast theme, it keeps going to black or white, whichever is
/// further from the background; one of them always meets 4.5:1.
pub fn legible(color: Rgba, background: Rgba, toward: Rgba, min: f32) -> Rgba {
    const STEPS: u16 = 20;
    let extreme = extreme(background);
    let path = (0..=STEPS)
        .map(|i| mix(color, toward, f32::from(i) / f32::from(STEPS)))
        .chain((1..=STEPS).map(|i| mix(toward, extreme, f32::from(i) / f32::from(STEPS))));
    for candidate in path {
        if contrast(candidate, background) >= min {
            return candidate;
        }
    }
    extreme
}

#[cfg(target_os = "macos")]
fn set_blur_radius(window: &Window, radius: u8) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_int;

    // Private CoreGraphics (SkyLight) calls. They have been stable for over a
    // decade and are what every Mac terminal with adjustable blur uses.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSMainConnectionID() -> c_int;
        fn CGSSetWindowBackgroundBlurRadius(
            connection: c_int,
            window: isize,
            radius: c_int,
        ) -> c_int;
    }

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let view = handle.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: GPUI's AppKit handle is its live content NSView, and this runs
    // on the main thread inside a window update.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let number: isize = msg_send![ns_window, windowNumber];
        if number > 0 {
            CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), number, c_int::from(radius));
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_blur_radius(_: &Window, _: u8) {}

#[cfg(test)]
mod tests {
    use super::*;
    use shika_terminal::ThemeUi;

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
            background_for(&appearance, false),
            WindowBackgroundAppearance::Transparent
        );
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
            background_for(&appearance, false),
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
            background_for(&appearance, true),
            WindowBackgroundAppearance::Opaque
        );
        let chrome = chrome_for(&appearance, shika_theme(false), true);
        assert!(!chrome.glass);
        assert_eq!(
            Rgba {
                a: 1.0,
                ..chrome.column
            },
            rgb(0xF1F2EC)
        );
    }

    #[test]
    fn glass_uses_the_frosted_column_and_resting_card() {
        let appearance = Appearance {
            opacity: 58,
            blur: 44,
            translucency: Translucency::Sidebar,
        };
        let chrome = chrome_for(&appearance, shika_theme(false), false);
        assert!(chrome.glass);
        assert!((chrome.column.a - 0.58).abs() < 0.001);
        assert_eq!(
            Rgba {
                a: 1.0,
                ..chrome.column
            },
            rgb(0xF6F8F2)
        );
        assert!((chrome.card_rest.a - 0.48).abs() < 0.001);
        assert_eq!(chrome.card_selected.a, 1.0);
        let dark = chrome_for(&appearance, shika_theme(true), false);
        assert_eq!(
            Rgba {
                a: 1.0,
                ..dark.column
            },
            rgb(0x181A17)
        );
        assert!((dark.card_rest.a - 0.045).abs() < 0.001);
        assert_eq!(dark.card_selected.a, 1.0);
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
            assert!(chrome.glass);
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
        assert_eq!(light.card_asking, rgb(0xFFF5E7));
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
        assert_eq!(glass_dark.card_ready, tint(0x1C3422, 0.62));
        assert_eq!(glass_dark.term_header, rgb(0x181A17));
        assert!((glass_dark.term_header_alpha - 0.5).abs() < 1e-6);
        let glass_light = chrome_for(&glass, shika_theme(false), false);
        assert_eq!(glass_light.hairline, tint(0x000000, 0.10));
        assert_eq!(glass_light.card_rest, tint(0xFFFFFF, 0.48));
        assert_eq!(
            terminal_palette(&glass, shika_theme(false), false).background,
            Rgb::hex(0xF6F8F2)
        );
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
        assert_eq!(chrome.card_selected, rgba_of(ui.surface));
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
}
