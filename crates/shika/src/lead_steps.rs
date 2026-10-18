//! The Lead's activity steps: what the chat lane and the Lead's card say it
//! is doing while a turn runs. They come only from the `shika` commands the
//! Lead runs over the control socket, never from provider output. Memory
//! only, one list per Lead, reset when a new turn starts and cleared when the
//! turn settles. The copy lives in [`step_text`]; DESIGN.md has the table.
use shika_core::control::{Command, Reply};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Finished steps kept and shown (the newest).
const FINISHED_KEPT: usize = 3;
/// A prompt's first line, as a task name, is cut to this many characters.
const NAME_CHARS: usize = 40;
/// The refusal `dialog_reply` gives when the author closes a `pr` or `close`
/// dialog without confirming.
pub const CANCELLED: &str = "The author cancelled.";

/// A command that gets a step. `help` and `report` get none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    New,
    Wait,
    Check,
    Read,
    Diff,
    Answer,
    Pr,
    Close,
    Memory,
    Remember,
    Forget,
}

impl StepKind {
    pub fn of(command: &Command) -> Option<Self> {
        Some(match command {
            Command::Help | Command::Report { .. } => return None,
            Command::New { .. } => Self::New,
            Command::Wait { .. } => Self::Wait,
            Command::Tasks | Command::Status { .. } => Self::Check,
            Command::Read { .. } => Self::Read,
            Command::Diff { .. } => Self::Diff,
            Command::Send { .. } | Command::Key { .. } => Self::Answer,
            Command::Pr { .. } => Self::Pr,
            Command::Close { .. } => Self::Close,
            Command::Memory => Self::Memory,
            Command::Remember { .. } => Self::Remember,
            Command::Forget { .. } => Self::Forget,
        })
    }
}

/// The task a command is about, for the commands whose copy names one: the
/// session id the Lead typed, or for `new` the prompt.
pub fn command_task(command: &Command) -> Option<&str> {
    match command {
        Command::Status { task }
        | Command::Read { task, .. }
        | Command::Diff { task, .. }
        | Command::Send { task, .. }
        | Command::Key { task, .. }
        | Command::Pr { task }
        | Command::Close { task } => Some(task),
        Command::New { prompt, .. } => Some(prompt),
        _ => None,
    }
}

/// The first line of a prompt, shortened, as a name for a task whose own name
/// is not known yet.
pub fn prompt_name(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > NAME_CHARS {
        let cut: String = line.chars().take(NAME_CHARS).collect();
        format!("{}\u{2026}", cut.trim_end())
    } else {
        line
    }
}

/// What a step says while it runs and once it is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepText {
    pub current: String,
    pub done: String,
    /// Only `pr` has one: the author closed the dialog.
    pub cancelled: Option<String>,
}

pub fn step_text(kind: StepKind, task: &str) -> StepText {
    let (current, done) = match kind {
        StepKind::New => (format!("Starting {task}..."), format!("Started {task}")),
        StepKind::Wait => ("Waiting for workers...".into(), "Workers changed".into()),
        StepKind::Check => ("Checking workers...".into(), "Checked workers".into()),
        StepKind::Read => (format!("Reading {task}..."), format!("Read {task}")),
        StepKind::Diff => (
            format!("Checking the diff of {task}..."),
            format!("Checked the diff of {task}"),
        ),
        StepKind::Answer => (format!("Answering {task}..."), format!("Answered {task}")),
        StepKind::Pr => (
            format!("Opening Create PR for {task}..."),
            format!("Opened Create PR for {task}"),
        ),
        StepKind::Close => (format!("Closing {task}..."), format!("Closed {task}")),
        StepKind::Memory => ("Reading its memory...".into(), "Read its memory".into()),
        StepKind::Remember => (
            "Saving a memory note...".into(),
            "Saved a memory note".into(),
        ),
        StepKind::Forget => (
            "Removing a memory note...".into(),
            "Removed a memory note".into(),
        ),
    };
    StepText {
        current,
        done,
        cancelled: (kind == StepKind::Pr).then(|| format!("Create PR cancelled for {task}")),
    }
}

/// How a command ended, for its step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Cancelled,
    /// Refused, failed, or never answered: the step leaves no trace, so a
    /// command that did nothing is not shown as work.
    Dropped,
}

/// The outcome of `reply`, and the task name to finish with when the reply
/// knows a better one than the command did (`new` learns the card's name).
pub fn outcome(kind: StepKind, reply: &Reply) -> (Outcome, Option<String>) {
    match reply {
        Reply::Started { task } => (Outcome::Done, Some(task.title.clone())),
        Reply::Waited {
            events, timed_out, ..
        } if *timed_out && events.is_empty() => (Outcome::Dropped, None),
        Reply::Refused { reason } if kind == StepKind::Pr && reason == CANCELLED => {
            (Outcome::Cancelled, None)
        }
        Reply::Refused { .. } | Reply::Error { .. } => (Outcome::Dropped, None),
        _ => (Outcome::Done, None),
    }
}

