mod appearance;
mod model;
mod notifications;

use appearance::tint;
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder, px, rgb, size,
};
use model::{PromptCapture, Status, TitleWatch, visible_indices};
use notifications::Notifications;
use shika_core::{
    Appearance, CliCatalog, Core, JournalEntry, Project, PtyEvent, PtyId, PtySize, Session,
    SessionGitState, Settings, Translucency,
};
use shika_terminal::{
    Palette, PtyHost, Terminal, TerminalConfig, TerminalOptions, TerminalSize, TerminalView,
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
    fn write(&self, bytes: &[u8]) {
        let mut s = lock(&self.state);
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
        if let Some(pty) = s.pty {
            let _ = self.core.resize(pty, PtySize::new(size.rows, size.cols));
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
                TerminalConfig::default(),
                Palette::shika(),
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
    last_ready: Option<Instant>,
    creating: bool,
    title_watch: TitleWatch,
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
    /// `row` is the selected setting: opacity, blur, translucency, then the
    /// branch prefix. `edit` holds digits typed into the selected number, or
    /// the prefix being typed, not yet applied.
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
        let branch_prefix = shika_core::normalize_branch_prefix(&settings.branch_prefix);
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
        let opacity = appearance::terminal_alpha(&self.appearance);
        let pane = Pane::new(self.core.clone(), true, opacity, window, cx);
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
            last_ready: None,
            creating: true,
            title_watch: TitleWatch::default(),
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
                                lock(&output_state).last_output = Some(Instant::now());
                            }
                            PtyEvent::Exit(exit) => {
                                sink_terminal.feed(
                                    format!("\r\n[process exited with code {}]\r\n", exit.code)
                                        .as_bytes(),
                                );
                                lock(&output_state).exited = true;
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
        let opacity = appearance::terminal_alpha(&self.appearance);
        let pane = Pane::new(self.core.clone(), false, opacity, window, cx);
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
        if now.duration_since(self.clock) >= Duration::from_secs(1) {
            self.clock = now;
            changed = self.cards.iter().any(|c| c.status == Status::Working);
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
                let latest = state.last_output.unwrap_or(card.since);
                if latest > card.since && card.status == Status::Ready {
                    card.status = Status::Working;
                    card.since = latest;
                    changed = true;
                }
                if card.status == Status::Working
                    && (state.exited
                        || now.duration_since(latest.max(card.since)) >= Duration::from_secs(2))
                {
                    card.status = Status::Ready;
                    card.since = now;
                    changed = true;
                    if card
                        .last_ready
                        .is_none_or(|last| now.duration_since(last) > Duration::from_secs(2))
                        && let Some(session) = &card.session
                    {
                        let project = self
                            .projects
                            .iter()
                            .find(|p| p.id == card.project)
                            .map(|p| p.name.as_str())
                            .unwrap_or("Shika");
                        self.notifications.post(&session.id, project, &card.title);
                    }
                    card.last_ready = Some(now);
                }
            } else if state.exited {
                card.status = Status::Ready;
                card.since = now;
                changed = true;
                if let Some(session) = &card.session {
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
                        .last_output
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(2)))
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
        let Some(value) = edit.take().and_then(|text| text.parse::<i64>().ok()) else {
            return;
        };
        let next = match row {
            0 => self.appearance.with_opacity(value),
            _ => self.appearance.with_blur(value),
        };
        self.set_appearance(next, window, cx);
    }
    fn set_appearance(&mut self, next: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.appearance {
            return;
        }
        self.appearance = next;
        appearance::apply(&next, window);
        let opacity = appearance::terminal_alpha(&next);
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shell.as_ref()) {
                pane.view
                    .update(cx, |view, cx| view.set_background_opacity(opacity, cx));
            }
        }
        self.save_settings();
        cx.notify();
    }
    fn save_settings(&mut self) {
        let settings = Settings {
            appearance: self.appearance,
            branch_prefix: self.branch_prefix.clone(),
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
                    let number_row = at < 2;
                    match (edit.as_mut(), key) {
                        (Some(text), _) if digit => {
                            if text.len() < 3 {
                                text.push_str(key)
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
/// Settings rows: opacity, blur, translucency, then the branch prefix.
const SETTING_ROWS: usize = 4;
const PREFIX_ROW: usize = 3;
impl Shika {
    /// The branch prefix row: a text field. Enter or a click starts typing.
    fn prefix_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row, edit }) => (
                *row == PREFIX_ROW,
                edit.as_deref().filter(|_| *row == PREFIX_ROW),
            ),
            _ => (false, None),
        };
        let text = edit.unwrap_or(&self.branch_prefix).to_string();
        let empty = text.is_empty();
        let field = div()
            .id("setting-prefix-value")
            .w(px(168.))
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded(px(6.))
            .bg(rgb(0xFFFFFF))
            .border_1()
            .border_color(rgb(if edit.is_some() { 0x2F332C } else { 0xCFD3C7 }))
            .font_family("JetBrains Mono")
            .cursor_text()
            .when(!empty, |d| d.child(text))
            .when(edit.is_some(), |d| {
                d.child(div().w(px(1.)).h(px(14.)).bg(rgb(0x2F332C)))
            })
            .when(empty && edit.is_none(), |d| {
                d.child(div().text_color(rgb(0x9EA296)).child("none"))
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
        div()
            .p_2()
            .rounded(px(8.))
            .when(selected, |d| d.bg(rgb(0xE8EBE2)))
            .flex()
            .items_center()
            .justify_between()
            .child("Branch prefix")
            .child(field)
    }
    /// A settings row: minus, a number field that takes typed digits, plus.
    fn setting_row(
        &self,
        row: usize,
        label: &'static str,
        value: u8,
        unit: &'static str,
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
        let field = div()
            .id(SharedString::from(format!("setting-{row}-value")))
            .w(px(64.))
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(2.))
            .rounded(px(6.))
            .bg(rgb(0xFFFFFF))
            .border_1()
            .border_color(rgb(if edit.is_some() { 0x2F332C } else { 0xCFD3C7 }))
            .font_family("JetBrains Mono")
            .cursor_text()
            .child(
                div()
                    .when(edit == Some(""), |d| d.text_color(rgb(0x9EA296)))
                    .child(text),
            )
            .when(edit.is_some(), |d| {
                d.child(div().w(px(1.)).h(px(14.)).bg(rgb(0x2F332C)))
            })
            .child(div().text_color(rgb(0x9EA296)).child(unit))
            .on_click(
                cx.listener(move |this, _, window, cx| this.edit_setting(row, "", window, cx)),
            );
        div()
            .p_2()
            .rounded(px(8.))
            .when(selected, |d| d.bg(rgb(0xE8EBE2)))
            .flex()
            .items_center()
            .justify_between()
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button(SharedString::from(format!("setting-{row}-less")), "-")
                            .on_click(step(-1)),
                    )
                    .child(field)
                    .child(
                        button(SharedString::from(format!("setting-{row}-more")), "+")
                            .on_click(step(1)),
                    ),
            )
    }

    /// The strip above the sidebar and the agent header. The system title is
    /// hidden so this bar can hold the settings icon. Its fill uses the same
    /// opacity as the sidebar, so the window blur shows through it.
    fn title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inset = if window.is_fullscreen() || window.is_simple_fullscreen() {
            px(16.)
        } else {
            px(TITLE_BAR_INSET)
        };
        div()
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .bg(tint(0xF1F2EC, appearance::sidebar_alpha(&self.appearance)))
            .border_b_1()
            .border_color(rgb(0xDADDD3))
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .pl(inset)
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
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Shika")),
            )
            .child(
                div()
                    .id("settings")
                    .mr(px(8.))
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0xE3E6DD)))
                    .tooltip(|_, cx| cx.new(|_| SettingsHint).into())
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                    .child(
                        gpui::svg()
                            .data(SETTINGS_ICON)
                            .size(px(15.))
                            .text_color(rgb(0x3C4038)),
                    ),
            )
    }
}
impl Focusable for Shika {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
/// Clears the traffic lights on macOS 26 before the title.
const TITLE_BAR_INSET: f32 = 78.;
/// Matches the traffic-light container: button height plus 9px above and below.
const TITLE_BAR_HEIGHT: f32 = 34.;

/// Filled gear. Drawn as an alpha mask and tinted by the element's text color.
const SETTINGS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-rule="evenodd" d="M11.078 2.25c-.917 0-1.699.663-1.85 1.567L9.05 4.889c-.02.12-.115.26-.297.348a7.493 7.493 0 0 0-.986.57c-.166.115-.334.126-.45.083L6.3 5.508a1.875 1.875 0 0 0-2.282.819l-.922 1.597a1.875 1.875 0 0 0 .432 2.385l.84.692c.095.078.17.229.154.43a7.598 7.598 0 0 0 0 1.139c.015.2-.059.352-.153.43l-.841.692a1.875 1.875 0 0 0-.432 2.385l.922 1.597a1.875 1.875 0 0 0 2.282.818l1.019-.382c.115-.043.283-.031.45.082.312.214.641.405.985.57.182.088.277.228.297.35l.178 1.071c.151.904.933 1.567 1.85 1.567h1.844c.916 0 1.699-.663 1.85-1.567l.178-1.072c.02-.12.114-.26.297-.349.344-.165.673-.356.985-.57.167-.114.335-.125.45-.082l1.02.382a1.875 1.875 0 0 0 2.28-.819l.923-1.597a1.875 1.875 0 0 0-.432-2.385l-.84-.692c-.095-.078-.17-.229-.154-.43a7.614 7.614 0 0 0 0-1.139c-.016-.2.059-.352.153-.43l.84-.692c.708-.582.891-1.59.433-2.385l-.922-1.597a1.875 1.875 0 0 0-2.282-.818l-1.02.382c-.114.043-.282.031-.449-.083a7.49 7.49 0 0 0-.985-.57c-.183-.087-.277-.227-.297-.348l-.179-1.072a1.875 1.875 0 0 0-1.85-1.567h-1.843ZM12 15.75a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5Z"/></svg>"##;

struct SettingsHint;

impl Render for SettingsHint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(rgb(0x252823))
            .text_color(rgb(0xE9ECE3))
            .text_size(px(12.))
            .font_family(".AppleSystemUIFont")
            .child("Settings  \u{2318},")
    }
}

