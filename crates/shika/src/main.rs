mod activity;
mod agent_marks;
mod appearance;
mod background;
mod background_view;
mod changes;
mod chat_input;
mod checks;
mod control;
mod control_client;
mod lead_chat;
mod lead_chat_markdown;
mod lead_chat_question;
mod lead_steps;
mod lifecycle;
mod model;
mod name_input;
mod notifications;
mod project_motion;
mod scrollbar;
mod shortcuts;
mod text_edit;
mod updates;
mod usage_dashboard;
mod worktree_setup;

use appearance::{Chrome, with_alpha};
use gpui::{
    AnimationExt, App, AppContext, Bounds, BoxShadow, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Render, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use model::{PromptCapture, Status, TitleWatch};
use notifications::Notifications;
use shika_core::{
    AgentSettings, Appearance, CliCatalog, CliPreset, Column, Core, DiffStat, FontSize,
    JournalEntry, KeyOverrides, KnownBranches, LaunchOptions, LeadLaunch, PreparationConfig,
    PreparationControl, PreparationEvent, Project, ProjectBase, PtyEvent, PtyId, PtySize,
    PublishPreview, Session, SessionGitState, Settings, ThemeMode, ThemeSettings, Translucency,
    WorkerEnv,
};
use shika_terminal::{
    InputSource, Palette, PtyHost, Terminal, TerminalConfig, TerminalEvent, TerminalOptions,
    TerminalSize, TerminalView, Theme, openable_uri,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use usage_dashboard::{UsageFilter, UsageRange, UsageView};

gpui::actions!(
    shika,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        OpenSettings,
        CheckForUpdates,
        NewAgent,
        NewTerminal,
        NewLead,
        CreatePr,
        SwitchAgent,
        CloseTerminal,
        CloseTask,
        RenameTask,
        NextTerminal,
        PreviousTerminal,
        NextAgent,
        PreviousAgent,
        ToggleColumn,
        ToggleChanges,
        ToggleUsage
    ]
);

/// The prompt for a same-worktree switch. Empty starts the CLI with no
/// positional argument. A non-empty prompt is that argument; Core rejects
/// one the CLI would misread.
fn switch_prompt(prompt: &str) -> Option<&str> {
    (!prompt.trim().is_empty()).then_some(prompt)
}

/// Arms the new agent host after a switch. `name` is true while the task is
/// still on its draft branch: then naming works as it does after New, from
/// the prompt or the first typed line. Otherwise nothing names the task. An
/// empty prompt leaves activity Waiting, so the card stays idle until the
/// author types. A prompt counts as a submission, and the returned activity
/// is already in that launch turn.
fn apply_switch_turn(
    host: &mut HostState,
    prompt: Option<&str>,
    name: bool,
    now: Instant,
) -> activity::Activity {
    let mut activity = activity::Activity::new(now);
    if let Some(prompt) = prompt {
        host.seed_launch_prompt(prompt, name, now);
        host.launch_turn = false;
        activity.hold_for_launch();
        let _ = activity.advance(
            activity::Signal::Unknown,
            host.last_submission,
            None,
            now,
            false,
        );
    } else if !name {
        host.prompt.finish();
    }
    activity
}

/// The drag on the agent column's edge. It draws nothing: the column itself
/// follows the pointer.
struct ColumnDrag;
impl Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Jump to a task-local tab. Zero is the pinned agent, so Cmd+1 selects it
/// until Settings points that chord at an agent.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = shika, no_json)]
struct SelectTerminal(usize);

/// Select the nth card in the column, skipping project headers. Zero is the
/// first card. A missing card does nothing.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = shika, no_json)]
struct SelectAgent(usize);

/// Agent traversal follows row order and skips project headers. A resting
/// Lead is an agent row. `j` / `k` and Cmd+] / Cmd+[ both use it.
fn adjacent_agent(
    rows: &[Selection],
    selection: Option<&Selection>,
    delta: isize,
) -> Option<Selection> {
    let at = selection.and_then(|selected| rows.iter().position(|row| row == selected));
    for step in 1..=rows.len() {
        let index = match at {
            Some(at) => {
                (at as isize + delta * step as isize).rem_euclid(rows.len() as isize) as usize
            }
            None if delta > 0 => step - 1,
            None => rows.len() - step,
        };
        if !matches!(rows[index], Selection::Project(_)) {
            return Some(rows[index].clone());
        }
    }
    None
}

/// The nth agent in column order, skipping project headers. None when the
/// column does not have that many.
fn nth_agent(rows: &[Selection], n: usize) -> Option<Selection> {
    rows.iter()
        .filter(|row| !matches!(row, Selection::Project(_)))
        .nth(n)
        .cloned()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Default)]
struct HostState {
    pty: Option<PtyId>,
    measured: Option<TerminalSize>,
    prompt: PromptCapture,
    submission_capture: PromptCapture,
    submission_recalled: bool,
    title: Option<String>,
    last_output: Option<Instant>,
    /// Focus/mouse reports and resize, separate from draft typing so real
    /// transcript changes are not suppressed while the user edits a prompt.
    last_interaction: Option<Instant>,
    /// The user's last key or paste.
    last_typed: Option<Instant>,
    exited: bool,
    submission: u64,
    last_submission: Option<Instant>,
    /// The latest submission is the prompt the CLI was launched with. The
    /// tick hands that to the activity clock once.
    launch_turn: bool,
    lifecycle: Option<shika_core::AgentActivity>,
    lifecycle_checking: bool,
    pending_input: Vec<Vec<u8>>,
    preparing: bool,
    agent_starting: bool,
    preparation_stage: String,
}
struct Host {
    core: Arc<Core>,
    state: Arc<Mutex<HostState>>,
    capture: bool,
}
impl PtyHost for Host {
    fn write(&self, bytes: &[u8], source: InputSource) {
        let mut s = lock(&self.state);
        // Setup is non-interactive. Never queue installer input and later send
        // it as an unintended prompt to the agent.
        if s.preparing && !(s.agent_starting && source == InputSource::Reply) {
            return;
        }
        let now = Instant::now();
        if source == InputSource::Typed {
            s.last_typed = Some(now);
        }
        if source == InputSource::Report {
            s.last_interaction = Some(now);
        }
        if self.capture && source == InputSource::Typed {
            s.capture_typed(bytes, now);
        }
        if let Some(pty) = s.pty {
            let _ = self.core.write(pty, bytes);
        } else {
            // Engine replies can arrive before Core has registered its PTY.
            // Keep their order, along with any input typed during startup.
            s.pending_input.push(bytes.to_vec());
        }
    }
    fn resize(&self, size: TerminalSize) {
        let mut s = lock(&self.state);
        s.measured = Some(size);
        let now = Instant::now();
        s.last_interaction = Some(now);
        if let Some(pty) = s.pty {
            let _ = self.core.resize(pty, PtySize::new(size.rows, size.cols));
        }
    }
}
impl HostState {
    /// Keep the agent's fixed title without seeding a turn. Leads call this
    /// before any startup typeahead or injected summary can be captured.
    fn keep_title(&mut self) {
        self.prompt.finish();
    }

    /// Feeds typed bytes to the naming and submission capture. A nonempty
    /// submitted line is a candidate turn; embedded paste newlines, reports,
    /// and query replies are not.
    fn capture_typed(&mut self, bytes: &[u8], now: Instant) {
        if let Some(title) = self.prompt.feed(bytes) {
            self.title = Some(title);
        }
        if matches!(bytes, b"\x1b[A" | b"\x1bOA" | b"\x1b[B" | b"\x1bOB") {
            // History lives inside the CLI editor, not in captured keys.
            // Treat a subsequent Enter as a candidate accepted prompt.
            self.submission_recalled = true;
        }
        if matches!(bytes, b"\x15" | b"\x03") {
            self.submission_recalled = false;
        }
        let recalled_enter = self.submission_recalled && matches!(bytes, b"\r" | b"\n" | b"\r\n");
        if self.submission_capture.feed(bytes).is_some() || recalled_enter {
            self.submission += 1;
            self.last_submission = Some(now);
            self.submission_capture = PromptCapture::default();
            self.submission_recalled = false;
        }
    }
    /// Typed input that has not been submitted: text on the line, or a
    /// recalled history entry. A CLI's grey suggestion is not typed, so it
    /// does not count.
    fn has_draft(&self) -> bool {
        self.submission_capture.has_text() || self.submission_recalled
    }
    /// Bytes are arrival metadata, not proof of work. The activity sampler
    /// separately compares live transcript content, excluding the editor.
    fn note_output(&mut self, now: Instant) {
        self.last_output = Some(now);
    }
    fn note_lifecycle(&mut self, generation: u64, report: Option<shika_core::AgentActivity>) {
        // A read scheduled for the old turn cannot inject its Idle into a
        // newly submitted prompt, even if the filesystem read was delayed.
        if self.submission == generation {
            self.lifecycle = report;
        }
        self.lifecycle_checking = false;
    }
}
/// Feeds an agent PTY into its terminal and records output and exit on its
/// host, for New and Switch agent alike.
fn agent_sink(terminal: Terminal, state: Arc<Mutex<HostState>>) -> impl shika_core::PtySink {
    move |_, event| match event {
        PtyEvent::Output(bytes) => {
            terminal.feed(&bytes);
            lock(&state).note_output(Instant::now());
        }
        PtyEvent::Exit(exit) => {
            terminal.feed(format!("\r\n[process exited with code {}]\r\n", exit.code).as_bytes());
            let mut state = lock(&state);
            state.last_output = Some(Instant::now());
            state.exited = true;
        }
    }
}
fn bind_host(core: &Core, state: &Mutex<HostState>, pty: PtyId) {
    let mut host = lock(state);
    host.pty = Some(pty);
    if let Some(size) = host.measured {
        let _ = core.resize(pty, PtySize::new(size.rows, size.cols));
    }
    for bytes in host.pending_input.drain(..) {
        let _ = core.write(pty, &bytes);
    }
}
struct Pane {
    shell_number: usize,
    view: Entity<TerminalView>,
    terminal: Terminal,
    state: Arc<Mutex<HostState>>,
}
impl Pane {
    fn new(
        core: Arc<Core>,
        capture: bool,
        opacity: f32,
        font_size: f32,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Shika>,
    ) -> Self {
        let state = Arc::new(Mutex::new(HostState::default()));
        let terminal = Terminal::new(
            TerminalOptions {
                size: TerminalSize::new(2, 2),
                ..Default::default()
            },
            Host {
                core,
                state: state.clone(),
                capture,
            },
        );
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(
                terminal.clone(),
                TerminalConfig {
                    font_size: px(font_size),
                    ..TerminalConfig::default()
                },
                palette,
                window,
                cx,
            );
            view.set_background_opacity(opacity, cx);
            view
        });
        cx.subscribe(&view, |this, _, event, cx| {
            if let TerminalEvent::OpenLink(uri) = event
                && let Err(text) = open_terminal_link(uri)
            {
                this.message(text);
                cx.notify();
            }
        })
        .detach();
        Self {
            shell_number: 0,
            view,
            terminal,
            state,
        }
    }
}
fn open_terminal_link(uri: &str) -> Result<(), String> {
    if !openable_uri(uri) {
        return Err("Couldn't open link".to_string());
    }
    let mut child = std::process::Command::new("/usr/bin/open")
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Couldn't open link".to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// What a card's launch starts. A retry repeats it.
#[derive(Clone)]
enum Launch {
    /// New from the picker: nothing typed yet.
    Task,
    /// `shika new`: the CLI starts with the Lead's prompt and the card is
    /// not selected, because the author is typing elsewhere.
    Worker(LaunchOptions),
    /// The project's Lead, started by the author.
    Lead(LeadLaunch),
    /// The project's Lead, started again by the app after its resume was
    /// refused. Not an author launch: it closes no overlay, takes no busy
    /// hold, and the selection and focus only follow the old Lead's.
    /// `focus`: the old Lead's terminal had focus.
    LeadRestart { launch: LeadLaunch, focus: bool },
}

struct Card {
    session: Option<Session>,
    project: String,
    title: String,
    preset: String,
    status: Status,
    since: Instant,
    activity: activity::Activity,
    evidence: activity::OutputEvidence,
    lifecycle: lifecycle::Lifecycle,
    lifecycle_checked: Option<Instant>,
    /// Unbound host/identity pane for a chat card; never receives a PTY or CLI bytes.
    agent: Pane,
    chat: bool,
    chat_delivery: Option<control::ChatDelivery>,
    shells: Vec<Pane>,
    /// Zero is the pinned agent tab; shells use their index plus one.
    active_tab: usize,
    shell_serial: usize,
    tab_scroll: gpui::ScrollHandle,
    /// The tab under the pointer, which paints its hover in the tab shape.
    hovered_tab: Option<usize>,
    submitted: u64,
    creating: bool,
    title_watch: TitleWatch,
    /// The prompt or the CLI's title has named the card. Until then it reads
    /// "New <CLI>" in a lighter ink.
    named: bool,
    /// What the task changed, fetched each time the card turns Ready.
    diff: Option<DiffStat>,
    /// The checks on the PR that Create PR or a Lead's `shika pr` made or
    /// reused. Memory only.
    pr: Option<checks::PrWatch>,
    /// A Lead's `shika pr` is publishing this worker. Create PR, Close,
    /// Switch agent, and the Lead's own commands on it wait until it ends
    /// ([`control::LEAD_PUBLISHING`]). Memory only.
    lead_publishing: bool,
    /// The `since` of the Ready turn the user has seen. Every turn that ends
    /// gets a new `since`, so its dot returns until it is seen.
    seen: Option<Instant>,
    launch_preset: String,
    launch: Launch,
    /// The Lead session that started this worker.
    started_by: Option<String>,
    /// The control socket state, on the project's Lead card only.
    lead: Option<control::LeadState>,
    /// The `SHIKA_TOKEN` of a Lead-started worker's agent PTY. It may run
    /// only `shika report`, and dies with the card.
    worker_token: Option<String>,
    /// The worker's latest `shika report`. Memory only.
    report: Option<control::WorkerReport>,
    /// A failed launch nobody asked for (a worker): removed by the tick once
    /// no dialog depends on card positions.
    discard: bool,
    launch_control: Option<PreparationControl>,
    launch_error: Option<String>,
    stage: String,
    /// Close is stopping its terminals and removing its worktree: the card
    /// dims and says so, and its terminal fades behind "Closing...".
    closing: bool,
    /// Switch agent's replacement terminal while its process starts. The
    /// tick leaves the card alone meanwhile, so the old agent's exit is not
    /// read as a finished turn.
    next_agent: Option<Pane>,
    /// The CLI exit was already handed to the token read. A later Ready
    /// still reads on its own.
    usage_exit_seen: bool,
    /// What the app tracks for a Lead beyond the card, once it has started.
    lead_run: Option<LeadRun>,
}
/// What stopping a Lead (and forgetting it) came to.
enum LeadCloseOutcome {
    /// Stopped, and forgotten if that was asked.
    Done,
    /// The Lead could not be stopped: it is still live.
    NotStopped(shika_core::Error),
    /// Stopped, but its worktree changed since the dialog opened.
    Changed(shika_core::LeadLoss),
    /// Stopped, but Forget failed.
    ForgetFailed(shika_core::Error),
}
/// What the app tracks for a started Lead: what core said about the start
/// and what is still to be done about it. Memory only.
struct LeadRun {
    /// When a launch that resumed the saved conversation completed. A CLI
    /// that exits within [`model::RESUME_GRACE`] refused the saved id.
    resumed_at: Option<Instant>,
    unexpected_exit: bool,
    /// What became of a resumed Lead that exited early (see
    /// [`model::ResumeExit`]), until the app has acted on it.
    resume_exit: Option<model::ResumeExit>,
    /// Looking for the conversation id the CLI picks (Codex).
    discovery: Option<model::Discovery>,
    /// The restart summary, until it has been typed into the Lead.
    summary: Option<RestartSummary>,
}
/// The `[shika]` line for a restarted Lead and the lost workers it names.
#[derive(Clone)]
struct RestartSummary {
    line: String,
    workers: Vec<std::path::PathBuf>,
}
impl LeadRun {
    fn new(start: &shika_core::LeadStart, now: Instant) -> Self {
        let resumed = start.conversation == shika_core::LeadConversation::Resumed;
        let branches = start
            .lost_workers
            .iter()
            .map(|worker| worker.branch.clone())
            .collect::<Vec<_>>();
        Self {
            resumed_at: resumed.then_some(now),
            unexpected_exit: false,
            resume_exit: None,
            // Codex picks its own conversation id, and only once the first
            // message is submitted.
            discovery: (start.session.preset_id == "codex" && !resumed)
                .then(|| model::Discovery::new(start.started_at, now)),
            summary: model::restart_summary(
                &branches,
                &start.worktree,
                start.session.base_ref.as_deref().unwrap_or_default(),
            )
            .map(|line| RestartSummary {
                line,
                workers: start
                    .lost_workers
                    .iter()
                    .map(|worker| worker.path.clone())
                    .collect(),
            }),
        }
    }
}
/// The column group a card was shown in, by project id: the Leads section
/// (a Lead and its workers) or the project's own cards.
#[derive(Clone, PartialEq, Eq)]
enum Home {
    Leads(String),
    Project(String),
}
/// A closed card on its way out. It is no longer in `cards`, so nothing can
/// select, count, or act on it; it only paints until its exit has played.
struct Departing {
    card: Card,
    home: Home,
    /// Drawn before this card, the one that followed it in its group when it
    /// closed. `None` puts it at the end of its group.
    before: Option<gpui::EntityId>,
    key: u64,
}
impl Card {
    fn unseen(&self) -> bool {
        matches!(self.status, Status::Ready | Status::Asking) && self.seen != Some(self.since)
    }
    fn running(&self) -> bool {
        let host = lock(&self.agent.state);
        model::activity_requires_confirmation(
            self.status,
            host.exited,
            host.submission != self.submitted,
        )
    }
    /// Rename task is offered for a started task, never for a Lead.
    fn renamable(&self) -> bool {
        self.session.is_some() && self.lead.is_none()
    }
    fn active_pane(&self) -> &Pane {
        self.active_tab
            .checked_sub(1)
            .and_then(|index| self.shells.get(index))
            .unwrap_or(&self.agent)
    }
    /// Scrolls the active tab into view. The strip's children are the lead
    /// space, the tabs, and the trailing space, so the first and last tabs
    /// reveal the space that holds their outer flare.
    fn reveal_active_tab(&self) {
        let item = match self.active_tab {
            0 => 0,
            tab if tab == self.shells.len() => tab + 2,
            tab => tab + 1,
        };
        self.tab_scroll.scroll_to_item(item);
    }
}
impl Drop for Card {
    fn drop(&mut self) {
        if let Some(control) = &self.launch_control {
            control.cancel_preserving_worktree();
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
enum Selection {
    Project(String),
    Card(usize),
    /// The saved, stopped Lead of this project. It has no card.
    Resting(String),
}
enum CloseCheck {
    Normal(SessionGitState),
    Switched(shika_core::SwitchedBranchClose),
}

/// A context menu stays bound to the clicked card, not the current selection
/// or a vector index that asynchronous setup cleanup could invalidate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CardMenuAction {
    Rename,
    Close,
}
struct CardMenu {
    target: gpui::EntityId,
    position: gpui::Point<Pixels>,
    action: CardMenuAction,
}
impl CardMenu {
    fn target_index(&self, mut ids: impl Iterator<Item = gpui::EntityId>) -> Option<usize> {
        ids.position(|id| id == self.target)
    }
}

/// Settings sections, top to bottom in the list on the left.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsSection {
    Appearance,
    Keyboard,
    Agents,
    Projects,
    Usage,
}

impl SettingsSection {
    const ALL: [SettingsSection; 5] = [
        SettingsSection::Appearance,
        SettingsSection::Keyboard,
        SettingsSection::Agents,
        SettingsSection::Projects,
        SettingsSection::Usage,
    ];

    /// One section up (`-1`) or down (`1`), wrapping.
    fn step(self, delta: i64) -> Self {
        let at = Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0) as i64;
        let len = Self::ALL.len() as i64;
        Self::ALL[(at + delta).rem_euclid(len) as usize]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Keyboard => "Keyboard",
            Self::Agents => "Agents",
            Self::Projects => "Projects",
            Self::Usage => "Usage",
        }
    }
}

