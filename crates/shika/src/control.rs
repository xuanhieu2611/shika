//! The app side of the Lead's `shika` command (see `docs/lead-agent.md`).
//!
//! A background thread accepts connections on a Unix socket. Each connection
//! carries one request. The thread hands it to the UI thread over a channel
//! that `Shika::tick` drains, the same way notification clicks arrive, and
//! then blocks on a reply channel, holding no lock. All state lives on the UI
//! thread: the Lead is the card that holds the request's token, so the token
//! dies with the card and needs no table.
//!
//! `wait` is a [`Waiter`] on the Lead's card. The tick resolves it from the
//! cards' current status, so there is no polling thread. The doorbell
//! ([`Doorbell`]) is resolved from the same tick: when workers settle and the
//! Lead is idle and its author is not typing, Shika submits one `[shika]` line
//! into the Lead's terminal. `send` and `key` use the same typing path
//! ([`Shika::type_into`]) with the guard in [`input_refusal`].
use crate::lead_chat::WorkerSummary;
use crate::lead_steps::{self, SharedSteps, StepKind, StepToken};
use crate::{Card, HostState, Launch, Overlay, Selection, Shika, lock, model::Status};
use gpui::{Context, Window};
use shika_core::control::{
    Command, ControlDir, ControlError, MAX_READ_LINES, Reply, TaskDiffStat, TaskInfo, TaskStatus,
    WORKER_HELP, WaitEvent, clean_report, read_request, write_reply,
};
use shika_core::{LaunchOptions, Session, WorkerEnv};
use shika_terminal::Modes;
use shika_terminal::input::{Key, KeyMods, encode_key, encode_paste};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// How long the doorbell waits after the first unrung settlement, so several
/// workers finishing together become one line.
const BELL_BATCH: Duration = Duration::from_millis(1500);

/// The doorbell stays quiet this long after the author last typed in the
/// Lead's terminal.
pub(crate) const BELL_TYPING_QUIET: Duration = Duration::from_secs(3);

/// A restarted Lead's terminal must have been quiet this long before the
/// summary is typed: the CLI has drawn its prompt and can take input.
const SUMMARY_QUIET: Duration = Duration::from_millis(1500);

/// Pause between a pasted line and its Enter. Some TUIs (Codex's paste-burst
/// handling) read an Enter that arrives inside a paste burst as a newline.
const ENTER_DELAY: Duration = Duration::from_millis(150);

/// Pause between the keys of one `shika key`, so a TUI sees them one by one.
const KEY_DELAY: Duration = Duration::from_millis(40);

/// Appended to the prompt of every worker a Lead starts with `shika new`,
/// after a blank line. Not added by `send`.
pub const WORKER_FOOTER: &str = "When you finish or get blocked, run: {command} report \"<what you changed, the tests you ran and their result, your commit>\". Wait for it to print reported. If reporting fails, retry after correcting the error, or include the exact failure in your final response. Do not push.";

/// Shell-quoted absolute executable: a provider's login shell may reset PATH.
/// Never put credentials in a prompt.
pub fn command_path(bin_dir: &Path) -> String {
    format!(
        "'{}'",
        bin_dir
            .join("shika")
            .to_string_lossy()
            .replace('\'', "'\"'\"'")
    )
}

pub fn lead_prompt(project: &str, bin_dir: &Path) -> String {
    format!(
        "You are the Lead agent for the \"{project}\" project in Shika. Run {0} help and {0} memory, follow them, then ask the author what they want done.",
        command_path(bin_dir)
    )
}

pub fn lead_guide(bin_dir: &Path) -> String {
    format!(
        "Shika command for this session: {}. In every shell command below, replace `shika` with this absolute executable path. Do not rely on PATH: a CLI's login shell can reset it.\n\n{LEAD_GUIDE}",
        command_path(bin_dir)
    )
}

/// The prompt a Lead-started worker receives: the Lead's text, then the
/// report instruction. The first line, which names the card and branch, is
/// still the Lead's.
pub fn worker_prompt(prompt: &str, bin_dir: &Path) -> String {
    let footer = WORKER_FOOTER.replace("{command}", &command_path(bin_dir));
    format!("{}\n\n{footer}", prompt.trim_end())
}

/// What `shika help` prints.
const LEAD_GUIDE: &str = include_str!("lead_guide.md");

/// Live workers one Lead may have at a time.
pub const MAX_WORKERS: usize = 4;

/// The longest a single `wait` blocks, whatever the client asked for.
pub const MAX_WAIT_SECS: u64 = 600;

/// Connections served at once. Each is a short-lived thread.
const MAX_CONNECTIONS: usize = 32;

/// One request, handed to the UI thread.
pub struct Incoming {
    pub token: String,
    pub command: Command,
    pub reply: Sender<Reply>,
    /// The connection thread's report on the reply: true once it was written
    /// to the client, false if the write failed. Dropped unsent when the
    /// client left first. Only a `wait` reads it.
    pub delivered: Receiver<bool>,
}