struct Running {
    id: u64,
    current: String,
}

/// One Lead's steps for the running turn.
#[derive(Default)]
pub struct StepLog {
    finished: Vec<String>,
    running: Vec<Running>,
    next: u64,
    /// Bumped by [`Self::reset`]: a step from before it finishes into nothing.
    generation: u64,
    version: u64,
    /// The turn the steps belong to.
    turn: Option<Instant>,
}

impl StepLog {
    fn begin(&mut self, current: String) -> (u64, u64) {
        self.next += 1;
        self.version += 1;
        self.running.push(Running {
            id: self.next,
            current,
        });
        (self.next, self.generation)
    }

    /// Ends a running step. `done` is the finished copy, or None to leave
    /// nothing behind. An identical consecutive finished step collapses.
    fn finish(&mut self, id: u64, generation: u64, done: Option<String>) {
        if generation != self.generation {
            return;
        }
        let Some(at) = self.running.iter().position(|step| step.id == id) else {
            return;
        };
        self.running.remove(at);
        self.version += 1;
        if let Some(done) = done
            && self.finished.last() != Some(&done)
        {
            self.finished.push(done);
            let skip = self.finished.len().saturating_sub(FINISHED_KEPT);
            self.finished.drain(..skip);
        }
    }

    /// Forgets everything: a new turn started or the turn settled. Steps
    /// still running finish into nothing.
    fn reset(&mut self) {
        if self.finished.is_empty() && self.running.is_empty() {
            return;
        }
        self.finished.clear();
        self.running.clear();
        self.generation += 1;
        self.version += 1;
    }

    /// Follows the Lead's turn: a different turn than the last seen starts
    /// the list over. The first turn seen keeps what was recorded before the
    /// tick noticed it.
    fn follow_turn(&mut self, turn: Option<Instant>) {
        if turn != self.turn {
            if self.turn.is_some() {
                self.reset();
            }
            self.turn = turn;
        }
    }

    /// The step to show as current: the newest one still running.
    pub fn current(&self) -> Option<&str> {
        self.running.last().map(|step| step.current.as_str())
    }

    pub fn finished(&self) -> &[String] {
        &self.finished
    }

    /// Changes whenever what is shown changes.
    pub fn version(&self) -> u64 {
        self.version
    }
}

pub type SharedSteps = Arc<Mutex<StepLog>>;

fn lock(log: &SharedSteps) -> std::sync::MutexGuard<'_, StepLog> {
    log.lock().unwrap_or_else(|e| e.into_inner())
}

/// A running step. Settle it with the reply that ends its command; if it is
/// dropped unanswered (the client left, the Lead closed) it leaves no trace.
pub struct StepToken {
    log: SharedSteps,
    kind: StepKind,
    task: String,
    id: u64,
    generation: u64,
    settled: AtomicBool,
}

impl StepToken {
    pub fn start(log: &SharedSteps, kind: StepKind, task: &str) -> Self {
        let (id, generation) = lock(log).begin(step_text(kind, task).current);
        Self {
            log: log.clone(),
            kind,
            task: task.to_string(),
            id,
            generation,
            settled: AtomicBool::new(false),
        }
    }

    pub fn settle(&self, reply: &Reply) {
        let (outcome, name) = outcome(self.kind, reply);
        self.end(outcome, name.as_deref());
    }

    fn end(&self, outcome: Outcome, name: Option<&str>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let text = step_text(self.kind, name.unwrap_or(&self.task));
        let done = match outcome {
            Outcome::Done => Some(text.done),
            Outcome::Cancelled => text.cancelled,
            Outcome::Dropped => None,
        };
        lock(&self.log).finish(self.id, self.generation, done);
    }
}

impl Drop for StepToken {
    fn drop(&mut self) {
        self.end(Outcome::Dropped, None);
    }
}

/// Read access for the app: the finished steps, the current one, the version.
pub fn snapshot(log: &SharedSteps) -> (Vec<String>, Option<String>, u64) {
    let log = lock(log);
    (
        log.finished().to_vec(),
        log.current().map(str::to_string),
        log.version(),
    )
}

/// Brings the Lead's list up to date with its turn and returns what to show.
/// `turn` is the Lead's current turn; `settled` is true once it is Ready,
/// Waiting, or exited, which clears the list.
pub fn sync(
    log: &SharedSteps,
    turn: Option<Instant>,
    settled: bool,
) -> (Vec<String>, Option<String>, u64) {
    {
        let mut log = lock(log);
        log.follow_turn(turn);
        if settled {
            log.reset();
        }
    }
    snapshot(log)
}

