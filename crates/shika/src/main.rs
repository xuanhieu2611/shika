mod activity;
mod appearance;
mod changes;
mod checks;
mod control;
mod control_client;
mod lifecycle;
mod model;
mod name_input;
mod notifications;
mod updates;

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
    JournalEntry, KnownBranches, LaunchOptions, LeadEnv, PreparationConfig, PreparationControl,
    PreparationEvent, Project, ProjectBase, PtyEvent, PtyId, PtySize, PublishPreview, Session,
    SessionGitState, Settings, ThemeMode, ThemeSettings, Translucency,
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
        CloseTerminal,
        CloseTask,
        RenameTask,
        NextTerminal,
        PreviousTerminal,
        NextAgent,
        PreviousAgent,
        ToggleColumn,
        ToggleChanges
    ]
);

/// The drag on the agent column's edge. It draws nothing: the column itself
/// follows the pointer.
struct ColumnDrag;
impl Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Jump to a task-local tab. Zero is the pinned agent, so Cmd+1 selects it.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = shika, no_json)]
struct SelectTerminal(usize);

/// Card traversal follows row order and skips project headers.
/// `j` / `k` and Cmd+] / Cmd+[ both use it.
fn adjacent_agent(
    rows: &[Selection],
    selection: Option<&Selection>,
    delta: isize,
) -> Option<usize> {
    let at = selection.and_then(|selected| rows.iter().position(|row| row == selected));
    for step in 1..=rows.len() {
        let index = match at {
            Some(at) => {
                (at as isize + delta * step as isize).rem_euclid(rows.len() as isize) as usize
            }
            None if delta > 0 => step - 1,
            None => rows.len() - step,
        };
        if let Selection::Card(i) = rows[index] {
            return Some(i);
        }
    }
    None
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
    Lead(LeadEnv),
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
    agent: Pane,
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
    /// The checks on the PR that Create PR made or reused. Memory only.
    pr: Option<checks::PrWatch>,
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
}
/// A closed card on its way out. It is no longer in `cards`, so nothing can
/// select, count, or act on it; it only paints until its exit has played.
struct Departing {
    card: Card,
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
    Agents,
}

impl SettingsSection {
    const ALL: [SettingsSection; 2] = [SettingsSection::Appearance, SettingsSection::Agents];

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
            Self::Agents => "Agents",
        }
    }
}

