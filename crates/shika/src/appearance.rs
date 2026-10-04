//! Window translucency. GPUI makes the window transparent. The blur comes
//! from the macOS window server, because GPUI's own blurred background has
//! one fixed strength. Ghostty, WezTerm, and winit set the radius the same
//! way.

use gpui::{Rgba, Window, WindowBackgroundAppearance, rgb};
use shika_core::{Appearance, Translucency};

/// Make the window transparent, or opaque again, and set the blur behind it.
pub fn apply(appearance: &Appearance, window: &mut Window) {
    window.set_background_appearance(background(appearance));
    let radius = if appearance.is_opaque() {
        0
    } else {
        appearance.blur
    };
    set_blur_radius(window, radius);
}

pub fn background(appearance: &Appearance) -> WindowBackgroundAppearance {
    if appearance.is_opaque() {
        WindowBackgroundAppearance::Opaque
    } else {
        WindowBackgroundAppearance::Transparent
    }
}

/// Alpha for the sidebar background.
pub fn sidebar_alpha(appearance: &Appearance) -> f32 {
    f32::from(appearance.opacity.min(100)) / 100.0
}

/// Alpha for the terminal side: its header, its empty state, and the
/// terminal's default background.
pub fn terminal_alpha(appearance: &Appearance) -> f32 {
    match appearance.translucency {
        Translucency::Sidebar => 1.0,
        Translucency::SidebarAndTerminal => sidebar_alpha(appearance),
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
        assert_eq!(terminal_alpha(&appearance), 1.0);
        assert_eq!(
            background(&appearance),
            WindowBackgroundAppearance::Transparent
        );
    }

    #[test]
    fn both_share_one_opacity_and_full_opacity_is_an_opaque_window() {
        let mut appearance = Appearance {
            opacity: 80,
            blur: 0,
            translucency: Translucency::SidebarAndTerminal,
        };
        assert_eq!(terminal_alpha(&appearance), 0.8);
        appearance.opacity = 100;
        assert_eq!(terminal_alpha(&appearance), 1.0);
        assert_eq!(background(&appearance), WindowBackgroundAppearance::Opaque);
    }

    #[test]
    fn tint_keeps_the_color_and_sets_alpha() {
        let color = tint(0xF1F2EC, 0.5);
        assert_eq!(Rgba { a: 1.0, ..color }, rgb(0xF1F2EC));
        assert_eq!(color.a, 0.5);
    }
}
