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

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::memory::MemoryNote;
use crate::publish::MAX_PR_BODY_CHARS;

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

/// macOS allows 104 bytes in `sockaddr_un.sun_path`, including the NUL.
const MAX_SOCKET_PATH: usize = 103;

/// The longest `shika report` the server keeps, in bytes.
pub const MAX_REPORT_BYTES: usize = 4096;

/// The CLI ids `shika new --cli` accepts, the same as the preset ids.
pub const CLI_IDS: [&str; 4] = ["claude", "codex", "cursor", "pi"];

/// Usage text for argument errors.
pub const USAGE: &str = "usage: shika [--json] <command>\n  help\n  tasks\n  new --cli <claude|codex|cursor|pi> [--base <branch>] <prompt...>\n  status <task>\n  wait [<task>...] [--timeout <seconds>]\n  read <task> [--lines <n>]\n  diff <task> [--stat]\n  send <task> [--no-enter] <text...>\n  key <task> <key>...   (enter escape up down left right tab space backspace a-z 0-9)\n  pr <task> [--title <text>] [--body <text>]\n  close <task>
  memory
  remember <text...>
  forget <n>
  report <text...>   (workers only)";

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
    /// Start a worker. `cli` is a preset id from [`CLI_IDS`].
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

fn duration_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

fn task_line(task: &TaskInfo) -> String {
    let mut line = format!(
        "{} {} {:?} cli={}",
        task.id,
        task.status.as_str(),
        task.title,
        task.cli
    );
    if !task.branch.is_empty() {
        let _ = write!(line, " branch={}", task.branch);
    }
    if let Some(stat) = task.diff_stat {
        let _ = write!(
            line,
            " diff={} files +{} -{}",
            stat.files, stat.added, stat.removed
        );
    }
    if let Some(secs) = task.elapsed_secs {
        let _ = write!(line, " elapsed={}", duration_text(secs));
    }
    if let Some(pr) = task.pr {
        let _ = write!(line, " pr=#{pr}");
    }
    if task.started_by_lead {
        line.push_str(" by-lead");
    }
    if !task.path.is_empty() {
        if task.path.contains(char::is_whitespace) {
            let _ = write!(line, " path={:?}", task.path);
        } else {
            let _ = write!(line, " path={}", task.path);
        }
    }
    line
}