enum Overlay {
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
    /// `section` is Appearance or Agents. `row` is the selected row in that
    /// section: an appearance `*_ROW`, or a preset index. `edit` holds digits
    /// typed into the selected number, or the prefix being typed, not yet applied.
    Settings {
        section: SettingsSection,
        row: usize,
        edit: Option<String>,
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
            Self::CardMenu(_) => "a card menu",
            Self::Rename { .. } => "the Rename task dialog",
            Self::Publish { .. } => "the Create PR dialog",
            Self::Picker { .. } => "the New picker",
            Self::Close { .. } | Self::SwitchedClose { .. } => "the Close task dialog",
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
    selection: Option<Selection>,
    focus: FocusHandle,
    catalog: Option<CliCatalog>,
    overlay: Option<Overlay>,
    /// Non-workflow overlays return to the surface that opened them.
    overlay_return_focus: Option<FocusHandle>,
    sidebar_scroll: gpui::ScrollHandle,
    /// The Base branch list, so Up and Down can bring the highlight into view.
    base_scroll: gpui::ScrollHandle,
    /// The Settings row list, so `j` / `k` keep the selected row on screen
    /// when a short window makes that list scroll.
    settings_scroll: gpui::ScrollHandle,
    last_revealed_selection: Option<Selection>,
    busy: bool,
    toast: Option<(String, Instant)>,
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
    /// The Create PR or Close dialog a Lead's `shika pr` or `shika close` is
    /// waiting on. See `control::LeadDialog`.
    lead_dialog: Option<control::LeadDialog>,
    control_error: Option<String>,
    /// Closed cards playing their exit. See [`Departing`].
    departing: Vec<Departing>,
    departing_serial: u64,
}
impl Shika {
    fn new(
        core: Arc<Core>,
        settings: shika_core::Result<Settings>,
        diagnostics: Option<PathBuf>,
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
        let column = settings.column;
        let changes = changes::Panel::new(settings.changes, cx.focus_handle(), diagnostics.clone());
        let (control, control_error) = match control::Server::start() {
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
            selection,
            focus: cx.focus_handle(),
            catalog: None,
            overlay: if leftovers.is_empty() {
                None
            } else {
                Some(Overlay::Leftovers)
            },
            overlay_return_focus: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            base_scroll: gpui::ScrollHandle::new(),
            settings_scroll: gpui::ScrollHandle::new(),
            last_revealed_selection: None,
            busy: false,
            toast: if load_errors.is_empty() {
                None
            } else {
                Some((load_errors.join("; "), Instant::now()))
            },
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
            column,
            column_drag_from: None,
            bases: HashMap::new(),
            changes,
            control,
            lead_dialog: None,
            control_error,
            departing: Vec::new(),
            departing_serial: 0,
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
        this
    }
    fn message(&mut self, text: String) {
        self.toast = Some((text, Instant::now()));
    }
    fn selected_card(&self) -> Option<usize> {
        match self.selection {
            Some(Selection::Card(i)) if i < self.cards.len() => Some(i),
            _ => None,
        }
    }
    fn project_id(&self) -> Option<String> {
        match &self.selection {
            Some(Selection::Project(p)) => Some(p.clone()),
            Some(Selection::Card(i)) => self.cards.get(*i).map(|c| c.project.clone()),
            _ => self.projects.first().map(|p| p.id.clone()),
        }
    }
    fn rows(&self) -> Vec<Selection> {
        let mut rows = vec![];
        for p in &self.projects {
            rows.push(Selection::Project(p.id.clone()));
            for i in self.sorted_cards(&p.id) {
                rows.push(Selection::Card(i));
            }
        }
        rows
    }
    fn sorted_cards(&self, project: &str) -> Vec<usize> {
        let mut indices = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, c)| c.project == project)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        // The Lead leads its group; the rest sort by attention.
        indices.sort_by_key(|i| (self.cards[*i].lead.is_none(), self.cards[*i].status.rank()));
        indices
    }
    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        // Headers, including a project with no cards, are not stops.
        let Some(i) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) else {
            return;
        };
        self.selection = Some(Selection::Card(i));
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn move_agent(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        // From the Changes panel, focus stays in the panel, which follows.
        let panel_focused = self.changes.open && self.changes.focus.is_focused(window);
        let terminal_focused = !self.focus.is_focused(window) && !panel_focused;
        if let Some(i) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) {
            self.selection = Some(Selection::Card(i));
            if terminal_focused {
                self.focus_terminal(window, cx);
            }
            cx.notify();
        }
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
        if cards || changes || selected_terminal.as_ref() == Some(&focus) {
            window.focus(&focus, cx);
        } else if self.cards.iter().any(|card| {
            std::iter::once(&card.agent)
                .chain(card.shells.iter())
                .any(|pane| pane.view.focus_handle(cx) == focus)
        }) && !self.busy
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

    /// The picker's choice in Lead mode: the project's one Lead.
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
        if self.show_lead(&project, window, cx) {
            self.overlay = None;
            self.overlay_return_focus = None;
            return;
        }
        let Some(server) = &self.control else {
            let why = self.control_error.clone().unwrap_or_default();
            self.message(format!("The Lead needs Shika's control socket: {why}"));
            cx.notify();
            return;
        };
        let name = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let env = LeadEnv {
            socket: server.socket(),
            token: shika_core::control::new_token(),
            bin_dir: server.bin_dir(),
            prompt: format!(
                "You are the Lead agent for the \"{name}\" project in Shika. Run `shika help` and follow it, then ask the author what they want done."
            ),
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

    /// Cmd+L and Agent > New Lead: the selected project's Lead, started or,
    /// when it exists, shown with its terminal focused.
    fn new_lead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(project) = self.project_id() else {
            self.message("Add a project first.".into());
            cx.notify();
            return;
        };
        if self.show_lead(&project, window, cx) {
            return;
        }
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

    /// Selects the project's Lead and focuses its terminal. False when it
    /// has none.
    fn show_lead(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.lead_card(project) else {
            return false;
        };
        self.selection = Some(Selection::Card(index));
        self.focus_terminal(window, cx);
        cx.notify();
        true
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
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let config = core.project_preparation(&lookup)?;
                    let approved = match &config {
                        Some(config) => core.preparation_approved(&lookup, config)?,
                        None => true,
                    };
                    Ok::<_, shika_core::Error>((config, approved))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok((Some(config), false)) => {
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
                    Ok((config, _)) => this.begin_launch(
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
        let before = self
            .sorted_cards(&project)
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
        self.departing.push(Departing { card, before, key });
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
        reply: Option<std::sync::mpsc::Sender<shika_core::control::Reply>>,
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
        let author = retried || !matches!(launch, Launch::Worker(_));
        let worker_token = match &launch {
            Launch::Worker(options) => options.control.as_ref().map(|c| c.token.clone()),
            _ => None,
        };
        let (started_by, lead) = match &launch {
            Launch::Worker(options) => (options.started_by.clone(), None),
            Launch::Lead(env) => (None, Some(control::LeadState::new(env.token.clone()))),
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
        lock(&state).preparing = configured;
        if configured || !author {
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
        });
        if author {
            self.selection = Some(Selection::Card(index));
            window.focus(&self.focus, cx);
        }
        cx.notify();
        let core = self.core.clone();
        let finished = launch.clone();
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
            let sink_terminal = terminal.clone();
            let state_bind = state.clone();
            let progress_state = state.clone();
            let progress_terminal = terminal.clone();
            let callback_core = core.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let size = PtySize::new(measured.rows, measured.cols);
                    let sink = move |_, event| match event {
                        PtyEvent::Output(bytes) => {
                            sink_terminal.feed(&bytes);
                            lock(&output_state).note_output(Instant::now());
                        }
                        PtyEvent::Exit(exit) => {
                            sink_terminal.feed(
                                format!("\r\n[process exited with code {}]\r\n", exit.code)
                                    .as_bytes(),
                            );
                            let mut state = lock(&output_state);
                            state.last_output = Some(Instant::now());
                            state.exited = true;
                        }
                    };
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
                        Launch::Lead(env) => {
                            core.create_lead(&project, &preset.id, size, sink, env)
                        }
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
                    if let Ok(s) = &result {
                        bind_host(&core, &state_bind, s.pty);
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
                                Launch::Lead(env) => {
                                    host.seed_launch_prompt(&env.prompt, false, now)
                                }
                                _ => {}
                            }
                        }
                        this.cards[index].session = Some(session);
                        this.cards[index].launch_control = None;
                        // Completion never steals focus from another card or
                        // an overlay opened while installation was running.
                        if this.selection == Some(Selection::Card(index)) && this.overlay.is_none()
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
        if tab > self.cards[index].shells.len() {
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
        let mut changed = false;
        // The Lead's shika commands run here, with the rest of the state.
        self.drain_control(window, cx);
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
        if now.duration_since(self.clock) >= Duration::from_secs(1) {
            self.clock = now;
            changed = self.cards.iter().any(|c| c.status == Status::Working);
            let reduce = appearance::reduce_transparency();
            if reduce != self.reduce_transparency {
                self.reduce_transparency = reduce;
                appearance::apply(&self.appearance, window);
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
            // Sample live terminal chrome, never the user's scrollback. Read
            // the engine before taking HostState so query replies cannot
            // create an inverted lock order.
            let lines = if !card.creating && card.session.is_some() {
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
            if card.session.is_none() {
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
                card.title_watch.start(now);
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
                && (stayed || card.active_pane().view.focus_handle(cx).is_focused(window))
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
        let session_id = id.clone();
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
                        this.lead_dialog_failed(&session_id, e.to_string());
                        this.message(e.to_string());
                    }
                }
                this.settle_lead_dialog(window, cx);
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
                        this.lead_dialog_succeeded(&session_id, published.url.clone());
                        if let Some(card) = this.cards.iter_mut().find(|c| {
                            c.session.as_ref().is_some_and(|s| s.id == session_id)
                        }) {
                            card.pr = checks::PrWatch::new(&published, Instant::now());
                        }
                        this.message(format!("Published PR: {}", published.url));
                        cx.open_url(&published.url);
                    }
                    Err(e) => {
                        this.lead_dialog_failed(&session_id, control::publish_failure(&e.to_string()));
                        if let Some(Overlay::Publish { error, .. }) = &mut this.overlay {
                            *error = Some(format!("{e} Completed commits and pushes were kept. Cancel and reopen Create PR to retry."));
                        }
                    }
                }
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        }).detach();
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
            action: if card.session.is_some() {
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
        let Some(session) = &card.session else {
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
        if self.busy && matches!(self.overlay, Some(Overlay::Publish { .. })) {
            return;
        }
        self.commit_setting_edit(window, cx);
        let shell = match &self.overlay {
            // The Lead has no shell to commit in.
            // A Lead that asked for the dialog gets selection and focus back.
            Some(Overlay::Close { index, state })
                if (state.dirty || state.unpushed)
                    && self.cards[*index].lead.is_none()
                    && !self.lead_dialog_is_for(*index) =>
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
        });
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
        let Some(Overlay::Settings { section, row, edit }) = &mut self.overlay else {
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
        appearance::apply(&next, window);
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
            (!self.column.hidden).then_some(f32::from(self.column.width)),
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
        let room = window.viewport_size().width - px(MIN_TERMINAL_WIDTH) - panel;
        let next = self.column.with_width(f32::from(x.min(room)));
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
    /// Give every live terminal view the current theme's palette and alpha.
    fn push_terminal_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
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
        };
        if let Err(e) = self.core.save_settings(&settings) {
            self.message(e.to_string());
        }
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.modifiers.platform && stroke.key == "q" {
            cx.quit();
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
                            .is_some_and(|index| self.cards[index].session.is_some())
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
        if stroke.modifiers.platform
            || stroke.modifiers.control
            || stroke.modifiers.alt
            || stroke.modifiers.shift
        {
            return;
        }
        let key = stroke.key.as_str();
        if self.overlay.is_some() {
            if self.busy {
                return;
            }
            let offered_len = self.offered_presets().len();
            let catalog_ready = self.catalog.is_some();
            let settings_nav = match &self.overlay {
                Some(Overlay::Settings { section, row, edit }) => {
                    Some((*section, *row, edit.is_some()))
                }
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
                Some(Overlay::Preparation { .. }) => match key {
                    "enter" => self.approve_preparation(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    _ => {}
                },
                Some(Overlay::Close { index, state }) => {
                    let i = *index;
                    let can_push = state.can_push();
                    match key {
                        "d" => self.finish_close(i, 1, window, cx),
                        "p" if can_push => self.finish_close(i, 2, window, cx),
                        "escape" => self.cancel_overlay(window, cx),
                        _ => {}
                    }
                }
                Some(Overlay::SwitchedClose { index, .. }) => {
                    let i = *index;
                    match key {
                        "enter" => self.finish_close(i, 3, window, cx),
                        "escape" => self.cancel_overlay(window, cx),
                        _ => {}
                    }
                }
                Some(Overlay::RemoveLeftover(i)) => {
                    let i = *i;
                    match key {
                        "d" => self.remove_leftover(i, cx),
                        "escape" => self.overlay = Some(Overlay::Leftovers),
                        _ => {}
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
                Some(Overlay::RemoveProject(id)) => {
                    let id = id.clone();
                    match key {
                        "r" | "enter" => self.remove_project(id, cx),
                        "escape" => self.cancel_overlay(window, cx),
                        _ => {}
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
                            self.select_setting((at + 1) % SETTING_ROWS, window, cx)
                        }
                        (_, "k" | "up") => {
                            self.select_setting((at + SETTING_ROWS - 1) % SETTING_ROWS, window, cx)
                        }
                        _ => {}
                    }
                }
                // Typed into above, before the modifier check.
                Some(Overlay::Base { .. })
                | Some(Overlay::Publish { .. })
                | Some(Overlay::CardMenu(_))
                | Some(Overlay::Rename { .. })
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
            "enter" => self.focus_terminal(window, cx),
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
/// opacity, blur, translucency, font size, the branch prefix, then
/// notification sound.
const MODE_ROW: usize = 0;
const LIGHT_ROW: usize = 1;
const DARK_ROW: usize = 2;
const OPACITY_ROW: usize = 3;
const BLUR_ROW: usize = 4;
const APPLY_ROW: usize = 5;
const FONT_ROW: usize = 6;
const PREFIX_ROW: usize = 7;
const SOUND_ROW: usize = 8;
const SETTING_ROWS: usize = 9;
/// Wide enough for the longest catalog name, such as "Catppuccin Macchiato".
const THEME_FIELD_WIDTH: f32 = 168.;
/// Settings is wide enough for a section list and the appearance controls.
const SETTINGS_WIDTH: f32 = 640.;
/// Tall enough for the appearance rows. A short window uses less.
const SETTINGS_HEIGHT: f32 = 520.;
const SETTINGS_NAV_WIDTH: f32 = 148.;
impl Shika {
    /// Move the Settings selection, applying any typed value first, and keep
    /// the row on screen. The rows are the children of the scrolling list.
    fn select_setting(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_setting_edit(window, cx);
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
        self.commit_setting_edit(window, cx);
        if let Some(Overlay::Settings {
            section: at,
            row,
            edit,
        }) = &mut self.overlay
            && *at != section
        {
            *at = section;
            *row = 0;
            *edit = None;
            self.settings_scroll.scroll_to_item(0);
        }
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
                    this.column = this.column.with_width(f32::from(Column::DEFAULT_WIDTH));
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
    /// wordmark, the column toggle, the settings gear, and New agent. The
    /// system title is hidden.
    fn top_row(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inset = Self::title_inset(window);
        let hover = chrome.hover;
        let gear_hover = chrome.ink_1;
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let row = div()
            .id("titlebar")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(inset)
            .pr(px(14.))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .child("Shika"),
            )
            .child(div().flex_1())
            .child(self.column_toggle(chrome.ink_3, chrome.ink_1, hover, chrome, cx))
            .child(
                div()
                    .id("settings")
                    .group("settings")
                    .occlude()
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
                            text: "Settings  \u{2318},".into(),
                        })
                        .into()
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                    .child(
                        gpui::svg()
                            .data(SETTINGS_ICON)
                            .size(px(15.))
                            .text_color(chrome.ink_3)
                            .group_hover("settings", move |style| style.text_color(gear_hover)),
                    ),
            )
            .child(
                secondary_button("new", chrome)
                    .occlude()
                    .gap(px(8.))
                    .pl(px(11.))
                    .pr(px(6.))
                    .py(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .child("New agent")
                    .child(kbd("\u{2318}N", chrome.sunken, chrome.ink_3))
                    .tooltip(move |_, cx| {
                        cx.new(|_| KeyTip {
                            bg: tip_bg,
                            fg: tip_fg,
                            text: "New agent  ⌘N".into(),
                        })
                        .into()
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.picker(window, cx))),
            );
        self.title_drag(row, cx)
    }

    /// One project: its header, then every card.
    fn project_group(
        &self,
        project: &Project,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = project.id.clone();
        let selected = self.selection == Some(Selection::Project(id.clone()));
        let indices = self.sorted_cards(&id);
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
                    .group_hover(group, |style| style.opacity(1.))
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
        let departing = self
            .departing
            .iter()
            .filter(|d| d.card.project == id)
            .collect::<Vec<_>>();
        let mut column = div().flex().flex_col().gap(px(CARD_GAP)).child(header);
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
            let face = self.card_view(card, Some(at), chrome, cards_focused, reveal_selection, cx);
            column = column.child(if card.closing {
                face.with_animation(
                    SharedString::from(format!("card-closing-{view}")),
                    gpui::Animation::new(model::CLOSING_FADE).with_easing(model::ease),
                    |d, t| d.opacity(1. - (1. - CLOSING_CARD_OPACITY) * t),
                )
                .into_any_element()
            } else {
                face.into_any_element()
            });
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
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(div().child(face).with_animation(
                SharedString::from(format!("{key}-shrink")),
                exit(),
                move |d, t| {
                    let (shrink, _) = model::card_exit(t);
                    let scale = 1. - (1. - EXIT_SCALE) * shrink;
                    d.w(gpui::relative(scale))
                        .h(gpui::relative(scale))
                        .opacity(from * (1. - shrink))
                },
            ))
            .with_animation(
                SharedString::from(format!("{key}-collapse")),
                exit(),
                |d, t| {
                    let (_, collapse) = model::card_exit(t);
                    d.h(px(CARD_HEIGHT * (1. - collapse)))
                        .mb(px(-CARD_GAP * collapse))
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
        let key: SharedString = match at {
            Some(i) => i.to_string().into(),
            None => format!("departing-{}", card.agent.view.entity_id()).into(),
        };
        let colors = chrome.status(card.status);
        let separator = || div().flex_none().text_color(chrome.ink_5).child("·");
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
                    .child(card.title.clone()),
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
                div().flex_none().w(px(13.)).into_any_element()
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
            .text_color(chrome.ink_4)
            .child(div().flex_none().child(card.preset.clone()))
            // Where the task came from, quiet: not a status.
            .when(card.started_by.is_some(), |d| {
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .text_color(chrome.ink_4)
                        .child("from Lead"),
                )
            })
            .when(card.lead.is_some(), |d| {
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(chrome.ink_2)
                        .child("Project lead"),
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
                card.session.as_ref().filter(|_| card.lead.is_none()),
                |d, session| {
                    // The branch truncates before the diff stat.
                    d.child(separator()).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .child(session.branch.clone()),
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
            .child(div().flex_1())
            .when(selected && cards_focused, |d| {
                d.children(hints.iter().map(|(key, label)| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .ml(px(6.))
                        .text_size(px(11.))
                        .text_color(chrome.ink_3)
                        .child(kbd(*key, chrome.sunken, chrome.ink_2).py_0())
                        .child(*label)
                }))
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
        base.child(task)
            .child(meta)
            .when(selected && reveal_selection, |d| {
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
                    let delta = model::reveal_delta(
                        bounds.top().into(),
                        bounds.bottom().into(),
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

    /// Add project, leftovers, and the key hints. A column too narrow for
    /// the hints drops them; the keys still work.
    fn footer(&self, chrome: &Chrome, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let hints_fit = width
            >= px(if self.leftovers.is_empty() {
                FOOTER_HINTS_FIT
            } else {
                FOOTER_HINTS_FIT_WITH_LEFTOVERS
            });
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
                        text_button("add")
                            .flex_none()
                            .child("Add project…")
                            .child(kbd("a", chrome.sunken, chrome.ink_3))
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
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
        // With the column hidden, this row is the whole title bar: it starts
        // after the traffic lights with the button that shows the column.
        let show_column = self.column.hidden.then(|| {
            div()
                .flex_none()
                .h(px(BAR_HEIGHT))
                .flex()
                .items_center()
                .pl(Self::title_inset(window))
                .pr(px(4.))
                .child(self.column_toggle(
                    chrome.term_dim,
                    chrome.term_white,
                    chrome.term_hover,
                    chrome,
                    cx,
                ))
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
            let path = card
                .session
                .as_ref()
                .map(|s| model::tilde(&s.worktree, home))
                .unwrap_or_else(|| card.stage.clone());
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
                                div().h(px(TAB_HEIGHT)).flex().items_center().child(
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
                            div().h(px(TAB_HEIGHT)).flex().items_center().child(
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
                                    .h(px(TAB_HEIGHT))
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
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .font_family(MONO)
                                .child(path),
                        )
                        .when(
                            card.session.is_some() && !card.creating && card.lead.is_none(),
                            |d| {
                                d.child(
                                    div()
                                        .id("create-pr")
                                        .occlude()
                                        .flex_none()
                                        .rounded(px(6.))
                                        .px(px(8.))
                                        .py(px(4.))
                                        .cursor_pointer()
                                        .hover(move |style| {
                                            style.bg(term_hover).text_color(term_white)
                                        })
                                        .child("Create PR")
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| KeyTip {
                                                bg: tip_bg,
                                                fg: tip_fg,
                                                text: "Commit, push, create PR  ⌘⇧P".into(),
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
            right = right.child(
                div()
                    .relative()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
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
                                            .flex_col()
                                            .justify_end()
                                            .pr(px(12.))
                                            .child(
                                                div()
                                                    .h(px(TAB_HEIGHT))
                                                    .flex()
                                                    .items_center()
                                                    .child(self.changes_toggle(chrome, cx)),
                                            ),
                                    )
                                }),
                            cx,
                        ),
                    )
                    .child(gpui::img(icon).size(px(64.)).mb(px(6.)))
                    .child("No agent selected")
                    .child(
                        div()
                            .flex()
                            .gap(px(14.))
                            .text_size(px(11.5))
                            .text_color(chrome.term_fainter)
                            .child(key("j", "select"))
                            .child(key("\u{2318}N", "new agent")),
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
        right
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
            .is_some_and(|index| self.cards[index].session.is_some());
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

    fn settings_keys(
        &self,
        section: SettingsSection,
        row: usize,
        editing: bool,
    ) -> &'static [(&'static str, &'static str)] {
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
            .min_w_0()
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
                    .child(rows),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .px(px(10.))
                    .children(keys.iter().map(|(key, label)| hint(key, label, chrome))),
            )
            .child(dialog_buttons().child(self.cancel_button("Done", chrome, cx)))
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
        let height = window.viewport_size().height;
        let picker = matches!(overlay, Overlay::Picker { .. });
        let top = height * if picker { 0.18 } else { 0.20 };
        let max_h = (height - top - px(24.)).max(px(120.));
        let panel = match overlay {
            Overlay::CardMenu(_) => unreachable!("card menus render at the pointer"),
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
                let lead = self.cards[i].lead.is_some();
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Close “{}”?", self.cards[i].title)));
                if state.agent_working {
                    panel = panel.child(dialog_text(chrome).child("The agent is still working."));
                }
                if state.dirty && lead {
                    panel = panel.child(dialog_text(chrome).child(
                        "The Lead edited files in its worktree. It should never do that; its tasks belong to workers.",
                    ));
                } else if state.dirty {
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
                panel = panel.child(dialog_text(chrome).child(if lead {
                    "Discard stops the Lead and deletes its worktree. Its workers stay as ordinary cards."
                } else {
                    "Discard stops the session and deletes the worktree and local branch."
                }));
                let discard =
                    cx.listener(move |this, _, window, cx| this.finish_close(i, 1, window, cx));
                let buttons = dialog_buttons().child(self.cancel_button("Cancel", chrome, cx));
                // The primary action is Push when it is offered.
                let buttons = if can_push {
                    buttons
                        .child(
                            dialog_button("discard", "Discard changes", "d", chrome)
                                .on_click(discard),
                        )
                        .child(
                            primary_button("push", "Push changes", "p", chrome).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.finish_close(i, 2, window, cx)
                                }),
                            ),
                        )
                } else {
                    buttons.child(
                        primary_button("discard", "Discard changes", "d", chrome).on_click(discard),
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
                            primary_button("close-switched", "Close task", "↵", chrome).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.finish_close(i, 3, window, cx)
                                }),
                            ),
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
                            primary_button("remove-project", "Remove project", "r", chrome)
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
                                primary_button("remove-confirm", "Discard worktree", "d", chrome)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_leftover(i, cx)
                                    })),
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
/// The 960px window minimum leaves room for the default 540px column.
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
/// A closed card shrinks toward its center to this size as it fades out.
const EXIT_SCALE: f32 = 0.94;
/// The narrowest column that fits the footer's key hints, without and with
/// the Leftover worktrees button.
const FOOTER_HINTS_FIT: f32 = 420.;
const FOOTER_HINTS_FIT_WITH_LEFTOVERS: f32 = 580.;
/// The invisible strip over the column's edge that starts a resize.
const COLUMN_HANDLE_WIDTH: f32 = 8.;
/// The top row of both halves of the window, which is the title bar.
const BAR_HEIGHT: f32 = 48.;
/// Six base-branch rows. The list scrolls after that.
const BASE_LIST_MAX: f32 = 220.;
/// A terminal tab, sitting on the bottom of the 48px header.
const TAB_HEIGHT: f32 = 34.;
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

/// The right end of a card's first line, one 13px slot so the signals line
/// up down the column: working pixels, a ready dot until the result is seen,
/// a grey waiting dot, and nothing once a Ready result has been seen.
fn card_signal(card: &str, status: Status, unseen: bool, chrome: &Chrome) -> gpui::AnyElement {
    let slot = div().flex_none().flex().justify_center().w(px(13.));
    let color = chrome.status(status).dot;
    match status {
        Status::Working => slot.child(working_pixels(card, color)),
        Status::Ready if !unseen => slot,
        _ => slot.child(div().size(px(8.)).rounded_full().bg(color)),
    }
    .into_any_element()
}

/// The terminal area. While its task closes, the terminal fades to a trace
/// behind a grey wave and "Closing...", so the CLI's exit line and the
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
                .child(pixel_wave(
                    format!("closing-wave-{view}"),
                    chrome.term_dim,
                    4.,
                    3.,
                    6.,
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

/// A working card's indicator: three 3px squares rising and falling in a
/// staggered wave every 1.2s. Offsets snap to whole points so the squares
/// stay crisp and step like pixels. GPUI skips the loop when macOS asks for
/// reduced motion, leaving the first frame, a still staircase.
fn working_pixels(card: &str, color: gpui::Rgba) -> gpui::AnyElement {
    pixel_wave(format!("working-{card}"), color, 3., 2., 5.)
}

/// Three squares of `size` rising up to `rise` in a staggered wave every
/// 1.2s, the shape of the working pixels.
fn pixel_wave(id: String, color: gpui::Rgba, size: f32, gap: f32, rise: f32) -> gpui::AnyElement {
    div()
        .flex_none()
        .relative()
        .w(px(3. * size + 2. * gap))
        .h(px(size + rise))
        .with_animation(
            SharedString::from(id),
            gpui::Animation::new(Duration::from_millis(1200))
                .repeat_synced()
                .with_max_fps(30.),
            move |d, t| {
                d.children((0..3).map(|i| {
                    let phase = t - i as f32 / 6.;
                    let lift = (1. - (std::f32::consts::TAU * phase).cos()) / 2.;
                    div()
                        .absolute()
                        .left(px(i as f32 * (size + gap)))
                        .top(px(rise - (lift * rise).round()))
                        .size(px(size))
                        .bg(color)
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

/// The floating surface shared by the picker and every dialog.
fn dialog_shell(width: f32, max_h: Pixels, chrome: &Chrome) -> gpui::Stateful<gpui::Div> {
    div()
        .id("overlay-panel")
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
        let cards_focused = self.focus.is_focused(window);
        let chrome = self.chrome(window);
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
        for project in &self.projects {
            projects = projects.child(self.project_group(
                project,
                home.as_deref(),
                &chrome,
                cards_focused,
                reveal_selection,
                cx,
            ));
        }
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
            .on_action(cx.listener(|this, _: &CreatePr, window, cx| this.create_pr(window, cx)))
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
            .relative()
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(gpui::relative(1.3))
            .text_color(chrome.ink_1)
            .children(sidebar)
            .child(self.terminal_side(&chrome, cards_focused, home.as_deref(), window, cx))
            .children(changes_width.map(|width| self.changes_panel(width, &chrome, window, cx)))
            .children(column_width.map(|width| self.column_handle(width, &chrome, cx)))
            .children(changes_width.map(|width| {
                let left = window.viewport_size().width - width;
                self.changes_handle(left, &chrome, cx)
            }));
        if let Some(overlay) = &self.overlay {
            root = root.child(self.overlay_view(overlay, &chrome, home.as_deref(), window, cx));
        }
        root
    }
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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
        shika_terminal::init(cx);
        let mut bindings = vec![
            gpui::KeyBinding::new("cmd-n", NewAgent, Some("Shika")),
            gpui::KeyBinding::new("cmd-l", NewLead, Some("Shika")),
            gpui::KeyBinding::new("cmd-t", NewTerminal, Some("Shika")),
            gpui::KeyBinding::new("cmd-shift-p", CreatePr, Some("Shika")),
            gpui::KeyBinding::new("cmd-w", CloseTerminal, Some("Shika")),
            gpui::KeyBinding::new("cmd-shift-w", CloseTask, Some("Shika")),
            gpui::KeyBinding::new("cmd-shift-r", RenameTask, Some("Shika")),
            gpui::KeyBinding::new("ctrl-tab", NextTerminal, Some("Shika")),
            gpui::KeyBinding::new("ctrl-shift-tab", PreviousTerminal, Some("Shika")),
            gpui::KeyBinding::new("cmd-]", NextAgent, Some("Shika")),
            gpui::KeyBinding::new("cmd-[", PreviousAgent, Some("Shika")),
            gpui::KeyBinding::new("cmd-b", ToggleColumn, Some("Shika")),
            gpui::KeyBinding::new("cmd-alt-b", ToggleChanges, Some("Shika")),
            gpui::KeyBinding::new("cmd-q", Quit, None),
            gpui::KeyBinding::new("cmd-,", OpenSettings, None),
            gpui::KeyBinding::new("cmd-h", Hide, None),
            gpui::KeyBinding::new("cmd-alt-h", HideOthers, None),
        ];
        // Cmd+1 is the pinned agent. Later numbers follow the shell tabs in order.
        for index in 0..9 {
            let key = format!("cmd-{}", index + 1);
            bindings.push(gpui::KeyBinding::new(
                &key,
                SelectTerminal(index),
                Some("Shika"),
            ));
        }
        cx.bind_keys(bindings);
        let quitting_core = core.clone();
        cx.on_app_quit(move |_| {
            quitting_core.cancel_preparations();
            async {}
        })
        .detach();
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        // Only a release bundle has Sparkle, so only it offers the check.
        let mut app_menu = Vec::new();
        if let Some(updater) = updates::Updater::start() {
            cx.set_global(updater);
            cx.on_action(|_: &CheckForUpdates, cx| cx.global::<updates::Updater>().check());
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
            ]),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
        let settings = core.settings();
        let start = settings.as_ref().map(|s| s.appearance).unwrap_or_default();
        // Before the window opens, so it starts in the forced appearance.
        appearance::apply_mode(settings.as_ref().map(|s| s.theme.mode).unwrap_or_default());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Shika".into()),
                        appears_transparent: true,
                        // Centers the traffic lights in the 48px top row.
                        traffic_light_position: Some(gpui::point(
                            px(TRAFFIC_LIGHTS.0),
                            px(TRAFFIC_LIGHTS.1),
                        )),
                    }),
                    // The default 540px column plus a terminal wide enough to read.
                    window_min_size: Some(size(px(960.), px(600.))),
                    // The gear and New agent live in the title bar, so clicks
                    // there have to reach the app. The bar starts the drag itself.
                    app_owns_titlebar_drag: true,
                    window_background: appearance::background(&start),
                    ..Default::default()
                },
                |window, cx| {
                    let app = cx.new(|cx| Shika::new(core, settings, diagnostics, window, cx));
                    window.focus(&app.focus_handle(cx), cx);
                    app
                },
            )
            .expect("Open Shika window");
        // The blur radius needs the window on screen, which it is now.
        let _ = handle.update(cx, |_, window, _| appearance::apply(&start, window));
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
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), Some(1));
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(1)), 1), Some(2));
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(2)), -1),
            Some(1)
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Project("b".into())), -1),
            Some(0)
        );
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Project("b".into())), 1),
            Some(1)
        );
    }

    #[test]
    fn agent_navigation_handles_empty_single_and_missing_selection() {
        assert_eq!(adjacent_agent(&[], None, 1), None);
        let rows = vec![Selection::Project("a".into()), Selection::Card(0)];
        assert_eq!(adjacent_agent(&rows, None, 1), Some(0));
        assert_eq!(adjacent_agent(&rows, None, -1), Some(0));
        assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), Some(0));
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(0)), -1),
            Some(0)
        );
        assert_eq!(adjacent_agent(&rows[..1], None, 1), None);
        assert_eq!(
            adjacent_agent(&rows, Some(&Selection::Card(9)), -1),
            Some(0)
        );
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
        drop(host);
        drop(core);
        std::fs::remove_dir_all(path).unwrap();
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
