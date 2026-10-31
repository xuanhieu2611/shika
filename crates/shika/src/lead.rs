//! Lead lifecycle on the root view: start, resume, upkeep, restart, Stop, and
//! Forget, plus the memory-only state the app keeps for each started Lead.
use super::*;

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
pub(super) struct LeadRun {
    /// When a launch that resumed the saved conversation completed. A CLI
    /// that exits within [`model::RESUME_GRACE`] refused the saved id.
    pub(super) resumed_at: Option<Instant>,
    pub(super) unexpected_exit: bool,
    /// What became of a resumed Lead that exited early (see
    /// [`model::ResumeExit`]), until the app has acted on it.
    pub(super) resume_exit: Option<model::ResumeExit>,
    /// Looking for the conversation id the CLI picks (Codex).
    discovery: Option<model::Discovery>,
    /// The restart summary, until it has been typed into the Lead.
    pub(super) summary: Option<RestartSummary>,
}

/// The `[shika]` line for a restarted Lead and the lost workers it names.
#[derive(Clone)]
pub(super) struct RestartSummary {
    pub(super) line: String,
    pub(super) workers: Vec<std::path::PathBuf>,
}

impl LeadRun {
    pub(super) fn new(start: &shika_core::LeadStart, now: Instant) -> Self {
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

impl Shika {
    /// The project whose resting Lead is selected.
    pub(super) fn selected_resting(&self) -> Option<&str> {
        match &*self.selection {
            Some(Selection::Resting(project)) => Some(project),
            _ => None,
        }
    }

    /// Projects with a saved Lead and no live one, as (project id, CLI
    /// preset id), in project order. A Lead that is still starting counts as
    /// live.
    pub(super) fn resting_leads(&self) -> Vec<(&str, &str)> {
        self.projects
            .iter()
            .filter(|p| self.lead_card(&p.id).is_none())
            .filter_map(|p| Some((p.id.as_str(), p.lead.as_ref()?.cli.as_str())))
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
    pub(super) fn start_lead(
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
    pub(super) fn new_lead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            mode: PickerMode::Lead,
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
        self.selection.set(Some(Selection::Card(index)));
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
    pub(super) fn resume_lead(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// What a started Lead still needs: its conversation id when the CLI
    /// picks it (Codex) once it has been used, and what to do about a resumed
    /// CLI that exited at once. Nothing runs for a Lead that needs neither.
    pub(super) fn lead_upkeep(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            *self.selection == Some(Selection::Card(index)),
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
                            this.focus_shown(window, cx);
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
                        this.selection.set(Some(Selection::Card(this.cards.len())));
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

    /// Opens the Lead's close dialog: Stop for a live Lead (`session`), Forget
    /// for a resting one. Reads whether its worktree has changes first.
    pub(super) fn open_lead_close(
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
    pub(super) fn finish_lead_close(
        &mut self,
        forget: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                        let was = this.overlay_return_focus.clone();
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
                        // A dialog asking again keeps the focus it needs.
                        if this.overlay.is_some() {
                            window.focus(&this.focus, cx);
                        } else {
                            this.land_after(was, window, cx);
                        }
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
        let before = self.selection.clone();
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
        let next = if !mine {
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
        self.selection.set(next);
        self.last_revealed_selection = self.selection.clone();
    }

    /// Forget from the Lead's close dialog. From the Stop step, a worktree
    /// with something to lose first shows the Forget step with those facts;
    /// otherwise, and from the Forget step, the Lead is forgotten.
    pub(super) fn forget_lead_from_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}
