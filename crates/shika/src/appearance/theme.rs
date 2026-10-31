//! Theme picks from the catalog, and the app's forced light or dark
//! appearance.
use gpui::WindowAppearance;
use shika_core::{ThemeMode, ThemeSettings};
use shika_terminal::{Theme, catalog};

use super::is_dark;

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
