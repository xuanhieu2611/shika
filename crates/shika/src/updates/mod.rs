//! Software updates through Sparkle. Only the bundle from
//! `scripts/release-app.sh` embeds Sparkle.framework, so `cargo run` and local
//! bundles start no updater and show no menu item, and never replace themselves
//! with a release.
//!
//! Sparkle fetches the signed feed, verifies the update, and installs it on
//! quit. Shika draws the corner notice itself. Sparkle's own windows are not
//! used. See docs/updates.md.
//!
//! `card` holds the notice's states and what a click means to Sparkle,
//! `text` its words, and `sparkle` the macOS bridge that loads the framework
//! and drives the notice from Sparkle's user driver callbacks.

mod card;
#[cfg(target_os = "macos")]
mod sparkle;
mod text;

pub use card::*;
pub use text::*;

/// Sparkle's updater, kept for the life of the app. Background checks and the
/// menu item both come through here. The corner notice is drawn by the app.
pub struct Updater {
    #[cfg(target_os = "macos")]
    inner: sparkle::Inner,
}

impl gpui::Global for Updater {}

impl Updater {
    /// Load the embedded framework and start the updater. `None` when this build
    /// has no Sparkle, or it fails to load. Call on the main thread.
    pub fn start() -> Option<Self> {
        #[cfg(target_os = "macos")]
        return sparkle::start().map(|inner| Self { inner });
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// A check the user asked for from the menu.
    pub fn check(&self) {
        #[cfg(target_os = "macos")]
        self.inner.check();
    }

    /// Notices posted since the last poll. Sparkle calls the driver on the main
    /// thread, and the window reads these on that same thread.
    pub fn poll(&self) -> Vec<UpdateEvent> {
        #[cfg(target_os = "macos")]
        return self.inner.poll();
        #[cfg(not(target_os = "macos"))]
        Vec::new()
    }

    /// The notice button the user clicked.
    pub fn respond(&self, action: CardAction) {
        #[cfg(target_os = "macos")]
        self.inner.respond(action);
        #[cfg(not(target_os = "macos"))]
        let _ = action;
    }
}

#[cfg(test)]
mod tests;
