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
use crate::{Card, HostState, Launch, Shika, lock, model::Status};
use gpui::{Context, Window};
use shika_core::LaunchOptions;
use shika_core::control::{
    Command, ControlDir, ControlError, MAX_READ_LINES, Reply, TaskDiffStat, TaskInfo, TaskStatus,
    WaitEvent, read_request, write_reply,
};
use shika_terminal::Modes;
use shika_terminal::input::{Key, KeyMods, encode_key, encode_paste};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// How long the doorbell waits after the first unrung settlement, so several
/// workers finishing together become one line.
const BELL_BATCH: Duration = Duration::from_millis(1500);

/// The doorbell stays quiet this long after the author last typed in the
/// Lead's terminal.
const BELL_TYPING_QUIET: Duration = Duration::from_secs(3);

/// Pause between a pasted line and its Enter. Some TUIs (Codex's paste-burst
/// handling) read an Enter that arrives inside a paste burst as a newline.
const ENTER_DELAY: Duration = Duration::from_millis(150);

/// Pause between the keys of one `shika key`, so a TUI sees them one by one.
const KEY_DELAY: Duration = Duration::from_millis(40);

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
    pub fn start() -> Result<Self, ControlError> {
        let dir = ControlDir::create()?;
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

/// Everything the Lead's card keeps for the control socket.
pub struct LeadState {
    pub token: String,
    ledger: Ledger,
    waiters: Vec<Waiter>,
    unconfirmed: Vec<Unconfirmed>,
    doorbell: Doorbell,
}

impl LeadState {
    pub fn new(token: String) -> Self {
        Self {
            token,
            ledger: Ledger::default(),
            waiters: Vec::new(),
            unconfirmed: Vec::new(),
            doorbell: Doorbell::default(),
        }
    }
}

/// A task as `wait` sees it: its state and which turn that state belongs to.
pub struct Observed {
    pub info: TaskInfo,
    pub turn: Option<Instant>,
}

/// A settlement already reported to the Lead: the task, the state it settled
/// in, and the turn. A task that works again and settles again is a new turn,
/// so it is reported again; the same settlement is never reported twice.
type Settlement = (TaskStatus, Option<Instant>);

#[derive(Default)]
pub struct Ledger {
    reported: HashMap<String, Settlement>,
}

impl Ledger {
    fn is_reported(&self, task: &Observed) -> bool {
        self.reported.get(&task.info.id) == Some(&(task.info.status, task.turn))
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
    /// (task, status, turn).
    pub fn rang(&mut self, tasks: &[&Observed]) {
        self.rung.mark(
            tasks
                .iter()
                .map(|task| (task.info.id.clone(), (task.info.status, task.turn)))
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
            format!("{} \"{}\" {what}", task.id, title.replace('"', "'"))
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
            .map(|task| (task.info.id.clone(), (task.info.status, task.turn)))
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
    reply: Sender<Reply>,
    delivered: Receiver<bool>,
}

/// Sends the answer to a `shika new`, if one is waiting for it.
pub fn answer(reply: &Option<Sender<Reply>>, answer: Reply) {
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
        | Error::NoSuchBranch(_)
        | Error::BaseBranchMissing(_)
        | Error::CliNotFound(_)
        | Error::PreparationNeedsApproval => refused(error.to_string()),
        _ => Reply::Error {
            message: error.to_string(),
        },
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
        })
    }

    fn observed(&self, lead_id: &str) -> Option<Observed> {
        Some(Observed {
            info: self.task_info(lead_id)?,
            turn: self.activity.turn_started,
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
        let now = Instant::now();
        self.resolve_waiters(now);
        self.ring_doorbells(now, window, cx);
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
        let answer = match command {
            Command::Help => Reply::Help {
                text: LEAD_GUIDE.to_string(),
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
        reply: Sender<Reply>,
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
        reply: Sender<Reply>,
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

    /// Rings each Lead whose workers settled, when [`Gate`] is open.
    fn ring_doorbells(&mut self, now: Instant, window: &mut Window, cx: &mut Context<Self>) {
        for lead in 0..self.cards.len() {
            let card = &self.cards[lead];
            let (Some(state), Some(session)) = (&card.lead, &card.session) else {
                continue;
            };
            let lead_id = session.id.clone();
            let gate = {
                let host = lock(&card.agent.state);
                Gate {
                    waiting: !state.waiters.is_empty() || !state.unconfirmed.is_empty(),
                    idle: !card.creating
                        && matches!(card.status, Status::Waiting | Status::Ready)
                        && host.submission == card.submitted,
                    draft: host.has_draft(),
                    last_typed: host.last_typed,
                    alive: !host.exited && !host.preparing && host.pty.is_some(),
                }
            };
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
            doorbell.rang(&due);
            self.type_into(
                lead_id,
                paste_steps(&line, true, &modes),
                window,
                cx,
                Box::new(move |this, result| {
                    if result.is_err()
                        && let Some(state) =
                            this.cards.get_mut(lead).and_then(|card| card.lead.as_mut())
                    {
                        state.doorbell.forget(&ids);
                    }
                }),
            );
        }
    }

    /// The project's task cards: everything but its Lead.
    fn project_tasks<'a>(&'a self, project: &'a str) -> impl Iterator<Item = &'a Card> {
        self.cards
            .iter()
            .filter(move |card| card.lead.is_none() && card.project == project)
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
                        if waiter.reply.send(reply).is_ok() {
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
        reply: Sender<Reply>,
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
                        let options = LaunchOptions {
                            prompt: Some(prompt),
                            started_by: Some(lead_id),
                            base,
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

#[cfg(test)]
mod tests {
    use super::*;
    use shika_core::control::{PROTOCOL_VERSION, Request, send};
    use std::sync::Mutex;

    fn task(id: &str, status: TaskStatus) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            title: id.into(),
            cli: "codex".into(),
            status,
            elapsed_secs: None,
            branch: String::new(),
            diff_stat: None,
            pr: None,
            started_by_lead: true,
            path: String::new(),
        }
    }

    fn observed(id: &str, status: TaskStatus, turn: Option<Instant>) -> Observed {
        Observed {
            info: task(id, status),
            turn,
        }
    }

    fn waited(resolved: Option<Resolved>) -> (Vec<String>, Vec<String>, bool, Resolved) {
        let resolved = resolved.expect("a reply");
        let Reply::Waited {
            events,
            still_working,
            timed_out,
        } = resolved.reply.clone()
        else {
            panic!("expected Waited");
        };
        (
            events.into_iter().map(|e| e.task.id).collect(),
            still_working.into_iter().map(|t| t.id).collect(),
            timed_out,
            resolved,
        )
    }

    #[test]
    fn wait_returns_settled_tasks_once_and_again_after_a_new_turn() {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(100);
        let turn1 = Some(t0);
        let mut ledger = Ledger::default();
        let tasks = [
            observed("a", TaskStatus::Ready, turn1),
            observed("b", TaskStatus::Working, turn1),
        ];
        let (events, working, timed_out, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(
            (events, working, timed_out),
            (vec!["a".into()], vec!["b".into()], false)
        );
        ledger.mark(resolved.marks);

        // The same settlement is not reported twice: keep waiting for b.
        assert!(resolve(&ledger, &tasks, t0, deadline).is_none());

        // b asks, then a later wait sees it settle in Ready as a new state.
        let tasks = [
            observed("a", TaskStatus::Ready, turn1),
            observed("b", TaskStatus::Asking, turn1),
        ];
        let (events, working, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!((events, working), (vec!["b".into()], vec![]));
        ledger.mark(resolved.marks);
        let tasks = [
            observed("a", TaskStatus::Ready, turn1),
            observed("b", TaskStatus::Ready, turn1),
        ];
        let (events, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["b".to_string()]);
        ledger.mark(resolved.marks);

        // Nothing unreported and nothing working: answered at once, empty.
        let (events, working, timed_out, _) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!((events, working, timed_out), (vec![], vec![], false));

        // a works again (a new turn) and settles again: reported again.
        let turn2 = Some(t0 + Duration::from_secs(60));
        let tasks = [
            observed("a", TaskStatus::Working, turn2),
            observed("b", TaskStatus::Ready, turn1),
        ];
        assert!(resolve(&ledger, &tasks, t0, deadline).is_none());
        let tasks = [
            observed("a", TaskStatus::Ready, turn2),
            observed("b", TaskStatus::Ready, turn1),
        ];
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["a".to_string()]);
    }

    #[test]
    fn wait_times_out_naming_what_still_works_and_exits_count_as_settled() {
        let t0 = Instant::now();
        let ledger = Ledger::default();
        let tasks = [
            observed("a", TaskStatus::Working, Some(t0)),
            observed("b", TaskStatus::Starting, None),
            observed("c", TaskStatus::Waiting, None),
        ];
        assert!(resolve(&ledger, &tasks, t0, t0 + Duration::from_secs(5)).is_none());
        let (events, working, timed_out, _) = waited(resolve(
            &ledger,
            &tasks,
            t0 + Duration::from_secs(5),
            t0 + Duration::from_secs(5),
        ));
        assert_eq!(
            (events, working, timed_out),
            (vec![], vec!["a".to_string(), "b".to_string()], true)
        );
        let tasks = [observed("a", TaskStatus::Exited, Some(t0))];
        let (events, _, timed_out, _) =
            waited(resolve(&ledger, &tasks, t0, t0 + Duration::from_secs(5)));
        assert_eq!((events, timed_out), (vec!["a".to_string()], false));
        // No tasks at all: nothing to wait for.
        let (events, working, timed_out, _) = waited(resolve(&ledger, &[], t0, t0));
        assert_eq!((events, working, timed_out), (vec![], vec![], false));
    }

    #[test]
    fn undelivered_events_stay_unreported() {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(10);
        let mut ledger = Ledger::default();
        let tasks = [observed("a", TaskStatus::Ready, Some(t0))];
        // Resolving alone marks nothing: the caller marks after confirmation.
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["a".to_string()]);
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["a".to_string()]);

        // A write that failed, or a client that left first, drops the marks.
        for outcome in [Some(false), None] {
            let (_, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
            let (confirm, delivered) = mpsc::channel();
            let mut pending = vec![Unconfirmed {
                delivered,
                marks: resolved.marks,
            }];
            apply_confirmations(&mut ledger, &mut pending);
            assert_eq!(pending.len(), 1, "still in flight");
            match outcome {
                Some(written) => confirm.send(written).unwrap(),
                None => drop(confirm),
            }
            apply_confirmations(&mut ledger, &mut pending);
            assert!(pending.is_empty());
            let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
            assert_eq!(events, vec!["a".to_string()]);
        }
    }

    #[test]
    fn a_confirmed_write_marks_the_events_reported() {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(10);
        let mut ledger = Ledger::default();
        let tasks = [observed("a", TaskStatus::Ready, Some(t0))];
        let (_, _, _, resolved) = waited(resolve(&ledger, &tasks, t0, deadline));
        let (confirm, delivered) = mpsc::channel();
        let mut pending = vec![Unconfirmed {
            delivered,
            marks: resolved.marks,
        }];
        // Until the write is confirmed the event is still owed.
        apply_confirmations(&mut ledger, &mut pending);
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert_eq!(events, vec!["a".to_string()]);
        confirm.send(true).unwrap();
        apply_confirmations(&mut ledger, &mut pending);
        assert!(pending.is_empty());
        let (events, ..) = waited(resolve(&ledger, &tasks, t0, deadline));
        assert!(events.is_empty());
    }

    #[test]
    fn the_connection_thread_confirms_a_written_reply() {
        let server = Server::start().expect("control server");
        let socket = server.socket();
        let (seen, confirmations) = mpsc::channel();
        let seen = Mutex::new(seen);
        answer_with(server, move |incoming| {
            let _ = incoming.reply.send(Reply::Help { text: "hi".into() });
            let _ = seen.lock().unwrap().send(incoming.delivered);
        });
        send(&socket, &Request::new("t", Command::Help)).unwrap();
        let delivered = confirmations.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(delivered.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    /// A stand-in for the UI thread: answers every request on its own thread.
    fn answer_with(server: Server, handler: impl Fn(Incoming) + Send + 'static) {
        std::thread::spawn(move || {
            loop {
                match server.next() {
                    Some(incoming) => handler(incoming),
                    None => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
    }

    #[test]
    fn requests_reach_the_ui_thread_and_replies_come_back() {
        let server = Server::start().expect("control server");
        let socket = server.socket();
        assert!(server.bin_dir().join("shika").exists());
        answer_with(server, |incoming| {
            let reply = match (incoming.token.as_str(), incoming.command) {
                ("good", Command::Tasks) => Reply::Tasks {
                    tasks: vec![task("a", TaskStatus::Ready)],
                },
                ("good", _) => Reply::Help { text: "hi".into() },
                _ => refused("unknown token"),
            };
            let _ = incoming.reply.send(reply);
        });

        let reply = send(&socket, &Request::new("good", Command::Tasks)).unwrap();
        assert!(matches!(reply, Reply::Tasks { tasks } if tasks.len() == 1));
        let reply = send(&socket, &Request::new("bad", Command::Tasks)).unwrap();
        assert_eq!(reply.exit_code(), 1);

        let mut stale = Request::new("good", Command::Help);
        stale.version = PROTOCOL_VERSION + 1;
        assert_eq!(send(&socket, &stale).unwrap().exit_code(), 1);
    }

    #[test]
    fn a_client_that_left_is_noticed_before_its_reply_is_sent() {
        let server = Server::start().expect("control server");
        let socket = server.socket();
        let (arrived, request_seen) = mpsc::channel();
        let (go, proceed) = mpsc::channel::<()>();
        let (delivered, outcome) = mpsc::channel();
        let (arrived, proceed) = (Mutex::new(arrived), Mutex::new(proceed));
        answer_with(server, move |incoming| {
            let _ = arrived.lock().unwrap().send(());
            // Hold the request until the client is gone, as a wait would.
            let _ = proceed.lock().unwrap().recv();
            let sent = incoming.reply.send(Reply::Help {
                text: "late".into(),
            });
            let _ = delivered.send(sent.is_ok());
        });
        let stream = UnixStream::connect(&socket).unwrap();
        let request = serde_json::to_vec(&Request::new("t", Command::Help)).unwrap();
        {
            use std::io::Write;
            let mut writer = &stream;
            writer.write_all(&request).unwrap();
            writer.write_all(b"\n").unwrap();
        }
        request_seen.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(stream);
        // The connection thread checks the socket twice a second; once it
        // sees the client gone it drops its end, so the send fails and a wait
        // would keep its events unreported.
        std::thread::sleep(Duration::from_millis(1500));
        go.send(()).unwrap();
        assert!(!outcome.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn dropping_the_server_removes_its_directory() {
        let server = Server::start().expect("control server");
        let socket = server.socket();
        assert!(socket.exists());
        drop(server);
        assert!(!socket.exists());
    }

    #[test]
    fn launch_failures_split_into_refusals_and_errors() {
        use shika_core::Error;
        assert_eq!(
            failure_reply(&Error::InvalidPrompt("x".into())).exit_code(),
            1
        );
        assert_eq!(
            failure_reply(&Error::NoSuchBranch("x".into())).exit_code(),
            1
        );
        assert_eq!(failure_reply(&Error::UnknownCli).exit_code(), 2);
    }

    fn gate() -> Gate {
        Gate {
            waiting: false,
            idle: true,
            draft: false,
            last_typed: None,
            alive: true,
        }
    }

    fn at(t0: Instant, secs: f64) -> Instant {
        t0 + Duration::from_secs_f64(secs)
    }

    fn ids(due: &[&Observed]) -> Vec<String> {
        due.iter().map(|task| task.info.id.clone()).collect()
    }

    #[test]
    fn the_doorbell_batches_settlements_then_rings_once() {
        let t0 = Instant::now();
        let turn = Some(t0);
        let reported = Ledger::default();
        let mut bell = Doorbell::default();
        let tasks = [observed("a", TaskStatus::Ready, turn)];
        // Nothing rings before the batch window ends.
        assert!(bell.due(&reported, &tasks, &gate(), t0).is_empty());
        assert!(bell.due(&reported, &tasks, &gate(), at(t0, 1.0)).is_empty());
        // A second worker settling inside the window joins the same ring.
        let tasks = [
            observed("a", TaskStatus::Ready, turn),
            observed("b", TaskStatus::Asking, turn),
            observed("c", TaskStatus::Working, turn),
        ];
        let due = bell.due(&reported, &tasks, &gate(), at(t0, 1.6));
        assert_eq!(ids(&due), ["a", "b"]);
        bell.rang(&due);
        // Never twice for the same (task, status, turn).
        assert!(bell.due(&reported, &tasks, &gate(), at(t0, 9.0)).is_empty());
        // Asking then Ready is a different status: rings again, after its own batch.
        let tasks = [observed("b", TaskStatus::Ready, turn)];
        assert!(
            bell.due(&reported, &tasks, &gate(), at(t0, 10.0))
                .is_empty()
        );
        let due = bell.due(&reported, &tasks, &gate(), at(t0, 11.6));
        assert_eq!(ids(&due), ["b"]);
        bell.rang(&due);
        // A new turn of the same task rings again.
        let again = [observed("a", TaskStatus::Ready, Some(at(t0, 20.0)))];
        assert!(
            bell.due(&reported, &again, &gate(), at(t0, 21.0))
                .is_empty()
        );
        assert_eq!(
            ids(&bell.due(&reported, &again, &gate(), at(t0, 22.6))),
            ["a"]
        );
    }

    #[test]
    fn the_doorbell_skips_what_a_wait_reported_and_what_is_not_settled() {
        let t0 = Instant::now();
        let turn = Some(t0);
        let mut reported = Ledger::default();
        reported.mark(vec![("a".into(), (TaskStatus::Ready, turn))]);
        let mut bell = Doorbell::default();
        let tasks = [
            observed("a", TaskStatus::Ready, turn),
            observed("w", TaskStatus::Waiting, None),
            observed("s", TaskStatus::Starting, None),
        ];
        assert!(bell.due(&reported, &tasks, &gate(), at(t0, 5.0)).is_empty());
        let tasks = [observed("x", TaskStatus::Exited, turn)];
        bell.due(&reported, &tasks, &gate(), t0);
        assert_eq!(
            ids(&bell.due(&reported, &tasks, &gate(), at(t0, 2.0))),
            ["x"]
        );
    }

    #[test]
    fn the_doorbell_waits_for_every_condition_and_rings_when_they_clear() {
        let t0 = Instant::now();
        let turn = Some(t0);
        let reported = Ledger::default();
        let tasks = [observed("a", TaskStatus::Ready, turn)];
        let closed = |gate: Gate| {
            let mut bell = Doorbell::default();
            bell.due(&reported, &tasks, &gate, t0);
            assert!(bell.due(&reported, &tasks, &gate, at(t0, 2.0)).is_empty());
            // The condition clears: it rings at once, the batch window is over.
            assert_eq!(
                ids(&bell.due(&reported, &tasks, &Gate { ..self::gate() }, at(t0, 2.1))),
                ["a"]
            );
        };
        closed(Gate {
            waiting: true,
            ..gate()
        });
        closed(Gate {
            idle: false,
            ..gate()
        });
        closed(Gate {
            draft: true,
            ..gate()
        });
        closed(Gate {
            alive: false,
            ..gate()
        });
        closed(Gate {
            last_typed: Some(at(t0, 0.5)),
            ..gate()
        });
        // Typing is quiet after three seconds.
        let typed = Gate {
            last_typed: Some(t0),
            ..gate()
        };
        assert!(typed.open(at(t0, 3.1)));
        assert!(!typed.open(at(t0, 2.9)));
    }

    #[test]
    fn a_failed_ring_can_be_tried_again() {
        let t0 = Instant::now();
        let turn = Some(t0);
        let reported = Ledger::default();
        let tasks = [observed("a", TaskStatus::Ready, turn)];
        let mut bell = Doorbell::default();
        bell.due(&reported, &tasks, &gate(), t0);
        let due = bell.due(&reported, &tasks, &gate(), at(t0, 2.0));
        bell.rang(&due);
        bell.forget(&["a".to_string()]);
        bell.due(&reported, &tasks, &gate(), at(t0, 3.0));
        assert_eq!(
            ids(&bell.due(&reported, &tasks, &gate(), at(t0, 4.6))),
            ["a"]
        );
    }

    #[test]
    fn the_doorbell_line_is_short_and_names_each_task() {
        let mut long = task("18d0b57", TaskStatus::Ready);
        long.title = "Add slugify to textkit with a very long title that goes on".into();
        let mut asking = task("18d0b58", TaskStatus::Asking);
        asking.title = "Add \"word_count\"".into();
        let exited = task("18d0b59", TaskStatus::Exited);
        let line = doorbell_line(&[&long, &asking, &exited]);
        assert_eq!(
            line,
            "[shika] Workers changed: 18d0b57 \"Add slugify to textkit wi...\" is ready; 18d0b58 \"Add 'word_count'\" is asking; 18d0b59 \"18d0b59\" has exited. Run shika wait."
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn typing_into_a_worker_is_refused_unless_it_is_safe() {
        let state = |status, draft| InputState { status, draft };
        let ok = |status| input_refusal("t", &state(status, false), false);
        assert_eq!(ok(TaskStatus::Ready), None);
        assert_eq!(ok(TaskStatus::Asking), None);
        assert_eq!(ok(TaskStatus::Waiting), None);
        assert!(
            ok(TaskStatus::Working)
                .unwrap()
                .contains("shika key t escape")
        );
        assert!(ok(TaskStatus::Exited).unwrap().contains("has exited"));
        assert!(ok(TaskStatus::Starting).unwrap().contains("starting"));
        // Escape may interrupt a working task.
        assert_eq!(
            input_refusal("t", &state(TaskStatus::Working, false), true),
            None
        );
        // The author's unsent draft is never appended to, even by an interrupt.
        let refusal = input_refusal("t", &state(TaskStatus::Ready, true), false).unwrap();
        assert!(refusal.contains("author has typed"), "{refusal}");
        assert!(input_refusal("t", &state(TaskStatus::Working, true), true).is_some());
    }

    #[test]
    fn keys_encode_for_the_programs_mode() {
        let normal = Modes::default();
        let app = Modes {
            app_cursor: true,
            ..Modes::default()
        };
        assert_eq!(key_bytes("enter", &normal).unwrap(), b"\r");
        assert_eq!(key_bytes("escape", &normal).unwrap(), b"\x1b");
        assert_eq!(key_bytes("tab", &normal).unwrap(), b"\t");
        assert_eq!(key_bytes("space", &normal).unwrap(), b" ");
        assert_eq!(key_bytes("backspace", &normal).unwrap(), b"\x7f");
        assert_eq!(key_bytes("y", &normal).unwrap(), b"y");
        assert_eq!(key_bytes("7", &normal).unwrap(), b"7");
        assert_eq!(key_bytes("down", &normal).unwrap(), b"\x1b[B");
        assert_eq!(key_bytes("down", &app).unwrap(), b"\x1bOB");
        assert_eq!(key_bytes("left", &app).unwrap(), b"\x1bOD");
        assert_eq!(key_bytes("f1", &normal), None);
        assert_eq!(key_bytes("Y", &normal), None);
        let steps = key_steps(&["down".into(), "enter".into()], &normal).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].delay, Duration::ZERO);
        assert_eq!(steps[1].delay, KEY_DELAY);
        assert!(steps.iter().all(|step| !step.count));
        assert!(key_steps(&["nope".into()], &normal).is_err());
    }

    #[test]
    fn a_pasted_line_and_its_separate_enter_are_one_submission() {
        let bracketed = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        let steps = paste_steps("Run the tests\nthen commit \x1b[31m", true, &bracketed);
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].bytes,
            b"\x1b[200~Run the tests\rthen commit [31m\x1b[201~"
        );
        assert_eq!(steps[1].bytes, b"\r");
        assert!(steps[1].delay >= Duration::from_millis(100));
        assert!(steps.iter().all(|step| step.count));
        // The same capture a typed submission goes through counts it once,
        // and leaves no draft behind.
        let mut host = HostState::default();
        let now = Instant::now();
        host.capture_typed(&steps[0].bytes, now);
        assert!(host.has_draft());
        assert_eq!(host.submission, 0);
        host.capture_typed(&steps[1].bytes, now);
        assert_eq!(host.submission, 1);
        assert_eq!(host.last_submission, Some(now));
        assert!(!host.has_draft());

        // Without Enter it is not a submission and is not a captured draft.
        let steps = paste_steps("1", false, &bracketed);
        assert_eq!(steps.len(), 1);
        assert!(!steps[0].count);
        // Without bracketed paste, line breaks cannot submit early.
        let plain = paste_steps("a\nb", true, &Modes::default());
        assert_eq!(plain[0].bytes, b"a b");
    }

    #[test]
    fn injected_steps_are_refused_after_the_author_types_or_the_cli_exits() {
        let path = std::env::temp_dir().join(format!(
            "shika-inject-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = shika_core::Core::open(&path).unwrap();
        let step = Step {
            bytes: b"x".to_vec(),
            count: true,
            delay: Duration::ZERO,
        };
        let mut host = HostState::default();
        // No PTY: nothing to write into.
        assert!(
            host.inject(&core, &step, None)
                .unwrap_err()
                .contains("exited")
        );
        assert_eq!(host.submission, 0);
        // The author typed after the typing began.
        host.last_typed = Some(Instant::now());
        assert!(
            host.inject(&core, &step, None)
                .unwrap_err()
                .contains("author typed")
        );
        assert!(!host.has_draft());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn read_text_drops_trailing_blank_lines_and_spaces() {
        let lines = ["first  ", "", "  indented", "   ", ""]
            .map(String::from)
            .to_vec();
        assert_eq!(screen_text(lines), "first\n\n  indented");
        assert_eq!(
            screen_text(vec!["".into(), " ".into()]),
            "(the terminal is empty)"
        );
    }
}