/// The task line, then its report (if any) on indented lines.
fn task_text(task: &TaskInfo) -> String {
    let mut text = task_line(task);
    if let Some(report) = &task.report {
        text.push_str("\n  report:");
        for line in report.lines() {
            text.push_str("\n    ");
            text.push_str(line.trim_end());
        }
    }
    text
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

/// Plain, compact text for the model: one line per task. `--json` output is
/// just `serde_json` of the [`Reply`].
pub fn render_text(reply: &Reply) -> String {
    match reply {
        Reply::Help { text } => text.trim_end().to_string(),
        Reply::Tasks { tasks } if tasks.is_empty() => "No tasks.".to_string(),
        Reply::Tasks { tasks } => tasks.iter().map(task_text).collect::<Vec<_>>().join("\n"),
        Reply::Started { task } => format!("started {}", task_text(task)),
        Reply::Status { task } => task_text(task),
        Reply::Text { text } => text.trim_end().to_string(),
        Reply::Done { message } => message.clone(),
        Reply::Waited {
            events,
            still_working,
            timed_out,
        } => {
            let mut lines = Vec::new();
            if events.is_empty() {
                lines.push(if *timed_out {
                    "timed out: no task needs attention yet".to_string()
                } else {
                    "nothing to wait for".to_string()
                });
            }
            for event in events {
                lines.push(format!(
                    "{}: {}",
                    event.task.status.as_str(),
                    task_text(&event.task)
                ));
            }
            if !still_working.is_empty() {
                lines.push("still working:".to_string());
                lines.extend(
                    still_working
                        .iter()
                        .map(|task| format!("  {}", task_line(task))),
                );
            }
            lines.join("\n")
        }
        Reply::Memory { notes } if notes.is_empty() => "No notes.".to_string(),
        Reply::Memory { notes } => notes
            .iter()
            .enumerate()
            .map(|(index, note)| format!("{}. {}", index + 1, note.text))
            .collect::<Vec<_>>()
            .join("\n"),
        Reply::Refused { reason } => format!("refused: {reason}"),
        Reply::Error { message } => format!("error: {message}"),
    }
}

/// Whether `word` is a command the `shika` client handles, so `main` can run
/// as the client before starting GPUI. [`is_client_invocation`] adds the
/// leading `--json` and the Lead-terminal rule.
pub fn is_command(word: &str) -> bool {
    matches!(
        word,
        "help"
            | "tasks"
            | "new"
            | "status"
            | "wait"
            | "read"
            | "diff"
            | "send"
            | "key"
            | "pr"
            | "close"
            | "memory"
            | "remember"
            | "forget"
            | "report"
    )
}

/// Whether this process runs as the `shika` client instead of the app.
/// `args` are the process arguments without the program name; `lead_env` is
/// whether `SHIKA_SOCKET` or `SHIKA_TOKEN` is set, which only a Lead's
/// terminal has. In a Lead terminal the binary is always the client, so a bare
/// or mistyped `shika` is a usage error and never a second app on the real
/// data. Elsewhere it is the client only for a command word, optionally after
/// a leading `--json`, so `--data-dir`, `--diagnostics-file`, and a Finder
/// launch still start the app.
pub fn is_client_invocation(args: &[String], lead_env: bool) -> bool {
    if lead_env {
        return true;
    }
    let mut args = args.iter();
    match args.next() {
        Some(first) if first == "--json" => args.next().is_some_and(|word| is_command(word)),
        Some(first) => is_command(first),
        None => false,
    }
}

fn usage_error(message: &str) -> String {
    format!("{message}\n{USAGE}")
}

/// Parses the arguments after the program name. Returns the command and
/// whether `--json` was given. `--json` is accepted before the command word
/// and, except inside a `new` prompt, anywhere after it. Errors carry a short
/// usage string.
pub fn parse_args(args: &[String]) -> Result<(Command, bool), String> {
    let mut json = false;
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        if first == "--json" {
            json = true;
            rest = tail;
        } else {
            break;
        }
    }
    let Some((word, rest)) = rest.split_first() else {
        return Err(usage_error("Missing command."));
    };
    let command = match word.as_str() {
        "help" | "tasks" | "memory" => {
            let mut extra = Vec::new();
            for arg in rest {
                if arg == "--json" {
                    json = true;
                } else {
                    extra.push(arg);
                }
            }
            if !extra.is_empty() {
                return Err(usage_error(&format!("{word} takes no arguments.")));
            }
            match word.as_str() {
                "help" => Command::Help,
                "tasks" => Command::Tasks,
                _ => Command::Memory,
            }
        }
        "new" => parse_new(rest, &mut json)?,
        "status" => {
            let mut tasks = Vec::new();
            for arg in rest {
                if arg == "--json" {
                    json = true;
                } else if arg.starts_with('-') {
                    return Err(usage_error(&format!("Unknown option {arg}.")));
                } else {
                    tasks.push(arg.clone());
                }
            }
            match <[String; 1]>::try_from(tasks) {
                Ok([task]) => Command::Status { task },
                Err(_) => return Err(usage_error("status takes exactly one task.")),
            }
        }
        "wait" => {
            let mut tasks = Vec::new();
            let mut timeout_secs = DEFAULT_WAIT_SECS;
            let mut iter = rest.iter();
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--json" => json = true,
                    "--timeout" => {
                        timeout_secs = iter
                            .next()
                            .and_then(|value| value.parse().ok())
                            .ok_or_else(|| usage_error("--timeout needs a number of seconds."))?;
                    }
                    other if other.starts_with('-') => {
                        return Err(usage_error(&format!("Unknown option {other}.")));
                    }
                    other => tasks.push(other.to_string()),
                }
            }
            Command::Wait {
                tasks,
                timeout_secs,
            }
        }
        "read" => parse_read(rest, &mut json)?,
        "diff" => parse_diff(rest, &mut json)?,
        "send" => parse_send(rest, &mut json)?,
        "key" => parse_key(rest, &mut json)?,
        "report" => Command::Report {
            text: parse_text(word, rest, &mut json)?,
        },
        "remember" => Command::Remember {
            text: parse_text(word, rest, &mut json)?,
        },
        "forget" => parse_forget(rest, &mut json)?,
        "pr" => parse_pr(rest, &mut json)?,
        "close" => {
            let mut tasks = Vec::new();
            for arg in rest {
                if arg == "--json" {
                    json = true;
                } else if arg.starts_with('-') {
                    return Err(usage_error(&format!("Unknown option {arg}.")));
                } else {
                    tasks.push(arg.clone());
                }
            }
            match <[String; 1]>::try_from(tasks) {
                Ok([task]) => Command::Close { task },
                Err(_) => return Err(usage_error("close takes exactly one task.")),
            }
        }
        other => return Err(usage_error(&format!("Unknown command {other}."))),
    };
    Ok((command, json))
}

