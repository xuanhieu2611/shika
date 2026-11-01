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
            self.overlay = Some(Overlay::Picker(PickerDialog {
                project,
                index: 0,
                mode: PickerMode::Task,
            }));
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
            Some(Overlay::Picker(PickerDialog {
                project,
                index,
                mode,
            })) => (project.clone(), *index, *mode),
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
                        this.overlay = Some(Overlay::Preparation(PreparationDialog {
                            project,
                            preset,
                            config,
                            retry,
                        }));
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
        let Some(Overlay::Preparation(PreparationDialog {
            project,
            preset,
            config,
            retry,
        })) = &self.overlay
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
        self.overlay = Some(Overlay::Picker(PickerDialog {
            project,
            index: 0,
            mode: PickerMode::Task,
        }));
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
