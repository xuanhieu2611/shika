//! The doorbell: when workers a Lead started settle and the Lead is idle,
//! Shika submits one `[shika]` line to it, and after a restart one summary
//! line. Both go through the chat transport or the Lead's terminal.
use super::{LeadState, Ledger, Observed, Settlement, is_settled, paste_steps};
use crate::{Card, Shika, lock, model::Status};
use gpui::{Context, Window};
use shika_core::control::{TaskInfo, TaskStatus};
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

/// Internal chat delivery is committed only after the pipe accepted the message.
pub(crate) enum ChatDelivery {
    Bell(Vec<(String, Settlement)>),
    Restart(crate::RestartSummary),
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
    pub(super) fn open(&self, now: Instant) -> bool {
        self.alive
            && self.idle
            && !self.waiting
            && !self.draft
            && !self
                .last_typed
                .is_some_and(|at| now.duration_since(at) < BELL_TYPING_QUIET)
    }
}

/// A chat Lead is quiet for the doorbell when it is neither sending, replying,
/// failing to save, nor holding queued author messages, which go first.
pub(super) fn chat_quiet(saving: bool, busy: bool, save_failed: bool, queued: usize) -> bool {
    !saving && !busy && !save_failed && queued == 0
}

/// The Lead's [`Gate`] right now.
fn lead_gate(
    card: &Card,
    state: &LeadState,
    chat: Option<&crate::lead_chat::LeadChat>,
    cx: &gpui::App,
) -> Gate {
    let host = lock(&card.agent.state);
    Gate {
        waiting: !state.waiters.is_empty() || !state.unconfirmed.is_empty(),
        idle: !card.creating
            && matches!(card.status, Status::Waiting | Status::Ready)
            && host.submission == card.submitted
            && chat.is_none_or(|chat| {
                chat_quiet(chat.saving, chat.busy, chat.save_failed, chat.queued_len())
            }),
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

/// The event rows a chat Lead shows for a doorbell: one per due worker,
/// with its CLI when titles repeat and a note when only its report changed
/// since the last ring.
fn bell_events(
    due: &[&Observed],
    tasks: &[Observed],
    rung: &Ledger,
) -> Vec<(String, Option<String>)> {
    due.iter()
        .map(|task| {
            let label = match task.info.status {
                TaskStatus::Asking => "is asking you",
                TaskStatus::Exited => "exited",
                _ => "is ready to check",
            };
            let updated = rung
                .reported
                .get(&task.info.id)
                .is_some_and(|(_, turn, receipt)| {
                    *turn == task.turn
                        && *receipt != task.report_received
                        && task.report_received.is_some()
                });
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
        .collect()
}

impl Shika {
    pub(crate) fn chat_delivery_finished(&mut self, index: usize, accepted: bool) {
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

    /// Rings each Lead whose workers settled, when [`Gate`] is open.
    pub(super) fn ring_doorbells(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                self.leads
                    .chats
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
                let events = bell_events(&due, &tasks, &doorbell.rung);
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
    pub(super) fn deliver_restart_summaries(
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
                    self.leads
                        .chats
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
}