/// The socket, its directory, and the queue of requests for the UI thread.
/// Dropping it stops the accept loop and removes the directory.
pub struct Server {
    dir: ControlDir,
    requests: Receiver<Incoming>,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Binds the socket and installs the stable `shika` command under
    /// `data_dir`.
    pub fn start(data_dir: &Path) -> Result<Self, ControlError> {
        let dir = ControlDir::create(data_dir)?;
        let listener = UnixListener::bind(dir.socket_path())
            .map_err(|err| ControlError::Setup(err.to_string()))?;
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let accept_stop = stop.clone();
        std::thread::Builder::new()
            .name("shika-control".into())
            .spawn(move || accept_loop(listener, sender, accept_stop))
            .map_err(|err| ControlError::Setup(err.to_string()))?;
        Ok(Self {
            dir,
            requests,
            stop,
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.socket_path()
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.dir.bin_dir()
    }

    fn next(&self) -> Option<Incoming> {
        self.requests.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the blocked accept so the thread sees the flag.
        let _ = UnixStream::connect(self.dir.socket_path());
    }
}

fn accept_loop(listener: UnixListener, requests: Sender<Incoming>, stop: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = write_reply(
                &stream,
                &Reply::Error {
                    message: "Shika is serving too many commands at once.".into(),
                },
            );
            continue;
        }
        let requests = requests.clone();
        let active = active.clone();
        let spawned = std::thread::Builder::new()
            .name("shika-control-conn".into())
            .spawn({
                let active = active.clone();
                move || {
                    serve(stream, &requests);
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// True when the client has gone away, so its reply would be lost.
fn peer_closed(stream: &UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let closed = matches!((&*stream).read(&mut [0u8; 1]), Ok(0));
    let _ = stream.set_nonblocking(false);
    closed
}

fn serve(stream: UnixStream, requests: &Sender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut delivered_to = None;
    let reply = match read_request(&stream) {
        Err(err) => Reply::Error {
            message: err.to_string(),
        },
        Ok(request) => match request.version_refusal() {
            Some(refusal) => refusal,
            None => {
                let (reply, answer) = mpsc::channel();
                let (confirm, delivered) = mpsc::channel();
                delivered_to = Some(confirm);
                let incoming = Incoming {
                    token: request.token,
                    command: request.command,
                    reply,
                    delivered,
                };
                if requests.send(incoming).is_err() {
                    Reply::Error {
                        message: "Shika is quitting.".into(),
                    }
                } else {
                    loop {
                        match answer.recv_timeout(Duration::from_millis(500)) {
                            Ok(reply) => break reply,
                            // Dropping `answer` tells the UI thread the
                            // client is gone, so a wait keeps its events.
                            Err(RecvTimeoutError::Timeout) if peer_closed(&stream) => return,
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => {
                                break Reply::Error {
                                    message: "Shika stopped handling this command. The Lead may have been closed."
                                        .into(),
                                };
                            }
                        }
                    }
                }
            }
        },
    };
    let written = write_reply(&stream, &reply).is_ok();
    if let Some(confirm) = delivered_to {
        let _ = confirm.send(written);
    }
}

/// A worker's latest `shika report`, tagged with the turn it was made in. A
/// new turn leaves it behind: the Lead sees only the current turn's report.
pub struct WorkerReport {
    pub text: String,
    pub turn: Option<Instant>,
    /// A changed report is a new event even if Ready was already delivered.
    pub received: Instant,
}

impl WorkerReport {
    fn record(self, current: &mut Option<Self>) {
        // Retrying an identical report is idempotent, including a retry
        // after the worker lost the acknowledgement.
        if !current
            .as_ref()
            .is_some_and(|old| old.turn == self.turn && old.text == self.text)
        {
            *current = Some(self);
        }
    }
}

/// The reply to a worker's command, and the report it stores. A worker may
/// run `report` and `help`; every other command is refused.
pub fn worker_command(command: Command, turn: Option<Instant>) -> (Reply, Option<WorkerReport>) {
    match command {
        Command::Help => (
            Reply::Help {
                text: WORKER_HELP.to_string(),
            },
            None,
        ),
        Command::Report { text } => match clean_report(&text) {
            Ok(text) => (
                Reply::Done {
                    message: "reported".into(),
                },
                Some(WorkerReport {
                    text,
                    turn,
                    received: Instant::now(),
                }),
            ),
            Err(reason) => (refused(reason), None),
        },
        _ => (refused("Workers can only run shika report."), None),
    }
}

/// Internal chat delivery is committed only after the pipe accepted the message.
pub(super) enum ChatDelivery {
    Bell(Vec<(String, Settlement)>),
    Restart(super::RestartSummary),
}

/// Everything the Lead's card keeps for the control socket.
pub struct LeadState {
    pub token: String,
    ledger: Ledger,
    waiters: Vec<Waiter>,
    unconfirmed: Vec<Unconfirmed>,
    doorbell: Doorbell,
    /// The activity steps of the running turn (see `lead_steps`).
    pub steps: SharedSteps,
    /// The step list version last pushed to the chat and the card.
    steps_seen: u64,
}

impl LeadState {
    pub fn new(token: String) -> Self {
        Self {
            token,
            ledger: Ledger::default(),
            waiters: Vec::new(),
            unconfirmed: Vec::new(),
            doorbell: Doorbell::default(),
            steps: SharedSteps::default(),
            steps_seen: 0,
        }
    }
}

/// A task as `wait` sees it: its state and which turn that state belongs to.
pub struct Observed {
    pub info: TaskInfo,
    pub turn: Option<Instant>,
    pub report_received: Option<Instant>,
}

impl Observed {
    fn settlement(&self) -> Settlement {
        (self.info.status, self.turn, self.report_received)
    }
}

/// A settlement already reported to the Lead: status, turn, and current report
/// receipt. A new turn or changed report is a new event, so a report arriving
/// after a delivered Ready event cannot disappear. Identical retries keep the
/// receipt, so they do not repeatedly wake the Lead.
type Settlement = (TaskStatus, Option<Instant>, Option<Instant>);

#[derive(Default)]
pub struct Ledger {
    reported: HashMap<String, Settlement>,
}

impl Ledger {
    fn is_reported(&self, task: &Observed) -> bool {
        self.reported.get(&task.info.id) == Some(&task.settlement())
    }

    fn mark(&mut self, marks: Vec<(String, Settlement)>) {
        self.reported.extend(marks);
    }

    fn forget(&mut self, id: &str) {
        self.reported.remove(id);
    }
}

/// Whether the Lead can take a doorbell line now. Every part must hold.
pub struct Gate {
    /// A `wait` is pending, or its reply is being delivered: it reports the
    /// settlements itself.
    pub waiting: bool,
    /// The Lead card is Ready or Waiting, with no submission unprocessed.
    pub idle: bool,
    /// The author left typed text in the Lead's terminal, unsent.
    pub draft: bool,
    /// When the author last typed in the Lead's terminal.
    pub last_typed: Option<Instant>,
    /// The Lead's PTY is alive and the CLI is running.
    pub alive: bool,
}

impl Gate {
    fn open(&self, now: Instant) -> bool {
        self.alive
            && self.idle
            && !self.waiting
            && !self.draft
            && !self
                .last_typed
                .is_some_and(|at| now.duration_since(at) < BELL_TYPING_QUIET)
    }
}

/// The Lead's [`Gate`] right now.
fn lead_gate(
    card: &Card,
    state: &LeadState,
    chat: Option<&super::lead_chat::LeadChat>,
    cx: &gpui::App,
) -> Gate {
    let host = lock(&card.agent.state);
    Gate {
        waiting: !state.waiters.is_empty() || !state.unconfirmed.is_empty(),
        idle: !card.creating
            && matches!(card.status, Status::Waiting | Status::Ready)
            && host.submission == card.submitted
            && chat.is_none_or(|chat| !chat.saving && !chat.busy && !chat.save_failed),
        draft: chat.map_or_else(|| host.has_draft(), |chat| chat.has_draft(cx)),
        last_typed: chat.map_or(host.last_typed, |chat| chat.last_typed),
        alive: !host.exited && !host.preparing && chat.map_or(host.pty.is_some(), |chat| chat.live),
    }
}

/// Decides when Shika wakes an idle Lead. Remembers which settlements it has
/// rung, so none rings twice.
#[derive(Default)]
pub struct Doorbell {
    rung: Ledger,
    /// When the oldest unrung, unreported settlement was first seen.
    since: Option<Instant>,
}

impl Doorbell {
    /// The settlements to announce now. A settlement is due when it is
    /// settled, was not reported by a `wait`, and was not rung before; it
    /// rings once it has aged [`BELL_BATCH`] and the gate is open. Until
    /// then, none.
    pub fn due<'a>(
        &mut self,
        reported: &Ledger,
        tasks: &'a [Observed],
        gate: &Gate,
        now: Instant,
    ) -> Vec<&'a Observed> {
        let fresh: Vec<&Observed> = tasks
            .iter()
            .filter(|task| {
                is_settled(task.info.status)
                    && !reported.is_reported(task)
                    && !self.rung.is_reported(task)
            })
            .collect();
        if fresh.is_empty() {
            self.since = None;
            return fresh;
        }
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) < BELL_BATCH || !gate.open(now) {
            return Vec::new();
        }
        fresh
    }

    /// The caller is ringing for these: never again for the same
    /// (task, status, turn, report).
    pub fn rang(&mut self, tasks: &[&Observed]) {
        self.rung.mark(
            tasks
                .iter()
                .map(|task| (task.info.id.clone(), task.settlement()))
                .collect(),
        );
        self.since = None;
    }

    /// The line did not reach the Lead; these may ring again.
    pub fn forget(&mut self, ids: &[String]) {
        for id in ids {
            self.rung.forget(id);
        }
    }
}

/// The one-line message typed into an idle Lead. Short: titles are cut.
pub fn doorbell_line(tasks: &[&TaskInfo]) -> String {
    let parts: Vec<String> = tasks
        .iter()
        .map(|task| {
            let title: String = task.title.chars().filter(|c| !c.is_control()).collect();
            let title = if title.chars().count() > 28 {
                format!(
                    "{}...",
                    title.chars().take(25).collect::<String>().trim_end()
                )
            } else {
                title
            };
            let what = match task.status {
                TaskStatus::Exited => "has exited",
                TaskStatus::Asking => "is asking",
                _ => "is ready",
            };
            let reported = if task.report.is_some() {
                " (reported)"
            } else {
                ""
            };
            format!(
                "{} \"{}\" {what}{reported}",
                task.id,
                title.replace('"', "'")
            )
        })
        .collect();
    format!(
        "[shika] Workers changed: {}. Run shika wait.",
        parts.join("; ")
    )
}

/// What the Lead needs to know to type into a worker.
pub struct InputState {
    pub status: TaskStatus,
    /// The author typed into that terminal and has not submitted it.
    pub draft: bool,
    /// A `shika pr` is publishing it ([`Card::lead_publishing`]).
    pub publishing: bool,
}

/// Why the Lead may not type into `task` now. `escape_only` is a `key` of
/// nothing but escape, which may interrupt a Working task.
pub fn input_refusal(task: &str, state: &InputState, escape_only: bool) -> Option<String> {
    match state.status {
        TaskStatus::Exited => {
            return Some(format!(
                "{task} has exited, so there is nothing to type into."
            ));
        }
        TaskStatus::Starting => return Some(format!("{task} is still starting. Try again.")),
        _ if state.publishing => return Some(LEAD_PUBLISHING.into()),
        TaskStatus::Working if !escape_only => {
            return Some(format!(
                "{task} is working. Wait for it, or interrupt it with: shika key {task} escape"
            ));
        }
        _ => {}
    }
    state.draft.then(|| {
        format!(
            "The author has typed into {task}'s terminal and not sent it. Do not add to it; tell the author."
        )
    })
}

/// One write to a terminal. `count` sends it through the typed-input capture,
/// so a pasted line and its Enter are a submission like a typed one.
pub struct Step {
    pub bytes: Vec<u8>,
    pub count: bool,
    /// Wait this long before the write.
    pub delay: Duration,
}

/// `text` as a paste (bracketed when the program asked for it), then Enter as
/// a separate, delayed write. Control characters are dropped. Without
/// bracketed paste, line breaks become spaces so none submits early.
pub fn paste_steps(text: &str, enter: bool, modes: &Modes) -> Vec<Step> {
    let clean: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let bytes = if modes.bracketed_paste {
        encode_paste(&clean, true)
    } else {
        encode_paste(&clean.replace('\n', " "), false)
    };
    let mut steps = vec![Step {
        bytes,
        count: enter,
        delay: Duration::ZERO,
    }];
    if enter {
        steps.push(Step {
            bytes: b"\r".to_vec(),
            count: true,
            delay: ENTER_DELAY,
        });
    }
    steps
}

/// The bytes the terminal sends for a key name from `shika key`, for the
/// program's current mode (application cursor keys).
pub fn key_bytes(name: &str, modes: &Modes) -> Option<Vec<u8>> {
    let key = match name {
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "tab" => Key::Tab,
        "space" => Key::Char(' '),
        "backspace" => Key::Backspace,
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_lowercase() || c.is_ascii_digit() => Key::Char(c),
                _ => return None,
            }
        }
    };
    encode_key(key, KeyMods::NONE, modes)
}

