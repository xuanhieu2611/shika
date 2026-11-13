//! Lead conversation and composer, independent of the worker terminals.
//! This file holds the `LeadChat` state, its events in from Core and actions
//! out to `Shika`; the children hold the history, rows, view, and glue.
mod composer;
mod glue;
mod history;
pub(crate) mod markdown;
mod progress;
mod question;
mod queue;
#[cfg(test)]
mod queue_tests;
mod rows;
#[cfg(test)]
mod scroll_tests;
mod selection;
mod side;
mod view;

pub(crate) use progress::*;
pub(crate) use question::*;
pub(crate) use queue::{returns_note, warning};

use crate::{
    Chrome,
    chat_input::{ChatInput, InputEvent},
};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, ListAlignment,
    ListState, Window, px,
};
use history::prepare;
use markdown::Block;
use queue::{Lane, Queue, Route};
use shika_core::{ChatContent, ChatEvent, ChatRecord, ChatStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// The Lead mark: a pair of antlers over a small head, line art drawn for
/// Shika from the antlers in `design/assets/shika-logo.png`. A single-color
/// mask: GPUI colors it with `text_color`.
pub(crate) const ANTLERS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.2 15.2C6.6 14.6 4.4 11.4 4.6 4"/><path d="M5.1 11C4.2 10.4 3.4 9.4 3 8"/><path d="M8.6 13.4C8 11.4 8.4 8.6 9.4 6.6"/><path d="M13.8 15.2C17.4 14.6 19.6 11.4 19.4 4"/><path d="M18.9 11C19.8 10.4 20.6 9.4 21 8"/><path d="M15.4 13.4C16 11.4 15.6 8.6 14.6 6.6"/><path fill="#000" d="M10 16.4 12 20.2 14 16.4Z"/></svg>"##;

/// The composer's up-arrow.
pub(crate) const SEND_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="M5.5 11.5 12 5l6.5 6.5"/></svg>"##;

#[derive(Clone)]
pub enum ChatAction {
    Send(String),
    Interrupt,
    Resume,
    Back,
    RetrySave,
}
#[derive(Clone)]
struct Message {
    record: ChatRecord,
    blocks: Arc<Vec<Block>>,
}
#[derive(Clone)]
enum Row {
    Time {
        id: i64,
        text: String,
    },
    Block {
        message: usize,
        block: usize,
        first: bool,
        last: bool,
        gap: f32,
    },
    Event {
        id: i64,
        text: String,
    },
}

type TextPoint = (i64, usize, usize);

pub struct LeadChat {
    project: String,
    core: Arc<shika_core::Core>,
    composer: Entity<ChatInput>,
    /// The composer scrollbar's thumb while the pointer drags it.
    composer_grab: crate::scrollbar::Grab,
    focus: FocusHandle,
    action_focus: FocusHandle,
    header_focus: FocusHandle,
    new_focus: FocusHandle,
    chrome: Chrome,
    records: Vec<Message>,
    rows: Vec<Row>,
    list: ListState,
    text_cache: HashMap<(i64, usize), Entity<ChatInput>>,
    width_cache: HashMap<i64, f32>,
    selection: Option<(TextPoint, TextPoint)>,
    selecting: bool,
    loading: bool,
    older: bool,
    newer: bool,
    pub live: bool,
    pub starting: bool,
    pub busy: bool,
    blocked: bool,
    pub saving: bool,
    pub save_failed: bool,
    pub last_typed: Option<Instant>,
    /// The current line when no step is running, and the line that replaces
    /// the step ("Waiting for your decision", "Stopping...").
    pub activity: &'static str,
    pub steps: ActivitySteps,
    workers: WorkerSummary,
    pending: Option<String>,
    /// Messages the author sent while the Lead was Working. Memory only.
    queue: Queue,
    error: Option<String>,
    retry: Option<String>,
    pub epoch: u64,
}
impl EventEmitter<ChatAction> for LeadChat {}
impl Focusable for LeadChat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}
impl LeadChat {
    fn new(
        project: String,
        core: Arc<shika_core::Core>,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| ChatInput::new(String::new(), chrome, cx));
        cx.subscribe(&composer, |this, _, event, cx| {
            match event {
                InputEvent::Changed => {
                    this.last_typed = Some(Instant::now());
                }
                InputEvent::Submit => this.submit(cx),
                InputEvent::Interrupt if this.busy => cx.emit(ChatAction::Interrupt),
                InputEvent::Back => cx.emit(ChatAction::Back),
                _ => {}
            }
            cx.notify();
        })
        .detach();
        let list = ListState::new(0, ListAlignment::Bottom, px(200.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        let weak = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let older = event.visible_range.start < 3 && event.is_scrolled;
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |this, cx| {
                    if older && this.older {
                        this.load(this.records.first().map(|m| m.record.id), cx);
                    }
                    cx.notify();
                });
            });
        });
        Self {
            project,
            core,
            composer,
            composer_grab: Default::default(),
            focus: cx.focus_handle().tab_stop(true),
            action_focus: cx.focus_handle().tab_stop(true),
            header_focus: cx.focus_handle().tab_stop(true).tab_index(10),
            new_focus: cx.focus_handle().tab_stop(true),
            chrome,
            records: Vec::new(),
            rows: Vec::new(),
            list,
            text_cache: HashMap::new(),
            width_cache: HashMap::new(),
            selection: None,
            selecting: false,
            loading: false,
            older: false,
            newer: false,
            live: false,
            starting: false,
            busy: false,
            blocked: false,
            saving: false,
            save_failed: false,
            last_typed: None,
            activity: "Working...",
            steps: ActivitySteps::default(),
            workers: WorkerSummary::default(),
            pending: None,
            queue: Queue::default(),
            error: None,
            retry: None,
            epoch: 0,
        }
    }
    /// The header's worker summary: the workers this Lead started.
    pub fn set_worker_summary(&mut self, workers: WorkerSummary, cx: &mut Context<Self>) {
        if self.workers != workers {
            self.workers = workers;
            cx.notify();
        }
    }
    /// The steps of the running turn. Steps only show while `activity` is
    /// "Working..."; the blocked and stopping lines replace the current step.
    pub fn set_steps(
        &mut self,
        finished: Vec<String>,
        current: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let before = self.steps.clone();
        self.steps.set(finished, current);
        if self.steps != before {
            cx.notify();
        }
    }
    /// The line that replaces the current step ("Stopping...", "Waiting for
    /// your decision"), or None while the steps show.
    pub fn replaced_line(&self) -> Option<&'static str> {
        (self.activity != "Working...").then_some(self.activity)
    }
    fn set_chrome(&mut self, chrome: Chrome, cx: &mut Context<Self>) {
        if self.chrome != chrome {
            self.chrome = chrome;
            cx.notify();
        }
    }
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.focus_handle(cx).is_focused(window)
            || self.focus.contains_focused(window, cx)
            || self.header_focus.is_focused(window)
            || self.action_focus.is_focused(window)
            || self.new_focus.is_focused(window)
            || self
                .text_cache
                .values()
                .any(|input| input.focus_handle(cx).is_focused(window))
    }
    pub fn owns_focus(&self, focus: &FocusHandle, cx: &App) -> bool {
        &self.composer.focus_handle(cx) == focus
            || &self.focus == focus
            || &self.header_focus == focus
            || &self.action_focus == focus
            || &self.new_focus == focus
            || self
                .text_cache
                .values()
                .any(|input| &input.focus_handle(cx) == focus)
    }
    pub fn has_draft(&self, cx: &App) -> bool {
        !self.composer.read(cx).value().is_empty()
    }
    /// The state that decides where Enter goes and when a queued message may.
    fn lane(&self, cx: &App) -> Lane {
        Lane {
            live: self.live,
            busy: self.busy,
            saving: self.saving,
            save_failed: self.save_failed,
            composing: self.composer.read(cx).composing(),
            queued: !self.queue.is_empty(),
        }
    }
    /// Enter or Send: submits when the Lead is free, queues behind a running
    /// turn or older queued messages, and ignores blank text.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let value = self.composer.read(cx).value().to_string();
        match self.lane(cx).route(&value) {
            Route::Ignore => return,
            Route::Queue => {
                self.queue.push(trim_blank_lines(&value));
                self.composer
                    .update(cx, |input, cx| input.clear_if(&value, cx));
            }
            Route::Send => {
                self.error = None;
                self.retry = None;
                self.pending = Some(value.clone());
                self.saving = true;
                cx.emit(ChatAction::Send(trim_blank_lines(&value)));
            }
        }
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.newer = false;
        cx.notify();
    }
    /// How many messages wait for the Lead.
    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }
    /// The first queued message, when the Lead is free to take it.
    pub fn next_queued(&self, now: Instant, cx: &App) -> Option<String> {
        self.queue.next(&self.lane(cx), now).map(str::to_string)
    }
    /// The first queued message was accepted: it is no longer queued.
    pub fn queued_delivered(&mut self, cx: &mut Context<Self>) {
        self.queue.delivered();
        cx.notify();
    }
    /// The first queued message was refused: it stays queued, first.
    pub fn queued_refused(&mut self, now: Instant) {
        self.queue.refused(now);
    }
    /// Puts the queued messages back into the composer, ahead of its draft.
    pub fn restore_queued(&mut self, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if let Some(text) = self.queue.restore(&draft) {
            self.composer
                .update(cx, |input, cx| input.set_value(text, cx));
            cx.notify();
        }
    }
    pub fn receive(&mut self, event: ChatEvent, cx: &mut Context<Self>) {
        match event {
            ChatEvent::Record(record) => {
                if matches!(record.content, ChatContent::Author { .. })
                    && let Some(text) = self.pending.take()
                {
                    self.composer
                        .update(cx, |input, cx| input.clear_if(&text, cx));
                }
                let epoch = self.epoch;
                cx.spawn(async move |this, cx| {
                    let records = cx
                        .background_executor()
                        .spawn(async move { prepare(vec![record]) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.epoch == epoch {
                            this.add_records(records, false, cx);
                        }
                    });
                })
                .detach();
            }
            ChatEvent::Status(status) => {
                self.saving = false;
                self.blocked = status == ChatStatus::Asking;
                self.busy = matches!(status, ChatStatus::Working | ChatStatus::Asking);
                if !self.busy {
                    self.steps.clear();
                }
                self.activity = if status == ChatStatus::Asking {
                    "Waiting for your decision"
                } else {
                    "Working..."
                };
            }
            ChatEvent::Resuming => {}
            ChatEvent::Stopping => {
                self.busy = true;
                self.activity = "Stopping...";
            }
            ChatEvent::SaveFailed => {
                self.saving = false;
                self.save_failed = true;
                self.error = Some("Could not save this conversation".into());
            }
            ChatEvent::Rejected(text) => {
                self.saving = false;
                self.retry = Some(text);
            }
            ChatEvent::Saved => {
                self.saving = false;
                self.save_failed = false;
                self.error = None;
            }
            ChatEvent::Error(text) => {
                self.saving = false;
                self.error = Some(text);
            }
            ChatEvent::Exit => {
                self.saving = false;
                self.live = false;
                self.busy = false;
                self.restore_queued(cx);
                self.error = Some("The Lead stopped unexpectedly".into());
            }
            ChatEvent::Confirmed(_)
            | ChatEvent::Accepted
            | ChatEvent::Started
            | ChatEvent::Question(_)
            | ChatEvent::QuestionCancelled(_) => {}
            ChatEvent::DeliveryFailed => {
                self.saving = false;
            }
        }
        cx.notify();
    }
}
fn trim_blank_lines(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |i| i + 1);
    lines[start..end].join("\n")
}
