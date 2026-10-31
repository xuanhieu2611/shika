//! Window translucency. GPUI makes the window transparent. The blur comes
//! from the macOS window server, because GPUI's own blurred background has
//! one fixed strength. Ghostty, WezTerm, and winit set the radius the same
//! way.

//!
//! Children hold the rest of the paint: `chrome` (the `Chrome` table and the
//! derived chrome for catalog themes), `shika_chrome` (Shika Light and Dark),
//! `theme` (theme picks and the app's appearance mode), and `color` (color math).

mod chrome;
mod color;
mod shika_chrome;
mod theme;

pub use chrome::*;
pub use color::*;
pub use theme::*;

use gpui::{Window, WindowAppearance, WindowBackgroundAppearance};
use shika_core::{Appearance, Translucency};

/// Make the window transparent, or opaque again, and set the blur behind it.
/// macOS Reduce transparency forces a solid window even when opacity is lower.
/// With a background `image`, the app paints the backdrop itself: the window
/// stays opaque and the window server blurs nothing.
pub fn apply(appearance: &Appearance, image: bool, window: &mut Window) {
    apply_for(appearance, reduce_transparency(), image, window);
}

pub fn apply_for(
    appearance: &Appearance,
    reduce_transparency: bool,
    image: bool,
    window: &mut Window,
) {
    window.set_background_appearance(background_for(appearance, reduce_transparency, image));
    let radius = if appearance.is_opaque() || reduce_transparency || image {
        0
    } else {
        appearance.blur
    };
    set_blur_radius(window, radius);
}

pub fn background(appearance: &Appearance, image: bool) -> WindowBackgroundAppearance {
    background_for(appearance, reduce_transparency(), image)
}

pub fn background_for(
    appearance: &Appearance,
    reduce_transparency: bool,
    image: bool,
) -> WindowBackgroundAppearance {
    if appearance.is_opaque() || reduce_transparency || image {
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
mod tests;
