//! Window translucency. GPUI makes the window transparent. The blur comes
//! from the macOS window server, because GPUI's own blurred background has
//! one fixed strength. Ghostty, WezTerm, and winit set the radius the same
//! way.

use crate::model::Status;
use gpui::{Rgba, Window, WindowAppearance, WindowBackgroundAppearance, rgb};
use shika_core::{Appearance, Translucency};
use shika_terminal::Palette;

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

/// Alpha for the terminal side: its header, its empty state, and the
/// terminal's default background. Sidebar-only frost keeps the terminal
/// solid. When frost covers the terminal, the alpha stays at or above 0.85
/// so text never sits on a visible photo.
pub fn terminal_alpha_for(appearance: &Appearance, reduce_transparency: bool) -> f32 {
    if !glass_active(appearance, reduce_transparency) {
        return 1.0;
    }
    match appearance.translucency {
        Translucency::Sidebar => 1.0,
        Translucency::SidebarAndTerminal => sidebar_alpha(appearance).max(0.85),
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

/// One status hue: the dot, text that meets contrast on the column, and the
/// summary chip fill.
#[derive(Clone, Copy, Debug)]
pub struct StatusColors {
    pub dot: Rgba,
    pub text: Rgba,
    pub chip: Rgba,
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
    pub working: StatusColors,
    pub waiting: StatusColors,
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
            Status::Working => self.working,
            Status::Waiting => self.waiting,
        }
    }
}

pub fn chrome_for(appearance: &Appearance, dark: bool, reduce_transparency: bool) -> Chrome {
    let glass = glass_active(appearance, reduce_transparency);
    let alpha = if glass {
        sidebar_alpha(appearance)
    } else {
        1.0
    };
    let term_alpha = terminal_alpha_for(appearance, reduce_transparency);
    let frost_term = glass && matches!(appearance.translucency, Translucency::SidebarAndTerminal);
    // Pick by mode: solid light, solid dark, glass light, glass dark.
    let pick =
        |light: Rgba, dark_solid: Rgba, glass_light: Rgba, glass_dark: Rgba| match (glass, dark) {
            (false, false) => light,
            (false, true) => dark_solid,
            (true, false) => glass_light,
            (true, true) => glass_dark,
        };
    let column = tint(
        match (glass, dark) {
            (false, false) => 0xF1F2EC,
            (false, true) => 0x1A1C19,
            (true, false) => 0xF6F8F2,
            (true, true) => 0x181A17,
        },
        alpha,
    );
    let hover = pick(
        tint(0x141E0A, 0.045),
        tint(0xFFFFFF, 0.05),
        tint(0x28341E, 0.06),
        tint(0xFFFFFF, 0.06),
    );
    let ink_3 = pick(rgb(0x6C7166), rgb(0x8D9286), rgb(0x5F6459), rgb(0xA6AB9E));
    let (term_header, term_line, term_seg, term_seg_active, term_hover, term_empty) = if frost_term
    {
        (
            rgb(if dark { 0x141613 } else { 0x1A1C19 }),
            tint(0xFFFFFF, 0.07),
            tint(0xFFFFFF, 0.06),
            tint(0xFFFFFF, 0.14),
            tint(0xFFFFFF, 0.06),
            rgb(if dark { 0x0E100D } else { 0x131512 }),
        )
    } else if dark {
        (
            rgb(0x151714),
            rgb(0x232621),
            rgb(0x1D201B),
            rgb(0x353932),
            rgb(0x1D201B),
            rgb(0x10120F),
        )
    } else {
        (
            rgb(0x181A17),
            rgb(0x262924),
            rgb(0x20231F),
            rgb(0x353932),
            rgb(0x20231F),
            rgb(0x131512),
        )
    };
    let term_header_alpha = if frost_term {
        (term_alpha - 0.1).max(0.75)
    } else {
        1.0
    };
    let palette = if dark {
        Palette::shika_dark()
    } else {
        Palette::shika()
    };
    let term_surface = Rgba {
        r: f32::from(palette.background.r) / 255.0,
        g: f32::from(palette.background.g) / 255.0,
        b: f32::from(palette.background.b) / 255.0,
        a: term_alpha,
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
                chip: rgb(0x1C3723),
            }
        } else {
            StatusColors {
                dot: rgb(0x45B164),
                text: rgb(0x21763C),
                chip: rgb(0xD1F2D7),
            }
        },
        working: if dark {
            StatusColors {
                dot: rgb(0x66ABE5),
                text: rgb(0x8CC4F4),
                chip: rgb(0x1E3243),
            }
        } else {
            StatusColors {
                dot: rgb(0x4493D0),
                text: rgb(0x266EA4),
                chip: rgb(0xD5EBFE),
            }
        },
        waiting: StatusColors {
            dot: if dark { rgb(0x6B7065) } else { rgb(0xA3A79B) },
            text: ink_3,
            chip: hover,
        },
        term_header,
        term_header_alpha,
        term_surface,
        term_tab: over_to_match(term_surface, with_alpha(term_header, term_header_alpha)),
        term_line,
        term_seg,
        term_seg_active,
        term_hover,
        term_fg: rgb(0xC5CABE),
        term_white: rgb(0xF2F5EC),
        term_dim: rgb(0x878C80),
        term_faint: rgb(0x757A6E),
        term_fainter: rgb(0x5A5F55),
        term_empty,
        term_empty_alpha: term_alpha,
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
    fn terminal_frost_stays_at_or_above_the_floor() {
        let mut appearance = Appearance {
            opacity: 80,
            blur: 0,
            translucency: Translucency::SidebarAndTerminal,
        };
        assert_eq!(terminal_alpha_for(&appearance, false), 0.85);
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
        let chrome = chrome_for(&appearance, false, true);
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
        let chrome = chrome_for(&appearance, false, false);
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
        let dark = chrome_for(&appearance, true, false);
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
        for dark in [false, true] {
            let chrome = chrome_for(&appearance, dark, false);
            assert!(chrome.glass);
            assert_eq!(chrome.overlay.a, 1.0);
            assert_eq!(chrome.toast_bg.a, 1.0);
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
            (40, Translucency::SidebarAndTerminal),
            (90, Translucency::SidebarAndTerminal),
        ] {
            let appearance = Appearance {
                opacity,
                blur: 30,
                translucency,
            };
            for dark in [false, true] {
                let chrome = chrome_for(&appearance, dark, false);
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
                    assert!((got - want).abs() < 1e-4, "{opacity} {dark}: {got} {want}");
                }
            }
        }
    }

    #[test]
    fn tint_keeps_the_color_and_sets_alpha() {
        let color = tint(0xF1F2EC, 0.5);
        assert_eq!(Rgba { a: 1.0, ..color }, rgb(0xF1F2EC));
        assert_eq!(color.a, 0.5);
    }
}