enum Overlay {
    LeadQuestion(lead_chat_question::QuestionDialog),
    WorktreeSetup(Box<worktree_setup::Editor>),
    /// Frame the background image, opened from Settings, which it returns to.
    Adjust(Box<background_view::Adjust>),
    CardMenu(CardMenu),
    Rename {
        target: gpui::EntityId,
        input: Entity<name_input::NameInput>,
        error: Option<String>,
    },
    Publish {
        preview: PublishPreview,
        title: String,
        target: String,
        row: usize,
        error: Option<String>,
    },
    /// Switch the selected task's agent CLI in its current worktree. `index`
    /// is a row in the same list New offers. The prompt starts empty and is
    /// not a branch name.
    SwitchAgent {
        session_id: String,
        prompt: Entity<name_input::NameInput>,
        index: usize,
        error: Option<String>,
    },
    /// `lead` starts the project's Lead instead of a task.
    Picker {
        project: String,
        index: usize,
        lead: bool,
    },
    Close {
        index: usize,
        state: SessionGitState,
    },
    /// The pointer menu of a resting Lead: Forget Lead.
    RestingMenu {
        project: String,
        position: gpui::Point<Pixels>,
    },
    /// Stop a live Lead, or forget one. `session` is the live Lead's id,
    /// None for a resting one.
    LeadClose {
        project: String,
        session: Option<String>,
        step: model::LeadClose,
    },
    SwitchedClose {
        index: usize,
        preview: shika_core::SwitchedBranchClose,
        working: bool,
    },
    Preparation {
        project: String,
        preset: CliPreset,
        config: PreparationConfig,
        retry: Option<Arc<Mutex<HostState>>>,
    },
    Leftovers,
    RemoveLeftover(usize),
    RemoveProject(String),
    /// `section` is Appearance, Keyboard, Agents, Projects, or Usage. `row` is the selected row
    /// in that section: an appearance `*_ROW`, a keyboard command, or a preset
    /// index. `edit` holds digits typed into the selected number, or the
    /// prefix being typed, not yet applied. `usage` is the saved snapshot,
    /// read the first time Usage shows and kept until Settings closes.
    Settings {
        section: SettingsSection,
        row: usize,
        edit: Option<String>,
        usage: Option<Box<UsageView>>,
    },
    /// The branch New starts from in `project`. `text` is the field, always
    /// being typed into; `error` is why the last Enter was refused.
    /// `choices` is None until the branches already on disk have been read.
    /// `highlight` is a row in the filtered list. None with an empty field
    /// uses the default branch; None with an unknown name fetches it.
    Base {
        project: String,
        text: String,
        error: Option<String>,
        choices: Option<KnownBranches>,
        highlight: Option<usize>,
    },
}
impl Overlay {
    /// How a refusal names this overlay to a Lead.
    fn name(&self) -> &'static str {
        match self {
            Self::LeadQuestion(_) => "the Lead question dialog",
            Self::WorktreeSetup(_) => "the Worktree setup dialog",
            Self::Adjust(_) => "the Adjust background dialog",
            Self::CardMenu(_) => "a card menu",
            Self::Rename { .. } => "the Rename task dialog",
            Self::Publish { .. } => "the Create PR dialog",
            Self::SwitchAgent { .. } => "the Switch agent dialog",
            Self::Picker { .. } => "the New picker",
            Self::Close { .. } | Self::SwitchedClose { .. } => "the Close task dialog",
            Self::RestingMenu { .. } => "a card menu",
            Self::LeadClose { .. } => "the Lead's Stop dialog",
            Self::Preparation { .. } => "the setup approval dialog",
            Self::Leftovers | Self::RemoveLeftover(_) => "the leftovers list",
            Self::RemoveProject(_) => "the Remove project dialog",
            Self::Settings { .. } => "Settings",
            Self::Base { .. } => "the Base branch dialog",
        }
    }
}
struct Shika {
    core: Arc<Core>,
    projects: Vec<Project>,
    cards: Vec<Card>,
    lead_chats: HashMap<String, Entity<lead_chat::LeadChat>>,
    /// First line of each project's latest Lead reply, for its card. Filled
    /// from live replies and one off-thread read of the saved history.
    lead_snippets: HashMap<String, String>,
    snippets_requested: std::collections::HashSet<String>,
    chat_questions: Vec<(String, String, shika_core::ChatQuestion)>,
    /// When the confirmation dialog on screen appeared, to ignore keys that
    /// were already in flight.
    dialog_grace: model::DialogGrace,
    selection: Option<Selection>,
    focus: FocusHandle,
    catalog: Option<CliCatalog>,
    overlay: Option<Overlay>,
    /// Non-workflow overlays return to the surface that opened them.
    overlay_return_focus: Option<FocusHandle>,
    /// The background image from settings, painted behind the glass.
    background: Option<shika_core::Background>,
    /// The image's rasters on screen and the one being built.
    backdrop: background::Backdrop,
    /// The image was picked or removed, so the window's own background
    /// (opaque or transparent) changes on the next paint.
    window_backdrop_dirty: bool,
    sidebar_scroll: gpui::ScrollHandle,
    /// The Base branch list, so Up and Down can bring the highlight into view.
    base_scroll: gpui::ScrollHandle,
    /// The Settings row list, so `j` / `k` keep the selected row on screen
    /// when a short window makes that list scroll.
    settings_scroll: gpui::ScrollHandle,
    /// Its scrollbar's thumb while the pointer drags it.
    settings_grab: scrollbar::Grab,
    last_revealed_selection: Option<Selection>,
    busy: bool,
    toast: Option<(String, Instant)>,
    /// The corner update notice. A release build sets it from Sparkle.
    /// `--preview-update` paints a sample so the layout can be seen locally.
    update_notice: Option<updates::UpdateCard>,
    /// Clicks on the sample card only change the picture.
    preview_update: bool,
    leftovers: Vec<JournalEntry>,
    leftover_selected: usize,
    clock: Instant,
    /// How long the selected card has been on screen, for clearing its dot.
    dwell: model::Dwell,
    branch_check_at: Instant,
    branch_check_pending: bool,
    notifications: Notifications,
    clicks: std::sync::mpsc::Receiver<String>,
    appearance: Appearance,
    /// Theme mode and the light and dark picks, as saved. Ids resolve to a
    /// catalog theme when painting.
    theme: ThemeSettings,
    /// Terminal text size. New panes and live ones both use it.
    font_size: FontSize,
    /// Last read of macOS Reduce transparency. Glass stays off while this is set.
    reduce_transparency: bool,
    /// Keeps the light and dark listener alive for the life of the window.
    #[allow(dead_code)]
    appearance_watch: Subscription,
    /// Mouse is down on the title bar and has not moved yet. The drag starts
    /// on the first move, so a double-click can still zoom.
    title_drag: bool,
    /// As typed in Settings, already normalized. Core reads it from disk.
    branch_prefix: String,
    /// Play the system alert sound with a ready notification.
    notification_sound: bool,
    /// Which agents New lists. A missing id is on.
    agents: AgentSettings,
    /// Shortcut overrides. Missing entries keep the built-in chords.
    keys: KeyOverrides,
    /// Settings, Keyboard, is waiting for the next Command shortcut.
    key_recording: bool,
    /// Why the last recorded shortcut was refused. Shown in the Keyboard footer.
    key_note: Option<String>,
    /// The agent column's stored width and whether it is hidden.
    column: Column,
    /// The column as it was when the current drag on its edge began. The
    /// file is written once the drag ends.
    column_drag_from: Option<Column>,
    /// Each project's base branch, resolved off the main thread. A project
    /// missing here shows no base label.
    bases: HashMap<String, ProjectBase>,
    /// The read-only Changes panel right of the terminal.
    changes: changes::Panel,
    /// The Lead's control socket, or why there is none.
    control: Option<control::Server>,
    /// The Close flow a Lead's `shika close` is waiting on. See
    /// `control::LeadDialog`. `shika pr` has no dialog.
    lead_dialog: Option<control::LeadDialog>,
    control_error: Option<String>,
    /// Closed cards playing their exit. See [`Departing`].
    departing: Vec<Departing>,
    departing_serial: u64,
    /// Last rendered project order, independent of the saved repository list.
    project_order: Vec<String>,
    project_order_epoch: u64,
}
impl Shika {
    fn new(
        core: Arc<Core>,
        settings: shika_core::Result<Settings>,
        diagnostics: Option<PathBuf>,
        preview_update: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (notifications, clicks) = Notifications::new();
        if let Some(path) = &diagnostics {
            notifications.set_diagnostics_file(path);
        }
        let mut load_errors = Vec::new();
        let projects = core.projects().unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Vec::new()
        });
        let selection = projects.first().map(|p| Selection::Project(p.id.clone()));
        let leftovers = core.leftovers_list().unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Vec::new()
        });
        let settings = settings.unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Settings::default()
        });
        let appearance = settings.appearance;
        let theme = settings.theme.clone();
        let font_size = settings.font_size;
        let branch_prefix = shika_core::normalize_branch_prefix(&settings.branch_prefix);
        let notification_sound = settings.notification_sound;
        let agents = settings.agents.clone();
        let keys = settings.keys.clone();
        let column = settings.column;
        let background = settings.background.clone();
        let changes = changes::Panel::new(settings.changes, cx.focus_handle(), diagnostics.clone());
        let (control, control_error) = match control::Server::start(core.data_dir()) {
            Ok(server) => (Some(server), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let reduce_transparency = appearance::reduce_transparency();
        let entity = cx.entity().downgrade();
        let appearance_watch = window.observe_window_appearance(move |window, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.push_terminal_theme(window, cx);
                cx.notify();
            });
        });
        let this = Self {
            core: core.clone(),
            projects,
            cards: vec![],
            lead_chats: HashMap::new(),
            lead_snippets: HashMap::new(),
            snippets_requested: Default::default(),
            chat_questions: Vec::new(),
            dialog_grace: model::DialogGrace::default(),
            selection,
            focus: cx.focus_handle(),
            catalog: None,
            overlay: if leftovers.is_empty() {
                None
            } else {
                Some(Overlay::Leftovers)
            },
            overlay_return_focus: None,
            background,
            backdrop: background::Backdrop::default(),
            window_backdrop_dirty: false,
            sidebar_scroll: gpui::ScrollHandle::new(),
            base_scroll: gpui::ScrollHandle::new(),
            settings_scroll: gpui::ScrollHandle::new(),
            settings_grab: scrollbar::Grab::default(),
            last_revealed_selection: None,
            busy: false,
            toast: if load_errors.is_empty() {
                None
            } else {
                Some((load_errors.join("; "), Instant::now()))
            },
            update_notice: preview_update.then(sample_update_notice),
            preview_update,
            leftovers,
            leftover_selected: 0,
            clock: Instant::now(),
            dwell: model::Dwell::default(),
            branch_check_at: Instant::now(),
            branch_check_pending: false,
            notifications,
            clicks,
            appearance,
            theme,
            font_size,
            reduce_transparency,
            appearance_watch,
            title_drag: false,
            branch_prefix,
            notification_sound,
            agents,
            keys,
            key_recording: false,
            key_note: None,
            column,
            column_drag_from: None,
            bases: HashMap::new(),
            changes,
            control,
            lead_dialog: None,
            control_error,
            departing: Vec::new(),
            departing_serial: 0,
            project_order: Vec::new(),
            project_order_epoch: 0,
        };
        cx.spawn_in(window, async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async move {
                    let c = core.cli_catalog();
                    if let Some(path) = diagnostics {
                        let text = c
                            .presets
                            .iter()
                            .map(|p| {
                                format!(
                                    "{}={}\n",
                                    p.binary,
                                    p.path
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or("not found".into())
                                )
                            })
                            .collect::<String>();
                        let _ = std::fs::write(path, text);
                    }
                    c
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(error) = &catalog.error {
                    this.message(format!("Login shell PATH: {error:?}"));
                }
                this.catalog = Some(catalog);
                let ids = this.projects.iter().map(|p| p.id.clone()).collect();
                this.refresh_bases(ids, cx);
                cx.notify();
            });
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        install_keys(cx, &this.keys);
        this
    }
    fn message(&mut self, text: String) {
        self.toast = Some((text, Instant::now()));
    }
    /// Sparkle posts on the main thread. The corner card reads the queue here,
    /// and again immediately after a click.
    fn drain_update_notice(&mut self, cx: &App) -> bool {
        if !cx.has_global::<updates::Updater>() {
            return false;
        }
        let mut changed = false;
        for event in cx.global::<updates::Updater>().poll() {
            changed = true;
            match event {
                updates::UpdateEvent::Show(card) => self.update_notice = Some(card),
                updates::UpdateEvent::Clear => self.update_notice = None,
                updates::UpdateEvent::Toast(text) => self.message(text),
            }
        }
        changed
    }
    fn answer_update(&mut self, action: updates::CardAction, cx: &mut Context<Self>) {
        if self.preview_update {
            self.advance_update_preview(action);
            cx.notify();
            return;
        }
        let open = match (&action, &self.update_notice) {
            (
                updates::CardAction::Primary,
                Some(updates::UpdateCard::Available {
                    offer: Some(updates::Offer::Open(url)),
                    ..
                }),
            ) => Some(url.clone()),
            _ => None,
        };
        if let Some(url) = open {
            cx.open_url(&url);
        }
        if cx.has_global::<updates::Updater>() {
            cx.global::<updates::Updater>().respond(action);
        }
        self.drain_update_notice(cx);
        cx.notify();
    }
    /// The sample card. Download shows the restart card. Every other button closes it.
    fn advance_update_preview(&mut self, action: updates::CardAction) {
        self.update_notice = match (&self.update_notice, action) {
            (
                Some(updates::UpdateCard::Available {
                    offer: Some(updates::Offer::Download),
                    version,
                    ..
                }),
                updates::CardAction::Primary,
            ) => Some(updates::UpdateCard::Ready {
                version: version.clone(),
            }),
            (Some(updates::UpdateCard::Available { .. }), updates::CardAction::Secondary)
            | (Some(updates::UpdateCard::Ready { .. }), _) => None,
            (other, _) => other.clone(),
        };
    }
    fn selected_card(&self) -> Option<usize> {
        match self.selection {
            Some(Selection::Card(i)) if i < self.cards.len() => Some(i),
            _ => None,
        }
    }
    /// The project the selection belongs to: a selected header, a card of
    /// it (its own, its Lead, or a worker of its Lead), or its resting Lead.
    fn selected_project(&self) -> Option<&str> {
        match &self.selection {
            Some(Selection::Project(id) | Selection::Resting(id)) => Some(id),
            Some(Selection::Card(i)) => self.cards.get(*i).map(|c| c.project.as_str()),
            None => None,
        }
    }
    /// The project whose resting Lead is selected.
    fn selected_resting(&self) -> Option<&str> {
        match &self.selection {
            Some(Selection::Resting(project)) => Some(project),
            _ => None,
        }
    }
    /// Projects with a saved Lead and no live one, as (project id, CLI
    /// preset id), in project order. A Lead that is still starting counts as
    /// live.
    fn resting_leads(&self) -> Vec<(&str, &str)> {
        self.projects
            .iter()
            .filter(|p| self.lead_card(&p.id).is_none())
            .filter_map(|p| Some((p.id.as_str(), p.lead.as_ref()?.cli.as_str())))
            .collect()
    }
    /// The CLI's display name for a saved Lead. The catalog loads a moment
    /// after launch; until then the preset id stands in.
    fn cli_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.iter().find(|p| p.id == id))
            .map_or(id, |preset| preset.name.as_str())
    }
    fn project_id(&self) -> Option<String> {
        match &self.selection {
            Some(Selection::Project(p) | Selection::Resting(p)) => Some(p.clone()),
            Some(Selection::Card(i)) => self.cards.get(*i).map(|c| c.project.clone()),
            _ => self.projects.first().map(|p| p.id.clone()),
        }
    }
    /// The Leads section above the projects, each Lead followed by its
    /// workers; then every project's own cards, sorted by attention.
    fn column_layout(&self) -> model::ColumnLayout {
        let placements = self
            .cards
            .iter()
            .map(|c| model::Placement {
                project: &c.project,
                rank: c.status.rank(),
                lead: c.lead.is_some(),
                session: c.session.as_ref().map(|s| s.id.as_str()),
                started_by: c.started_by.as_deref(),
            })
            .collect::<Vec<_>>();
        let projects = self
            .projects
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>();
        let resting = self
            .resting_leads()
            .into_iter()
            .map(|(project, cli)| model::Resting {
                project,
                cli: self.cli_name(cli),
            })
            .collect::<Vec<_>>();
        let departing = self
            .departing
            .iter()
            .filter_map(|d| match &d.home {
                Home::Project(project) => Some(project.as_str()),
                Home::Leads(_) => None,
            })
            .collect::<Vec<_>>();
        model::ColumnLayout::with_departing(&projects, &placements, &resting, &departing)
    }
    /// Walking order for `j` / `k`, Cmd+] / Cmd+[ and Go to agent N: the Leads
    /// section (no headers), then each project's header and cards.
    fn rows(&self) -> Vec<Selection> {
        let layout = self.column_layout();
        let mut rows = vec![];
        for entry in &layout.leads {
            match entry.kind {
                model::LeadKind::Running { .. } => {
                    rows.extend(entry.cards().into_iter().map(Selection::Card))
                }
                model::LeadKind::Resting { .. } => {
                    rows.push(Selection::Resting(entry.project.clone()))
                }
            }
        }
        for entry in &layout.projects {
            rows.push(Selection::Project(entry.project.clone()));
            rows.extend(entry.cards.iter().copied().map(Selection::Card));
        }
        rows
    }
    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        // Headers, including a project with no cards, are not stops.
        let Some(row) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) else {
            return;
        };
        self.selection = Some(row);
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn move_agent(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if let Some(row) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) {
            self.select_agent_keeping_surface(row, window, cx);
        }
    }
    /// Select an agent row and keep the surface that invoked it: a terminal
    /// stays in the destination terminal, the Changes panel stays in the
    /// panel, and the cards stay on the cards. A resting Lead has no
    /// terminal, so leaving a terminal for it lands on the cards.
    fn select_agent_keeping_surface(
        &mut self,
        row: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel_focused = self.changes.open && self.changes.focus.is_focused(window);
        let terminal_focused = !self.focus.is_focused(window) && !panel_focused;
        let card = matches!(row, Selection::Card(_));
        self.selection = Some(row);
        if terminal_focused {
            if card {
                self.focus_terminal(window, cx);
            } else {
                window.focus(&self.focus, cx);
            }
        }
        cx.notify();
    }
    fn select_agent(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(row) = nth_agent(&self.rows(), index) else {
            return;
        };
        self.select_agent_keeping_surface(row, window, cx);
    }
    fn restore_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self
            .overlay_return_focus
            .take()
            .unwrap_or_else(|| self.focus.clone());
        let cards = focus == self.focus;
        let changes = self.changes.open && focus == self.changes.focus;
        let selected_terminal = self
            .selected_card()
            .map(|index| self.cards[index].active_pane().view.focus_handle(cx));
        let chat_focus = self
            .selected_chat()
            .is_some_and(|chat| chat.read(cx).owns_focus(&focus, cx));
        if cards || changes || chat_focus || selected_terminal.as_ref() == Some(&focus) {
            window.focus(&focus, cx);
        } else if (self.cards.iter().any(|card| {
            std::iter::once(&card.agent)
                .chain(card.shells.iter())
                .any(|pane| pane.view.focus_handle(cx) == focus)
        }) || self
            .lead_chats
            .values()
            .any(|chat| chat.read(cx).owns_focus(&focus, cx)))
            && !self.busy
        {
            // A notification may have selected another task while editing.
            // Never restore focus to the old task's now-hidden terminal.
            self.focus_terminal(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(chat) = self.selected_chat() {
            window.focus(&chat.focus_handle(cx), cx);
            cx.notify();
            return;
        }
        if let Some(i) = self.selected_card() {
            let c = &self.cards[i];
            c.reveal_active_tab();
            let pane = c.active_pane();
            window.focus(&pane.view.focus_handle(cx), cx);
            cx.notify();
        }
    }
    fn picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if let Some(project) = self.project_id() {
            self.prefetch_base(project.clone(), cx);
            self.overlay_return_focus = window.focused(cx);
            self.overlay = Some(Overlay::Picker {
                project,
                index: 0,
                lead: false,
            });
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            self.add_project(cx);
        }
    }
    fn add_project(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let result = cx
                    .background_executor()
                    .spawn(async move { core.add_project(&path) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(added) => {
                            this.projects = this.core.projects().unwrap_or_default();
                            this.refresh_bases(vec![added.project.id.clone()], cx);
                            this.selection = Some(Selection::Project(added.project.id));
                            if let Some(note) = added.note {
                                this.message(note);
                            }
                        }
                        Err(e) => this.message(e.to_string()),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (project, index, lead) = match &self.overlay {
            Some(Overlay::Picker {
                project,
                index,
                lead,
            }) => (project.clone(), *index, *lead),
            _ => return,
        };
        let Some(preset) = self.offered_presets().into_iter().nth(index) else {
            return;
        };
        if lead {
            self.start_lead(project, preset, window, cx);
        } else {
            self.request_launch(project, preset, None, window, cx);
        }
    }
    /// Installed agents the user has left on, in preset order.
    fn offered_presets(&self) -> Vec<CliPreset> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        shika_core::picker_presets(&catalog.presets, |id| self.agents.enabled(id))
            .into_iter()
            .cloned()
            .collect()
    }

    /// What a Lead is started with, or None (and a toast) without the control
    /// socket it talks through.
    fn lead_launch(&mut self, project: &str) -> Option<LeadLaunch> {
        let Some(server) = &self.control else {
            let why = self.control_error.clone().unwrap_or_default();
            self.message(format!("The Lead needs Shika's control socket: {why}"));
            return None;
        };
        let name = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        Some(LeadLaunch {
            socket: server.socket(),
            token: shika_core::control::new_token(),
            bin_dir: server.bin_dir(),
            instructions: control::lead_guide(&server.bin_dir()),
            cursor_bootstrap: Some(control::lead_prompt(&name, &server.bin_dir())),
        })
    }

    /// The picker's choice in Lead mode: the project's one Lead, in this CLI.
    /// A saved conversation of another CLI is not continued (core starts a
    /// new one).
    fn start_lead(
        &mut self,
        project: String,
        preset: CliPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !preset.found() {
            self.message(format!("{} not found on PATH", preset.binary));
            cx.notify();
            return;
        }
        // Tab can move the picker to a project that already has its Lead.
        if self.show_live_lead(&project, window, cx) {
            self.overlay = None;
            self.overlay_return_focus = None;
            return;
        }
        let Some(env) = self.lead_launch(&project) else {
            cx.notify();
            return;
        };
        self.begin_launch(
            project,
            preset,
            false,
            None,
            Launch::Lead(env),
            None,
            window,
            cx,
        );
    }

    /// Cmd+L and Agent > New Lead: the selected project's Lead, shown when
    /// live, resumed when saved, and chosen from the picker only when the
    /// project has none.
    fn new_lead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(project) = self.project_id() else {
            self.message("Add a project first.".into());
            cx.notify();
            return;
        };
        if !self.show_lead(&project, window, cx) {
            self.lead_picker(project, window, cx);
        }
    }

    /// The picker in Lead mode for `project`.
    fn lead_picker(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.control.is_none() {
            let why = self.control_error.clone().unwrap_or_default();
            self.message(format!("The Lead needs Shika's control socket: {why}"));
            cx.notify();
            return;
        }
        self.prefetch_base(project.clone(), cx);
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Picker {
            project,
            index: 0,
            lead: true,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Selects the project's live Lead and focuses its terminal. False when
    /// it has none.
    fn show_live_lead(
        &mut self,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.lead_card(project) else {
            return false;
        };
        self.selection = Some(Selection::Card(index));
        self.focus_terminal(window, cx);
        cx.notify();
        true
    }

    /// Shows the project's Lead: the live one, else its saved one, resumed.
    /// False when the project has neither.
    fn show_lead(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.show_live_lead(project, window, cx) {
            return true;
        }
        if self.resting_leads().iter().any(|(id, _)| *id == project) {
            self.resume_lead(project.to_string(), window, cx);
            return true;
        }
        false
    }

    /// Starts a project's saved Lead in the CLI it ran in, with no picker.
    /// When that CLI is not found or is turned off in Settings the picker
    /// opens instead: another CLI starts a new conversation.
    fn resume_lead(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(saved) = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.lead.as_ref())
            .map(|lead| lead.cli.clone())
        else {
            return;
        };
        let Some(catalog) = &self.catalog else {
            self.message("Still looking for your agents. Try again in a moment.".into());
            cx.notify();
            return;
        };
        let preset = catalog
            .presets
            .iter()
            .find(|p| p.id == saved && p.found() && self.agents.enabled(&p.id))
            .cloned();
        match preset {
            Some(preset) => self.start_lead(project, preset, window, cx),
            None => {
                self.message(format!(
                    "{} is not available. Another agent starts a new conversation.",
                    self.cli_name(&saved)
                ));
                self.lead_picker(project, window, cx);
            }
        }
    }

    /// Re-reads the saved projects, which hold each project's saved Lead.
    fn reload_projects(&mut self) {
        if let Ok(projects) = self.core.projects() {
            self.projects = projects;
        }
    }

    fn request_launch(
        &mut self,
        project: String,
        preset: CliPreset,
        retry: Option<Arc<Mutex<HostState>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if !preset.found() {
            self.message(format!("{} not found on PATH", preset.binary));
            cx.notify();
            return;
        }
        self.busy = true;
        let core = self.core.clone();
        let lookup = project.clone();
        let first_attempt = retry.is_none();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut config = core.project_preparation(&lookup)?;
                    let onboarding = if first_attempt
                        && config.is_none()
                        && core.preparation_onboarding_pending(&lookup)?
                    {
                        let draft = core.project_preparation_draft(&lookup)?;
                        config = draft.existing.clone();
                        if config.is_none() { Some(draft) } else { None }
                    } else {
                        None
                    };
                    let approved = match &config {
                        Some(config) => core.preparation_approved(&lookup, config)?,
                        None => true,
                    };
                    Ok::<_, shika_core::Error>((config, approved, onboarding))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok((_, _, Some(draft))) => {
                        if this.overlay_return_focus.is_none() {
                            this.overlay_return_focus = window.focused(cx);
                        }
                        this.show_preparation_onboarding(project, preset, draft, window, cx);
                    }
                    Ok((Some(config), false, None)) => {
                        if this.overlay_return_focus.is_none() {
                            this.overlay_return_focus = window.focused(cx);
                        }
                        this.overlay = Some(Overlay::Preparation {
                            project,
                            preset,
                            config,
                            retry,
                        });
                        window.focus(&this.focus, cx);
                    }
                    Ok((config, _, None)) => this.begin_launch(
                        project,
                        preset,
                        config.is_some(),
                        retry,
                        Launch::Task,
                        None,
                        window,
                        cx,
                    ),
                    Err(error) => this.message(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn approve_preparation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Preparation {
            project,
            preset,
            config,
            retry,
        }) = &self.overlay
        else {
            return;
        };
        let (project, preset, config, retry) = (
            project.clone(),
            preset.clone(),
            config.clone(),
            retry.clone(),
        );
        self.busy = true;
        let core = self.core.clone();
        let lookup = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.approve_preparation(&lookup, &config) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.begin_launch(
                        project,
                        preset,
                        true,
                        retry,
                        Launch::Task,
                        None,
                        window,
                        cx,
                    ),
                    Err(error) => {
                        this.overlay = None;
                        this.restore_overlay_focus(window, cx);
                        this.message(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry_preparation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(i) = self.selected_card() else {
            return;
        };
        let card = &self.cards[i];
        if card.creating || card.launch_error.is_none() {
            return;
        }
        let Some(preset) = self
            .catalog
            .as_ref()
            .and_then(|c| c.presets.iter().find(|p| p.id == card.launch_preset))
            .cloned()
        else {
            return;
        };
        self.request_launch(
            card.project.clone(),
            preset,
            Some(card.agent.state.clone()),
            window,
            cx,
        );
    }

    fn stop_cancelled_launch(&mut self, id: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move { core.cancel_session_start(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.leftovers = this.core.leftovers_list().unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_card(&mut self, index: usize) {
        self.cards.remove(index);
        self.selection = match self.selection.take() {
            Some(Selection::Card(at)) if at > index => Some(Selection::Card(at - 1)),
            Some(Selection::Card(at)) if at == index => {
                if self.cards.is_empty() {
                    self.projects
                        .first()
                        .map(|p| Selection::Project(p.id.clone()))
                } else {
                    Some(Selection::Card(index.min(self.cards.len() - 1)))
                }
            }
            selection => selection,
        };
        self.last_revealed_selection = None;
    }

    /// Takes a closed card out of `cards` and lets it play its exit in place
    /// for [`model::EXIT`]. Selection is the caller's. With macOS Reduce
    /// motion the card goes at once.
    fn depart(&mut self, index: usize, cx: &mut Context<Self>) {
        let project = self.cards[index].project.clone();
        // It leaves from the group it was shown in, before the card that
        // followed it there.
        let layout = self.column_layout();
        let (home, group) = match layout
            .leads
            .iter()
            .find(|entry| entry.cards().contains(&index))
        {
            Some(entry) => (Home::Leads(project), entry.cards()),
            None => (
                Home::Project(project.clone()),
                layout
                    .projects
                    .iter()
                    .find(|entry| entry.project == project)
                    .map(|entry| entry.cards.clone())
                    .unwrap_or_default(),
            ),
        };
        let before = group
            .into_iter()
            .skip_while(|&i| i != index)
            .nth(1)
            .map(|i| self.cards[i].agent.view.entity_id());
        let card = self.cards.remove(index);
        self.last_revealed_selection = None;
        if cx.reduce_motion() {
            return;
        }
        self.departing_serial += 1;
        let key = self.departing_serial;
        self.departing.push(Departing {
            card,
            home,
            before,
            key,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(model::EXIT).await;
            let _ = this.update(cx, |this, cx| {
                this.departing.retain(|d| d.key != key);
                cx.notify();
            });
        })
        .detach();
    }

    /// Creates the card and starts the launch off the UI thread. A task from
    /// the picker, a Lead, and a retry are the author's: the card is selected
    /// and the dialogs close. A worker from `shika new` (`reply` answers the
    /// Lead's command) touches neither selection, focus, dialogs, nor `busy`,
    /// because the author is typing elsewhere.
    #[allow(clippy::too_many_arguments)]
    fn begin_launch(
        &mut self,
        project: String,
        preset: CliPreset,
        configured: bool,
        retry: Option<Arc<Mutex<HostState>>>,
        launch: Launch,
        reply: Option<control::Responder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut launch = launch;
        let mut retried = false;
        if let Some(retry) = retry
            && let Some(i) = self
                .cards
                .iter()
                .position(|c| Arc::ptr_eq(&c.agent.state, &retry))
        {
            launch = self.cards[i].launch.clone();
            retried = true;
            self.remove_card(i);
        }
        let author = retried || !matches!(launch, Launch::Worker(_) | Launch::LeadRestart { .. });
        let worker_token = match &launch {
            Launch::Worker(options) => options.control.as_ref().map(|c| c.token.clone()),
            _ => None,
        };
        let (started_by, lead) = match &launch {
            Launch::Worker(options) => (options.started_by.clone(), None),
            Launch::Lead(env) | Launch::LeadRestart { launch: env, .. } => {
                (None, Some(control::LeadState::new(env.token.clone())))
            }
            Launch::Task => (None, None),
        };
        if author {
            self.overlay = None;
            self.overlay_return_focus = None;
        }
        // Long setup must not prevent starting another card or using a live
        // terminal. Keep the existing short-launch behavior without config.
        let holds_busy = author && !configured;
        if author {
            self.busy = holds_busy;
        }
        let chat = lead.is_some() && preset.id == "claude";
        if chat {
            self.ensure_lead_chat(&project, window, cx)
                .update(cx, |chat, cx| {
                    chat.live = false;
                    chat.starting = true;
                    cx.notify();
                });
        }
        let opacity = self.terminal_opacity();
        let pane = Pane::new(
            self.core.clone(),
            true,
            opacity,
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        );
        let terminal = pane.terminal.clone();
        let state = pane.state.clone();
        {
            let mut host = lock(&state);
            host.preparing = configured;
            if lead.is_some() {
                host.keep_title();
            }
        }
        if configured || !author || chat {
            // A second launch can hide this pane before its first layout, and
            // a worker's pane is never shown until the author picks it. Give
            // it a real initial grid so the launch does not wait forever for
            // a visible view; the next layout/PTY binding uses the actual fit.
            let fallback = self
                .selected_card()
                .and_then(|i| lock(&self.cards[i].agent.state).measured)
                .unwrap_or(TerminalSize::new(32, 100));
            terminal.resize(fallback);
        }
        let control = PreparationControl::default();
        let index = self.cards.len();
        let is_lead = lead.is_some();
        self.cards.push(Card {
            session: None,
            project: project.clone(),
            title: if is_lead {
                "Lead".into()
            } else {
                format!("New {}", preset.name)
            },
            preset: preset.name.clone(),
            status: Status::Waiting,
            since: Instant::now(),
            activity: activity::Activity::new(Instant::now()),
            evidence: activity::OutputEvidence::default(),
            lifecycle: lifecycle::Lifecycle::default(),
            lifecycle_checked: None,
            agent: pane,
            chat,
            chat_delivery: None,
            shells: Vec::new(),
            active_tab: 0,
            shell_serial: 0,
            tab_scroll: gpui::ScrollHandle::new(),
            hovered_tab: None,
            submitted: 0,
            creating: true,
            title_watch: TitleWatch::default(),
            named: is_lead,
            diff: None,
            pr: None,
            lead_publishing: false,
            seen: None,
            launch_preset: preset.id.clone(),
            launch: launch.clone(),
            started_by,
            lead,
            worker_token,
            report: None,
            discard: false,
            launch_control: Some(control.clone()),
            launch_error: None,
            stage: "Creating worktree...".into(),
            closing: false,
            next_agent: None,
            usage_exit_seen: false,
            lead_run: None,
        });
        if author {
            self.selection = Some(Selection::Card(index));
            window.focus(&self.focus, cx);
        }
        cx.notify();
        let core = self.core.clone();
        let finished = launch.clone();
        // What core says about a Lead start, for the completion below.
        let lead_start: Arc<Mutex<Option<shika_core::LeadStart>>> = Arc::default();
        let started_lead = lead_start.clone();
        cx.spawn_in(window, async move |this, cx| {
            let measured = loop {
                if let Some(size) = lock(&state).measured {
                    break size;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this.upgrade().is_none() {
                    return;
                }
            };
            let sink = agent_sink(terminal.clone(), state.clone());
            let state_bind = state.clone();
            let progress_state = state.clone();
            let progress_terminal = terminal.clone();
            let callback_core = core.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let size = PtySize::new(measured.rows, measured.cols);
                    let report = move |event| match event {
                        PreparationEvent::Stage(stage) => {
                            lock(&progress_state).preparation_stage = stage;
                        }
                        PreparationEvent::StartingAgent => {
                            let mut host = lock(&progress_state);
                            host.agent_starting = true;
                            host.preparation_stage = "Starting agent...".into();
                        }
                        PreparationEvent::Output(bytes) => {
                            // Pipes emit LF; the terminal needs CRLF. Preserve
                            // existing CRLF rather than doubling its CR.
                            let mut output = Vec::with_capacity(bytes.len());
                            for byte in bytes {
                                if byte == b'\n' && output.last() != Some(&b'\r') {
                                    output.push(b'\r');
                                }
                                output.push(byte);
                            }
                            progress_terminal.feed(&output);
                        }
                    };
                    let result = match launch {
                        Launch::Lead(launch) | Launch::LeadRestart { launch, .. } => (if chat {
                            core.start_claude_lead(&project, launch)
                        } else {
                            core.start_lead(&project, &preset.id, size, sink, launch)
                        })
                        .map(|start| {
                            let session = start.session.clone();
                            *lock(&started_lead) = Some(start);
                            session
                        }),
                        Launch::Worker(options) => core.create_session_with_preparation(
                            &project, &preset.id, size, sink, options, control, report,
                        ),
                        Launch::Task => core.create_session_with_preparation(
                            &project,
                            &preset.id,
                            size,
                            sink,
                            LaunchOptions::default(),
                            control,
                            report,
                        ),
                    };
                    if let Ok(s) = &result
                        && let Some(pty) = s.pty
                    {
                        bind_host(&core, &state_bind, pty);
                    }
                    result
                })
                .await;
            let orphan = result.as_ref().ok().map(|s| s.id.clone());
            let completion_core = callback_core.clone();
            let updated = this.update_in(cx, |this, window, cx| {
                if holds_busy {
                    this.busy = false;
                }
                let Some(index) = this
                    .cards
                    .iter()
                    .position(|c| Arc::ptr_eq(&c.agent.state, &state))
                else {
                    control::answer(
                        &reply,
                        control::failure_reply(&shika_core::Error::PreparationCancelled),
                    );
                    if let Ok(session) = result {
                        let core = completion_core.clone();
                        cx.background_executor()
                            .spawn(async move {
                                let _ = core.cancel_session_start(&session.id);
                            })
                            .detach();
                    }
                    return;
                };
                this.cards[index].creating = false;
                if let Some(chat) = this
                    .lead_chats
                    .get(&this.cards[index].project)
                    .filter(|_| this.cards[index].chat)
                {
                    chat.update(cx, |chat, cx| {
                        chat.starting = false;
                        cx.notify();
                    });
                }
                match result {
                    Ok(session)
                        if !this.cards[index]
                            .launch_control
                            .as_ref()
                            .is_some_and(|c| c.is_cancelled()) =>
                    {
                        {
                            let mut host = lock(&state);
                            host.preparing = false;
                            // Nobody types a launch prompt, so it counts as
                            // submitted now: the turn starts with the CLI.
                            let now = Instant::now();
                            match &finished {
                                Launch::Worker(LaunchOptions {
                                    prompt: Some(prompt),
                                    ..
                                }) => host.seed_launch_prompt(prompt, true, now),
                                // Only when core passed it: a fresh Cursor
                                // start. Every other Lead waits for the author.
                                Launch::Lead(LeadLaunch {
                                    cursor_bootstrap: Some(prompt),
                                    ..
                                })
                                | Launch::LeadRestart {
                                    launch:
                                        LeadLaunch {
                                            cursor_bootstrap: Some(prompt),
                                            ..
                                        },
                                    ..
                                } if lock(&lead_start)
                                    .as_ref()
                                    .is_some_and(|start| start.bootstrap_sent) =>
                                {
                                    host.seed_launch_prompt(prompt, false, now)
                                }
                                _ => {}
                            }
                        }
                        if let Some(start) = lock(&lead_start).take() {
                            this.cards[index].lead_run = Some(LeadRun::new(&start, Instant::now()));
                            // The saved Lead is on disk now; a stop leaves
                            // its resting row.
                            this.reload_projects();
                        }
                        if this.cards[index].chat {
                            let project = this.cards[index].project.clone();
                            if let Some(chat) = this.lead_chats.get(&project) {
                                chat.update(cx, |chat, cx| {
                                    chat.live = true;
                                    chat.busy = false;
                                    cx.notify();
                                });
                            }
                        }
                        this.cards[index].session = Some(session);
                        this.cards[index].launch_control = None;
                        // Completion never steals focus from another card or
                        // an overlay opened while installation was running.
                        if this.selection == Some(Selection::Card(index))
                            && this.overlay.is_none()
                            && !matches!(finished, Launch::LeadRestart { focus: false, .. })
                        {
                            this.focus_terminal(window, cx);
                        }
                        control::answer(&reply, this.started_reply(index));
                    }
                    Ok(session) => {
                        this.stop_cancelled_launch(session.id, cx);
                        this.cards[index].launch_error =
                            Some("Worktree preparation was cancelled.".into());
                        control::answer(
                            &reply,
                            control::failure_reply(&shika_core::Error::PreparationCancelled),
                        );
                    }
                    Err(error) if configured => {
                        control::answer(&reply, control::failure_reply(&error));
                        lock(&state).agent_starting = false;
                        this.cards[index].agent.terminal.feed(
                            format!("\r\n{error}\r\nRetry creates a fresh worktree.\r\n")
                                .as_bytes(),
                        );
                        this.cards[index].launch_error = Some(error.to_string());
                        this.cards[index].stage = "Setup failed".into();
                    }
                    Err(error) => {
                        control::answer(&reply, control::failure_reply(&error));
                        if author {
                            this.remove_card(index);
                        } else {
                            // Removing a card under an open dialog would
                            // shift the positions it holds; the tick does it.
                            this.cards[index].discard = true;
                        }
                        this.message(error.to_string());
                    }
                }
                this.leftovers = this.core.leftovers_list().unwrap_or_default();
                cx.notify();
            });
            if updated.is_err()
                && let Some(id) = orphan
            {
                cx.background_executor()
                    .spawn(async move {
                        let _ = callback_core.cancel_session_start(&id);
                    })
                    .detach();
            }
        })
        .detach();
    }
    /// Close cancellation uses this to land on a shell. It is not a key.
    fn toggle(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating || self.busy || self.overlay.is_some() {
            return;
        }
        if self.cards[index].active_tab != 0 {
            self.cards[index].active_tab = 0;
            self.cards[index].reveal_active_tab();
            if focus {
                self.focus_terminal(window, cx);
            }
            cx.notify();
            return;
        }
        if !self.cards[index].shells.is_empty() {
            self.cards[index].active_tab = 1;
            self.cards[index].reveal_active_tab();
            if focus {
                self.focus_terminal(window, cx);
            }
            cx.notify();
            return;
        }
        self.new_shell(focus, window, cx);
    }

    fn new_shell(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating {
            return;
        }
        if self.cards[index].lead.is_some() {
            self.message("The Lead has no shell tabs. It works from its terminal.".into());
            cx.notify();
            return;
        }
        let Some(session) = self.cards[index].session.clone() else {
            return;
        };
        let opacity = self.terminal_opacity();
        let mut pane = Pane::new(
            self.core.clone(),
            false,
            opacity,
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        );
        let state = pane.state.clone();
        let terminal = pane.terminal.clone();
        self.cards[index].shell_serial += 1;
        pane.shell_number = self.cards[index].shell_serial;
        self.cards[index].shells.push(pane);
        let tab = self.cards[index].shells.len();
        self.cards[index].active_tab = tab;
        self.cards[index].reveal_active_tab();
        if focus {
            // Focus the new view now. Its host queues typeahead until the
            // PTY is bound, and startup must not steal focus back later.
            self.focus_terminal(window, cx);
        }
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let measured = loop {
                if let Some(size) = lock(&state).measured {
                    break size;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this.upgrade().is_none() {
                    return;
                }
            };
            let output_state = state.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.open_shell(
                        &session.id,
                        PtySize::new(measured.rows, measured.cols),
                        move |_, event| match event {
                            PtyEvent::Output(bytes) => terminal.feed(&bytes),
                            PtyEvent::Exit(_) => lock(&output_state).exited = true,
                        },
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(pty) => {
                        bind_host(&this.core, &state, pty);
                    }
                    Err(e) => {
                        let shell_focused = this.cards[index].shells[tab - 1]
                            .view
                            .focus_handle(cx)
                            .is_focused(window);
                        this.cards[index].shells.remove(tab - 1);
                        this.cards[index].active_tab = model::tab_after_close(
                            this.cards[index].active_tab,
                            tab,
                            this.cards[index].shells.len(),
                        );
                        if shell_focused {
                            this.focus_terminal(window, cx);
                        }
                        this.message(e.to_string());
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn select_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].chat || tab > self.cards[index].shells.len() {
            return;
        }
        self.cards[index].active_tab = tab;
        self.focus_terminal(window, cx);
    }

    fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_card() else {
            return;
        };
        let card = &self.cards[index];
        let tab = model::adjacent_tab(card.active_tab, card.shells.len(), forward);
        self.select_tab(tab, window, cx);
    }

    fn close_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() || tab == 0 {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        let card = &self.cards[index];
        let Some(pane) = card.shells.get(tab - 1) else {
            return;
        };
        let Some(session) = card.session.as_ref() else {
            return;
        };
        let Some(pty) = lock(&pane.state).pty else {
            return;
        };
        let id = session.id.clone();
        let focused = pane.view.focus_handle(cx).is_focused(window);
        let card = &mut self.cards[index];
        card.shells.remove(tab - 1);
        card.active_tab = model::tab_after_close(card.active_tab, tab, card.shells.len());
        // Process teardown may block; never do it on the UI thread.
        let core = self.core.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = core.close_shell(&id, pty);
            })
            .detach();
        if focused {
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    /// What a started Lead still needs: its conversation id when the CLI
    /// picks it (Codex) once it has been used, and what to do about a resumed
    /// CLI that exited at once. Nothing runs for a Lead that needs neither.
    fn lead_upkeep(&mut self, now: Instant, window: &mut Window, cx: &mut Context<Self>) {
        let mut reads = vec![];
        let mut checks = vec![];
        let mut early = None;
        for card in &mut self.cards {
            let (Some(run), Some(session)) = (&mut card.lead_run, &card.session) else {
                continue;
            };
            let exited = lock(&card.agent.state).exited;
            if let Some(launched) = run.resumed_at
                && exited
            {
                run.resumed_at = None;
                if model::resume_failed(launched, now) {
                    // Did the CLI refuse the id, or quit for another reason?
                    run.resume_exit = Some(model::ResumeExit::Checking);
                    checks.push((session.id.clone(), card.project.clone()));
                }
            }
            if !exited
                && let Some(find) = &mut run.discovery
                && find.due(now, card.submitted > 0)
            {
                find.started();
                reads.push((session.id.clone(), find.since));
            }
            if early.is_none()
                && let Some(exit @ (model::ResumeExit::Missing | model::ResumeExit::Kept)) =
                    run.resume_exit
            {
                early = Some((session.id.clone(), exit));
            }
        }
        for (id, project) in checks {
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let exists = cx
                    .background_executor()
                    .spawn(async move { core.saved_lead_session_exists(&project) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(run) = this
                        .cards
                        .iter_mut()
                        .find(|card| card.session.as_ref().is_some_and(|s| s.id == id))
                        .and_then(|card| card.lead_run.as_mut())
                    {
                        run.resume_exit =
                            Some(model::ResumeExit::from_exists(exists.ok().flatten()));
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        for (id, since) in reads {
            let core = self.core.clone();
            let lookup = id.clone();
            cx.spawn(async move |this, cx| {
                let found = cx
                    .background_executor()
                    .spawn(async move { core.discover_lead_session(&lookup, since) })
                    .await;
                let _ = this.update(cx, |this, _| {
                    let Some(run) = this
                        .cards
                        .iter_mut()
                        .find(|card| card.session.as_ref().is_some_and(|s| s.id == id))
                        .and_then(|card| card.lead_run.as_mut())
                    else {
                        return;
                    };
                    // Found: saved by core, nothing left to look for.
                    if matches!(found, Ok(Some(_))) {
                        run.discovery = None;
                    } else if let Some(find) = &mut run.discovery {
                        find.missed(Instant::now());
                    }
                });
            })
            .detach();
        }
        // A dialog or another launch in progress waits its turn.
        if let Some((id, exit)) = early
            && !self.busy
            && self.overlay.is_none()
        {
            match exit {
                model::ResumeExit::Missing => self.restart_lead_fresh(id, window, cx),
                _ => self.rest_exited_lead(id, window, cx),
            }
        }
    }

    /// The card of a Lead by session id, and whether its terminal has focus.
    fn lead_card_state(
        &mut self,
        id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(usize, bool, bool)> {
        let index = self
            .cards
            .iter()
            .position(|card| card.session.as_ref().is_some_and(|s| s.id == id))?;
        if let Some(run) = &mut self.cards[index].lead_run {
            run.resume_exit = None;
        }
        let focused = self
            .lead_chats
            .get(&self.cards[index].project)
            .is_some_and(|chat| self.cards[index].chat && chat.read(cx).focused(window, cx))
            || self.cards[index]
                .active_pane()
                .view
                .focus_handle(cx)
                .is_focused(window);
        Some((
            index,
            self.selection == Some(Selection::Card(index)),
            focused,
        ))
    }

    /// A resumed Lead whose CLI quit at once while its conversation is still
    /// there (or cannot be checked): stop it and let it rest, keeping the
    /// saved id. Selecting it tries again; Forget starts over.
    fn rest_exited_lead(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, _, focused)) = self.lead_card_state(&id, window, cx) else {
            return;
        };
        let Some(project) = self
            .cards
            .iter()
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == id))
            .map(|card| card.project.clone())
        else {
            return;
        };
        let unexpected = self
            .cards
            .iter()
            .find(|c| c.session.as_ref().is_some_and(|s| s.id == id))
            .and_then(|c| c.lead_run.as_ref())
            .is_some_and(|run| run.unexpected_exit);
        let core = self.core.clone();
        let lead_id = id.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.stop_lead(&lead_id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.lead_departed(Some(&id), &project, false, cx);
                        this.message(if unexpected {
                            "The Lead stopped unexpectedly. Select it to resume.".into()
                        } else {
                            model::RESUME_EXIT_KEPT.into()
                        });
                        if focused && this.overlay.is_none() {
                            window.focus(&this.focus, cx);
                        }
                    }
                    Err(e) => this.message(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A Lead whose saved conversation the CLI no longer has (it exited at
    /// once): forget the id, stop it, and start the same CLI once more, fresh.
    /// The new Lead was not resumed, so this cannot repeat. The app, not the
    /// author, starts it, so it touches no overlay and moves focus only if
    /// the old Lead's terminal had it.
    fn restart_lead_fresh(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((index, was_selected, had_focus)) = self.lead_card_state(&id, window, cx) else {
            return;
        };
        let project = self.cards[index].project.clone();
        let Some(preset) = self
            .catalog
            .as_ref()
            .and_then(|c| {
                c.presets
                    .iter()
                    .find(|p| p.id == self.cards[index].launch_preset)
            })
            .cloned()
        else {
            return;
        };
        let core = self.core.clone();
        let (lead_project, lead_id) = (project.clone(), id.clone());
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.stop_lead(&lead_id)?;
                    core.set_lead_session(&lead_project, None)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if result.is_err()
                    || !this
                        .cards
                        .iter()
                        .any(|card| card.session.as_ref().is_some_and(|s| s.id == id))
                {
                    this.message("Could not restart the Lead.".into());
                    cx.notify();
                    return;
                }
                this.lead_departed(Some(&id), &project, false, cx);
                this.message("Could not resume the Lead's conversation. Started a new one.".into());
                if let Some(launch) = this.lead_launch(&project) {
                    // The new card is pushed last; a selected old Lead hands
                    // its selection over.
                    if was_selected {
                        this.selection = Some(Selection::Card(this.cards.len()));
                    }
                    this.begin_launch(
                        project,
                        preset,
                        false,
                        None,
                        Launch::LeadRestart {
                            launch,
                            focus: had_focus,
                        },
                        None,
                        window,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications.refresh_diagnostics();
        // A drag moves the column every frame; the file is written once it ends.
        if let Some(from) = self.column_drag_from
            && !cx.has_active_drag()
        {
            self.column_drag_from = None;
            if from != self.column {
                self.save_settings();
            }
            cx.notify();
        }
        if let Some(from) = self.changes.drag_from
            && !cx.has_active_drag()
        {
            self.changes.drag_from = None;
            if from != self.changes.width {
                self.save_settings();
            }
            cx.notify();
        }
        let now = Instant::now();
        self.observe_dialog(now);
        let mut changed = self.drain_update_notice(cx);
        // The Lead's shika commands run here, with the rest of the state.
        self.drain_control(window, cx);
        self.drain_lead_chats(window, cx);
        self.sync_lead_activity(cx);
        self.lead_upkeep(now, window, cx);
        // Reconcile agent/shell branch renames independently of the one-shot
        // CLI title watch. Git runs off-thread, with only one batch in flight.
        if !self.branch_check_pending
            && !self.cards.is_empty()
            && now.duration_since(self.branch_check_at) >= Duration::from_secs(2)
        {
            self.branch_check_at = now;
            self.branch_check_pending = true;
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .spawn(async move {
                        for session in core.sessions() {
                            // A switched branch is deliberately not adopted.
                            // Close reports errors; polling does not spam toasts.
                            let _ = core.session_refresh_branch(&session.id);
                        }
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.branch_check_pending = false;
                    let mut changed = false;
                    for card in &mut this.cards {
                        if let Some(session) = &mut card.session
                            && let Some(current) = this.core.session(&session.id)
                            && session.branch != current.branch
                        {
                            session.branch = current.branch;
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        let mut rename = vec![];
        // Sessions whose card just turned Ready, for a fresh diff stat.
        let mut ready = vec![];
        // Claude Code and Codex totals to read once, off the UI thread.
        let mut usage_reads = vec![];
        if now.duration_since(self.clock) >= Duration::from_secs(1) {
            self.clock = now;
            changed = self.cards.iter().any(|c| c.status == Status::Working);
            let reduce = appearance::reduce_transparency();
            if reduce != self.reduce_transparency {
                self.reduce_transparency = reduce;
                appearance::apply(&self.appearance, self.background.is_some(), window);
                self.push_terminal_theme(window, cx);
                changed = true;
            }
        }
        for warning in self.notifications.warnings() {
            self.message(warning);
            changed = true;
        }
        // Cancelled cards are removed only between other card operations;
        // existing short async close/shell paths retain their stable indices.
        if !self.busy && self.overlay.is_none() {
            for i in (0..self.cards.len()).rev() {
                let card = &self.cards[i];
                if !card.creating
                    && card.session.is_none()
                    && (card.discard
                        || card
                            .launch_control
                            .as_ref()
                            .is_some_and(|c| c.is_cancelled()))
                {
                    let return_to_cards = self.selection == Some(Selection::Card(i))
                        || card.agent.view.focus_handle(cx).is_focused(window);
                    self.remove_card(i);
                    if return_to_cards {
                        window.focus(&self.focus, cx);
                    }
                    changed = true;
                }
            }
        }
        for card in &mut self.cards {
            if card.chat {
                continue;
            }
            // Sample live terminal chrome, never the user's scrollback. Read
            // the engine before taking HostState so query replies cannot
            // create an inverted lock order.
            let lines = if !card.creating && card.session.is_some() && card.next_agent.is_none() {
                card.agent.terminal.live_text_lines()
            } else {
                Vec::new()
            };
            let screen = activity::detect(
                &card.launch_preset,
                &lines,
                card.agent.terminal.title().as_deref(),
            );
            let mut state = lock(&card.agent.state);
            if card.creating {
                if card
                    .launch_control
                    .as_ref()
                    .is_some_and(|c| c.is_cancelled())
                {
                    state.preparation_stage = "Cancelling setup...".into();
                }
                if !state.preparation_stage.is_empty() && card.stage != state.preparation_stage {
                    card.stage = state.preparation_stage.clone();
                    changed = true;
                }
                continue;
            }
            if card.session.is_none() || card.next_agent.is_some() {
                continue;
            }
            if let Some(title) = state.title.take() {
                if !card
                    .session
                    .as_ref()
                    .is_some_and(|s| s.cli_titled || s.manual_title)
                {
                    card.title = model::card_title(&title);
                }
                card.named = true;
                if let Some(s) = &card.session {
                    rename.push((s.id.clone(), title));
                }
                changed = true;
            }
            // The Lead keeps its title and has no CLI title file to read.
            if state.submission > 0 && card.lead.is_none() {
                if card.launch_preset == "pi" {
                    card.title_watch.start_for(now, TitleWatch::PI_FOR);
                } else {
                    card.title_watch.start(now);
                }
            }
            let submission = if state.submission != card.submitted {
                card.submitted = state.submission;
                if std::mem::take(&mut state.launch_turn) {
                    card.activity.hold_for_launch();
                }
                if matches!(card.status, Status::Waiting | Status::Ready) {
                    // An idle report from the previous turn is not evidence
                    // about a newly submitted prompt.
                    card.lifecycle.submitted(state.lifecycle);
                }
                state.last_submission
            } else {
                None
            };
            if card.launch_preset == "pi"
                && !state.exited
                && !state.lifecycle_checking
                && card
                    .lifecycle_checked
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_millis(500))
                && let Some(session) = &card.session
            {
                card.lifecycle_checked = Some(now);
                state.lifecycle_checking = true;
                let core = self.core.clone();
                let id = session.id.clone();
                let host = card.agent.state.clone();
                let generation = state.submission;
                // Tiny metadata files still belong off the UI thread. A
                // single in-flight read per pane, with stable pane identity.
                cx.background_executor()
                    .spawn(async move {
                        let report = core.session_activity(&id);
                        let mut host = lock(&host);
                        host.note_lifecycle(generation, report);
                    })
                    .detach();
            }
            let lifecycle = card.lifecycle.observe(state.lifecycle);
            let signal = if screen == activity::Signal::Blocked {
                screen
            } else {
                lifecycle.unwrap_or(screen)
            };
            let output = card.evidence.observe(
                &card.launch_preset,
                &lines,
                state.last_output,
                state.last_interaction,
                now,
            );
            let transition = if lifecycle.is_some() && screen != activity::Signal::Blocked {
                card.activity
                    .advance_authoritative(signal, submission, output, now, state.exited)
            } else {
                card.activity
                    .advance(signal, submission, output, now, state.exited)
            };
            card.status = card.activity.status;
            card.since = card.activity.since;
            changed |= transition.changed;
            let exited_now = state.exited && !card.usage_exit_seen;
            if state.exited {
                card.usage_exit_seen = true;
            }
            if matches!(card.launch_preset.as_str(), "claude" | "codex")
                && (transition.ready || exited_now)
                && let Some(worktree) = card
                    .session
                    .as_ref()
                    .map(|session| session.worktree.clone())
            {
                usage_reads.push((card.launch_preset.clone(), worktree));
            }
            if let Some(session) = &card.session {
                // The Lead has no diff to count.
                if transition.ready && card.lead.is_none() {
                    ready.push(session.id.clone());
                }
                if transition.notify {
                    let project = self
                        .projects
                        .iter()
                        .find(|p| p.id == card.project)
                        .map(|p| p.name.as_str())
                        .unwrap_or("Shika");
                    self.notifications.post(
                        &session.id,
                        project,
                        &card.title,
                        Status::Ready.label(),
                        self.notification_sound,
                    );
                }
            }
        }
        let mut title_checks = vec![];
        for card in &mut self.cards {
            if let Some(session) = &card.session
                && card.title_watch.due(now)
            {
                title_checks.push(session.id.clone());
            }
        }
        for id in title_checks {
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let session_id = id.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { core.session_apply_cli_title(&id) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let Some(card) = this
                        .cards
                        .iter_mut()
                        .find(|c| c.session.as_ref().is_some_and(|s| s.id == session_id))
                    else {
                        return;
                    };
                    match result {
                        Ok(Some(mut session)) => {
                            if let Some(current) = this.core.session(&session.id) {
                                session = current;
                            }
                            card.title = model::card_title(&session.title);
                            card.named = true;
                            card.session = Some(session);
                            card.title_watch.finish();
                        }
                        Ok(None) => card.title_watch.checked(Instant::now()),
                        Err(e) => {
                            card.title_watch.finish();
                            this.message(e.to_string());
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        for id in ready {
            // The Changes panel reads again at the same moment, if it shows
            // this task. No other refresh happens while it is open.
            if self.changes.shows(&id) {
                self.fetch_changes(cx);
            }
            self.fetch_diff_stat(id, cx);
        }
        if !usage_reads.is_empty() {
            let core = self.core.clone();
            cx.background_executor()
                .spawn(async move {
                    for (preset, worktree) in usage_reads {
                        core.record_token_usage(&preset, &worktree);
                    }
                })
                .detach();
        }
        self.watch_checks(now, cx);
        for (id, title) in rename {
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { core.session_rename_from_prompt(&id, &title) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(mut session) => {
                            if let Some(current) = this.core.session(&session.id) {
                                session = current;
                            }
                            // A CLI title applied meanwhile is newer.
                            if let Some(card) = this.cards.iter_mut().find(|c| {
                                c.session
                                    .as_ref()
                                    .is_some_and(|s| s.id == session.id && !s.cli_titled)
                            }) {
                                card.session = Some(session);
                            }
                        }
                        Err(e) => this.message(e.to_string()),
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        while let Ok(id) = self.clicks.try_recv() {
            if let Some(i) = self
                .cards
                .iter()
                .position(|c| c.session.as_ref().is_some_and(|s| s.id == id))
            {
                self.selection = Some(Selection::Card(i));
                cx.activate(true);
                window.activate_window();
                // A notification means this agent needs typing. Card focus
                // would make the next letter move the list.
                if self.overlay.is_none() {
                    if self.busy {
                        window.focus(&self.focus, cx);
                    } else {
                        self.focus_terminal(window, cx);
                    }
                }
                changed = true;
            }
        }
        // A Ready card's result counts as seen once its terminal has focus,
        // or once it stays selected in the active window for SEEN_AFTER.
        let on_screen = self
            .selected_card()
            .filter(|_| window.is_window_active())
            .and_then(|i| self.cards[i].session.as_ref().map(|s| (i, s.id.clone())));
        let stayed = self
            .dwell
            .observe(on_screen.as_ref().map(|(_, id)| id.as_str()), now);
        if let Some((i, _)) = on_screen {
            let card = &mut self.cards[i];
            if card.unseen()
                && (stayed
                    || card.active_pane().view.focus_handle(cx).is_focused(window)
                    || (card.chat
                        && self
                            .lead_chats
                            .get(&card.project)
                            .is_some_and(|chat| chat.read(cx).focused(window, cx))))
            {
                card.seen = Some(card.since);
                changed = true;
            }
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, t)| now.duration_since(*t) > Duration::from_secs(7))
        {
            self.toast = None;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
    /// Starts each card's due checks read off the main thread. Cards with
    /// no PR cost nothing here.
    fn watch_checks(&mut self, now: Instant, cx: &mut Context<Self>) {
        for card in &mut self.cards {
            let (Some(session), Some(watch)) = (&card.session, &mut card.pr) else {
                continue;
            };
            let Some(due) = watch.due(now) else {
                continue;
            };
            let core = self.core.clone();
            let id = session.id.clone();
            let (repository, number) = (watch.repository.clone(), watch.number);
            cx.spawn(async move |this, cx| {
                let session_id = id.clone();
                let read = cx
                    .background_executor()
                    .spawn(async move {
                        match due {
                            checks::Due::Poll => checks::Read::Poll(
                                core.session_pr_checks(&id, &repository, number).ok(),
                            ),
                            checks::Due::Ref => {
                                checks::Read::Ref(core.session_pushed_head(&id).ok().flatten())
                            }
                        }
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.checks_read(&session_id, number, read, cx)
                });
            })
            .detach();
        }
    }
    /// A checks read finished. Merged or closed PRs lose their mark; a mark
    /// that turns red posts one notification.
    fn checks_read(&mut self, id: &str, number: u64, read: checks::Read, cx: &mut Context<Self>) {
        let now = Instant::now();
        let Some(card) = self
            .cards
            .iter_mut()
            .find(|c| c.session.as_ref().is_some_and(|s| s.id == id))
        else {
            return;
        };
        let Some(watch) = card.pr.as_mut().filter(|w| w.number == number) else {
            return;
        };
        let changed = match read {
            checks::Read::Ref(head) => watch.ref_read(head, now),
            checks::Read::Poll(result) => {
                let polled = watch.polled(result, now);
                if polled.closed {
                    card.pr = None;
                }
                if polled.failed {
                    let project = self
                        .projects
                        .iter()
                        .find(|p| p.id == card.project)
                        .map(|p| p.name.as_str())
                        .unwrap_or("Shika");
                    self.notifications.post(
                        id,
                        project,
                        &card.title,
                        model::CHECKS_FAILED,
                        self.notification_sound,
                    );
                }
                polled.changed
            }
        };
        if changed {
            cx.notify();
        }
    }
    /// Reads what a Ready task changed, off the main thread. A failure only
    /// leaves the stat hidden.
    fn fetch_diff_stat(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|c| c.session.as_ref().is_some_and(|s| s.id == id))
        {
            card.diff = None;
        }
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let session_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.session_diff_stat(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(card) = this
                    .cards
                    .iter_mut()
                    .find(|c| c.session.as_ref().is_some_and(|s| s.id == session_id))
                    && matches!(card.status, Status::Ready | Status::Asking)
                {
                    card.diff = result.ok();
                    cx.notify();
                }
            });
        })
        .detach();
    }
    /// The picker for one project, from its `+`.
    fn picker_for(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.prefetch_base(project.clone(), cx);
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Picker {
            project,
            index: 0,
            lead: false,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }
    /// Resolves the base branch of each project in `ids`, off the main thread.
    /// A project whose base cannot be read loses its label.
    fn refresh_bases(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move {
                    ids.into_iter()
                        .map(|id| {
                            let base = core.project_base(&id).ok();
                            (id, base)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for (id, base) in resolved {
                    match base {
                        Some(base) => this.bases.insert(id, base),
                        None => this.bases.remove(&id),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Starts fetching the project's base branch while the picker is open,
    /// so New starts from the remote's latest without waiting for it.
    fn prefetch_base(&mut self, project: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = project.clone();
            cx.background_executor()
                .spawn(async move {
                    let _ = core.prefetch_base(&id);
                })
                .await;
            // A branch that was only on origin may exist locally now.
            let _ = this.update(cx, |this, cx| this.refresh_bases(vec![project], cx));
        })
        .detach();
    }
    /// The Base branch dialog for one project, from `b` or its header label.
    fn open_base(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let text = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.base_branch.clone())
            .unwrap_or_default();
        self.refresh_bases(vec![project.clone()], cx);
        self.base_scroll = gpui::ScrollHandle::new();
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Base {
            project: project.clone(),
            text,
            error: None,
            choices: None,
            highlight: None,
        });
        self.load_base_branches(project, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }
    /// Reads branches already on disk. The dialog stays usable while this runs.
    fn load_base_branches(&mut self, project: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = project.clone();
            let listed = cx
                .background_executor()
                .spawn(async move { core.project_branches(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let at = {
                    let Some(Overlay::Base {
                        project: open,
                        text,
                        choices,
                        highlight,
                        ..
                    }) = &mut this.overlay
                    else {
                        return;
                    };
                    if *open != project {
                        return;
                    }
                    let Ok(found) = listed else {
                        return;
                    };
                    let matched = model::branch_matches(&found.names, text).len();
                    *highlight = model::branch_highlight_after_type(matched, text);
                    let at = *highlight;
                    *choices = Some(found);
                    at
                };
                if let Some(at) = at {
                    this.base_scroll.scroll_to_item(at);
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// The branch Enter would save: the highlighted row, or the field.
    fn chosen_base_name(&self) -> String {
        let Some(Overlay::Base {
            text,
            choices,
            highlight,
            ..
        }) = &self.overlay
        else {
            return String::new();
        };
        let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
        let matched = model::branch_matches(names, text);
        let shown: Vec<&str> = matched.iter().map(|index| names[*index].as_str()).collect();
        model::branch_to_apply(text, &shown, *highlight).to_string()
    }
    /// After the field changes, the highlight returns to the first match.
    fn sync_base_highlight(&mut self) {
        let at = {
            let Some(Overlay::Base {
                text,
                choices,
                highlight,
                ..
            }) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::branch_highlight_after_type(matched, text);
            *highlight
        };
        if let Some(at) = at {
            self.base_scroll.scroll_to_item(at);
        }
    }
    fn move_base_highlight(&mut self, delta: isize) {
        let at = {
            let Some(Overlay::Base {
                text,
                choices,
                highlight,
                ..
            }) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::move_branch_highlight(*highlight, matched, delta);
            *highlight
        };
        if let Some(at) = at {
            self.base_scroll.scroll_to_item(at);
        }
    }
    /// Saves `branch` once core finds it, or shows why not.
    /// Empty goes back to the default branch.
    fn apply_base(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Base { project, .. }) = &self.overlay else {
            return;
        };
        let project = project.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let id = project.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.set_project_base(&id, &branch) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(saved) => {
                        if let Some(p) = this.projects.iter_mut().find(|p| p.id == saved.id) {
                            *p = saved;
                        }
                        if matches!(&this.overlay, Some(Overlay::Base { project: open, .. }) if *open == project)
                        {
                            this.overlay = None;
                            this.restore_overlay_focus(window, cx);
                        }
                        this.refresh_bases(vec![project], cx);
                    }
                    Err(e) => {
                        if let Some(Overlay::Base { error, .. }) = &mut this.overlay {
                            *error = Some(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Why Create PR is unavailable for the card at `index`. The shortcut and
    /// a Lead's `shika pr` both ask this, so they never disagree.
    fn publish_blocker(&self, index: usize) -> Option<&'static str> {
        let card = &self.cards[index];
        if card.lead.is_some() {
            Some("The Lead has no branch, so there is nothing to publish.")
        } else if card.creating || card.running() {
            Some("Wait for the agent to finish before publishing.")
        } else if card.lead_publishing {
            Some(control::LEAD_PUBLISHING)
        } else {
            None
        }
    }
    fn create_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if let Some(why) = self.publish_blocker(index) {
            self.message(why.into());
            cx.notify();
            return;
        }
        let card = &self.cards[index];
        let Some(session) = &card.session else {
            return;
        };
        let id = session.id.clone();
        let core = self.core.clone();
        self.overlay_return_focus = window.focused(cx);
        self.busy = true;
        self.message("Preparing PR preview...".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.session_publish_preview(&id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(preview) => {
                        let target = preview.target.clone().unwrap_or_default();
                        let title = preview.title.clone();
                        let row = usize::from(target.is_empty());
                        this.overlay = Some(Overlay::Publish {
                            preview,
                            title,
                            target,
                            row,
                            error: None,
                        });
                        this.toast = None;
                        window.focus(&this.focus, cx);
                    }
                    Err(e) => {
                        this.overlay_return_focus = None;
                        this.message(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn publish_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Publish {
            preview,
            title,
            target,
            error,
            ..
        }) = &mut self.overlay
        else {
            return;
        };
        if title.trim().is_empty()
            || !preview.branches.contains(target)
            || *target == preview.branch
        {
            *error = Some("Enter a title and choose a different, existing target branch.".into());
            cx.notify();
            return;
        }
        if self.cards.iter().any(|c| {
            c.session
                .as_ref()
                .is_some_and(|s| s.id == preview.session_id)
                && c.running()
        }) {
            *error = Some("Wait for the agent to finish before publishing.".into());
            cx.notify();
            return;
        }
        let (preview, title, target) = (preview.clone(), title.clone(), target.clone());
        let session_id = preview.session_id.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { core.session_publish(&preview, &target, &title) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(published) => {
                        this.overlay = None;
                        this.restore_overlay_focus(window, cx);
                        if let Some(card) = this.cards.iter_mut().find(|c| {
                            c.session.as_ref().is_some_and(|s| s.id == session_id)
                        }) {
                            card.pr = checks::PrWatch::new(&published, Instant::now());
                        }
                        this.message(format!("Published PR: {}", published.url));
                        cx.open_url(&published.url);
                    }
                    Err(e) => {
                        if let Some(Overlay::Publish { error, .. }) = &mut this.overlay {
                            *error = Some(format!("{e} Completed commits and pushes were kept. Cancel and reopen Create PR to retry."));
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    /// Why Switch agent is unavailable for the card at `index`.
    fn switch_blocker(&self, index: usize) -> Option<&'static str> {
        let card = &self.cards[index];
        if card.lead.is_some() {
            return Some("The Lead cannot switch agent.");
        }
        if card.creating {
            return Some("Wait for setup to finish before switching agents.");
        }
        if card.closing {
            return Some("This task is closing.");
        }
        if card.lead_publishing {
            return Some(control::LEAD_PUBLISHING);
        }
        if card.session.is_none() {
            return Some("This task has no agent to switch.");
        }
        let host = lock(&card.agent.state);
        if host.has_draft() {
            return Some("Send or clear the unsent text in the agent terminal before switching.");
        }
        if model::activity_requires_confirmation(
            card.status,
            host.exited,
            host.submission != card.submitted,
        ) {
            return Some("Wait for the agent to finish before switching agents.");
        }
        None
    }
    fn switch_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if let Some(why) = self.switch_blocker(index) {
            self.message(why.into());
            cx.notify();
            return;
        }
        let session_id = self.cards[index]
            .session
            .as_ref()
            .map(|session| session.id.clone())
            .unwrap_or_default();
        let current = self.cards[index].launch_preset.clone();
        let listed = self
            .offered_presets()
            .iter()
            .position(|preset| preset.id == current)
            .unwrap_or(0);
        let chrome = self.chrome(window);
        let prompt = cx.new(|cx| name_input::NameInput::new(String::new(), chrome, cx));
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::SwitchAgent {
            session_id,
            prompt,
            index: listed,
            error: None,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn move_switch(&mut self, delta: isize) {
        let len = self.offered_presets().len();
        if len == 0 {
            return;
        }
        if let Some(Overlay::SwitchAgent { index, .. }) = &mut self.overlay {
            *index = (*index as isize + delta).rem_euclid(len as isize) as usize;
        }
    }
    fn switch_error(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(Overlay::SwitchAgent { error, .. }) = &mut self.overlay {
            *error = Some(text);
        }
        cx.notify();
    }
    fn confirm_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (session_id, prompt_text, preset_index) = {
            let Some(Overlay::SwitchAgent {
                session_id,
                prompt,
                index,
                ..
            }) = &self.overlay
            else {
                return;
            };
            if prompt.read(cx).composing() {
                return;
            }
            (
                session_id.clone(),
                prompt.read(cx).value().to_string(),
                *index,
            )
        };
        let Some(card_index) = self.cards.iter().position(|card| {
            card.session
                .as_ref()
                .is_some_and(|session| session.id == session_id)
        }) else {
            self.switch_error("This task has no agent to switch.".into(), cx);
            return;
        };
        if let Some(why) = self.switch_blocker(card_index) {
            self.switch_error(why.into(), cx);
            return;
        }
        let prompt = switch_prompt(&prompt_text).map(str::to_owned);
        if self.catalog.is_none() {
            self.switch_error("Resolving login-shell PATH...".into(), cx);
            return;
        }
        let presets = self.offered_presets();
        let Some(preset) = presets.get(preset_index) else {
            self.switch_error("No agents available.".into(), cx);
            return;
        };
        let preset_id = preset.id.clone();
        let control = if self.cards[card_index].started_by.is_some() {
            match (
                self.cards[card_index].worker_token.clone(),
                self.control.as_ref(),
            ) {
                (Some(token), Some(server)) => Some(WorkerEnv {
                    socket: server.socket(),
                    token,
                    bin_dir: server.bin_dir(),
                }),
                (None, _) => {
                    self.switch_error("This worker has no control token.".into(), cx);
                    return;
                }
                (_, None) => {
                    self.switch_error("Shika's control socket is not running.".into(), cx);
                    return;
                }
            }
        } else {
            None
        };
        let measured = lock(&self.cards[card_index].agent.state)
            .measured
            .unwrap_or(TerminalSize::new(32, 100));
        let pane = Pane::new(
            self.core.clone(),
            true,
            self.terminal_opacity(),
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        );
        pane.terminal.resize(measured);
        let size = PtySize::new(measured.rows, measured.cols);
        let sink = agent_sink(pane.terminal.clone(), pane.state.clone());
        let state = pane.state.clone();
        self.cards[card_index].next_agent = Some(pane);
        let core = self.core.clone();
        let prompt_for_card = prompt.clone();
        let switched_id = session_id.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let session = core.session_switch_agent(
                        &session_id,
                        &preset_id,
                        prompt.as_deref(),
                        size,
                        sink,
                        control,
                    )?;
                    if let Some(pty) = session.pty {
                        bind_host(&core, &state, pty);
                    }
                    Ok::<_, shika_core::Error>(session)
                })
                .await;
            let _ =
                this.update_in(cx, |this, window, cx| {
                    this.busy = false;
                    match result {
                        Ok(session) => {
                            this.install_switched_agent(session, prompt_for_card, window, cx)
                        }
                        Err(err) => {
                            if let Some(card) = this.cards.iter_mut().find(|card| {
                                card.session.as_ref().is_some_and(|s| s.id == switched_id)
                            }) {
                                card.next_agent = None;
                            }
                            this.switch_error(err.to_string(), cx);
                        }
                    }
                });
        })
        .detach();
    }
    fn install_switched_agent(
        &mut self,
        session: Session,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((index, pane)) = self.cards.iter_mut().enumerate().find_map(|(index, card)| {
            card.session
                .as_ref()
                .is_some_and(|current| current.id == session.id)
                .then(|| card.next_agent.take())
                .flatten()
                .map(|pane| (index, pane))
        }) else {
            if matches!(self.overlay, Some(Overlay::SwitchAgent { .. })) {
                self.overlay = None;
                self.overlay_return_focus = None;
            }
            return;
        };
        let now = Instant::now();
        let activity = {
            let mut host = lock(&pane.state);
            apply_switch_turn(&mut host, prompt.as_deref(), !session.cli_titled, now)
        };
        let submitted = lock(&pane.state).submission;
        let card = &mut self.cards[index];
        card.agent = pane;
        card.preset = session.preset_name.clone();
        card.launch_preset = session.preset_id.clone();
        card.usage_exit_seen = false;
        if !card.named {
            card.title = model::card_title(&session.title);
        }
        card.status = activity.status;
        card.since = activity.since;
        card.activity = activity;
        card.submitted = submitted;
        card.evidence = activity::OutputEvidence::default();
        card.lifecycle = lifecycle::Lifecycle::default();
        card.lifecycle_checked = None;
        // A draft task watches the new CLI's own title from its first turn.
        if session.cli_titled {
            card.title_watch.finish();
        } else {
            card.title_watch = TitleWatch::default();
        }
        card.active_tab = 0;
        card.session = Some(session);
        self.overlay = None;
        self.overlay_return_focus = None;
        self.selection = Some(Selection::Card(index));
        self.cards[index].reveal_active_tab();
        let handle = self.cards[index].agent.view.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }
    fn open_card_menu(
        &mut self,
        index: usize,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(card) = self.cards.get(index) else {
            return;
        };
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::CardMenu(CardMenu {
            target: card.agent.view.entity_id(),
            position,
            action: if card.renamable() {
                CardMenuAction::Rename
            } else {
                CardMenuAction::Close
            },
        }));
        // Do not select the card or enter its terminal just to open a menu.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn activate_card_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::CardMenu(menu)) if menu.action == CardMenuAction::Rename => {
                self.rename_task(window, cx)
            }
            Some(Overlay::CardMenu(_)) => self.close_from_card_menu(window, cx),
            _ => {}
        }
    }

    fn rename_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let index = match &self.overlay {
            Some(Overlay::CardMenu(menu)) => {
                menu.target_index(self.cards.iter().map(|card| card.agent.view.entity_id()))
            }
            None => self.selected_card(),
            _ => return,
        };
        let Some(card) = index.and_then(|index| self.cards.get(index)) else {
            return;
        };
        let Some(session) = card.session.as_ref().filter(|_| card.lead.is_none()) else {
            return;
        };
        let target = card.agent.view.entity_id();
        let title = session.title.clone();
        if self.overlay.is_none() {
            self.overlay_return_focus = window.focused(cx);
        }
        let chrome = self.chrome(window);
        let input = cx.new(|cx| name_input::NameInput::new(title, chrome, cx));
        window.focus(&input.focus_handle(cx), cx);
        self.overlay = Some(Overlay::Rename {
            target,
            input,
            error: None,
        });
        cx.notify();
    }

    fn apply_task_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Rename { target, input, .. }) = &self.overlay else {
            return;
        };
        if input.read(cx).composing() {
            return;
        }
        let title = input.read(cx).value().to_string();
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.agent.view.entity_id() == *target)
        else {
            self.cancel_overlay(window, cx);
            return;
        };
        let Some(session) = &self.cards[index].session else {
            self.cancel_overlay(window, cx);
            return;
        };
        match self.core.session_set_title(&session.id, &title) {
            Ok(session) => {
                self.cards[index].title = session.title.clone();
                self.cards[index].named = true;
                self.cards[index].session = Some(session);
                self.overlay = None;
                self.restore_overlay_focus(window, cx);
            }
            Err(error) => {
                if let Some(Overlay::Rename { error: shown, .. }) = &mut self.overlay {
                    *shown = Some(error.to_string());
                }
            }
        }
        cx.notify();
    }

    fn close_from_card_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::CardMenu(menu)) = &self.overlay else {
            return;
        };
        let index = menu.target_index(self.cards.iter().map(|card| card.agent.view.entity_id()));
        self.overlay = None;
        // Close captures its opening focus for its own confirmation flow.
        self.restore_overlay_focus(window, cx);
        if let Some(index) = index {
            let panel_focused = self.changes.open && self.changes.focus.is_focused(window);
            let terminal_focused = !self.focus.is_focused(window) && !panel_focused;
            self.selection = Some(Selection::Card(index));
            if terminal_focused {
                // Never leave a hidden, previously selected terminal focused.
                self.focus_terminal(window, cx);
            }
            self.close(window, cx);
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(project) = self.selected_resting().map(str::to_string) {
            self.open_lead_close(project, None, window, cx);
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating {
            if let Some(control) = &self.cards[index].launch_control {
                control.cancel();
            }
            cx.notify();
            return;
        }
        if self.cards[index].launch_error.is_some() {
            self.remove_card(index);
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        let Some(session) = &self.cards[index].session else {
            return;
        };
        let id = session.id.clone();
        if self.cards[index].lead.is_some() {
            // A Lead is stopped, not closed: its conversation is kept.
            let project = self.cards[index].project.clone();
            self.open_lead_close(project, Some(id), window, cx);
            return;
        }
        if self.cards[index].lead_publishing {
            self.message(control::LEAD_PUBLISHING.into());
            cx.notify();
            return;
        }
        let task_id = id.clone();
        let working = self.cards[index].running();
        let core = self.core.clone();
        self.overlay_return_focus = window.focused(cx);
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match core.session_git_state(&id, working) {
                        Ok(state) => Ok(CloseCheck::Normal(state)),
                        Err(shika_core::Error::TaskBranchChanged { .. }) => core
                            .session_switched_close_check(&id)
                            .map(CloseCheck::Switched),
                        Err(e) => Err(e),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(CloseCheck::Switched(preview)) => {
                        this.overlay = Some(Overlay::SwitchedClose {
                            index,
                            preview,
                            working,
                        });
                        window.focus(&this.focus, cx);
                    }
                    Ok(CloseCheck::Normal(state)) if state.requires_confirmation() => {
                        this.overlay = Some(Overlay::Close { index, state });
                        window.focus(&this.focus, cx);
                    }
                    Ok(_) => this.finish_close(index, 0, window, cx),
                    Err(e) => {
                        this.overlay_return_focus = None;
                        this.lead_dialog_failed(&task_id, e.to_string());
                        this.message(e.to_string());
                    }
                };
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    /// Opens the Lead's close dialog: Stop for a live Lead (`session`), Forget
    /// for a resting one. Reads whether its worktree has changes first.
    fn open_lead_close(
        &mut self,
        project: String,
        session: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        // The same test as Close: an active or blocked turn is stated.
        let working = session.as_ref().is_some_and(|id| {
            self.cards
                .iter()
                .any(|c| c.session.as_ref().is_some_and(|s| &s.id == id) && c.running())
        });
        self.overlay_return_focus = window.focused(cx);
        self.busy = true;
        let core = self.core.clone();
        let lookup = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            let loss = cx
                .background_executor()
                .spawn(async move { core.lead_loss(&lookup).unwrap_or_default() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                let step = if session.is_some() {
                    model::LeadClose::Stop { working, loss }
                } else {
                    model::LeadClose::Forget { working, loss }
                };
                this.overlay = Some(Overlay::LeadClose {
                    project,
                    session,
                    step,
                });
                window.focus(&this.focus, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Stop (`forget` false) or Forget the Lead the dialog is about. Stop
    /// ends its process and keeps the conversation, worktree, and record;
    /// Forget also deletes the worktree and the link to the conversation.
    ///
    /// Once the Lead is stopped its card always leaves (the resting row
    /// appears), whatever Forget then does: a Forget that fails is reported
    /// after, and one that finds the worktree changed since the dialog opened
    /// asks again with the new facts.
    fn finish_lead_close(&mut self, forget: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::LeadClose {
            project,
            session,
            step,
        }) = &self.overlay
        else {
            return;
        };
        let (project, session) = (project.clone(), session.clone());
        // Discard only what the dialog said it would.
        let discard = matches!(step, model::LeadClose::Forget { .. }) && step.discards();
        let working = match step {
            model::LeadClose::Stop { working, .. } | model::LeadClose::Forget { working, .. } => {
                *working
            }
        };
        if let Some(card) = self.cards.iter_mut().find(|c| {
            c.session
                .as_ref()
                .is_some_and(|s| Some(&s.id) == session.as_ref())
        }) {
            card.closing = true;
        }
        let core = self.core.clone();
        let (lead_project, lead_session) = (project.clone(), session.clone());
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if let Some(id) = &lead_session
                        && let Err(e) = core.stop_lead(id)
                    {
                        return LeadCloseOutcome::NotStopped(e);
                    }
                    if !forget {
                        return LeadCloseOutcome::Done;
                    }
                    if !discard {
                        // The tree can change while the dialog is open.
                        match core.lead_loss(&lead_project) {
                            Ok(loss) if loss.any() => return LeadCloseOutcome::Changed(loss),
                            Ok(_) => {}
                            Err(e) => return LeadCloseOutcome::ForgetFailed(e),
                        }
                    }
                    match core.forget_lead(&lead_project, discard) {
                        Ok(()) => LeadCloseOutcome::Done,
                        Err(e) => LeadCloseOutcome::ForgetFailed(e),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match outcome {
                    LeadCloseOutcome::NotStopped(e) => {
                        if let Some(card) = this.cards.iter_mut().find(|c| {
                            c.session
                                .as_ref()
                                .is_some_and(|s| Some(&s.id) == session.as_ref())
                        }) {
                            card.closing = false;
                        }
                        this.message(e.to_string());
                    }
                    outcome => {
                        let forgotten = matches!(outcome, LeadCloseOutcome::Done) && forget;
                        this.lead_departed(session.as_deref(), &project, forgotten, cx);
                        this.overlay = None;
                        match outcome {
                            LeadCloseOutcome::Changed(loss) => {
                                this.overlay = Some(Overlay::LeadClose {
                                    project,
                                    session: None,
                                    step: model::LeadClose::Forget { working, loss },
                                });
                            }
                            LeadCloseOutcome::ForgetFailed(e) => {
                                this.overlay_return_focus = None;
                                this.message(e.to_string());
                            }
                            _ => this.overlay_return_focus = None,
                        }
                        window.focus(&this.focus, cx);
                    }
                }
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// A stopped Lead leaves the column: its card departs and the selection
    /// settles. Stop leaves a resting row in the same place. Forget leaves
    /// none, so the next card takes the selection. Positions can change while
    /// a stop runs, so the card is found again by session id.
    fn lead_departed(
        &mut self,
        session: Option<&str>,
        project: &str,
        forgotten: bool,
        cx: &mut Context<Self>,
    ) {
        let index = session.and_then(|id| {
            self.cards
                .iter()
                .position(|c| c.session.as_ref().is_some_and(|s| s.id == id))
        });
        self.reload_projects();
        if forgotten {
            self.lead_chats.remove(project);
        } else if let Some(chat) = self.lead_chats.get(project) {
            chat.update(cx, |chat, cx| {
                chat.live = false;
                chat.busy = false;
                cx.notify();
            });
        }
        self.leftovers = self.core.leftovers_list().unwrap_or_default();
        let before = self.selection.take();
        let mine = match (&before, index) {
            (Some(Selection::Card(at)), Some(index)) => *at == index,
            (Some(Selection::Resting(p)), _) => p == project,
            _ => false,
        };
        if let Some(index) = index {
            self.depart(index, cx);
        }
        let shifted = match (before, index) {
            (Some(Selection::Card(at)), Some(index)) if at > index => Some(Selection::Card(at - 1)),
            (other, _) => other,
        };
        self.selection = if !mine {
            shifted
        } else if !forgotten {
            Some(Selection::Resting(project.to_string()))
        } else if self.cards.is_empty() {
            Some(Selection::Project(project.to_string()))
        } else {
            Some(Selection::Card(
                index.unwrap_or(0).min(self.cards.len() - 1),
            ))
        };
        self.last_revealed_selection = self.selection.clone();
    }

    /// Right-click on a resting Lead: a menu with one item, Forget Lead.
    fn open_resting_menu(
        &mut self,
        project: String,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::RestingMenu { project, position });
        // Do not select the row just to open a menu.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The menu's Forget Lead: the same dialog Cmd+Shift+W opens.
    fn forget_from_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::RestingMenu { project, .. }) = &self.overlay else {
            return;
        };
        let project = project.clone();
        self.overlay = None;
        let returns_to = self.overlay_return_focus.take();
        self.open_lead_close(project, None, window, cx);
        if self.busy {
            self.overlay_return_focus = returns_to;
        }
    }

    fn finish_close(
        &mut self,
        index: usize,
        action: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(session) = self.cards.get(index).and_then(|c| c.session.as_ref()) else {
            return;
        };
        let id = session.id.clone();
        let task_id = id.clone();
        // Use the same check before showing choices and before removing the
        // task. Redraw bytes and draft typing are never active-turn evidence.
        let working = self.cards[index].running();
        let preview = match &self.overlay {
            Some(Overlay::SwitchedClose { preview, .. }) if action == 3 => Some(preview.clone()),
            _ => None,
        };
        let core = self.core.clone();
        self.busy = true;
        self.cards[index].closing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match action {
                        1 => core.session_discard(&id),
                        2 => core.session_push_and_close(&id),
                        3 => match preview {
                            Some(preview) => core.session_close_switched(&id, &preview),
                            None => Err(shika_core::Error::CloseNeedsConfirmation),
                        },
                        _ => core.session_close(&id, working),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                // Find the card again: positions can change while Close runs.
                let index = this
                    .cards
                    .iter()
                    .position(|c| c.session.as_ref().is_some_and(|s| s.id == task_id));
                match result {
                    Ok(()) => {
                        this.lead_dialog_succeeded(&task_id, control::close_outcome(action).into());
                        if let Some(index) = index {
                            this.depart(index, cx);
                            this.selection = if this.cards.is_empty() {
                                this.projects
                                    .first()
                                    .map(|p| Selection::Project(p.id.clone()))
                            } else {
                                Some(Selection::Card(index.min(this.cards.len() - 1)))
                            };
                            // The replacement takes the closed card's place.
                            // Revealing it would undo manual scrolling during
                            // the row's exit. A Lead restore still reveals.
                            this.last_revealed_selection = this.selection.clone();
                        }
                        this.overlay = None;
                        this.overlay_return_focus = None;
                        window.focus(&this.focus, cx);
                    }
                    Err(e) => {
                        if let Some(index) = index {
                            this.cards[index].closing = false;
                        }
                        this.lead_dialog_failed(&task_id, e.to_string());
                        this.message(e.to_string());
                    }
                };
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn cancel_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.answer_chat_question(true, window, cx);
            return;
        }
        if self.busy
            && matches!(
                self.overlay,
                Some(
                    Overlay::Publish { .. }
                        | Overlay::WorktreeSetup(_)
                        | Overlay::SwitchAgent { .. }
                )
            )
        {
            return;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.finish_worktree_setup(window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::Adjust(_))) {
            self.close_adjust(false, window, cx);
            return;
        }
        self.commit_setting_edit(window, cx);
        self.key_recording = false;
        self.key_note = None;
        let shell = match &self.overlay {
            // A Lead that asked for the dialog gets selection and focus back.
            Some(Overlay::Close { index, state })
                if (state.dirty || state.unpushed) && !self.lead_dialog_is_for(*index) =>
            {
                Some(*index)
            }
            _ => None,
        };
        self.overlay = None;
        if let Some(i) = shell {
            self.overlay_return_focus = None;
            self.selection = Some(Selection::Card(i));
            if let Some(i) = self.selected_card() {
                if self.cards[i].active_tab == 0 {
                    self.toggle(true, window, cx);
                } else {
                    self.focus_terminal(window, cx);
                }
            }
        } else {
            self.restore_overlay_focus(window, cx);
        }
        self.settle_lead_dialog(window, cx);
        cx.notify();
    }
    fn remove_project(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let removed_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.remove_project(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.cards.retain(|c| c.project != removed_id);
                        this.projects.retain(|p| p.id != removed_id);
                        this.lead_chats.remove(&removed_id);
                        this.chat_questions
                            .retain(|(_, project, _)| project != &removed_id);
                        this.bases.remove(&removed_id);
                        this.selection = this
                            .projects
                            .first()
                            .map(|p| Selection::Project(p.id.clone()));
                        this.leftovers = this.core.leftovers_list().unwrap_or_default();
                        this.overlay = None;
                    }
                    Err(e) => this.message(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn remove_leftover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(entry) = self.leftovers.get(index) else {
            return;
        };
        let path = entry.path.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.leftover_remove(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.leftovers.remove(index);
                        this.overlay = if this.leftovers.is_empty() {
                            None
                        } else {
                            Some(Overlay::Leftovers)
                        };
                    }
                    Err(e) => this.message(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }
    /// View > Usage and its shortcut (Cmd+Shift+U until Settings changes
    /// it) open Settings on Usage, or switch an open Settings to it. Settings
    /// already on Usage closes and restores the focus that opened it; any
    /// other overlay keeps the shortcut inert.
    fn toggle_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.key_recording {
            return;
        }
        match &self.overlay {
            Some(Overlay::Settings {
                section: SettingsSection::Usage,
                ..
            }) => self.cancel_overlay(window, cx),
            Some(Overlay::Settings { edit: None, .. }) => {
                self.select_section(SettingsSection::Usage, window, cx)
            }
            Some(_) => {}
            None => {
                self.overlay_return_focus = window.focused(cx);
                self.show_settings(SettingsSection::Usage, window, cx);
            }
        }
    }
    /// Reads the saved store once per Settings; CLI files stay on the
    /// collector.
    fn load_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Settings {
            section: SettingsSection::Usage,
            usage: usage @ None,
            ..
        }) = &mut self.overlay
        else {
            return;
        };
        let view = UsageView::new();
        let request = view.request.clone();
        *usage = Some(Box::new(view));
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    core.recover_token_usage();
                    core.token_usage()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let chrome = this.chrome(window);
                if let Some(view) = this.usage_view_mut()
                    && Arc::ptr_eq(&view.request, &request)
                {
                    view.load(report, &chrome, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn usage_view_mut(&mut self) -> Option<&mut UsageView> {
        match &mut self.overlay {
            Some(Overlay::Settings {
                usage: Some(view), ..
            }) => Some(view),
            _ => None,
        }
    }
    fn set_usage_filter(&mut self, filter: UsageFilter, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &mut self.overlay
        {
            view.set_filter(filter, cx);
        }
        cx.notify();
    }
    fn set_usage_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &mut self.overlay
        {
            view.set_range(range, cx);
        }
        cx.notify();
    }
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.overlay_return_focus = window.focused(cx);
        self.show_settings(SettingsSection::Appearance, window, cx);
    }
    /// Replace whatever overlay is up. The picker uses this to open Agents
    /// without dropping the focus it already saved.
    fn show_settings(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.overlay = Some(Overlay::Settings {
            section,
            row: 0,
            edit: None,
            usage: None,
        });
        self.key_recording = false;
        self.key_note = None;
        self.load_usage(window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }
    /// One step left (`-1`) or right (`1`) on a settings row. Applies and
    /// saves at once so the window is the preview.
    fn step_setting(
        &mut self,
        row: usize,
        delta: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_setting_edit(window, cx);
        if row == MODE_ROW {
            self.set_theme_mode(self.theme.mode.step(delta), window, cx);
            return;
        }
        if row == LIGHT_ROW || row == DARK_ROW {
            let dark = row == DARK_ROW;
            let from = appearance::resolve_theme(&self.theme, dark);
            self.set_theme(dark, appearance::step_theme(from, delta), window, cx);
            return;
        }
        if row == FONT_ROW {
            self.set_font_size(self.font_size.step(delta), cx);
            return;
        }
        if row == SOUND_ROW {
            self.set_notification_sound(delta > 0, cx);
            return;
        }
        let mut next = self.appearance;
        match row {
            OPACITY_ROW => next = next.with_opacity(i64::from(next.opacity) + delta * 5),
            BLUR_ROW => next = next.with_blur(i64::from(next.blur) + delta * 5),
            APPLY_ROW => {
                next.translucency = if delta < 0 {
                    Translucency::Sidebar
                } else {
                    Translucency::SidebarAndTerminal
                }
            }
            _ => return,
        }
        self.set_appearance(next, window, cx);
    }
    /// Select a settings row and start typing into its number. An empty
    /// field shows the current value until a digit arrives.
    fn edit_setting(
        &mut self,
        row: usize,
        digits: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_setting_edit(window, cx);
        if let Some(Overlay::Settings { row: at, edit, .. }) = &mut self.overlay {
            *at = row;
            *edit = Some(digits.to_string());
        }
        cx.notify();
    }
    /// Apply typed digits, pulled into range, or the typed prefix, made
    /// safe for git. Nothing typed keeps a number.
    fn commit_setting_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Settings {
            section, row, edit, ..
        }) = &mut self.overlay
        else {
            return;
        };
        if *section != SettingsSection::Appearance {
            *edit = None;
            return;
        }
        let row = *row;
        if row == PREFIX_ROW {
            if let Some(text) = edit.take() {
                let prefix = shika_core::normalize_branch_prefix(&text);
                if prefix != self.branch_prefix {
                    self.branch_prefix = prefix;
                    self.save_settings();
                }
                cx.notify();
            }
            return;
        }
        if row == FONT_ROW {
            if let Some(text) = edit.take()
                && let Some(size) = FontSize::from_text(&text)
            {
                self.set_font_size(size, cx);
            }
            return;
        }
        let Some(value) = edit.take().and_then(|text| text.parse::<i64>().ok()) else {
            return;
        };
        let next = match row {
            OPACITY_ROW => self.appearance.with_opacity(value),
            BLUR_ROW => self.appearance.with_blur(value),
            _ => return,
        };
        self.set_appearance(next, window, cx);
    }
    fn set_appearance(&mut self, next: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.appearance {
            return;
        }
        self.appearance = next;
        self.reduce_transparency = appearance::reduce_transparency();
        appearance::apply(&next, self.background.is_some(), window);
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }
    /// System, Light, or Dark. A forced mode also sets the app's AppKit
    /// appearance, so the traffic lights and menus match the paint.
    fn set_theme_mode(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.theme.mode {
            return;
        }
        self.theme.mode = mode;
        appearance::apply_mode(mode);
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }
    /// The light or dark pick. It shows at once when that side is painting.
    fn set_theme(
        &mut self,
        dark: bool,
        theme: &'static Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = if dark {
            &mut self.theme.dark
        } else {
            &mut self.theme.light
        };
        if *id == theme.id {
            return;
        }
        *id = theme.id.to_string();
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }
    fn set_font_size(&mut self, next: FontSize, cx: &mut Context<Self>) {
        if next == self.font_size {
            return;
        }
        self.font_size = next;
        let size = px(next.points());
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shells.iter()) {
                pane.view.update(cx, |view, cx| {
                    let mut config = view.config().clone();
                    config.font_size = size;
                    view.set_config(config, cx);
                });
            }
        }
        self.save_settings();
        cx.notify();
    }
    fn set_notification_sound(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.notification_sound {
            return;
        }
        self.notification_sound = on;
        self.save_settings();
        cx.notify();
    }
    /// Hide or show the agent column. Focus stays where it is: with the
    /// column hidden, `j` / `k` still change the agent on screen.
    fn toggle_column(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.column.hidden = !self.column.hidden;
        self.save_settings();
        cx.notify();
    }
    /// The column's and the Changes panel's widths on screen, `None` while
    /// hidden or closed. A window too narrow for the stored widths and a
    /// readable terminal shows less: the panel gives way first, then the
    /// column.
    fn pane_widths(&self, window: &Window) -> (Option<Pixels>, Option<Pixels>) {
        let (column, panel) = changes::pane_widths(
            f32::from(window.viewport_size().width),
            MIN_TERMINAL_WIDTH,
            (!self.column.hidden)
                .then_some(self.column.width(f32::from(window.viewport_size().width))),
            self.changes
                .open
                .then_some(f32::from(self.changes.width.width)),
        );
        (column.map(px), panel.map(px))
    }
    /// The column's width on screen, or `None` while it is hidden.
    fn column_width(&self, window: &Window) -> Option<Pixels> {
        self.pane_widths(window).0
    }
    /// The column's edge follows the pointer and stops at the narrowest
    /// width. A drag never hides the column; only the toggle does.
    fn drag_column(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_active_drag_cursor_style(gpui::CursorStyle::ResizeLeftRight, window);
        self.column_drag_from.get_or_insert(self.column);
        // Beside the Changes panel as shown, the terminal still keeps 420.
        let panel = self.pane_widths(window).1.unwrap_or(px(0.));
        let viewport = window.viewport_size().width;
        let room = viewport - px(MIN_TERMINAL_WIDTH) - panel;
        let next = self
            .column
            .with_width(f32::from(x.min(room)), f32::from(viewport));
        if next != self.column {
            self.column = next;
            cx.notify();
        }
    }
    fn terminal_opacity(&self) -> f32 {
        appearance::terminal_alpha_for(&self.appearance, self.reduce_transparency)
    }
    /// Whether this paint is dark: the forced theme mode, or macOS's
    /// appearance in System mode. Every theme lookup goes through here.
    fn is_dark(&self, window: &Window) -> bool {
        appearance::is_dark_for(self.theme.mode, window.appearance())
    }
    /// The catalog theme painting the window now.
    fn active_theme(&self, window: &Window) -> &'static Theme {
        appearance::resolve_theme(&self.theme, self.is_dark(window))
    }
    fn terminal_palette(&self, window: &Window) -> Palette {
        appearance::terminal_palette(
            &self.appearance,
            self.active_theme(window),
            self.reduce_transparency,
        )
    }
    fn chrome(&self, window: &Window) -> Chrome {
        appearance::chrome_for(
            &self.appearance,
            self.active_theme(window),
            self.reduce_transparency,
        )
    }
    /// Give every live terminal view the current theme's palette and alpha,
    /// and invalidate Settings' cached usage chart before rendering.
    fn push_terminal_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &self.overlay
        {
            view.set_chrome(&self.chrome(window), cx);
        }
        let opacity = self.terminal_opacity();
        let palette = self.terminal_palette(window);
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shells.iter()) {
                pane.view.update(cx, |view, cx| {
                    view.set_palette(palette, cx);
                    view.set_background_opacity(opacity, cx);
                });
            }
        }
    }
    fn save_settings(&mut self) {
        let settings = Settings {
            theme: self.theme.clone(),
            appearance: self.appearance,
            branch_prefix: self.branch_prefix.clone(),
            font_size: self.font_size,
            notification_sound: self.notification_sound,
            column: self.column,
            changes: self.changes.width,
            agents: self.agents.clone(),
            keys: self.keys.clone(),
            background: self.background.clone(),
        };
        if let Err(e) = self.core.save_settings(&settings) {
            self.message(e.to_string());
        }
    }
    /// Which confirmation dialog is on screen, for the input grace window.
    /// Text-entry dialogs, the picker, and Settings have none.
    fn dialog_identity(&self) -> Option<model::DialogId> {
        fn hash(text: &str) -> u64 {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            hasher.finish()
        }
        Some(match self.overlay.as_ref()? {
            Overlay::Close { index, .. } => ("close", *index as u64),
            Overlay::SwitchedClose { index, .. } => ("switched-close", *index as u64),
            Overlay::LeadClose { project, step, .. } => (
                "lead-close",
                hash(project) ^ matches!(step, model::LeadClose::Forget { .. }) as u64,
            ),
            Overlay::RestingMenu { project, .. } => ("resting-menu", hash(project)),
            Overlay::RemoveProject(id) => ("remove-project", hash(id)),
            Overlay::RemoveLeftover(i) => ("remove-leftover", *i as u64),
            Overlay::Preparation { project, .. } => ("preparation", hash(project)),
            Overlay::LeadQuestion(dialog) => ("lead-question", dialog.identity()),
            _ => return None,
        })
    }
    /// Starts the grace window for a confirmation dialog newly on screen.
    /// Called from render, the tick, and every key, so a dialog is noticed
    /// whichever comes first.
    fn observe_dialog(&mut self, now: Instant) {
        let id = self.dialog_identity();
        self.dialog_grace.observe(id, now);
    }
    /// Whether a key is dropped because the confirmation dialog has just
    /// appeared. Escape always works. Mouse clicks are not keys.
    fn dialog_ignores_key(&mut self, key: &str) -> bool {
        let now = Instant::now();
        self.observe_dialog(now);
        key != "escape" && self.dialog_grace.active(now)
    }
    /// The Command chords that choose in a confirmation dialog. True when
    /// the keystroke belonged to one (acted on or deliberately dropped), so
    /// terminals and cards never see it while the dialog is open.
    fn dialog_chord_key(
        &mut self,
        chord: model::DialogChord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use model::DialogChord::{Delete, Discard, Primary};
        if !matches!(
            self.overlay,
            Some(
                Overlay::Close { .. }
                    | Overlay::SwitchedClose { .. }
                    | Overlay::LeadClose { .. }
                    | Overlay::RestingMenu { .. }
                    | Overlay::RemoveProject(_)
                    | Overlay::RemoveLeftover(_)
            )
        ) {
            return false;
        }
        if self.dialog_ignores_key("") || self.busy {
            return true;
        }
        match (&self.overlay, chord) {
            (Some(Overlay::Close { index, .. }), Discard) => {
                let i = *index;
                self.finish_close(i, 1, window, cx);
            }
            (Some(Overlay::Close { index, state }), Primary) if state.can_push() => {
                let i = *index;
                self.finish_close(i, 2, window, cx);
            }
            (Some(Overlay::SwitchedClose { index, .. }), Primary) => {
                let i = *index;
                self.finish_close(i, 3, window, cx);
            }
            (Some(Overlay::LeadClose { .. }), Delete) => self.forget_lead_from_dialog(window, cx),
            (Some(Overlay::RestingMenu { .. }), Delete) => self.forget_from_menu(window, cx),
            (Some(Overlay::RemoveProject(id)), Delete) => {
                let id = id.clone();
                self.remove_project(id, cx);
            }
            (Some(Overlay::RemoveLeftover(i)), Delete) => {
                let i = *i;
                self.remove_leftover(i, cx);
            }
            _ => {}
        }
        true
    }
    /// Forget from the Lead's close dialog. From the Stop step, a worktree
    /// with something to lose first shows the Forget step with those facts;
    /// otherwise, and from the Forget step, the Lead is forgotten.
    fn forget_lead_from_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::LeadClose { step, .. }) = &mut self.overlay else {
            return;
        };
        if matches!(step, model::LeadClose::Stop { .. }) && step.discards() {
            *step = step.forget();
            cx.notify();
        } else {
            self.finish_lead_close(true, window, cx);
        }
    }
    /// Whether the author has stopped typing in every terminal (agent and
    /// shell) and Lead chat input. A dialog that opens on its own waits for
    /// this, so keystrokes meant for a prompt never land in it.
    fn author_quiet(&self, now: Instant, cx: &gpui::App) -> bool {
        let terminals = self.cards.iter().flat_map(|card| {
            std::iter::once(&card.agent)
                .chain(card.shells.iter())
                .map(|pane| lock(&pane.state).last_typed)
        });
        let chats = self
            .lead_chats
            .values()
            .map(|chat| chat.read(cx).last_typed);
        model::typing_quiet(terminals.chain(chats), now, control::BELL_TYPING_QUIET)
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.chat_question_key(event, window, cx);
            return;
        }
        let stroke = &event.keystroke;
        if stroke.modifiers.platform && stroke.key == "q" {
            cx.quit();
            return;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.worktree_setup_key(event, window, cx);
            return;
        }
        if let Some(Overlay::Rename { input, .. }) = &self.overlay {
            // The native text input owns typing, selection, clipboard, and IME.
            if !input.read(cx).composing()
                && !stroke.modifiers.platform
                && !stroke.modifiers.control
                && !stroke.modifiers.alt
            {
                match stroke.key.as_str() {
                    "enter" => self.apply_task_name(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }
            return;
        }
        // Menu typing belongs to the app, never to the previously focused PTY.
        if matches!(self.overlay, Some(Overlay::CardMenu(_))) {
            if !stroke.modifiers.platform && !stroke.modifiers.control && !stroke.modifiers.alt {
                match stroke.key.as_str() {
                    "enter" => self.activate_card_menu(window, cx),
                    "j" | "k" | "down" | "up" | "tab" => {
                        let can_rename = if let Some(Overlay::CardMenu(menu)) = &self.overlay {
                            menu.target_index(
                                self.cards.iter().map(|card| card.agent.view.entity_id()),
                            )
                            .is_some_and(|index| self.cards[index].renamable())
                        } else {
                            false
                        };
                        if can_rename && let Some(Overlay::CardMenu(menu)) = &mut self.overlay {
                            menu.action = if menu.action == CardMenuAction::Rename {
                                CardMenuAction::Close
                            } else {
                                CardMenuAction::Rename
                            };
                            cx.notify();
                        }
                    }
                    "escape" => self.cancel_overlay(window, cx),
                    _ => {}
                }
                cx.stop_propagation();
            }
            return;
        }
        if matches!(self.overlay, Some(Overlay::SwitchAgent { .. })) {
            if self.busy {
                cx.stop_propagation();
                return;
            }
            let (editing, composing) =
                if let Some(Overlay::SwitchAgent { prompt, .. }) = &self.overlay {
                    (
                        prompt.focus_handle(cx).is_focused(window),
                        prompt.read(cx).composing(),
                    )
                } else {
                    (false, false)
                };
            if composing {
                return;
            }
            let plain = !stroke.modifiers.platform
                && !stroke.modifiers.control
                && !stroke.modifiers.alt
                && !stroke.modifiers.shift;
            if editing {
                if plain {
                    let handled = match stroke.key.as_str() {
                        "enter" => {
                            self.confirm_switch(window, cx);
                            true
                        }
                        "escape" => {
                            self.cancel_overlay(window, cx);
                            true
                        }
                        "tab" => {
                            window.focus(&self.focus, cx);
                            true
                        }
                        "up" => {
                            self.move_switch(-1);
                            true
                        }
                        "down" => {
                            self.move_switch(1);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
                return;
            }
            if plain {
                match stroke.key.as_str() {
                    "j" | "down" => self.move_switch(1),
                    "k" | "up" => self.move_switch(-1),
                    "enter" => self.confirm_switch(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    "tab" => {
                        if let Some(Overlay::SwitchAgent { prompt, .. }) = &self.overlay {
                            window.focus(&prompt.focus_handle(cx), cx);
                        }
                    }
                    _ => {}
                }
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // Ctrl+Q leaves a focused terminal. Escape is typed into the program.
        if self.overlay.is_none()
            && !self.focus.is_focused(window)
            && stroke.key == "q"
            && stroke.modifiers.control
            && !stroke.modifiers.platform
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
        {
            window.focus(&self.focus, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(Overlay::Settings {
            section: SettingsSection::Appearance,
            row: PREFIX_ROW,
            edit: Some(text),
            ..
        }) = &mut self.overlay
        {
            match stroke.key.as_str() {
                "enter" => self.commit_setting_edit(window, cx),
                "escape" => {
                    if let Some(Overlay::Settings { edit, .. }) = &mut self.overlay {
                        *edit = None;
                    }
                }
                "backspace" => {
                    text.pop();
                }
                _ => {
                    let typed = stroke.key_char.as_deref().filter(|_| {
                        !stroke.modifiers.platform
                            && !stroke.modifiers.control
                            && !stroke.modifiers.alt
                    });
                    if let Some(ch) = typed.and_then(|t| t.chars().next())
                        && (ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
                        && text.len() < 40
                    {
                        text.push(ch);
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if matches!(&self.overlay, Some(Overlay::Publish { .. })) {
            if !self.busy {
                match stroke.key.as_str() {
                    "enter" => self.publish_pr(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    "tab" => {
                        if let Some(Overlay::Publish { row, .. }) = &mut self.overlay {
                            *row = 1 - *row;
                        }
                    }
                    _ => {
                        if let Some(Overlay::Publish {
                            row,
                            title,
                            target,
                            error,
                            ..
                        }) = &mut self.overlay
                        {
                            let value = if *row == 0 { title } else { target };
                            if stroke.key == "backspace" {
                                value.pop();
                            } else if !stroke.modifiers.platform
                                && !stroke.modifiers.control
                                && !stroke.modifiers.alt
                                && let Some(typed) = &stroke.key_char
                            {
                                for ch in typed.chars().filter(|c| !c.is_control()) {
                                    if value.len() < 256 {
                                        value.push(ch);
                                    }
                                }
                            }
                            *error = None;
                        }
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if matches!(&self.overlay, Some(Overlay::Base { .. })) {
            if !self.busy {
                let plain = !stroke.modifiers.platform
                    && !stroke.modifiers.control
                    && !stroke.modifiers.alt
                    && !stroke.modifiers.shift;
                match stroke.key.as_str() {
                    "enter" => {
                        let branch = self.chosen_base_name();
                        self.apply_base(branch, window, cx);
                    }
                    "escape" => self.cancel_overlay(window, cx),
                    "up" if plain => self.move_base_highlight(-1),
                    "down" if plain => self.move_base_highlight(1),
                    "backspace" => {
                        let changed =
                            if let Some(Overlay::Base { text, error, .. }) = &mut self.overlay {
                                let changed = text.pop().is_some();
                                if changed {
                                    *error = None;
                                }
                                changed
                            } else {
                                false
                            };
                        if changed {
                            self.sync_base_highlight();
                        }
                    }
                    _ => {
                        let typed = stroke.key_char.as_deref().filter(|_| {
                            !stroke.modifiers.platform
                                && !stroke.modifiers.control
                                && !stroke.modifiers.alt
                        });
                        let changed = if let Some(ch) = typed.and_then(|t| t.chars().next())
                            && model::branch_char(ch)
                        {
                            if let Some(Overlay::Base { text, error, .. }) = &mut self.overlay
                                && text.len() < model::BRANCH_MAX
                            {
                                text.push(ch);
                                *error = None;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if changed {
                            self.sync_base_highlight();
                        }
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.recording_shortcut() {
            self.capture_shortcut(stroke, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(chord) = model::dialog_chord(
            &stroke.key,
            stroke.modifiers.platform,
            stroke.modifiers.control,
            stroke.modifiers.alt,
            stroke.modifiers.shift,
        ) && self.dialog_chord_key(chord, window, cx)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if stroke.modifiers.platform
            || stroke.modifiers.control
            || stroke.modifiers.alt
            || stroke.modifiers.shift
        {
            return;
        }
        let key = stroke.key.as_str();
        if self.overlay.is_some() {
            if self.dialog_ignores_key(key) {
                cx.stop_propagation();
                return;
            }
            if self.busy {
                return;
            }
            let offered_len = self.offered_presets().len();
            let catalog_ready = self.catalog.is_some();
            let settings_nav = match &self.overlay {
                Some(Overlay::Settings {
                    section, row, edit, ..
                }) => Some((*section, *row, edit.is_some())),
                _ => None,
            };
            if let Some((section, at, editing)) = settings_nav {
                if !editing && matches!(key, "[" | "]") {
                    let delta = if key == "]" { 1 } else { -1 };
                    self.select_section(section.step(delta), window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Agents {
                    self.on_agents_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Projects {
                    self.on_projects_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Keyboard {
                    self.on_keyboard_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if section == SettingsSection::Usage {
                    self.on_usage_key(key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing
                    && section == SettingsSection::Appearance
                    && self.on_background_key(at, key, window, cx)
                {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
            }
            // A project the picker moved to, to fetch its base branch.
            let mut moved_to = None;
            match &mut self.overlay {
                Some(Overlay::Picker { index, project, .. }) => {
                    let len = offered_len;
                    let chosen = key.parse::<usize>().ok().filter(|n| (1..=len).contains(n));
                    match key {
                        "j" | "down" if len > 0 => *index = (*index + 1) % len,
                        "k" | "up" if len > 0 => *index = (*index + len - 1) % len,
                        "enter" => {
                            if catalog_ready && len == 0 {
                                self.show_settings(SettingsSection::Agents, window, cx);
                            } else {
                                self.launch(window, cx);
                            }
                        }
                        "escape" => self.cancel_overlay(window, cx),
                        "tab" => {
                            if let Some(at) = self.projects.iter().position(|p| &p.id == project) {
                                *project = self.projects[(at + 1) % self.projects.len()].id.clone();
                                moved_to = Some(project.clone());
                            }
                        }
                        _ => {
                            if let Some(n) = chosen {
                                *index = n - 1;
                                self.launch(window, cx);
                            }
                        }
                    }
                }
                Some(Overlay::Adjust(_)) => self.adjust_key(key, window, cx),
                Some(Overlay::Preparation { .. }) => match key {
                    "enter" => self.approve_preparation(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    _ => {}
                },
                // Escape, and Enter to Stop the Lead, are the only plain keys
                // here. The destructive and publishing choices are Command
                // chords, handled by `dialog_chord_key`.
                Some(Overlay::Close { .. } | Overlay::RestingMenu { .. }) => {
                    if key == "escape" {
                        self.cancel_overlay(window, cx);
                    }
                }
                Some(Overlay::LeadClose { step, .. }) => {
                    let stopping = matches!(step, model::LeadClose::Stop { .. });
                    match key {
                        "enter" if stopping => self.finish_lead_close(false, window, cx),
                        "escape" => self.cancel_overlay(window, cx),
                        _ => {}
                    }
                }
                Some(Overlay::SwitchedClose { .. }) => {
                    if key == "escape" {
                        self.cancel_overlay(window, cx);
                    }
                }
                Some(Overlay::RemoveLeftover(_)) => {
                    if key == "escape" {
                        self.overlay = Some(Overlay::Leftovers);
                    }
                }
                Some(Overlay::Leftovers) => {
                    let len = self.leftovers.len();
                    match key {
                        "j" | "down" if len > 0 => {
                            self.leftover_selected = (self.leftover_selected + 1) % len
                        }
                        "k" | "up" if len > 0 => {
                            self.leftover_selected = (self.leftover_selected + len - 1) % len
                        }
                        "d" | "enter" if len > 0 => {
                            self.overlay =
                                Some(Overlay::RemoveLeftover(self.leftover_selected.min(len - 1)))
                        }
                        "escape" => self.cancel_overlay(window, cx),
                        _ => {}
                    }
                }
                Some(Overlay::RemoveProject(_)) => {
                    if key == "escape" {
                        self.cancel_overlay(window, cx);
                    }
                }
                Some(Overlay::Settings { row, edit, .. }) => {
                    let at = *row;
                    let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
                    let number_row = matches!(at, OPACITY_ROW | BLUR_ROW | FONT_ROW);
                    let max_len = if at == FONT_ROW { 4 } else { 3 };
                    match (edit.as_mut(), key) {
                        (Some(text), _) if digit || (at == FONT_ROW && key == ".") => {
                            let accept = if key == "." {
                                !text.is_empty() && !text.contains('.')
                            } else {
                                true
                            };
                            if accept && text.len() < max_len {
                                text.push_str(key);
                            }
                        }
                        (Some(text), "backspace") => {
                            text.pop();
                        }
                        (Some(_), "escape") => *edit = None,
                        (Some(_), "enter") => self.commit_setting_edit(window, cx),
                        (Some(_), "h" | "l" | "left" | "right") => {}
                        (None, _) if digit && number_row => self.edit_setting(at, key, window, cx),
                        (None, "backspace") if number_row => self.edit_setting(at, "", window, cx),
                        (None, "enter") if at == PREFIX_ROW => {
                            let prefix = self.branch_prefix.clone();
                            self.edit_setting(at, &prefix, window, cx)
                        }
                        (None, "h" | "left") => self.step_setting(at, -1, window, cx),
                        (None, "l" | "right") => self.step_setting(at, 1, window, cx),
                        (None, "enter" | "escape") => self.cancel_overlay(window, cx),
                        (_, "j" | "down" | "tab") => {
                            let next = self.next_appearance_row(at, 1);
                            self.select_setting(next, window, cx)
                        }
                        (_, "k" | "up") => {
                            let next = self.next_appearance_row(at, -1);
                            self.select_setting(next, window, cx)
                        }
                        _ => {}
                    }
                }
                // Typed into above, before the modifier check.
                Some(Overlay::LeadQuestion(_))
                | Some(Overlay::WorktreeSetup(_))
                | Some(Overlay::Base { .. })
                | Some(Overlay::Publish { .. })
                | Some(Overlay::CardMenu(_))
                | Some(Overlay::Rename { .. })
                | Some(Overlay::SwitchAgent { .. })
                | None => {}
            }
            if let Some(project) = moved_to {
                self.prefetch_base(project, cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !self.focus.is_focused(window) {
            return;
        }
        match key {
            "j" | "down" => self.move_selection(1, window, cx),
            "k" | "up" => self.move_selection(-1, window, cx),
            "a" => self.add_project(cx),
            "r" => self.retry_preparation(window, cx),
            "enter" => match self.selection.clone() {
                Some(Selection::Resting(project)) => self.resume_lead(project, window, cx),
                _ => self.focus_terminal(window, cx),
            },
            "b" => {
                if self.busy || self.selection.is_none() {
                    return;
                }
                if let Some(project) = self.project_id() {
                    self.open_base(project, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}
/// Settings rows, top to bottom: the theme mode, the light and dark themes,
/// the background image and its effect (hidden without an image), opacity,
/// blur, translucency, font size, the branch prefix, then notification sound.
const MODE_ROW: usize = 0;
const LIGHT_ROW: usize = 1;
const DARK_ROW: usize = 2;
const IMAGE_ROW: usize = 3;
const EFFECT_ROW: usize = 4;
const OPACITY_ROW: usize = 5;
const BLUR_ROW: usize = 6;
const APPLY_ROW: usize = 7;
const FONT_ROW: usize = 8;
const PREFIX_ROW: usize = 9;
const SOUND_ROW: usize = 10;
const SETTING_ROWS: usize = 11;
/// Wide enough for the longest catalog name, such as "Catppuccin Macchiato".
const THEME_FIELD_WIDTH: f32 = 168.;
/// Tall enough for the appearance rows and the usage chart. A short window
/// uses less, and the rows scroll with a scrollbar.
const SETTINGS_HEIGHT: f32 = 580.;
const SETTINGS_NAV_WIDTH: f32 = 148.;
/// One size for every section, so switching never resizes the dialog: the
/// usage year calendar beside the section list, inside the rows' 10px inset,
/// the 12px gap and the dialog's padding.
const SETTINGS_WIDTH: f32 = usage_dashboard::WIDTH + 20. + SETTINGS_NAV_WIDTH + 12. + 40.;
// It fits the app's 960px minimum window with the dialog's 40px margin.
const _: () = assert!(SETTINGS_WIDTH <= 960. - 40.);
impl Shika {
    /// Move the Settings selection, applying any typed value first, and keep
    /// the row on screen. The rows are the children of the scrolling list.
    fn select_setting(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_setting_edit(window, cx);
        self.key_recording = false;
        self.key_note = None;
        if let Some(Overlay::Settings { row, .. }) = &mut self.overlay {
            *row = to;
            self.settings_scroll.scroll_to_item(to);
        }
        cx.notify();
    }
    /// Switch section. A typed value is applied first. The same section keeps
    /// its row.
    fn select_section(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.commit_setting_edit(window, cx);
        self.key_recording = false;
        self.key_note = None;
        if let Some(Overlay::Settings {
            section: at,
            row,
            edit,
            ..
        }) = &mut self.overlay
            && *at != section
        {
            *at = section;
            *row = 0;
            *edit = None;
            self.settings_scroll.scroll_to_item(0);
        }
        self.load_usage(window, cx);
        cx.notify();
    }
    fn set_agent_enabled(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if self.agents.enabled(id) == on {
            return;
        }
        self.agents.set_enabled(id, on);
        self.save_settings();
        cx.notify();
    }
    fn set_agent_row(&mut self, row: usize, on: bool, cx: &mut Context<Self>) {
        let Some(id) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.get(row))
            .map(|preset| preset.id.clone())
        else {
            return;
        };
        self.set_agent_enabled(&id, on, cx);
    }
    fn on_agents_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.presets.len())
            .unwrap_or(0);
        match key {
            "j" | "down" | "tab" if count > 0 => {
                self.select_setting((row + 1) % count, window, cx);
            }
            "k" | "up" if count > 0 => {
                self.select_setting((row + count - 1) % count, window, cx);
            }
            "h" | "left" => self.set_agent_row(row, false, cx),
            "l" | "right" => self.set_agent_row(row, true, cx),
            "enter" | "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }
    /// The provider row steps like a segment, stopping at either end; the
    /// range has a letter each.
    fn on_usage_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let filter = self.usage_view_mut().map(|view| view.filter);
        match key {
            "h" | "left" | "l" | "right" => {
                if let Some(filter) = filter {
                    let step = if matches!(key, "h" | "left") { -1 } else { 1 };
                    self.set_usage_filter(filter.step(step), cx);
                }
            }
            "enter" | "escape" => self.cancel_overlay(window, cx),
            _ => {
                if let Some(range) = UsageRange::from_key(key) {
                    self.set_usage_range(range, cx);
                }
            }
        }
    }
    fn recording_shortcut(&self) -> bool {
        self.key_recording
            && matches!(
                self.overlay,
                Some(Overlay::Settings {
                    section: SettingsSection::Keyboard,
                    ..
                })
            )
    }
    fn begin_key_recording(&mut self, row: usize, cx: &mut Context<Self>) {
        if !matches!(
            self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                ..
            })
        ) {
            return;
        }
        if let Some(Overlay::Settings { row: at, .. }) = &mut self.overlay {
            *at = row;
            self.settings_scroll.scroll_to_item(row);
        }
        self.key_recording = true;
        self.key_note = None;
        cx.notify();
    }
    fn clear_shortcut(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(command) = shortcuts::COMMANDS.get(row) else {
            return;
        };
        match shortcuts::assign(&self.keys, command.id, None) {
            Ok(next) => {
                self.keys = next;
                self.key_note = None;
                self.save_settings();
                install_keys(cx, &self.keys);
                cx.notify();
            }
            Err(text) => {
                self.key_note = Some(text);
                cx.notify();
            }
        }
    }
    fn capture_shortcut(&mut self, stroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        let plain = !stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift;
        if plain && stroke.key == "escape" {
            self.key_recording = false;
            self.key_note = None;
            return;
        }
        let Some(command) = self
            .keyboard_row()
            .and_then(|row| shortcuts::COMMANDS.get(row))
        else {
            self.key_recording = false;
            return;
        };
        let id = command.id;
        match shortcuts::chord_from_keystroke(stroke) {
            Ok(chord) => match shortcuts::assign(&self.keys, id, Some(chord)) {
                Ok(next) => {
                    self.keys = next;
                    self.key_recording = false;
                    self.key_note = None;
                    self.save_settings();
                    install_keys(cx, &self.keys);
                }
                Err(text) => self.key_note = Some(text),
            },
            Err(text) => self.key_note = Some(text),
        }
    }
    fn keyboard_row(&self) -> Option<usize> {
        match self.overlay {
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                row,
                ..
            }) => Some(row),
            _ => None,
        }
    }
    fn on_keyboard_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = shortcuts::COMMANDS.len();
        match key {
            "j" | "down" | "tab" if count > 0 => {
                self.select_setting((row + 1) % count, window, cx);
            }
            "k" | "up" if count > 0 => {
                self.select_setting((row + count - 1) % count, window, cx);
            }
            "enter" => self.begin_key_recording(row, cx),
            "backspace" | "delete" => self.clear_shortcut(row, cx),
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }
    /// System, Light, or Dark. `h` / `l` step one segment.
    fn mode_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == MODE_ROW
        );
        let shadow = chrome.control_shadow;
        let choice = |id: &'static str, label: &'static str, mode: ThemeMode| {
            let chosen = self.theme.mode == mode;
            segment(id, label, chosen, chrome.raised, chrome.ink_1, chrome.ink_3)
                .when(chosen, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_theme_mode(mode, window, cx);
                    this.select_setting(MODE_ROW, window, cx);
                }))
        };
        list_row(selected, chrome)
            .justify_between()
            .child("Theme")
            .child(
                div()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(choice("theme-mode-system", "System", ThemeMode::System))
                    .child(choice("theme-mode-light", "Light", ThemeMode::Light))
                    .child(choice("theme-mode-dark", "Dark", ThemeMode::Dark)),
            )
    }
    /// The light or dark theme: minus, the theme's name, plus, like the
    /// number rows. `h` / `l` step through that side of the catalog.
    fn theme_row(
        &self,
        row: usize,
        label: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row: at, .. }) if *at == row
        );
        let name = appearance::resolve_theme(&self.theme, row == DARK_ROW).name;
        let step = |delta: i64| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.step_setting(row, delta, window, cx);
                this.select_setting(row, window, cx);
            })
        };
        let field = text_field(
            SharedString::from(format!("setting-{row}-value")),
            THEME_FIELD_WIDTH,
            false,
            chrome,
        )
        .font_family(UI_FONT)
        .cursor_default()
        .child(div().min_w_0().truncate().child(name))
        .on_click(cx.listener(move |this, _, window, cx| this.select_setting(row, window, cx)));
        list_row(selected, chrome)
            .justify_between()
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-less")),
                            "-",
                            chrome,
                        )
                        .on_click(step(-1)),
                    )
                    .child(field)
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-more")),
                            "+",
                            chrome,
                        )
                        .on_click(step(1)),
                    ),
            )
    }
    /// The branch prefix row: a text field. Enter or a click starts typing.
    fn prefix_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row, edit, .. }) => (
                *row == PREFIX_ROW,
                edit.as_deref().filter(|_| *row == PREFIX_ROW),
            ),
            _ => (false, None),
        };
        let text = edit.unwrap_or(&self.branch_prefix).to_string();
        let empty = text.is_empty();
        let field = text_field("setting-prefix-value", 168., edit.is_some(), chrome)
            .when(!empty, |d| d.child(text))
            .when(edit.is_some(), |d| {
                d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
            })
            .when(empty && edit.is_none(), |d| {
                d.child(div().text_color(chrome.ink_4).child("none"))
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if !matches!(
                    this.overlay,
                    Some(Overlay::Settings {
                        row: PREFIX_ROW,
                        edit: Some(_),
                        ..
                    })
                ) {
                    let prefix = this.branch_prefix.clone();
                    this.edit_setting(PREFIX_ROW, &prefix, window, cx);
                }
            }));
        list_row(selected, chrome)
            .justify_between()
            .child("Branch prefix")
            .child(field)
    }
    /// Off or On. `h` turns the alert off, `l` turns it on.
    fn sound_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == SOUND_ROW
        );
        let on = self.notification_sound;
        let shadow = chrome.control_shadow;
        let choice = |id: &'static str, label: &'static str, chosen: bool, delta: i64| {
            segment(id, label, chosen, chrome.raised, chrome.ink_1, chrome.ink_3)
                .when(chosen, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.step_setting(SOUND_ROW, delta, window, cx);
                    if let Some(Overlay::Settings { row, .. }) = &mut this.overlay {
                        *row = SOUND_ROW;
                    }
                    cx.notify();
                }))
        };
        list_row(selected, chrome)
            .justify_between()
            .child("Notification sound")
            .child(
                div()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(choice("notification-sound-off", "Off", !on, -1))
                    .child(choice("notification-sound-on", "On", on, 1)),
            )
    }
    /// A settings row: minus, a number field that takes typed digits, plus.
    fn setting_row(
        &self,
        row: usize,
        label: &'static str,
        value: &str,
        unit: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row: at, edit, .. }) => {
                (*at == row, edit.as_deref().filter(|_| *at == row))
            }
            _ => (false, None),
        };
        let step = |delta: i64| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.step_setting(row, delta, window, cx);
                if let Some(Overlay::Settings { row: at, .. }) = &mut this.overlay {
                    *at = row;
                }
            })
        };
        let text = match edit {
            Some("") | None => value.to_string(),
            Some(digits) => digits.to_string(),
        };
        let field = text_field(
            SharedString::from(format!("setting-{row}-value")),
            76.,
            edit.is_some(),
            chrome,
        )
        .justify_end()
        .child(
            div()
                .when(edit == Some(""), |d| d.text_color(chrome.ink_4))
                .child(text),
        )
        .when(edit.is_some(), |d| {
            d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
        })
        .child(div().text_color(chrome.ink_4).child(unit))
        .on_click(cx.listener(move |this, _, window, cx| this.edit_setting(row, "", window, cx)));
        list_row(selected, chrome)
            .justify_between()
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-less")),
                            "-",
                            chrome,
                        )
                        .on_click(step(-1)),
                    )
                    .child(field)
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-more")),
                            "+",
                            chrome,
                        )
                        .on_click(step(1)),
                    ),
            )
    }

    /// Cancel, or Done in Settings. Reads "Working..." while a close runs.
    fn cancel_button(
        &self,
        label: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        dialog_button(
            "cancel",
            if self.busy { "Working..." } else { label },
            "esc",
            chrome,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            if !this.busy {
                this.cancel_overlay(window, cx);
            }
        }))
    }

    /// Empty space in a 48px top row drags the window, and a double-click
    /// zooms. The drag starts on the first move, so a double-click can still
    /// zoom. Buttons in the row occlude it, so pressing them never drags.
    fn title_drag<E: InteractiveElement>(&self, element: E, cx: &mut Context<Self>) -> E {
        element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, _| {
                    if event.click_count >= 2 {
                        this.title_drag = false;
                        window.titlebar_double_click();
                    } else {
                        this.title_drag = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.title_drag = false;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.title_drag = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.title_drag {
                    this.title_drag = false;
                    window.start_window_move();
                }
            }))
    }

    /// An invisible strip over the column's edge. Dragging it resizes the
    /// column, and a double-click puts back the default width. Its line shows
    /// on hover and for the length of a drag.
    fn column_handle(
        &self,
        width: Pixels,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let line = chrome.ink_4;
        div()
            .id("column-handle")
            .group("column-handle")
            .occlude()
            .absolute()
            .top_0()
            .bottom_0()
            .left(width - px(COLUMN_HANDLE_WIDTH / 2.))
            .w(px(COLUMN_HANDLE_WIDTH))
            .flex()
            .justify_center()
            .cursor_col_resize()
            .on_drag(ColumnDrag, |_, _, _, cx| cx.new(|_| ColumnDrag))
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.column = this.column.reset();
                    this.save_settings();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .when(self.column_drag_from.is_some(), |d| d.bg(line))
                    .group_hover("column-handle", move |style| style.bg(line)),
            )
    }

    /// The button that hides the column, in its top row, or shows it again,
    /// in the terminal's top row after the traffic lights.
    fn column_toggle(
        &self,
        ink: Rgba,
        ink_hover: Rgba,
        hover: Rgba,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = if self.column.hidden {
            "Show agent column  \u{2318}B"
        } else {
            "Hide agent column  \u{2318}B"
        };
        div()
            .id("column-toggle")
            .group("column-toggle")
            .occlude()
            .flex_none()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |_, cx| {
                cx.new(|_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: tip.into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_column(cx)))
            .child(
                gpui::svg()
                    .data(COLUMN_ICON)
                    .size(px(15.))
                    .text_color(ink)
                    .group_hover("column-toggle", move |style| style.text_color(ink_hover)),
            )
    }

    /// Keep the app name and column toggle together on every title surface.
    fn title_identity(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .child("Shika"),
            )
            .child(self.column_toggle(chrome.ink_3, chrome.ink_1, chrome.hover, chrome, cx))
    }

    /// Where the title bar's content starts on the left: after the traffic
    /// lights, or near the edge in full screen, where macOS hides them.
    fn title_inset(window: &Window) -> Pixels {
        if window.is_fullscreen() || window.is_simple_fullscreen() {
            px(18.)
        } else {
            px(WORDMARK_INSET)
        }
    }

    /// The sidebar half of the title bar: the real traffic lights, the
    /// wordmark, and the column toggle beside it. The system title is hidden.
    /// New agent sits on the Projects header and the gear in the footer.
    fn top_row(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inset = Self::title_inset(window);
        let row = div()
            .id("titlebar")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(inset)
            .pr(px(14.))
            .child(self.title_identity(chrome, cx));
        self.title_drag(row, cx)
    }

    /// The Projects header: its label, then Add project and New agent, the
    /// `+` lining up with each project header's `+` below it.
    fn projects_header(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        section_header("Projects", chrome)
            .child(
                icon_button(
                    "add-project",
                    Some(FOLDER_PLUS_ICON),
                    "Add project  a",
                    chrome,
                )
                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
            )
            .child(
                icon_button("new-agent", None, "New agent  \u{2318}N", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.picker(window, cx))),
            )
    }

    /// One project: its header, then the author's own cards. `indices` is
    /// that list: not a Lead, and not a worker of a live Lead.
    #[allow(clippy::too_many_arguments)]
    fn project_group(
        &self,
        project: &Project,
        indices: Vec<usize>,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = project.id.clone();
        let selected = self.selection == Some(Selection::Project(id.clone()));
        let plus_kept = self.selected_project() == Some(id.as_str());
        let group = SharedString::from(format!("project-group-{id}"));
        let hover = chrome.hover;
        let ink_1 = chrome.ink_1;
        let select_id = id.clone();
        let remove_id = id.clone();
        let new_id = id.clone();
        let base_id = id.clone();
        let base_name = self.bases.get(&id).and_then(|b| b.name.clone());
        let base_group = SharedString::from(format!("base-group-{id}"));
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let path = SharedString::from(model::tilde(&project.path, home));
        let header = div()
            .id(SharedString::from(format!("project-{id}")))
            .relative()
            .when(selected && reveal_selection, |d| {
                d.child(self.selection_reveal(cx))
            })
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(8.))
            .pr(px(4.))
            .min_h(px(24.))
            .rounded(px(6.))
            .when(selected, |d| d.bg(chrome.row_selected))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy || this.overlay.is_some() {
                    return;
                }
                this.selection = Some(Selection::Project(select_id.clone()));
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            // The path is in the name's tooltip, not on the row.
            .child(
                div()
                    .id(SharedString::from(format!("name-{id}")))
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .tooltip(move |_, cx| {
                        cx.new(|_| KeyTip {
                            bg: tip_bg,
                            fg: tip_fg,
                            text: path.clone(),
                        })
                        .into()
                    })
                    .child(project.name.clone()),
            )
            // The branch New starts from. A click or `b` changes it.
            .when_some(base_name, |d, name| {
                d.child(
                    div()
                        .id(SharedString::from(format!("base-{id}")))
                        .group(base_group.clone())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(chrome.ink_3)
                        .hover(move |style| style.text_color(ink_1))
                        .tooltip(move |_, cx| {
                            cx.new(|_| KeyTip {
                                bg: tip_bg,
                                fg: tip_fg,
                                text: "Base branch  b".into(),
                            })
                            .into()
                        })
                        // The glyph tells the branch apart from the path before it.
                        .child(
                            gpui::svg()
                                .data(BRANCH_ICON)
                                .flex_none()
                                .size(px(12.))
                                .text_color(chrome.ink_3)
                                .group_hover(base_group, move |style| style.text_color(ink_1)),
                        )
                        .child(name)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_base(base_id.clone(), window, cx);
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id(SharedString::from(format!("forget-{id}")))
                    .flex_none()
                    .px(px(4.))
                    .rounded(px(4.))
                    .text_size(px(11.5))
                    .text_color(chrome.ink_3)
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
                    .hover(move |style| style.text_color(ink_1))
                    .child("Remove")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if !this.busy && this.overlay.is_none() {
                            this.overlay = Some(Overlay::RemoveProject(remove_id.clone()));
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("new-in-{id}")))
                    .flex_none()
                    .size(px(24.))
                    // Hidden, the slot keeps its width so nothing moves.
                    .when(!plus_kept, |d| {
                        d.opacity(0.)
                            .group_hover(group.clone(), |style| style.opacity(1.))
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(16.))
                    .text_color(chrome.ink_3)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover).text_color(ink_1))
                    .child("+")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.picker_for(new_id.clone(), window, cx);
                    })),
            );
        // Each card owns its leading gap. Collapsing a negative margin in a
        // gapped flex column corrupts GPUI's intrinsic group height, squeezing
        // surviving cards and moving other project headers out of place.
        let column = div().flex_none().flex().flex_col().child(header);
        self.card_rows(
            column,
            indices,
            &Home::Project(id),
            chrome,
            cards_focused,
            reveal_selection,
            cx,
        )
    }

    /// The Leads section: its header with New Lead, then each Lead with its
    /// workers under it. With no Lead it is the header alone. A project with
    /// a Lead leaving still draws its exit here after the entry is gone.
    fn leads_section(
        &self,
        layout: &model::ColumnLayout,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().flex_none().flex().flex_col().child(
            section_header("Leads", chrome).child(
                icon_button("new-lead", None, "New Lead  \u{2318}L", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.new_lead(window, cx))),
            ),
        );
        for project in &self.projects {
            let entry = layout.leads.iter().find(|e| e.project == project.id);
            let leaving = self
                .departing
                .iter()
                .any(|d| d.home == Home::Leads(project.id.clone()));
            if entry.is_none() && !leaving {
                continue;
            }
            let indices = entry.map(|e| e.cards()).unwrap_or_default();
            section = self.card_rows(
                section,
                indices,
                &Home::Leads(project.id.clone()),
                chrome,
                cards_focused,
                reveal_selection,
                cx,
            );
            // A stopped Lead's row follows the exit of its card.
            if let Some(model::LeadEntry {
                kind: model::LeadKind::Resting { cli },
                ..
            }) = entry
            {
                section = section.child(
                    card_row().child(
                        self.resting_view(
                            &project.id,
                            &project.name,
                            cli,
                            chrome,
                            cards_focused,
                            reveal_selection,
                            cx,
                        )
                        .w_full()
                        .h(px(CARD_HEIGHT)),
                    ),
                );
            }
        }
        section
    }

    /// A saved Lead that is not running. It reads like its live card, with
    /// no signal, timer, or diff stat: the project, then the CLI and "Lead".
    /// A click or Enter resumes it.
    #[allow(clippy::too_many_arguments)]
    fn resting_view(
        &self,
        project: &str,
        name: &str,
        cli: &str,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self.selected_resting() == Some(project);
        let cli_id = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.lead.as_ref())
            .map_or("", |lead| lead.cli.as_str());
        // The latest reply's first line; only a Lead with a chat has one.
        let snippet = self.lead_snippets.get(project).cloned();
        let (quiet, quiet_separator) = if selected {
            (chrome.ink_3, chrome.ink_4)
        } else {
            (chrome.ink_4, chrome.ink_5)
        };
        let task = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if selected { chrome.ink_1 } else { chrome.ink_2 })
                    .child(name.to_string()),
            )
            // The signal's place stays empty, so the name truncates where a
            // live Lead's does.
            .child(div().flex_none().w(px(SIGNAL_SLOT)));
        let meta = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(quiet)
            .when_some(snippet.clone(), |d, line| {
                d.child(div().flex_1().min_w_0().truncate().child(line))
            })
            .when(snippet.is_none(), |d| {
                d.child(cli_mark(
                    SharedString::from(format!("cli-resting-{project}")),
                    cli_id,
                    cli,
                    quiet,
                    chrome,
                ))
                .child(div().flex_none().text_color(quiet_separator).child("·"))
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(chrome.ink_2)
                        .child("Lead"),
                )
                .child(div().flex_1())
            })
            .when(selected && cards_focused, |d| {
                d.child(row_hint("↵", "resume", chrome))
            });
        let (click, menu) = (project.to_string(), project.to_string());
        let base = div()
            .id(SharedString::from(format!("resting-lead-{project}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(10.))
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .capture_any_mouse_down(cx.listener(
                move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if event.button == MouseButton::Right
                        || (event.button == MouseButton::Left && event.modifiers.control)
                    {
                        cx.stop_propagation();
                        this.open_resting_menu(menu.clone(), event.position, window, cx);
                    }
                },
            ))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy || this.overlay.is_some() {
                    return;
                }
                this.selection = Some(Selection::Resting(click.clone()));
                this.resume_lead(click.clone(), window, cx);
            }));
        let base = if selected {
            base.bg(chrome.card_selected)
        } else {
            base.overflow_hidden()
        };
        base.flex_row()
            .gap(px(10.))
            .child(antler_tile(selected, chrome))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(task)
                    .child(meta),
            )
            .when(selected && reveal_selection, |d| {
                d.child(self.selection_reveal(cx))
            })
    }

    /// The rows of one group: each card, with the exits of cards that left
    /// it drawn where they were. A worker in the Leads section is indented
    /// under its Lead.
    #[allow(clippy::too_many_arguments)]
    fn card_rows(
        &self,
        mut column: gpui::Div,
        indices: Vec<usize>,
        place: &Home,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let departing = self
            .departing
            .iter()
            .filter(|d| d.home == *place)
            .collect::<Vec<_>>();
        let shown = indices
            .iter()
            .map(|&at| self.cards[at].agent.view.entity_id())
            .collect::<Vec<_>>();
        for at in indices {
            let card = &self.cards[at];
            let view = card.agent.view.entity_id();
            for leaving in departing.iter().filter(|d| d.before == Some(view)) {
                column = column.child(self.departing_view(leaving, chrome, cx));
            }
            let face = self
                .card_view(card, Some(at), chrome, cards_focused, reveal_selection, cx)
                .w_full()
                .h(px(CARD_HEIGHT));
            let face = if card.closing {
                face.with_animation(
                    SharedString::from(format!("card-closing-{view}")),
                    gpui::Animation::new(model::CLOSING_FADE).with_easing(model::ease),
                    |d, t| d.opacity(1. - (1. - CLOSING_CARD_OPACITY) * t),
                )
                .into_any_element()
            } else {
                face.into_any_element()
            };
            let indent = matches!(place, Home::Leads(_)) && card.lead.is_none();
            column = column.child(
                card_row()
                    .when(indent, |d| d.pl(px(WORKER_INDENT)))
                    .child(face),
            );
        }
        // Cards whose follower is gone too leave from the end of the group.
        for leaving in departing
            .iter()
            .filter(|d| d.before.is_none_or(|view| !shown.contains(&view)))
        {
            column = column.child(self.departing_view(leaving, chrome, cx));
        }
        column
    }

    /// A closed card's exit: it shrinks toward its center and fades out, then
    /// its row collapses so the cards below slide up. GPUI cannot scale text,
    /// so the box shrinks and the text keeps its size; it is fading by then.
    /// Both animations start on the same frame and share one clock.
    fn departing_view(
        &self,
        leaving: &Departing,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = SharedString::from(format!("departing-{}", leaving.key));
        let from = if leaving.card.closing {
            CLOSING_CARD_OPACITY
        } else {
            1.
        };
        let face = self
            .card_view(&leaving.card, None, chrome, false, false, cx)
            .size_full();
        let exit = || gpui::Animation::new(model::EXIT);
        let indent = matches!(leaving.home, Home::Leads(_)) && leaving.card.lead.is_none();
        card_row()
            .overflow_hidden()
            .when(indent, |d| d.pl(px(WORKER_INDENT)))
            .child(div().child(face).with_animation(
                SharedString::from(format!("{key}-shrink")),
                exit(),
                move |d, t| {
                    let (shrink, _) = model::card_exit(t);
                    let scale = 1. - (1. - EXIT_SCALE) * shrink;
                    d.w(gpui::relative(scale))
                        .h(px(CARD_HEIGHT * scale))
                        .opacity(from * (1. - shrink))
                },
            ))
            .with_animation(
                SharedString::from(format!("{key}-collapse")),
                exit(),
                |d, t| {
                    let (_, collapse) = model::card_exit(t);
                    let (height, gap) = card_row_geometry(collapse);
                    d.h(px(height)).pt(px(gap))
                },
            )
    }

    /// A card: task, the timer while working, and the status signal; then
    /// CLI, branch, the diff stat when ready, and key hints on the selected
    /// card. A setup stage, or "Setup failed", sits between the CLI and the
    /// branch. The status words are not painted here.
    /// `at` is the card's place in `cards`; a departing card has none and
    /// takes no clicks. Every card is [`CARD_HEIGHT`] tall.
    fn card_view(
        &self,
        card: &Card,
        at: Option<usize>,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = at.is_some() && self.selected_card() == at;
        let key = card_key(card.agent.view.entity_id());
        let colors = chrome.status(card.status);
        // The quiet meta steps one ink brighter on the selected card, as the
        // title does, so it stays readable on the lighter wash.
        let (quiet, quiet_separator) = if selected {
            (chrome.ink_3, chrome.ink_4)
        } else {
            (chrome.ink_4, chrome.ink_5)
        };
        let separator = || div().flex_none().text_color(quiet_separator).child("·");
        let hints: &[(&str, &str)] = if card.closing {
            &[]
        } else if card.creating {
            &[("↵", "view setup"), ("c", "cancel")]
        } else if card.launch_error.is_some() {
            &[("r", "retry"), ("c", "close")]
        } else {
            match card.status {
                Status::Ready => &[("↵", "read")],
                Status::Working => &[("↵", "watch")],
                Status::Asking => &[("↵", "answer")],
                Status::Waiting => &[("↵", "write prompt")],
            }
        };
        let stat = card
            .diff
            .filter(|d| card.status == Status::Ready && d.files > 0)
            .map(|d| model::diff_stat_parts(d.files, d.insertions, d.deletions));
        // Setup progress and failure are facts the signal cannot say. Waiting,
        // Working, and Ready stay on the signal, the tint, the timer, and the
        // diff stat.
        let notice = if card.closing {
            Some("Closing...".to_string())
        } else if card.creating {
            Some(card.stage.clone())
        } else if card.launch_error.is_some() {
            Some("Setup failed".to_string())
        } else {
            None
        };
        // A live Lead's line two: its current step while Working, else its
        // latest reply. None keeps the CLI and "Lead".
        let lead_line = card
            .lead
            .as_ref()
            .filter(|_| notice.is_none())
            .and_then(|state| {
                let (_, current, _) = lead_steps::snapshot(&state.steps);
                let chat = self
                    .lead_chats
                    .get(&card.project)
                    .filter(|_| card.chat)
                    .map(|chat| chat.read(cx));
                lead_steps::card_line(
                    card.status == Status::Working,
                    current.as_deref(),
                    chat.and_then(|chat| chat.replaced_line()),
                    card.chat
                        .then(|| self.lead_snippets.get(&card.project))
                        .flatten()
                        .map(String::as_str),
                )
            });
        let task = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if card.named || selected {
                        chrome.ink_1
                    } else {
                        chrome.ink_2
                    })
                    // A Lead sits outside its project's group, so it names it.
                    .child(
                        match (
                            &card.lead,
                            self.projects.iter().find(|p| p.id == card.project),
                        ) {
                            (Some(_), Some(project)) => project.name.clone(),
                            _ => card.title.clone(),
                        },
                    ),
            )
            .when(card.status == Status::Working && !card.closing, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(colors.text)
                        .child(model::short_time(
                            card.activity.turn_started.unwrap_or(card.since).elapsed(),
                        )),
                )
            })
            .child(if card.closing {
                // A card on its way out has no status to signal.
                div().flex_none().w(px(SIGNAL_SLOT)).into_any_element()
            } else {
                card_signal(&key, card.status, card.unseen(), chrome)
            });
        let meta = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(12.))
            .line_height(px(16.))
            // The CLI and branch sit back so the task reads first. Results
            // (the diff stat, the PR mark, the hints) stay a step brighter.
            .text_color(quiet)
            .when(lead_line.is_none(), |d| {
                d.child(cli_mark(
                    SharedString::from(format!("cli-{key}")),
                    &card.launch_preset,
                    &card.preset,
                    quiet,
                    chrome,
                ))
            })
            .when_some(lead_line.clone(), |d, line| {
                // The system font, truncating, in the card's line-two inks.
                d.child(div().flex_1().min_w_0().truncate().child(line))
            })
            .when(card.lead.is_some() && lead_line.is_none(), |d| {
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(chrome.ink_2)
                        .child("Lead"),
                )
            })
            .when_some(notice, |d, notice| {
                d.child(separator()).child(
                    div()
                        .min_w_0()
                        .truncate()
                        // A long setup stage gives way. "Setup failed" stays whole.
                        .when(!card.creating, |stage| stage.flex_none())
                        .text_color(if card.closing {
                            chrome.ink_2
                        } else {
                            colors.text
                        })
                        .font_weight(match card.status {
                            _ if card.closing => FontWeight::MEDIUM,
                            Status::Ready | Status::Asking => FontWeight::SEMIBOLD,
                            Status::Working => FontWeight::MEDIUM,
                            Status::Waiting => FontWeight::NORMAL,
                        })
                        .child(notice),
                )
            })
            .when_some(
                card.session
                    .as_ref()
                    .filter(|_| card.lead.is_none())
                    .and_then(model::card_branch),
                |d, branch| {
                    // The branch truncates before the diff stat. A draft
                    // has no name yet, so line two shows only the CLI.
                    d.child(separator()).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .child(branch.to_string()),
                    )
                },
            )
            .when_some(stat, |d, stat| {
                // Text on the card, not a control. A click here is the
                // card's click: select it and focus its terminal. The
                // panel opens from the shortcut, the toggle, or the menu.
                // The counts take the Changes panel's diff colors; the file
                // count stays ink.
                let (files, added, removed) = stat;
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(4.))
                        .text_color(chrome.ink_3)
                        .child(files)
                        .child(div().text_color(chrome.diff_added.text).child(added))
                        .child(div().text_color(chrome.diff_removed.text).child(removed)),
                )
            })
            .when_some(card.pr.as_ref(), |d, pr| {
                d.child(separator()).child(pr_mark(&key, pr, chrome, cx))
            })
            .when(lead_line.is_none(), |d| d.child(div().flex_1()))
            .when(selected && cards_focused, |d| {
                d.children(
                    hints
                        .iter()
                        .map(|(key, label)| row_hint(key, label, chrome)),
                )
            });
        let base = div()
            .id(SharedString::from(format!("card-{key}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(10.))
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .when_some(at, |d, i| {
                d.capture_any_mouse_down(cx.listener(
                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Right
                            || (event.button == MouseButton::Left && event.modifiers.control)
                        {
                            cx.stop_propagation();
                            this.open_card_menu(i, event.position, window, cx);
                        }
                    },
                ))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.busy || this.overlay.is_some() {
                        return;
                    }
                    this.selection = Some(Selection::Card(i));
                    this.focus_terminal(window, cx);
                }))
            });
        // The selected card is a soft wash over the column, like a hover,
        // so glass shows through it. The key hints show when the cards have
        // focus. Every other card is bare text.
        let base = if selected {
            base.bg(chrome.card_selected)
        } else {
            base.overflow_hidden()
        };
        // A Lead's antler tile spans both lines on the left.
        let base = if card.lead.is_some() {
            base.flex_row()
                .gap(px(10.))
                .child(antler_tile(selected, chrome))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(task)
                        .child(meta),
                )
        } else {
            base.child(task).child(meta)
        };
        base.when(selected && reveal_selection, |d| {
            d.child(self.selection_reveal(cx))
        })
    }

    /// Measure the selected row after layout, then reveal only the clipped
    /// edge. Do this on selection changes, not on every terminal redraw, so
    /// manual scrolling stays under the user's control.
    fn selection_reveal(&self, cx: &Context<Self>) -> impl IntoElement {
        let scroll = self.sidebar_scroll.clone();
        let selection = self.selection.clone();
        let entity = cx.entity().downgrade();
        div().absolute().inset_0().child(
            gpui::canvas(
                move |bounds, window, _cx| {
                    let viewport = scroll.bounds();
                    // Project reorder translates prepaint (and hitboxes), not
                    // layout. Reveal the final row, not its in-flight position.
                    let translation = window.element_offset().y - scroll.offset().y;
                    let delta = model::reveal_delta(
                        (bounds.top() - translation).into(),
                        (bounds.bottom() - translation).into(),
                        viewport.top().into(),
                        viewport.bottom().into(),
                    );
                    if delta == 0. {
                        return;
                    }
                    let scroll = scroll.clone();
                    let selection = selection.clone();
                    let entity = entity.clone();
                    window.on_next_frame(move |_, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            if this.selection == selection {
                                scroll.set_offset(scroll.offset() + gpui::point(px(0.), px(delta)));
                                cx.notify();
                            }
                        });
                    });
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }

    /// The settings gear, leftovers, and the key hints. A column too narrow
    /// for the hints beside Leftover worktrees drops them; the keys still
    /// work.
    fn footer(&self, chrome: &Chrome, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let hints_fit = self.leftovers.is_empty() || width >= px(FOOTER_HINTS_FIT);
        let hover = chrome.hover;
        let text_button = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(6.))
                .px(px(8.))
                .py(px(5.))
                .text_size(px(12.5))
                .line_height(px(16.))
                .text_color(chrome.ink_2)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
        };
        // One row. `justify_between` already pins the hints to the right.
        // `ml_auto` did that a second time and pushed the hints past the
        // column edge.
        div()
            .w_full()
            .flex_shrink_0()
            .border_t_1()
            .border_color(chrome.line_2)
            .pt(px(10.))
            .px(px(12.))
            .pb(px(12.))
            .flex()
            .flex_nowrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_nowrap()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        icon_button(
                            "settings",
                            Some(SETTINGS_ICON),
                            "Settings  \u{2318},",
                            chrome,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                        ),
                    )
                    .when(!self.leftovers.is_empty(), |d| {
                        d.child(
                            text_button("leftovers")
                                .flex_none()
                                .child(format!("Leftover worktrees ({})", self.leftovers.len()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.busy || this.overlay.is_some() {
                                        return;
                                    }
                                    this.overlay = Some(Overlay::Leftovers);
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(hints_fit, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_nowrap()
                        .items_center()
                        .gap_x(px(12.))
                        .child(hint("\u{2318}] \u{2318}[", "move", chrome))
                        .child(hint("\u{2318}\u{21e7}W", "close", chrome)),
                )
            })
    }

    /// The selected card's terminal under its header, or the empty state.
    fn terminal_side(
        &self,
        chrome: &Chrome,
        cards_focused: bool,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if let Some(chat) = self.chat_side(chrome, window, cx) {
            return chat;
        }
        // With the column hidden, this row is the whole title bar: it starts
        // after the traffic lights with the wordmark and column toggle.
        let show_column = self.column.hidden.then(|| {
            div()
                .flex_none()
                .h(px(BAR_HEIGHT))
                .flex()
                .items_center()
                .pl(Self::title_inset(window))
                .pr(px(4.))
                .child(self.title_identity(chrome, cx))
        });
        // Each child paints its own background, so a translucent terminal is
        // not stacked over a second translucent fill.
        let mut right = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .text_color(chrome.term_fg);
        if let Some(i) = self.selected_card() {
            let card = &self.cards[i];
            let active_tab = card.active_tab;
            // The metadata row names the project and branch. The full worktree
            // path is its tooltip. Before the worktree exists, the setup stage
            // stands in for both.
            let path = SharedString::from(
                card.session
                    .as_ref()
                    .map(|s| model::tilde(&s.worktree, home))
                    .unwrap_or_else(|| card.stage.clone()),
            );
            let (project_name, branch, stage) = match card.session.as_ref() {
                // A Lead has no branch.
                Some(s) => (
                    self.projects
                        .iter()
                        .find(|p| p.id == card.project)
                        .map(|p| SharedString::from(p.name.clone())),
                    (!s.lead).then(|| SharedString::from(s.branch.clone())),
                    None,
                ),
                None => (None, None, Some(SharedString::from(card.stage.clone()))),
            };
            let create_pr_chord = shortcuts::display("cmd-shift-p");
            let term_hover = chrome.term_hover;
            let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
            // Connected tabs: the active tab is filled like the terminal and
            // its sides flare into the header's bottom line, which breaks
            // under it. The line is drawn by each piece of the row, not the
            // header, because the active tab cannot cover a line it is
            // painted over while translucent.
            let line = chrome.term_line;
            let (tab_fill, term_fg, term_white) =
                (chrome.term_tab, chrome.term_fg, chrome.term_white);
            let baseline = |d: gpui::Div| d.flex_none().h_full().border_b_1().border_color(line);
            // The strip holds a lead space before the first tab and a
            // trailing space after the last, wide enough for their outer
            // flares, so the scroll strip never clips a flare at its ends.
            let mut tabs = div()
                .id("terminal-tabs")
                .min_w_0()
                .h_full()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&card.tab_scroll);
            // A 1px line under a piece of the row, shortened by a flare's
            // width on a side where the active tab's flare meets it.
            let segment = |flare_left: bool, flare_right: bool| {
                let inset = |flare: bool| px(if flare { TAB_FLARE } else { 0. });
                div()
                    .absolute()
                    .bottom_0()
                    .h(px(1.))
                    .left(inset(flare_left))
                    .right(inset(flare_right))
                    .bg(line)
            };
            tabs = tabs.child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(TAB_LEAD))
                    .child(segment(false, active_tab == 0)),
            );
            let labels: Vec<_> = std::iter::once((card.preset.to_string(), false))
                .chain(card.shells.iter().map(|pane| {
                    let label = if pane.shell_number == 1 {
                        "Shell".to_string()
                    } else {
                        format!("Shell {}", pane.shell_number)
                    };
                    (label, true)
                }))
                .collect();
            let last = labels.len() - 1;
            for (tab, (label, closable)) in labels.into_iter().enumerate() {
                let active = active_tab == tab;
                let group = SharedString::from(format!("terminal-tab-{tab}"));
                let body = div()
                    .id(("terminal-tab", tab))
                    .occlude()
                    .group(group.clone())
                    .relative()
                    .h(px(TAB_HEIGHT))
                    .min_w(px(88.))
                    .max_w(px(180.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pb(px(TAB_LABEL_LIFT))
                    .pl(px(12.))
                    .pr(px(if closable { 6. } else { 12. }))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .cursor_pointer()
                    .when(active, |d| d.text_color(term_white))
                    .when(!active, |d| {
                        d.text_color(chrome.term_dim)
                            .hover(move |style| style.text_color(term_fg))
                    })
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if let Some(i) = this.selected_card() {
                            let card = &mut this.cards[i];
                            if *hovered {
                                card.hovered_tab = Some(tab);
                            } else if card.hovered_tab == Some(tab) {
                                card.hovered_tab = None;
                            }
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(label),
                    )
                    .when(closable, |d| {
                        d.child(
                            div()
                                .id(("shell-close", tab))
                                .flex_none()
                                .size(px(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .text_color(chrome.term_dim)
                                .when(!active, |d| {
                                    d.opacity(0.).group_hover(group, |style| style.opacity(1.))
                                })
                                .hover(move |style| style.bg(term_hover).text_color(term_white))
                                .child("×")
                                .tooltip(move |_, cx| {
                                    cx.new(|_| KeyTip {
                                        bg: tip_bg,
                                        fg: tip_fg,
                                        text: "Close shell tab (stops its processes)  ⌘W".into(),
                                    })
                                    .into()
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(tab, window, cx)
                                })),
                        )
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(tab, window, cx)),
                    );
                // The active tab's flares are painted outside its bounds, over
                // the bottom corners of its neighbors, so they never change
                // the layout. A neighbor's line stops where a flare meets it,
                // because a translucent flare cannot hide a line under it.
                // Hover paints the same shape in the hover wash, standing on
                // the line instead of breaking it, and rounds the corner that
                // meets an active neighbor's flare so the two fit together.
                let wrapper = div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_end();
                let (after_active, before_active) =
                    (tab > 0 && active_tab == tab - 1, active_tab == tab + 1);
                let wrapper = if active {
                    wrapper.child(tab_canvas(move |bounds, window| {
                        paint_active_tab(bounds, tab_fill, line, window)
                    }))
                } else {
                    wrapper
                        .when(card.hovered_tab == Some(tab), |d| {
                            d.child(tab_canvas(move |bounds, window| {
                                if let Some(path) =
                                    tab_shape(bounds, !after_active, !before_active, false)
                                {
                                    window.paint_path(path, term_hover);
                                }
                            }))
                        })
                        .child(segment(after_active, before_active))
                };
                tabs = tabs.child(wrapper.pb(px(1.)).child(body));
            }
            tabs = tabs.child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(TAB_FLARE))
                    .child(segment(active_tab == last, false)),
            );
            let header = div()
                .id("terminal-header")
                .h(px(BAR_HEIGHT))
                .flex_shrink_0()
                .flex()
                .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
                .children(show_column.map(baseline))
                .child(tabs)
                .when(card.lead.is_none(), |d| {
                    d.child(
                        baseline(div())
                            .flex()
                            .flex_col()
                            .justify_end()
                            .pr(px(4.))
                            .child(
                                div().h(px(CONTROL_BAND)).flex().items_center().child(
                                    div()
                                        .id("new-terminal")
                                        .occlude()
                                        .size(px(24.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(6.))
                                        .text_size(px(15.))
                                        .text_color(chrome.term_dim)
                                        .cursor_pointer()
                                        .hover(move |style| {
                                            style.bg(term_hover).text_color(term_white)
                                        })
                                        .child("+")
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| KeyTip {
                                                bg: tip_bg,
                                                fg: tip_fg,
                                                text: "New shell tab  ⌘T".into(),
                                            })
                                            .into()
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_shell(true, window, cx)
                                        })),
                                ),
                            ),
                    )
                })
                .child(baseline(div()).flex_1().min_w_0())
                .child(
                    baseline(div())
                        .flex()
                        .flex_col()
                        .justify_end()
                        .pl(px(12.))
                        // Closed, the Changes toggle follows 8 after Close task.
                        .pr(px(if self.changes.open { 12. } else { 8. }))
                        .child(
                            div().h(px(CONTROL_BAND)).flex().items_center().child(
                                div()
                                    .id("close")
                                    .occlude()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .border_1()
                                    .border_color(chrome.term_seg_active)
                                    .rounded(px(6.))
                                    .pl(px(10.))
                                    .pr(px(6.))
                                    .py(px(4.))
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(chrome.term_fg)
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(term_hover))
                                    .child(if card.creating {
                                        "Cancel setup"
                                    } else {
                                        "Close task"
                                    })
                                    .child(
                                        kbd("\u{2318}\u{21e7}W", chrome.term_line, chrome.term_dim)
                                            .py_0(),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                                    ),
                            ),
                        ),
                )
                .when(!self.changes.open, |d| {
                    d.child(
                        baseline(div())
                            .flex()
                            .flex_col()
                            .justify_end()
                            .pr(px(12.))
                            .child(
                                div()
                                    .h(px(CONTROL_BAND))
                                    .flex()
                                    .items_center()
                                    .child(self.changes_toggle(chrome, cx)),
                            ),
                    )
                });
            let pane = card.active_pane();
            right = right
                .child(self.title_drag(header, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(20.))
                        .pt(px(8.))
                        .pb(px(2.))
                        .flex_shrink_0()
                        .bg(chrome.term_surface)
                        .text_size(px(11.5))
                        .text_color(chrome.term_dim)
                        .child(
                            div()
                                .id("task-where")
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .whitespace_nowrap()
                                .tooltip(move |_, cx| {
                                    cx.new(|_| KeyTip {
                                        bg: tip_bg,
                                        fg: tip_fg,
                                        text: path.clone(),
                                    })
                                    .into()
                                })
                                // The project never gives way before the branch.
                                .when_some(project_name, |d, name| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(chrome.term_fg)
                                            .child(name),
                                    )
                                })
                                .when_some(branch, |d, branch| {
                                    d.child(
                                        gpui::svg()
                                            .data(BRANCH_ICON)
                                            .flex_none()
                                            .size(px(12.))
                                            .text_color(chrome.term_dim),
                                    )
                                    .child(
                                        div().min_w_0().truncate().font_family(MONO).child(branch),
                                    )
                                })
                                .when_some(stage, |d, stage| {
                                    d.child(div().min_w_0().truncate().child(stage))
                                }),
                        )
                        .child(div().flex_1())
                        .when(
                            card.session.is_some() && !card.creating && card.lead.is_none(),
                            |d| {
                                d.child(
                                    div()
                                        .id("create-pr")
                                        .occlude()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap(px(7.))
                                        .border_1()
                                        .border_color(chrome.term_seg_active)
                                        .rounded(px(6.))
                                        .pl(px(10.))
                                        .pr(px(6.))
                                        .py(px(4.))
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .text_color(chrome.term_fg)
                                        .cursor_pointer()
                                        .hover(move |style| style.bg(term_hover))
                                        .child("Create PR")
                                        .child(
                                            kbd(
                                                create_pr_chord.clone(),
                                                chrome.term_line,
                                                chrome.term_dim,
                                            )
                                            .py_0(),
                                        )
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| KeyTip {
                                                bg: tip_bg,
                                                fg: tip_fg,
                                                text: format!(
                                                    "Commit, push, create PR  {create_pr_chord}"
                                                )
                                                .into(),
                                            })
                                            .into()
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.create_pr(window, cx)
                                        })),
                                )
                            },
                        )
                        .child(div().flex_none().text_color(chrome.term_faint).child(
                            if card.creating && cards_focused {
                                "setup is non-interactive"
                            } else if card.launch_error.is_some() && cards_focused {
                                "r retry setup"
                            } else if cards_focused {
                                "↵ type here"
                            } else {
                                "ctrl q back to cards"
                            },
                        )),
                )
                .when(card.launch_error.is_some(), |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .px(px(14.))
                            .py(px(8.))
                            .text_size(px(12.5))
                            .child(
                                dialog_button("retry-setup", "Retry setup", "r", chrome).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.retry_preparation(window, cx)
                                    }),
                                ),
                            )
                            .child(div().text_color(chrome.term_dim).child(
                                "Creates a fresh worktree. Changed files remain in leftovers.",
                            )),
                    )
                })
                .child(closing_terminal(card, pane, chrome));
        } else {
            let icon = Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                include_bytes!("../../../assets/macos/shika-app-icon-256.png").to_vec(),
            ));
            let key = |key: &'static str, label: &'static str| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(kbd(key, chrome.term_seg, chrome.term_dim))
                    .child(label)
            };
            // A resting Lead says so, and a click on the area resumes it.
            let resting = self.selected_resting().map(|project| {
                let name = self
                    .projects
                    .iter()
                    .find(|p| p.id == project)
                    .map_or("The", |p| p.name.as_str());
                (project.to_string(), format!("{name} Lead is not running"))
            });
            let (message, keys) = match &resting {
                Some((_, message)) => (message.as_str(), key("\u{21b5}", "resume")),
                None => (
                    "No agent selected",
                    div()
                        .flex()
                        .gap(px(14.))
                        .child(key("j", "select"))
                        .child(key("\u{2318}N", "new agent")),
                ),
            };
            right = right.child(
                div()
                    .id("terminal-empty")
                    .relative()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .when_some(resting.as_ref(), |d, (project, _)| {
                        let project = project.clone();
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.resume_lead(project.clone(), window, cx)
                            }))
                    })
                    .bg(with_alpha(chrome.term_empty, chrome.term_empty_alpha))
                    .text_size(px(13.))
                    .text_color(chrome.term_faint)
                    .child(
                        self.title_drag(
                            div()
                                .id("terminal-drag")
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(BAR_HEIGHT))
                                .flex()
                                .children(show_column)
                                .child(div().flex_1())
                                .when(!self.changes.open, |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .h_full()
                                            .flex()
                                            .items_center()
                                            .pr(px(12.))
                                            .child(self.changes_toggle(chrome, cx)),
                                    )
                                }),
                            cx,
                        ),
                    )
                    .child(gpui::img(icon).size(px(64.)).mb(px(6.)))
                    .child(message.to_string())
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(chrome.term_fainter)
                            .child(keys),
                    ),
            );
        }
        if let Some((text, _)) = &self.toast {
            right = right.child(
                div()
                    .absolute()
                    .bottom(px(22.))
                    .left(px(20.))
                    .right(px(20.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(8.))
                            .bg(chrome.toast_bg)
                            .text_color(chrome.toast_fg)
                            .text_size(px(12.5))
                            .line_height(px(16.))
                            .shadow(vec![
                                BoxShadow::new(px(0.), px(6.), chrome.toast_shadow.into())
                                    .blur_radius(px(20.)),
                            ])
                            .child(text.clone()),
                    ),
            );
        }
        if let Some(card) = &self.update_notice
            && !card.centered()
        {
            right = right.child(
                div()
                    .absolute()
                    .bottom(px(22.))
                    .right(px(20.))
                    .child(self.update_notice_panel(card, chrome, window, cx)),
            );
        }
        right.into_any_element()
    }

    /// The update card. It does not take focus, so typing and Escape stay in
    /// the terminal. 320 is the column's minimum, with the card's own padding.
    fn update_notice_panel(
        &self,
        card: &updates::UpdateCard,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notice = card.view();
        let max_h = (window.viewport_size().height - px(44.)).max(px(120.));
        let mut panel = panel_shell("update-notice", 320., max_h, chrome)
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .gap(px(6.))
            .child(dialog_title(div()).child(notice.title))
            .child(dialog_text(chrome).child(notice.body));
        if notice.secondary.is_some() || notice.primary.is_some() {
            panel = panel.child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .flex_wrap()
                    .when_some(notice.secondary, |row, label| {
                        row.child(
                            notice_secondary("update-secondary", label, chrome).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.answer_update(updates::CardAction::Secondary, cx)
                                }),
                            ),
                        )
                    })
                    .when_some(notice.primary, |row, label| {
                        row.child(notice_primary("update-primary", label, chrome).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.answer_update(updates::CardAction::Primary, cx)
                            }),
                        ))
                    }),
            );
        }
        panel
    }

    /// A pointer-anchored menu with a transparent, click-consuming backdrop.
    fn card_menu_view(
        &self,
        menu: &CardMenu,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = chrome.row_selected;
        let can_rename = menu
            .target_index(self.cards.iter().map(|card| card.agent.view.entity_id()))
            .is_some_and(|index| self.cards[index].renamable());
        let panel = dialog_shell(400., window.viewport_size().height, chrome)
            .w_auto()
            .p(px(8.))
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                list_row(menu.action == CardMenuAction::Rename && can_rename, chrome)
                    .id("card-menu-rename")
                    .gap(px(20.))
                    .justify_between()
                    .text_color(if can_rename {
                        chrome.ink_1
                    } else {
                        chrome.ink_4
                    })
                    .when(can_rename, |row| {
                        row.cursor_pointer().hover(move |style| style.bg(hover))
                    })
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered
                            && can_rename
                            && let Some(Overlay::CardMenu(menu)) = &mut this.overlay
                        {
                            menu.action = CardMenuAction::Rename;
                            cx.notify();
                        }
                    }))
                    .child("Rename task…")
                    .child(kbd("\u{2318}\u{21e7}R", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.rename_task(window, cx);
                    })),
            )
            .child(
                list_row(menu.action == CardMenuAction::Close, chrome)
                    .id("card-menu-close")
                    .gap(px(20.))
                    .justify_between()
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        if *hovered && let Some(Overlay::CardMenu(menu)) = &mut this.overlay {
                            menu.action = CardMenuAction::Close;
                            cx.notify();
                        }
                    }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child("Close task")
                    .child(kbd("\u{2318}\u{21e7}W", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_from_card_menu(window, cx);
                    })),
            );
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.cancel_overlay(window, cx);
            }))
            .child(gpui::deferred(
                gpui::anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            ))
    }

    /// The resting Lead's pointer menu: one item, Forget Lead.
    fn resting_menu_view(
        &self,
        position: gpui::Point<Pixels>,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = chrome.row_selected;
        let panel = dialog_shell(400., window.viewport_size().height, chrome)
            .w_auto()
            .p(px(8.))
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                list_row(true, chrome)
                    .id("resting-menu-forget")
                    .gap(px(20.))
                    .justify_between()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child("Forget Lead")
                    .child(kbd("\u{2318}\u{232b}", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.forget_from_menu(window, cx);
                    })),
            );
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.cancel_overlay(window, cx);
            }))
            .child(gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            ))
    }

    fn settings_keys(
        &self,
        section: SettingsSection,
        row: usize,
        editing: bool,
    ) -> &'static [(&'static str, &'static str)] {
        if section == SettingsSection::Keyboard {
            return if self.key_recording {
                &[("esc", "cancel")]
            } else {
                &[
                    ("j k", "choose"),
                    ("\u{21b5}", "set"),
                    ("\u{232b}", "clear"),
                    ("[ ]", "section"),
                    ("esc", "done"),
                ]
            };
        }
        if section == SettingsSection::Projects {
            return &[
                ("j k", "project"),
                ("↵", "setup"),
                ("[ ]", "section"),
                ("esc", "done"),
            ];
        }
        if section == SettingsSection::Usage {
            return &[
                ("h l", "provider"),
                ("w m y", "range"),
                ("[ ]", "section"),
                ("esc", "done"),
            ];
        }
        if section == SettingsSection::Agents {
            return if self.catalog.is_none() {
                &[("[ ]", "section"), ("esc", "done")]
            } else {
                &[
                    ("j k", "choose"),
                    ("h l", "enable"),
                    ("[ ]", "section"),
                    ("esc", "done"),
                ]
            };
        }
        match (editing, row) {
            (true, PREFIX_ROW) => &[("", "type a prefix"), ("↵", "apply"), ("esc", "cancel")],
            (true, _) => &[("", "type a number"), ("↵", "apply"), ("esc", "cancel")],
            (false, PREFIX_ROW) => &[
                ("j k", "choose"),
                ("↵", "edit"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, SOUND_ROW) => &[
                ("j k", "choose"),
                ("h l", "sound"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, MODE_ROW) => &[
                ("j k", "choose"),
                ("h l", "mode"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, LIGHT_ROW | DARK_ROW) => &[
                ("j k", "choose"),
                ("h l", "theme"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, APPLY_ROW) => &[
                ("j k", "choose"),
                ("h l", "change"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, IMAGE_ROW) if self.background.is_some() => &[
                ("j k", "choose"),
                ("↵", "replace"),
                ("a", "adjust"),
                ("⌫", "remove"),
                ("esc", "done"),
            ],
            (false, IMAGE_ROW) => &[
                ("j k", "choose"),
                ("↵", "choose image"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, EFFECT_ROW) => &[
                ("j k", "choose"),
                ("h l", "effect"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
            (false, _) => &[
                ("j k", "choose"),
                ("h l", "change"),
                ("", "type a number"),
                ("[ ]", "section"),
                ("esc", "done"),
            ],
        }
    }
    fn settings_nav(
        &self,
        current: SettingsSection,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut nav = div()
            .w(px(SETTINGS_NAV_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pr(px(12.))
            .border_r_1()
            .border_color(chrome.line_2);
        for section in SettingsSection::ALL {
            nav = nav.child(
                list_row(section == current, chrome)
                    .id(SharedString::from(format!(
                        "settings-section-{}",
                        section.label()
                    )))
                    .cursor_pointer()
                    .child(section.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_section(section, window, cx);
                    })),
            );
        }
        nav
    }
    /// One preset: its name, where the binary is, and whether New offers it.
    fn agent_row(
        &self,
        index: usize,
        preset: &CliPreset,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Agents,
                row,
                ..
            }) if *row == index
        );
        let on = self.agents.enabled(&preset.id);
        let detail = match &preset.path {
            Some(path) => model::tilde(path, home),
            None => format!("{} not found on PATH", preset.binary),
        };
        let name_color = if preset.found() {
            chrome.ink_1
        } else {
            chrome.ink_4
        };
        let shadow = chrome.control_shadow;
        let off_id = preset.id.clone();
        let on_id = preset.id.clone();
        let segment_shadow = move |chosen: bool, control: gpui::Stateful<gpui::Div>| {
            control.when(chosen, |d| {
                d.shadow(vec![
                    BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                ])
            })
        };
        list_row(selected, chrome)
            .id(SharedString::from(format!("agent-setting-{}", preset.id)))
            .gap(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_setting(index, window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(name_color)
                    .child(preset.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child(detail),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(
                        segment_shadow(
                            !on,
                            segment(
                                SharedString::from(format!("agent-{}-off", preset.id)),
                                "Off",
                                !on,
                                chrome.raised,
                                chrome.ink_1,
                                chrome.ink_3,
                            ),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_agent_enabled(&off_id, false, cx);
                                this.select_setting(index, window, cx);
                            },
                        )),
                    )
                    .child(
                        segment_shadow(
                            on,
                            segment(
                                SharedString::from(format!("agent-{}-on", preset.id)),
                                "On",
                                on,
                                chrome.raised,
                                chrome.ink_1,
                                chrome.ink_3,
                            ),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_agent_enabled(&on_id, true, cx);
                                this.select_setting(index, window, cx);
                            },
                        )),
                    ),
            )
    }
    /// One shortcut: its name, and the chord or "none". Enter or a click on
    /// the chord records the next Command shortcut.
    fn shortcut_row(
        &self,
        index: usize,
        command: &shortcuts::Command,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Keyboard,
                row,
                ..
            }) if *row == index
        );
        let recording = selected && self.key_recording;
        let label = command.label;
        let chord = shortcuts::bound_label(&self.keys, command.id);
        let value = if recording {
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(chrome.ink_2)
                .child("Press a shortcut")
                .into_any_element()
        } else if let Some(chord) = chord {
            kbd(chord, chrome.sunken, chrome.ink_2).into_any_element()
        } else {
            div()
                .flex_none()
                .text_color(chrome.ink_4)
                .child("none")
                .into_any_element()
        };
        list_row(selected, chrome)
            .id(SharedString::from(format!("shortcut-{}", command.id)))
            .gap(px(12.))
            .justify_between()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_setting(index, window, cx);
            }))
            .child(label)
            .child(
                div()
                    .id(SharedString::from(format!("shortcut-{}-chord", command.id)))
                    .flex_none()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.begin_key_recording(index, cx);
                    }))
                    .child(value),
            )
    }
    fn settings_panel(
        &self,
        section: SettingsSection,
        row: usize,
        max_h: Pixels,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let panel_h = if max_h < px(SETTINGS_HEIGHT) {
            max_h
        } else {
            px(SETTINGS_HEIGHT)
        };
        let editing = matches!(self.overlay, Some(Overlay::Settings { edit: Some(_), .. }));
        let keys = self.settings_keys(section, row, editing);
        let mut rows = div()
            .id("settings-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .flex()
            .flex_col()
            .gap(px(4.));
        match section {
            SettingsSection::Appearance => {
                let opacity = self.appearance.opacity.to_string();
                let blur = self.appearance.blur.to_string();
                let font = self.font_size.text();
                let both = self.appearance.translucency == Translucency::SidebarAndTerminal;
                let shadow = chrome.control_shadow;
                let choice = |id: &'static str, label: &'static str, on: bool| {
                    segment(id, label, on, chrome.raised, chrome.ink_1, chrome.ink_3).when(
                        on,
                        |d| {
                            d.shadow(vec![
                                BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                            ])
                        },
                    )
                };
                rows = rows
                    .child(self.mode_row(chrome, cx))
                    .child(self.theme_row(LIGHT_ROW, "Light theme", chrome, cx))
                    .child(self.theme_row(DARK_ROW, "Dark theme", chrome, cx))
                    .child(self.image_row(chrome, cx))
                    .child(self.effect_row(chrome, cx))
                    .child(self.setting_row(
                        OPACITY_ROW,
                        "Background opacity",
                        &opacity,
                        "%",
                        chrome,
                        cx,
                    ))
                    .child(self.setting_row(BLUR_ROW, "Background blur", &blur, "", chrome, cx))
                    .child(
                        list_row(row == APPLY_ROW, chrome)
                            .justify_between()
                            .child("Apply to")
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.))
                                    .gap(px(2.))
                                    .rounded(px(7.))
                                    .bg(chrome.sunken)
                                    .child(
                                        choice("translucent-sidebar", "Sidebar", !both).on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.step_setting(APPLY_ROW, -1, window, cx);
                                                this.select_setting(APPLY_ROW, window, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        choice("translucent-both", "Sidebar and terminal", both)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.step_setting(APPLY_ROW, 1, window, cx);
                                                this.select_setting(APPLY_ROW, window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(self.setting_row(
                        FONT_ROW,
                        "Terminal font size",
                        &font,
                        "px",
                        chrome,
                        cx,
                    ))
                    .child(self.prefix_row(chrome, cx))
                    .child(self.sound_row(chrome, cx));
            }
            SettingsSection::Agents => {
                if let Some(presets) = self.catalog.as_ref().map(|catalog| catalog.presets.clone())
                {
                    for (index, preset) in presets.iter().enumerate() {
                        rows = rows.child(self.agent_row(index, preset, home, chrome, cx));
                    }
                } else {
                    rows = rows.child(
                        list_row(false, chrome)
                            .text_color(chrome.ink_3)
                            .child("Resolving login-shell PATH..."),
                    );
                }
            }
            SettingsSection::Projects => {
                if self.projects.is_empty() {
                    rows = rows.child(
                        dialog_text(chrome).child("Add a project to configure worktree setup."),
                    );
                }
                for (index, project) in self.projects.iter().enumerate() {
                    let id = project.id.clone();
                    rows = rows.child(
                        list_row(row == index, chrome)
                            .id(SharedString::from(format!("project-setting-{id}")))
                            .flex_col()
                            .items_start()
                            .gap(px(4.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_worktree_setup(id.clone(), window, cx)
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(chrome.ink_3)
                                            .child("Worktree setup…"),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_family(MONO)
                                    .text_size(px(11.))
                                    .text_color(chrome.ink_3)
                                    .child(model::tilde(&project.path, home)),
                            ),
                    );
                }
            }
            SettingsSection::Keyboard => {
                for (index, command) in shortcuts::COMMANDS.iter().enumerate() {
                    rows = rows.child(self.shortcut_row(index, command, chrome, cx));
                }
            }
            SettingsSection::Usage => rows = rows.child(self.usage_rows(chrome, cx)),
        }
        dialog_shell(SETTINGS_WIDTH, panel_h, chrome)
            .h(panel_h)
            .overflow_hidden()
            .pt(px(20.))
            .px(px(20.))
            .pb(px(16.))
            .gap(px(8.))
            .child(dialog_title(div()).child("Settings"))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(12.))
                    .child(self.settings_nav(section, chrome, cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(rows)
                            .child(scrollbar::vertical(
                                &self.settings_scroll,
                                &self.settings_grab,
                                chrome.ink_4,
                                chrome.ink_3,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .px(px(10.))
                    .when_some(
                        (section == SettingsSection::Keyboard)
                            .then(|| self.key_note.clone())
                            .flatten(),
                        |row, note| {
                            row.child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(chrome.ink_1)
                                    .child(note),
                            )
                        },
                    )
                    .children(keys.iter().map(|(key, label)| hint(key, label, chrome))),
            )
            .child(dialog_buttons().child(self.cancel_button("Done", chrome, cx)))
    }

    fn switch_dialog(
        &self,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (prompt, index, error) = {
            let Some(Overlay::SwitchAgent {
                prompt,
                index,
                error,
                ..
            }) = &self.overlay
            else {
                return dialog(chrome, max_h);
            };
            (prompt.clone(), *index, error.clone())
        };
        prompt.update(cx, |input, _| input.set_chrome(*chrome));
        let presets = self.offered_presets();
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child("Switch agent?"))
            .child(
                dialog_text(chrome)
                    .child("This stops the current agent only after the new one starts."),
            )
            .child(dialog_text(chrome).child(
                "The worktree, branch, commits, and uncommitted files stay. Shell tabs stay.",
            ))
            .child(dialog_text(chrome).child("No transcript is copied."))
            .child(dialog_text(chrome).child("Prompt"))
            .child(prompt.clone())
            .child(
                dialog_text(chrome)
                    .text_color(chrome.ink_3)
                    .child("What should this agent do? Leave it empty to type in the terminal."),
            )
            .child(dialog_text(chrome).child("Agent"));
        if self.catalog.is_none() {
            panel = panel.child(dialog_text(chrome).child("Resolving login-shell PATH..."));
        } else if presets.is_empty() {
            panel = panel
                .child(dialog_text(chrome).child("No agents available"))
                .child(
                    dialog_text(chrome)
                        .text_color(chrome.ink_3)
                        .child("Turn one on in Settings."),
                );
        } else {
            let mut rows = div()
                .id("switch-agents")
                .max_h(px(BASE_LIST_MAX))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            for (i, preset) in presets.iter().enumerate() {
                let detail = preset
                    .path
                    .as_deref()
                    .map(|path| model::tilde(path, home))
                    .unwrap_or_else(|| format!("{} not found on PATH", preset.binary));
                rows = rows.child(
                    list_row(i == index, chrome)
                        .id(SharedString::from(format!("switch-agent-{}", preset.id)))
                        .gap(px(12.))
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(13.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(chrome.ink_1)
                                .child(preset.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_right()
                                .font_family(MONO)
                                .text_size(px(11.))
                                .text_color(chrome.ink_3)
                                .child(detail),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.busy {
                                return;
                            }
                            if let Some(Overlay::SwitchAgent { index, .. }) = &mut this.overlay {
                                *index = i;
                            }
                            window.focus(&this.focus, cx);
                            cx.notify();
                        })),
                );
            }
            panel = panel.child(rows);
        }
        if let Some(error) = error {
            panel = panel.child(dialog_text(chrome).child(error.to_string()));
        }
        let label = if self.busy { "Starting..." } else { "Switch" };
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .child(hint("j k", "choose", chrome))
                    .child(hint("tab", "edit prompt", chrome))
                    .child(hint("enter", "switch", chrome))
                    .child(hint("esc", "cancel", chrome)),
            )
            .child(
                dialog_buttons()
                    .child(self.cancel_button("Cancel", chrome, cx))
                    .child(primary_button("switch-agent", label, "↵", chrome).on_click(
                        cx.listener(|this, _, window, cx| this.confirm_switch(window, cx)),
                    )),
            )
    }

    /// Settings > Usage, from the snapshot read when it first showed: the
    /// range, the provider totals that filter the chart, then the chart.
    fn usage_rows(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = div().flex().flex_col().gap(px(12.)).px(px(10.)).pt(px(4.));
        let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &self.overlay
        else {
            return panel;
        };
        let (range, filter) = (view.range, view.filter);
        let mut segments = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(chrome.sunken);
        for (index, choice) in UsageRange::CHOICES.into_iter().enumerate() {
            segments = segments.child(
                segment(
                    ("usage-range", index),
                    choice.label(),
                    range == choice,
                    chrome.raised,
                    chrome.ink_1,
                    chrome.ink_3,
                )
                .when(range == choice, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), chrome.control_shadow.into())
                            .blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_usage_range(choice, cx))),
            );
        }
        panel = panel.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap(px(12.))
                .child(
                    dialog_text(chrome)
                        .min_w_0()
                        .child("Tokens from Claude Code and Codex in Shika. Not a bill."),
                )
                .child(segments),
        );
        let Some(report) = &view.report else {
            return panel.child(dialog_text(chrome).child("Reading usage..."));
        };
        if report.unreadable {
            panel = panel.child(dialog_text(chrome).child("Could not read usage."));
        } else if let Some(calendar) = &report.calendar {
            panel = panel.child(usage_dashboard::totals(
                calendar,
                range,
                filter,
                chrome,
                |choice, column| {
                    column.on_click(
                        cx.listener(move |this, _, _, cx| this.set_usage_filter(choice, cx)),
                    )
                },
            ));
            if let Some(chart) = &view.chart {
                panel = panel.child(usage_dashboard::chart(
                    calendar, range, filter, chrome, chart,
                ));
            }
            if range.totals(calendar).combined() == 0 {
                panel = panel.child(dialog_text(chrome).child(if report.empty {
                    "No tokens recorded yet."
                } else {
                    "No tokens recorded in this range."
                }));
            }
        }
        if report.pending {
            panel =
                panel.child(dialog_text(chrome).child(
                    "Some usage is pending or could not be read. Saved totals are retained.",
                ));
        }
        panel
    }

    /// The pointer menu, or the picker/dialogs near the top. No overlay dims
    /// the window: Settings previews opacity and blur on it, and the rest match.
    fn overlay_view(
        &self,
        overlay: &Overlay,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Overlay::CardMenu(menu) = overlay {
            return self
                .card_menu_view(menu, chrome, window, cx)
                .into_any_element();
        }
        if let Overlay::RestingMenu { position, .. } = overlay {
            return self
                .resting_menu_view(*position, chrome, window, cx)
                .into_any_element();
        }
        let height = window.viewport_size().height;
        let picker = matches!(overlay, Overlay::Picker { .. });
        let top = height * if picker { 0.18 } else { 0.20 };
        let max_h = (height - top - px(24.)).max(px(120.));
        let panel = match overlay {
            Overlay::CardMenu(_) | Overlay::RestingMenu { .. } => {
                unreachable!("card menus render at the pointer")
            }
            Overlay::LeadQuestion(question) => self.chat_question_view(question, chrome, max_h, cx),
            Overlay::LeadClose { step, .. } => {
                let stopping = matches!(step, model::LeadClose::Stop { .. });
                let mut panel =
                    dialog(chrome, max_h).child(dialog_title(div()).child(step.title()));
                for fact in step.facts() {
                    panel = panel.child(dialog_text(chrome).child(fact));
                }
                let buttons = dialog_buttons().child(self.cancel_button("Cancel", chrome, cx));
                panel.child(if stopping {
                    buttons
                        .child(
                            dialog_button("forget-lead", "Forget", "\u{2318}\u{232b}", chrome)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if !this.busy {
                                        this.forget_lead_from_dialog(window, cx);
                                    }
                                })),
                        )
                        .child(primary_button("stop-lead", "Stop", "↵", chrome).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.finish_lead_close(false, window, cx)
                            }),
                        ))
                } else {
                    buttons.child(
                        primary_button("forget-lead", "Forget", "\u{2318}\u{232b}", chrome)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_lead_close(true, window, cx)
                            })),
                    )
                })
            }
            Overlay::WorktreeSetup(editor) => {
                self.worktree_setup_view(editor, chrome, max_h, window, cx)
            }
            Overlay::Adjust(adjust) => self.adjust_view(adjust, max_h, chrome, window, cx),
            Overlay::Rename { input, error, .. } => {
                input.update(cx, |input, _| input.set_chrome(*chrome));
                dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Rename task"))
                    .child(dialog_text(chrome).child("Changes the task name and future Create PR defaults. The branch and worktree stay unchanged."))
                    .child(input.clone())
                    .when_some(error.as_ref(), |panel, error| panel.child(dialog_text(chrome).child(error.clone())))
                    .child(dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(primary_button("rename-task", "Rename task", "↵", chrome)
                            .on_click(cx.listener(|this, _, window, cx| this.apply_task_name(window, cx)))))
            }
            Overlay::SwitchAgent { .. } => self.switch_dialog(chrome, home, max_h, cx),
            Overlay::Publish {
                preview,
                title,
                target,
                row,
                error,
            } => {
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Create PR?"))
                    .child(dialog_text(chrome).font_family(MONO).child(format!("{}: {} → {}", preview.repository, preview.branch, if target.is_empty() { "choose target" } else { target })))
                    .child(dialog_text(chrome).child("Stages all non-ignored changes, commits, pushes, and creates a PR. Does not merge or close the task."))
                    .child(dialog_text(chrome).child("Commit and PR title"))
                    .child(text_field("pr-title", 360., *row == 0, chrome)
                        .overflow_hidden().child(title.clone())
                        .when(*row == 0, |d| d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus)))
                        .on_click(cx.listener(|this, _, _, cx| { if !this.busy { if let Some(Overlay::Publish { row, .. }) = &mut this.overlay { *row = 0; } cx.notify(); } })))
                    .child(dialog_text(chrome).child("PR target"))
                    .child(text_field("pr-target", 360., *row == 1, chrome)
                        .overflow_hidden().child(target.clone())
                        .when(*row == 1, |d| d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus)))
                        .on_click(cx.listener(|this, _, _, cx| { if !this.busy { if let Some(Overlay::Publish { row, .. }) = &mut this.overlay { *row = 1; } cx.notify(); } })));
                if preview.target.is_none() {
                    panel = panel.child(dialog_text(chrome).child("The recorded base is not an existing GitHub branch. Choose a target explicitly."));
                }
                if *row == 1 {
                    panel = panel.child(
                        div()
                            .id("pr-branches")
                            .max_h(px(BASE_LIST_MAX))
                            .overflow_y_scroll()
                            .children(
                                preview
                                    .branches
                                    .iter()
                                    .filter(|b| b.starts_with(target) && **b != preview.branch)
                                    .map(|b| {
                                        let branch = b.clone();
                                        list_row(b == target, chrome)
                                            .id(SharedString::from(format!("pr-branch-{b}")))
                                            .cursor_pointer()
                                            .font_family(MONO)
                                            .child(b.clone())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !this.busy {
                                                    if let Some(Overlay::Publish {
                                                        target,
                                                        error,
                                                        ..
                                                    }) = &mut this.overlay
                                                    {
                                                        *target = branch.clone();
                                                        *error = None;
                                                    }
                                                    cx.notify();
                                                }
                                            }))
                                    }),
                            ),
                    );
                }
                panel = panel
                    .child(
                        dialog_text(chrome).child(format!("{} changed files", preview.files.len())),
                    )
                    .child(
                        div()
                            .id("pr-files")
                            .max_h(px(BASE_LIST_MAX))
                            .overflow_y_scroll()
                            .children(preview.files.iter().map(|file| {
                                dialog_text(chrome).font_family(MONO).child(file.clone())
                            })),
                    );
                if let Some(error) = error {
                    panel = panel.child(dialog_text(chrome).child(error.clone()));
                }
                panel.child(hint("tab", "switch field", chrome)).child(
                    dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(
                            primary_button(
                                "publish-pr",
                                if self.busy {
                                    "Publishing..."
                                } else {
                                    "Commit, push, create PR"
                                },
                                "↵",
                                chrome,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.publish_pr(window, cx)),
                            ),
                        ),
                )
            }
            Overlay::Picker {
                project,
                index,
                lead,
            } => {
                let name = self
                    .projects
                    .iter()
                    .find(|p| &p.id == project)
                    .map(|p| p.name.as_str())
                    .unwrap_or("");
                let mut panel = dialog_shell(420., max_h, chrome)
                    .p(px(8.))
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(8.))
                            .px(px(10.))
                            .pb(px(10.))
                            .child(dialog_title(div()).child(if *lead {
                                "New Lead"
                            } else {
                                "New agent"
                            }))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(chrome.ink_3)
                                    .child(format!("in {name}")),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(chrome.ink_4)
                                    .child("tab to change project"),
                            ),
                    );
                let offered = self.offered_presets();
                let offer_settings = self.catalog.is_some() && offered.is_empty();
                if self.catalog.is_none() {
                    panel = panel.child(
                        list_row(false, chrome)
                            .text_color(chrome.ink_3)
                            .child("Resolving login-shell PATH..."),
                    );
                } else if offered.is_empty() {
                    panel = panel
                        .child(
                            div()
                                .px(px(10.))
                                .pt(px(4.))
                                .pb(px(2.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(dialog_text(chrome).child("No agents available"))
                                .child(
                                    dialog_text(chrome)
                                        .text_color(chrome.ink_3)
                                        .child("Turn one on in Settings."),
                                ),
                        )
                        .child(
                            list_row(true, chrome)
                                .id("picker-open-settings")
                                .cursor_pointer()
                                .child("Open Settings")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_settings(SettingsSection::Agents, window, cx);
                                })),
                        );
                } else {
                    for (i, preset) in offered.iter().enumerate() {
                        let detail = preset
                            .path
                            .as_deref()
                            .map(|path| model::tilde(path, home))
                            .unwrap_or_default();
                        panel = panel.child(
                            list_row(i == *index, chrome)
                                .id(SharedString::from(format!("preset-{i}")))
                                .gap(px(12.))
                                .cursor_pointer()
                                .child(kbd((i + 1).to_string(), chrome.sunken, chrome.ink_3))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(13.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(chrome.ink_1)
                                        .child(preset.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_right()
                                        .font_family(MONO)
                                        .text_size(px(11.))
                                        .text_color(chrome.ink_3)
                                        .child(detail),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(Overlay::Picker { index, .. }) = &mut this.overlay {
                                        *index = i;
                                    }
                                    this.launch(window, cx);
                                })),
                        );
                    }
                }
                panel.child(
                    div()
                        .flex()
                        .gap(px(14.))
                        .mt(px(6.))
                        .pt(px(10.))
                        .px(px(10.))
                        .pb(px(6.))
                        .border_t_1()
                        .border_color(chrome.line_2)
                        .text_size(px(11.))
                        .text_color(chrome.ink_3)
                        .child(if offer_settings {
                            "↵ settings"
                        } else {
                            "↵ start"
                        })
                        .child("esc cancel")
                        .child(match self.bases.get(project).and_then(|b| b.name.clone()) {
                            // Where New will start, so it is never a surprise.
                            Some(name) => div()
                                .flex()
                                .gap(px(4.))
                                .child(if *lead {
                                    "reads a checkout of"
                                } else {
                                    "branches from"
                                })
                                .child(div().font_family(MONO).child(name)),
                            None if *lead => div().child("reads a detached worktree"),
                            None => div().child("creates a branch and a worktree"),
                        }),
                )
            }
            Overlay::Preparation {
                project, config, ..
            } => {
                let name = self
                    .projects
                    .iter()
                    .find(|p| &p.id == project)
                    .map(|p| p.name.as_str())
                    .unwrap_or("");
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Prepare worktrees for {name}?")))
                    .child(dialog_text(chrome).child("These commands run with your permissions before each agent starts. Approve only a repository you trust, including its scripts."))
                    .child(dialog_text(chrome).font_family(MONO).child(".shika/worktrees.json"));
                for path in &config.copy_files {
                    panel = panel.child(
                        dialog_text(chrome)
                            .font_family(MONO)
                            .child(format!("Copy {path}")),
                    );
                }
                for command in &config.commands {
                    panel =
                        panel.child(dialog_text(chrome).font_family(MONO).child(command.clone()));
                }
                panel
                    .child(dialog_text(chrome).child(format!(
                        "Setup timeout: {} seconds. Configuration changes ask again.",
                        config.timeout_seconds
                    )))
                    .child(
                        dialog_buttons()
                            .child(self.cancel_button("Cancel", chrome, cx))
                            .child(
                                primary_button("approve-setup", "Approve and start", "↵", chrome)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.approve_preparation(window, cx)
                                    })),
                            ),
                    )
            }
            Overlay::Close { index, state } => {
                let i = *index;
                let can_push = state.can_push();
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Close “{}”?", self.cards[i].title)));
                if state.agent_working {
                    panel = panel.child(dialog_text(chrome).child("The agent is still working."));
                }
                if state.dirty {
                    panel = panel.child(dialog_text(chrome).child(
                        "The worktree has uncommitted changes. Commit in the shell or use Create PR before closing. Close does not commit.",
                    ));
                }
                if state.unpushed {
                    panel = panel.child(
                        dialog_text(chrome)
                            .child("The branch has commits that are not on the remote."),
                    );
                }
                panel =
                    panel.child(dialog_text(chrome).child(
                        "Discard stops the session and deletes the worktree and local branch.",
                    ));
                let discard =
                    cx.listener(move |this, _, window, cx| this.finish_close(i, 1, window, cx));
                let buttons = dialog_buttons().child(self.cancel_button("Cancel", chrome, cx));
                // The primary action is Push when it is offered.
                let buttons = if can_push {
                    buttons
                        .child(
                            dialog_button("discard", "Discard changes", "\u{2318}D", chrome)
                                .on_click(discard),
                        )
                        .child(
                            primary_button("push", "Push changes", "\u{2318}\u{21b5}", chrome)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.finish_close(i, 2, window, cx)
                                })),
                        )
                } else {
                    buttons.child(
                        primary_button("discard", "Discard changes", "\u{2318}D", chrome)
                            .on_click(discard),
                    )
                };
                panel.child(buttons)
            }
            Overlay::SwitchedClose {
                index,
                preview,
                working,
            } => {
                let i = *index;
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Close “{}” after branch switch?", self.cards[i].title)))
                    .child(dialog_text(chrome).child("This task started on:"))
                    .child(dialog_text(chrome).font_family(MONO).child(preview.recorded.clone()))
                    .child(dialog_text(chrome).child("The worktree is now on:"))
                    .child(dialog_text(chrome).font_family(MONO).child(preview.current.clone()))
                    .child(dialog_text(chrome).child("No unpublished work was found. Closing removes the worktree and stops all task terminals. Both local branches will be kept."));
                if *working {
                    panel = panel.child(
                        dialog_text(chrome)
                            .child("The agent is still active. Closing stops its current turn."),
                    );
                }
                panel.child(
                    dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(
                            primary_button(
                                "close-switched",
                                "Close task",
                                "\u{2318}\u{21b5}",
                                chrome,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.finish_close(i, 3, window, cx),
                            )),
                        ),
                )
            }
            Overlay::Leftovers => {
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Leftover worktrees"))
                    .child(dialog_text(chrome).child(
                        "These sessions ended when Shika quit. Their work remains on disk.",
                    ));
                let mut rows = div().flex().flex_col().gap(px(2.)).mt(px(4.));
                for (i, entry) in self.leftovers.iter().enumerate() {
                    rows = rows.child(
                        list_row(i == self.leftover_selected, chrome)
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .font_family(MONO)
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.))
                                            .child(entry.branch.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.))
                                            .text_color(chrome.ink_3)
                                            .child(model::tilde(&entry.path, home)),
                                    ),
                            )
                            .child(
                                secondary_button(SharedString::from(format!("remove-{i}")), chrome)
                                    .px(px(10.))
                                    .py(px(4.))
                                    .text_size(px(12.))
                                    .child("Remove worktree")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.overlay = Some(Overlay::RemoveLeftover(i));
                                        cx.notify();
                                    })),
                            ),
                    );
                }
                panel = panel.child(rows).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_x(px(12.))
                        .gap_y(px(6.))
                        .mt(px(4.))
                        .child(hint("j k", "choose", chrome))
                        .child(hint("d", "remove", chrome))
                        .child(hint("esc", "keep worktrees", chrome)),
                );
                panel.child(dialog_buttons().child(self.cancel_button("Cancel", chrome, cx)))
            }
            Overlay::RemoveProject(id) => {
                let id = id.clone();
                dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Remove project?"))
                    .child(dialog_text(chrome).child(
                        "Stops its sessions and forgets this project. Worktrees remain on disk in the leftovers list.",
                    ))
                    .child(
                        dialog_buttons().child(self.cancel_button("Cancel", chrome, cx)).child(
                            primary_button(
                                "remove-project",
                                "Remove project",
                                "\u{2318}\u{232b}",
                                chrome,
                            )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_project(id.clone(), cx)
                                })),
                        ),
                    )
            }
            Overlay::RemoveLeftover(i) => {
                let i = *i;
                dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Remove leftover worktree?"))
                    .child(
                        dialog_text(chrome)
                            .font_family(MONO)
                            .text_size(px(12.))
                            .child(model::tilde(&self.leftovers[i].path, home)),
                    )
                    .child(
                        dialog_text(chrome)
                            .child("This deletes any uncommitted work and the local branch."),
                    )
                    .child(
                        dialog_buttons()
                            .child(self.cancel_button("Cancel", chrome, cx))
                            .child(
                                primary_button(
                                    "remove-confirm",
                                    "Discard worktree",
                                    "\u{2318}\u{232b}",
                                    chrome,
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_leftover(i, cx)),
                                ),
                            ),
                    )
            }
            Overlay::Base {
                project,
                text,
                error,
                choices,
                highlight,
            } => {
                let name = self
                    .projects
                    .iter()
                    .find(|p| &p.id == project)
                    .map(|p| p.name.as_str())
                    .unwrap_or("");
                let default = self.bases.get(project).and_then(|b| b.default_name.clone());
                let field = text_field("base-branch-value", 360., true, chrome)
                    .when(!text.is_empty(), |d| d.child(text.clone()))
                    .child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                    .when_some(default.filter(|_| text.is_empty()), |d, default| {
                        d.child(div().text_color(chrome.ink_4).child(default))
                    });
                let names = choices
                    .as_ref()
                    .map(|choices| choices.names.as_slice())
                    .unwrap_or(&[]);
                let checked_out = choices
                    .as_ref()
                    .and_then(|choices| choices.checked_out.clone());
                let matched = model::branch_matches(names, text);
                let highlight = highlight
                    .as_ref()
                    .copied()
                    .filter(|index| *index < matched.len());
                let action = if self.chosen_base_name().trim().is_empty() {
                    "Use default branch"
                } else {
                    "Set base branch"
                };
                let fetch_hint = choices.as_ref().is_some_and(|choices| choices.has_origin)
                    && !text.is_empty()
                    && matched.is_empty()
                    && error.is_none();
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Base branch for {name}")))
                    .child(
                        dialog_text(chrome)
                            .child("New agents branch from it. Running agents keep their base."),
                    )
                    .child(field.mt(px(4.)));
                if !matched.is_empty() {
                    let mut rows = div()
                        .id("base-branch-list")
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .max_h(px(BASE_LIST_MAX))
                        .overflow_y_scroll()
                        .track_scroll(&self.base_scroll);
                    for (row, index) in matched.iter().copied().enumerate() {
                        let branch = names[index].clone();
                        let label = branch.clone();
                        let checked = checked_out.as_deref() == Some(label.as_str());
                        rows = rows.child(
                            list_row(highlight == Some(row), chrome)
                                .id(("base-branch-row", row))
                                .w_full()
                                .justify_between()
                                .gap(px(8.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_base(branch.clone(), window, cx);
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .font_family(MONO)
                                        .text_size(px(12.))
                                        .child(label),
                                )
                                .when(checked, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(chrome.ink_3)
                                            .child("checked out"),
                                    )
                                }),
                        );
                    }
                    panel = panel.child(rows);
                }
                if fetch_hint {
                    panel = panel.child(
                        dialog_text(chrome)
                            .text_color(chrome.ink_3)
                            .child("Not on this machine. Enter fetches it from origin."),
                    );
                }
                panel
                    .when_some(error.clone(), |d, error| {
                        d.child(dialog_text(chrome).text_color(chrome.ink_1).child(error))
                    })
                    .child(
                        dialog_buttons()
                            .child(self.cancel_button("Cancel", chrome, cx))
                            .child(primary_button("set-base", action, "↵", chrome).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let branch = this.chosen_base_name();
                                    this.apply_base(branch, window, cx);
                                }),
                            )),
                    )
            }
            Overlay::Settings { section, row, .. } => {
                self.settings_panel(*section, *row, max_h, chrome, home, cx)
            }
        };
        div()
            .absolute()
            .occlude()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(top)
            .child(panel)
            .into_any_element()
    }
}
impl Focusable for Shika {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
const UI_FONT: &str = ".AppleSystemUIFont";
const MONO: &str = "JetBrains Mono";
/// The terminal keeps at least this much of the window beside the column.
/// The 960px window minimum leaves room for the column at 30% or 320px.
const MIN_TERMINAL_WIDTH: f32 = 420.;
/// Every card's height: the 20px task line, the 6px gap, the 16px meta line,
/// and 23px of padding.
/// A closed card's row collapses from this.
const CARD_HEIGHT: f32 = 65.;
/// The space between cards, and between a project header and its cards.
const CARD_GAP: f32 = 6.;
/// A card dims to this while Close stops its terminals and removes its tree.
const CLOSING_CARD_OPACITY: f32 = 0.5;
/// A closing task's terminal fades to a trace of its output.
const CLOSING_TERMINAL_OPACITY: f32 = 0.06;
/// The status signal at the right end of a card's first line: the 14px
/// working pixels, or an 8px dot centered in it.
const SIGNAL_SLOT: f32 = 14.;
/// A closed card shrinks toward its center to this size as it fades out.
const EXIT_SCALE: f32 = 0.94;

/// A worker under its Lead sits this far in from the column's content edge.
const WORKER_INDENT: f32 = 16.;

/// One key hint at the right end of the selected row's second line.
fn row_hint(key: &'static str, label: &'static str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .ml(px(6.))
        .text_size(px(11.))
        .text_color(chrome.ink_3)
        .child(kbd(key, chrome.sunken, chrome.ink_2).py_0())
        .child(label)
}

/// The "Leads" or "Projects" header: a quiet sentence-case label, 12px
/// medium ink-3, aligned with the project header's text. Its actions follow
/// at the right, padded like a project header so the `+` buttons line up.
fn section_header(text: &'static str, chrome: &Chrome) -> gpui::Div {
    div()
        .min_h(px(24.))
        .flex()
        .items_center()
        .gap(px(2.))
        .pl(px(8.))
        .pr(px(4.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(chrome.ink_3)
                .child(text),
        )
}

/// A 24px column button: ink-3, ink-1 on the hover wash, with its name and
/// key in a tooltip. `icon` is an alpha-mask glyph at 15px; without one it
/// is the 16px `+` the project headers use.
fn icon_button(
    id: &'static str,
    icon: Option<&'static [u8]>,
    tip: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let (hover, ink_1, ink_3) = (chrome.hover, chrome.ink_1, chrome.ink_3);
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let button = div()
        .id(id)
        .group(id)
        .occlude()
        .flex_none()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(ink_3)
        .hover(move |style| style.bg(hover).text_color(ink_1))
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: tip.into(),
            })
            .into()
        });
    match icon {
        Some(data) => button.child(
            gpui::svg()
                .data(data)
                .size(px(15.))
                .text_color(ink_3)
                .group_hover(id, move |style| style.text_color(ink_1)),
        ),
        None => button.text_size(px(16.)).child("+"),
    }
}