/// One write per key, in order.
pub fn key_steps(keys: &[String], modes: &Modes) -> Result<Vec<Step>, String> {
    keys.iter()
        .enumerate()
        .map(|(index, name)| {
            key_bytes(name, modes)
                .map(|bytes| Step {
                    bytes,
                    count: false,
                    delay: if index == 0 {
                        Duration::ZERO
                    } else {
                        KEY_DELAY
                    },
                })
                .ok_or_else(|| format!("Unknown key {name}."))
        })
        .collect()
}

/// Screen rows as the text `read` prints: no trailing blank lines.
pub fn screen_text(mut lines: Vec<String>) -> String {
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return "(the terminal is empty)".to_string();
    }
    lines
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_settled(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Ready | TaskStatus::Asking | TaskStatus::Exited
    )
}

fn is_busy(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Starting | TaskStatus::Working)
}

/// A reply for a `wait`, and the settlements it reports. The caller marks
/// them in the ledger only once the connection thread confirms the reply was
/// written (see [`Unconfirmed`]).
pub struct Resolved {
    reply: Reply,
    marks: Vec<(String, Settlement)>,
}

/// Decides a `wait` over `tasks` (already the right set):
/// - some task settled and was not reported: reply with all of those;
/// - nothing is starting or working: reply at once with nothing to wait for;
/// - the deadline passed: reply that it timed out, naming what still works;
/// - otherwise keep waiting (None).
pub fn resolve(
    ledger: &Ledger,
    tasks: &[Observed],
    now: Instant,
    deadline: Instant,
) -> Option<Resolved> {
    let still_working: Vec<TaskInfo> = tasks
        .iter()
        .filter(|task| is_busy(task.info.status))
        .map(|task| task.info.clone())
        .collect();
    let fresh: Vec<&Observed> = tasks
        .iter()
        .filter(|task| is_settled(task.info.status) && !ledger.is_reported(task))
        .collect();
    let (events, timed_out) = if !fresh.is_empty() {
        (fresh, false)
    } else if still_working.is_empty() {
        (Vec::new(), false)
    } else if now >= deadline {
        (Vec::new(), true)
    } else {
        return None;
    };
    Some(Resolved {
        marks: events
            .iter()
            .map(|task| (task.info.id.clone(), task.settlement()))
            .collect(),
        reply: Reply::Waited {
            events: events
                .iter()
                .map(|task| WaitEvent {
                    task: task.info.clone(),
                })
                .collect(),
            still_working,
            timed_out,
        },
    })
}

/// The settlements of a `wait` reply that was handed to its connection thread
/// but not yet confirmed written. They enter the ledger only on confirmation,
/// so a client that disconnects in that window loses nothing. The price is a
/// possible duplicate: a second `wait` that resolves before the confirmation
/// lands, or a client that read the reply but died before acting on it,
/// reports the same event again. A duplicate is harmless; a lost event is not.
struct Unconfirmed {
    delivered: Receiver<bool>,
    marks: Vec<(String, Settlement)>,
}

/// Applies the confirmations that arrived: a written reply marks its
/// settlements reported, a failed or abandoned one drops them. Replies still
/// in flight stay pending.
fn apply_confirmations(ledger: &mut Ledger, pending: &mut Vec<Unconfirmed>) {
    pending.retain_mut(|entry| match entry.delivered.try_recv() {
        Ok(written) => {
            if written {
                ledger.mark(std::mem::take(&mut entry.marks));
            }
            false
        }
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => false,
    });
}

/// What `send` or `key` types.
enum Input {
    Paste { text: String, enter: bool },
    Keys(Vec<String>),
}

/// What runs on the app thread when a typing finishes.
type Done = Box<dyn FnOnce(&mut Shika, Result<(), String>)>;

/// One write of a typing, checked and counted. A later step is refused if the
/// author typed since the first.
fn write_step(
    core: &shika_core::Core,
    state: &std::sync::Mutex<HostState>,
    step: &Step,
    stamp: Option<Instant>,
) -> Result<(), String> {
    lock(state).inject(core, step, stamp)
}

/// A `wait` that has not been answered yet.
struct Waiter {
    /// The tasks named on the command line; empty means every worker the
    /// Lead started.
    named: Vec<String>,
    deadline: Instant,
    reply: Responder,
    delivered: Receiver<bool>,
}

impl Waiter {
    /// The connection thread drops this channel when the client leaves.
    /// An abandoned wait must not keep the doorbell gate closed.
    fn connected(&self) -> bool {
        matches!(self.delivered.try_recv(), Err(TryRecvError::Empty))
    }
}

/// The way back to a Lead's command: the client's channel, and the activity
/// step the command runs as. Sending the reply settles the step; dropping it
/// unanswered (the client left, the Lead closed) removes the step.
pub struct Responder {
    tx: Sender<Reply>,
    step: Option<StepToken>,
}

impl Responder {
    fn new(tx: Sender<Reply>, step: Option<StepToken>) -> Self {
        Self { tx, step }
    }

    /// Whether the client was still there to receive it.
    pub fn send(&self, reply: Reply) -> bool {
        if let Some(step) = &self.step {
            step.settle(&reply);
        }
        self.tx.send(reply).is_ok()
    }
}

/// Sends the answer to a `shika new`, if one is waiting for it.
pub fn answer(reply: &Option<Responder>, answer: Reply) {
    if let Some(reply) = reply {
        let _ = reply.send(answer);
    }
}

fn refused(reason: impl Into<String>) -> Reply {
    Reply::Refused {
        reason: reason.into(),
    }
}

/// How a failed launch reads to the Lead: a refusal when the request itself
/// was wrong, an error otherwise.
pub fn failure_reply(error: &shika_core::Error) -> Reply {
    use shika_core::Error;
    match error {
        Error::InvalidPrompt(_)
        | Error::MemoryRefused(_)
        | Error::NoSuchBranch(_)
        | Error::BaseBranchMissing(_)
        | Error::CliNotFound(_)
        | Error::PreparationNeedsApproval => refused(error.to_string()),
        _ => Reply::Error {
            message: error.to_string(),
        },
    }
}

/// The one `close` waiting on the author. Shika holds at most one, because
/// the flow may open a dialog and only one overlay exists at a time. It
/// outlives the dialog just long enough to answer the client and give the
/// selection and focus back to the Lead. `pr` has none: it publishes without
/// a dialog ([`Shika::control_pr`]).
pub struct LeadDialog {
    /// Session id of the Lead that asked.
    pub(super) lead_id: String,
    /// Session id of the worker the dialog is about.
    task: String,
    reply: Responder,
    /// Set when the flow succeeded: what Close did.
    outcome: Option<String>,
    /// The last failure the flow reported. The dialog stays open for a retry,
    /// so it is only sent if the author then gives up.
    error: Option<String>,
    /// Set while the dialog waits for the author to stop typing: when the
    /// request came in. Nothing is selected or opened until then.
    deferred: Option<Instant>,
}

/// The longest a Lead's dialog waits for the author to stop typing, or for
/// another dialog to close, before the request is refused.
const DEFER_LIMIT: Duration = Duration::from_secs(120);

/// Why a Lead's dialog cannot open right now, from the state of Shika alone.
/// `open` names an overlay already on screen. None when it may go ahead.
pub fn dialog_gate_refusal(pending: bool, busy: bool, open: Option<&str>) -> Option<String> {
    if pending {
        return Some(
            "Another dialog from this Lead is already waiting for the author. Try again after they answer it."
                .into(),
        );
    }
    if let Some(open) = open {
        return Some(format!(
            "Shika has {open} open. The author has to finish or dismiss it first; try again later."
        ));
    }
    busy.then(|| "Shika is busy with another operation. Try again in a moment.".into())
}

/// Why a Lead's `pr` cannot publish `task` right now, from the state of
/// Shika alone. `open` names an overlay the author has open about this same
/// task (its Create PR, Close, or Switch agent dialog). Other overlays do not
/// matter: `pr` opens none.
pub fn pr_gate_refusal(task: &str, busy: bool, open: Option<&str>) -> Option<String> {
    if let Some(open) = open {
        return Some(format!(
            "The author has {open} open for {task}. Try again after they finish it."
        ));
    }
    busy.then(|| "Shika is busy with another operation. Try again in a moment.".into())
}

/// The refusal for anything that would act on a worker while a Lead's `pr`
/// publishes it: the author's Create PR, Close, and Switch agent, and the
/// Lead's `pr`, `close`, `send`, and `key`.
pub const LEAD_PUBLISHING: &str = "The Lead is publishing this task. Try again when it finishes.";

