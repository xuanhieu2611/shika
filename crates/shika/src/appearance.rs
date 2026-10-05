//! Window translucency. GPUI makes the window transparent. The blur comes
//! from the macOS window server, because GPUI's own blurred background has
//! one fixed strength. Ghostty, WezTerm, and winit set the radius the same
//! way.

use gpui::{Rgba, Window, WindowAppearance, WindowBackgroundAppearance, rgb};
use shika_core::{Appearance, Translucency};

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

/// Colors for one paint of the window. Solid at 100% opacity, and whenever
/// macOS Reduce transparency is on. Glass uses the frosted tints.
#[derive(Clone, Copy, Debug)]
pub struct Chrome {
    pub glass: bool,
    pub column: Rgba,
    pub hairline: Rgba,
    pub ink: Rgba,
    pub meta: Rgba,
    pub faint: Rgba,
    pub focus: Rgba,
    pub card_rest: Rgba,
    pub card_selected: Rgba,
    pub card_ready: Rgba,
    pub card_highlight: Rgba,
    pub row_selected: Rgba,
    pub overlay: Rgba,
    pub toast_bg: Rgba,
    pub toast_fg: Rgba,
    pub scrim: Rgba,
    pub sunken: Rgba,
    pub raised: Rgba,
    pub field_line: Rgba,
    pub dashed: Rgba,
    pub primary_bg: Rgba,
    pub primary_fg: Rgba,
    pub hover: Rgba,
    pub icon: Rgba,
    pub ready_dot: Rgba,
    pub working_dot: Rgba,
    pub waiting_dot: Rgba,
    pub term_header: Rgba,
    pub term_header_alpha: f32,
    pub term_line: Rgba,
    pub term_seg: Rgba,
    pub term_seg_active: Rgba,
    pub term_fg: Rgba,
    pub term_dim: Rgba,
    pub term_faint: Rgba,
    pub term_empty: Rgba,
    pub term_empty_alpha: f32,
}

