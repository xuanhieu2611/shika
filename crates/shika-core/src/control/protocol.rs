//! The request and reply types both sides serialize, the key names
//! `shika key` accepts, and the worker's report rules.

use serde::{Deserialize, Serialize};

use super::{MAX_REPORT_BYTES, PROTOCOL_VERSION};
use crate::memory::MemoryNote;

/// One request line. `token` is the `SHIKA_TOKEN` of the calling Lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// [`PROTOCOL_VERSION`] of the client that built this request.
    pub version: u32,
    pub token: String,
    pub command: Command,
}

impl Request {
    /// A request at the current protocol version.
    pub fn new(token: impl Into<String>, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            token: token.into(),
            command,
        }
    }

    /// For the server: the refusal to send when the client speaks another
    /// protocol version. None when the version matches.
    pub fn version_refusal(&self) -> Option<Reply> {
        (self.version != PROTOCOL_VERSION).then(|| Reply::Refused {
            reason: format!(
                "This shika speaks protocol {} but the running Shika app speaks {}. Restart the Lead from the app.",
                self.version, PROTOCOL_VERSION
            ),
        })
    }
}

/// What the Lead asks for. Serialized with a `type` tag in snake_case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Print the Lead guide.
    Help,
    /// List the project's tasks.
    Tasks,
    /// Start a worker. `cli` is a preset id from [`CLI_IDS`](super::CLI_IDS).
    New {
        cli: String,
        /// Start from this branch instead of the project's base.
        base: Option<String>,
        prompt: String,
    },
    /// One task.
    Status {
        /// A task id, as `tasks` prints it.
        task: String,
    },
    /// Block until one of `tasks` (empty: every live task this Lead started)
    /// is Ready, Asking, or exited, or `timeout_secs` pass.
    Wait {
        tasks: Vec<String>,
        /// The server caps this at 600 seconds.
        timeout_secs: u64,
    },
    /// The task's agent terminal as plain text: the screen and up to `lines`
    /// lines of scrollback above it. Any task in the project.
    Read { task: String, lines: usize },
    /// The task's changes as text, as the Changes panel computes them. Any
    /// task in the project.
    Diff { task: String, stat: bool },
    /// Type `text` into a worker's agent terminal as a paste, then Enter
    /// unless `enter` is false. Only a worker this Lead started.
    Send {
        task: String,
        text: String,
        enter: bool,
    },
    /// Press `keys` (names from [`KEY_NAMES`] or one of a-z, 0-9) in order in
    /// a worker's agent terminal. Only a worker this Lead started.
    Key { task: String, keys: Vec<String> },
    /// Publish a worker this Lead started, without a dialog: what Create PR
    /// does when the author confirms it untouched. Blocks until done. The
    /// reply is the PR URL.
    Pr {
        task: String,
        /// Commit and PR title. None: the task title, as Create PR prefills.
        #[serde(default)]
        title: Option<String>,
        /// PR description. None: gh fills it from the commits.
        #[serde(default)]
        body: Option<String>,
    },
    /// Close a worker this Lead started: at once when Close needs no
    /// confirmation, otherwise through the close dialog. Blocks until done.
    Close { task: String },
    /// The project's Lead memory notes, numbered from 1.
    Memory,
    /// Save one note in the project's Lead memory.
    Remember { text: String },
    /// Remove note `note` (1-based) from the project's Lead memory.
    Forget { note: usize },
    /// A worker's own account of its turn. Only a worker's token may send
    /// it, and only it may send anything else than `Help`.
    Report { text: String },
}

/// The named keys `shika key` accepts besides single letters and digits.
pub const KEY_NAMES: [&str; 9] = [
    "enter",
    "escape",
    "up",
    "down",
    "left",
    "right",
    "tab",
    "space",
    "backspace",
];

