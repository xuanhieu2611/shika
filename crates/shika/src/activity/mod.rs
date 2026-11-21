//! Conservative live-screen detection and a clock-driven activity state machine.
//!
//! `detect` expects the terminal's current, ANSI-decoded screen in row order,
//! never scrollback or a PTY chunk. Private CLI chrome can change: unfamiliar
//! layouts deliberately return Unknown. Textual questions are not blockers.
//! This implementation is original; Herdr's manifests were consulted as research
//! only (the inspected checkout declares Apache-2.0, not MIT).

mod chrome;
mod detect;
mod evidence;
mod turn;

use shika_terminal::Terminal;

#[cfg(test)]
pub use detect::detect;
pub use evidence::OutputEvidence;
pub use turn::{Activity, Transition};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Unknown,
    Idle,
    Working,
    Blocked,
}

/// Cached classification of one agent pane, not of a card position or turn.
pub(crate) struct ScreenWatch {
    revision: Option<u64>,
    signal: Signal,
}

impl Default for ScreenWatch {
    fn default() -> Self {
        Self {
            revision: None,
            signal: Signal::Unknown,
        }
    }
}

impl ScreenWatch {
    pub(crate) fn sample(
        &mut self,
        terminal: &Terminal,
        preset: &str,
    ) -> (Signal, Option<Vec<String>>) {
        let lines = terminal
            .sample_live_screen(&mut self.revision)
            .map(|screen| {
                self.signal = detect::detect(preset, &screen.lines, screen.title.as_deref());
                screen.lines
            });
        (self.signal, lines)
    }
}

#[cfg(test)]
mod tests;
