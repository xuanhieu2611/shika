//! The `Shika` glue: creating each project's chat, sending, and draining
//! Core chat events into card status, notifications, and questions.
use super::{ChatAction, LeadChat, markdown};
use crate::{Shika, Status};
use gpui::{AppContext, Context, Entity, Window};
use shika_core::{ChatContent, ChatEvent, ChatOrigin, ChatStatus};
use std::time::Instant;

impl Shika {
    pub(crate) fn ensure_lead_chat(
        &mut self,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<LeadChat> {
        if let Some(chat) = self.leads.chats.get(project) {
            return chat.clone();
        }
        let chrome = self.chrome(window);
        let core = self.core.clone();
        let owned = project.to_string();
        let chat = cx.new(|cx| LeadChat::new(owned.clone(), core, chrome, cx));
        cx.subscribe_in(
            &chat,
            window,
            move |this, _, action, window, cx| match action {
                ChatAction::Resume => this.resume_lead(owned.clone(), window, cx),
                ChatAction::Back => window.focus(&this.focus, cx),
                _ => {
                    let Some(index) = this
                        .cards
                        .iter()
                        .position(|c| c.project == owned && c.lead.is_some())
                    else {
                        return;
                    };
                    let Some(id) = this.cards[index].session.as_ref().map(|s| s.id.clone()) else {
                        return;
                    };
                    match action {
                        ChatAction::Send(text) => {
                            this.submit_lead_chat(
                                index,
                                text.clone(),
                                ChatOrigin::Author,
                                Vec::new(),
                                cx,
                            );
                        }
                        ChatAction::Interrupt => this.core.lead_chat_interrupt(&id),
                        ChatAction::RetrySave => this.core.lead_chat_retry_save(&id),
                        _ => {}
                    }
                }
            },
        )
        .detach();
        self.leads.chats.insert(project.into(), chat.clone());
        chat
    }

    pub(crate) fn submit_lead_chat(
        &mut self,
        index: usize,
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.cards[index].session.as_ref().map(|s| s.id.clone()) else {
            return false;
        };
        if self.cards[index].running() {
            return false;
        }
        if let Err(error) = self.core.lead_chat_send(&id, text, origin, events) {
            self.message(error.to_string());
            return false;
        }
        let card = &mut self.cards[index];
        let now = Instant::now();
        let transition = card.activity.advance_chat(Status::Working, true, now);
        let _ = transition;
        card.status = card.activity.status;
        card.since = card.activity.since;
        if let Some(chat) = self.leads.chats.get(&card.project) {
            chat.update(cx, |chat, cx| {
                chat.busy = true;
                chat.activity = "Working...";
                cx.notify();
            });
        }
        cx.notify();
        true
    }

    pub(crate) fn drain_lead_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.show_queued_chat_question(now, window, cx);
        for index in 0..self.cards.len() {
            if !self.cards[index].chat {
                continue;
            }
            let Some(id) = self.cards[index].session.as_ref().map(|s| s.id.clone()) else {
                continue;
            };
            let project = self.cards[index].project.clone();
            for event in self.core.lead_chat_events(&id) {
                if matches!(event, ChatEvent::Accepted | ChatEvent::DeliveryFailed) {
                    self.chat_delivery_finished(index, matches!(event, ChatEvent::Accepted));
                }
                if let ChatEvent::Question(question) = &event {
                    self.open_chat_question(
                        id.clone(),
                        project.clone(),
                        question.clone(),
                        window,
                        cx,
                    );
                }
                if let ChatEvent::QuestionCancelled(request) = &event {
                    self.cancel_chat_question(&id, request, window, cx);
                }
                if matches!(
                    event,
                    ChatEvent::Exit | ChatEvent::Status(ChatStatus::Ready)
                ) {
                    self.drop_chat_questions(&id, window, cx);
                }
                if let ChatEvent::Confirmed(conversation) = &event {
                    self.confirm_chat_conversation(id.clone(), conversation.clone(), cx);
                }
                if matches!(event, ChatEvent::Started) {
                    // A turn the CLI began itself, such as after a background
                    // `shika wait` returned, is a new turn with its own clock
                    // and notification, like one the author submitted.
                    let card = &mut self.cards[index];
                    card.activity
                        .advance_chat(Status::Working, true, Instant::now());
                    card.status = card.activity.status;
                    card.since = card.activity.since;
                }
                if let ChatEvent::Status(status) = event {
                    let status = match status {
                        ChatStatus::Working => Status::Working,
                        ChatStatus::Asking => Status::Asking,
                        ChatStatus::Ready => Status::Ready,
                        ChatStatus::Waiting => Status::Waiting,
                    };
                    self.advance_lead_chat_card(index, status, now, &id, &project);
                }
                if let ChatEvent::Record(record) = &event
                    && let ChatContent::Lead { text } = &record.content
                    && let Some(line) = markdown::reply_first_line(text)
                {
                    self.leads.snippets.insert(project.clone(), line);
                }
                if matches!(event, ChatEvent::Resuming)
                    && let Some(run) = &mut self.cards[index].lead_run
                {
                    run.resumed_at = Some(now);
                }
                if matches!(event, ChatEvent::Exit) {
                    self.lead_chat_exited(index, now);
                }
                if let Some(chat) = self.leads.chats.get(&project) {
                    chat.update(cx, |chat, cx| chat.receive(event, cx));
                }
            }
            self.sync_lead_chat_card(index, &id, &project, now, window, cx);
        }
    }