/// `new` flags come first; the first plain word starts the prompt, and
/// everything from there on is the prompt, even words that look like flags.
/// `--` also starts the prompt.
fn parse_new(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut cli = None;
    let mut base = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--cli" | "--base" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| usage_error(&format!("{arg} needs a value.")))?;
                if arg == "--cli" {
                    cli = Some(value.clone());
                } else {
                    base = Some(value.clone());
                }
            }
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let cli = cli.ok_or_else(|| usage_error("new needs --cli."))?;
    if !CLI_IDS.contains(&cli.as_str()) {
        return Err(usage_error(&format!(
            "Unknown CLI {cli}. Use one of {}.",
            CLI_IDS.join(", ")
        )));
    }
    let prompt = args[index.min(args.len())..].join(" ");
    if prompt.trim().is_empty() {
        return Err(usage_error("new needs a prompt."));
    }
    Ok(Command::New { cli, base, prompt })
}

/// `pr <task> [--title <text>] [--body <text>]`: each value is one argument,
/// so the Lead quotes it. The title is one line, as Create PR's field is; the
/// body may span lines. Neither may be blank or given twice.
fn parse_pr(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut title = None;
    let mut body = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => *json = true,
            "--title" | "--body" => {
                let value = iter
                    .next()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| usage_error(&format!("{arg} needs text.")))?;
                let slot = if arg == "--title" {
                    &mut title
                } else {
                    &mut body
                };
                if slot.replace(value.clone()).is_some() {
                    return Err(usage_error(&format!("{arg} is given twice.")));
                }
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    if title
        .as_deref()
        .is_some_and(|title| title.contains(['\n', '\r', '\0']))
    {
        return Err(usage_error("--title must be one line."));
    }
    if body
        .as_deref()
        .is_some_and(|body| body.contains('\0') || body.chars().count() > MAX_PR_BODY_CHARS)
    {
        return Err(usage_error(&format!(
            "--body is text of at most {MAX_PR_BODY_CHARS} characters."
        )));
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Pr { task, title, body }),
        Err(_) => Err(usage_error("pr takes exactly one task.")),
    }
}

fn parse_read(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut lines = 0;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => *json = true,
            "--lines" => {
                lines = iter
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| usage_error("--lines needs a number."))?;
                if lines > MAX_READ_LINES {
                    return Err(usage_error(&format!(
                        "--lines is at most {MAX_READ_LINES}."
                    )));
                }
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Read { task, lines }),
        Err(_) => Err(usage_error("read takes exactly one task.")),
    }
}

fn parse_diff(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut stat = false;
    for arg in args {
        match arg.as_str() {
            "--json" => *json = true,
            "--stat" => stat = true,
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Diff { task, stat }),
        Err(_) => Err(usage_error("diff takes exactly one task.")),
    }
}

/// `send <task> [--no-enter] <text...>`: like `new`, flags come first and the
/// first plain word starts the text. `--` also starts it.
fn parse_send(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut rest = args;
    let mut enter = true;
    let task = loop {
        let Some((first, tail)) = rest.split_first() else {
            return Err(usage_error("send needs a task."));
        };
        rest = tail;
        match first.as_str() {
            "--json" => *json = true,
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => break other.to_string(),
        }
    };
    let mut index = 0;
    while let Some(arg) = rest.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--no-enter" => enter = false,
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let text = rest[index.min(rest.len())..].join(" ");
    if text.trim().is_empty() {
        return Err(usage_error("send needs text."));
    }
    Ok(Command::Send { task, text, enter })
}