/// `pr` needs a task that has finished a turn: the same condition as Create PR
/// ([`crate::Shika::publish_blocker`]) plus the states a Lead can see.
pub fn pr_status_refusal(task: &str, status: TaskStatus) -> Option<String> {
    match status {
        TaskStatus::Working | TaskStatus::Starting => Some(format!(
            "{task} is still working. Run shika wait, then try again."
        )),
        TaskStatus::Asking => Some(format!(
            "{task} is waiting on a question or permission. Answer it first."
        )),
        TaskStatus::Exited => Some(format!("{task} has exited, so it cannot be published.")),
        TaskStatus::Waiting | TaskStatus::Ready => None,
    }
}

/// What a finished `close` reports for the close action the author chose
/// (the numbers `finish_close` takes).
pub fn close_outcome(action: u8) -> &'static str {
    match action {
        1 => "closed (the author discarded the worktree and branch)",
        2 => "closed (the author pushed the branch first)",
        3 => "closed (the author used branch-switch recovery; both local branches were kept)",
        _ => "closed",
    }
}

/// A publish failure as the Lead reads it.
pub fn publish_failure(error: &str) -> String {
    format!("{error} Completed commits and pushes were kept.")
}

/// What a finished `pr` answers: the URL, a refusal for what only the author
/// can resolve before publishing (no PR target, the worktree on another
/// branch), or the error with what was kept.
pub fn pr_reply(result: Result<String, shika_core::Error>) -> Reply {
    use shika_core::Error;
    match result {
        Ok(url) => Reply::Done { message: url },
        Err(error @ (Error::PublishNoTarget(_) | Error::TaskBranchChanged { .. })) => {
            refused(error.to_string())
        }
        Err(Error::UnknownSession) => Reply::Error {
            message: "The task is no longer in Shika.".into(),
        },
        Err(error) => Reply::Error {
            message: publish_failure(&error.to_string()),
        },
    }
}

/// The reply for a dialog that ended. Success wins, then the last failure the
/// flow reported, then the reasons the dialog could not finish, and last the
/// author cancelling it.
pub fn dialog_reply(
    outcome: Option<String>,
    error: Option<String>,
    lead_open: bool,
    task_open: bool,
) -> Reply {
    if let Some(message) = outcome {
        Reply::Done { message }
    } else if let Some(message) = error {
        Reply::Error { message }
    } else if !lead_open {
        Reply::Error {
            message: "The Lead was closed before the author answered.".into(),
        }
    } else if !task_open {
        Reply::Error {
            message: "The task is no longer in Shika.".into(),
        }
    } else {
        refused(crate::lead_steps::CANCELLED)
    }
}

impl Card {
    /// The state the Lead sees. A prompt that was just handed to the CLI
    /// counts as Working before the tick consumes it, so a `wait` right after
    /// `new` does not find nothing to wait for.
    fn control_status(&self) -> TaskStatus {
        if self.session.is_none() {
            return if self.launch_error.is_some() {
                TaskStatus::Exited
            } else {
                TaskStatus::Starting
            };
        }
        // Switch agent stops the old CLI; that exit is not the task's.
        if self.next_agent.is_some() {
            return TaskStatus::Starting;
        }
        let host = lock(&self.agent.state);
        if host.exited {
            TaskStatus::Exited
        } else if host.submission != self.submitted {
            TaskStatus::Working
        } else {
            match self.status {
                Status::Waiting => TaskStatus::Waiting,
                Status::Working => TaskStatus::Working,
                Status::Asking => TaskStatus::Asking,
                Status::Ready => TaskStatus::Ready,
            }
        }
    }

    fn control_turn(&self) -> Option<Instant> {
        lock(&self.agent.state).report_turn(&self.activity, self.submitted)
    }

    fn current_report(&self) -> Option<&WorkerReport> {
        self.report
            .as_ref()
            .filter(|report| report.turn == self.control_turn())
    }

    /// None while the card has no session: it has no id the Lead could name.
    fn task_info(&self, lead_id: &str) -> Option<TaskInfo> {
        let session = self.session.as_ref()?;
        let status = self.control_status();
        Some(TaskInfo {
            id: session.id.clone(),
            title: self.title.clone(),
            cli: self.launch_preset.clone(),
            status,
            elapsed_secs: (status == TaskStatus::Working).then(|| {
                self.activity
                    .turn_started
                    .unwrap_or(self.since)
                    .elapsed()
                    .as_secs()
            }),
            branch: session.branch.clone(),
            diff_stat: self.diff.map(|stat| TaskDiffStat {
                files: stat.files as u64,
                added: stat.insertions as u64,
                removed: stat.deletions as u64,
            }),
            pr: self.pr.as_ref().map(|watch| watch.number),
            started_by_lead: self.started_by.as_deref() == Some(lead_id),
            path: session.worktree.display().to_string(),
            report: self.current_report().map(|report| report.text.clone()),
        })
    }

    fn observed(&self, lead_id: &str) -> Option<Observed> {
        Some(Observed {
            info: self.task_info(lead_id)?,
            turn: self.control_turn(),
            report_received: self.current_report().map(|report| report.received),
        })
    }

    /// A worker that still occupies one of the Lead's slots.
    fn is_live_worker_of(&self, lead_id: &str) -> bool {
        self.lead.is_none()
            && self.started_by.as_deref() == Some(lead_id)
            && !self.discard
            && self.launch_error.is_none()
            && !lock(&self.agent.state).exited
    }
}