    /// Opens the oldest queued question once no dialog is open and the
    /// author is quiet, unless its Lead is gone.
    fn show_queued_chat_question(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_none() && !self.leads.questions.is_empty() && self.author_quiet(now, cx)
        {
            let (id, _, question) = self.leads.questions.remove(0);
            if self
                .cards
                .iter()
                .any(|c| c.session.as_ref().is_some_and(|s| s.id == id))
            {
                self.show_chat_question(id, question, window, cx);
            }
        }
    }

    /// The CLI withdrew one question: close its dialog if it shows and drop
    /// it from the queue.
    fn cancel_chat_question(
        &mut self,
        id: &str,
        request: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(&self.overlay,Some(crate::Overlay::LeadQuestion(dialog)) if dialog.session==id && dialog.question.request_id==*request)
        {
            self.overlay = None;
            self.restore_overlay_focus(window, cx);
        }
        self.leads
            .questions
            .retain(|(session, _, q)| session != id || q.request_id != request);
    }

    /// The turn settled or the Lead exited: close any of its question
    /// dialogs and drop its queued questions.
    fn drop_chat_questions(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.overlay,Some(crate::Overlay::LeadQuestion(dialog)) if dialog.session==id)
        {
            self.overlay = None;
            self.restore_overlay_focus(window, cx);
        }
        self.leads.questions.retain(|(session, _, _)| session != id);
    }

    /// Saves the conversation id the CLI confirmed, off the UI thread, and
    /// reloads the projects once it is saved.
    fn confirm_chat_conversation(
        &mut self,
        id: String,
        conversation: String,
        cx: &mut Context<Self>,
    ) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.confirm_lead_chat(&id, &conversation) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    if error != shika_core::Error::UnknownSession {
                        this.message(error.to_string());
                    }
                } else {
                    this.reload_projects();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves a chat Lead's card to `status` and posts its notification when
    /// the turn reaches its first blocker or completion.
    fn advance_lead_chat_card(
        &mut self,
        index: usize,
        status: Status,
        now: Instant,
        id: &str,
        project: &str,
    ) {
        let card = &mut self.cards[index];
        let transition = card.activity.advance_chat(status, false, now);
        card.status = card.activity.status;
        card.since = card.activity.since;
        if transition.notify {
            let name = self
                .projects
                .iter()
                .find(|p| p.id == project)
                .map_or("Shika", |p| p.name.as_str());
            self.notifications.post(
                id,
                name,
                "Lead",
                card.status.label(),
                self.notification_sound,
            );
        }
    }

    /// The chat child exited: mark the agent exited and, unless a resume
    /// just failed, record an unexpected exit that keeps the conversation.
    fn lead_chat_exited(&mut self, index: usize, now: Instant) {
        crate::lock(&self.cards[index].agent.state).exited = true;
        if let Some(run) = &mut self.cards[index].lead_run
            && run
                .resumed_at
                .is_none_or(|at| !crate::model::resume_failed(at, now))
        {
            run.resumed_at = None;
            run.unexpected_exit = true;
            run.resume_exit = Some(crate::model::ResumeExit::Kept);
        }
    }

    /// After draining: a busy chat keeps its card Working or Asking (a
    /// question or Lead dialog open), and the view gets the current chrome
    /// and the matching activity line.
    fn sync_lead_chat_card(
        &mut self,
        index: usize,
        id: &str,
        project: &str,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(chat) = self.leads.chats.get(project).cloned() {
            let dialog = self.lead_dialog.as_ref().is_some_and(|d| d.lead_id == id);
            if chat.read(cx).busy {
                let status = if dialog || chat.read(cx).blocked {
                    Status::Asking
                } else {
                    Status::Working
                };
                self.advance_lead_chat_card(index, status, now, id, project);
            }
            let chrome = self.chrome(window);
            chat.update(cx, |chat, cx| {
                chat.set_chrome(chrome, cx);
                if chat.busy && chat.activity != "Stopping..." {
                    let label = if dialog || chat.blocked {
                        "Waiting for your decision"
                    } else {
                        "Working..."
                    };
                    if chat.activity != label {
                        chat.activity = label;
                        cx.notify();
                    }
                }
            });
        }
    }
}
