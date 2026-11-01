//! `begin_launch`, which starts every card: tasks, checkouts, Leads,
//! workers, and recoveries. It creates the card, starts the session off the
//! UI thread with live preparation progress, and finishes the card with
//! core's answer: started, cancelled, setup failed, or failed.
use super::*;

/// Who a launch belongs to, read from what it starts.
struct LaunchOwner {
    started_by: Option<String>,
    lead: Option<control::LeadState>,
    worker_token: Option<String>,
}

impl LaunchOwner {
    fn of(launch: &Launch) -> Self {
        let worker_token = match launch {
            Launch::Worker(options) => options.control.as_ref().map(|c| c.token.clone()),
            _ => None,
        };
        let (started_by, lead) = match launch {
            Launch::Worker(options) => (options.started_by.clone(), None),
            Launch::Lead(env) | Launch::LeadRestart { launch: env, .. } => {
                (None, Some(control::LeadState::new(env.token.clone())))
            }
            Launch::Task | Launch::Checkout | Launch::Recover { .. } => (None, None),
        };
        Self {
            started_by,
            lead,
            worker_token,
        }
    }
}

/// What the background thread needs to start the session in core.
struct SessionStart {
    core: Arc<Core>,
    project: String,
    preset: CliPreset,
    launch: Launch,
    chat: bool,
    shell_only: bool,
    control: PreparationControl,
    /// Where a Lead start leaves core's answer for the completion.
    started_lead: Arc<Mutex<Option<shika_core::LeadStart>>>,
}

impl SessionStart {
    /// Blocking: the core call for this launch, then the PTY binding.
    fn run(
        self,
        size: PtySize,
        sink: impl shika_core::PtySink,
        report: impl FnMut(PreparationEvent),
        state_bind: &Mutex<HostState>,
    ) -> Result<Session, shika_core::Error> {
        let Self {
            core,
            project,
            preset,
            launch,
            chat,
            shell_only,
            control,
            started_lead,
        } = self;
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
            Launch::Recover { path, resume } => {
                core.reopen_leftover(&path, &preset.id, resume, size, sink)
            }
            Launch::Checkout if shell_only => core.create_checkout_terminal(&project, size, sink),
            Launch::Checkout => core.create_checkout_session(&project, &preset.id, size, sink),
            Launch::Worker(options) => core.create_session_with_preparation(
                &project, &preset.id, size, sink, options, control, report,
            ),
            Launch::Task if shell_only => {
                core.create_terminal_with_preparation(&project, size, sink, control, report)
            }
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
            bind_host(&core, state_bind, pty);
        }
        result
    }
}

/// What the UI thread needs to finish a launch once core answers.
struct LaunchCompletion {
    /// The card's agent state: finds the card, which may have moved or gone.
    state: Arc<Mutex<HostState>>,
    finished: Launch,
    reply: Option<control::Responder>,
    /// What core says about a Lead start.
    lead_start: Arc<Mutex<Option<shika_core::LeadStart>>>,
    configured: bool,
    author: bool,
    holds_busy: bool,
}

/// Preparation progress from core, painted into the card's terminal.
fn preparation_reporter(
    progress_state: Arc<Mutex<HostState>>,
    progress_terminal: Terminal,
    shell_only: bool,
) -> impl FnMut(PreparationEvent) {
    move |event| match event {
        PreparationEvent::Stage(stage) => {
            lock(&progress_state).preparation_stage = stage;
        }
        PreparationEvent::StartingAgent => {
            let mut host = lock(&progress_state);
            host.agent_starting = true;
            host.preparation_stage = if shell_only {
                "Starting terminal..."
            } else {
                "Starting agent..."
            }
            .into();
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
    }
}

/// Waits for the pane's first measured grid. `None` when the app is gone.
async fn wait_for_measured_grid(
    state: &Mutex<HostState>,
    this: &gpui::WeakEntity<Shika>,
    cx: &gpui::AsyncWindowContext,
) -> Option<TerminalSize> {
    loop {
        if let Some(size) = lock(state).measured {
            break Some(size);
        }
        cx.background_executor()
            .timer(Duration::from_millis(16))
            .await;
        this.upgrade()?;
    }
}

/// The card a launch shows while it starts.
fn launch_card(
    project: &str,
    preset: &CliPreset,
    pane: Pane,
    chat: bool,
    launch: &Launch,
    owner: LaunchOwner,
    control: PreparationControl,
) -> Card {
    let is_lead = owner.lead.is_some();
    Card {
        session: None,
        project: project.into(),
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
        started_by: owner.started_by,
        lead: owner.lead,
        worker_token: owner.worker_token,
        report: None,
        discard: false,
        launch_control: Some(control),
        launch_error: None,
        stage: match launch {
            Launch::Checkout if preset.id == "terminal" => "Starting terminal...",
            Launch::Checkout => "Starting agent...",
            _ => "Creating worktree...",
        }
        .into(),
        closing: false,
        next_agent: None,
        usage_exit_seen: false,
        lead_run: None,
    }
}

/// The started card's host: preparation is over, and a Worker's or fresh
/// Cursor Lead's launch prompt is seeded.
fn mark_host_started(
    state: &Mutex<HostState>,
    finished: &Launch,
    lead_start: &Mutex<Option<shika_core::LeadStart>>,
) {
    let mut host = lock(state);
    host.preparing = false;
    // Nobody types a launch prompt, so it counts as
    // submitted now: the turn starts with the CLI.
    let now = Instant::now();
    match finished {
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
        } if lock(lead_start)
            .as_ref()
            .is_some_and(|start| start.bootstrap_sent) =>
        {
            host.seed_launch_prompt(prompt, false, now)
        }
        _ => {}
    }
}

