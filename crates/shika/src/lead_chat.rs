//! Lead conversation and composer, independent of the worker terminals.
use crate::{
    Chrome, Selection, Shika, Status, appearance,
    chat_input::{ChatInput, InputEvent},
    lead_chat_markdown::{self as markdown, Block},
};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ListAlignment, ListState, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window, div, prelude::FluentBuilder, px,
};
use shika_core::control::TaskStatus;
use shika_core::{ChatContent, ChatEvent, ChatOrigin, ChatRecord, ChatStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// The Lead mark: a pair of antlers over a small head, line art drawn for
/// Shika from the antlers in `design/assets/shika-logo.png`. A single-color
/// mask: GPUI colors it with `text_color`.
pub(crate) const ANTLERS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.2 15.2C6.6 14.6 4.4 11.4 4.6 4"/><path d="M5.1 11C4.2 10.4 3.4 9.4 3 8"/><path d="M8.6 13.4C8 11.4 8.4 8.6 9.4 6.6"/><path d="M13.8 15.2C17.4 14.6 19.6 11.4 19.4 4"/><path d="M18.9 11C19.8 10.4 20.6 9.4 21 8"/><path d="M15.4 13.4C16 11.4 15.6 8.6 14.6 6.6"/><path fill="#000" d="M10 16.4 12 20.2 14 16.4Z"/></svg>"##;

/// The composer's up-arrow.
pub(crate) const SEND_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="M5.5 11.5 12 5l6.5 6.5"/></svg>"##;

/// Counts of the workers this Lead started, by status. The header shows them
/// as one line, most urgent first. A worker whose CLI exited counts as
/// waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerSummary {
    pub asking: usize,
    pub ready: usize,
    pub working: usize,
    pub waiting: usize,
}
impl WorkerSummary {
    /// Counts workers by status. A worker still starting counts as working
    /// and one whose CLI exited as waiting.
    pub fn count(statuses: impl IntoIterator<Item = TaskStatus>) -> Self {
        let mut summary = Self::default();
        for status in statuses {
            match status {
                TaskStatus::Asking => summary.asking += 1,
                TaskStatus::Ready => summary.ready += 1,
                TaskStatus::Starting | TaskStatus::Working => summary.working += 1,
                TaskStatus::Waiting | TaskStatus::Exited => summary.waiting += 1,
            }
        }
        summary
    }
    /// "1 asking · 2 ready", in attention order with zero counts left out, or
    /// "No workers".
    pub fn label(&self) -> String {
        let parts = [
            (self.asking, "asking"),
            (self.ready, "ready"),
            (self.working, "working"),
            (self.waiting, "waiting"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>();
        if parts.is_empty() {
            "No workers".into()
        } else {
            parts.join(" \u{b7} ")
        }
    }
    /// The status of the dot: the most urgent one present, none with no
    /// workers.
    pub fn dot(&self) -> Option<Status> {
        if self.asking > 0 {
            Some(Status::Asking)
        } else if self.ready > 0 {
            Some(Status::Ready)
        } else if self.working > 0 {
            Some(Status::Working)
        } else if self.waiting > 0 {
            Some(Status::Waiting)
        } else {
            None
        }
    }
}

/// The steps shown under the newest bubble while a turn runs: up to three
/// finished ones, then the current one. Memory only, per turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySteps {
    finished: Vec<String>,
    current: Option<String>,
}
impl ActivitySteps {
    /// Replaces the steps. Only the newest three finished ones are kept.
    pub fn set(&mut self, finished: Vec<String>, current: Option<String>) {
        let skip = finished.len().saturating_sub(3);
        self.finished = finished.into_iter().skip(skip).collect();
        self.current = current;
    }
    pub fn clear(&mut self) {
        self.finished.clear();
        self.current = None;
    }
    /// The lines to show, oldest first: finished steps with a leading
    /// check mark (true), then the current line (false). `fallback` is the
    /// current line when no step is running.
    fn lines<'a>(&'a self, fallback: &'a str) -> Vec<(bool, String)> {
        let mut lines = self
            .finished
            .iter()
            .map(|step| (true, format!("\u{2713} {step}")))
            .collect::<Vec<_>>();
        lines.push((
            false,
            self.current.as_deref().unwrap_or(fallback).to_string(),
        ));
        lines
    }
}

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
    /// The activity lines, oldest first, as `(finished, text)`.
    fn activity_lines(&self) -> Vec<(bool, String)> {
        if self.activity == "Working..." {
            self.steps.lines(self.activity)
        } else {
            let mut lines = self.steps.lines(self.activity);
            if let Some(last) = lines.last_mut() {
                last.1 = self.activity.to_string();
            }
            lines
        }
    }
    /// The composer: one raised field with no border (the caret shows focus)
    /// and a round 32px action at its bottom right, Send or Stop. One
    /// element builder for both places it lives (the empty state's center
    /// and the bottom dock), so moving it keeps the same entity and focus.
    fn composer_box(
        &self,
        chrome: &Chrome,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = self.busy;
        let (bg, hover) = if busy || !enabled {
            (chrome.sunken, chrome.sunken)
        } else {
            (chrome.primary_bg, chrome.primary_hover)
        };
        let ring = self.action_focus.is_focused(window);
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = if busy { "Stop esc" } else { "Send \u{21b5}" };
        let clickable = busy || enabled;
        div()
            .flex()
            .items_end()
            .rounded(px(24.))
            .min_h(px(48.))
            .max_h(px(144.))
            .bg(chrome.raised)
            .pl(px(20.))
            .pr(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h(px(48.))
                    .flex()
                    .items_center()
                    .py(px(12.))
                    .pr(px(8.))
                    .child(div().flex_1().min_w_0().child(self.composer.clone())),
            )
            .child(
                div().flex_none().pb(px(8.)).child(
                    div()
                        .id(if busy {
                            "stop-lead-reply"
                        } else {
                            "send-lead-message"
                        })
                        .size(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(bg)
                        .track_focus(&self.action_focus)
                        .role(gpui::Role::Button)
                        .aria_label(if busy { "Stop generating" } else { "Send" })
                        .when(ring, |d| {
                            d.shadow(vec![
                                gpui::BoxShadow::new(px(0.), px(0.), chrome.focus.into())
                                    .spread_radius(px(2.)),
                            ])
                        })
                        .when(clickable, |d| {
                            d.cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        cx.emit(ChatAction::Interrupt);
                                    } else {
                                        this.submit(cx);
                                    }
                                }))
                        })
                        .tooltip(move |_, cx| {
                            cx.new(|_| crate::KeyTip {
                                bg: tip_bg,
                                fg: tip_fg,
                                text: tip.into(),
                            })
                            .into()
                        })
                        .child(if busy {
                            div()
                                .size(px(7.))
                                .rounded(px(2.))
                                .bg(chrome.ink_1)
                                .into_any_element()
                        } else {
                            gpui::svg()
                                .data(SEND_ICON)
                                .size(px(16.))
                                .text_color(if enabled {
                                    chrome.primary_fg
                                } else {
                                    chrome.ink_4
                                })
                                .into_any_element()
                        }),
                ),
            )
            .into_any_element()
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
    fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.live
            || self.busy
            || self.saving
            || self.save_failed
            || self.composer.read(cx).composing()
        {
            return;
        }
        let text = trim_blank_lines(self.composer.read(cx).value());
        if text.trim().is_empty() {
            return;
        }
        self.error = None;
        self.retry = None;
        self.pending = Some(self.composer.read(cx).value().into());
        self.saving = true;
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.newer = false;
        cx.emit(ChatAction::Send(text));
        cx.notify();
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
    fn add_records(&mut self, records: Vec<Message>, prepend: bool, cx: &mut Context<Self>) {
        let mut additions = Vec::new();
        let mut replaced = false;
        for record in records {
            if let Some(old) = self
                .records
                .iter_mut()
                .find(|m| m.record.id == record.record.id)
            {
                if old.record.content != record.record.content {
                    *old = record;
                    replaced = true;
                }
            } else {
                additions.push(record);
            }
        }
        let records = additions;
        if records.is_empty() && !replaced {
            return;
        }
        let follow = self.list.is_following_tail();
        let anchor = self.list.logical_scroll_top();
        let anchor_key = self.rows.get(anchor.item_ix).map(|row| self.row_key(row));
        if prepend {
            self.records.splice(0..0, records);
        } else {
            self.records.extend(records);
        }
        self.records.sort_by_key(|m| m.record.id);
        // Bounded pages in memory; retained SQLite history is never pruned.
        if self.records.len() > 180 {
            if prepend {
                self.records.truncate(180);
                self.newer = true;
            } else if follow {
                let remove = self.records.len() - 180;
                self.records.drain(..remove);
                self.older = true;
            } else {
                self.records.truncate(180);
                self.newer = true;
            }
        }
        self.rebuild_rows();
        self.list.reset(self.rows.len());
        if follow && !prepend {
            self.list.scroll_to_end();
        } else {
            let item_ix = anchor_key
                .and_then(|key| self.rows.iter().position(|row| self.row_key(row) == key))
                .unwrap_or(anchor.item_ix.min(self.rows.len()));
            self.list.scroll_to(gpui::ListOffset {
                item_ix,
                offset_in_item: anchor.offset_in_item,
            });
            if !prepend {
                self.newer = true;
            }
        }
        self.text_cache
            .retain(|(id, _), _| self.records.iter().any(|r| r.record.id == *id));
        cx.notify();
    }
    fn row_key(&self, row: &Row) -> (i64, usize) {
        match row {
            Row::Time { id, .. } => (*id, usize::MAX),
            Row::Event { id, .. } => (*id, usize::MAX - 1),
            Row::Block { message, block, .. } => (self.records[*message].record.id, *block),
        }
    }
    fn newest(&mut self, cx: &mut Context<Self>) {
        self.records.clear();
        self.rows.clear();
        self.text_cache.clear();
        self.width_cache.clear();
        self.list.reset(0);
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.newer = false;
        self.load(None, cx);
    }
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut last_time = 0;
        let mut last_role = None;
        let mut latest_event = HashMap::new();
        for message in &self.records {
            if let ChatContent::Event { key: Some(key), .. } = &message.record.content {
                latest_event.insert(key, message.record.id);
            }
        }
        for (index, message) in self.records.iter().enumerate() {
            let record = &message.record;
            if let ChatContent::Event { key: Some(key), .. } = &record.content
                && latest_event.get(key) != Some(&record.id)
            {
                continue;
            }
            if last_time == 0
                || record.timestamp.saturating_sub(last_time) >= 300
                || local_day(record.timestamp) != local_day(last_time)
            {
                self.rows.push(Row::Time {
                    id: record.id,
                    text: local_time(record.timestamp),
                });
                last_role = None;
            }
            last_time = record.timestamp;
            match &record.content {
                ChatContent::Event { text, .. } => {
                    self.rows.push(Row::Event {
                        id: record.id,
                        text: text.clone(),
                    });
                    last_role = None;
                }
                ChatContent::Author { .. } | ChatContent::Lead { .. } => {
                    let author = matches!(record.content, ChatContent::Author { .. });
                    let gap = if last_role == Some(author) { 6. } else { 20. };
                    for block in 0..message.blocks.len() {
                        self.rows.push(Row::Block {
                            message: index,
                            block,
                            first: block == 0,
                            last: block + 1 == message.blocks.len(),
                            gap,
                        });
                    }
                    last_role = Some(author);
                }
                _ => {}
            }
        }
    }
    fn load(&mut self, before: Option<i64>, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let core = self.core.clone();
        let project = self.project.clone();
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.lead_chat_page(&project, before).map(prepare) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(records) => {
                        this.older = records.len() == 60;
                        this.add_records(records, before.is_some(), cx);
                    }
                    Err(error) => {
                        this.error = Some(error.to_string());
                        this.save_failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn selection_range(&self, key: (i64, usize), length: usize) -> Option<std::ops::Range<usize>> {
        let (a, b) = self.selection?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if key < (a.0, a.1) || key > (b.0, b.1) {
            return None;
        }
        Some(
            if key == (a.0, a.1) {
                a.2.min(length)
            } else {
                0
            }..if key == (b.0, b.1) {
                b.2.min(length)
            } else {
                length
            },
        )
    }
    fn text_selection(&mut self, key: (i64, usize), event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            InputEvent::Select {
                anchor,
                cursor,
                start,
                shift,
            } => {
                if *start {
                    let anchor = if *shift {
                        self.selection
                            .map(|(a, _)| a)
                            .unwrap_or((key.0, key.1, *anchor))
                    } else {
                        (key.0, key.1, *anchor)
                    };
                    self.selection = Some((anchor, (key.0, key.1, *cursor)));
                    self.selecting = true;
                } else if self.selecting
                    && let Some((_, end)) = &mut self.selection
                {
                    *end = (key.0, key.1, *cursor);
                }
            }
            InputEvent::SelectMessage => {
                if let Some(message) = self.records.iter().find(|m| m.record.id == key.0)
                    && let Some(last) = message.blocks.last()
                {
                    self.selection = Some((
                        (key.0, 0, 0),
                        (key.0, message.blocks.len() - 1, last.text.len()),
                    ));
                }
            }
            InputEvent::CopySelection => {
                let mut selected = String::new();
                let mut previous = None;
                for message in &self.records {
                    for (block, item) in message.blocks.iter().enumerate() {
                        if let Some(range) =
                            self.selection_range((message.record.id, block), item.text.len())
                            && !range.is_empty()
                        {
                            if let Some(old) = previous {
                                selected.push_str(if old == message.record.id {
                                    "\n"
                                } else {
                                    "\n\n"
                                });
                            }
                            selected.push_str(&item.text[range]);
                            previous = Some(message.record.id);
                        }
                    }
                }
                if !selected.is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected));
                }
                return;
            }
            InputEvent::Back => {
                cx.emit(ChatAction::Back);
                return;
            }
            InputEvent::Interrupt if self.busy => {
                cx.emit(ChatAction::Interrupt);
                return;
            }
            _ => return,
        }
        for (key, input) in &self.text_cache {
            let range = self.selection_range(*key, input.read(cx).value().len());
            input.update(cx, |input, cx| {
                if input.select_range(range) {
                    cx.notify();
                }
            });
        }
        cx.notify();
    }
    fn row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut chrome = self.chrome;
        chrome.ink_3 = appearance::legible(
            chrome.ink_3,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        match row {
            Row::Time { text, .. } => div()
                .w_full()
                .py(px(12.))
                .text_center()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child(text)
                .into_any_element(),
            Row::Event { text, .. } => div()
                .w_full()
                .py(px(12.))
                .text_center()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child(text)
                .into_any_element(),
            Row::Block {
                message,
                block,
                first,
                last,
                gap,
            } => {
                let message = &self.records[message];
                let item = &message.blocks[block];
                let width = *self
                    .width_cache
                    .entry(message.record.id)
                    .or_insert_with(|| message_width(message, window));
                let author = matches!(message.record.content, ChatContent::Author { .. });
                let key = (message.record.id, block);
                let mut ink = chrome;
                ink.ink_1 = if author {
                    appearance::legible(
                        chrome.primary_fg,
                        chrome.primary_bg,
                        chrome.primary_fg,
                        4.5,
                    )
                } else {
                    appearance::legible(
                        chrome.ink_1,
                        if item.code {
                            chrome.sunken
                        } else {
                            chrome.raised
                        },
                        chrome.ink_1,
                        4.5,
                    )
                };
                let spans = markdown::highlights(item, ink);
                let input = if let Some(input) = self.text_cache.get(&key) {
                    input.clone()
                } else {
                    let input = cx.new(|cx| ChatInput::new(item.text.clone(), ink, cx));
                    cx.subscribe(&input, move |this, _, event, cx| {
                        this.text_selection(key, event, cx)
                    })
                    .detach();
                    self.text_cache.insert(key, input.clone());
                    input
                };
                let selection = self.selection_range(key, item.text.len());
                input.update(cx, |input, cx| {
                    let changed = input.style(ink, true, item.code, spans);
                    input.label(format!(
                        "{}, {}. {}",
                        if author { "You" } else { "Lead" },
                        local_time(message.record.timestamp),
                        if author {
                            "Submitted message"
                        } else {
                            "Completed reply"
                        }
                    ));
                    input.inline_style(
                        item.spans
                            .iter()
                            .filter(|s| s.code)
                            .map(|s| s.range.clone())
                            .collect(),
                        item.spans
                            .iter()
                            .filter_map(|s| {
                                s.link.as_ref().map(|link| (s.range.clone(), link.clone()))
                            })
                            .collect(),
                    );
                    let selected = input.select_range(selection);
                    if changed || selected {
                        cx.notify();
                    }
                });
                // Keep only a bounded set of shaped visible text entities.
                if self.text_cache.len() > 120 {
                    let active = input.entity_id();
                    self.text_cache.retain(|_, v| {
                        v.entity_id() == active || v.focus_handle(cx).is_focused(window)
                    });
                }
                div()
                    .w_full()
                    .flex()
                    .when(author, |d| d.justify_end())
                    .pt(px(if first { gap } else { 0. }))
                    .child(
                        div()
                            .max_w(gpui::relative(0.8))
                            .w(px(width))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(480.))
                                    .px(px(20.))
                                    .pt(px(if first {
                                        12.
                                    } else if author
                                        || item.continuation
                                        || (item.code
                                            && block > 0
                                            && message.blocks[block - 1].code)
                                    {
                                        0.
                                    } else {
                                        3.
                                    }))
                                    .pb(px(if last {
                                        12.
                                    } else if author
                                        || message
                                            .blocks
                                            .get(block + 1)
                                            .is_some_and(|b| b.continuation)
                                        || (item.code
                                            && message
                                                .blocks
                                                .get(block + 1)
                                                .is_some_and(|b| b.code))
                                    {
                                        0.
                                    } else {
                                        3.
                                    }))
                                    .when(first, |d| d.rounded_t(px(22.)))
                                    .when(last, |d| d.rounded_b(px(22.)))
                                    .bg(if author {
                                        chrome.primary_bg
                                    } else {
                                        chrome.raised
                                    })
                                    .child(
                                        div()
                                            .pl(px(item.indent as f32 * 20.))
                                            .when(item.code, |d| {
                                                d.bg(chrome.sunken)
                                                    .px(px(12.))
                                                    .pt(px(
                                                        if block == 0
                                                            || !message.blocks[block - 1].code
                                                        {
                                                            12.
                                                        } else {
                                                            0.
                                                        },
                                                    ))
                                                    .pb(px(
                                                        if !message
                                                            .blocks
                                                            .get(block + 1)
                                                            .is_some_and(|b| b.code)
                                                        {
                                                            12.
                                                        } else {
                                                            0.
                                                        },
                                                    ))
                                                    .when(
                                                        block == 0
                                                            || !message.blocks[block - 1].code,
                                                        |d| d.rounded_t(px(6.)),
                                                    )
                                                    .when(
                                                        !message
                                                            .blocks
                                                            .get(block + 1)
                                                            .is_some_and(|b| b.code),
                                                        |d| d.rounded_b(px(6.)),
                                                    )
                                            })
                                            .child(input),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }
}
impl Render for LeadChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut chrome = self.chrome;
        chrome.ink_3 = appearance::legible(
            chrome.ink_3,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        chrome.ink_2 = appearance::legible(
            chrome.ink_2,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        let view = cx.entity();
        let mut editor_chrome = chrome;
        editor_chrome.ink_1 = appearance::legible(chrome.ink_1, chrome.raised, chrome.ink_1, 4.5);
        editor_chrome.ink_3 = appearance::legible(chrome.ink_3, chrome.raised, chrome.ink_1, 4.5);
        self.composer.update(cx, |input, cx| {
            if input.set_chrome(editor_chrome) {
                cx.notify();
            }
        });
        if self.rows.is_empty() && !self.loading && self.error.is_none() && self.epoch == 0 {
            self.epoch = 1;
            self.load(None, cx);
        }
        let focused = self.focused(window, cx);
        let enabled = self.live && !self.saving && !self.save_failed && self.has_draft(cx);
        let empty = self.rows.is_empty() && !self.loading;
        let welcome = empty && self.live && !self.starting;
        let list = gpui::list(self.list.clone(), move |index, window, cx| {
            view.update(cx, |this, cx| this.row(index, window, cx))
        })
        .flex_1()
        .min_h_0()
        .w_full();
        let composer = self.composer_box(&chrome, enabled, window, cx);
        let hint = focused.then(|| {
            div()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child("ctrl q back to cards")
                .into_any_element()
        });
        let (welcome_composer, dock_composer) = if welcome {
            (Some(composer), None)
        } else if self.live {
            (None, Some(composer))
        } else {
            (None, None)
        };
        let (welcome_hint, dock_hint) = if welcome { (hint, None) } else { (None, hint) };
        let dock_extras = self.error.is_some() || self.retry.is_some() || self.save_failed;
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(20.))
            .pt(px(20.))
            .pb(px(20.))
            .bg(chrome.term_surface)
            .track_focus(&self.focus)
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                if key.key == "enter" && this.action_focus.is_focused(window) {
                    if this.busy {
                        cx.emit(ChatAction::Interrupt);
                    } else if this.live {
                        this.submit(cx);
                    } else {
                        cx.emit(ChatAction::Resume);
                    }
                    cx.stop_propagation();
                    return;
                }
                if key.key == "enter" && this.new_focus.is_focused(window) {
                    this.newest(cx);
                    cx.stop_propagation();
                    return;
                }
                if key.key == "tab" {
                    if key.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
                if key.key == "q" && key.modifiers.control {
                    cx.emit(ChatAction::Back);
                    cx.stop_propagation();
                }
                if key.key == "escape" && this.busy && !this.composer.read(cx).composing() {
                    cx.emit(ChatAction::Interrupt);
                    cx.stop_propagation();
                }
                if this.focus.is_focused(window) {
                    match key.key.as_str() {
                        "home" => this.list.scroll_to(gpui::ListOffset::default()),
                        "end" => this.list.scroll_to_end(),
                        "pageup" | "pagedown" => {
                            let offset = this.list.logical_scroll_top();
                            let step = if key.key == "pageup" {
                                offset.item_ix.saturating_sub(8)
                            } else {
                                (offset.item_ix + 8).min(this.rows.len())
                            };
                            this.list.scroll_to(gpui::ListOffset {
                                item_ix: step,
                                offset_in_item: offset.offset_in_item,
                            });
                        }
                        _ => {}
                    }
                }
                cx.notify();
            }))
            .child(
                div()
                    .max_w(px(640.))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(self.older, |d| {
                        d.child(
                            crate::dialog_button(
                                "older-lead-messages",
                                "Earlier messages",
                                "",
                                &chrome,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load(this.records.first().map(|m| m.record.id), cx)
                            })),
                        )
                    })
                    .when(self.rows.is_empty() && self.loading, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(13.))
                                .text_color(chrome.ink_3)
                                .child("Loading conversation..."),
                        )
                    })
                    .when(empty && !welcome, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(14.))
                                .child(mark_tile(&chrome))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .line_height(px(19.))
                                        .text_color(chrome.ink_3)
                                        .child(if self.starting {
                                            "Starting Lead..."
                                        } else {
                                            "Lead is not running"
                                        }),
                                ),
                        )
                    })
                    .when(welcome, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(14.))
                                .child(mark_tile(&chrome))
                                .child(
                                    div()
                                        .text_size(px(17.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(chrome.ink_2)
                                        .child("What should we work on?"),
                                )
                                .children(welcome_composer.map(|c| div().w_full().child(c)))
                                .children(welcome_hint),
                        )
                    })
                    .when(!self.rows.is_empty(), |d| d.child(list)),
            )
            .when(!welcome || dock_extras, |d| {
                d.child(
                    div()
                        .max_w(px(640.))
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .pt(px(12.))
                        .when(self.busy, |d| {
                            let lines = self.activity_lines();
                            let single = lines.len() == 1;
                            d.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .text_size(px(12.))
                                    .when(single, |d| d.h(px(20.)))
                                    .children(lines.into_iter().map(|(finished, text)| {
                                        div()
                                            .h(px(16.))
                                            .line_height(px(16.))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_color(if finished {
                                                chrome.ink_4
                                            } else {
                                                chrome.ink_3
                                            })
                                            .child(text)
                                    })),
                            )
                        })
                        .when_some(self.error.clone(), |d, error| {
                            d.child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(19.))
                                    .text_color(chrome.ink_2)
                                    .child(error),
                            )
                        })
                        .when_some(self.retry.clone(), |d, text| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .text_color(chrome.ink_2)
                                    .child("Work may already have started.")
                                    .child(
                                        crate::dialog_button(
                                            "retry-lead-message",
                                            "Retry message",
                                            "",
                                            &chrome,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if this.live
                                                    && !this.busy
                                                    && !this.saving
                                                    && !this.save_failed
                                                {
                                                    this.retry = None;
                                                    this.saving = true;
                                                    cx.emit(ChatAction::Send(text.clone()));
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    ),
                            )
                        })
                        .when(self.save_failed, |d| {
                            d.child(
                                crate::dialog_button("retry-chat-save", "Retry save", "", &chrome)
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(ChatAction::RetrySave)),
                                    ),
                            )
                        })
                        .when(self.newer || !self.list.is_following_tail(), |d| {
                            d.child(
                                div().flex().justify_center().child(
                                    crate::dialog_button(
                                        "new-lead-messages",
                                        "New messages",
                                        "",
                                        &chrome,
                                    )
                                    .track_focus(&self.new_focus)
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.newest(cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
                            )
                        })
                        .children(dock_composer)
                        .when(!self.live && !self.starting, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .text_color(chrome.ink_3)
                                    .child("Lead is not running")
                                    .child(
                                        crate::dialog_button(
                                            "resume-chat-lead",
                                            "Resume",
                                            "↵",
                                            &chrome,
                                        )
                                        .track_focus(&self.action_focus)
                                        .on_click(
                                            cx.listener(|_, _, _, cx| cx.emit(ChatAction::Resume)),
                                        ),
                                    ),
                            )
                        })
                        .children(dock_hint),
                )
            })
    }
}

