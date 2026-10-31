//! Routing a Lead's command to its handler, the replies shared by every
//! command, and the simple commands (`help`, `memory`, `tasks`, `status`,
//! `read`, `diff`).
use super::{Incoming, Input, LEAD_GUIDE, LEAD_PUBLISHING, MAX_WAIT_SECS, Waiter, lead_guide};
use crate::Shika;
use crate::lead_steps::{self, StepKind, StepToken};
use gpui::{Context, Window};
use shika_core::control::{Command, MAX_READ_LINES, Reply};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

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

/// The way back to a Lead's command: the client's channel, and the activity
/// step the command runs as. Sending the reply settles the step; dropping it
/// unanswered (the client left, the Lead closed) removes the step.
pub struct Responder {
    tx: Sender<Reply>,
    step: Option<StepToken>,
}

impl Responder {
    pub(super) fn new(tx: Sender<Reply>, step: Option<StepToken>) -> Self {
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

pub(super) fn refused(reason: impl Into<String>) -> Reply {
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

impl Shika {
    pub(super) fn handle_control(
        &mut self,
        incoming: Incoming,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// What `pr` and `close` both require of `task`: a worker this Lead
    /// started in its project, set up, and not being published. `verb` is
    /// the action, for the ownership refusal.
    pub(super) fn worker_refusal(
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
}