impl Shika {
    fn stop_cancelled_launch(&mut self, id: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move { core.cancel_session_start(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.leftovers.entries = this.core.leftovers_list().unwrap_or_default();
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
    pub(crate) fn begin_launch(
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
        let (launch, retried) = self.take_retried_launch(retry, launch);
        let author = retried || !matches!(launch, Launch::Worker(_) | Launch::LeadRestart { .. });
        let owner = LaunchOwner::of(&launch);
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
        let shell_only = preset.id == "terminal";
        let chat = owner.lead.is_some() && preset.id == "claude";
        if chat {
            self.ensure_lead_chat(&project, window, cx)
                .update(cx, |chat, cx| {
                    chat.live = false;
                    chat.starting = true;
                    cx.notify();
                });
        }
        let pane = self.launch_pane(shell_only, window, cx);
        let terminal = pane.terminal.clone();
        let state = pane.state.clone();
        {
            let mut host = lock(&state);
            host.preparing = configured;
            if shell_only || owner.lead.is_some() || matches!(launch, Launch::Recover { .. }) {
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
        self.cards.push(launch_card(
            &project,
            &preset,
            pane,
            chat,
            &launch,
            owner,
            control.clone(),
        ));
        if author {
            self.selection.set(Some(Selection::Card(index)));
            // The new card's terminal takes the keys now. It drops typing
            // during copy and setup, but `j` can no longer move the selection
            // away while it runs, so completion focus still lands.
            self.focus_pane(window, cx);
        }
        cx.notify();
        let core = self.core.clone();
        let finished = launch.clone();
        let lead_start: Arc<Mutex<Option<shika_core::LeadStart>>> = Arc::default();
        let started_lead = lead_start.clone();
        let start = SessionStart {
            core,
            project,
            preset,
            launch,
            chat,
            shell_only,
            control,
            started_lead,
        };
        let done = LaunchCompletion {
            state,
            finished,
            reply,
            lead_start,
            configured,
            author,
            holds_busy,
        };
        self.spawn_session_start(start, done, terminal, window, cx);
    }

    /// A Retry repeats the failed card's launch and replaces that card.
    fn take_retried_launch(
        &mut self,
        retry: Option<Arc<Mutex<HostState>>>,
        launch: Launch,
    ) -> (Launch, bool) {
        if let Some(retry) = retry
            && let Some(i) = self
                .cards
                .iter()
                .position(|c| Arc::ptr_eq(&c.agent.state, &retry))
        {
            let launch = self.cards[i].launch.clone();
            self.remove_card(i);
            return (launch, true);
        }
        (launch, false)
    }

    /// The new card's agent pane, painted like every other terminal.
    fn launch_pane(&self, shell_only: bool, window: &mut Window, cx: &mut Context<Self>) -> Pane {
        let opacity = self.terminal_opacity();
        Pane::new(
            self.core.clone(),
            !shell_only,
            opacity,
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        )
    }

    /// Waits for the pane's grid, starts the session off the UI thread, and
    /// finishes the card. A started session nobody is left to receive is
    /// cancelled.
    fn spawn_session_start(
        &mut self,
        start: SessionStart,
        done: LaunchCompletion,
        terminal: Terminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let Some(measured) = wait_for_measured_grid(&done.state, &this, cx).await else {
                return;
            };
            let sink = agent_sink(terminal.clone(), done.state.clone());
            let state_bind = done.state.clone();
            let progress_state = done.state.clone();
            let progress_terminal = terminal.clone();
            let callback_core = start.core.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let size = PtySize::new(measured.rows, measured.cols);
                    let report =
                        preparation_reporter(progress_state, progress_terminal, start.shell_only);
                    start.run(size, sink, report, &state_bind)
                })
                .await;
            let orphan = result.as_ref().ok().map(|s| s.id.clone());
            let completion_core = callback_core.clone();
            let updated = this.update_in(cx, |this, window, cx| {
                this.finish_launch(&done, result, &completion_core, window, cx);
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

    /// Core's answer, back on the UI thread. A card closed meanwhile cancels
    /// the start it no longer shows.
    fn finish_launch(
        &mut self,
        done: &LaunchCompletion,
        result: Result<Session, shika_core::Error>,
        completion_core: &Arc<Core>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if done.holds_busy {
            self.busy = false;
        }
        let Some(index) = self
            .cards
            .iter()
            .position(|c| Arc::ptr_eq(&c.agent.state, &done.state))
        else {
            control::answer(
                &done.reply,
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
        self.cards[index].creating = false;
        if let Some(chat) = self
            .leads
            .chats
            .get(&self.cards[index].project)
            .filter(|_| self.cards[index].chat)
        {
            chat.update(cx, |chat, cx| {
                chat.starting = false;
                cx.notify();
            });
        }
        match result {
            Ok(session)
                if !self.cards[index]
                    .launch_control
                    .as_ref()
                    .is_some_and(|c| c.is_cancelled()) =>
            {
                self.launch_started(index, session, done, window, cx);
            }
            Ok(session) => self.launch_cancelled(index, session, &done.reply, cx),
            Err(error) if done.configured => self.launch_setup_failed(index, error, done),
            Err(error) => self.launch_failed(index, error, done),
        }
        self.leftovers.entries = self.core.leftovers_list().unwrap_or_default();
        cx.notify();
    }

    /// The CLI started and nobody cancelled: the card goes live.
    fn launch_started(
        &mut self,
        index: usize,
        session: Session,
        done: &LaunchCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        mark_host_started(&done.state, &done.finished, &done.lead_start);
        if let Some(start) = lock(&done.lead_start).take() {
            self.cards[index].lead_run = Some(LeadRun::new(&start, Instant::now()));
            // The saved Lead is on disk now; a stop leaves
            // its resting row.
            self.reload_projects();
        }
        if self.cards[index].chat {
            let project = self.cards[index].project.clone();
            if let Some(chat) = self.leads.chats.get(&project) {
                chat.update(cx, |chat, cx| {
                    chat.live = true;
                    chat.busy = false;
                    cx.notify();
                });
            }
        }
        let recovered = matches!(done.finished, Launch::Recover { .. });
        let recovered_id = session.id.clone();
        if recovered || session.is_terminal() {
            self.cards[index].title = model::card_title(&session.title);
            self.cards[index].named = true;
            self.cards[index].title_watch.finish();
        }
        self.cards[index].session = Some(session);
        self.cards[index].launch_control = None;
        if recovered {
            self.fetch_diff_stat(recovered_id, cx);
        }
        if self.started_card_takes_focus(index, &done.finished, window) {
            self.focus_terminal(window, cx);
        }
        control::answer(&done.reply, self.started_reply(index));
    }

    /// Completion never steals focus from another card or an overlay opened
    /// while installation was running, nor from the cards the author
    /// returned to with Ctrl+Q or the Changes panel.
    fn started_card_takes_focus(&self, index: usize, finished: &Launch, window: &Window) -> bool {
        *self.selection == Some(Selection::Card(index))
            && self.overlay.is_none()
            && !matches!(finished, Launch::LeadRestart { focus: false, .. })
            && ((!self.focus.is_focused(window)
                && !(self.changes.open && self.changes.focus.is_focused(window)))
                || matches!(finished, Launch::LeadRestart { focus: true, .. }))
    }

    /// The author cancelled while the start ran: stop what it started.
    fn launch_cancelled(
        &mut self,
        index: usize,
        session: Session,
        reply: &Option<control::Responder>,
        cx: &mut Context<Self>,
    ) {
        self.stop_cancelled_launch(session.id, cx);
        self.cards[index].launch_error = Some(
            if self.cards[index].launch_preset == "terminal" && self.cards[index].is_checkout() {
                "Terminal start was cancelled."
            } else if self.cards[index].is_checkout() {
                "Agent start was cancelled."
            } else {
                "Worktree preparation was cancelled."
            }
            .into(),
        );
        control::answer(
            reply,
            control::failure_reply(&shika_core::Error::PreparationCancelled),
        );
    }

    /// Configured preparation failed: the card stays with its output and
    /// offers Retry.
    fn launch_setup_failed(
        &mut self,
        index: usize,
        error: shika_core::Error,
        done: &LaunchCompletion,
    ) {
        control::answer(&done.reply, control::failure_reply(&error));
        lock(&done.state).agent_starting = false;
        self.cards[index]
            .agent
            .terminal
            .feed(format!("\r\n{error}\r\nRetry creates a fresh worktree.\r\n").as_bytes());
        self.cards[index].launch_error = Some(error.to_string());
        self.cards[index].stage = "Setup failed".into();
    }

    /// The start failed without setup: the card goes away with a toast.
    fn launch_failed(&mut self, index: usize, error: shika_core::Error, done: &LaunchCompletion) {
        control::answer(&done.reply, control::failure_reply(&error));
        if done.author {
            self.remove_card(index);
        } else {
            // Removing a card under an open dialog would
            // shift the positions it holds; the tick does it.
            self.cards[index].discard = true;
        }
        self.message(error.to_string());
    }
}