/// The antler mark on a raised tile: the Lead's empty and resting states.
fn mark_tile(chrome: &Chrome) -> gpui::Div {
    div()
        .size(px(52.))
        .flex_none()
        .rounded(px(14.))
        .bg(chrome.raised)
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::svg()
                .data(ANTLERS_ICON)
                .size(px(30.))
                .text_color(chrome.ink_2),
        )
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

fn prepare(records: Vec<ChatRecord>) -> Vec<Message> {
    records
        .into_iter()
        .map(|record| {
            let lead = matches!(record.content, ChatContent::Lead { .. });
            let blocks = Arc::new(markdown::parse(
                record.content.text().unwrap_or_default(),
                lead,
            ));
            Message { record, blocks }
        })
        .collect()
}

// libc-backed local calendar and locale-aware strftime, without adding a
// second date library. No provider timestamps are used for visible messages.
fn local_day(seconds: u64) -> (i32, i32) {
    let value = local_tm(seconds);
    (value.tm_year, value.tm_yday)
}
fn local_tm(seconds: u64) -> libc::tm {
    let time = seconds as libc::time_t;
    let mut value = std::mem::MaybeUninit::zeroed();
    unsafe {
        libc::localtime_r(&time, value.as_mut_ptr());
        value.assume_init()
    }
}
fn local_time(seconds: u64) -> String {
    use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle};
    let formatter = NSDateFormatter::new();
    formatter.setDateStyle(NSDateFormatterStyle::MediumStyle);
    formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    formatter.setDoesRelativeDateFormatting(true);
    let date = NSDate::dateWithTimeIntervalSince1970(seconds as f64);
    formatter.stringFromDate(&date).to_string()
}