/// The card and its leading gap occupy one nonnegative layout slot. At the
/// end of the exit there is nothing left to remove, including on an otherwise
/// empty project. The face's shrink is independent of this slot's collapse.
fn card_row_geometry(collapse: f32) -> (f32, f32) {
    let remaining = 1. - collapse.clamp(0., 1.);
    ((CARD_HEIGHT + CARD_GAP) * remaining, CARD_GAP * remaining)
}

fn card_row() -> gpui::Div {
    div()
        .w_full()
        .h(px(CARD_HEIGHT + CARD_GAP))
        .pt(px(CARD_GAP))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// A vector index belongs to a position, not a card. Keep the face and its
/// status animation keyed to the same view when earlier cards leave.
fn card_key(view: gpui::EntityId) -> SharedString {
    view.to_string().into()
}
/// The narrowest column that fits the footer's key hints beside the Leftover
/// worktrees button. Beside the gear alone they fit the narrowest column.
const FOOTER_HINTS_FIT: f32 = 420.;
/// The invisible strip over the column's edge that starts a resize.
const COLUMN_HANDLE_WIDTH: f32 = 8.;
/// The top row of both halves of the window, which is the title bar.
const BAR_HEIGHT: f32 = 48.;
/// Six base-branch rows. The list scrolls after that.
const BASE_LIST_MAX: f32 = 220.;
/// A terminal tab, sitting on the bottom line of the 48px header and
/// reaching to 5px under its top.
const TAB_HEIGHT: f32 = 42.;
/// Lifts a tab's label off the tab's own middle onto the bar's middle line,
/// where the traffic lights, the wordmark, and every header control sit.
/// The tab ends on the 1px bottom line, so its box is 5..47.
const TAB_LABEL_LIFT: f32 = BAR_HEIGHT - 2. - TAB_HEIGHT;
/// A header control's band: the bar above its 1px bottom line, minus the
/// same 1px on top, so a control centered in it sits on the bar's middle.
const CONTROL_BAND: f32 = BAR_HEIGHT - 2.;
/// The radius of a tab's top corners and of the concave flares that turn its
/// sides into the bottom line. The flares reach this far past the tab.
const TAB_FLARE: f32 = 7.;
/// The first tab's label lines up with the terminal text.
const TAB_LEAD: f32 = 8.;
/// The traffic lights sit 18px from the left, centered in the 48px bar.
/// AppKit's buttons are 14px tall on macOS 26, so 17px above and below.
const TRAFFIC_LIGHTS: (f32, f32) = (18., 17.);
/// The traffic lights end at x=78. The wordmark starts 12px after them.
const WORDMARK_INSET: f32 = 90.;

/// Heroicons cog-6-tooth (24px solid). MIT notice: assets/licenses/Heroicons-MIT.txt.
/// Drawn as an alpha mask and tinted by the element's text color.
const SETTINGS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-rule="evenodd" d="M11.078 2.25c-.917 0-1.699.663-1.85 1.567L9.05 4.889c-.02.12-.115.26-.297.348a7.493 7.493 0 0 0-.986.57c-.166.115-.334.126-.45.083L6.3 5.508a1.875 1.875 0 0 0-2.282.819l-.922 1.597a1.875 1.875 0 0 0 .432 2.385l.84.692c.095.078.17.229.154.43a7.598 7.598 0 0 0 0 1.139c.015.2-.059.352-.153.43l-.841.692a1.875 1.875 0 0 0-.432 2.385l.922 1.597a1.875 1.875 0 0 0 2.282.818l1.019-.382c.115-.043.283-.031.45.082.312.214.641.405.985.57.182.088.277.228.297.35l.178 1.071c.151.904.933 1.567 1.85 1.567h1.844c.916 0 1.699-.663 1.85-1.567l.178-1.072c.02-.12.114-.26.297-.349.344-.165.673-.356.985-.57.167-.114.335-.125.45-.082l1.02.382a1.875 1.875 0 0 0 2.28-.819l.923-1.597a1.875 1.875 0 0 0-.432-2.385l-.84-.692c-.095-.078-.17-.229-.154-.43a7.614 7.614 0 0 0 0-1.139c-.016-.2.059-.352.153-.43l.84-.692c.708-.582.891-1.59.433-2.385l-.922-1.597a1.875 1.875 0 0 0-2.282-.818l-1.02.382c-.114.043-.282.031-.449-.083a7.49 7.49 0 0 0-.985-.57c-.183-.087-.277-.227-.297-.348l-.179-1.072a1.875 1.875 0 0 0-1.85-1.567h-1.843ZM12 15.75a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5Z"/></svg>"##;

/// Heroicons folder-plus (24px outline), on the Projects header. Drawn as an
/// alpha mask and tinted by the element's text color.
/// MIT notice: assets/licenses/Heroicons-MIT.txt.
const FOLDER_PLUS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 10.5v6m3-3H9m4.06-7.19-2.12-2.12a1.5 1.5 0 0 0-1.061-.44H4.5A2.25 2.25 0 0 0 2.25 6v12a2.25 2.25 0 0 0 2.25 2.25h15A2.25 2.25 0 0 0 21.75 18V9a2.25 2.25 0 0 0-2.25-2.25h-5.379a1.5 1.5 0 0 1-1.06-.44Z"/></svg>"##;

/// The agent column toggle: a window with its left pane split off. Drawn for
/// Shika at 16px.
const COLUMN_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="#000" fill-rule="evenodd" d="M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm3 1.5H3a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h3v-9Zm1.5 0v9H13a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H7.5Z"/></svg>"##;

/// Octicons git-branch, before the base branch on a project header. Drawn as
/// an alpha mask and tinted by the element's text color.
/// MIT notice: assets/licenses/Octicons-MIT.txt.
const BRANCH_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="#000" fill-rule="evenodd" d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.493 2.493 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z"/></svg>"##;

/// A tooltip naming a control and its key, such as `Settings ⌘,`.
struct KeyTip {
    bg: Rgba,
    fg: Rgba,
    text: SharedString,
}

impl Render for KeyTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(self.bg)
            .text_color(self.fg)
            .text_size(px(12.))
            .font_family(UI_FONT)
            .child(self.text.clone())
    }
}

