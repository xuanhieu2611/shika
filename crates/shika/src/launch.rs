//! New: the CLI picker, worktree setup approval and retry, and `begin_launch`,
//! which starts every card: tasks, checkouts, Leads, workers, and recoveries.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerMode {
    Task,
    Lead,
    Checkout,
}

impl PickerMode {
    pub(super) fn choices(self, mut agents: Vec<CliPreset>) -> Vec<CliPreset> {
        if self == Self::Task {
            agents.push(shika_core::terminal_choice());
        }
        agents
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Task => "New agent",
            Self::Lead => "New Lead",
            Self::Checkout => "In checkout",
        }
    }
}

impl Shika {
    pub(super) fn picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if let Some(project) = self.project_id() {
            self.prefetch_base(project.clone(), cx);
            self.overlay_return_focus = window.focused(cx);
            self.overlay = Some(Overlay::Picker {
                project,
                index: 0,
                mode: PickerMode::Task,
            });
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            self.add_project(window, cx);
        }
    }

    pub(super) fn launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.catalog.is_none() {
            return;
        }
        let (project, index, mode) = match &self.overlay {
            Some(Overlay::Picker {
                project,
                index,
                mode,
            }) => (project.clone(), *index, *mode),
            _ => return,
        };
        let Some(preset) = self.new_choices(mode).into_iter().nth(index) else {
            return;
        };
        match mode {
            PickerMode::Lead => self.start_lead(project, preset, window, cx),
            PickerMode::Checkout => self.start_checkout(project, preset, window, cx),
            PickerMode::Task => self.request_launch(project, preset, None, window, cx),
        }
    }

    /// Installed agents the user has left on, in preset order.
    pub(super) fn offered_presets(&self) -> Vec<CliPreset> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        shika_core::picker_presets(&catalog.presets, |id| self.agents.enabled(id))
            .into_iter()
            .cloned()
            .collect()
    }

    /// Terminal is a task launch choice, never an installed agent or a Lead.
    pub(super) fn new_choices(&self, mode: PickerMode) -> Vec<CliPreset> {
        mode.choices(self.offered_presets())
    }

    pub(super) fn recovery_choices(&self) -> Vec<CliPreset> {
        self.new_choices(PickerMode::Task)
    }

    pub(super) fn request_launch(
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
        if preset.id != "terminal" && !preset.found() {
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

    pub(super) fn approve_preparation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn retry_preparation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            .or_else(|| (card.launch_preset == "terminal").then(shika_core::terminal_choice))
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

    /// Creates the card and starts the launch off the UI thread. A task from
    /// the picker, a Lead, and a retry are the author's: the card is selected
    /// and the dialogs close. A worker from `shika new` (`reply` answers the
    /// Lead's command) touches neither selection, focus, dialogs, nor `busy`,
    /// because the author is typing elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin_launch(
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
            Launch::Task | Launch::Checkout | Launch::Recover { .. } => (None, None),
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
        let shell_only = preset.id == "terminal";
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
            !shell_only,
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
            if shell_only || lead.is_some() || matches!(launch, Launch::Recover { .. }) {
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
            stage: if matches!(launch, Launch::Checkout) {
                "Starting agent..."
            } else {
                "Creating worktree..."
            }
            .into(),
            closing: false,
            next_agent: None,
            usage_exit_seen: false,
            lead_run: None,
        });
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
                        Launch::Recover { path, resume } => {
                            core.reopen_leftover(&path, &preset.id, resume, size, sink)
                        }
                        Launch::Checkout => {
                            core.create_checkout_session(&project, &preset.id, size, sink)
                        }
                        Launch::Worker(options) => core.create_session_with_preparation(
                            &project, &preset.id, size, sink, options, control, report,
                        ),
                        Launch::Task if shell_only => core.create_terminal_with_preparation(
                            &project, size, sink, control, report,
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
                        let recovered = matches!(finished, Launch::Recover { .. });
                        let recovered_id = session.id.clone();
                        if recovered || session.is_terminal() {
                            this.cards[index].title = model::card_title(&session.title);
                            this.cards[index].named = true;
                            this.cards[index].title_watch.finish();
                        }
                        this.cards[index].session = Some(session);
                        this.cards[index].launch_control = None;
                        if recovered {
                            this.fetch_diff_stat(recovered_id, cx);
                        }
                        // Completion never steals focus from another card or
                        // an overlay opened while installation was running,
                        // nor from the cards the author returned to with Ctrl+Q
                        // or the Changes panel.
                        if *this.selection == Some(Selection::Card(index))
                            && this.overlay.is_none()
                            && !matches!(finished, Launch::LeadRestart { focus: false, .. })
                            && ((!this.focus.is_focused(window)
                                && !(this.changes.open && this.changes.focus.is_focused(window)))
                                || matches!(finished, Launch::LeadRestart { focus: true, .. }))
                        {
                            this.focus_terminal(window, cx);
                        }
                        control::answer(&reply, this.started_reply(index));
                    }
                    Ok(session) => {
                        this.stop_cancelled_launch(session.id, cx);
                        this.cards[index].launch_error = Some(
                            if this.cards[index].is_checkout() {
                                "Agent start was cancelled."
                            } else {
                                "Worktree preparation was cancelled."
                            }
                            .into(),
                        );
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

    /// The picker for one project, from its `+`.
    pub(super) fn picker_for(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.prefetch_base(project.clone(), cx);
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Picker {
            project,
            index: 0,
            mode: PickerMode::Task,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }
}
