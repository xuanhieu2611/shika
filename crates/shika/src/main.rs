mod appearance;
mod model;
mod notifications;

use appearance::{Chrome, with_alpha};
use gpui::{
    AnimationExt, App, AppContext, Bounds, BoxShadow, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Render, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use model::{PromptCapture, Status, TitleWatch, visible_indices};
use notifications::Notifications;
use shika_core::{
    Appearance, CliCatalog, Core, DiffStat, FontSize, JournalEntry, Project, PtyEvent, PtyId,
    PtySize, Session, SessionGitState, Settings, Translucency,
};
use shika_terminal::{
    InputSource, Palette, PtyHost, Terminal, TerminalConfig, TerminalOptions, TerminalSize,
    TerminalView,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

gpui::actions!(shika, [Quit, Hide, HideOthers, ShowAll, OpenSettings]);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Default)]
struct HostState {
    pty: Option<PtyId>,
    measured: Option<TerminalSize>,
    prompt: PromptCapture,
    title: Option<String>,
    last_output: Option<Instant>,
    /// Newest output that is the agent's own work, not an echo or a redraw.
    last_agent_output: Option<Instant>,
    /// The user's last key or paste.
    last_typed: Option<Instant>,
    /// The user's last key, paste, focus change, click, scroll, or resize,
    /// any of which can make the CLI redraw.
    last_input: Option<Instant>,
    exited: bool,
    submission: u64,
    pending_input: Vec<Vec<u8>>,
}
struct Host {
    core: Arc<Core>,
    state: Arc<Mutex<HostState>>,
    capture: bool,
}
impl PtyHost for Host {
    fn write(&self, bytes: &[u8], source: InputSource) {
        let mut s = lock(&self.state);
        let now = Instant::now();
        if source == InputSource::Typed {
            s.last_typed = Some(now);
        }
        if source != InputSource::Reply {
            s.last_input = Some(now);
        }
        if self.capture {
            if let Some(title) = s.prompt.feed(bytes) {
                s.title = Some(title);
            }
            if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
                s.submission += 1;
            }
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
        s.last_input = Some(Instant::now());
        if let Some(pty) = s.pty {
            let _ = self.core.resize(pty, PtySize::new(size.rows, size.cols));
        }
    }
}
impl HostState {
    /// Record PTY bytes. Echoes and redraws stay in `last_output` only, so a
    /// draft typed into the prompt does not look like the agent working.
    fn note_output(&mut self, now: Instant) {
        self.last_output = Some(now);
        if model::is_agent_output(self.last_input, now) {
            self.last_agent_output = Some(now);
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
        Self {
            view,
            terminal,
            state,
        }
    }
}
struct Card {
    session: Option<Session>,
    project: String,
    title: String,
    preset: String,
    status: Status,
    since: Instant,
    agent: Pane,
    shell: Option<Pane>,
    show_shell: bool,
    submitted: u64,
    /// The `last_typed` whose turn already notified.
    notified: Option<Instant>,
    creating: bool,
    title_watch: TitleWatch,
    /// The prompt or the CLI's title has named the card. Until then it reads
    /// "New <CLI>" in a lighter ink.
    named: bool,
    /// What the task changed, fetched each time the card turns Ready.
    diff: Option<DiffStat>,
}
#[derive(Clone, PartialEq)]
enum Selection {
    Project(String),
    Card(usize),
}
enum Overlay {
    Picker {
        project: String,
        index: usize,
    },
    Close {
        index: usize,
        state: SessionGitState,
    },
    Leftovers,
    RemoveLeftover(usize),
    RemoveProject(String),
    /// `row` is the selected setting: opacity, blur, translucency, font size,
    /// then the branch prefix. `edit` holds digits typed into the selected
    /// number, or the prefix being typed, not yet applied.
    Settings {
        row: usize,
        edit: Option<String>,
    },
}
struct Shika {
    core: Arc<Core>,
    projects: Vec<Project>,
    cards: Vec<Card>,
    selection: Option<Selection>,
    focus: FocusHandle,
    catalog: Option<CliCatalog>,
    overlay: Option<Overlay>,
    busy: bool,
    toast: Option<(String, Instant)>,
    leftovers: Vec<JournalEntry>,
    leftover_selected: usize,
    clock: Instant,
    notifications: Notifications,
    clicks: std::sync::mpsc::Receiver<String>,
    appearance: Appearance,
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
        let font_size = settings.font_size;
        let branch_prefix = shika_core::normalize_branch_prefix(&settings.branch_prefix);
        let reduce_transparency = appearance::reduce_transparency();
        let entity = cx.entity().downgrade();
        let appearance_watch = window.observe_window_appearance(move |window, cx| {
            let dark = appearance::is_dark(window.appearance());
            let _ = entity.update(cx, |this, cx| {
                this.push_terminal_theme(dark, cx);
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
            busy: false,
            toast: if load_errors.is_empty() {
                None
            } else {
                Some((load_errors.join("; "), Instant::now()))
            },
            leftovers,
            leftover_selected: 0,
            clock: Instant::now(),
            notifications,
            clicks,
            appearance,
            font_size,
            reduce_transparency,
            appearance_watch,
            title_drag: false,
            branch_prefix,
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
        indices.sort_by_key(|i| self.cards[*i].status.rank());
        indices
    }
    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let at = self
            .selection
            .as_ref()
            .and_then(|s| rows.iter().position(|r| r == s))
            .unwrap_or(0);
        self.selection =
            Some(rows[(at as isize + delta).rem_euclid(rows.len() as isize) as usize].clone());
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(i) = self.selected_card() {
            let c = &self.cards[i];
            let pane = if c.show_shell {
                c.shell.as_ref().unwrap_or(&c.agent)
            } else {
                &c.agent
            };
            window.focus(&pane.view.focus_handle(cx), cx);
            cx.notify();
        }
    }
    fn picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(project) = self.project_id() {
            self.overlay = Some(Overlay::Picker { project, index: 0 });
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
        let Some(Overlay::Picker { project, index }) = &self.overlay else {
            return;
        };
        let Some(preset) = self
            .catalog
            .as_ref()
            .and_then(|c| c.presets.get(*index))
            .cloned()
        else {
            return;
        };
        if !preset.found() {
            self.message(format!("{} not found on PATH", preset.binary));
            cx.notify();
            return;
        }
        let project = project.clone();
        self.overlay = None;
        self.busy = true;
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
        let index = self.cards.len();
        self.cards.push(Card {
            session: None,
            project: project.clone(),
            title: format!("New {}", preset.name),
            preset: preset.name.clone(),
            status: Status::Waiting,
            since: Instant::now(),
            agent: pane,
            shell: None,
            show_shell: false,
            submitted: 0,
            notified: None,
            creating: true,
            title_watch: TitleWatch::default(),
            named: false,
            diff: None,
        });
        self.selection = Some(Selection::Card(index));
        window.focus(&self.focus, cx);
        cx.notify();
        let core = self.core.clone();
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
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = core.create_session(
                        &project,
                        &preset.id,
                        PtySize::new(measured.rows, measured.cols),
                        move |_, event| match event {
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
                        },
                    );
                    if let Ok(s) = &result {
                        bind_host(&core, &state_bind, s.pty);
                    }
                    result
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(session) => {
                        if let Some(card) = this.cards.get_mut(index) {
                            card.session = Some(session);
                            card.creating = false;
                        }
                        this.focus_terminal(window, cx);
                    }
                    Err(e) => {
                        this.cards.remove(index);
                        this.selection = this
                            .projects
                            .first()
                            .map(|p| Selection::Project(p.id.clone()));
                        this.message(e.to_string());
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn toggle(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating || self.busy {
            return;
        }
        if self.cards[index].show_shell {
            self.cards[index].show_shell = false;
            if focus {
                self.focus_terminal(window, cx);
            }
            cx.notify();
            return;
        }
        self.cards[index].show_shell = true;
        if self.cards[index].shell.is_some() {
            if focus {
                self.focus_terminal(window, cx);
            }
            cx.notify();
            return;
        }
        let Some(session) = self.cards[index].session.clone() else {
            return;
        };
        let opacity = self.terminal_opacity();
        let pane = Pane::new(
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
        self.cards[index].shell = Some(pane);
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
                    Ok(opened) => {
                        bind_host(&this.core, &state, opened.pty);
                        if focus {
                            this.focus_terminal(window, cx);
                        }
                    }
                    Err(e) => {
                        this.cards[index].shell = None;
                        this.cards[index].show_shell = false;
                        this.message(e.to_string());
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications.refresh_diagnostics();
        let now = Instant::now();
        let mut changed = false;
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
                self.push_terminal_theme(appearance::is_dark(window.appearance()), cx);
                changed = true;
            }
        }
        for warning in self.notifications.warnings() {
            self.message(warning);
            changed = true;
        }
        for card in &mut self.cards {
            let mut state = lock(&card.agent.state);
            if card.creating {
                continue;
            }
            if let Some(title) = state.title.take() {
                if !card.session.as_ref().is_some_and(|s| s.cli_titled) {
                    card.title = model::card_title(&title);
                }
                card.named = true;
                card.status = Status::Working;
                card.since = now;
                if let Some(s) = &card.session {
                    rename.push((s.id.clone(), title));
                }
                changed = true;
            }
            if state.submission > 0 {
                card.title_watch.start(now);
            }
            if state.submission != card.submitted && card.status != Status::Waiting {
                card.submitted = state.submission;
                card.status = Status::Working;
                card.since = now;
                changed = true;
            }
            if card.status != Status::Waiting {
                let (clock, settled) = model::advance_status(
                    model::TurnClock {
                        status: card.status,
                        since: card.since,
                    },
                    state.last_input,
                    state.last_agent_output,
                    now,
                    state.exited,
                );
                if clock.status != card.status || clock.since != card.since {
                    card.status = clock.status;
                    card.since = clock.since;
                    changed = true;
                }
                if settled {
                    let latest = state.last_output.unwrap_or(card.since);
                    if let Some(session) = &card.session {
                        ready.push(session.id.clone());
                    }
                    if model::notify_ready(
                        state.last_typed,
                        card.notified,
                        state.last_input,
                        latest,
                    ) && let Some(session) = &card.session
                    {
                        let project = self
                            .projects
                            .iter()
                            .find(|p| p.id == card.project)
                            .map(|p| p.name.as_str())
                            .unwrap_or("Shika");
                        self.notifications.post(&session.id, project, &card.title);
                        card.notified = state.last_typed;
                    }
                }
            } else if state.exited {
                card.status = Status::Ready;
                card.since = now;
                changed = true;
                if let Some(session) = &card.session {
                    ready.push(session.id.clone());
                    let project = self
                        .projects
                        .iter()
                        .find(|p| p.id == card.project)
                        .map(|p| p.name.as_str())
                        .unwrap_or("Shika");
                    self.notifications.post(&session.id, project, &card.title);
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
                        Ok(Some(session)) => {
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
            self.fetch_diff_stat(id, cx);
        }
        for (id, title) in rename {
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { core.session_rename_from_prompt(&id, &title) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(session) => {
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
                let focus = self.focus.clone();
                window.focus(&focus, cx);
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
                    && card.status == Status::Ready
                {
                    card.diff = result.ok();
                    cx.notify();
                }
            });
        })
        .detach();
    }
    /// A summary chip: select the first card with that status, in row order.
    fn select_status(&mut self, status: Status, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let first = self
            .rows()
            .into_iter()
            .find(|row| matches!(row, Selection::Card(i) if self.cards[*i].status == status));
        if let Some(row) = first {
            self.selection = Some(row);
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }
    /// The picker for one project, from its `+` or its empty box.
    fn picker_for(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.overlay = Some(Overlay::Picker { project, index: 0 });
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        let Some(session) = &self.cards[index].session else {
            return;
        };
        let id = session.id.clone();
        let working = self.cards[index].status == Status::Working;
        let core = self.core.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.session_git_state(&id, working) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(state) if state.requires_confirmation() => {
                        this.overlay = Some(Overlay::Close { index, state });
                        window.focus(&this.focus, cx);
                    }
                    Ok(_) => this.finish_close(index, 0, window, cx),
                    Err(e) => this.message(e.to_string()),
                };
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
        let working = self.cards[index].status == Status::Working || {
            let host = lock(&self.cards[index].agent.state);
            !host.exited
                && self.cards[index].status != Status::Waiting
                && (host.submission != self.cards[index].submitted
                    || host
                        .last_agent_output
                        .is_some_and(|t| t.elapsed() < model::QUIET))
        };
        let core = self.core.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match action {
                        1 => core.session_discard(&id),
                        2 => core.session_push_and_close(&id),
                        _ => core.session_close(&id, working),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.cards.remove(index);
                        this.overlay = None;
                        this.selection = if this.cards.is_empty() {
                            this.projects
                                .first()
                                .map(|p| Selection::Project(p.id.clone()))
                        } else {
                            Some(Selection::Card(index.min(this.cards.len() - 1)))
                        };
                        window.focus(&this.focus, cx);
                    }
                    Err(e) => this.message(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn cancel_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_setting_edit(window, cx);
        let shell = match &self.overlay {
            Some(Overlay::Close { index, state }) if state.dirty || state.unpushed => Some(*index),
            _ => None,
        };
        self.overlay = None;
        if let Some(i) = shell {
            self.selection = Some(Selection::Card(i));
            if let Some(i) = self.selected_card() {
                if !self.cards[i].show_shell {
                    self.toggle(true, window, cx);
                } else {
                    self.focus_terminal(window, cx);
                }
            }
        } else {
            window.focus(&self.focus, cx);
        }
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
        self.overlay = Some(Overlay::Settings { row: 0, edit: None });
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
        if row == FONT_ROW {
            self.set_font_size(self.font_size.step(delta), cx);
            return;
        }
        let mut next = self.appearance;
        match row {
            0 => next = next.with_opacity(i64::from(next.opacity) + delta * 5),
            1 => next = next.with_blur(i64::from(next.blur) + delta * 5),
            2 => {
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
        if let Some(Overlay::Settings { row: at, edit }) = &mut self.overlay {
            *at = row;
            *edit = Some(digits.to_string());
        }
        cx.notify();
    }
    /// Apply typed digits, pulled into range, or the typed prefix, made
    /// safe for git. Nothing typed keeps a number.
    fn commit_setting_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Settings { row, edit }) = &mut self.overlay else {
            return;
        };
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
            0 => self.appearance.with_opacity(value),
            1 => self.appearance.with_blur(value),
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
        self.push_terminal_theme(appearance::is_dark(window.appearance()), cx);
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
            for pane in std::iter::once(&card.agent).chain(card.shell.as_ref()) {
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
    fn terminal_opacity(&self) -> f32 {
        appearance::terminal_alpha_for(&self.appearance, self.reduce_transparency)
    }
    fn terminal_palette(&self, window: &Window) -> Palette {
        if appearance::is_dark(window.appearance()) {
            Palette::shika_dark()
        } else {
            Palette::shika()
        }
    }
    fn chrome(&self, window: &Window) -> Chrome {
        appearance::chrome_for(
            &self.appearance,
            appearance::is_dark(window.appearance()),
            self.reduce_transparency,
        )
    }
    fn push_terminal_theme(&mut self, dark: bool, cx: &mut Context<Self>) {
        let opacity = self.terminal_opacity();
        let palette = if dark {
            Palette::shika_dark()
        } else {
            Palette::shika()
        };
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shell.as_ref()) {
                pane.view.update(cx, |view, cx| {
                    view.set_palette(palette, cx);
                    view.set_background_opacity(opacity, cx);
                });
            }
        }
    }
    fn save_settings(&mut self) {
        let settings = Settings {
            appearance: self.appearance,
            branch_prefix: self.branch_prefix.clone(),
            font_size: self.font_size,
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
            match &mut self.overlay {
                Some(Overlay::Picker { index, project }) => match key {
                    "j" | "down" => *index = (*index + 1) % 2,
                    "k" | "up" => *index = (*index + 1) % 2,
                    "1" => {
                        *index = 0;
                        self.launch(window, cx);
                    }
                    "2" => {
                        *index = 1;
                        self.launch(window, cx);
                    }
                    "enter" => self.launch(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    "tab" => {
                        if let Some(at) = self.projects.iter().position(|p| &p.id == project) {
                            *project = self.projects[(at + 1) % self.projects.len()].id.clone();
                        }
                    }
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
                Some(Overlay::Settings { row, edit }) => {
                    let at = *row;
                    let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
                    let number_row = matches!(at, 0 | 1 | FONT_ROW);
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
                            self.commit_setting_edit(window, cx);
                            if let Some(Overlay::Settings { row, .. }) = &mut self.overlay {
                                *row = (at + 1) % SETTING_ROWS;
                            }
                        }
                        (_, "k" | "up") => {
                            self.commit_setting_edit(window, cx);
                            if let Some(Overlay::Settings { row, .. }) = &mut self.overlay {
                                *row = (at + SETTING_ROWS - 1) % SETTING_ROWS;
                            }
                        }
                        _ => {}
                    }
                }
                None => {}
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
            "n" => self.picker(window, cx),
            "enter" => self.focus_terminal(window, cx),
            "g" => self.toggle(false, window, cx),
            "c" => self.close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}
/// Settings rows: opacity, blur, translucency, font size, then the branch prefix.
const SETTING_ROWS: usize = 5;
const FONT_ROW: usize = 3;
const PREFIX_ROW: usize = 4;
impl Shika {
    /// The branch prefix row: a text field. Enter or a click starts typing.
    fn prefix_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row, edit }) => (
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
                        edit: Some(_)
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
            Some(Overlay::Settings { row: at, edit }) => {
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

    /// The sidebar half of the title bar: the real traffic lights, the
    /// wordmark, the settings gear, and New agent. The system title is hidden.
    fn top_row(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inset = if window.is_fullscreen() || window.is_simple_fullscreen() {
            px(18.)
        } else {
            px(WORDMARK_INSET)
        };
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
                        cx.new(|_| SettingsHint {
                            bg: tip_bg,
                            fg: tip_fg,
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
                    .child(kbd("n", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| this.picker(window, cx))),
            );
        self.title_drag(row, cx)
    }

    /// The headline and the status chips.
    fn summary(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self
            .projects
            .iter()
            .filter(|p| self.cards.iter().any(|c| c.project == p.id))
            .count();
        let chips = [Status::Ready, Status::Working, Status::Waiting].map(|status| {
            let count = self.cards.iter().filter(|c| c.status == status).count();
            let colors = chrome.status(status);
            let loud = count > 0 && status != Status::Waiting;
            div()
                .id(SharedString::from(format!("chip-{}", status.chip())))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded_full()
                .pl(px(9.))
                .pr(px(10.))
                .py(px(4.))
                .text_size(px(12.))
                .line_height(px(14.))
                .cursor_pointer()
                .bg(if loud { colors.chip } else { chrome.hover })
                .text_color(if loud {
                    colors.text
                } else if count > 0 {
                    chrome.ink_2
                } else {
                    chrome.ink_4
                })
                .font_weight(if loud {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .child(dot(status, 7., chrome))
                .child(format!("{count} {}", status.chip()))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.select_status(status, window, cx)),
                )
        });
        div()
            .flex_shrink_0()
            .pt(px(6.))
            .px(px(20.))
            .pb(px(18.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .child(model::headline(self.cards.len(), active)),
            )
            .child(div().flex().flex_wrap().gap(px(6.)).children(chips))
    }

    /// One project: its header, its empty box or its cards, and the count of
    /// cards past the three shown.
    fn project_group(
        &self,
        project: &Project,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cards_focused: bool,
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
        let header = div()
            .id(SharedString::from(format!("project-{id}")))
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
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .child(project.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(chrome.ink_4)
                    .child(model::tilde(&project.path, home)),
            )
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
            .when(!indices.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(chrome.ink_3)
                        .child(model::plural(indices.len(), "agent")),
                )
            })
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
        let mut column = div().flex().flex_col().gap(px(6.)).child(header);
        if indices.is_empty() {
            let card_rest = chrome.card_rest;
            let ink_2 = chrome.ink_2;
            column = column.child(
                div()
                    .id(SharedString::from(format!("empty-{id}")))
                    .border_1()
                    .border_dashed()
                    .border_color(chrome.dashed)
                    .rounded(px(10.))
                    .px(px(14.))
                    .py(px(12.))
                    .text_size(px(12.5))
                    .line_height(px(16.))
                    .text_color(chrome.ink_3)
                    .cursor_pointer()
                    .hover(move |style| style.bg(card_rest).text_color(ink_2))
                    .child("No one is on this repo. Press + to start an agent.")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.picker_for(id.clone(), window, cx)
                    })),
            );
        }
        let selected_index = self
            .selected_card()
            .and_then(|i| indices.iter().position(|j| *j == i));
        let shown = visible_indices(indices.len(), selected_index);
        let visible = shown.len();
        for at in shown {
            column = column.child(self.card_view(indices[at], chrome, cards_focused, cx));
        }
        if indices.len() > visible {
            column = column.child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(12.))
                    .text_color(chrome.ink_3)
                    .child(format!("+ {} more · j to reach", indices.len() - visible)),
            );
        }
        column
    }

    /// A card: dot, task, and the timer while working; then CLI, status,
    /// branch, the diff stat when ready, and key hints on the selected card.
    fn card_view(
        &self,
        i: usize,
        chrome: &Chrome,
        cards_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let card = &self.cards[i];
        let selected = self.selected_card() == Some(i);
        let colors = chrome.status(card.status);
        let separator = || div().flex_none().text_color(chrome.ink_5).child("·");
        let hints: &[(&str, &str)] = match card.status {
            Status::Ready => &[("↵", "read"), ("g", "shell")],
            Status::Working => &[("↵", "watch")],
            Status::Waiting => &[("↵", "write prompt")],
        };
        let stat = card
            .diff
            .filter(|d| card.status == Status::Ready && d.files > 0)
            .map(|d| model::diff_stat_label(d.files, d.insertions, d.deletions));
        let task = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(status_dot(i, card.status, chrome))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.5))
                    .line_height(px(19.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if card.named {
                        chrome.ink_1
                    } else {
                        chrome.ink_3
                    })
                    .child(card.title.clone()),
            )
            .when(card.status == Status::Working, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(colors.text)
                        .child(model::short_time(card.since.elapsed())),
                )
            });
        let meta = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(18.))
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(chrome.ink_3)
            .child(div().flex_none().child(card.preset.clone()))
            .child(separator())
            .child(
                div()
                    .flex_none()
                    .text_color(colors.text)
                    .font_weight(match card.status {
                        Status::Ready => FontWeight::SEMIBOLD,
                        Status::Working => FontWeight::MEDIUM,
                        Status::Waiting => FontWeight::NORMAL,
                    })
                    .child(card.status.label()),
            )
            .when_some(card.session.as_ref(), |d, session| {
                d.child(separator()).child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .child(session.branch.clone()),
                )
            })
            .when_some(stat, |d, stat| {
                d.child(separator()).child(div().flex_none().child(stat))
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
                        .child(kbd(*key, chrome.sunken, chrome.ink_2).py_0())
                        .child(*label)
                }))
            });
        let base = div()
            .id(SharedString::from(format!("card-{i}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(10.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy || this.overlay.is_some() {
                    return;
                }
                this.selection = Some(Selection::Card(i));
                window.focus(&this.focus, cx);
                cx.notify();
            }));
        let base = if selected {
            // The ring sits outside the card, like the design's box-shadow,
            // so the selected card keeps the resting card's size.
            let (ring, width) = if cards_focused {
                (chrome.focus, 1.5)
            } else {
                (chrome.line_selected_dim, 1.)
            };
            let mut shadows = vec![];
            if cards_focused {
                shadows.push(
                    BoxShadow::new(px(0.), px(2.), chrome.card_shadow.into())
                        .blur_radius(px(chrome.card_shadow_blur)),
                );
            }
            shadows.push(BoxShadow::new(px(0.), px(0.), ring.into()).spread_radius(px(width)));
            base.bg(chrome.card_selected)
                .pt(px(12.))
                .px(px(14.))
                .pb(px(11.))
                .shadow(shadows)
        } else {
            // A drop shadow would also paint under a translucent card, so the
            // resting ring is a border and the padding gives back its 1px.
            base.overflow_hidden()
                .bg(if card.status == Status::Ready {
                    chrome.card_ready
                } else {
                    chrome.card_rest
                })
                .border_1()
                .border_color(chrome.line_subtle)
                .pt(px(11.))
                .px(px(13.))
                .pb(px(10.))
                .when(chrome.glass, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(1.))
                            .bg(chrome.card_highlight),
                    )
                })
        };
        base.child(task).child(meta)
    }

    /// Add project, leftovers, and the key hints.
    fn footer(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
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
        div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(chrome.line_2)
            .pt(px(10.))
            .px(px(12.))
            .pb(px(12.))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        text_button("add")
                            .child("Add project…")
                            .child(kbd("a", chrome.sunken, chrome.ink_3))
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .when(!self.leftovers.is_empty(), |d| {
                        d.child(
                            text_button("leftovers")
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
            .child(
                // Stays on the right when the footer wraps.
                div()
                    .ml_auto()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .pr(px(6.))
                    .child(hint("j k", "move", chrome))
                    .child(hint("↵", "terminal", chrome))
                    .child(hint("g", "shell", chrome))
                    .child(hint("c", "close", chrome)),
            )
    }

    /// The selected card's terminal under its header, or the empty state.
    fn terminal_side(
        &self,
        chrome: &Chrome,
        cards_focused: bool,
        home: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
            let shell = card.show_shell;
            let path = card
                .session
                .as_ref()
                .map(|s| model::tilde(&s.worktree, home))
                .unwrap_or("Creating worktree...".into());
            let term_hover = chrome.term_hover;
            let header = div()
                .id("terminal-header")
                .h(px(BAR_HEIGHT))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(12.))
                .pl(px(14.))
                .pr(px(12.))
                .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
                .border_b_1()
                .border_color(chrome.term_line)
                .child(
                    div()
                        .occlude()
                        .flex_none()
                        .flex()
                        .p(px(2.))
                        .gap(px(2.))
                        .rounded(px(7.))
                        .bg(chrome.term_seg)
                        .child(
                            segment(
                                "agent",
                                card.preset.clone(),
                                !shell,
                                chrome.term_seg_active,
                                chrome.term_white,
                                chrome.term_dim,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    if this.busy || this.overlay.is_some() {
                                        return;
                                    }
                                    if let Some(i) = this.selected_card() {
                                        this.cards[i].show_shell = false;
                                    }
                                    this.focus_terminal(window, cx);
                                },
                            )),
                        )
                        .child(
                            segment(
                                "shell",
                                "Shell",
                                shell,
                                chrome.term_seg_active,
                                chrome.term_white,
                                chrome.term_dim,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    if let Some(i) = this.selected_card() {
                                        if this.cards[i].show_shell {
                                            this.focus_terminal(window, cx);
                                        } else {
                                            this.toggle(true, window, cx);
                                        }
                                    }
                                },
                            )),
                        ),
                )
                .child(
                    // Ellipsis at the start keeps the end of the path, the
                    // worktree's own folder, in view.
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(chrome.term_dim)
                        .child(path),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(chrome.term_faint)
                        .child(if cards_focused {
                            "↵ type here"
                        } else {
                            "ctrl q back to cards"
                        }),
                )
                .child(
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
                        .child("Close")
                        .child(kbd("c", chrome.term_line, chrome.term_dim).py_0())
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                );
            let pane = if shell {
                card.shell.as_ref().unwrap_or(&card.agent)
            } else {
                &card.agent
            };
            right = right
                .child(self.title_drag(header, cx))
                .child(div().flex_1().min_h_0().child(pane.view.clone()));
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
                                .h(px(BAR_HEIGHT)),
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
                            .child(key("n", "new agent")),
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

    /// The picker, a dialog, or Settings, over a scrim near the top.
    fn overlay_view(
        &self,
        overlay: &Overlay,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let height = window.viewport_size().height;
        let picker = matches!(overlay, Overlay::Picker { .. });
        let top = height * if picker { 0.18 } else { 0.20 };
        let max_h = (height - top - px(24.)).max(px(120.));
        let settings = matches!(overlay, Overlay::Settings { .. });
        let panel = match overlay {
            Overlay::Picker { project, index } => {
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
                            .child(dialog_title(div()).child("New agent"))
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
                if let Some(catalog) = &self.catalog {
                    for (i, preset) in catalog.presets.iter().enumerate() {
                        let detail = match &preset.path {
                            Some(path) => model::tilde(path, home),
                            None => format!("{} not found on PATH", preset.binary),
                        };
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
                                        .text_color(if preset.found() {
                                            chrome.ink_1
                                        } else {
                                            chrome.ink_4
                                        })
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
                } else {
                    panel = panel.child(
                        list_row(false, chrome)
                            .text_color(chrome.ink_3)
                            .child("Resolving login-shell PATH..."),
                    );
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
                        .child("↵ start")
                        .child("esc cancel")
                        .child("creates a branch and a worktree"),
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
                        "The worktree has uncommitted changes. Commit in the shell before pushing. Shika does not commit.",
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
            Overlay::Settings { row, edit } => {
                let opacity = self.appearance.opacity.to_string();
                let blur = self.appearance.blur.to_string();
                let font = self.font_size.text();
                let both = self.appearance.translucency == Translucency::SidebarAndTerminal;
                let keys: &[(&str, &str)] = match (edit.is_some(), *row == PREFIX_ROW) {
                    (true, true) => &[("", "type a prefix"), ("↵", "apply"), ("esc", "cancel")],
                    (true, false) => &[("", "type a number"), ("↵", "apply"), ("esc", "cancel")],
                    (false, true) => &[("j k", "choose"), ("↵", "edit"), ("esc", "done")],
                    (false, false) => &[
                        ("j k", "choose"),
                        ("h l", "change"),
                        ("", "type a number"),
                        ("esc", "done"),
                    ],
                };
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
                dialog(chrome, max_h)
                    .w(px(440.))
                    .gap(px(4.))
                    .child(dialog_title(div()).mb(px(4.)).child("Settings"))
                    .child(self.setting_row(
                        0,
                        "Background opacity",
                        &opacity,
                        "%",
                        chrome,
                        cx,
                    ))
                    .child(self.setting_row(1, "Background blur", &blur, "", chrome, cx))
                    .child(
                        list_row(*row == 2, chrome)
                            .justify_between()
                            .child("Apply to")
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.))
                                    .gap(px(2.))
                                    .rounded(px(7.))
                                    .bg(chrome.sunken)
                                    .child(choice("translucent-sidebar", "Sidebar", !both).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.step_setting(2, -1, window, cx)
                                        }),
                                    ))
                                    .child(
                                        choice("translucent-both", "Sidebar and terminal", both)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.step_setting(2, 1, window, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(self.setting_row(FONT_ROW, "Font size", &font, "px", chrome, cx))
                    .child(self.prefix_row(chrome, cx))
                    .child(
                        div()
                            .mt(px(6.))
                            .px(px(10.))
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(chrome.ink_3)
                            .child(
                                "Opacity 0 to 100%. Blur radius 0 to 255, shown when opacity is below 100%. Font size is the terminal text, 8 to 32. The prefix starts each new branch name, like hieu/.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(12.))
                            .gap_y(px(6.))
                            .mt(px(6.))
                            .px(px(10.))
                            .children(keys.iter().map(|(key, label)| hint(key, label, chrome))),
                    )
                    .child(dialog_buttons().child(self.cancel_button("Done", chrome, cx)))
            }
        };
        div()
            .absolute()
            .occlude()
            .inset_0()
            // Settings leaves the window undimmed, so it is the preview.
            .when(!settings, |d| d.bg(chrome.scrim))
            .flex()
            .items_start()
            .justify_center()
            .pt(top)
            .child(panel)
    }
}
impl Focusable for Shika {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
const UI_FONT: &str = ".AppleSystemUIFont";
const MONO: &str = "JetBrains Mono";
/// The agent column. Fixed: there is no drag handle.
const COLUMN_WIDTH: f32 = 540.;
/// The top row of both halves of the window, which is the title bar.
const BAR_HEIGHT: f32 = 48.;
/// The traffic lights sit 18px from the left, centered in the 48px bar.
/// AppKit's buttons are 14px tall on macOS 26, so 17px above and below.
const TRAFFIC_LIGHTS: (f32, f32) = (18., 17.);
/// The traffic lights end at x=78. The wordmark starts 12px after them.
const WORDMARK_INSET: f32 = 90.;

/// Filled gear. Drawn as an alpha mask and tinted by the element's text color.
const SETTINGS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-rule="evenodd" d="M11.078 2.25c-.917 0-1.699.663-1.85 1.567L9.05 4.889c-.02.12-.115.26-.297.348a7.493 7.493 0 0 0-.986.57c-.166.115-.334.126-.45.083L6.3 5.508a1.875 1.875 0 0 0-2.282.819l-.922 1.597a1.875 1.875 0 0 0 .432 2.385l.84.692c.095.078.17.229.154.43a7.598 7.598 0 0 0 0 1.139c.015.2-.059.352-.153.43l-.841.692a1.875 1.875 0 0 0-.432 2.385l.922 1.597a1.875 1.875 0 0 0 2.282.818l1.019-.382c.115-.043.283-.031.45.082.312.214.641.405.985.57.182.088.277.228.297.35l.178 1.071c.151.904.933 1.567 1.85 1.567h1.844c.916 0 1.699-.663 1.85-1.567l.178-1.072c.02-.12.114-.26.297-.349.344-.165.673-.356.985-.57.167-.114.335-.125.45-.082l1.02.382a1.875 1.875 0 0 0 2.28-.819l.923-1.597a1.875 1.875 0 0 0-.432-2.385l-.84-.692c-.095-.078-.17-.229-.154-.43a7.614 7.614 0 0 0 0-1.139c-.016-.2.059-.352.153-.43l.84-.692c.708-.582.891-1.59.433-2.385l-.922-1.597a1.875 1.875 0 0 0-2.282-.818l-1.02.382c-.114.043-.282.031-.449-.083a7.49 7.49 0 0 0-.985-.57c-.183-.087-.277-.227-.297-.348l-.179-1.072a1.875 1.875 0 0 0-1.85-1.567h-1.843ZM12 15.75a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5Z"/></svg>"##;

struct SettingsHint {
    bg: Rgba,
    fg: Rgba,
}

impl Render for SettingsHint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(self.bg)
            .text_color(self.fg)
            .text_size(px(12.))
            .font_family(UI_FONT)
            .child("Settings  \u{2318},")
    }
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

/// A key cap and the verb it does, as in the footer: `j k move`. An empty
/// key leaves only the words.
fn hint(key: &str, label: &str, chrome: &Chrome) -> gpui::Div {
    div()
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

/// A status dot. Working pulses, waiting is a hollow ring.
fn dot(status: Status, size: f32, chrome: &Chrome) -> gpui::Div {
    let color = chrome.status(status).dot;
    let dot = div().flex_none().size(px(size)).rounded_full();
    if status == Status::Waiting {
        dot.border(px(1.5)).border_color(color)
    } else {
        dot.bg(color)
    }
}

/// A card's dot. The working dot fades to 30% and back every 1.6s. GPUI
/// skips the loop when macOS asks for reduced motion.
fn status_dot(card: usize, status: Status, chrome: &Chrome) -> gpui::AnyElement {
    let dot = dot(status, 8., chrome);
    if status != Status::Working {
        return dot.into_any_element();
    }
    dot.with_animation(
        SharedString::from(format!("pulse-{card}")),
        gpui::Animation::new(Duration::from_millis(1600))
            .repeat_synced()
            .with_easing(gpui::bounce(gpui::ease_in_out))
            .with_max_fps(30.),
        |dot, t| dot.opacity(1. - 0.7 * t),
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
    id: &'static str,
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
        let mut projects = div()
            .id("projects")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.))
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
                cx,
            ));
        }
        let sidebar = div()
            .w(px(COLUMN_WIDTH))
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(chrome.column)
            .border_r_1()
            .border_color(chrome.hairline)
            .child(self.top_row(&chrome, window, cx))
            .child(self.summary(&chrome, cx))
            .child(projects)
            .child(self.footer(&chrome, cx));
        let mut root = div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .relative()
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(gpui::relative(1.3))
            .text_color(chrome.ink_1)
            .child(sidebar)
            .child(self.terminal_side(&chrome, cards_focused, home.as_deref(), cx));
        if let Some(overlay) = &self.overlay {
            root = root.child(self.overlay_view(overlay, &chrome, home.as_deref(), window, cx));
        }
        root
    }
}
fn main() -> anyhow::Result<()> {
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
        cx.bind_keys([
            gpui::KeyBinding::new("cmd-q", Quit, None),
            gpui::KeyBinding::new("cmd-,", OpenSettings, None),
            gpui::KeyBinding::new("cmd-h", Hide, None),
            gpui::KeyBinding::new("cmd-alt-h", HideOthers, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.set_menus([gpui::Menu::new("Shika").items([
            gpui::MenuItem::action("Settings...", OpenSettings),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Hide Shika", Hide),
            gpui::MenuItem::action("Hide others", HideOthers),
            gpui::MenuItem::action("Show all", ShowAll),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Quit Shika", Quit),
        ])]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
        let settings = core.settings();
        let start = settings.as_ref().map(|s| s.appearance).unwrap_or_default();
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
                    // The 540px column plus a terminal wide enough to read.
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
        cx.activate(true);
    });
    Ok(())
}

#[cfg(test)]
mod host_tests {
    use super::*;

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