/// Paints the active terminal tab across `bounds`: the tab plus `TAB_FLARE`
/// on each side for its flares, and the 1px line row under it. The rounded
/// top and the concave flares are one shape, so the tab runs into the
/// terminal. The fill stops at the outline's outer edge, so nothing
/// translucent is stacked; neighbors stop their line where the outline
/// meets it.
fn paint_active_tab(bounds: Bounds<Pixels>, fill: Rgba, line: Rgba, window: &mut Window) {
    let (f, h) = (TAB_FLARE, TAB_HEIGHT);
    let w = f32::from(bounds.size.width);
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let at = |x: f32, y: f32| gpui::point(px(x0 + x), px(y0 + y));
    let radius = |r: f32| gpui::point(px(r), px(r));
    let (left, right) = (f, w - f);
    if let Some(path) = tab_shape(bounds, true, true, true) {
        window.paint_path(path, fill);
    }

    // The outline runs on pixel centers, from the bottom line on the left,
    // up and over the tab, and back down into the bottom line on the right.
    let mut outline = gpui::PathBuilder::stroke(px(1.));
    outline.move_to(at(0., h + 0.5));
    outline.line_to(at(0.5, h + 0.5));
    outline.arc_to(radius(f), px(0.), false, false, at(left + 0.5, h - f + 0.5));
    outline.line_to(at(left + 0.5, f));
    outline.arc_to(radius(f - 0.5), px(0.), false, true, at(left + f, 0.5));
    outline.line_to(at(right - f, 0.5));
    outline.arc_to(radius(f - 0.5), px(0.), false, true, at(right - 0.5, f));
    outline.line_to(at(right - 0.5, h - f + 0.5));
    outline.arc_to(radius(f), px(0.), false, false, at(w - 0.5, h + 0.5));
    outline.line_to(at(w, h + 0.5));
    if let Ok(path) = outline.build() {
        window.paint_path(path, line);
    }
}

