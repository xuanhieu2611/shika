//! The Lead chat transport: one long-lived structured child per Lead, never
//! terminal bytes. `protocol::LeadProtocol` is the per-CLI wire format
//! (`claude.rs`, `codex.rs`, `pi.rs`, and `acp.rs` with its Cursor flavor in
//! `cursor.rs`); the actor and everything here are shared. All IO runs on
//! the actor.
mod acp;
mod actor;
mod claude;
mod codex;
mod cursor;
// Shared by the Codex app-server and ACP adapters; some of it only tests use.
#[cfg_attr(not(test), allow(dead_code))]
mod jsonrpc;
mod pi;
mod protocol;

pub(crate) use cursor::{CURSOR_MINIMUM, CURSOR_RULE, cursor_version_supported};

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::sync::{Mutex, mpsc};
use std::thread;

use serde::Deserialize;
use serde_json::Value;

use crate::lead_chat_store::{ChatOrigin, ChatRecord, ChatResult, ChatStore};
use actor::{Actor, Process};

const UNSUPPORTED: &str = "This Lead needs an interaction chat does not support.";
const INCOMPLETE: &str = "The reply ended before it was complete";
const MAX_FRAME: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatStatus {
    Waiting,
    Working,
    Asking,
    Ready,
}

#[derive(Clone, Debug)]
pub enum ChatEvent {
    Confirmed(String),
    Accepted,
    /// The CLI began a turn without Shika input, for example after one of the
    /// Lead's background commands finished. It counts as a submission.
    Started,
    DeliveryFailed,
    Rejected(String),
    Question(ChatQuestion),
    QuestionCancelled(String),
    Record(ChatRecord),
    Status(ChatStatus),
    Stopping,
    Resuming,
    Saved,
    SaveFailed,
    Error(String),
    Exit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChatQuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChatQuestionItem {
    pub question: String,
    #[serde(default)]
    pub header: String,
    pub options: Vec<ChatQuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatQuestion {
    pub request_id: String,
    pub questions: Vec<ChatQuestionItem>,
    pub input: Value,
}

/// What one decoded line means to the actor, in no CLI's vocabulary.
#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    /// The CLI confirmed the expected conversation inside a turn.
    Confirmed,
    /// The CLI opened a turn with no Shika input.
    Started,
    /// The turn finished; the final response, if it had one.
    Final(Option<String>),
    Failed(&'static str),
    Question(ChatQuestion),
    Cancelled(String),
    Blocked,
    Ignore,
    /// The conversation id the CLI reported, for CLIs that allocate it
    /// (Codex `thread/start`, ACP `session/new`). It must equal any id Shika
    /// already holds, and binds the history to it. The id is saved only on a
    /// later `Confirmed`, once the CLI keeps the conversation; Claude has
    /// only `Confirmed`, where the id is checked.
    Identity(String),
    /// Frames to write at once without the author: a handshake step, or the
    /// empty success a report request expects. Never a permission or
    /// question answer.
    Reply(Vec<u8>),
    /// The CLI acknowledged the send, when that is separate from the write
    /// (`LeadProtocol::write_accepts` is false).
    Accepted,
    /// The send was accepted and starts no turn; it settles with no bubble.
    Handled,
    /// The conversation now holds `ChatLaunch::instructions`, for a CLI that
    /// keeps them from its creation (Codex): created with them, or sent them
    /// after a resume. The actor records it, so the next resume sends them
    /// only when they changed.
    Instructed,
    /// A stopped turn ended but left a tool running, such as a blocking
    /// `shika wait`. The process is ended so the tool is too; the next send
    /// resumes the conversation in a new one.
    Lingering,
    /// A failure that ends the transport, like `Strings::unreadable`, for
    /// example a resume the CLI refused.
    Fatal(&'static str),
    /// A failure whose text carries the CLI's own reason, already bounded
    /// (`jsonrpc::bounded`). `fatal` ends the transport, as `Fatal` does.
    Explained {
        text: String,
        fatal: bool,
    },
    /// A resume the CLI refused because it has no such conversation: fatal,
    /// and the transport reports the id missing
    /// ([`ChatTransport::conversation_missing`]), so the app's refused-resume
    /// rule starts the Lead fresh once.
    Missing(&'static str),
    /// The protocol wrote Shika's own input, not the author's, shown as this
    /// event row: the memory notes a resumed conversation lacks (Cursor).
    /// Within a turn the row joins it; otherwise it opens a Shika-origin
    /// turn, counted as a submission as `Started` is.
    ShikaInput(&'static str),
}

/// The preset ids with a chat protocol in Core, as the transport's own
/// `&'static` names: what `Core::start_chat_lead` and `confirm_lead_chat`
/// accept. Whether the app offers a chat Lead for one is
/// [`lead_chat_supported`].
pub(crate) fn chat_cli(preset: &str) -> Option<&'static str> {
    ["claude", "codex", "pi", "cursor"]
        .into_iter()
        .find(|cli| *cli == preset)
}

/// The CLIs shipped as chat Leads: the app runs a Lead on one of these in
/// the chat, and every other Lead in a terminal. Each has a chat protocol
/// (`chat_cli`) and passed its acceptance (`docs/lead-chat.md`); a CLI joins
/// this list in the commit that records that acceptance, which is the one
/// switch the app needs.
pub fn lead_chat_supported(cli: &str) -> bool {
    const SHIPPED: [&str; 4] = ["claude", "codex", "pi", "cursor"];
    SHIPPED.contains(&cli) && chat_cli(cli).is_some()
}

/// The error for a CLI that answered with another conversation.
pub(crate) fn other_conversation(cli: &str) -> &'static str {
    protocol::strings_for(cli)
        .map(|strings| strings.other_conversation)
        .unwrap_or("The Lead returned a different conversation")
}

pub(crate) struct ChatLaunch {
    /// The chat preset id; `protocol::for_cli` picks its wire format.
    pub cli: &'static str,
    pub usage: Option<crate::usage::Source>,
    /// Registers `usage` when the CLI reports its id, for a Codex thread
    /// that cwd discovery cannot find.
    pub usage_threads: Option<crate::usage::CodexThreads>,
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub path: String,
    pub env: Vec<(String, String)>,
    pub instructions: String,
    /// The project's memory notes, which also end `instructions`, for a CLI
    /// sent them on their own on a resume (Cursor, see `Parsed::ShikaInput`).
    pub notes: Vec<String>,
    /// Empty until a CLI that allocates its own id reports it.
    pub conversation: String,
    pub resumed: bool,
    /// The conversation already holds exactly `instructions` (the store's
    /// record, see `Parsed::Instructed`). Set by the transport.
    pub instructed: bool,
    pub history: Option<String>,
    pub data: PathBuf,
    pub project: String,
}

enum CommandIn {
    Send {
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
        epoch: u64,
    },
    Interrupt,
    RetrySave,
    Answer {
        request_id: String,
        answer: Option<Value>,
    },
    Stop,
    Line(u64, Vec<u8>),
    BadFrame(u64),
    Eof(u64),
}
pub(crate) struct ChatTransport {
    stop: Arc<AtomicBool>,
    missing: Arc<AtomicBool>,
    cancel: Arc<AtomicU64>,
    pub history: String,
    input: mpsc::Sender<CommandIn>,
    events: Mutex<mpsc::Receiver<ChatEvent>>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}
impl ChatTransport {
    pub fn start(launch: ChatLaunch) -> ChatResult<Self> {
        let protocol =
            protocol::for_cli(launch.cli).ok_or_else(|| "Could not start the Lead".to_string())?;
        Self::start_with(launch, protocol)
    }
    fn start_with(
        mut launch: ChatLaunch,
        mut protocol: Box<dyn protocol::LeadProtocol>,
    ) -> ChatResult<Self> {
        let mut store = ChatStore::open(&launch.data, &launch.project)?;
        if launch
            .history
            .as_ref()
            .is_some_and(|id| id != &store.lifetime)
        {
            return Err("Could not read this conversation: history identity changed".into());
        }
        let history = store.lifetime.clone();
        store.recover()?;
        // A CLI that reports its own id binds when it does (`Parsed::Identity`).
        let boundary = if launch.conversation.is_empty() {
            None
        } else {
            store.bind(protocol.cli(), &launch.conversation)?
        };
        launch.instructed = !launch.conversation.is_empty()
            && store.instructed(protocol.cli(), &launch.conversation, &launch.instructions);
        let next_turn = store.next_turn()?;
        let (input, rx) = mpsc::channel();
        let (frames, frame_rx) = mpsc::sync_channel(8);
        let (events, output) = mpsc::channel();
        if let Some(boundary) = boundary {
            let _ = events.send(ChatEvent::Record(boundary));
        }
        let child = Process::spawn(&launch, protocol.as_mut(), 1, frames.clone())?;
        let actor_frames = frames;
        let cancel = Arc::new(AtomicU64::new(0));
        let actor_cancel = cancel.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let actor_stop = stop.clone();
        let missing = Arc::new(AtomicBool::new(false));
        let actor_missing = missing.clone();
        let join = thread::Builder::new()
            .name("lead-chat".into())
            .spawn(move || {
                Actor {
                    cancel: actor_cancel,
                    stop: actor_stop,
                    missing: actor_missing,
                    cancel_seen: 0,
                    launch,
                    protocol,
                    store,
                    output: events,
                    frames: actor_frames,
                    child: Some(child),
                    generation: 1,
                    next_turn,
                    turn: None,
                    stopping: None,
                    soft_stop: false,
                    signalled: false,
                    unacknowledged: false,
                    exiting: false,
                    save_failed: false,
                    pending: Vec::new(),
                    active_input: None,
                    failed: false,
                }
                .run(rx, frame_rx);
            })
            .map_err(|_| "Could not start the Lead".to_string())?;
        Ok(Self {
            stop,
            missing,
            cancel,
            history,
            input,
            events: Mutex::new(output),
            join: Mutex::new(Some(join)),
        })
    }
    pub fn send(
        &self,
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
    ) -> ChatResult<()> {
        self.input
            .send(CommandIn::Send {
                text,
                origin,
                events,
                epoch: self.cancel.load(Ordering::Acquire),
            })
            .map_err(|_| "The Lead is not running".into())
    }
    pub fn interrupt(&self) {
        self.cancel.fetch_add(1, Ordering::AcqRel);
        let _ = self.input.send(CommandIn::Interrupt);
    }
    /// The CLI said the conversation this transport resumed does not exist
    /// (`Parsed::Missing`).
    pub fn conversation_missing(&self) -> bool {
        self.missing.load(Ordering::Acquire)
    }
    pub fn retry_save(&self) {
        let _ = self.input.send(CommandIn::RetrySave);
    }
    pub fn answer(&self, request_id: String, answer: Option<Value>) {
        let _ = self.input.send(CommandIn::Answer { request_id, answer });
    }
    pub fn drain(&self) -> Vec<ChatEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_iter()
            .collect()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.cancel.fetch_add(1, Ordering::AcqRel);
        let _ = self.input.send(CommandIn::Stop);
        if let Some(join) = self.join.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = join.join();
        }
    }
}
impl Drop for ChatTransport {
    fn drop(&mut self) {
        self.stop();
    }
}
