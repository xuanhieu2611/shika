//! `shika close`: the one deferred Close dialog a Lead may have waiting on
//! the author ([`LeadDialog`]), and answering it when the dialog ends.
use super::{Responder, refused};
use crate::{Overlay, Selection, Shika};
use gpui::{Context, Window};
use shika_core::control::Reply;
use std::time::{Duration, Instant};

/// The one `close` waiting on the author. Shika holds at most one, because
/// the flow may open a dialog and only one overlay exists at a time. It
/// outlives the dialog just long enough to answer the client and give the
/// selection and focus back to the Lead. `pr` has none: it publishes without
/// a dialog ([`Shika::control_pr`]).
pub struct LeadDialog {
    /// Session id of the Lead that asked.
    pub(crate) lead_id: String,
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

impl Shika {
    /// `shika close`: select the worker's card and run the ordinary Close
    /// flow, which asks the author when Close would. The reply is sent by
    /// [`Self::settle_lead_dialog`] when the flow ends.
    pub(super) fn control_close(
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
    pub(super) fn resume_deferred_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Whether the close dialog for `task` is on screen.
    fn lead_dialog_open(&self, task: &str) -> bool {
        match &self.overlay {
            Some(
                Overlay::Close(crate::close::CloseDialog { index, .. })
                | Overlay::SwitchedClose(crate::close::SwitchedCloseDialog { index, .. }),
            ) => self
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
}