/// A canvas over a terminal tab, extending `TAB_FLARE` past both sides and
/// covering the line row under it, so the tab's flares stay out of layout.
fn tab_canvas(paint: impl FnOnce(Bounds<Pixels>, &mut Window) + 'static) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint(bounds, window),
    )
    .absolute()
    .left(px(-TAB_FLARE))
    .right(px(-TAB_FLARE))
    .bottom_0()
    .h(px(TAB_HEIGHT + 1.))
}

/// The filled shape of a terminal tab across `bounds`, which extend
/// `TAB_FLARE` past both sides of the tab. The top corners are rounded. A
/// side with a flare curves outward into the bottom line; a side without one
/// rounds its bottom corner inward, exactly filling the inside of an active
/// neighbor's flare. The shape reaches the outer edge of the active tab's
/// 1px outline, and with `line_row` it also covers the line row under the
/// tab, so the active tab opens into the terminal.
fn tab_shape(
    bounds: Bounds<Pixels>,
    flare_left: bool,
    flare_right: bool,
    line_row: bool,
) -> Option<gpui::Path<Pixels>> {
    let (f, h) = (TAB_FLARE, TAB_HEIGHT);
    let w = f32::from(bounds.size.width);
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let at = |x: f32, y: f32| gpui::point(px(x0 + x), px(y0 + y));
    let radius = gpui::point(px(f - 0.5), px(f - 0.5));
    let corner = gpui::point(px(f), px(f));
    let bottom = if line_row { h + 1. } else { h };
    // The tab's sides: x = f on the left and w - f on the right. A bottom
    // corner's arc meets the side at h - f + 0.5 and the line at f - 0.5
    // from the side.
    let (left, right) = (f, w - f);
    let (side_end, reach) = (h - f + 0.5, f - 0.5);
    let mut shape = gpui::PathBuilder::fill();
    let foot = if flare_left {
        left - reach
    } else {
        left + reach
    };
    shape.move_to(at(foot, bottom));
    shape.line_to(at(foot, h));
    shape.arc_to(radius, px(0.), false, !flare_left, at(left, side_end));
    shape.line_to(at(left, f));
    shape.arc_to(corner, px(0.), false, true, at(left + f, 0.));
    shape.line_to(at(right - f, 0.));
    shape.arc_to(corner, px(0.), false, true, at(right, f));
    shape.line_to(at(right, side_end));
    let foot = if flare_right {
        right + reach
    } else {
        right - reach
    };
    shape.arc_to(radius, px(0.), false, !flare_right, at(foot, h));
    shape.line_to(at(foot, bottom));
    shape.close();
    shape.build().ok()
}

