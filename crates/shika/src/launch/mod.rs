//! New: the CLI picker, worktree setup approval and retry. `begin_launch`,
//! which starts every card, lives in `launch/start.rs`.
use super::*;

mod picker;
mod start;
pub(crate) use picker::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerMode {
    Task,
    Lead,
    Checkout,
}

impl PickerMode {
    pub(super) fn choices(self, mut agents: Vec<CliPreset>) -> Vec<CliPreset> {
        if self != Self::Lead {
            agents.push(shika_core::terminal_choice());
        }
        agents
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Task | Self::Checkout => "New agent",
            Self::Lead => "New Lead",
        }
    }
}

/// First New reviews setup only when detection suggested something. An empty
/// draft starts like no config and records no decision, so a later `.env` or
/// lockfile still offers the review; detection then reruns on each New.
fn first_new_review(draft: shika_core::PreparationDraft) -> Option<shika_core::PreparationDraft> {
    draft.suggests_setup().then_some(draft)
}

/// The New prompt to pass, trimmed. Empty or whitespace starts the CLI with
/// no positional argument, as before the field existed. Terminal never
/// takes one: the text stays in the field and is ignored.
fn launch_prompt(text: &str, preset: &CliPreset) -> Option<String> {
    let prompt = text.trim();
    (!prompt.is_empty() && preset.id != "terminal").then(|| prompt.to_owned())
}

impl Shika {
    pub(super) fn picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if let Some(project) = self.project_id() {
            self.prefetch_base(project.clone(), cx);
            self.show_picker(project, PickerMode::Task, window, cx);
        } else {
            self.add_project(window, cx);
        }
    }

    /// Opens the picker for `project`. Task and checkout pickers get the
    /// prompt field, focused, with any prompt whose start failed; New Lead
    /// has none.
    pub(super) fn show_picker(
        &mut self,
        project: String,
        mode: PickerMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlay_return_focus = window.focused(cx);
        let prompt = (mode != PickerMode::Lead).then(|| {
            let text = self.unsent_prompt.take().unwrap_or_default();
            new_prompt_input(text, self.chrome(window), cx)
        });
        match &prompt {
            Some(prompt) => window.focus(&prompt.focus_handle(cx), cx),
            None => window.focus(&self.focus, cx),
        }
        self.overlay = Some(Overlay::Picker(PickerDialog {
            project,
            index: 0,
            mode,
            prompt,
            error: None,
        }));
        cx.notify();
    }

    pub(super) fn launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.catalog.is_none() {
            return;
        }
        let Some(Overlay::Picker(picker)) = &self.overlay else {
            return;
        };
        if picker
            .prompt
            .as_ref()
            .is_some_and(|p| p.read(cx).composing())
        {
            return;
        }
        let (project, index, mode) = (picker.project.clone(), picker.index, picker.mode);
        let text = picker.prompt_text(cx);
        let Some(preset) = self.new_choices(mode).into_iter().nth(index) else {
            return;
        };
        let prompt = launch_prompt(&text, &preset);
        // Refused here, while the author can still fix it, never after the
        // worktree exists.
        if let Some(Err(error)) = prompt.as_deref().map(shika_core::check_launch_prompt) {
            if let Some(Overlay::Picker(picker)) = &mut self.overlay {
                picker.error = Some(error.to_string());
            }
            cx.notify();
            return;
        }
        match mode {
            PickerMode::Lead => self.start_lead(project, preset, window, cx),
            PickerMode::Checkout => self.start_checkout(project, preset, prompt, window, cx),
            PickerMode::Task => self.request_launch(project, preset, None, prompt, window, cx),
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

    /// Terminal is a worktree or checkout launch choice, never an installed agent or a Lead.
    pub(super) fn new_choices(&self, mode: PickerMode) -> Vec<CliPreset> {
        mode.choices(self.offered_presets())
    }

    pub(super) fn recovery_choices(&self) -> Vec<CliPreset> {
        self.new_choices(PickerMode::Task)
    }

    /// New for a worktree task: onboarding or approval when setup needs
    /// it, then the launch. `prompt` is the picker's; a Retry passes none and
    /// repeats the failed card's own.
    pub(super) fn request_launch(
        &mut self,
        project: String,
        preset: CliPreset,
        retry: Option<Arc<Mutex<HostState>>>,
        prompt: Option<String>,
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
                        first_new_review(draft)
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
                        this.show_preparation_onboarding(
                            project, preset, prompt, draft, window, cx,
                        );
                    }
                    Ok((Some(config), false, None)) => {
                        if this.overlay_return_focus.is_none() {
                            this.overlay_return_focus = window.focused(cx);
                        }
                        this.overlay = Some(Overlay::Preparation(PreparationDialog {
                            project,
                            preset,
                            config,
                            retry,
                            prompt,
                        }));
                        window.focus(&this.focus, cx);
                    }
                    Ok((config, _, None)) => this.begin_launch(
                        project,
                        preset,
                        config.is_some(),
                        retry,
                        Launch::Task { prompt },
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
        let Some(Overlay::Preparation(PreparationDialog {
            project,
            preset,
            config,
            retry,
            prompt,
        })) = &self.overlay
        else {
            return;
        };
        let (project, preset, config, retry, prompt) = (
            project.clone(),
            preset.clone(),
            config.clone(),
            retry.clone(),
            prompt.clone(),
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
                        Launch::Task { prompt },
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
            None,
            window,
            cx,
        );
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
        self.show_picker(project, PickerMode::Task, window, cx);
    }
}

#[cfg(test)]
mod tests;
