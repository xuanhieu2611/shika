//! Keeping each Lead's activity steps, worker count, and card reply line in
//! sync with its turn, every tick.
use crate::lead_chat::WorkerSummary;
use crate::lead_steps;
use crate::{Card, Shika};
use gpui::Context;
use shika_core::control::TaskStatus;

impl Shika {
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
            if chat && let Some(view) = self.leads.chats.get(&project) {
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
        self.leads.snippets.retain(|id, _| saved.contains(id));
        self.leads
            .snippets_requested
            .retain(|id| saved.contains(id));
        for id in saved {
            if !self.leads.snippets_requested.insert(id.clone()) {
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
                        && this.leads.snippets_requested.contains(&id)
                    {
                        this.leads.snippets.entry(id).or_insert(line);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }
}