/// A key cap: mono 10.5px on a small rounded fill.
fn kbd(text: impl Into<SharedString>, bg: Rgba, fg: Rgba) -> gpui::Div {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .font_family(MONO)
        .text_size(px(10.5))
        .line_height(px(14.))
        .child(text.into())
}

/// A key cap and the verb it does, as in the footer: `⌘] ⌘[ move`. An empty
/// key leaves only the words. The pair stays on one line and does not shrink,
/// so a wrapped row hides nothing.
fn hint(key: &str, label: &str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(11.))
        .line_height(px(14.))
        .text_color(chrome.ink_3)
        .when(!key.is_empty(), |d| {
            d.child(kbd(key.to_string(), chrome.sunken, chrome.ink_2))
        })
        .child(label.to_string())
}

/// The PR number and its checks: a ring while they run, a check when they
/// pass, a cross in the failed color when one fails, and the number alone
/// when the repository reports none. A click opens the PR.
fn pr_mark(
    card: &str,
    pr: &checks::PrWatch,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> impl IntoElement {
    let mark = pr.mark();
    let color = if mark == checks::Mark::Failed {
        chrome.failed.text
    } else {
        chrome.ink_3
    };
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let tip = model::checks_tip(mark);
    let url = pr.url.clone();
    div()
        .id(SharedString::from(format!("pr-{card}")))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_color(color)
        .when(mark == checks::Mark::Failed, |d| {
            d.font_weight(FontWeight::MEDIUM)
        })
        .child(
            div()
                .font_family(MONO)
                .text_size(px(11.5))
                .child(format!("#{}", pr.number)),
        )
        .map(|d| match mark {
            checks::Mark::Pending => d.child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .border_1()
                    .border_color(chrome.ink_4),
            ),
            checks::Mark::Passed => d.child("\u{2713}"),
            checks::Mark::Failed => d.child("\u{2717}"),
            checks::Mark::None => d,
        })
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: tip.into(),
            })
            .into()
        })
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.open_url(&url);
        }))
}

