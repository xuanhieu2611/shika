//! Shika's terminal: an engine wrapper that turns PTY bytes into snapshots,
//! an input encoder, and a GPUI view that paints the grid.
//!
//! The crate never spawns a process. The caller owns the PTY: it hands the
//! reader to [`Terminal::spawn_reader`] (or calls [`Terminal::feed`]), and
//! receives input bytes and size changes through [`PtyHost`].

mod boxdraw;
mod engine;
pub mod input;
mod io;
mod terminal;
mod theme;
mod types;
mod view;

pub use io::PtyWriter;
pub use terminal::{Notices, PtyHost, Terminal, TerminalOptions};
pub use theme::Palette;
pub use types::*;
pub use view::{
    Copy, FrameStats, KEY_CONTEXT, Paste, ScrollPageDown, ScrollPageUp, TerminalConfig,
    TerminalEvent, TerminalView, init,
};
