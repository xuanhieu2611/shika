//! The wire protocol between the `shika` command and the running app, for the
//! Lead agent. Reference: `docs/shika-cli.md`; design: `docs/lead-agent.md`.
//!
//! The client is the app binary run as `shika <command>` inside a Lead's
//! terminal. The server lives in the app crate (`crates/shika/src/control.rs`)
//! because task status and the dialogs are UI state. This module holds what
//! both sides share: request and reply types, JSON-line framing, argument
//! parsing, plain-text rendering, the per-run control directory, and the
//! token generator.
//!
//! One connection carries one request line and one reply line, both JSON,
//! over a Unix socket whose path is in `SHIKA_SOCKET`. Every request names
//! its [`PROTOCOL_VERSION`] and carries the Lead's `SHIKA_TOKEN`. The server
//! refuses another version, so a stale `shika` never misreads a newer app.
//! Bump the version whenever a request or reply changes shape.

mod args;
mod dir;
mod protocol;
mod render;
mod wire;

pub use args::*;
pub use dir::*;
pub use protocol::*;
pub use render::*;
pub use wire::*;

/// Bumped when a request or reply changes shape. The server refuses a request
/// with another version, so a stale `shika` never misreads a newer app.
pub const PROTOCOL_VERSION: u32 = 6;

/// The longest request line the server reads. A longer one is rejected.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Default `shika wait` timeout, under the default command timeout of the
/// agent CLIs.
pub const DEFAULT_WAIT_SECS: u64 = 100;

/// The most scrollback lines `shika read --lines` returns above the screen.
pub const MAX_READ_LINES: usize = 2000;

/// The most keys one `shika key` sends.
pub const MAX_KEYS: usize = 32;

/// The longest `shika report` the server keeps, in bytes.
pub const MAX_REPORT_BYTES: usize = 4096;

/// The CLI ids `shika new --cli` accepts, the same as the preset ids.
pub const CLI_IDS: [&str; 4] = ["claude", "codex", "cursor", "pi"];

/// Why a control call failed. The client prints it and exits with status 2.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// The socket could not be reached: Shika is not running, or the
    /// environment is stale.
    #[error("{0}")]
    Connect(String),
    /// A read, write, or timeout on the socket failed.
    #[error("Control socket failed: {0}")]
    Io(String),
    /// The other side sent something that is not a valid line of the protocol.
    #[error("Bad control message: {0}")]
    Protocol(String),
    /// The request line passed [`MAX_REQUEST_BYTES`].
    #[error("The request is larger than {} bytes.", MAX_REQUEST_BYTES)]
    TooLarge,
    /// The control directory could not be created.
    #[error("Could not set up the control directory: {0}")]
    Setup(String),
}

/// Control calls return [`ControlError`] by default.
pub type Result<T, E = ControlError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests;