impl Shika {
    /// Called every tick: answers queued commands, then pending waits.
    pub(crate) fn drain_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(incoming) = self.control.as_ref().and_then(Server::next) {
            self.handle_control(incoming, window, cx);
        }
        self.resume_deferred_close(window, cx);
        self.settle_lead_dialog(window, cx);
        let now = Instant::now();
        self.resolve_waiters(now);
        self.ring_doorbells(now, window, cx);
        self.deliver_restart_summaries(now, window, cx);
    }

    /// The Lead card for `project`, whether or not it is running yet.
    pub(crate) fn lead_card(&self, project: &str) -> Option<usize> {
        self.cards
            .iter()
            .position(|card| card.lead.is_some() && card.project == project)
    }

    fn handle_control(&mut self, incoming: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        let Incoming {
            token,
            command,
            reply,
            delivered,
        } = incoming;
        if self
            .cards
            .iter()
            .any(|card| card.worker_token.as_deref() == Some(token.as_str()))
        {
            let _ = reply.send(self.handle_worker(&token, command));
            return;
        }
        let Some(lead) = self
            .cards
            .iter()
            .position(|card| card.lead.as_ref().is_some_and(|lead| lead.token == token))
        else {
            let _ = reply.send(refused(
                "This Lead is not running in Shika (unknown or expired token). Start a Lead from Shika.",
            ));
            return;
        };
        let Some(lead_id) = self.cards[lead].session.as_ref().map(|s| s.id.clone()) else {
            let _ = reply.send(refused("The Lead is still starting."));
            return;
        };
        let project = self.cards[lead].project.clone();
        // Each command but help and report runs as an activity step, settled
        // by the reply wherever it is sent.
        let task = lead_steps::command_task(&command).map(|task| match &command {
            Command::New { prompt, .. } => lead_steps::prompt_name(prompt),
            _ => self
                .project_card(&project, task)
                .map_or_else(|| task.to_string(), |card| card.title.clone()),
        });
        let step = StepKind::of(&command)
            .zip(self.cards[lead].lead.as_ref())
            .map(|(kind, state)| {
                StepToken::start(&state.steps, kind, task.as_deref().unwrap_or(""))
            });
        let reply = Responder::new(reply, step);
        let answer = match command {
            Command::Help => Reply::Help {
                text: self
                    .control
                    .as_ref()
                    .map(|server| lead_guide(&server.bin_dir()))
                    .unwrap_or_else(|| LEAD_GUIDE.to_string()),
            },
            Command::Report { .. } => refused("Only workers can run shika report."),
            Command::Memory => match self.core.lead_memory(&project) {
                Ok(notes) => Reply::Memory { notes },
                Err(err) => failure_reply(&err),
            },
            Command::Remember { text } => match self.core.remember(&project, &text) {
                Ok(number) => Reply::Done {
                    message: format!("remembered {number}"),
                },
                Err(err) => failure_reply(&err),
            },
            Command::Forget { note } => match self.core.forget_note(&project, note) {
                Ok(()) => Reply::Done {
                    message: format!("forgot {note}"),
                },
                Err(err) => failure_reply(&err),
            },
            Command::Tasks => Reply::Tasks {
                tasks: self
                    .project_tasks(&project)
                    .filter_map(|card| card.task_info(&lead_id))
                    .collect(),
            },
            Command::Status { task } => match self
                .project_tasks(&project)
                .find(|card| card.session.as_ref().is_some_and(|s| s.id == task))
                .and_then(|card| card.task_info(&lead_id))
            {
                Some(task) => Reply::Status { task },
                None => refused(format!("No task {task} in this project.")),
            },
            Command::New { cli, base, prompt } => {
                self.control_new(lead, cli, base, prompt, reply, window, cx);
                return;
            }
            Command::Read { task, lines } => match self.project_card(&project, &task) {
                Some(card) => Reply::Text {
                    text: screen_text(
                        card.agent
                            .terminal
                            .text_with_history(lines.min(MAX_READ_LINES)),
                    ),
                },
                None => refused(format!("No task {task} in this project.")),
            },
            Command::Diff { task, stat } => {
                self.control_diff(&project, task, stat, reply, cx);
                return;
            }
            Command::Send { task, text, enter } => {
                let message = if enter {
                    format!(
                        "sent {} characters and Enter to {task}.",
                        text.chars().count()
                    )
                } else {
                    format!(
                        "typed {} characters into {task}, no Enter.",
                        text.chars().count()
                    )
                };
                self.control_input(
                    &project,
                    &lead_id,
                    task,
                    Input::Paste { text, enter },
                    message,
                    reply,
                    window,
                    cx,
                );
                return;
            }
            Command::Key { task, keys } => {
                let message = format!("pressed {} in {task}.", keys.join(" "));
                self.control_input(
                    &project,
                    &lead_id,
                    task,
                    Input::Keys(keys),
                    message,
                    reply,
                    window,
                    cx,
                );
                return;
            }
            Command::Pr { task, title, body } => {
                self.control_pr(&lead_id, &project, task, title, body, reply, cx);
                return;
            }
            Command::Close { task } => {
                self.control_close(&lead_id, &project, task, reply, window, cx);
                return;
            }
            Command::Wait {
                tasks,
                timeout_secs,
            } => {
                if let Some(refusal) = self.wait_refusal(&project, &lead_id, &tasks) {
                    refusal
                } else {
                    let deadline =
                        Instant::now() + Duration::from_secs(timeout_secs.min(MAX_WAIT_SECS));
                    if let Some(state) = self.cards[lead].lead.as_mut() {
                        state.waiters.push(Waiter {
                            named: tasks,
                            deadline,
                            reply,
                            delivered,
                        });
                    }
                    // Answer a wait that is already decided without waiting
                    // for the next tick.
                    self.resolve_waiters(Instant::now());
                    return;
                }
            }
        };
        let _ = reply.send(answer);
    }

    /// A command from a worker's token: `report` and `help`, nothing else.
    /// The token names the worker, so a worker can only ever report for
    /// itself.
    fn handle_worker(&mut self, token: &str, command: Command) -> Reply {
        let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.worker_token.as_deref() == Some(token))
        else {
            return refused("This worker is no longer in Shika.");
        };
        let (reply, report) = worker_command(command, card.control_turn());
        if let Some(report) = report {
            report.record(&mut card.report);
        }
        reply
    }

    /// `shika pr`: publish a worker without a dialog, as Create PR does when
    /// the author confirms it untouched, and answer with the URL. The
    /// author's selection, focus, and overlays are left alone, and the global
    /// `busy` is not set, so they keep working meanwhile; the card is marked
    /// [`Card::lead_publishing`] instead, which refuses what could race with
    /// it on that task. The `#N` checks mark appears as after any publish.
    #[allow(clippy::too_many_arguments)]
    fn control_pr(
        &mut self,
        lead_id: &str,
        project: &str,
        task: String,
        title: Option<String>,
        body: Option<String>,
        reply: Responder,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.pr_refusal(lead_id, project, &task) {
            let _ = reply.send(refused(reason));
            return;
        }
        let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == task))
        else {
            let _ = reply.send(refused(format!("No task {task} in this project.")));
            return;
        };
        card.lead_publishing = true;
        cx.notify();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = task.clone();
            let result = cx
                .background_executor()
                .spawn(
                    async move { core.session_publish_now(&id, title.as_deref(), body.as_deref()) },
                )
                .await;
            // If the app is gone the reply drops, and the connection thread
            // tells the client.
            let _ = this.update(cx, |this, cx| {
                let card = this
                    .cards
                    .iter_mut()
                    .find(|card| card.session.as_ref().is_some_and(|s| s.id == task));
                if let Some(card) = card {
                    card.lead_publishing = false;
                    if let Ok(published) = &result {
                        card.pr = crate::checks::PrWatch::new(published, Instant::now());
                    }
                }
                let _ = reply.send(pr_reply(result.map(|published| published.url)));
                cx.notify();
            });
        })
        .detach();
    }

    /// The refusal for a `pr`, or None when it may publish.
    fn pr_refusal(&self, lead_id: &str, project: &str, task: &str) -> Option<String> {
        let open = self.overlay.as_ref().filter(|overlay| {
            let card_is = |index: &usize| {
                self.cards
                    .get(*index)
                    .and_then(|card| card.session.as_ref())
                    .is_some_and(|s| s.id == task)
            };
            match overlay {
                Overlay::Publish { preview, .. } => preview.session_id == task,
                Overlay::SwitchAgent { session_id, .. } => session_id == task,
                Overlay::Close { index, .. } | Overlay::SwitchedClose { index, .. } => {
                    card_is(index)
                }
                _ => false,
            }
        });
        if let Some(reason) = pr_gate_refusal(task, self.busy, open.map(Overlay::name)) {
            return Some(reason);
        }
        if let Some(reason) = self.worker_refusal(lead_id, project, task, "publish") {
            return Some(reason);
        }
        let card = self.project_card(project, task)?;
        pr_status_refusal(task, card.control_status()).or_else(|| {
            let index = self.cards.iter().position(|c| std::ptr::eq(c, card))?;
            self.publish_blocker(index).map(str::to_string)
        })
    }

    /// `shika close`: select the worker's card and run the ordinary Close
    /// flow, which asks the author when Close would. The reply is sent by
    /// [`Self::settle_lead_dialog`] when the flow ends.
    fn control_close(
        &mut self,
        lead_id: &str,
        project: &str,
        task: String,
        reply: Responder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refusal = dialog_gate_refusal(
            self.lead_dialog.is_some(),
            self.busy,
            self.overlay.as_ref().map(Overlay::name),
        )
        .or_else(|| self.worker_refusal(lead_id, project, &task, "close"));
        if let Some(reason) = refusal {
            let _ = reply.send(refused(reason));
            return;
        }
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.session.as_ref().is_some_and(|s| s.id == task))
        else {
            let _ = reply.send(refused(format!("No task {task} in this project.")));
            return;
        };
        let now = Instant::now();
        let quiet = self.author_quiet(now, cx);
        self.lead_dialog = Some(LeadDialog {
            lead_id: lead_id.to_string(),
            task,
            reply,
            outcome: None,
            error: None,
            // The author is typing: keystrokes meant for a prompt must not
            // land in a Close dialog. [`Self::resume_deferred_close`] opens
            // it once they stop.
            deferred: (!quiet).then_some(now),
        });
        if !quiet {
            cx.notify();
            return;
        }
        self.start_lead_close(index, window, cx);
    }

    /// Opens the Close flow a Lead asked for, once the author is not typing.
    /// Refuses the request when the worker went away or changed, another
    /// dialog stays open too long, or the author keeps typing.
    fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.lead_dialog else {
            return;
        };
        let Some(since) = dialog.deferred else {
            return;
        };
        let now = Instant::now();
        let refusal = if now.duration_since(since) >= DEFER_LIMIT {
            Some("The author kept typing or had another dialog open. Try again later.".to_string())
        } else {
            let project = self
                .cards
                .iter()
                .find(|card| {
                    card.session
                        .as_ref()
                        .is_some_and(|s| s.id == dialog.lead_id)
                })
                .map(|card| card.project.clone());
            match project {
                None => Some("The Lead is gone.".to_string()),
                Some(project) => {
                    self.worker_refusal(&dialog.lead_id, &project, &dialog.task, "close")
                }
            }
        };
        if let Some(reason) = refusal {
            if let Some(dialog) = self.lead_dialog.take() {
                let _ = dialog.reply.send(refused(reason));
            }
            cx.notify();
            return;
        }
        if self.busy || self.overlay.is_some() || !self.author_quiet(now, cx) {
            return;
        }
        let task = dialog.task.clone();
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.session.as_ref().is_some_and(|s| s.id == task))
        else {
            if let Some(dialog) = self.lead_dialog.take() {
                let _ = dialog
                    .reply
                    .send(refused(format!("No task {task} in this project.")));
            }
            return;
        };
        if let Some(dialog) = &mut self.lead_dialog {
            dialog.deferred = None;
        }
        self.start_lead_close(index, window, cx);
    }

    /// Selects the worker and runs the ordinary Close flow for a Lead's
    /// `close`.
    fn start_lead_close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.set(Some(Selection::Card(index)));
        self.close(window, cx);
        // The flow starts with `busy` set. If neither that nor a dialog
        // appeared, nothing will ever answer, so answer now.
        if !self.busy
            && self.overlay.is_none()
            && let Some(dialog) = self.lead_dialog.take()
        {
            let _ = dialog
                .reply
                .send(refused("Shika could not open the dialog. Try again."));
        }
        cx.notify();
    }

    /// What `pr` and `close` both require of `task`: a worker this Lead
    /// started in its project, set up, and not being published. `verb` is
    /// the action, for the ownership refusal.
    fn worker_refusal(
        &self,
        lead_id: &str,
        project: &str,
        task: &str,
        verb: &str,
    ) -> Option<String> {
        let Some(card) = self.project_card(project, task) else {
            return Some(format!("No task {task} in this project."));
        };
        if card.started_by.as_deref() != Some(lead_id) {
            return Some(format!(
                "{task} was not started by this Lead, so it cannot {verb} it."
            ));
        }
        if card.creating || card.launch_error.is_some() {
            return Some(format!("{task} is still being set up."));
        }
        card.lead_publishing.then(|| LEAD_PUBLISHING.to_string())
    }

    /// Whether the close dialog for `task` is on screen.
    fn lead_dialog_open(&self, task: &str) -> bool {
        match &self.overlay {
            Some(Overlay::Close { index, .. } | Overlay::SwitchedClose { index, .. }) => self
                .cards
                .get(*index)
                .and_then(|card| card.session.as_ref())
                .is_some_and(|s| s.id == task),
            _ => false,
        }
    }

    /// Records that the pending dialog for `task` succeeded.
    /// A request still deferred has no dialog yet: a Close on `task` is the
    /// author's own and is not recorded.
    pub(crate) fn lead_dialog_succeeded(&mut self, task: &str, outcome: String) {
        if let Some(dialog) = self
            .lead_dialog
            .as_mut()
            .filter(|d| d.task == task && d.deferred.is_none())
        {
            dialog.outcome = Some(outcome);
        }
    }

    /// Records the latest failure of the pending dialog for `task`.
    pub(crate) fn lead_dialog_failed(&mut self, task: &str, error: String) {
        if let Some(dialog) = self
            .lead_dialog
            .as_mut()
            .filter(|d| d.task == task && d.deferred.is_none())
        {
            dialog.error = Some(error);
        }
    }

    /// Answers the waiting `close` once its dialog is gone and no
    /// flow is still running, then gives the selection and focus back to the
    /// Lead, since the author was talking to it. Called from every place a
    /// dialog can end and from the tick, so a dialog that goes away any other
    /// way (the card or project removed) never leaves the client hanging. If
    /// the Lead itself is gone the client is answered at once and any dialog
    /// stays open for the author.
    pub(crate) fn settle_lead_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.lead_dialog else {
            return;
        };
        let lead = self.cards.iter().position(|card| {
            card.lead.is_some()
                && card
                    .session
                    .as_ref()
                    .is_some_and(|s| s.id == dialog.lead_id)
        });
        let task_open = self
            .cards
            .iter()
            .any(|card| card.session.as_ref().is_some_and(|s| s.id == dialog.task));
        let running = dialog.deferred.is_some()
            || self.lead_dialog_open(&dialog.task)
            || (self.busy && self.overlay.is_none());
        if running && lead.is_some() && task_open {
            return;
        }
        let Some(dialog) = self.lead_dialog.take() else {
            return;
        };
        // Never opened: the author's own Close or a removal ended it, and
        // selection and focus were never taken from the Lead.
        let was_deferred = dialog.deferred.is_some();
        let _ = dialog.reply.send(dialog_reply(
            dialog.outcome,
            dialog.error,
            lead.is_some(),
            task_open,
        ));
        if let Some(lead) = lead
            && !was_deferred
            && self.overlay.is_none()
        {
            self.selection.set(Some(Selection::Card(lead)));
            // The dialog scrolled to the worker, so bring the Lead back
            // into view even when Close kept the scroll for its replacement.
            self.last_revealed_selection = None;
            self.overlay_return_focus = None;
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    /// The task card with session id `task` in `project`.
    fn project_card<'a>(&'a self, project: &'a str, task: &str) -> Option<&'a Card> {
        self.project_tasks(project)
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == task))
    }

    /// `shika diff`: the Changes panel's computation, rendered as text. Git
    /// runs off the app thread.
    fn control_diff(
        &mut self,
        project: &str,
        task: String,
        stat: bool,
        reply: Responder,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self
            .project_card(project, &task)
            .and_then(|card| card.session.clone())
        else {
            let _ = reply.send(refused(format!("No task {task} in this project.")));
            return;
        };
        let core = self.core.clone();
        cx.spawn(async move |_, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    match core.session_diff(&session.id) {
                        Ok(diff) => Reply::Text {
                            text: shika_core::render_unified(
                                &diff,
                                stat,
                                &session.worktree,
                                shika_core::RENDER_CAP_BYTES,
                            ),
                        },
                        Err(error) => Reply::Error {
                            message: error.to_string(),
                        },
                    }
                })
                .await;
            let _ = reply.send(answer);
        })
        .detach();
    }

    /// `shika send` and `shika key`: only a worker this Lead started, and
    /// only when [`input_refusal`] allows it.
    #[allow(clippy::too_many_arguments)]
    fn control_input(
        &mut self,
        project: &str,
        lead_id: &str,
        task: String,
        input: Input,
        message: String,
        reply: Responder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = self.project_card(project, &task) else {
            let _ = reply.send(refused(format!("No task {task} in this project.")));
            return;
        };
        if card.started_by.as_deref() != Some(lead_id) {
            let _ = reply.send(refused(format!(
                "{task} was not started by this Lead, so it cannot type into it."
            )));
            return;
        }
        let escape_only =
            matches!(&input, Input::Keys(keys) if keys.iter().all(|key| key == "escape"));
        let state = InputState {
            status: card.control_status(),
            draft: lock(&card.agent.state).has_draft(),
            publishing: card.lead_publishing,
        };
        if let Some(reason) = input_refusal(&task, &state, escape_only) {
            let _ = reply.send(refused(reason));
            return;
        }
        let modes = card.agent.terminal.modes();
        let steps = match input {
            Input::Paste { text, enter } => Ok(paste_steps(&text, enter, &modes)),
            Input::Keys(keys) => key_steps(&keys, &modes),
        };
        match steps {
            Ok(steps) => self.type_into(
                task.clone(),
                steps,
                window,
                cx,
                Box::new(move |_, result| {
                    let _ = reply.send(match result {
                        Ok(()) => Reply::Done { message },
                        Err(why) => refused(format!("{task}: {why}")),
                    });
                }),
            ),
            Err(why) => {
                let _ = reply.send(refused(why));
            }
        }
    }

    /// Writes `steps` into the agent terminal of session `id`, as the Lead and
    /// not the author: the writes do not count as the author typing, but those
    /// marked `count` go through the typed-input capture, so a line and its
    /// Enter start a turn exactly as a typed one does. The first step is
    /// written now and the rest after their delays; a later step is dropped
    /// if the author typed in between. `done` runs on the app thread with the
    /// outcome.
    pub(crate) fn type_into(
        &mut self,
        id: String,
        mut steps: Vec<Step>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: Done,
    ) {
        let Some(state) = self
            .cards
            .iter()
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == id))
            .map(|card| card.agent.state.clone())
        else {
            done(self, Err("that task is gone.".into()));
            return;
        };
        let stamp = lock(&state).last_typed;
        let core = self.core.clone();
        if steps.is_empty() {
            done(self, Ok(()));
            return;
        }
        let first = steps.remove(0);
        let result = write_step(&core, &state, &first, stamp);
        if steps.is_empty() || result.is_err() {
            done(self, result);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut result = Ok(());
            for step in steps {
                cx.background_executor().timer(step.delay).await;
                result = write_step(&core, &state, &step, stamp);
                if result.is_err() {
                    break;
                }
            }
            let _ = this.update_in(cx, |this, _, _| done(this, result));
        })
        .detach();
    }

    /// Every tick, after commands and chat events: brings each Lead's
    /// activity steps up to its turn (a new turn starts them over, a settled
    /// one clears them), counts the workers it started for the chat header,
    /// pushes both into the chat, and keeps the card's reply line loaded.
    /// Counting is a few status reads per Lead and the setters ignore an
    /// unchanged value, so nothing repaints without a change.
    pub(crate) fn sync_lead_activity(&mut self, cx: &mut Context<Self>) {
        let mut repaint = false;
        for lead in 0..self.cards.len() {
            let card = &self.cards[lead];
            let (Some(state), Some(session)) = (&card.lead, &card.session) else {
                continue;
            };
            let settled = matches!(
                card.control_status(),
                TaskStatus::Waiting | TaskStatus::Ready | TaskStatus::Exited
            );
            let (finished, current, version) =
                lead_steps::sync(&state.steps, card.activity.turn_started, settled);
            let seen = state.steps_seen;
            let (project, chat) = (card.project.clone(), card.chat);
            let workers = WorkerSummary::count(
                self.cards
                    .iter()
                    .filter(|worker| {
                        worker.lead.is_none()
                            && !worker.discard
                            && !worker.closing
                            && worker.started_by.as_deref() == Some(session.id.as_str())
                    })
                    .map(Card::control_status),
            );
            if version != seen {
                repaint = true;
                if let Some(state) = self.cards[lead].lead.as_mut() {
                    state.steps_seen = version;
                }
            }
            if chat && let Some(view) = self.lead_chats.get(&project) {
                view.update(cx, |view, cx| {
                    view.set_steps(finished, current, cx);
                    view.set_worker_summary(workers, cx);
                });
            }
        }
        self.sync_lead_snippets(cx);
        if repaint {
            cx.notify();
        }
    }

    /// Reads the saved history of each chat Lead once, off the UI thread, for
    /// the reply line on its card (a live reply replaces it as it arrives),
    /// and drops the line of a Lead that was forgotten.
    fn sync_lead_snippets(&mut self, cx: &mut Context<Self>) {
        let saved: Vec<String> = self
            .projects
            .iter()
            .filter(|project| {
                project
                    .lead
                    .as_ref()
                    .is_some_and(|lead| lead.cli == "claude" && lead.chat_history.is_some())
            })
            .map(|project| project.id.clone())
            .collect();
        self.lead_snippets.retain(|id, _| saved.contains(id));
        self.snippets_requested.retain(|id| saved.contains(id));
        for id in saved {
            if !self.snippets_requested.insert(id.clone()) {
                continue;
            }
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let project = id.clone();
                let line = cx
                    .background_executor()
                    .spawn(async move {
                        let records = core.lead_chat_page(&project, None).ok()?;
                        records
                            .iter()
                            .filter_map(|record| match &record.content {
                                shika_core::ChatContent::Lead { text } => Some((record.id, text)),
                                _ => None,
                            })
                            .max_by_key(|(id, _)| *id)
                            .and_then(|(_, text)| crate::lead_chat_markdown::reply_first_line(text))
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    // A reply that arrived meanwhile is newer.
                    if let Some(line) = line
                        && this.snippets_requested.contains(&id)
                    {
                        this.lead_snippets.entry(id).or_insert(line);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Rings each Lead whose workers settled, when [`Gate`] is open.
    pub(super) fn chat_delivery_finished(&mut self, index: usize, accepted: bool) {
        let Some(delivery) = self.cards[index].chat_delivery.take() else {
            return;
        };
        match delivery {
            ChatDelivery::Bell(marks) if accepted => {
                if let Some(state) = &mut self.cards[index].lead {
                    state.doorbell.rung.mark(marks);
                    state.doorbell.since = None;
                }
            }
            ChatDelivery::Restart(summary) if accepted => {
                if !summary.workers.is_empty()
                    && let Err(error) = self
                        .core
                        .mark_workers_reported(&self.cards[index].project, &summary.workers)
                {
                    self.message(error.to_string());
                }
            }
            ChatDelivery::Restart(summary) => {
                if let Some(run) = &mut self.cards[index].lead_run {
                    run.summary = Some(summary);
                }
            }
            _ => {}
        }
    }

    fn ring_doorbells(&mut self, now: Instant, window: &mut Window, cx: &mut Context<Self>) {
        for lead in 0..self.cards.len() {
            let card = &self.cards[lead];
            let (Some(state), Some(session)) = (&card.lead, &card.session) else {
                continue;
            };
            let lead_id = session.id.clone();
            let chat = card.chat;
            let gate = lead_gate(
                card,
                state,
                self.lead_chats
                    .get(&card.project)
                    .filter(|_| card.chat)
                    .map(|chat| chat.read(cx)),
                cx,
            );
            let tasks = self.wait_set(lead, &[]);
            let modes = self.cards[lead].agent.terminal.modes();
            let Some(LeadState {
                ledger, doorbell, ..
            }) = self.cards[lead].lead.as_mut()
            else {
                continue;
            };
            let due = doorbell.due(ledger, &tasks, &gate, now);
            if due.is_empty() {
                continue;
            }
            let line = doorbell_line(&due.iter().map(|task| &task.info).collect::<Vec<_>>());
            let ids: Vec<String> = due.iter().map(|task| task.info.id.clone()).collect();
            if chat {
                let events = due
                    .iter()
                    .map(|task| {
                        let label = match task.info.status {
                            TaskStatus::Asking => "is asking you",
                            TaskStatus::Exited => "exited",
                            _ => "is ready to check",
                        };
                        let updated = doorbell.rung.reported.get(&task.info.id).is_some_and(
                            |(_, turn, receipt)| {
                                *turn == task.turn
                                    && *receipt != task.report_received
                                    && task.report_received.is_some()
                            },
                        );
                        let title = if tasks
                            .iter()
                            .filter(|other| other.info.title == task.info.title)
                            .count()
                            > 1
                        {
                            format!("{} ({})", task.info.title, task.info.cli)
                        } else {
                            task.info.title.clone()
                        };
                        (
                            format!(
                                "{title} {label}{}",
                                if updated { " - Report updated" } else { "" }
                            ),
                            Some(format!("worker:{}:{:?}", task.info.id, task.turn)),
                        )
                    })
                    .collect();
                self.cards[lead].chat_delivery = Some(ChatDelivery::Bell(
                    due.iter()
                        .map(|task| (task.info.id.clone(), task.settlement()))
                        .collect(),
                ));
                if !self.submit_lead_chat(
                    lead,
                    line,
                    shika_core::ChatOrigin::ShikaEvent,
                    events,
                    cx,
                ) {
                    self.cards[lead].chat_delivery = None;
                }
                continue;
            }
            doorbell.rang(&due);
            self.type_into(
                lead_id.clone(),
                paste_steps(&line, true, &modes),
                window,
                cx,
                Box::new(move |this, result| {
                    if result.is_err()
                        && let Some(state) = this
                            .cards
                            .iter_mut()
                            .find(|card| card.session.as_ref().is_some_and(|s| s.id == lead_id))
                            .and_then(|card| card.lead.as_mut())
                    {
                        state.doorbell.forget(&ids);
                    }
                }),
            );
        }
    }

    /// Types a restarted Lead's one-line summary (`LeadRun::summary`) the way
    /// the doorbell types its line, through the same [`Gate`], and only once
    /// the CLI has drawn its prompt and gone quiet. It is typed once: the
    /// workers it names are recorded as reported when it lands, and a line
    /// that did not land is kept for the next tick.
    fn deliver_restart_summaries(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for lead in 0..self.cards.len() {
            let card = &self.cards[lead];
            let (Some(state), Some(session), Some(run)) =
                (&card.lead, &card.session, &card.lead_run)
            else {
                continue;
            };
            if run.summary.is_none() {
                continue;
            }
            let quiet = card.chat
                || lock(&card.agent.state)
                    .last_output
                    .is_some_and(|at| now.duration_since(at) >= SUMMARY_QUIET);
            if !quiet
                || !lead_gate(
                    card,
                    state,
                    self.lead_chats
                        .get(&card.project)
                        .filter(|_| card.chat)
                        .map(|chat| chat.read(cx)),
                    cx,
                )
                .open(now)
            {
                continue;
            }
            let (lead_id, project) = (session.id.clone(), card.project.clone());
            let modes = card.agent.terminal.modes();
            let Some(summary) = self.cards[lead]
                .lead_run
                .as_mut()
                .and_then(|run| run.summary.take())
            else {
                continue;
            };
            if self.cards[lead].chat {
                let mut events = vec![(
                    "Shika restarted. Worker worktrees were kept.".to_string(),
                    None,
                )];
                if summary.line.contains("checkout") {
                    events.push((
                        "The Lead checkout was kept without moving to the latest base.".into(),
                        None,
                    ));
                }
                self.cards[lead].chat_delivery = Some(ChatDelivery::Restart(summary.clone()));
                if !self.submit_lead_chat(
                    lead,
                    summary.line.clone(),
                    shika_core::ChatOrigin::ShikaEvent,
                    events,
                    cx,
                ) {
                    self.cards[lead].chat_delivery = None;
                    if let Some(run) = &mut self.cards[lead].lead_run {
                        run.summary = Some(summary);
                    }
                }
                continue;
            }
            let steps = paste_steps(&summary.line, true, &modes);
            self.type_into(
                lead_id.clone(),
                steps,
                window,
                cx,
                Box::new(move |this, result| match result {
                    Ok(()) if summary.workers.is_empty() => {}
                    Ok(()) => {
                        // Named once: a later relaunch does not repeat them.
                        if let Err(error) =
                            this.core.mark_workers_reported(&project, &summary.workers)
                        {
                            this.message(error.to_string());
                        }
                    }
                    Err(_) => {
                        if let Some(run) = this
                            .cards
                            .iter_mut()
                            .find(|card| card.session.as_ref().is_some_and(|s| s.id == lead_id))
                            .and_then(|card| card.lead_run.as_mut())
                        {
                            run.summary = Some(summary);
                        }
                    }
                }),
            );
        }
    }

    /// The project's task cards: everything but its Lead and its checkout
    /// session. Every command that names or lists tasks goes through here,
    /// so a Lead can never see, read, send to, or close the agent in the
    /// author's checkout.
    fn project_tasks<'a>(&'a self, project: &'a str) -> impl Iterator<Item = &'a Card> {
        self.cards.iter().filter(move |card| {
            card.project == project && is_lead_visible(card.lead.is_some(), card.session.as_ref())
        })
    }

    fn wait_refusal(&self, project: &str, lead_id: &str, named: &[String]) -> Option<Reply> {
        for id in named {
            let card = self
                .project_tasks(project)
                .find(|card| card.session.as_ref().is_some_and(|s| &s.id == id));
            match card {
                None => return Some(refused(format!("No task {id} in this project."))),
                Some(card) if card.started_by.as_deref() != Some(lead_id) => {
                    return Some(refused(format!(
                        "{id} was not started by this Lead, so it cannot wait on it."
                    )));
                }
                Some(_) => {}
            }
        }
        None
    }

    /// The tasks a wait covers, as they are right now.
    fn wait_set(&self, lead: usize, named: &[String]) -> Vec<Observed> {
        let Some(lead_id) = self.cards[lead].session.as_ref().map(|s| s.id.as_str()) else {
            return Vec::new();
        };
        self.project_tasks(&self.cards[lead].project)
            .filter(|card| card.started_by.as_deref() == Some(lead_id) && !card.discard)
            .filter(|card| {
                named.is_empty() || card.session.as_ref().is_some_and(|s| named.contains(&s.id))
            })
            .filter_map(|card| card.observed(lead_id))
            .collect()
    }

    /// Answers every wait whose condition holds. Events are marked reported
    /// only when the connection thread confirms the reply was written, so a
    /// client that went away leaves its events unreported.
    fn resolve_waiters(&mut self, now: Instant) {
        for lead in 0..self.cards.len() {
            let Some(state) = self.cards[lead].lead.as_mut() else {
                continue;
            };
            apply_confirmations(&mut state.ledger, &mut state.unconfirmed);
            state.waiters.retain(Waiter::connected);
            if state.waiters.is_empty() {
                continue;
            }
            let waiters = std::mem::take(&mut state.waiters);
            let mut pending = Vec::new();
            for waiter in waiters {
                let tasks = self.wait_set(lead, &waiter.named);
                let Some(state) = self.cards[lead].lead.as_mut() else {
                    continue;
                };
                match resolve(&state.ledger, &tasks, now, waiter.deadline) {
                    Some(Resolved { reply, marks }) => {
                        if waiter.reply.send(reply) {
                            state.unconfirmed.push(Unconfirmed {
                                delivered: waiter.delivered,
                                marks,
                            });
                        }
                    }
                    None => pending.push(waiter),
                }
            }
            if let Some(state) = self.cards[lead].lead.as_mut() {
                state.waiters.extend(pending);
            }
        }
    }

    /// Why this Lead may not start another worker with `cli` now.
    fn new_refusal(&self, lead_id: &str, cli: &str) -> Option<Reply> {
        let Some(catalog) = &self.catalog else {
            return Some(refused(
                "Shika is still looking for installed CLIs. Try again.",
            ));
        };
        let Some(preset) = catalog.presets.iter().find(|preset| preset.id == cli) else {
            return Some(refused(format!(
                "Unknown CLI {cli}. Use one of {}.",
                shika_core::control::CLI_IDS.join(", ")
            )));
        };
        if !preset.found() {
            return Some(refused(format!(
                "{} is not installed (no `{}` on PATH). Pick another CLI.",
                preset.name, preset.binary
            )));
        }
        let live = self
            .cards
            .iter()
            .filter(|card| card.is_live_worker_of(lead_id))
            .count();
        (live >= MAX_WORKERS).then(|| {
            refused(format!(
                "This Lead already has {MAX_WORKERS} live workers. Wait for them, and ask the author to close finished tasks before starting more."
            ))
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn control_new(
        &mut self,
        lead: usize,
        cli: String,
        base: Option<String>,
        prompt: String,
        reply: Responder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(lead_id) = self.cards[lead].session.as_ref().map(|s| s.id.clone()) else {
            return;
        };
        if let Some(refusal) = self.new_refusal(&lead_id, &cli) {
            let _ = reply.send(refusal);
            return;
        }
        let Some(preset) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.iter().find(|preset| preset.id == cli))
            .cloned()
        else {
            return;
        };
        let project = self.cards[lead].project.clone();
        let core = self.core.clone();
        let lookup = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Same approval check as New. The Lead can never approve setup.
            let checked = cx
                .background_executor()
                .spawn(async move {
                    let config = core.project_preparation(&lookup)?;
                    let approved = match &config {
                        Some(config) => core.preparation_approved(&lookup, config)?,
                        None => true,
                    };
                    Ok::<_, shika_core::Error>((config, approved))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The Lead may have closed, or parallel calls may have filled
                // its slots, while the check ran.
                if !this.cards.iter().any(|card| {
                    card.lead.is_some() && card.session.as_ref().is_some_and(|s| s.id == lead_id)
                }) {
                    let _ = reply.send(refused("This Lead was closed."));
                    return;
                }
                if let Some(refusal) = this.new_refusal(&lead_id, &cli) {
                    let _ = reply.send(refusal);
                    return;
                }
                match checked {
                    Ok((Some(_), false)) => {
                        let _ = reply.send(refused(
                            "This project's setup needs the author's approval in Shika first: start one task with Cmd+N and approve it.",
                        ));
                    }
                    Ok((config, _)) => {
                        let Some(server) = &this.control else {
                            let _ = reply.send(refused("Shika's control socket is not running."));
                            return;
                        };
                        let options = LaunchOptions {
                            prompt: Some(worker_prompt(&prompt, &server.bin_dir())),
                            started_by: Some(lead_id),
                            base,
                            control: Some(WorkerEnv {
                                socket: server.socket(),
                                token: shika_core::control::new_token(),
                                bin_dir: server.bin_dir(),
                            }),
                        };
                        this.begin_launch(
                            project,
                            preset,
                            config.is_some(),
                            None,
                            Launch::Worker(options),
                            Some(reply),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        let _ = reply.send(failure_reply(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The reply to `shika new` once the card at `index` has its session.
    pub(crate) fn started_reply(&self, index: usize) -> Reply {
        let card = &self.cards[index];
        match card
            .started_by
            .as_deref()
            .and_then(|lead_id| card.task_info(lead_id))
        {
            Some(mut task) => {
                // The launch prompt names the card on the next tick; say
                // that name now rather than the `New <CLI>` placeholder.
                if let Some(title) = &lock(&card.agent.state).title {
                    task.title = crate::model::card_title(title);
                }
                Reply::Started { task }
            }
            None => Reply::Error {
                message: "The task started but has no session.".into(),
            },
        }
    }
}

impl HostState {
    /// A fast worker can report before the tick consumes its launch/send
    /// submission. Use the epoch Activity will start, not the previous turn.
    /// Input during an active turn retains the existing epoch.
    fn report_turn(&self, activity: &crate::activity::Activity, submitted: u64) -> Option<Instant> {
        if self.submission != submitted
            && matches!(activity.status, Status::Waiting | Status::Ready)
        {
            self.last_submission.or(activity.turn_started)
        } else {
            activity.turn_started
        }
    }

    /// Writes a step the Lead sent. It leaves `last_typed` alone, so the
    /// author-typing guards see only the author, and a step marked `count`
    /// goes through the capture that typed input does. Refused when the CLI
    /// is gone or setup is running, or when the author typed since `stamp`.
    fn inject(
        &mut self,
        core: &shika_core::Core,
        step: &Step,
        stamp: Option<Instant>,
    ) -> Result<(), String> {
        if self.last_typed != stamp {
            return Err("the author typed into it meanwhile, so nothing more was sent.".into());
        }
        let Some(pty) = self.pty.filter(|_| !self.exited && !self.preparing) else {
            return Err("its CLI has exited.".into());
        };
        if step.count {
            self.capture_typed(&step.bytes, Instant::now());
        }
        core.write(pty, &step.bytes)
            .map_err(|_| "its terminal is closed.".to_string())
    }
}

/// Start and stop of the Lead's own launch prompt, in one place for the host.
impl HostState {
    /// The CLI was launched with `prompt`. Counts as the first submitted
    /// line, through the fields a typed submission sets, so the existing
    /// turn machinery (candidate turn, timer, once-per-turn notification,
    /// lifecycle fencing, branch naming from the first line) needs no second
    /// path. `name` is false for the Lead, which keeps its title.
    pub(crate) fn seed_launch_prompt(&mut self, prompt: &str, name: bool, now: Instant) {
        if name {
            self.title = crate::model::prompt_title(prompt);
        }
        // A line typed later must not name the task or rename its branch.
        self.prompt.finish();
        self.submission += 1;
        self.last_submission = Some(now);
        self.launch_turn = true;
    }
}

/// Whether a Lead may see a card of its own project as a task at all: never
/// a Lead card, and never the agent in the author's checkout. A card still
/// starting, with no session yet, is a worktree task.
fn is_lead_visible(lead_card: bool, session: Option<&Session>) -> bool {
    !lead_card && !session.is_some_and(Session::is_checkout)
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
