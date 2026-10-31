//! `shika pr`: publishing a worker without a dialog, its refusals, and the
//! reply.
use super::{Responder, refused};
use crate::{Overlay, Shika};
use gpui::Context;
use shika_core::control::{Reply, TaskStatus};
use std::time::Instant;

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

impl Shika {
    /// `shika pr`: publish a worker without a dialog, as Create PR does when
    /// the author confirms it untouched, and answer with the URL. The
    /// author's selection, focus, and overlays are left alone, and the global
    /// `busy` is not set, so they keep working meanwhile; the card is marked
    /// [`Card::lead_publishing`] instead, which refuses what could race with
    /// it on that task. The `#N` checks mark appears as after any publish.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn control_pr(
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
                Overlay::Publish(publish) => publish.session_id() == task,
                Overlay::SwitchAgent(switch) => switch.session_id() == task,
                Overlay::Close(crate::close::CloseDialog { index, .. })
                | Overlay::SwitchedClose(crate::close::SwitchedCloseDialog { index, .. }) => {
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
}
