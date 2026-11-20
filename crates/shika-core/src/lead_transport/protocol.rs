//! The boundary between the shared Lead chat actor and one CLI's wire format.
//! The actor owns the process, persistence, fencing and events; a protocol
//! only builds frames and decodes lines, and never does IO itself.
use std::time::Instant;

use serde_json::Value;

use super::{ChatLaunch, Parsed};

/// How Stop generating reaches the CLI.
pub(super) enum Interrupt {
    /// `LeadProtocol::stop_signal` to the child's process group; the child
    /// exits and the next send starts a new one on the same conversation.
    Signal,
    /// A frame that asks the CLI to end the turn and keep running. The turn
    /// settles as stopped on the protocol's next `Final` or `Handled`, or
    /// `Lingering`, which ends the process after all. One that has not ended
    /// 2 seconds later gets the stop signal.
    Frame(Vec<u8>),
    /// Nothing is running on the CLI's side, for example a send it has not
    /// been given yet: the turn settles as stopped at once.
    Idle,
}

/// How Stop Lead and quit end the child.
pub(super) enum Shutdown {
    /// The same signal to the group as `Interrupt::Signal`.
    Signal,
    /// Close stdin, which the CLI documents as a request to exit. One still
    /// running 2 seconds later gets the stop signal.
    CloseStdin,
}

/// CLI-specific messages the actor shows. `unreadable` and
/// `other_conversation` also end the transport when a turn fails with them.
pub(super) struct Strings {
    /// The stream could not be decoded, or the handshake did not finish.
    pub unreadable: &'static str,
    /// The CLI reported a conversation other than the one Shika asked for.
    pub other_conversation: &'static str,
    /// The first process could not be spawned.
    pub start_failed: &'static str,
    /// Its stdin could not be made nonblocking.
    pub input_failed: &'static str,
    /// The handshake frames could not be written.
    pub handshake_failed: &'static str,
    /// A later process, started for a send, could not be spawned.
    pub restart_failed: &'static str,
}

/// One per Lead, kept across the processes the actor starts for it. Every
/// method returns at once; frames are one or more LF-terminated lines.
pub(super) trait LeadProtocol: Send {
    /// The preset id the history is bound to: "claude", "codex", "pi", ...
    fn cli(&self) -> &'static str;
    fn strings(&self) -> &'static Strings;
    /// The arguments for a new process on `launch.conversation`, resuming it
    /// when `launch.resumed` is set.
    fn args(&self, launch: &ChatLaunch) -> Vec<String>;
    /// Variables removed from the child's environment beyond the git and
    /// worker report ones every Lead loses.
    fn env_remove(&self) -> &'static [&'static str] {
        &[]
    }
    /// A new process started: reset all per-process state and return the
    /// frames written before anything else. Later handshake steps that wait
    /// for a response go out as `Parsed::Reply`.
    fn handshake(&mut self, launch: &ChatLaunch) -> Vec<u8>;
    /// The handshake finished. A process not ready 10 seconds after spawn
    /// fails with `Strings::unreadable`.
    fn ready(&self) -> bool;
    /// The CLI version the handshake reported, shown with `unreadable`.
    fn version(&self) -> Option<&str> {
        None
    }
    /// Opens a turn for `text`, resetting the per-turn decoder state, and
    /// returns its frame.
    fn send(&mut self, text: &str) -> Vec<u8>;
    /// True when a completed write is the CLI's acceptance. Otherwise the turn
    /// waits for `Parsed::Accepted` (or `Handled`) from the stream.
    fn write_accepts(&self) -> bool {
        true
    }
    /// The frame that answers the pending question `request_id`, or denies it
    /// when `answer` is None.
    fn answer(&mut self, request_id: &str, answer: Option<Value>) -> Vec<u8>;
    /// Stop generating. Drops any unpublished candidate.
    fn interrupt(&mut self) -> Interrupt;
    fn shutdown(&self) -> Shutdown;
    /// The signal that ends the process group for `Interrupt::Signal`, or
    /// when a gentler stop did not end it in time. SIGKILL follows 2 seconds
    /// later.
    fn stop_signal(&self) -> i32 {
        libc::SIGINT
    }
    /// Which lines longer than `MAX_FRAME` the reader may drop unread
    /// instead of failing the stream, judged by their first bytes: records
    /// the protocol never reads. Runs on the reader thread.
    fn oversized_skippable(&self) -> fn(&[u8]) -> bool {
        |_| false
    }
    /// Decodes one complete line of the CLI's stdout.
    fn parse(&mut self, line: &[u8]) -> Parsed;
    /// Called after every line and about every 25ms while a process runs, so
    /// requests can expire without a line arriving. The actor calls it again
    /// until it returns `Ignore`, before the next line or command, so a
    /// protocol can queue the further results one line has.
    fn poll(&mut self, _now: Instant) -> Parsed {
        Parsed::Ignore
    }
}

/// The protocol for a chat-capable preset id. None for any other, or when
/// Pi's extension file cannot be written.
pub(super) fn for_cli(cli: &str) -> Option<Box<dyn LeadProtocol>> {
    match cli {
        "claude" => Some(Box::new(super::claude::ClaudeStream::new(String::new()))),
        "codex" => Some(Box::new(super::codex::CodexAppServer::new())),
        "pi" => Some(Box::new(super::pi::PiRpc::installed()?)),
        _ => None,
    }
}

/// A chat preset's messages, without starting its protocol.
pub(super) fn strings_for(cli: &str) -> Option<&'static Strings> {
    let protocol: Box<dyn LeadProtocol> = match cli {
        "claude" => Box::new(super::claude::ClaudeStream::new(String::new())),
        "codex" => Box::new(super::codex::CodexAppServer::new()),
        "pi" => Box::new(super::pi::PiRpc::new()),
        _ => return None,
    };
    Some(protocol.strings())
}