fn message_width(message: &Message, window: &mut Window) -> f32 {
    let mut width = 0f32;
    for block in message.blocks.iter() {
        if block.text.chars().count() > 80 {
            return 480.;
        }
        let text: gpui::SharedString = block.text.clone().into();
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(if block.code {
                crate::MONO
            } else {
                crate::UI_FONT
            }),
            color: window.text_style().color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            text,
            px(if block.code { 12.5 } else { 14. }),
            &[run],
            None,
        );
        width = width.max(
            f32::from(line.width()) + if block.code { 24. } else { 0. } + block.indent as f32 * 20.,
        );
        if width >= 440. {
            return 480.;
        }
    }
    (width + 40.).clamp(44., 480.)
}

impl Shika {
    pub(crate) fn ensure_lead_chat(
        &mut self,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<LeadChat> {
        if let Some(chat) = self.lead_chats.get(project) {
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
        self.lead_chats.insert(project.into(), chat.clone());
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
        if let Some(chat) = self.lead_chats.get(&card.project) {
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
        if self.overlay.is_none() && !self.chat_questions.is_empty() && self.author_quiet(now, cx) {
            let (id, _, question) = self.chat_questions.remove(0);
            if self
                .cards
                .iter()
                .any(|c| c.session.as_ref().is_some_and(|s| s.id == id))
            {
                self.show_chat_question(id, question, window, cx);
            }
        }
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
                    if matches!(&self.overlay,Some(crate::Overlay::LeadQuestion(dialog)) if dialog.session==id && dialog.question.request_id==*request)
                    {
                        self.overlay = None;
                        self.restore_overlay_focus(window, cx);
                    }
                    self.chat_questions
                        .retain(|(session, _, q)| session != &id || &q.request_id != request);
                }
                if matches!(
                    event,
                    ChatEvent::Exit | ChatEvent::Status(ChatStatus::Ready)
                ) {
                    if matches!(&self.overlay,Some(crate::Overlay::LeadQuestion(dialog)) if dialog.session==id)
                    {
                        self.overlay = None;
                        self.restore_overlay_focus(window, cx);
                    }
                    self.chat_questions.retain(|(session, _, _)| session != &id);
                }
                if let ChatEvent::Confirmed(conversation) = &event {
                    let core = self.core.clone();
                    let id = id.clone();
                    let conversation = conversation.clone();
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
                    let card = &mut self.cards[index];
                    let status = match status {
                        ChatStatus::Working => Status::Working,
                        ChatStatus::Asking => Status::Asking,
                        ChatStatus::Ready => Status::Ready,
                        ChatStatus::Waiting => Status::Waiting,
                    };
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
                            &id,
                            name,
                            "Lead",
                            card.status.label(),
                            self.notification_sound,
                        );
                    }
                }
                if let ChatEvent::Record(record) = &event
                    && let ChatContent::Lead { text } = &record.content
                    && let Some(line) = crate::lead_chat_markdown::reply_first_line(text)
                {
                    self.lead_snippets.insert(project.clone(), line);
                }
                if matches!(event, ChatEvent::Resuming)
                    && let Some(run) = &mut self.cards[index].lead_run
                {
                    run.resumed_at = Some(now);
                }
                if matches!(event, ChatEvent::Exit) {
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
                if let Some(chat) = self.lead_chats.get(&project) {
                    chat.update(cx, |chat, cx| chat.receive(event, cx));
                }
            }
            if let Some(chat) = self.lead_chats.get(&project) {
                let dialog = self.lead_dialog.as_ref().is_some_and(|d| d.lead_id == id);
                if chat.read(cx).busy {
                    let status = if dialog || chat.read(cx).blocked {
                        Status::Asking
                    } else {
                        Status::Working
                    };
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
                            &id,
                            name,
                            "Lead",
                            card.status.label(),
                            self.notification_sound,
                        );
                    }
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
    pub(crate) fn selected_chat(&self) -> Option<&Entity<LeadChat>> {
        let project = match &*self.selection {
            Some(Selection::Card(i)) if self.cards.get(*i).is_some_and(|c| c.chat) => {
                &self.cards[*i].project
            }
            Some(Selection::Resting(project))
                if self
                    .projects
                    .iter()
                    .find(|p| p.id == *project)
                    .and_then(|p| p.lead.as_ref())
                    .is_some_and(|lead| lead.cli == "claude") =>
            {
                project
            }
            _ => return None,
        };
        self.lead_chats.get(project)
    }
    pub(crate) fn chat_side(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let chat = self.selected_chat()?.clone();
        chat.update(cx, |chat, cx| chat.set_chrome(*chrome, cx));
        let project = self.shown_project()?;
        let project_name = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map_or("", |p| p.name.as_str())
            .to_string();
        let live = self.selected_card().is_some_and(|i| self.cards[i].chat);
        let workers = chat.read(cx).workers;
        let project_click = project.clone();
        let header = div()
            .h(px(crate::BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .pr(px(20.))
            .border_b_1()
            .border_color(chrome.term_line)
            .bg(appearance::with_alpha(
                chrome.term_header,
                chrome.term_header_alpha,
            ))
            .when(self.column.hidden, |d| {
                d.child(
                    div()
                        .pl(Self::title_inset(window))
                        .child(self.title_identity(chrome, cx)),
                )
            })
            .when(!self.column.hidden, |d| d.pl(px(20.)))
            .child(
                div()
                    .size(px(24.))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(chrome.raised)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        gpui::svg()
                            .data(ANTLERS_ICON)
                            .size(px(16.))
                            .text_color(chrome.ink_2),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(chrome.ink_1)
                            .child(project_name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(chrome.ink_3)
                            .child("Lead"),
                    ),
            )
            .when(live, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(chrome.ink_3)
                        .whitespace_nowrap()
                        .when_some(workers.dot(), |d, status| {
                            d.child(div().size(px(6.)).flex_none().rounded_full().bg(
                                match status {
                                    Status::Asking => chrome.asking.dot,
                                    Status::Ready => chrome.ready.dot,
                                    Status::Working => chrome.working.dot,
                                    Status::Waiting => chrome.waiting.dot,
                                },
                            ))
                        })
                        .child(workers.label()),
                )
            })
            .child(
                crate::dialog_button(
                    "lead-chat-header",
                    if live { "Stop Lead" } else { "Resume" },
                    if live { "⌘⇧W" } else { "↵" },
                    chrome,
                )
                .track_focus(&chat.read(cx).header_focus)
                .role(gpui::Role::Button)
                .aria_label(if live { "Stop Lead" } else { "Resume Lead" })
                .on_key_down(cx.listener({
                    let project = project.clone();
                    move |this, event: &gpui::KeyDownEvent, window, cx| match event
                        .keystroke
                        .key
                        .as_str()
                    {
                        "enter" | "space" => {
                            if live {
                                this.close(window, cx);
                            } else {
                                this.resume_lead(project.clone(), window, cx);
                            }
                            cx.stop_propagation();
                        }
                        "tab" => {
                            if event.keystroke.modifiers.shift {
                                window.focus_prev(cx);
                            } else {
                                window.focus_next(cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if live {
                        this.close(window, cx);
                    } else {
                        this.resume_lead(project_click.clone(), window, cx);
                    }
                })),
            );
        Some(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(self.title_drag(header, cx))
                .child(chat)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn summary(asking: usize, ready: usize, working: usize, waiting: usize) -> WorkerSummary {
        WorkerSummary {
            asking,
            ready,
            working,
            waiting,
        }
    }

    #[test]
    fn summary_label_follows_attention_order_and_skips_zeros() {
        assert_eq!(summary(0, 0, 0, 0).label(), "No workers");
        assert_eq!(summary(1, 2, 0, 0).label(), "1 asking \u{b7} 2 ready");
        assert_eq!(summary(0, 0, 2, 0).label(), "2 working");
        assert_eq!(summary(0, 0, 0, 3).label(), "3 waiting");
        assert_eq!(
            summary(1, 1, 1, 1).label(),
            "1 asking \u{b7} 1 ready \u{b7} 1 working \u{b7} 1 waiting"
        );
    }

    #[test]
    fn summary_dot_is_the_most_urgent_status() {
        assert_eq!(summary(0, 0, 0, 0).dot(), None);
        assert_eq!(summary(1, 2, 3, 4).dot(), Some(Status::Asking));
        assert_eq!(summary(0, 2, 3, 4).dot(), Some(Status::Ready));
        assert_eq!(summary(0, 0, 3, 4).dot(), Some(Status::Working));
        assert_eq!(summary(0, 0, 0, 4).dot(), Some(Status::Waiting));
    }

    #[test]
    fn steps_keep_the_newest_three_finished_with_check_marks() {
        let mut steps = ActivitySteps::default();
        assert_eq!(
            steps.lines("Working..."),
            vec![(false, "Working...".into())]
        );
        steps.set(
            ["a", "b", "c", "d"].map(String::from).to_vec(),
            Some("Reading x...".into()),
        );
        assert_eq!(
            steps.lines("Working..."),
            vec![
                (true, "\u{2713} b".to_string()),
                (true, "\u{2713} c".to_string()),
                (true, "\u{2713} d".to_string()),
                (false, "Reading x...".to_string()),
            ]
        );
        steps.clear();
        assert_eq!(
            steps.lines("Stopping..."),
            vec![(false, "Stopping...".into())]
        );
    }

    #[test]
    fn workers_are_counted_by_status_with_exited_as_waiting() {
        use TaskStatus::*;
        assert_eq!(
            WorkerSummary::count([Asking, Ready, Ready, Starting, Working, Waiting, Exited]),
            summary(1, 2, 2, 2)
        );
        assert_eq!(WorkerSummary::count([]), WorkerSummary::default());
    }
}
