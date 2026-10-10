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

/// Bumped when a request or reply changes shape. The server refuses a request
/// with another version, so a stale `shika` never misreads a newer app.
pub const PROTOCOL_VERSION: u32 = 2;

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

/// The CLI ids `shika new --cli` accepts, the same as the preset ids.
pub const CLI_IDS: [&str; 4] = ["claude", "codex", "cursor", "pi"];

/// Usage text for argument errors.
pub const USAGE: &str = "usage: shika [--json] <command>\n  help\n  tasks\n  new --cli <claude|codex|cursor|pi> [--base <branch>] <prompt...>\n  status <task>\n  wait [<task>...] [--timeout <seconds>]\n  read <task> [--lines <n>]\n  diff <task> [--stat]\n  send <task> [--no-enter] <text...>\n  key <task> <key>...   (enter escape up down left right tab space backspace a-z 0-9)";

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

/// Plain, compact text for the model: one line per task. `--json` output is
/// just `serde_json` of the [`Reply`].
pub fn render_text(reply: &Reply) -> String {
    match reply {
        Reply::Help { text } => text.trim_end().to_string(),
        Reply::Tasks { tasks } if tasks.is_empty() => "No tasks.".to_string(),
        Reply::Tasks { tasks } => tasks.iter().map(task_line).collect::<Vec<_>>().join("\n"),
        Reply::Started { task } => format!("started {}", task_line(task)),
        Reply::Status { task } => task_line(task),
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
                    task_line(&event.task)
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
        "help" | "tasks" | "new" | "status" | "wait" | "read" | "diff" | "send" | "key"
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
        "help" | "tasks" => {
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
            if word == "help" {
                Command::Help
            } else {
                Command::Tasks
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
/// line. Blocking; `wait` blocks here for up to its timeout and `new` until
/// the launch finishes.
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
    // app closes the socket, which ends the read with EOF.
    let patience = match &request.command {
        Command::Wait { timeout_secs, .. } => {
            Some(Duration::from_secs(timeout_secs.saturating_add(30)))
        }
        Command::New { .. } => None,
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

fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
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

/// The per-run directory holding the socket and the `shika` command:
///
/// ```text
/// <tmp>/shika-control-<pid>-<nonce>/   0700
///   sock                               bound by the server
///   bin/shika                          symlink to the running executable
/// ```
///
/// The Lead's `PATH` gets [`ControlDir::bin_dir`] first, so `shika` is always
/// the same build as the app. The directory is removed on drop.
#[derive(Debug)]
pub struct ControlDir {
    root: PathBuf,
}

impl ControlDir {
    /// Creates the directory and the symlink. Fails when the socket path
    /// would not fit in `sockaddr_un` (103 bytes).
    pub fn create() -> Result<Self> {
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
        let exe = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .map_err(setup)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(setup)?;
        // Dropping removes the directory if anything below fails.
        let dir = Self { root };
        fs::set_permissions(&dir.root, fs::Permissions::from_mode(0o700)).map_err(setup)?;
        let bin = dir.bin_dir();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&bin)
            .map_err(setup)?;
        symlink(&exe, bin.join("shika")).map_err(setup)?;
        Ok(dir)
    }

    /// Where the server binds its `UnixListener`.
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("sock")
    }

    /// The directory to put first on the Lead's `PATH`.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
}

impl Drop for ControlDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn args(words: &str) -> Vec<String> {
        words.split_whitespace().map(str::to_string).collect()
    }

    fn task(id: &str, status: TaskStatus) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            title: "Fix login".into(),
            cli: "codex".into(),
            status,
            elapsed_secs: Some(192),
            branch: "fix-login".into(),
            diff_stat: Some(TaskDiffStat {
                files: 2,
                added: 64,
                removed: 3,
            }),
            pr: Some(12),
            started_by_lead: true,
            path: "/repo/.worktrees/fix-login".into(),
        }
    }

    #[test]
    fn requests_and_replies_round_trip_as_json() {
        let commands = [
            Command::Help,
            Command::Tasks,
            Command::New {
                cli: "claude".into(),
                base: Some("dev".into()),
                prompt: "Fix \"quotes\"\nand lines".into(),
            },
            Command::Status { task: "a".into() },
            Command::Wait {
                tasks: vec!["a".into(), "b".into()],
                timeout_secs: 5,
            },
            Command::Read {
                task: "a".into(),
                lines: 40,
            },
            Command::Diff {
                task: "a".into(),
                stat: true,
            },
            Command::Send {
                task: "a".into(),
                text: "yes\n\"ok\"".into(),
                enter: false,
            },
            Command::Key {
                task: "a".into(),
                keys: vec!["down".into(), "enter".into()],
            },
        ];
        for command in commands {
            let request = Request::new("tok", command);
            let json = serde_json::to_string(&request).unwrap();
            assert!(!json.contains('\n'));
            assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        }
        let replies = [
            Reply::Help { text: "hi".into() },
            Reply::Tasks {
                tasks: vec![task("a", TaskStatus::Working)],
            },
            Reply::Started {
                task: task("a", TaskStatus::Starting),
            },
            Reply::Status {
                task: task("a", TaskStatus::Asking),
            },
            Reply::Waited {
                events: vec![WaitEvent {
                    task: task("a", TaskStatus::Ready),
                }],
                still_working: vec![task("b", TaskStatus::Working)],
                timed_out: false,
            },
            Reply::Text {
                text: "screen".into(),
            },
            Reply::Done {
                message: "sent".into(),
            },
            Reply::Refused {
                reason: "no".into(),
            },
            Reply::Error {
                message: "bad".into(),
            },
        ];
        for reply in replies {
            let json = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<Reply>(&json).unwrap(), reply);
        }
        assert_eq!(
            serde_json::to_string(&Request::new("t", Command::Tasks)).unwrap(),
            r#"{"version":2,"token":"t","command":{"type":"tasks"}}"#
        );
    }

    #[test]
    fn a_version_mismatch_is_refused_with_a_reason() {
        let mut request = Request::new("t", Command::Help);
        assert_eq!(request.version_refusal(), None);
        request.version = PROTOCOL_VERSION + 1;
        let Some(Reply::Refused { reason }) = request.version_refusal() else {
            panic!("expected a refusal");
        };
        assert!(reason.contains("protocol"), "{reason}");
    }

    #[test]
    fn text_is_one_compact_line_per_task() {
        let text = render_text(&Reply::Tasks {
            tasks: vec![task("a1", TaskStatus::Ready), {
                let mut idle = task("b2", TaskStatus::Working);
                idle.diff_stat = None;
                idle.elapsed_secs = Some(5);
                idle.pr = None;
                idle.started_by_lead = false;
                idle.branch = String::new();
                idle.path = "/my repo/.worktrees/x".into();
                idle
            }],
        });
        assert_eq!(
            text,
            "a1 ready \"Fix login\" cli=codex branch=fix-login diff=2 files +64 -3 elapsed=3m12s pr=#12 by-lead path=/repo/.worktrees/fix-login\nb2 working \"Fix login\" cli=codex elapsed=5s path=\"/my repo/.worktrees/x\""
        );
        assert_eq!(render_text(&Reply::Tasks { tasks: vec![] }), "No tasks.");
        let waited = render_text(&Reply::Waited {
            events: vec![WaitEvent {
                task: task("a1", TaskStatus::Asking),
            }],
            still_working: vec![task("b2", TaskStatus::Working)],
            timed_out: false,
        });
        assert!(waited.starts_with("asking: a1 asking"), "{waited}");
        assert!(waited.contains("still working:\n  b2 working"), "{waited}");
        let timeout = render_text(&Reply::Waited {
            events: vec![],
            still_working: vec![],
            timed_out: true,
        });
        assert!(timeout.starts_with("timed out"), "{timeout}");
        assert_eq!(
            render_text(&Reply::Refused {
                reason: "full".into()
            }),
            "refused: full"
        );
        assert_eq!(Reply::Refused { reason: "x".into() }.exit_code(), 1);
        assert_eq!(
            Reply::Error {
                message: "x".into()
            }
            .exit_code(),
            2
        );
        assert_eq!(Reply::Help { text: "x".into() }.exit_code(), 0);
    }

    #[test]
    fn arguments_parse_into_commands() {
        let ok = |words: &str| parse_args(&args(words)).unwrap();
        assert_eq!(ok("help"), (Command::Help, false));
        assert_eq!(ok("tasks --json"), (Command::Tasks, true));
        assert_eq!(ok("--json tasks"), (Command::Tasks, true));
        assert_eq!(
            ok("status abc --json"),
            (Command::Status { task: "abc".into() }, true)
        );
        assert_eq!(
            ok("new --cli codex fix the login bug"),
            (
                Command::New {
                    cli: "codex".into(),
                    base: None,
                    prompt: "fix the login bug".into(),
                },
                false
            )
        );
        assert_eq!(
            ok("new --json --base dev --cli pi add tests"),
            (
                Command::New {
                    cli: "pi".into(),
                    base: Some("dev".into()),
                    prompt: "add tests".into(),
                },
                true
            )
        );
        // Words after the prompt starts are the prompt, flags included.
        assert_eq!(
            ok("new --cli claude explain --json output"),
            (
                Command::New {
                    cli: "claude".into(),
                    base: None,
                    prompt: "explain --json output".into(),
                },
                false
            )
        );
        assert_eq!(
            ok("new --cli claude -- --odd"),
            (
                Command::New {
                    cli: "claude".into(),
                    base: None,
                    prompt: "--odd".into(),
                },
                false
            )
        );
        assert_eq!(
            ok("wait"),
            (
                Command::Wait {
                    tasks: vec![],
                    timeout_secs: 100,
                },
                false
            )
        );
        assert_eq!(
            ok("read abc"),
            (
                Command::Read {
                    task: "abc".into(),
                    lines: 0
                },
                false
            )
        );
        assert_eq!(
            ok("read --lines 200 abc --json"),
            (
                Command::Read {
                    task: "abc".into(),
                    lines: 200
                },
                true
            )
        );
        assert_eq!(
            ok("diff abc --stat"),
            (
                Command::Diff {
                    task: "abc".into(),
                    stat: true
                },
                false
            )
        );
        assert_eq!(
            ok("send abc yes please"),
            (
                Command::Send {
                    task: "abc".into(),
                    text: "yes please".into(),
                    enter: true
                },
                false
            )
        );
        // Flags come before the text; after it they are the text.
        assert_eq!(
            ok("send abc --no-enter --json 1 --no-enter"),
            (
                Command::Send {
                    task: "abc".into(),
                    text: "1 --no-enter".into(),
                    enter: false
                },
                true
            )
        );
        assert_eq!(
            ok("send abc -- -1"),
            (
                Command::Send {
                    task: "abc".into(),
                    text: "-1".into(),
                    enter: true
                },
                false
            )
        );
        assert_eq!(
            ok("key abc Down down ENTER y 7 --json"),
            (
                Command::Key {
                    task: "abc".into(),
                    keys: ["down", "down", "enter", "y", "7"]
                        .map(String::from)
                        .to_vec()
                },
                true
            )
        );
        assert_eq!(key_name("Escape").as_deref(), Some("escape"));
        assert_eq!(key_name("A").as_deref(), Some("a"));
        assert_eq!(key_name("ctrl-c"), None);
        assert_eq!(key_name(""), None);
        assert_eq!(
            ok("wait a b --timeout 30 --json"),
            (
                Command::Wait {
                    tasks: vec!["a".into(), "b".into()],
                    timeout_secs: 30,
                },
                true
            )
        );
    }

    #[test]
    fn bad_arguments_return_a_short_usage() {
        for words in [
            "",
            "--json",
            "frobnicate",
            "--json frob 1",
            "--data-dir /x",
            "help extra",
            "tasks extra",
            "status",
            "status a b",
            "status --nope",
            "new fix it",
            "new --cli",
            "new --cli claude",
            "new --cli gemini fix",
            "new --cli claude --bogus fix",
            "wait --timeout",
            "wait --timeout soon",
            "wait --nope",
            "read",
            "read a b",
            "read a --lines",
            "read a --lines many",
            "read a --lines 2001",
            "read a --nope",
            "diff",
            "diff a --nope",
            "diff a b",
            "send",
            "send a",
            "send a --no-enter",
            "send --nope a hi",
            "send a --bogus hi",
            "key",
            "key a",
            "key a f13",
            "key a enter --nope",
            "key a ab",
        ] {
            let err = parse_args(&args(words)).unwrap_err();
            assert!(err.contains("usage: shika"), "{words:?}: {err}");
        }
        let err = parse_args(&args("frob 1")).unwrap_err();
        assert!(err.starts_with("Unknown command frob."), "{err}");
        let too_many = format!("key a {}", ["tab"; MAX_KEYS + 1].join(" "));
        assert!(parse_args(&args(&too_many)).is_err());
    }

    #[test]
    fn command_words_decide_client_mode() {
        for word in [
            "help", "tasks", "new", "status", "wait", "read", "diff", "send", "key",
        ] {
            assert!(is_command(word));
        }
        assert!(!is_command("--json"));
        assert!(!is_command("-psn_0_12345"));
        // Outside a Lead terminal only a command word makes the client.
        assert!(!is_client_invocation(&[], false));
        assert!(!is_client_invocation(&args("--data-dir /x"), false));
        assert!(!is_client_invocation(&args("--diagnostics-file /x"), false));
        assert!(!is_client_invocation(&args("-psn_0_12345"), false));
        assert!(is_client_invocation(&args("tasks"), false));
        assert!(is_client_invocation(&args("--json tasks"), false));
        assert!(!is_client_invocation(&args("--json"), false));
        // Inside one, never the app: bare, mistyped, or an app flag.
        assert!(is_client_invocation(&[], true));
        assert!(is_client_invocation(&args("frob 1"), true));
        assert!(is_client_invocation(&args("--data-dir /x"), true));
        assert!(is_client_invocation(&args("tasks"), true));
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_control_directory_is_private_linked_and_removed() {
        let dir = ControlDir::create().unwrap();
        let root = dir.socket_path().parent().unwrap().to_path_buf();
        assert!(root.is_absolute());
        assert_eq!(root, root.canonicalize().unwrap());
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("shika-control-{}-", std::process::id()))
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(dir.socket_path().as_os_str().len() <= MAX_SOCKET_PATH);
        let link = dir.bin_dir().join("shika");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert!(link.exists());
        let other = ControlDir::create().unwrap();
        assert_ne!(other.socket_path(), dir.socket_path());
        drop(dir);
        assert!(!root.exists());
        assert!(other.bin_dir().exists());
    }

    #[test]
    fn a_socket_round_trip_carries_one_request_and_one_reply() {
        let dir = ControlDir::create().unwrap();
        let listener = UnixListener::bind(dir.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let request = read_request(&stream).unwrap();
            let reply = match request.command {
                Command::Status { task } => Reply::Status {
                    task: task_for(&task),
                },
                _ => Reply::Refused {
                    reason: "unexpected".into(),
                },
            };
            write_reply(&stream, &reply).unwrap();
            request.token
        });
        fn task_for(id: &str) -> TaskInfo {
            TaskInfo {
                id: id.into(),
                title: "t".into(),
                cli: "claude".into(),
                status: TaskStatus::Ready,
                elapsed_secs: None,
                branch: String::new(),
                diff_stat: None,
                pr: None,
                started_by_lead: false,
                path: String::new(),
            }
        }
        let reply = send(
            &dir.socket_path(),
            &Request::new("secret", Command::Status { task: "abc".into() }),
        )
        .unwrap();
        assert_eq!(
            reply,
            Reply::Status {
                task: task_for("abc")
            }
        );
        assert_eq!(server.join().unwrap(), "secret");
    }

    #[test]
    fn a_missing_socket_explains_itself_and_oversized_requests_are_rejected() {
        let err = send(
            Path::new("/nonexistent/shika/sock"),
            &Request::new("t", Command::Help),
        )
        .unwrap_err();
        assert!(matches!(err, ControlError::Connect(_)));
        assert!(err.to_string().contains("Is the app running"), "{err}");

        let mut big = vec![b'a'; MAX_REQUEST_BYTES + 10];
        big.push(b'\n');
        assert_eq!(read_request(&big[..]), Err(ControlError::TooLarge));
        assert!(matches!(
            read_request(&b"not json\n"[..]),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            read_request(&b""[..]),
            Err(ControlError::Protocol(_))
        ));
        // A well-formed line without a trailing newline still parses.
        let line = serde_json::to_vec(&Request::new("t", Command::Tasks)).unwrap();
        assert_eq!(read_request(&line[..]).unwrap().command, Command::Tasks);
    }
}