/// `report <text...>` and `remember <text...>`: flags come first and the first
/// plain word starts the text. `--` also starts it, for text that begins with
/// a dash.
fn parse_text(word: &str, args: &[String], json: &mut bool) -> Result<String, String> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let text = args[index.min(args.len())..].join(" ");
    if text.trim().is_empty() {
        return Err(usage_error(&format!("{word} needs text.")));
    }
    Ok(text)
}

/// `forget <n>`: one note number, 1 or more.
fn parse_forget(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut words = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else if arg.starts_with('-') {
            return Err(usage_error(&format!("Unknown option {arg}.")));
        } else {
            words.push(arg.as_str());
        }
    }
    match words.as_slice() {
        [word] => match word.parse::<usize>() {
            Ok(note) if note >= 1 => Ok(Command::Forget { note }),
            _ => Err(usage_error("forget needs a note number, 1 or more.")),
        },
        _ => Err(usage_error("forget takes exactly one note number.")),
    }
}

fn parse_key(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut words = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else {
            words.push(arg.as_str());
        }
    }
    let Some((task, keys)) = words.split_first() else {
        return Err(usage_error("key needs a task."));
    };
    if task.starts_with('-') {
        return Err(usage_error(&format!("Unknown option {task}.")));
    }
    if keys.is_empty() {
        return Err(usage_error("key needs at least one key."));
    }
    if keys.len() > MAX_KEYS {
        return Err(usage_error(&format!("key takes at most {MAX_KEYS} keys.")));
    }
    let keys = keys
        .iter()
        .map(|key| key_name(key).ok_or_else(|| usage_error(&format!("Unknown key {key}."))))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Command::Key {
        task: task.to_string(),
        keys,
    })
}

fn io_error(err: &std::io::Error) -> ControlError {
    ControlError::Io(err.to_string())
}

/// Client side: connect to `socket`, send one request line, read one reply
/// line. Blocking; `wait` blocks here for up to its timeout, `new` until the
/// launch finishes, `pr` until publishing ends, and `close` until the author
/// decides.
pub fn send(socket: &Path, request: &Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket).map_err(|err| {
        ControlError::Connect(format!(
            "Could not reach Shika at {} ({err}). Is the app running, and was this Lead started by it?",
            socket.display()
        ))
    })?;
    // `wait` is bounded by its own timeout. `new` is not bounded at all: it
    // replies once the worktree is prepared, which can take as long as the
    // project's `timeout-seconds`, and a client that gave up early would
    // leave the worker running with no one told its id. The server always
    // answers a live app (the launch result, or an error), and a quitting
    // app closes the socket, which ends the read with EOF. `pr` runs a chain
    // of git and gh commands, each bounded at 120 seconds but not the chain,
    // and a client that gave up could miss the URL of a PR that was created.
    // `close` waits for the author at a confirmation dialog. Both are
    // unbounded for the same reason: the server always answers.
    let patience = match &request.command {
        Command::Wait { timeout_secs, .. } => {
            Some(Duration::from_secs(timeout_secs.saturating_add(30)))
        }
        Command::New { .. } | Command::Pr { .. } | Command::Close { .. } => None,
        _ => Some(Duration::from_secs(60)),
    };
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .and_then(|_| stream.set_read_timeout(patience))
        .map_err(|err| io_error(&err))?;
    let mut line =
        serde_json::to_vec(request).map_err(|err| ControlError::Protocol(err.to_string()))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|err| io_error(&err))?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|err| io_error(&err))?;
    if reply.trim().is_empty() {
        return Err(ControlError::Protocol(
            "Shika closed the connection without a reply.".into(),
        ));
    }
    serde_json::from_str(&reply).map_err(|err| ControlError::Protocol(err.to_string()))
}

/// Server side: read the one request line from a connection. A line over
/// [`MAX_REQUEST_BYTES`] is [`ControlError::TooLarge`].
pub fn read_request(stream: impl Read) -> Result<Request> {
    let mut line = Vec::new();
    BufReader::new(stream)
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|err| io_error(&err))?;
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(ControlError::TooLarge);
    }
    if line.is_empty() {
        return Err(ControlError::Protocol("empty request".into()));
    }
    serde_json::from_slice(&line).map_err(|err| ControlError::Protocol(err.to_string()))
}