/// The CLI on a card's second line: its mark at 14px in the line's ink, with
/// the name as its tooltip. A CLI without a mark keeps its name.
fn cli_mark(
    id: SharedString,
    preset: &str,
    name: &str,
    color: Rgba,
    chrome: &Chrome,
) -> gpui::AnyElement {
    let Some(mark) = agent_marks::for_preset(preset) else {
        return div().flex_none().child(name.to_string()).into_any_element();
    };
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let name = SharedString::from(name.to_string());
    div()
        .id(id)
        .flex_none()
        .child(gpui::svg().data(mark).size(px(14.)).text_color(color))
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: name.clone(),
            })
            .into()
        })
        .into_any_element()
}

/// The right end of a card's first line, one 14px slot so the signals line
/// up down the column: working pixels, a ready dot until the result is seen,
/// a grey waiting dot, and nothing once a Ready result has been seen.
/// The antler mark on a raised tile, left of both lines of a Lead's card.
fn antler_tile(selected: bool, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .mt(px(2.))
        .size(px(30.))
        .rounded(px(8.))
        .bg(if selected {
            chrome.raised_hover
        } else {
            chrome.raised
        })
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::svg()
                .data(lead_chat::ANTLERS_ICON)
                .size(px(18.))
                .text_color(chrome.ink_2),
        )
}

fn card_signal(card: &str, status: Status, unseen: bool, chrome: &Chrome) -> gpui::AnyElement {
    let slot = div().flex_none().flex().justify_center().w(px(SIGNAL_SLOT));
    let color = chrome.status(status).dot;
    match status {
        Status::Working => slot.child(working_pixels(card, color)),
        Status::Ready if !unseen => slot,
        _ => slot.child(div().size(px(8.)).rounded_full().bg(color)),
    }
    .into_any_element()
}

/// The terminal area. While its task closes, the terminal fades to a trace
/// behind grey sparkling pixels and "Closing...", so the CLI's exit line and the
/// stopped session do not read as the result. Opacity only: the terminal
/// keeps its size, so its PTY is never resized.
fn closing_terminal(card: &Card, pane: &Pane, chrome: &Chrome) -> gpui::AnyElement {
    let terminal = div().flex_1().min_h_0().child(pane.view.clone());
    if !card.closing {
        return terminal.into_any_element();
    }
    let view = card.agent.view.entity_id();
    let fade = || gpui::Animation::new(model::CLOSING_FADE).with_easing(model::ease);
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(terminal.with_animation(
            SharedString::from(format!("terminal-closing-{view}")),
            fade(),
            |d, t| d.opacity(1. - (1. - CLOSING_TERMINAL_OPACITY) * t),
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .text_size(px(13.))
                .text_color(chrome.term_faint)
                .child(pixel_sparkle(
                    format!("closing-pixels-{view}"),
                    chrome.term_dim,
                    4.,
                    2.,
                ))
                .child("Closing...")
                .with_animation(
                    SharedString::from(format!("closing-label-{view}")),
                    fade(),
                    |d, t| d.opacity(t),
                ),
        )
        .into_any_element()
}

/// A working card's indicator: a 5x5 field of 2px pixels, 14px square, that
/// twinkle about eight at a time. Every size and offset is a whole point, so
/// the pixels stay crisp and the field sits 3px from the top of the 20px
/// line, level with the 8px dots. GPUI skips the loop when macOS asks for
/// reduced motion, leaving the first frame, a scatter of lit pixels.
fn working_pixels(card: &str, color: gpui::Rgba) -> gpui::AnyElement {
    pixel_sparkle(format!("working-{card}"), color, 2., 1.)
}

/// One sparkle cycle. Each pixel lights once per cycle.
const SPARKLE_CYCLE: Duration = Duration::from_millis(2000);
/// The share of a cycle one pixel stays lit, rising and falling.
const SPARKLE_TWINKLE: f32 = 0.36;
/// When each pixel of the 5x5 field lights, row by row: pixel `i` starts at
/// `SPARKLE_SLOT[i] / 25` of the cycle. Pixels that start within four slots
/// of each other are never neighbors, so the field reads as scattered light
/// rather than a sweep.
const SPARKLE_SLOT: [u8; 25] = [
    2, 14, 9, 20, 8, //
    7, 19, 4, 15, 3, //
    1, 13, 24, 10, 22, //
    21, 6, 18, 5, 17, //
    16, 11, 23, 12, 0,
];

/// How lit a pixel is at phase `t` of the cycle: off, or a third, two
/// thirds, or full, in one rise and fall starting at its slot.
fn sparkle_level(t: f32, slot: u8) -> f32 {
    let local = (t - f32::from(slot) / 25.).rem_euclid(1.);
    if local >= SPARKLE_TWINKLE {
        return 0.;
    }
    ((std::f32::consts::PI * local / SPARKLE_TWINKLE).sin() * 3.).round() / 3.
}

/// A 5x5 field of `size` pixels, `gap` apart, that twinkle in `color`. A lit
/// pixel blooms: a soft glow of its own color, as strong as the pixel is lit.
fn pixel_sparkle(id: String, color: gpui::Rgba, size: f32, gap: f32) -> gpui::AnyElement {
    let side = 5. * size + 4. * gap;
    div()
        .flex_none()
        .relative()
        .size(px(side))
        .with_animation(
            SharedString::from(id),
            gpui::Animation::new(SPARKLE_CYCLE)
                .repeat_synced()
                .with_max_fps(30.),
            move |d, t| {
                d.children(SPARKLE_SLOT.iter().enumerate().filter_map(|(i, &slot)| {
                    let level = sparkle_level(t, slot);
                    (level > 0.).then(|| {
                        div()
                            .absolute()
                            .left(px((i % 5) as f32 * (size + gap)))
                            .top(px((i / 5) as f32 * (size + gap)))
                            .size(px(size))
                            .bg(with_alpha(color, color.a * level))
                            .shadow(vec![
                                BoxShadow::new(
                                    px(0.),
                                    px(0.),
                                    with_alpha(color, color.a * level * 0.5).into(),
                                )
                                .blur_radius(px(size * 1.5)),
                            ])
                    })
                }))
            },
        )
        .into_any_element()
}

/// A secondary button without its label: raised fill, control border, and
/// the light-mode control shadow.
fn secondary_button(id: impl Into<gpui::ElementId>, chrome: &Chrome) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.raised_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .border_1()
        .border_color(chrome.line_control)
        .bg(chrome.raised)
        .rounded(px(7.))
        .text_size(px(12.5))
        .line_height(px(16.))
        .text_color(chrome.ink_1)
        .cursor_pointer()
        .shadow(vec![
            BoxShadow::new(px(0.), px(1.), chrome.control_shadow.into()).blur_radius(px(1.)),
        ])
        .hover(move |style| style.bg(hover))
}

/// A dialog's secondary action with its key after the label.
fn dialog_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    key: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(10.))
        .py(px(6.))
        .child(label.into())
        .child(
            div()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(chrome.ink_3)
                .child(key),
        )
}

/// A notice button with no key. The update card does not take focus.
fn notice_secondary(
    id: &'static str,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .px(px(12.))
        .py(px(6.))
        .child(label)
}

/// The ink-filled notice button, with the primary button's padding and no key.
fn notice_primary(
    id: &'static str,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.primary_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(7.))
        .px(px(12.))
        .py(px(7.))
        .bg(chrome.primary_bg)
        .text_color(chrome.primary_fg)
        .text_size(px(12.5))
        .line_height(px(16.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}

/// The ink-filled primary action of a dialog.
fn primary_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    key: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.primary_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(7.))
        .pl(px(12.))
        .pr(px(10.))
        .py(px(7.))
        .bg(chrome.primary_bg)
        .text_color(chrome.primary_fg)
        .text_size(px(12.5))
        .line_height(px(16.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label.into())
        .child(
            div()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(chrome.kbd_inverse)
                .child(key),
        )
}

/// A settings `-` or `+`: a small secondary button.
fn step_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .size(px(26.))
        .justify_center()
        .rounded(px(6.))
        .text_size(px(13.))
        .text_color(chrome.ink_2)
        .child(label)
}

