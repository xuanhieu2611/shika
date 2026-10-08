//! Software updates through Sparkle. Only the bundle from
//! `scripts/release-app.sh` embeds Sparkle.framework, so `cargo run` and local
//! bundles start no updater and show no menu item, and never replace themselves
//! with a release. See docs/updates.md.

/// Sparkle's standard updater controller, kept for the life of the app. It
/// schedules its own background checks and owns every update window.
pub struct Updater {
    #[cfg(target_os = "macos")]
    controller: objc2::rc::Retained<objc2::runtime::AnyObject>,
}

impl gpui::Global for Updater {}

impl Updater {
    /// Load the embedded framework and start the updater. `None` when this build
    /// has no Sparkle, or it fails to load. Call on the main thread.
    pub fn start() -> Option<Self> {
        #[cfg(target_os = "macos")]
        return native::start().map(|controller| Self { controller });
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// A check the user asked for. Sparkle shows its own window, or brings an
    /// update already in progress to the front.
    pub fn check(&self) {
        #[cfg(target_os = "macos")]
        native::check(&self.controller);
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::{
        msg_send,
        rc::{Allocated, Retained},
        runtime::{AnyClass, AnyObject},
    };
    use objc2_foundation::{NSBundle, NSString};

    pub fn start() -> Option<Retained<AnyObject>> {
        let frameworks = NSBundle::mainBundle().privateFrameworksPath()?;
        let path = format!("{frameworks}/Sparkle.framework");
        if !std::path::Path::new(&path).exists() {
            return None;
        }
        let Some(bundle) = NSBundle::bundleWithPath(&NSString::from_str(&path)) else {
            eprintln!("Sparkle: {path} is not a bundle");
            return None;
        };
        // SAFETY: the release build embeds and signs this framework, and the
        // hardened runtime only loads it with the app's own Team ID.
        if let Err(error) = unsafe { bundle.loadAndReturnError() } {
            eprintln!("Sparkle: {}", error.localizedDescription());
            return None;
        }
        let Some(class) = AnyClass::get(c"SPUStandardUpdaterController") else {
            eprintln!("Sparkle: SPUStandardUpdaterController is missing");
            return None;
        };
        // SAFETY: Sparkle 2's documented initializer. Nil delegates take its
        // defaults, and the settings come from Info.plist.
        unsafe {
            let controller: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![
                controller,
                initWithStartingUpdater: true,
                updaterDelegate: None::<&AnyObject>,
                userDriverDelegate: None::<&AnyObject>
            ]
        }
    }

    pub fn check(controller: &AnyObject) {
        // SAFETY: the menu action Sparkle documents; the sender is unused.
        unsafe {
            let _: () = msg_send![controller, checkForUpdates: None::<&AnyObject>];
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_build_without_embedded_sparkle_has_no_updater() {
        assert!(super::Updater::start().is_none());
    }
}