pub fn chrome_for(appearance: &Appearance, dark: bool, reduce_transparency: bool) -> Chrome {
    let glass = glass_active(appearance, reduce_transparency);
    let alpha = if glass {
        sidebar_alpha(appearance)
    } else {
        1.0
    };
    let term_alpha = terminal_alpha_for(appearance, reduce_transparency);
    let frost_term =
        glass && matches!(appearance.translucency, Translucency::SidebarAndTerminal);
    let (column, hairline, ink, meta, faint, focus) = if dark {
        (
            tint(if glass { 0x181A17 } else { 0x1A1C19 }, alpha),
            if glass {
                tint(0xFFFFFF, 0.08)
            } else {
                rgb(0x2A2D27)
            },
            rgb(0xE8EBE3),
            rgb(if glass { 0xA6AB9E } else { 0x8D9286 }),
            rgb(if glass { 0x9A9F92 } else { 0x6B7065 }),
            rgb(0xE8EBE3),
        )
    } else {
        (
            tint(if glass { 0xF6F8F2 } else { 0xF1F2EC }, alpha),
            if glass {
                tint(0x000000, 0.10)
            } else {
                rgb(0xDADDD3)
            },
            rgb(0x262824),
            rgb(if glass { 0x5F6459 } else { 0x6C7166 }),
            rgb(if glass { 0x868B7E } else { 0x9EA296 }),
            rgb(0x2F332C),
        )
    };
    let card_selected = if dark { rgb(0x272A25) } else { rgb(0xFFFFFF) };
    let card_rest = if glass {
        tint(0xFFFFFF, if dark { 0.045 } else { 0.48 })
    } else if dark {
        tint(0xFFFFFF, 0.03)
    } else {
        rgb(0xF8F9F5)
    };
    let card_ready = if glass && dark {
        tint(0x1C3422, 0.62)
    } else if glass {
        tint(0xE7FEEB, 0.78)
    } else if dark {
        rgb(0x18241A)
    } else {
        rgb(0xF1F8F0)
    };
    let card_highlight = if glass {
        tint(0xFFFFFF, if dark { 0.05 } else { 0.75 })
    } else {
        tint(0xFFFFFF, 0.0)
    };
    let overlay = if glass && dark {
        tint(0x242722, 0.78)
    } else if glass {
        tint(0xFAFBF7, 0.80)
    } else if dark {
        rgb(0x242722)
    } else {
        rgb(0xFAFAF7)
    };
    let toast_bg = if glass {
        tint(if dark { 0x363A33 } else { 0x252823 }, 0.86)
    } else if dark {
        rgb(0x363A33)
    } else {
        rgb(0x252823)
    };
    let scrim = if glass && dark {
        tint(0x000000, 0.30)
    } else if glass {
        tint(0x10120F, 0.22)
    } else if dark {
        tint(0x000000, 0.50)
    } else {
        tint(0x10120F, 0.34)
    };
    let sunken = if glass && dark {
        tint(0xFFFFFF, 0.09)
    } else if glass {
        tint(0x28341E, 0.08)
    } else if dark {
        rgb(0x30342E)
    } else {
        rgb(0xE3E6DD)
    };
    let hover = if glass && dark {
        tint(0xFFFFFF, 0.06)
    } else if glass {
        tint(0x28341E, 0.06)
    } else if dark {
        rgb(0x2E322C)
    } else {
        rgb(0xE3E6DD)
    };
    let dashed = if glass && dark {
        tint(0xFFFFFF, 0.16)
    } else if glass {
        tint(0x28341E, 0.22)
    } else if dark {
        rgb(0x383C35)
    } else {
        rgb(0xCFD3C7)
    };
    let field_line = if glass && dark {
        tint(0xFFFFFF, 0.12)
    } else if glass {
        tint(0x000000, 0.10)
    } else if dark {
        rgb(0x383C35)
    } else {
        rgb(0xCFD3C7)
    };
    let (term_header, term_line, term_seg, term_seg_active, term_empty) = if frost_term {
        (
            rgb(if dark { 0x141613 } else { 0x1A1C19 }),
            tint(0xFFFFFF, 0.07),
            tint(0xFFFFFF, 0.06),
            tint(0xFFFFFF, 0.14),
            rgb(if dark { 0x0E100D } else { 0x131512 }),
        )
    } else if dark {
        (
            rgb(0x151714),
            rgb(0x232621),
            rgb(0x1D201B),
            rgb(0x353932),
            rgb(0x121211),
        )
    } else {
        (
            rgb(0x181A17),
            rgb(0x262924),
            rgb(0x20231F),
            rgb(0x353932),
            rgb(0x131512),
        )
    };
    Chrome {
        glass,
        column,
        hairline,
        ink,
        meta,
        faint,
        focus,
        card_rest,
        card_selected,
        card_ready,
        card_highlight,
        row_selected: if dark { rgb(0x363A33) } else { rgb(0xE8EBE2) },
        overlay,
        toast_bg,
        toast_fg: if dark { rgb(0xF1F4EC) } else { rgb(0xE9ECE3) },
        scrim,
        sunken,
        raised: if dark { rgb(0x272A25) } else { rgb(0xFFFFFF) },
        field_line,
        dashed,
        primary_bg: if dark { rgb(0xE8EBE3) } else { rgb(0x2F332C) },
        primary_fg: if dark { rgb(0x1A1C19) } else { rgb(0xF2F5EC) },
        hover,
        icon: if dark { rgb(0xE8EBE3) } else { rgb(0x3C4038) },
        ready_dot: rgb(if dark { 0x68CA80 } else { 0x399A62 }),
        working_dot: rgb(if dark { 0x66ABE5 } else { 0x5A8AB3 }),
        waiting_dot: rgb(if dark { 0x6B7065 } else { 0xA3A79B }),
        term_header,
        term_header_alpha: if frost_term {
            (term_alpha - 0.1).max(0.75)
        } else {
            1.0
        },
        term_line,
        term_seg,
        term_seg_active,
        term_fg: rgb(0xD5D9CF),
        term_dim: rgb(0x878C80),
        term_faint: rgb(0x757A6E),
        term_empty,
        term_empty_alpha: term_alpha,
    }
}

pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
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
        assert_eq!(Rgba { a: 1.0, ..chrome.column }, rgb(0xF1F2EC));
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
        assert_eq!(Rgba { a: 1.0, ..chrome.column }, rgb(0xF6F8F2));
        assert!((chrome.card_rest.a - 0.48).abs() < 0.001);
        assert_eq!(chrome.card_selected.a, 1.0);
        let dark = chrome_for(&appearance, true, false);
        assert_eq!(Rgba { a: 1.0, ..dark.column }, rgb(0x181A17));
        assert!((dark.card_rest.a - 0.045).abs() < 0.001);
        assert_eq!(dark.card_selected.a, 1.0);
    }

    #[test]
    fn tint_keeps_the_color_and_sets_alpha() {
        let color = tint(0xF1F2EC, 0.5);
        assert_eq!(Rgba { a: 1.0, ..color }, rgb(0xF1F2EC));
        assert_eq!(color.a, 0.5);
    }
}
