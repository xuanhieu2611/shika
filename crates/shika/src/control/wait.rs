//! `shika wait`: which settlements a wait reports, the ledger of what was
//! already reported, and resolving pending waits from the tick.
use super::{Responder, refused};
use crate::Shika;
use shika_core::control::{Reply, TaskInfo, TaskStatus, WaitEvent};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// The longest a single `wait` blocks, whatever the client asked for.
pub const MAX_WAIT_SECS: u64 = 600;

/// A task as `wait` sees it: its state and which turn that state belongs to.
pub struct Observed {
    pub info: TaskInfo,
    pub turn: Option<Instant>,
    pub report_received: Option<Instant>,
}

impl Observed {
    pub(super) fn settlement(&self) -> Settlement {
        (self.info.status, self.turn, self.report_received)
    }
}

/// A settlement already reported to the Lead: status, turn, and current report
/// receipt. A new turn or changed report is a new event, so a report arriving
/// after a delivered Ready event cannot disappear. Identical retries keep the
/// receipt, so they do not repeatedly wake the Lead.
pub(super) type Settlement = (TaskStatus, Option<Instant>, Option<Instant>);

#[derive(Default)]
pub struct Ledger {
    pub(super) reported: HashMap<String, Settlement>,
}

impl Ledger {
    pub(super) fn is_reported(&self, task: &Observed) -> bool {
        self.reported.get(&task.info.id) == Some(&task.settlement())
    }

    pub(super) fn mark(&mut self, marks: Vec<(String, Settlement)>) {
        self.reported.extend(marks);
    }

    pub(super) fn forget(&mut self, id: &str) {
        self.reported.remove(id);
    }
}

pub(super) fn is_settled(status: TaskStatus) -> bool {
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
    pub(super) reply: Reply,
    pub(super) marks: Vec<(String, Settlement)>,
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
pub(super) struct Unconfirmed {
    pub(super) delivered: Receiver<bool>,
    pub(super) marks: Vec<(String, Settlement)>,
}

/// Applies the confirmations that arrived: a written reply marks its
/// settlements reported, a failed or abandoned one drops them. Replies still
/// in flight stay pending.
pub(super) fn apply_confirmations(ledger: &mut Ledger, pending: &mut Vec<Unconfirmed>) {
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

/// A `wait` that has not been answered yet.
pub(super) struct Waiter {
    /// The tasks named on the command line; empty means every worker the
    /// Lead started.
    pub(super) named: Vec<String>,
    pub(super) deadline: Instant,
    pub(super) reply: Responder,
    pub(super) delivered: Receiver<bool>,
}

impl Waiter {
    /// The connection thread drops this channel when the client leaves.
    /// An abandoned wait must not keep the doorbell gate closed.
    pub(super) fn connected(&self) -> bool {
        matches!(self.delivered.try_recv(), Err(TryRecvError::Empty))
    }
}

impl Shika {
    /// `shika wait`: queue a [`Waiter`] on the Lead's card, or refuse it.
    pub(super) fn control_wait(
        &mut self,
        lead: usize,
        lead_id: &str,
        tasks: Vec<String>,
        timeout_secs: u64,
        reply: Responder,
        delivered: Receiver<bool>,
    ) {
        if let Some(refusal) = self.wait_refusal(&self.cards[lead].project, lead_id, &tasks) {
            let _ = reply.send(refusal);
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.min(MAX_WAIT_SECS));
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
    pub(super) fn wait_set(&self, lead: usize, named: &[String]) -> Vec<Observed> {
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
    pub(super) fn resolve_waiters(&mut self, now: Instant) {
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
}