/// Normalizes a key name for `shika key`: the names in [`KEY_NAMES`] in any
/// case, or one of `a`-`z`, `0`-`9`. None for anything else.
pub fn key_name(arg: &str) -> Option<String> {
    let lower = arg.to_ascii_lowercase();
    let mut chars = lower.chars();
    let single = matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_lowercase() || c.is_ascii_digit());
    (single || KEY_NAMES.contains(&lower.as_str())).then_some(lower)
}

/// The coarse state of a task as the Lead sees it. The app maps its own UI
/// status onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Starting,
    Working,
    Waiting,
    Asking,
    Ready,
    Exited,
}

impl TaskStatus {
    /// The lowercase word the text output and JSON use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Asking => "asking",
            Self::Ready => "ready",
            Self::Exited => "exited",
        }
    }
}

/// What a task changed, as the card's diff stat shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDiffStat {
    pub files: u64,
    pub added: u64,
    pub removed: u64,
}

/// One task as the Lead sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// The session id, the handle every command takes.
    pub id: String,
    pub title: String,
    /// The CLI preset id: `claude`, `codex`, `cursor`, or `pi`.
    pub cli: String,
    pub status: TaskStatus,
    /// Seconds into the current turn. None outside Working.
    pub elapsed_secs: Option<u64>,
    /// The task branch; empty for a Lead.
    pub branch: String,
    /// Present once the card has fetched its diff stat (when it turned Ready).
    pub diff_stat: Option<TaskDiffStat>,
    /// The pull request number, once one exists.
    pub pr: Option<u64>,
    /// True when the calling Lead started this task.
    pub started_by_lead: bool,
    /// The task's worktree, absolute. The Lead may read files there.
    pub path: String,
    /// The worker's latest `shika report` from its current turn. None when
    /// it has not reported in this turn.
    pub report: Option<String>,
}

/// A task that needs the Lead; `task.status` says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitEvent {
    pub task: TaskInfo,
}

/// One reply line. Serialized with a `type` tag in snake_case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// The Lead guide, for `help`.
    Help { text: String },
    /// Every task in the Lead's project except the Lead itself.
    Tasks { tasks: Vec<TaskInfo> },
    /// A worker that `new` started, once its CLI is running.
    Started { task: TaskInfo },
    /// One task, for `status`.
    Status { task: TaskInfo },
    /// Plain text for `read` and `diff`.
    Text { text: String },
    /// `send` or `key` delivered what it was asked to.
    Done { message: String },
    /// The answer to `wait`: tasks that settled and were not reported yet,
    /// the tasks still starting or working, and whether the timeout passed.
    Waited {
        events: Vec<WaitEvent>,
        still_working: Vec<TaskInfo>,
        timed_out: bool,
    },
    /// The project's Lead memory notes, oldest first.
    Memory { notes: Vec<MemoryNote> },
    /// The request was understood and declined, with a one-line reason.
    Refused { reason: String },
    /// The request failed.
    Error { message: String },
}

impl Reply {
    /// Exit status of the `shika` command: 0 on success, 1 on a refusal.
    /// An `Error` reply is 2, like a usage or connection error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Refused { .. } => 1,
            Self::Error { .. } => 2,
            _ => 0,
        }
    }
}

/// What a worker may run: `report` and a short `help`.
pub const WORKER_HELP: &str = "You are a worker started by a Lead in Shika. The only command you can run is:\n  shika report <text...>   say what you changed, the tests you ran and their result, and your commit.\nRun it when you finish or get blocked. Do not push.";

/// Cleans a report for storage: control characters other than line breaks
/// are dropped, the text is trimmed, and a tab is a space. An empty report or
/// one over [`MAX_REPORT_BYTES`] is refused with the reason.
pub fn clean_report(text: &str) -> std::result::Result<String, String> {
    let cleaned: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter_map(|ch| match ch {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err("The report is empty.".into());
    }
    if cleaned.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "The report is {} bytes; the most is {MAX_REPORT_BYTES}. Shorten it.",
            cleaned.len()
        ));
    }
    Ok(cleaned)
}
