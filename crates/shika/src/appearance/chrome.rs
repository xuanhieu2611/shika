//! The `Chrome` color table for one paint, the frame both kinds of chrome
//! share, the derived chrome for catalog themes, and diff hues.
use gpui::{Rgba, rgb};
use shika_core::{Appearance, Translucency};
use shika_terminal::{Palette, Rgb, Theme, catalog};

use super::shika_chrome::shika_chrome;
use super::{
    contrast, extreme, glass_active, legible, mix, over_to_match, rgba_of, sidebar_alpha,
    terminal_alpha_for, with_alpha,
};
use crate::model::Status;

/// One status hue: the dot, and text that meets contrast on the column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusColors {
    pub dot: Rgba,
    pub text: Rgba,
}

/// One diff hue in the Changes panel: the opaque row tint, and the text that
/// meets 4.5:1 on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffColors {
    pub tint: Rgba,
    pub text: Rgba,
}

/// Colors for one paint of the window. Solid at 100% opacity, and whenever
/// macOS Reduce transparency is on. Glass uses the frosted tints. The names
/// follow `design/DESIGN.md`: `ink_1` is `--ink-1`, `line_2` is `--line-2`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    pub column: Rgba,
    /// `--line-1`: the column edge.
    pub hairline: Rgba,
    /// `--line-2`: the footer and picker footer rules.
    pub line_2: Rgba,
    /// Borders of buttons and number fields.
    pub line_control: Rgba,
    pub ink_1: Rgba,
    pub ink_2: Rgba,
    pub ink_3: Rgba,
    pub ink_4: Rgba,
    /// Meta separators (`·`).
    pub ink_5: Rgba,
    pub focus: Rgba,
    /// The selected card: an ink wash a step stronger than `hover`.
    pub card_selected: Rgba,
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
    /// The terminal palette's foreground: context lines in the Changes panel.
    pub term_text: Rgba,
    /// Added and removed lines in the Changes panel, and the file header
    /// counts. Never chrome.
    pub diff_added: DiffColors,
    pub diff_removed: DiffColors,
}

impl Chrome {
    /// Daily usage: neutral, opaque steps from the solid overlay toward ink.
    /// Zero is a faint wash that still marks the square without a border;
    /// nonzero buckets strengthen in even steps without status hues.
    pub fn usage_intensities(&self) -> [Rgba; 5] {
        [0.08, 0.26, 0.46, 0.66, 0.86].map(|amount| mix(self.overlay, self.ink_1, amount))
    }
    /// File-card header wash, composed once into an opaque fill. Using a
    /// translucent wash over the frosted list would add a second alpha layer.
    pub fn diff_file_header(&self) -> Rgba {
        with_alpha(
            mix(self.term_surface, self.term_hover, self.term_hover.a),
            1.,
        )
    }

    pub fn diff_file_header_text(&self, color: Rgba) -> Rgba {
        legible(
            color,
            self.diff_file_header(),
            self.term_white,
            TEXT_CONTRAST,
        )
    }

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
pub(super) struct Frame {
    pub(super) glass: bool,
    /// The column's alpha: the Settings opacity in glass, otherwise 1.
    pub(super) alpha: f32,
    pub(super) term_alpha: f32,
    /// Frost covers the terminal side too.
    frost_term: bool,
    pub(super) column_base: Rgb,
    pub(super) palette: Palette,
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
    pub(super) fn term_surfaces(&self, solid_header: Rgba) -> TermSurfaces {
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

pub(super) struct TermSurfaces {
    pub(super) header: Rgba,
    pub(super) header_alpha: f32,
    pub(super) surface: Rgba,
    pub(super) tab: Rgba,
    /// The empty state's color before its alpha.
    pub(super) empty: Rgba,
}

/// A diff hue from the terminal palette's ANSI `index` (2 green for added, 1
/// red for removed). The tint is the terminal background mixed toward the
/// hue, 8% on a light theme and 10% on a dark one, and opaque, like a
/// CLI-colored cell. The text darkens the hue toward black (lightens it
/// toward white on a dark theme) until it meets 4.5:1 on that tint, which
/// keeps the hue. Every theme, Shika's included, derives them this way.
pub fn diff_colors(palette: &Palette, dark: bool, index: usize) -> DiffColors {
    let hue = rgba_of(palette.ansi[index]);
    let tint = quantize(mix(
        rgba_of(palette.background),
        hue,
        if dark { 0.10 } else { 0.08 },
    ));
    // Checked on the 8-bit colors the screen shows, in 1% steps, so the
    // text stays as close to the palette's hue as the ratio allows.
    let toward = extreme(tint);
    let text = (0..=100)
        .map(|i| quantize(mix(hue, toward, f32::from(i as u8) / 100.)))
        .find(|candidate| contrast(*candidate, tint) >= TEXT_CONTRAST)
        .unwrap_or(toward);
    DiffColors { tint, text }
}

/// A color rounded to 8 bits a channel, as it reaches the screen.
fn quantize(color: Rgba) -> Rgba {
    let channel = |c: f32| (c * 255.).round() / 255.;
    Rgba {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: color.a,
    }
}

/// Text on the column meets this contrast ratio (WCAG AA for body text).
pub const TEXT_CONTRAST: f32 = 4.5;

/// Chrome for any theme but Shika's, from its palette and `ui`:
///
/// - The theme's background is the column and the terminal, solid and in
///   glass. `ui.surface` is raised controls and popups;
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
///   Waiting is the ink scale's grey.
/// - The primary button is the foreground with background text; the toast
///   is inverted the same way. Popups stay solid.
fn derived_chrome(frame: &Frame, theme: &Theme) -> Chrome {
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
    let term = frame.term_surfaces(mantle);
    Chrome {
        column: with_alpha(rgba_of(frame.column_base), frame.alpha),
        hairline: ink(side(0.12, 0.08)),
        line_2: ink(side(0.10, 0.07)),
        line_control: ink(side(0.14, 0.12)),
        ink_1: fg,
        ink_2,
        ink_3,
        ink_4,
        ink_5,
        focus: fg,
        card_selected: ink(side(0.10, 0.18)),
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
        term_text: fg,
        diff_added: diff_colors(&frame.palette, dark, 2),
        diff_removed: diff_colors(&frame.palette, dark, 1),
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
