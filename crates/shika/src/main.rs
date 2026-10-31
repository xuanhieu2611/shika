mod activity;
mod agent_marks;
mod appearance;
mod background;
mod background_view;
mod base_branch;
mod card_menu;
mod changes;
mod chat_input;
mod checkout;
mod checks;
mod close;
mod column;
mod control;
mod control_client;
mod focus;
mod keys;
mod launch;
mod lead;
mod lead_chat;
mod lead_chat_markdown;
mod lead_chat_question;
mod lead_steps;
mod lifecycle;
mod model;
mod name_input;
mod notifications;
mod overlay;
mod project_motion;
mod projects;
mod publish;
mod recovery;
mod rename;
mod scrollbar;
mod selection;
mod settings;
mod shortcuts;
mod switch_agent;
mod tabs;
mod terminal_side;
mod text_edit;
mod theme;
mod tick;
mod ui;
mod update_notice;
mod updates;
mod usage_dashboard;
mod worktree_setup;

use base_branch::*;
use card_menu::*;
use close::*;
use column::*;
use focus::*;
use keys::*;
use launch::*;
use lead::*;
use overlay::*;
use projects::*;
use publish::*;
use rename::*;
use selection::*;
use settings::*;
use switch_agent::*;
use tabs::*;
use terminal_side::*;
use theme::*;
use tick::*;
use ui::*;
use update_notice::*;

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
        NewInCheckout,
        NewTerminal,
        NewLead,
        AddProject,
        BaseBranch,
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
    Recover {
        path: PathBuf,
        resume: bool,
    },
    /// An agent in the author's repository root, with no owned git state.
    Checkout,
    /// `shika new`: the CLI starts with the Lead's prompt and the card is
    /// not selected, because the author is typing elsewhere.
    Worker(LaunchOptions),
    /// The project's Lead, started by the author.
    Lead(LeadLaunch),
    /// The project's Lead, started again by the app after its resume was
    /// refused. Not an author launch: it closes no overlay, takes no busy
    /// hold, and the selection and focus only follow the old Lead's.
    /// `focus`: the old Lead's terminal had focus.
    LeadRestart {
        launch: LeadLaunch,
        focus: bool,
    },
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
/// The column group a card was shown in, by project id: the Leads section
/// (a Lead and its workers) or the project's own cards.
#[derive(Clone, PartialEq, Eq)]
enum Home {
    Leads(String),
    Project(String),
}
impl Card {
    fn is_checkout(&self) -> bool {
        matches!(self.launch, Launch::Checkout)
    }
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
        /// Empty leaves the PR body to `gh --fill`.
        body: Entity<chat_input::ChatInput>,
        target: String,
        /// 0 is the title, 1 the description, 2 the target.
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
    Picker {
        project: String,
        index: usize,
        mode: PickerMode,
    },
    /// Stops PTYs only. Never offers Discard or reads the checkout's git.
    CheckoutClose {
        index: usize,
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
    Recover {
        entry: JournalEntry,
        index: usize,
    },
    Quit {
        previous: Option<Box<Overlay>>,
        focus: Option<FocusHandle>,
    },
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
    /// Appearance previews and pointer menus leave the underlying app unaltered.
    fn dims_background(&self) -> bool {
        !matches!(
            self,
            Self::Settings { .. } | Self::Adjust(_) | Self::CardMenu(_) | Self::RestingMenu { .. }
        )
    }

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
            Self::Close { .. } | Self::SwitchedClose { .. } | Self::CheckoutClose { .. } => {
                "the Close task dialog"
            }
            Self::RestingMenu { .. } => "a card menu",
            Self::LeadClose { .. } => "the Lead's Stop dialog",
            Self::Preparation { .. } => "the setup approval dialog",
            Self::Leftovers | Self::RemoveLeftover(_) | Self::Recover { .. } => {
                "the leftovers list"
            }
            Self::Quit { .. } => "the Quit dialog",
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
    checkout_heads: HashMap<String, checkout::Head>,
    checkout_selection_epoch: u64,
    snippets_requested: std::collections::HashSet<String>,
    chat_questions: Vec<(String, String, shika_core::ChatQuestion)>,
    /// When the confirmation dialog on screen appeared, to ignore keys that
    /// were already in flight.
    dialog_grace: model::DialogGrace,
    selection: model::Tracked<Option<Selection>>,
    focus: FocusHandle,
    catalog: Option<CliCatalog>,
    overlay: Option<Overlay>,
    /// Non-workflow overlays return to the surface that opened them.
    overlay_return_focus: Option<FocusHandle>,
    /// A project header clicked while a terminal stayed on screen: where
    /// New, Lead, and Base branch aim until the selection next changes. The
    /// selection epoch it was recorded under makes that check, so any change
    /// ends it (`model::Tracked`, `model::header_target`).
    project_target: Option<(String, u64)>,
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
            checkout_heads: HashMap::new(),
            checkout_selection_epoch: u64::MAX,
            snippets_requested: Default::default(),
            chat_questions: Vec::new(),
            dialog_grace: model::DialogGrace::default(),
            selection: model::Tracked::new(selection),
            focus: cx.focus_handle(),
            catalog: None,
            overlay: if leftovers.is_empty() {
                None
            } else {
                Some(Overlay::Leftovers)
            },
            overlay_return_focus: None,
            project_target: None,
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
}
impl Shika {
}
impl Focusable for Shika {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
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
        let reveal_selection = *self.selection != self.last_revealed_selection;
        self.last_revealed_selection = self.selection.clone();
        // The Changes panel follows the selected card. Closed, this is a no-op.
        self.sync_changes(cx);
        self.sync_checkout_selection(cx);
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
            .key_context("Shika")
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.request_quit(window, cx)))
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.picker(window, cx)))
            .on_action(cx.listener(|this, _: &NewLead, window, cx| this.new_lead(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewInCheckout, window, cx| this.new_in_checkout(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AddProject, window, cx| {
                if !this.busy && this.overlay.is_none() {
                    this.add_project(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &BaseBranch, window, cx| {
                if let Some(project) = this.project_id() {
                    this.open_base(project, window, cx);
                }
            }))
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
            // The cards' focus handle sits on an element with no size, so no
            // mouse-down can land on it: GPUI focuses a tracked element on any
            // click it did not prevent, which sent a stray click in the
            // column, the title bar, or the footer's gaps to the card list and
            // stopped the author's typing from reaching the terminal. It is
            // still inside the root, so key context, actions, and the capture
            // handler apply when the cards have focus.
            .child(div().absolute().size_0().track_focus(&self.focus))
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
/// Bind the terminal, the fixed app shortcuts, and the user's overrides, then
/// refresh the menus so they show the chords that are actually bound.
fn install_keys(cx: &mut App, keys: &KeyOverrides) {
    cx.clear_key_bindings();
    shika_terminal::init(cx);
    let mut bindings = vec![
        gpui::KeyBinding::new("cmd-n", NewAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-l", NewLead, Some("Shika")),
        gpui::KeyBinding::new("cmd-o", AddProject, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-b", BaseBranch, Some("Shika")),
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
        } else if binding.id == "newInCheckout" {
            bindings.push(gpui::KeyBinding::new(chord, NewInCheckout, Some("Shika")));
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
            gpui::MenuItem::action("Add project…", AddProject),
            gpui::MenuItem::action("New agent", NewAgent),
            gpui::MenuItem::action("New Agent in Checkout", NewInCheckout),
            gpui::MenuItem::action("New Lead…", NewLead),
            gpui::MenuItem::action("New terminal tab", NewTerminal),
            gpui::MenuItem::action("Create PR", CreatePr),
            gpui::MenuItem::action("Switch agent…", SwitchAgent),
            gpui::MenuItem::action("Base branch…", BaseBranch),
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
        cx.on_action(move |_: &Quit, cx| {
            // A native menu action may arrive while GPUI has the window out
            // of its store for dispatch. Reenter it only after dispatch ends.
            cx.defer(move |cx| {
                let _ = handle.update(cx, |app, window, cx| app.request_quit(window, cx));
            });
        });
        let _ = handle.update(cx, |_, window, cx| {
            let app = cx.entity().downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                let _ = app.update(cx, |app, cx| app.request_quit(window, cx));
                false
            });
        });
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
    fn terminal_is_a_task_choice_even_without_agents_but_never_a_lead_or_checkout() {
        let terminal = shika_core::terminal_choice();
        assert_eq!(PickerMode::Task.choices(vec![]), vec![terminal.clone()]);
        assert!(PickerMode::Lead.choices(vec![]).is_empty());
        assert!(PickerMode::Checkout.choices(vec![]).is_empty());
        let agent = CliPreset {
            id: "claude".into(),
            name: "Claude Code".into(),
            binary: "claude".into(),
            path: Some("/test/claude".into()),
            args: vec![],
        };
        assert_eq!(
            PickerMode::Task.choices(vec![agent.clone()]),
            vec![agent.clone(), terminal]
        );
        assert_eq!(
            PickerMode::Lead.choices(vec![agent.clone()]),
            vec![agent.clone()]
        );
        assert_eq!(
            PickerMode::Checkout.choices(vec![agent.clone()]),
            vec![agent]
        );
    }

    #[test]
    fn publish_change_label_names_an_empty_commit_and_pluralizes() {
        assert_eq!(publish_change_label(0), "Nothing new to commit");
        assert_eq!(publish_change_label(1), "1 changed file");
        assert_eq!(publish_change_label(2), "2 changed files");
    }

    #[test]
    fn modal_scrim_preserves_appearance_previews_and_pointer_menus() {
        for section in SettingsSection::ALL {
            assert!(
                !Overlay::Settings {
                    section,
                    row: 0,
                    edit: None,
                    usage: None,
                }
                .dims_background()
            );
        }
        assert!(
            !Overlay::RestingMenu {
                project: "demo".into(),
                position: gpui::point(px(10.), px(10.)),
            }
            .dims_background()
        );
        assert!(
            Overlay::Picker {
                project: "demo".into(),
                index: 0,
                mode: PickerMode::Task,
            }
            .dims_background()
        );
        assert!(Overlay::CheckoutClose { index: 0 }.dims_background());
        assert!(
            Overlay::Quit {
                previous: None,
                focus: None,
            }
            .dims_background()
        );
    }

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
        // Both a pinned Terminal task and additional shells use capture=false.
        let shell_state = Arc::new(Mutex::new(HostState::default()));
        let shell = Host {
            core: core.clone(),
            state: shell_state.clone(),
            capture: false,
        };
        shell.write(b"echo Working...\r", InputSource::Typed);
        shell.write(b"\x1b[A\r", InputSource::Typed);
        shell.write(b"\x1b[200~claude\n\x1b[201~\r", InputSource::Typed);
        assert_eq!(lock(&shell_state).submission, 0);
        assert!(lock(&shell_state).title.is_none());
        assert!(lock(&shell_state).last_typed.is_some());
        drop(shell);
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
