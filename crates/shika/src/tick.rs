//! The periodic tick: drags, Lead upkeep, branch checks, activity, and
//! notifications, plus the Ready diff stat fetch.
use super::*;

impl Shika {
    pub(super) fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            && self.cards.iter().any(|card| !card.is_checkout())
            && now.duration_since(self.branch_check_at) >= Duration::from_secs(2)
        {
            self.branch_check_at = now;
            self.branch_check_pending = true;
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .spawn(async move {
                        for session in core.sessions().into_iter().filter(|s| !s.is_checkout()) {
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
                    // Only focus that was in this card's terminals is lost;
                    // the cards and the Changes panel keep theirs.
                    let lost = card.agent.view.focus_handle(cx).is_focused(window)
                        || card
                            .shells
                            .iter()
                            .any(|pane| pane.view.focus_handle(cx).is_focused(window));
                    self.remove_card(i);
                    if lost {
                        self.focus_shown(window, cx);
                    }
                    changed = true;
                }
            }
        }
        for card in &mut self.cards {
            if card.chat || (!card.creating && card.launch_preset == "terminal") {
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
                    state.preparation_stage = if card.is_checkout() {
                        "Stopping agent..."
                    } else {
                        "Cancelling setup..."
                    }
                    .into();
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
                && !card.session.as_ref().is_some_and(Session::is_checkout)
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
            if let Some(project) = self
                .cards
                .iter()
                .find(|c| c.is_checkout() && c.session.as_ref().is_some_and(|s| s.id == id))
                .map(|c| c.project.clone())
            {
                self.refresh_checkout_head(project, cx);
            }
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
                self.selection.set(Some(Selection::Card(i)));
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

    /// Reads what a Ready task changed, off the main thread. A failure only
    /// leaves the stat hidden.
    pub(super) fn fetch_diff_stat(&mut self, id: String, cx: &mut Context<Self>) {
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
}