/// Line two of a live Lead's card: while Working, the current step (or the
/// chat's own line when it replaces the step, such as "Stopping..."), else
/// the first line of its latest reply. None means the CLI and "Lead".
pub fn card_line(
    working: bool,
    current: Option<&str>,
    replaced: Option<&str>,
    reply: Option<&str>,
) -> Option<String> {
    if working {
        Some(replaced.or(current).unwrap_or("Working...").to_string())
    } else {
        reply.map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shika_core::control::{TaskInfo, TaskStatus};

    fn shared() -> SharedSteps {
        Arc::new(Mutex::new(StepLog::default()))
    }

    fn info(title: &str) -> TaskInfo {
        TaskInfo {
            id: "t".into(),
            title: title.into(),
            cli: "claude".into(),
            status: TaskStatus::Working,
            elapsed_secs: None,
            branch: "b".into(),
            diff_stat: None,
            pr: None,
            started_by_lead: true,
            path: "/p".into(),
            report: None,
        }
    }

    #[test]
    fn the_copy_follows_the_table() {
        let text = step_text(StepKind::New, "Fix login");
        assert_eq!(text.current, "Starting Fix login...");
        assert_eq!(text.done, "Started Fix login");
        assert_eq!(text.cancelled, None);
        let wait = step_text(StepKind::Wait, "");
        assert_eq!(wait.current, "Waiting for workers...");
        assert_eq!(wait.done, "Workers changed");
        let pr = step_text(StepKind::Pr, "Fix login");
        assert_eq!(pr.current, "Opening Create PR for Fix login...");
        assert_eq!(pr.done, "Opened Create PR for Fix login");
        assert_eq!(
            pr.cancelled.as_deref(),
            Some("Create PR cancelled for Fix login")
        );
        assert_eq!(
            step_text(StepKind::Diff, "x").current,
            "Checking the diff of x..."
        );
        assert_eq!(step_text(StepKind::Answer, "x").done, "Answered x");
        assert_eq!(step_text(StepKind::Close, "x").done, "Closed x");
        assert_eq!(
            step_text(StepKind::Memory, "").current,
            "Reading its memory..."
        );
        assert_eq!(
            step_text(StepKind::Remember, "").done,
            "Saved a memory note"
        );
        assert_eq!(
            step_text(StepKind::Forget, "").current,
            "Removing a memory note..."
        );
        assert_eq!(step_text(StepKind::Check, "").done, "Checked workers");
    }

    #[test]
    fn help_and_report_have_no_step() {
        assert_eq!(StepKind::of(&Command::Help), None);
        assert_eq!(
            StepKind::of(&Command::Report {
                text: String::new()
            }),
            None
        );
        assert_eq!(StepKind::of(&Command::Tasks), Some(StepKind::Check));
        assert_eq!(
            StepKind::of(&Command::Key {
                task: "t".into(),
                keys: vec![]
            }),
            Some(StepKind::Answer)
        );
    }

    #[test]
    fn a_prompt_name_is_its_first_line_shortened() {
        assert_eq!(prompt_name("\n  Fix  the\tbug  \nmore"), "Fix the bug");
        let long = "a".repeat(60);
        let name = prompt_name(&long);
        assert_eq!(name.chars().count(), NAME_CHARS + 1);
        assert!(name.ends_with('\u{2026}'));
        assert_eq!(prompt_name(""), "");
    }

    #[test]
    fn a_step_runs_then_finishes() {
        let log = shared();
        let token = StepToken::start(&log, StepKind::Read, "Fix login");
        assert_eq!(snapshot(&log).1.as_deref(), Some("Reading Fix login..."));
        token.settle(&Reply::Text { text: "x".into() });
        let (finished, current, _) = snapshot(&log);
        assert_eq!(finished, ["Read Fix login"]);
        assert_eq!(current, None);
    }

    #[test]
    fn new_finishes_with_the_cards_name() {
        let log = shared();
        let token = StepToken::start(&log, StepKind::New, "Fix the…");
        token.settle(&Reply::Started {
            task: info("Fix the login redirect"),
        });
        assert_eq!(snapshot(&log).0, ["Started Fix the login redirect"]);
    }

    #[test]
    fn identical_consecutive_steps_collapse_and_three_are_kept() {
        let log = shared();
        for _ in 0..3 {
            StepToken::start(&log, StepKind::Check, "").settle(&Reply::Tasks { tasks: vec![] });
        }
        assert_eq!(snapshot(&log).0, ["Checked workers"]);
        for task in ["a", "b", "c", "d"] {
            StepToken::start(&log, StepKind::Read, task).settle(&Reply::Text { text: "".into() });
        }
        assert_eq!(snapshot(&log).0, ["Read b", "Read c", "Read d"]);
        // Not consecutive: the same copy appears again.
        StepToken::start(&log, StepKind::Check, "").settle(&Reply::Tasks { tasks: vec![] });
        StepToken::start(&log, StepKind::Read, "d").settle(&Reply::Text { text: "".into() });
        assert_eq!(
            snapshot(&log).0,
            ["Read d", "Checked workers", "Read d"]
                .map(String::from)
                .to_vec()
        );
    }

    #[test]
    fn refused_failed_and_abandoned_commands_leave_no_trace() {
        let log = shared();
        StepToken::start(&log, StepKind::Close, "x").settle(&Reply::Refused {
            reason: "no".into(),
        });
        StepToken::start(&log, StepKind::Read, "x").settle(&Reply::Error {
            message: "no".into(),
        });
        drop(StepToken::start(&log, StepKind::Wait, ""));
        // A wait that timed out with nothing to report is not "Workers changed".
        StepToken::start(&log, StepKind::Wait, "").settle(&Reply::Waited {
            events: vec![],
            still_working: vec![],
            timed_out: true,
        });
        let (finished, current, _) = snapshot(&log);
        assert!(finished.is_empty());
        assert_eq!(current, None);
    }

    #[test]
    fn a_cancelled_pr_says_so_and_a_cancelled_close_does_not() {
        let log = shared();
        let cancelled = Reply::Refused {
            reason: CANCELLED.into(),
        };
        StepToken::start(&log, StepKind::Pr, "Fix login").settle(&cancelled);
        StepToken::start(&log, StepKind::Close, "Fix login").settle(&cancelled);
        assert_eq!(snapshot(&log).0, ["Create PR cancelled for Fix login"]);
        StepToken::start(&log, StepKind::Pr, "Fix login").settle(&Reply::Done {
            message: "url".into(),
        });
        assert_eq!(
            snapshot(&log).0.last().unwrap(),
            "Opened Create PR for Fix login"
        );
    }

    #[test]
    fn the_newest_running_step_is_current() {
        let log = shared();
        let _wait = StepToken::start(&log, StepKind::Wait, "");
        let read = StepToken::start(&log, StepKind::Read, "x");
        assert_eq!(snapshot(&log).1.as_deref(), Some("Reading x..."));
        read.settle(&Reply::Text { text: "".into() });
        assert_eq!(snapshot(&log).1.as_deref(), Some("Waiting for workers..."));
    }

    #[test]
    fn a_new_turn_resets_and_late_replies_do_not_come_back() {
        let log = shared();
        let t0 = Instant::now();
        lock(&log).follow_turn(Some(t0));
        let late = StepToken::start(&log, StepKind::Wait, "");
        StepToken::start(&log, StepKind::Read, "x").settle(&Reply::Text { text: "".into() });
        // The first turn seen keeps earlier steps.
        lock(&log).follow_turn(Some(t0));
        assert_eq!(snapshot(&log).0, ["Read x"]);
        lock(&log).follow_turn(Some(t0 + std::time::Duration::from_secs(5)));
        let (finished, current, _) = snapshot(&log);
        assert!(finished.is_empty() && current.is_none());
        late.settle(&Reply::Waited {
            events: vec![],
            still_working: vec![],
            timed_out: false,
        });
        assert!(snapshot(&log).0.is_empty());
    }

    #[test]
    fn the_version_moves_with_what_is_shown() {
        let log = shared();
        let v0 = snapshot(&log).2;
        let token = StepToken::start(&log, StepKind::Memory, "");
        let v1 = snapshot(&log).2;
        assert_ne!(v0, v1);
        drop(token);
        assert_ne!(v1, snapshot(&log).2);
        lock(&log).reset();
        let v2 = snapshot(&log).2;
        lock(&log).reset();
        assert_eq!(v2, snapshot(&log).2);
    }

    #[test]
    fn card_line_two_follows_the_turn() {
        assert_eq!(
            card_line(true, Some("Reading x..."), None, Some("Done.")).as_deref(),
            Some("Reading x...")
        );
        assert_eq!(
            card_line(true, None, None, Some("Done.")).as_deref(),
            Some("Working...")
        );
        assert_eq!(
            card_line(true, Some("Reading x..."), Some("Stopping..."), None).as_deref(),
            Some("Stopping...")
        );
        assert_eq!(
            card_line(false, None, None, Some("Done.")).as_deref(),
            Some("Done.")
        );
        assert_eq!(card_line(false, Some("x"), None, None), None);
    }
}