/// A settings number or text field.
fn text_field(
    id: impl Into<gpui::ElementId>,
    width: f32,
    editing: bool,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(width))
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded(px(6.))
        .bg(chrome.raised)
        .border_1()
        .border_color(if editing {
            chrome.focus
        } else {
            chrome.line_control
        })
        .font_family(MONO)
        .text_size(px(12.))
        .cursor_text()
}

/// One segment of a segmented control.
fn segment(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    active_bg: Rgba,
    active_fg: Rgba,
    idle_fg: Rgba,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(5.))
        .text_size(px(12.))
        .line_height(px(16.))
        .cursor_pointer()
        .when(active, |d| d.bg(active_bg).text_color(active_fg))
        .when(!active, |d| d.text_color(idle_fg))
        .child(label.into())
}

/// A list row in the picker, Settings, and the leftovers list.
fn list_row(selected: bool, chrome: &Chrome) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(7.))
        .text_size(px(13.))
        .line_height(px(19.))
        .when(selected, |d| d.bg(chrome.row_selected))
}

/// The floating surface shared by the picker, every dialog, and the update notice.
fn dialog_shell(width: f32, max_h: Pixels, chrome: &Chrome) -> gpui::Stateful<gpui::Div> {
    panel_shell("overlay-panel", width, max_h, chrome)
}

fn panel_shell(
    id: &'static str,
    width: f32,
    max_h: Pixels,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(width))
        .max_h(max_h)
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .bg(chrome.overlay)
        .text_color(chrome.ink_1)
        .shadow(vec![
            BoxShadow::new(px(0.), px(24.), chrome.dialog_shadow.into()).blur_radius(px(60.)),
            BoxShadow::new(px(0.), px(0.), chrome.dialog_ring.into()).spread_radius(px(0.5)),
        ])
}

/// A confirm dialog: 400px, padded for a title, facts, and buttons.
fn dialog(chrome: &Chrome, max_h: Pixels) -> gpui::Stateful<gpui::Div> {
    dialog_shell(400., max_h, chrome)
        .pt(px(20.))
        .px(px(20.))
        .pb(px(16.))
        .gap(px(8.))
}

fn dialog_title(d: gpui::Div) -> gpui::Div {
    d.text_size(px(14.))
        .line_height(px(20.))
        .font_weight(FontWeight::SEMIBOLD)
}

fn dialog_text(chrome: &Chrome) -> gpui::Div {
    div()
        .text_size(px(13.))
        .line_height(px(19.))
        .text_color(chrome.ink_2)
}

fn dialog_buttons() -> gpui::Div {
    div().flex().justify_end().gap(px(8.)).mt(px(10.))
}

/// `$HOME`, to write paths with `~`.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

impl Render for Shika {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chat_projects = self
            .projects
            .iter()
            .filter(|p| p.lead.as_ref().is_some_and(|lead| lead.cli == "claude"))
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        for project in chat_projects {
            self.ensure_lead_chat(&project, window, cx);
        }
        self.sync_window_backdrop(window, cx);
        self.observe_dialog(Instant::now());
        let cards_focused = self.focus.is_focused(window);
        let chrome = self.chrome(window);
        let backdrop = self.backdrop_layers(&chrome, window, cx);
        let home = home_dir();
        let reveal_selection = self.selection != self.last_revealed_selection;
        self.last_revealed_selection = self.selection.clone();
        // The Changes panel follows the selected card. Closed, this is a no-op.
        self.sync_changes(cx);
        let mut projects = div()
            .id("projects")
            .track_scroll(&self.sidebar_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.))
            .pt(px(8.))
            .pb(px(18.))
            .flex()
            .flex_col()
            .gap(px(20.));
        let layout = self.column_layout();
        projects = projects.child(self.leads_section(
            &layout,
            &chrome,
            cards_focused,
            reveal_selection,
            cx,
        ));
        let order = layout
            .projects
            .iter()
            .map(|e| e.project.clone())
            .collect::<Vec<_>>();
        if order != self.project_order {
            self.project_order = order;
            self.project_order_epoch += 1;
        }
        let mut groups = div().flex_none().flex().flex_col().gap(px(20.));
        for entry in &layout.projects {
            let Some(project) = self.projects.iter().find(|p| p.id == entry.project) else {
                continue;
            };
            groups = groups.child(project_motion::project_motion(
                SharedString::from(format!("project-motion-{}", project.id)),
                self.project_group(
                    project,
                    entry.cards.clone(),
                    home.as_deref(),
                    &chrome,
                    cards_focused,
                    reveal_selection,
                    cx,
                ),
                self.project_order_epoch,
                self.sidebar_scroll.clone(),
            ));
        }
        projects = projects.child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(CARD_GAP))
                .child(self.projects_header(&chrome, cx))
                .child(groups),
        );
        let (column_width, changes_width) = self.pane_widths(window);
        let sidebar = column_width.map(|width| {
            div()
                .w(width)
                .flex_shrink_0()
                .h_full()
                .overflow_hidden()
                .flex()
                .flex_col()
                .bg(chrome.column)
                .border_r_1()
                .border_color(chrome.hairline)
                .child(self.top_row(&chrome, window, cx))
                .child(projects)
                .child(self.footer(&chrome, width, cx))
        });
        let mut root = div()
            .track_focus(&self.focus)
            .key_context("Shika")
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.picker(window, cx)))
            .on_action(cx.listener(|this, _: &NewLead, window, cx| this.new_lead(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| this.new_shell(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseTerminal, window, cx| {
                if let Some(i) = this.selected_card() {
                    this.close_tab(this.cards[i].active_tab, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RenameTask, window, cx| this.rename_task(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTask, window, cx| {
                if matches!(this.overlay, Some(Overlay::CardMenu(_))) {
                    this.close_from_card_menu(window, cx);
                } else if matches!(this.overlay, Some(Overlay::RestingMenu { .. })) {
                    this.forget_from_menu(window, cx);
                } else if !this.busy && this.overlay.is_none() {
                    this.close(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NextTerminal, window, cx| this.cycle_tab(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousTerminal, window, cx| {
                this.cycle_tab(false, window, cx)
            }))
            .on_action(cx.listener(|this, select: &SelectTerminal, window, cx| {
                this.select_tab(select.0, window, cx);
            }))
            .on_action(cx.listener(|this, select: &SelectAgent, window, cx| {
                this.select_agent(select.0, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &NextAgent, window, cx| this.move_agent(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousAgent, window, cx| this.move_agent(-1, window, cx)),
            )
            .capture_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleColumn, _, cx| this.toggle_column(cx)))
            .on_action(
                cx.listener(|this, _: &ToggleChanges, window, cx| this.toggle_changes(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleUsage, window, cx| this.toggle_usage(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CreatePr, window, cx| this.create_pr(window, cx)))
            .on_action(
                cx.listener(|this, _: &SwitchAgent, window, cx| this.switch_agent(window, cx)),
            )
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<ColumnDrag>, window, cx| {
                    this.drag_column(event.event.position.x, window, cx)
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<changes::ChangesDrag>, window, cx| {
                    this.drag_changes(event.event.position.x, window, cx)
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<background_view::PanDrag>, _, cx| {
                    this.drag_preview(event.event.position, cx)
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<background_view::ZoomDrag>, _, cx| {
                    this.drag_zoom(event.event.position.x, cx)
                },
            ))
            .relative()
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(gpui::relative(1.3))
            .text_color(chrome.ink_1)
            .children(backdrop)
            .children(sidebar)
            .child(self.terminal_side(&chrome, cards_focused, home.as_deref(), window, cx))
            .children(changes_width.map(|width| self.changes_panel(width, &chrome, window, cx)))
            .children(column_width.map(|width| self.column_handle(width, &chrome, cx)))
            .children(changes_width.map(|width| {
                let left = window.viewport_size().width - width;
                self.changes_handle(left, &chrome, cx)
            }));
        if let Some(card) = &self.update_notice
            && card.centered()
        {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.update_notice_panel(card, &chrome, window, cx)),
            );
        }
        if let Some(overlay) = &self.overlay {
            root = root.child(self.overlay_view(overlay, &chrome, home.as_deref(), window, cx));
        }
        root
    }
}
fn sample_update_notice() -> updates::UpdateCard {
    updates::UpdateCard::Available {
        version: "0.6.0".into(),
        body: updates::available_body("", false, Some(&updates::Offer::Download)),
        offer: Some(updates::Offer::Download),
    }
}

/// Bind the terminal, the fixed app shortcuts, and the user's overrides, then
/// refresh the menus so they show the chords that are actually bound.
fn install_keys(cx: &mut App, keys: &KeyOverrides) {
    cx.clear_key_bindings();
    shika_terminal::init(cx);
    let mut bindings = vec![
        gpui::KeyBinding::new("cmd-n", NewAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-l", NewLead, Some("Shika")),
        gpui::KeyBinding::new("cmd-t", NewTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-p", CreatePr, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-a", SwitchAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-w", CloseTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-w", CloseTask, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-r", RenameTask, Some("Shika")),
        gpui::KeyBinding::new("ctrl-tab", NextTerminal, Some("Shika")),
        gpui::KeyBinding::new("ctrl-shift-tab", PreviousTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-]", NextAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-[", PreviousAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-b", ToggleColumn, Some("Shika")),
        gpui::KeyBinding::new("cmd-q", Quit, None),
        gpui::KeyBinding::new("cmd-,", OpenSettings, None),
        gpui::KeyBinding::new("cmd-h", Hide, None),
        gpui::KeyBinding::new("cmd-alt-h", HideOthers, None),
    ];
    for binding in shortcuts::bindings(keys) {
        let Some(chord) = binding.chord.as_deref() else {
            continue;
        };
        if binding.id == "toggleChanges" {
            bindings.push(gpui::KeyBinding::new(chord, ToggleChanges, Some("Shika")));
        } else if binding.id == "toggleUsage" {
            bindings.push(gpui::KeyBinding::new(chord, ToggleUsage, Some("Shika")));
        } else if let Some(index) = shortcuts::agent_slot(binding.id) {
            bindings.push(gpui::KeyBinding::new(
                chord,
                SelectAgent(index),
                Some("Shika"),
            ));
        } else if let Some(index) = shortcuts::tab_slot(binding.id) {
            bindings.push(gpui::KeyBinding::new(
                chord,
                SelectTerminal(index),
                Some("Shika"),
            ));
        }
    }
    cx.bind_keys(bindings);
    install_menus(cx);
}

fn install_menus(cx: &mut App) {
    let mut app_menu = Vec::new();
    if cx.has_global::<updates::Updater>() {
        app_menu.push(gpui::MenuItem::action(
            "Check for updates...",
            CheckForUpdates,
        ));
    }
    app_menu.extend([
        gpui::MenuItem::action("Settings...", OpenSettings),
        gpui::MenuItem::separator(),
        gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
        gpui::MenuItem::separator(),
        gpui::MenuItem::action("Hide Shika", Hide),
        gpui::MenuItem::action("Hide others", HideOthers),
        gpui::MenuItem::action("Show all", ShowAll),
        gpui::MenuItem::separator(),
        gpui::MenuItem::action("Quit Shika", Quit),
    ]);
    cx.set_menus([
        gpui::Menu::new("Shika").items(app_menu),
        gpui::Menu::new("Agent").items([
            gpui::MenuItem::action("New agent", NewAgent),
            gpui::MenuItem::action("New Lead…", NewLead),
            gpui::MenuItem::action("New terminal tab", NewTerminal),
            gpui::MenuItem::action("Create PR", CreatePr),
            gpui::MenuItem::action("Switch agent…", SwitchAgent),
            gpui::MenuItem::action("Close terminal tab", CloseTerminal),
            gpui::MenuItem::action("Rename task…", RenameTask),
            gpui::MenuItem::action("Close task", CloseTask),
            gpui::MenuItem::action("Next terminal tab", NextTerminal),
            gpui::MenuItem::action("Previous terminal tab", PreviousTerminal),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Next agent", NextAgent),
            gpui::MenuItem::action("Previous agent", PreviousAgent),
        ]),
        gpui::Menu::new("View").items([
            gpui::MenuItem::action("Hide or show agent column", ToggleColumn),
            gpui::MenuItem::action("Hide or show changes", ToggleChanges),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Usage", ToggleUsage),
        ]),
    ]);
}

fn main() -> anyhow::Result<()> {
    // `shika help`, `shika new`, and the rest run in a Lead's terminal as this
    // same binary. They only talk to the socket: no GPUI, settings, or data.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Inside a Lead terminal (either variable set) this is always the client,
    // so a bare or mistyped `shika` never starts a second app on real data.
    let lead_env =
        std::env::var_os("SHIKA_SOCKET").is_some() || std::env::var_os("SHIKA_TOKEN").is_some();
    if shika_core::control::is_client_invocation(&args, lead_env) {
        std::process::exit(control_client::run(&args));
    }
    let mut data = None;
    let mut diagnostics = None;
    let mut preview_update = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--preview-update" => preview_update = true,
            "--data-dir" => {
                data = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?,
                ))
            }
            "--diagnostics-file" => {
                diagnostics =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--diagnostics-file needs a path")
                    })?))
            }
            other => anyhow::bail!("Unknown option {other}"),
        }
    }
    let core = Arc::new(Core::open(data.unwrap_or(shika_core::app_data_dir()?))?);
    core.start_usage_collection();
    gpui_platform::application().run(move |cx: &mut App| {
        let fonts = [
            include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf").as_slice(),
            include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf").as_slice(),
            include_bytes!("../../../assets/fonts/JetBrainsMono-Italic.ttf").as_slice(),
            include_bytes!("../../../assets/fonts/JetBrainsMono-BoldItalic.ttf").as_slice(),
        ]
        .into_iter()
        .map(std::borrow::Cow::Borrowed)
        .collect();
        if let Err(error) = cx.text_system().add_fonts(fonts) {
            eprintln!("JetBrains Mono: {error}");
        }
        let quitting_core = core.clone();
        cx.on_app_quit(move |_| {
            quitting_core.cancel_preparations();
            quitting_core.stop_lead_chats();
            async {}
        })
        .detach();
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        // Only a release bundle has Sparkle, so only it offers the check.
        if let Some(updater) = updates::Updater::start() {
            cx.set_global(updater);
            cx.on_action(|_: &CheckForUpdates, cx| cx.global::<updates::Updater>().check());
        }
        install_keys(cx, &KeyOverrides::default());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let mut bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
        // The sample sits beside a normal window instead of covering it.
        if preview_update {
            bounds.origin.x += px(48.);
            bounds.origin.y += px(36.);
        }
        let settings = core.settings();
        let start = settings.as_ref().map(|s| s.appearance).unwrap_or_default();
        let start_image = settings.as_ref().is_ok_and(|s| s.background.is_some());
        // Before the window opens, so it starts in the forced appearance.
        appearance::apply_mode(settings.as_ref().map(|s| s.theme.mode).unwrap_or_default());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(if preview_update {
                            "Shika preview".into()
                        } else {
                            "Shika".into()
                        }),
                        appears_transparent: true,
                        // Centers the traffic lights in the 48px top row.
                        traffic_light_position: Some(gpui::point(
                            px(TRAFFIC_LIGHTS.0),
                            px(TRAFFIC_LIGHTS.1),
                        )),
                    }),
                    // The column at 30% (320px here) plus a terminal wide enough to read.
                    window_min_size: Some(size(px(960.), px(600.))),
                    // The column toggle lives in the title bar, so clicks
                    // there have to reach the app. The bar starts the drag itself.
                    app_owns_titlebar_drag: true,
                    window_background: appearance::background(&start, start_image),
                    ..Default::default()
                },
                |window, cx| {
                    let app = cx.new(|cx| {
                        Shika::new(core, settings, diagnostics, preview_update, window, cx)
                    });
                    window.focus(&app.focus_handle(cx), cx);
                    app
                },
            )
            .expect("Open Shika window");
        // The blur radius needs the window on screen, which it is now.
        let _ = handle.update(cx, |_, window, _| {
            appearance::apply(&start, start_image, window)
        });
        // Closes the control socket and removes its directory on quit; the
        // window's state may never be dropped.
        cx.on_app_quit(move |cx| {
            let _ = handle.update(cx, |app, _, _| app.control = None);
            async {}
        })
        .detach();
        cx.activate(true);
    });
    Ok(())
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn exiting_row_keeps_its_gap_until_collapse_and_leaves_no_final_jump() {
        let full = CARD_HEIGHT + CARD_GAP;
        let shrinking = model::EXIT_SHRINK.as_secs_f32() / model::EXIT.as_secs_f32();
        assert_eq!(card_row_geometry(model::card_exit(0.).1), (full, CARD_GAP));
        assert_eq!(
            card_row_geometry(model::card_exit(shrinking).1),
            (full, CARD_GAP)
        );
        let mut previous = full;
        for step in 0..=100 {
            let (_, collapse) = model::card_exit(step as f32 / 100.);
            let (height, gap) = card_row_geometry(collapse);
            assert!(height >= 0. && height <= previous);
            assert!(gap >= 0. && gap <= height);
            assert!((height / full - gap / CARD_GAP).abs() < 1e-6);
            previous = height;
        }
        // Removing this slot does not move a follower or a later project
        // header, and an only-card project returns to its header's height.
        assert_eq!(card_row_geometry(model::card_exit(1.).1), (0., 0.));
    }

    #[test]
    fn settings_projects_is_a_separate_section_and_navigation_wraps() {
        assert!(SettingsSection::ALL.len() == 5);
        assert!(SettingsSection::Agents.step(1) == SettingsSection::Projects);
        assert!(SettingsSection::Projects.step(1) == SettingsSection::Usage);
        assert!(SettingsSection::Usage.step(1) == SettingsSection::Appearance);
        assert!(SettingsSection::Appearance.step(-1) == SettingsSection::Usage);
        assert!(SettingsSection::Projects.label() == "Projects");
    }

    #[test]
    fn every_sparkle_pixel_lights_once_per_cycle() {
        let mut slots = SPARKLE_SLOT.to_vec();
        slots.sort_unstable();
        assert_eq!(slots, (0..25).collect::<Vec<u8>>());
    }

    #[test]
    fn sparkle_pixels_that_start_together_are_never_neighbors() {
        for (a, &sa) in SPARKLE_SLOT.iter().enumerate() {
            for (b, &sb) in SPARKLE_SLOT.iter().enumerate() {
                let apart = (i32::from(sa) - i32::from(sb)).rem_euclid(25);
                let near = (1..=4).contains(&apart.min(25 - apart));
                let (ax, ay, bx, by) = (a % 5, a / 5, b % 5, b / 5);
                let touching = ax.abs_diff(bx) <= 1 && ay.abs_diff(by) <= 1;
                assert!(!(near && touching), "pixels {a} and {b}");
            }
        }
    }

    #[test]
    fn sparkle_keeps_about_eight_pixels_lit_in_steps() {
        // The first frame is the still frame under reduced motion.
        let lit = |t: f32| {
            SPARKLE_SLOT
                .iter()
                .filter(|&&s| sparkle_level(t, s) > 0.)
                .count()
        };
        assert!(lit(0.) >= 6);
        for step in 0..300 {
            let t = step as f32 / 300.;
            assert!((7..=10).contains(&lit(t)), "{} lit at {t}", lit(t));
            for &slot in &SPARKLE_SLOT {
                let level = sparkle_level(t, slot);
                assert!([0., 1. / 3., 2. / 3., 1.].contains(&level));
            }
        }
    }

    #[test]
    fn card_menu_stays_bound_to_the_clicked_card_after_reordering_or_removal() {
        let first = gpui::EntityId::from(1);
        let clicked = gpui::EntityId::from(2);
        let third = gpui::EntityId::from(3);
        let menu = CardMenu {
            target: clicked,
            position: gpui::point(px(100.), px(200.)),
            action: CardMenuAction::Close,
        };
        assert_eq!(
            menu.target_index([first, clicked, third].into_iter()),
            Some(1)
        );
        assert_eq!(
            menu.target_index([clicked, third, first].into_iter()),
            Some(0)
        );
        assert_eq!(menu.target_index([clicked, third].into_iter()), Some(0));
        // A disappeared target must not close the card that inherited its index.
        assert_eq!(menu.target_index([first, third].into_iter()), None);
        assert_eq!(menu.target_index(std::iter::empty()), None);
    }

    #[test]
    fn agent_navigation_skips_headers_and_wraps_in_both_directions() {
        let rows = vec![
            Selection::Project("a".into()),
            Selection::Card(2),
            Selection::Card(0),
            Selection::Project("b".into()),
            Selection::Card(1),
        ];
        let card = |i| Some(Selection::Card(i));
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), card(1));
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(1)), 1), card(2));
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(2)), -1),
            card(1)
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Project("b".into())), -1),
            card(0)
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Project("b".into())), 1),
            card(1)
        );
    }

    #[test]
    fn a_resting_lead_is_an_agent_stop_in_row_order() {
        let rows = vec![
            Selection::Resting("a".into()),
            Selection::Card(0),
            Selection::Resting("c".into()),
            Selection::Project("a".into()),
            Selection::Card(1),
        ];
        let resting = |p: &str| Some(Selection::Resting(p.into()));
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Resting("a".into())), 1),
            Some(Selection::Card(0))
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(0)), 1),
            resting("c")
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Resting("c".into())), 1),
            Some(Selection::Card(1))
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Resting("a".into())), -1),
            Some(Selection::Card(1))
        );
        assert_eq!(nth_agent(&rows, 0), resting("a"));
        assert_eq!(nth_agent(&rows, 2), resting("c"));
        assert_eq!(nth_agent(&rows, 3), Some(Selection::Card(1)));
        assert_eq!(nth_agent(&rows, 4), None);
    }

    #[test]
    fn agent_navigation_handles_empty_single_and_missing_selection() {
        let card = |i| Some(Selection::Card(i));
        assert_eq!(adjacent_agent(&[], None, 1), None);
        let rows = vec![Selection::Project("a".into()), Selection::Card(0)];
        assert_eq!(adjacent_agent(&rows, None, 1), card(0));
        assert_eq!(adjacent_agent(&rows, None, -1), card(0));
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), card(0));
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(0)), -1),
            card(0)
        );
        assert_eq!(adjacent_agent(&rows[..1], None, 1), None);
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(9)), -1),
            card(0)
        );
    }

    #[test]
    fn numbered_agents_follow_column_order_and_stop_when_the_card_is_missing() {
        let rows = vec![
            Selection::Project("a".into()),
            Selection::Card(2),
            Selection::Card(0),
            Selection::Project("b".into()),
            Selection::Card(1),
        ];
        assert_eq!(nth_agent(&rows, 0), Some(Selection::Card(2)));
        assert_eq!(nth_agent(&rows, 1), Some(Selection::Card(0)));
        assert_eq!(nth_agent(&rows, 2), Some(Selection::Card(1)));
        assert_eq!(nth_agent(&rows, 3), None);
        assert_eq!(nth_agent(&rows, 8), None);
        assert_eq!(nth_agent(&[], 0), None);
    }

    #[test]
    fn only_nonempty_typed_submissions_start_candidate_turns() {
        let path = std::env::temp_dir().join(format!(
            "shika-submission-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState::default()));
        let host = Host {
            core: core.clone(),
            state: state.clone(),
            capture: true,
        };
        host.write(b"\r", InputSource::Typed);
        host.write(b"\x1b[I", InputSource::Report);
        // Even a report/reply containing CR must never name/start a turn.
        host.write(b"mouse\r", InputSource::Report);
        host.write(b"reply\r", InputSource::Reply);
        host.resize(TerminalSize::new(20, 80));
        assert_eq!(lock(&state).submission, 0);
        assert!(lock(&state).title.is_none());
        host.write(b"fix it\r", InputSource::Typed);
        assert_eq!(lock(&state).submission, 1);
        assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
        let submitted = lock(&state).last_submission;
        host.write(b"draft", InputSource::Typed);
        host.write(b"\x15", InputSource::Typed);
        host.write(b"\x1b[200~line one\nline two\x1b[201~", InputSource::Typed);
        assert_eq!(lock(&state).submission, 1);
        assert_eq!(lock(&state).last_submission, submitted);
        host.write(b"\r", InputSource::Typed);
        assert_eq!(lock(&state).submission, 2);
        assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
        host.write(b"cleared draft\x01\x0b\r", InputSource::Typed);
        assert_eq!(lock(&state).submission, 2);
        host.write(b"\x1b[A", InputSource::Typed);
        host.write(b"\r", InputSource::Typed);
        assert_eq!(lock(&state).submission, 3);
        assert_eq!(lock(&state).title.as_deref(), Some("fix it"));
        drop(host);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn a_launch_prompt_counts_as_the_first_submission_and_names_the_task_once() {
        let path = std::env::temp_dir().join(format!(
            "shika-launch-prompt-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState::default()));
        let host = Host {
            core: core.clone(),
            state: state.clone(),
            capture: true,
        };
        let now = Instant::now();
        lock(&state).seed_launch_prompt("  Fix the   login bug\nsecond line", true, now);
        {
            let s = lock(&state);
            assert_eq!(s.submission, 1);
            assert_eq!(s.last_submission, Some(now));
            assert!(s.launch_turn);
            // The same name a typed first line would have produced.
            assert_eq!(s.title.as_deref(), Some("Fix the login bug"));
        }
        // The author typing a different first line later neither renames the
        // task nor is lost as a submission.
        lock(&state).title = None;
        host.write(b"something else\r", InputSource::Typed);
        assert!(lock(&state).title.is_none());
        assert_eq!(lock(&state).submission, 2);

        // The Lead keeps its own title.
        let mut lead = HostState::default();
        lead.seed_launch_prompt("You are the Lead", false, now);
        assert_eq!((lead.submission, lead.title.as_deref()), (1, None));

        // A switch prompt is a submission and must not become the task name.
        let mut switched = HostState::default();
        switched.seed_launch_prompt("Continue the backend.", false, now);
        assert_eq!(switched.submission, 1);
        assert!(switched.launch_turn);
        assert!(switched.title.is_none());
        switched.launch_turn = false;
        switched.capture_typed(b"rename the branch\r", now);
        assert!(switched.title.is_none());
        assert_eq!(switched.submission, 2);
        let mut activity = activity::Activity::new(now);
        activity.hold_for_launch();
        let started = activity.advance(activity::Signal::Unknown, Some(now), None, now, false);
        assert!(started.changed);
        assert_eq!(activity.status, Status::Working);
        drop(host);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn idle_leads_keep_their_title_without_losing_first_submissions() {
        let now = Instant::now();
        // Fresh Claude/Codex/Pi, and resumed Leads, start without a prompt.
        // Disable naming before startup typeahead can reach the host.
        for first in [
            "Please implement login",
            "[shika] Shika restarted. Workers are not running.",
        ] {
            let mut host = HostState::default();
            host.keep_title();
            assert_eq!(host.submission, 0);
            assert!(host.last_submission.is_none());
            assert!(!host.launch_turn);
            host.capture_typed(first.as_bytes(), now);
            assert!(host.has_draft());
            host.capture_typed(b"\r", now);
            assert!(host.title.is_none());
            assert_eq!(host.submission, 1);
            assert_eq!(host.last_submission, Some(now));
            assert!(!host.has_draft());
            let mut activity = activity::Activity::new(now);
            activity.advance(
                activity::Signal::Unknown,
                host.last_submission,
                None,
                now,
                false,
            );
            assert_eq!(activity.status, Status::Working);
            host.capture_typed(b"A second goal\r", now);
            assert!(host.title.is_none());
            assert_eq!(host.submission, 2);
        }
        // Cursor still counts its fresh bootstrap once, without naming.
        let mut cursor = HostState::default();
        cursor.keep_title();
        cursor.seed_launch_prompt("You are the Lead", false, now);
        cursor.capture_typed(b"Please implement login\r", now);
        assert!(cursor.title.is_none());
        assert_eq!(cursor.submission, 2);

        // Ordinary tasks retain first-prompt naming.
        let mut task = HostState::default();
        task.capture_typed(b"Please implement login\r", now);
        assert_eq!(task.title.as_deref(), Some("Please implement login"));
    }

    #[test]
    fn an_empty_switch_does_not_submit_and_only_a_draft_task_is_named() {
        let now = Instant::now();
        assert_eq!(switch_prompt(""), None);
        assert_eq!(switch_prompt("  \n"), None);
        assert_eq!(
            switch_prompt("Continue the backend."),
            Some("Continue the backend.")
        );

        let mut idle = HostState::default();
        let waiting = apply_switch_turn(&mut idle, None, false, now);
        assert_eq!(idle.submission, 0);
        assert!(!idle.launch_turn);
        assert!(idle.title.is_none());
        assert_eq!(waiting.status, Status::Waiting);
        idle.capture_typed(b"rename the branch\r", now);
        assert!(idle.title.is_none());
        assert_eq!(idle.submission, 1);

        let mut prompted = HostState::default();
        let working = apply_switch_turn(&mut prompted, Some("Continue the backend."), false, now);
        assert_eq!(prompted.submission, 1);
        assert!(!prompted.launch_turn);
        assert!(prompted.title.is_none());
        assert_eq!(working.status, Status::Working);
        prompted.capture_typed(b"rename the branch\r", now);
        assert!(prompted.title.is_none());
        assert_eq!(prompted.submission, 2);

        // A task still on its draft branch is named as after New.
        let mut draft = HostState::default();
        let waiting = apply_switch_turn(&mut draft, None, true, now);
        assert_eq!((draft.submission, waiting.status), (0, Status::Waiting));
        draft.capture_typed(b"Add the export button\r", now);
        assert_eq!(draft.title.as_deref(), Some("Add the export button"));
        assert_eq!(draft.submission, 1);

        let mut draft = HostState::default();
        let working = apply_switch_turn(&mut draft, Some("Add the export button"), true, now);
        assert_eq!((draft.submission, working.status), (1, Status::Working));
        assert_eq!(draft.title.as_deref(), Some("Add the export button"));
        draft.capture_typed(b"rename again\r", now);
        assert_eq!(draft.submission, 2);
    }

    /// The tick's consumption of a submission, in order, for a card whose CLI
    /// was started with a prompt instead of typed into.
    #[test]
    fn a_launch_prompt_turn_runs_the_same_steps_as_a_typed_one() {
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        let mut host = HostState::default();
        host.seed_launch_prompt("Add tests", true, t);
        let mut activity = activity::Activity::new(t);
        let mut lifecycle = lifecycle::Lifecycle::default();
        let mut submitted = 0;

        // First tick after the session exists: the submission is consumed,
        // the lifecycle is fenced, and the activity clock is armed.
        assert_ne!(host.submission, submitted);
        submitted = host.submission;
        lifecycle.submitted(host.lifecycle);
        if std::mem::take(&mut host.launch_turn) {
            activity.hold_for_launch();
        }
        let first = activity.advance(
            activity::Signal::Unknown,
            host.last_submission,
            None,
            at(100),
            false,
        );
        assert!(first.changed && !first.notify);
        assert_eq!(activity.status, Status::Working);
        assert_eq!(activity.turn_started, Some(t));
        assert_eq!(submitted, host.submission);

        // Pi: its startup Idle report is not an answer, a later Working
        // report confirms the turn, and a later Idle report finishes it.
        use shika_core::{AgentActivity, AgentActivityState::*};
        let report = |seq, state| Some(AgentActivity { seq, state });
        assert_eq!(lifecycle.observe(report(1, Idle)), None);
        assert_eq!(
            lifecycle.observe(report(2, Working)),
            Some(activity::Signal::Working)
        );
        activity.advance_authoritative(activity::Signal::Working, None, None, at(900), false);
        let idle = lifecycle.observe(report(3, Idle)).unwrap();
        activity.advance_authoritative(idle, None, None, at(5000), false);
        let done = activity.advance_authoritative(idle, None, None, at(5600), false);
        assert!(done.ready && done.notify);
        assert_eq!(activity.turn_started, Some(t));
    }

    #[test]
    fn delayed_lifecycle_read_cannot_cross_a_submission() {
        let mut host = HostState {
            submission: 2,
            lifecycle_checking: true,
            ..HostState::default()
        };
        host.note_lifecycle(
            1,
            Some(shika_core::AgentActivity {
                seq: 3,
                state: shika_core::AgentActivityState::Idle,
            }),
        );
        assert!(host.lifecycle.is_none());
        assert!(!host.lifecycle_checking);
        host.note_lifecycle(
            2,
            Some(shika_core::AgentActivity {
                seq: 4,
                state: shika_core::AgentActivityState::Working,
            }),
        );
        assert_eq!(
            host.lifecycle.unwrap().state,
            shika_core::AgentActivityState::Working
        );
    }

    #[test]
    fn live_terminal_interactions_and_active_enter_preserve_the_turn_timer() {
        let path = std::env::temp_dir().join(format!(
            "shika-activity-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState::default()));
        let terminal = Terminal::new(
            TerminalOptions {
                size: TerminalSize::new(12, 80),
                ..Default::default()
            },
            Host {
                core: core.clone(),
                state: state.clone(),
                capture: true,
            },
        );
        terminal.write(b"fix it\r");
        let start = lock(&state).last_submission.unwrap();
        let mut clock = activity::Activity::new(start);
        terminal.feed("\x1b[2J\x1b[H✻ Thinking… (12s · esc to interrupt)\r\n────\r\n❯ \r\n────\r\n? for shortcuts".as_bytes());
        let signal = activity::detect("claude", &terminal.live_text_lines(), None);
        assert_eq!(signal, activity::Signal::Working);
        clock.advance(signal, Some(start), None, start, false);
        terminal.feed(b"\x1b[?1004h");
        terminal.report(b"\x1b[I");
        terminal.report(b"\x1b[O");
        terminal.report(b"\x1b[<0;2;3M");
        terminal.scroll(2);
        terminal.resize(TerminalSize::new(14, 90));
        terminal.write(b"mistyped");
        assert_eq!(lock(&state).submission, 1);
        terminal.write(b"\r");
        assert_eq!(lock(&state).submission, 2);
        let signal = activity::detect("claude", &terminal.live_text_lines(), None);
        clock.advance(
            signal,
            lock(&state).last_submission,
            None,
            start + Duration::from_secs(30),
            false,
        );
        assert_eq!(clock.status, Status::Working);
        assert_eq!(clock.turn_started, Some(start));
        assert_eq!(
            model::short_time(
                (start + Duration::from_secs(30)).duration_since(clock.turn_started.unwrap())
            ),
            "30s"
        );
        drop(terminal);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn setup_input_is_not_queued_as_a_future_agent_prompt() {
        let path = std::env::temp_dir().join(format!(
            "shika-preparation-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState {
            preparing: true,
            ..HostState::default()
        }));
        let host = Host {
            core,
            state: state.clone(),
            capture: true,
        };
        host.write(b"installer answer\r", InputSource::Typed);
        host.write(b"\x1b[1;1R", InputSource::Reply);
        let state = lock(&state);
        assert!(state.pending_input.is_empty());
        assert!(state.title.is_none());
        assert_eq!(state.submission, 0);
        assert!(state.last_typed.is_none());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn prepared_agent_startup_preserves_query_replies_but_not_setup_input() {
        let path = std::env::temp_dir().join(format!(
            "shika-prepared-start-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState {
            preparing: true,
            agent_starting: true,
            ..HostState::default()
        }));
        let terminal = Terminal::new(
            TerminalOptions {
                size: TerminalSize::new(10, 40),
                ..Default::default()
            },
            Host {
                core: core.clone(),
                state: state.clone(),
                capture: true,
            },
        );
        terminal.feed(b"\x1b[6n");
        terminal.write(b"installer answer\r");
        let host = lock(&state);
        assert_eq!(host.pending_input.concat(), b"\x1b[1;1R");
        assert!(host.title.is_none());
        assert!(host.last_typed.is_none());
        assert_eq!(host.submission, 0);
        drop(host);
        drop(terminal);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn startup_query_replies_and_typeahead_survive_until_pty_binding() {
        let path = std::env::temp_dir().join(format!(
            "shika-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let core = Arc::new(Core::open(&path).unwrap());
        let state = Arc::new(Mutex::new(HostState::default()));
        let terminal = Terminal::new(
            TerminalOptions {
                size: TerminalSize::new(10, 40),
                ..Default::default()
            },
            Host {
                core: core.clone(),
                state: state.clone(),
                capture: true,
            },
        );
        // A CLI's cursor-position query may precede Core's create result.
        terminal.feed(b"\x1b[6n");
        terminal.write(b"fix resize\r");
        let host = lock(&state);
        assert_eq!(host.pending_input.concat(), b"\x1b[1;1Rfix resize\r");
        assert_eq!(host.title.as_deref(), Some("fix resize"));
        drop(host);
        drop(terminal);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
    }
}