/// Server side: write the one reply line.
pub fn write_reply(mut stream: impl Write, reply: &Reply) -> Result<()> {
    let mut line =
        serde_json::to_vec(reply).map_err(|err| ControlError::Protocol(err.to_string()))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|err| io_error(&err))
}

pub(crate) fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 128 random bits as 32 hex characters, from `/dev/urandom`. The token is
/// the only credential on the socket, so a failure to read randomness panics
/// rather than returning something guessable.
pub fn new_token() -> String {
    hex(&random_bytes::<16>().expect("could not read /dev/urandom"))
}

/// Where the `shika` command lives: `<data_dir>/bin/shika`, a symlink to the
/// running executable. The path is the same on every launch, because a Lead's
/// recorded conversation names it and a resumed Lead must find it again. Each
/// call replaces the link atomically (a temporary link, then `rename`), so the
/// name never goes missing. On APFS, resolving the link at the instant it is
/// replaced can still fail with EINVAL; the app installs once per launch, so
/// that window only exists while a second Shika starts on the same data
/// directory. The directory is `0700`. Returns `<data_dir>/bin`.
///
/// Two Shika processes sharing one data directory share the link; the one
/// started last wins. Nothing removes it on quit: a dangling or stale link
/// only matters while a process that wants it is running.
pub fn install_command(data_dir: &Path) -> Result<PathBuf, ControlError> {
    let setup = |err: std::io::Error| ControlError::Setup(err.to_string());
    let bin = std::path::absolute(data_dir).map_err(setup)?.join("bin");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&bin)
        .map_err(setup)?;
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(setup)?;
    // A name nobody else is using, so concurrent installs cannot trip over
    // each other's temporary link.
    let nonce = hex(&random_bytes::<4>().map_err(setup)?);
    let temp = bin.join(format!(".shika-{}-{nonce}.tmp", std::process::id()));
    symlink(&exe, &temp).map_err(setup)?;
    if let Err(err) = fs::rename(&temp, bin.join("shika")) {
        let _ = fs::remove_file(&temp);
        return Err(setup(err));
    }
    Ok(bin)
}

/// The per-run directory holding the socket, plus the stable command
/// directory from [`install_command`]:
///
/// ```text
/// <tmp>/shika-control-<pid>-<nonce>/   0700, removed on drop
///   sock                               bound by the server
/// <data_dir>/bin/shika                 symlink to the running executable
/// ```
///
/// The socket path appears only in the environment of processes started by
/// this run, so it may change every launch. The command path is written into
/// conversations, so it may not.
#[derive(Debug)]
pub struct ControlDir {
    root: PathBuf,
    bin_dir: PathBuf,
}

impl ControlDir {
    /// Creates the socket directory and installs the command under
    /// `data_dir`. Fails when the socket path would not fit in `sockaddr_un`
    /// (103 bytes).
    pub fn create(data_dir: &Path) -> Result<Self> {
        let setup = |err: std::io::Error| ControlError::Setup(err.to_string());
        // Bind the real path: on macOS the temp dir is under /private/var.
        let base = std::env::temp_dir().canonicalize().map_err(setup)?;
        let nonce = hex(&random_bytes::<4>().map_err(setup)?);
        let root = base.join(format!("shika-control-{}-{nonce}", std::process::id()));
        let socket = root.join("sock");
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(ControlError::Setup(format!(
                "{} is too long for a Unix socket path.",
                socket.display()
            )));
        }
        let bin_dir = install_command(data_dir)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(setup)?;
        // Dropping removes the directory if anything below fails.
        let dir = Self { root, bin_dir };
        fs::set_permissions(&dir.root, fs::Permissions::from_mode(0o700)).map_err(setup)?;
        Ok(dir)
    }

    /// Where the server binds its `UnixListener`.
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("sock")
    }

    /// The directory holding the `shika` command, for a Lead's or worker's
    /// `PATH` and for the absolute command path in prompts.
    pub fn bin_dir(&self) -> PathBuf {
        self.bin_dir.clone()
    }
}

impl Drop for ControlDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
