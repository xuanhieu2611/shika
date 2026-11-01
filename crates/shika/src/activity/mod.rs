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

#[cfg(test)]
mod tests;
