//! Task cards as a Lead sees them: which cards it may see, their state and
//! turn, and the reports workers file about themselves.
use super::{Observed, refused};
use crate::{Card, HostState, Shika, lock, model::Status};
use shika_core::Session;
use shika_core::control::{
    Command, Reply, TaskDiffStat, TaskInfo, TaskStatus, WORKER_HELP, clean_report,
};
use std::time::Instant;

/// A worker's latest `shika report`, tagged with the turn it was made in. A
/// new turn leaves it behind: the Lead sees only the current turn's report.
pub struct WorkerReport {
    pub text: String,
    pub turn: Option<Instant>,
    /// A changed report is a new event even if Ready was already delivered.
    pub received: Instant,
}

impl WorkerReport {
    pub(super) fn record(self, current: &mut Option<Self>) {
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

impl Card {
    /// The state the Lead sees. A prompt that was just handed to the CLI
    /// counts as Working before the tick consumes it, so a `wait` right after
    /// `new` does not find nothing to wait for.
    pub(super) fn control_status(&self) -> TaskStatus {
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
    pub(super) fn task_info(&self, lead_id: &str) -> Option<TaskInfo> {
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

    pub(super) fn observed(&self, lead_id: &str) -> Option<Observed> {
        Some(Observed {
            info: self.task_info(lead_id)?,
            turn: self.control_turn(),
            report_received: self.current_report().map(|report| report.received),
        })
    }

    /// A worker that still occupies one of the Lead's slots.
    pub(super) fn is_live_worker_of(&self, lead_id: &str) -> bool {
        self.lead.is_none()
            && self.started_by.as_deref() == Some(lead_id)
            && !self.discard
            && self.launch_error.is_none()
            && !lock(&self.agent.state).exited
    }
}

impl HostState {
    /// A fast worker can report before the tick consumes its launch/send
    /// submission. Use the epoch Activity will start, not the previous turn.
    /// Input during an active turn retains the existing epoch.
    pub(super) fn report_turn(
        &self,
        activity: &crate::activity::Activity,
        submitted: u64,
    ) -> Option<Instant> {
        if self.submission != submitted
            && matches!(activity.status, Status::Waiting | Status::Ready)
        {
            self.last_submission.or(activity.turn_started)
        } else {
            activity.turn_started
        }
    }
}

impl Shika {
    /// A command from a worker's token: `report` and `help`, nothing else.
    /// The token names the worker, so a worker can only ever report for
    /// itself.
    pub(super) fn handle_worker(&mut self, token: &str, command: Command) -> Reply {
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

    /// The task card with session id `task` in `project`.
    pub(super) fn project_card<'a>(&'a self, project: &'a str, task: &str) -> Option<&'a Card> {
        self.project_tasks(project)
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == task))
    }

    /// The project's task cards: everything but its Lead and its checkout
    /// session. Every command that names or lists tasks goes through here,
    /// so a Lead can never see, read, send to, or close the agent in the
    /// author's checkout.
    pub(super) fn project_tasks<'a>(&'a self, project: &'a str) -> impl Iterator<Item = &'a Card> {
        self.cards.iter().filter(move |card| {
            card.project == project && is_lead_visible(card.lead.is_some(), card.session.as_ref())
        })
    }
}

/// Whether a Lead may see a card of its own project as a task at all: never
/// a Lead card, and never the agent in the author's checkout. A card still
/// starting, with no session yet, is a worktree task.
pub(super) fn is_lead_visible(lead_card: bool, session: Option<&Session>) -> bool {
    !lead_card && !session.is_some_and(Session::is_checkout)
}