fn button(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded(px(8.))
        .bg(rgb(0xE3E6DD))
        .cursor_pointer()
        .child(text.into())
}
impl Render for Shika {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards_focused = self.focus.is_focused(window);
        let icon = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            include_bytes!("../../../assets/macos/shika-app-icon-256.png").to_vec(),
        ));
        let working = self
            .cards
            .iter()
            .filter(|c| c.status == Status::Working)
            .count();
        let ready = self
            .cards
            .iter()
            .filter(|c| c.status == Status::Ready)
            .count();
        let mut groups = div()
            .id("projects")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_3()
            .py_3()
            .flex()
            .flex_col()
            .gap_4();
        for project in &self.projects {
            let id = project.id.clone();
            let selected = self.selection == Some(Selection::Project(id.clone()));
            let project_id = id.clone();
            let remove_id = id.clone();
            let mut group = div().flex().flex_col().gap_2().child(
                div()
                    .id(SharedString::from(format!("project-{id}")))
                    .px_2()
                    .py_2()
                    .rounded(px(6.))
                    .when(selected, |d| d.bg(rgb(0xE8EBE2)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.busy || this.overlay.is_some() {
                            return;
                        }
                        this.selection = Some(Selection::Project(project_id.clone()));
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(project.name.clone())
                            .child(
                                div()
                                    .id(SharedString::from(format!("forget-{id}")))
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::NORMAL)
                                    .text_color(rgb(0x9EA296))
                                    .child("Remove")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if !this.busy && this.overlay.is_none() {
                                            this.overlay =
                                                Some(Overlay::RemoveProject(remove_id.clone()));
                                            window.focus(&this.focus, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(0x9EA296))
                            .truncate()
                            .child(project.path.display().to_string()),
                    ),
            );
            let indices = self.sorted_cards(&id);
            let selected_index = self
                .selected_card()
                .and_then(|i| indices.iter().position(|j| *j == i));
            let shown = visible_indices(indices.len(), selected_index);
            let visible = shown.len();
            if indices.is_empty() {
                group = group.child(
                    div()
                        .p_3()
                        .border_1()
                        .border_color(rgb(0xCFD3C7))
                        .rounded(px(10.))
                        .text_color(rgb(0x9EA296))
                        .child("No agents. Press n to start."),
                );
            }
            for at in shown {
                let i = indices[at];
                let card = &self.cards[i];
                let selected = self.selected_card() == Some(i);
                let color = match card.status {
                    Status::Ready => 0x399A62,
                    Status::Working => 0x5A8AB3,
                    Status::Waiting => 0xA3A79B,
                };
                group = group.child(
                    div()
                        .id(SharedString::from(format!("card-{i}")))
                        .p_3()
                        .overflow_hidden()
                        .rounded(px(10.))
                        .bg(rgb(if selected {
                            0xFFFFFF
                        } else if card.status == Status::Ready {
                            0xF1F8F0
                        } else {
                            0xF8F9F5
                        }))
                        .border_1()
                        .border_color(rgb(if selected && cards_focused {
                            0x2F332C
                        } else {
                            0xDADDD3
                        }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.busy || this.overlay.is_some() {
                                return;
                            }
                            this.selection = Some(Selection::Card(i));
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .items_start()
                                .child(div().mt_1().size(px(7.)).rounded_full().bg(rgb(color)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(card.title.clone()),
                                )
                                .when(card.status == Status::Working, |row| {
                                    row.child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(rgb(0x9EA296))
                                            .font_family("JetBrains Mono")
                                            .child(format!(
                                                "{}s",
                                                card.since.elapsed().as_secs()
                                            )),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_size(px(10.5))
                                .text_color(rgb(0x6C7166))
                                .flex()
                                .gap_1()
                                .min_w_0()
                                .child(format!("{} · {}", card.preset, card.status.label()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family("JetBrains Mono")
                                        .text_size(px(9.))
                                        .child(
                                            card.session
                                                .as_ref()
                                                .map(|s| format!("· {}", s.branch))
                                                .unwrap_or_default(),
                                        ),
                                ),
                        )
                        .when(selected && cards_focused, |d| {
                            d.child(
                                div()
                                    .mt_2()
                                    .text_size(px(10.))
                                    .text_color(rgb(0x9EA296))
                                    .child("enter terminal   g shell   c close"),
                            )
                        }),
                );
            }
            if indices.len() > visible {
                group = group.child(
                    div()
                        .px_2()
                        .text_size(px(11.))
                        .text_color(rgb(0x6C7166))
                        .child(format!("+ {} more · j to reach", indices.len() - visible)),
                );
            }
            groups = groups.child(group);
        }
        let sidebar = div()
            .w(px(280.))
            .min_w(px(280.))
            .max_w(px(280.))
            .overflow_hidden()
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(tint(0xF1F2EC, appearance::sidebar_alpha(&self.appearance)))
            .border_r_1()
            .border_color(rgb(0xDADDD3))
            .child(
                div()
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(0xDADDD3))
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Shika"))
                    .child(
                        button("new", "New agent  n")
                            .on_click(cx.listener(|this, _, window, cx| this.picker(window, cx))),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .py_4()
                    .text_size(px(9.))
                    .text_color(rgb(0x6C7166))
                    .child(format!(
                        "{} agents · {working} working · 0 asking · {ready} ready",
                        self.cards.len()
                    )),
            )
            .child(groups)
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(0xDADDD3))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        button("add", "Add project  a")
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .when(!self.leftovers.is_empty(), |d| {
                        d.child(
                            button(
                                "leftovers",
                                format!("Leftover worktrees ({})", self.leftovers.len()),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.overlay = Some(Overlay::Leftovers);
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(0x6C7166))
                            .child("j / k move   enter terminal   ctrl+q cards"),
                    ),
            );
        // Each child paints its own background, so a translucent terminal is
        // not stacked over a second translucent fill.
        let terminal_alpha = appearance::terminal_alpha(&self.appearance);
        let mut right = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .text_color(rgb(0xD5D9CF));
        if let Some(i) = self.selected_card() {
            let card = &self.cards[i];
            let shell = card.show_shell;
            let path = card
                .session
                .as_ref()
                .map(|s| s.worktree.display().to_string())
                .unwrap_or("Creating worktree...".into());
            right = right.child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(tint(0x181A17, terminal_alpha))
                    .border_b_1()
                    .border_color(rgb(0x262924))
                    .child(
                        div()
                            .id("agent")
                            .px_3()
                            .py_1()
                            .rounded(px(6.))
                            .bg(rgb(if shell { 0x20231F } else { 0x353932 }))
                            .cursor_pointer()
                            .child("Agent")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.busy || this.overlay.is_some() {
                                    return;
                                }
                                if let Some(i) = this.selected_card() {
                                    this.cards[i].show_shell = false;
                                }
                                this.focus_terminal(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("shell")
                            .px_3()
                            .py_1()
                            .rounded(px(6.))
                            .bg(rgb(if shell { 0x353932 } else { 0x20231F }))
                            .cursor_pointer()
                            .child("Shell")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(i) = this.selected_card() {
                                    if this.cards[i].show_shell {
                                        this.focus_terminal(window, cx);
                                    } else {
                                        this.toggle(true, window, cx);
                                    }
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family("JetBrains Mono")
                            .text_size(px(10.5))
                            .text_color(rgb(0x757A6E))
                            .child(path),
                    )
                    .child(
                        div()
                            .id("close")
                            .px_2()
                            .cursor_pointer()
                            .text_color(rgb(0x878C80))
                            .child(if cards_focused {
                                "Close  c"
                            } else {
                                "ctrl+q cards · Close"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            );
            let pane = if shell {
                card.shell.as_ref().unwrap_or(&card.agent)
            } else {
                &card.agent
            };
            right = right.child(div().flex_1().min_h_0().child(pane.view.clone()));
        } else {
            right = right.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .bg(tint(0x131512, terminal_alpha))
                    .text_color(rgb(0x757A6E))
                    .child(
                        div()
                            .text_size(px(20.))
                            .text_color(rgb(0xC5CABE))
                            .child("Shika"),
                    )
                    .child(gpui::img(icon).size(px(64.)).rounded(px(14.)))
                    .child("Select a card to open its terminal")
                    .child("n new agent   a add project"),
            );
        }
        let mut root = div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(".AppleSystemUIFont")
            .text_size(px(12.))
            .text_color(rgb(0x262824))
            .child(self.title_bar(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(sidebar)
                    .child(right),
            );
        if let Some((text, _)) = &self.toast {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(20.))
                    .left(px(300.))
                    .right(px(20.))
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(0x252823))
                    .text_color(rgb(0xE9ECE3))
                    .child(text.clone()),
            );
        }
        if let Some(overlay) = &self.overlay {
            let mut panel = div()
                .id("overlay-panel")
                .max_h((window.viewport_size().height - px(80.)).max(px(120.)))
                .overflow_y_scroll()
                .w(px(460.))
                .p_5()
                .rounded(px(12.))
                .bg(rgb(0xFAFAF7))
                .flex()
                .flex_col()
                .gap_3();
            match overlay {
                Overlay::Picker { project, index } => {
                    let name = self
                        .projects
                        .iter()
                        .find(|p| &p.id == project)
                        .map(|p| p.name.as_str())
                        .unwrap_or("");
                    panel = panel.child(
                        div()
                            .text_size(px(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("New agent in {name}")),
                    );
                    if let Some(catalog) = &self.catalog {
                        for (i, preset) in catalog.presets.iter().enumerate() {
                            panel = panel.child(
                                div()
                                    .id(SharedString::from(format!("preset-{i}")))
                                    .p_3()
                                    .rounded(px(8.))
                                    .bg(rgb(if i == *index { 0xE8EBE2 } else { 0xFAFAF7 }))
                                    .text_color(rgb(if preset.found() {
                                        0x262824
                                    } else {
                                        0x9EA296
                                    }))
                                    .cursor_pointer()
                                    .child(format!(
                                        "{}  {}{}",
                                        i + 1,
                                        preset.name,
                                        if preset.found() {
                                            ""
                                        } else {
                                            " · not found on PATH"
                                        }
                                    ))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(Overlay::Picker { index, .. }) =
                                            &mut this.overlay
                                        {
                                            *index = i;
                                        }
                                        this.launch(window, cx);
                                    })),
                            );
                        }
                    } else {
                        panel = panel.child("Resolving login-shell PATH...");
                    }
                    panel = panel.child("j / k choose   enter start   tab project   esc cancel");
                }
                Overlay::Close { index, state } => {
                    panel = panel.child(
                        div()
                            .text_size(px(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("Close “{}”?", self.cards[*index].title)),
                    );
                    if state.agent_working {
                        panel = panel.child("The agent is still working.");
                    }
                    if state.dirty {
                        panel = panel.child("The worktree has uncommitted changes. Commit in the shell before pushing. Shika does not commit.");
                    }
                    if state.unpushed {
                        panel = panel.child("The branch has commits that are not on the remote.");
                    }
                    panel = panel.child(
                        "Discard stops the session and deletes the worktree and local branch.",
                    );
                    let i = *index;
                    panel = panel.child(button("discard", "Discard changes  d").on_click(
                        cx.listener(move |this, _, window, cx| this.finish_close(i, 1, window, cx)),
                    ));
                    if state.can_push() {
                        panel =
                            panel.child(button("push", "Push changes  p").on_click(cx.listener(
                                move |this, _, window, cx| this.finish_close(i, 2, window, cx),
                            )));
                    }
                }
                Overlay::Leftovers => {
                    panel = panel
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Leftover worktrees"),
                        )
                        .child("These sessions ended when Shika quit. Their work remains on disk.");
                    panel = panel.child("j / k choose   d remove   esc keep worktrees");
                    for (i, entry) in self.leftovers.iter().enumerate() {
                        panel = panel.child(
                            div()
                                .p_2()
                                .rounded(px(8.))
                                .when(i == self.leftover_selected, |d| d.bg(rgb(0xE8EBE2)))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_family("JetBrains Mono")
                                        .text_size(px(11.))
                                        .child(entry.branch.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(rgb(0x6C7166))
                                        .child(entry.path.display().to_string()),
                                )
                                .child(
                                    button(
                                        SharedString::from(format!("remove-{i}")),
                                        "Remove worktree",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.overlay = Some(Overlay::RemoveLeftover(i));
                                            cx.notify();
                                        },
                                    )),
                                ),
                        );
                    }
                }
                Overlay::RemoveProject(id) => {
                    let id = id.clone();
                    panel = panel.child(div().text_size(px(16.)).child("Remove project?" )).child("Stops its sessions and forgets this project. Worktrees remain on disk in the leftovers list.").child(button("remove-project", "Remove project  r").on_click(cx.listener(move |this, _, _, cx| this.remove_project(id.clone(), cx))));
                }
                Overlay::RemoveLeftover(i) => {
                    let i = *i;
                    panel = panel
                        .child(div().text_size(px(16.)).child("Remove leftover worktree?"))
                        .child(self.leftovers[i].path.display().to_string())
                        .child("This deletes any uncommitted work and the local branch.")
                        .child(button("remove-confirm", "Discard worktree  d").on_click(
                            cx.listener(move |this, _, _, cx| this.remove_leftover(i, cx)),
                        ));
                }
                Overlay::Settings { row, edit } => {
                    let a = &self.appearance;
                    let both = a.translucency == Translucency::SidebarAndTerminal;
                    let choice = |id: &'static str, text: &'static str, on: bool| {
                        button(id, text).when(on, |d| d.bg(rgb(0x2F332C)).text_color(rgb(0xF2F5EC)))
                    };
                    panel = panel
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Settings"),
                        )
                        .child(self.setting_row(0, "Background opacity", a.opacity, "%", cx))
                        .child(self.setting_row(1, "Background blur", a.blur, "", cx))
                        .child(
                            div()
                                .p_2()
                                .rounded(px(8.))
                                .when(*row == 2, |d| d.bg(rgb(0xE8EBE2)))
                                .flex()
                                .items_center()
                                .justify_between()
                                .child("Apply to")
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            choice("translucent-sidebar", "Sidebar", !both)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.step_setting(2, -1, window, cx)
                                                })),
                                        )
                                        .child(
                                            choice(
                                                "translucent-both",
                                                "Sidebar and terminal",
                                                both,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.step_setting(2, 1, window, cx)
                                                }),
                                            ),
                                        ),
                                ),
                        )
                        .child(self.prefix_row(cx))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(0x6C7166))
                                .child(
                                    "Opacity 0 to 100%. Blur radius 0 to 255, shown when opacity is below 100%. The prefix starts each new branch name, like hieu/.",
                                ),
                        )
                        .child(match (edit.is_some(), *row == PREFIX_ROW) {
                            (true, true) => "type a prefix   enter apply   esc cancel",
                            (true, false) => "type a number   enter apply   esc cancel",
                            (false, true) => "j / k choose   enter edit   esc done",
                            (false, false) => {
                                "j / k choose   h / l change   type a number   esc done"
                            }
                        });
                }
            }
            let settings = matches!(overlay, Overlay::Settings { .. });
            panel = panel.child(
                button(
                    "cancel",
                    if self.busy {
                        "Working..."
                    } else if settings {
                        "Done  esc"
                    } else {
                        "Cancel  esc"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if !this.busy {
                        this.cancel_overlay(window, cx);
                    }
                })),
            );
            root = root.child(
                div()
                    .absolute()
                    .occlude()
                    .inset_0()
                    // Settings leaves the window undimmed, so it is the preview.
                    .when(!settings, |d| d.bg(gpui::rgba(0x10120F57)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(panel),
            );
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
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let settings = core.settings();
        let start = settings.as_ref().map(|s| s.appearance).unwrap_or_default();
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Shika".into()),
                        appears_transparent: true,
                        // Centers the traffic lights in the 34px bar drawn below.
                        traffic_light_position: Some(gpui::point(px(9.), px(9.))),
                    }),
                    // The settings icon lives in the title bar, so clicks there
                    // have to reach the app. The bar starts the drag itself.
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
