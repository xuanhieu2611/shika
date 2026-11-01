//! The New picker and the worktree setup approval dialog: their state,
//! keys, and views. Launching itself lives in `launch/mod.rs`.
use super::*;

/// The New picker. `index` is a row in `new_choices(mode)` for `project`.
pub(crate) struct PickerDialog {
    pub(crate) project: String,
    pub(crate) index: usize,
    pub(crate) mode: PickerMode,
}

impl PickerDialog {
    /// Changing location never starts a session. Terminal is worktree-only;
    /// preserve a selected agent, but reset a removed Terminal row to the first.
    pub(super) fn toggle_worktree(&mut self, agent_count: usize) -> bool {
        self.mode = match self.mode {
            PickerMode::Task => PickerMode::Checkout,
            PickerMode::Checkout => PickerMode::Task,
            PickerMode::Lead => return false,
        };
        if self.index >= agent_count {
            self.index = 0;
        }
        true
    }
}

/// Approval for a project's worktree setup before New starts `preset`.
/// `retry` is the failed card's agent state when this is a Retry.
pub(crate) struct PreparationDialog {
    pub(crate) project: String,
    pub(crate) preset: CliPreset,
    pub(crate) config: PreparationConfig,
    pub(crate) retry: Option<Arc<Mutex<HostState>>>,
}

impl Shika {
    /// A plain key while the picker is open.
    pub(crate) fn picker_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if key == "space" {
            self.toggle_picker_worktree(cx);
            return;
        }
        let offered_len = match &self.overlay {
            Some(Overlay::Picker(dialog)) => self.new_choices(dialog.mode).len(),
            _ => return,
        };
        let catalog_ready = self.catalog.is_some();
        // A project the picker moved to: fetch the base, or read checkout HEAD.
        let mut moved_to = None;
        if let Some(Overlay::Picker(PickerDialog {
            index,
            project,
            mode,
        })) = &mut self.overlay
        {
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
                        moved_to = Some((project.clone(), *mode));
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
        if let Some((project, mode)) = moved_to {
            if mode == PickerMode::Checkout {
                self.refresh_checkout_head(project, cx);
            } else {
                self.prefetch_base(project, cx);
            }
        }
    }

    /// Reuse the existing launch routes, including core's one-checkout guard.
    fn toggle_picker_worktree(&mut self, cx: &mut Context<Self>) {
        let agent_count = self.offered_presets().len();
        let Some(Overlay::Picker(dialog)) = &mut self.overlay else {
            return;
        };
        if !dialog.toggle_worktree(agent_count) {
            return;
        }
        let (project, mode) = (dialog.project.clone(), dialog.mode);
        if mode == PickerMode::Checkout {
            self.refresh_checkout_head(project, cx);
        } else {
            self.prefetch_base(project, cx);
        }
        cx.notify();
    }

    /// A plain key while the setup approval dialog is open.
    pub(crate) fn preparation_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key {
            "enter" => self.approve_preparation(window, cx),
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }

    pub(crate) fn picker_view(
        &self,
        picker: &PickerDialog,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let PickerDialog {
            project,
            index,
            mode,
        } = picker;
        let name = self
            .projects
            .iter()
            .find(|p| &p.id == project)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        let mut panel = dialog_shell(420., max_h, chrome)
            .p(px(8.))
            .gap(px(2.))
            .child(picker_header(*mode, name, chrome));
        let offered = self.new_choices(*mode);
        let offer_settings = self.catalog.is_some() && offered.is_empty();
        if self.catalog.is_none() {
            panel = panel.child(
                list_row(false, chrome)
                    .text_color(chrome.ink_3)
                    .child("Resolving login-shell PATH..."),
            );
        } else if offered.is_empty() {
            panel = picker_no_agents(panel, chrome, cx);
        } else {
            for (i, preset) in offered.iter().enumerate() {
                if preset.id == "terminal" && i > 0 {
                    panel = panel.child(
                        div()
                            .mt(px(6.))
                            .mb(px(6.))
                            .border_t_1()
                            .border_color(chrome.line_2),
                    );
                }
                panel = panel.child(picker_preset_row(i, preset, i == *index, home, chrome, cx));
            }
        }
        if *mode == PickerMode::Checkout {
            panel = panel.child(
                div()
                    .px(px(10.))
                    .pt(px(6.))
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child("Runs in your project folder on ")
                    .child(
                        div().flex_1().min_w_0().truncate().font_family(MONO).child(
                            self.checkout_branch(project)
                                .unwrap_or("Reading branch...")
                                .to_owned(),
                        ),
                    )
                    .flex()
                    .flex_wrap()
                    .items_center(),
            );
        }
        panel.child(self.picker_footer(project, *mode, offer_settings, chrome, cx))
    }

    /// The picker's footer: the keys, then where the start lands.
    fn picker_footer(
        &self,
        project: &str,
        mode: PickerMode,
        offer_settings: bool,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let footer = div()
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
            .child("esc cancel");
        if mode != PickerMode::Lead {
            return footer
                .items_center()
                .child(div().flex_1())
                .child(self.picker_worktree_switch(project, mode, chrome, cx));
        }
        footer.child(match self.bases.get(project).and_then(|b| b.name.clone()) {
            Some(name) => div()
                .flex()
                .gap(px(4.))
                .child("reads a checkout of")
                .child(div().font_family(MONO).child(name)),
            None => div().child("reads a detached worktree"),
        })
    }

    /// Quiet, neutral switch. The label and track share one click target;
    /// focus stays on the picker so navigation and Escape keep working.
    fn picker_worktree_switch(
        &self,
        project: &str,
        mode: PickerMode,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let on = mode == PickerMode::Task;
        let tip = if on {
            match self.bases.get(project).and_then(|b| b.name.as_deref()) {
                Some(base) => format!("Creates an isolated worktree from {base}. Space to toggle."),
                None => "Creates an isolated worktree. Space to toggle.".to_owned(),
            }
        } else {
            "Runs in your project folder, no worktree. Space to toggle.".to_owned()
        };
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let ink = chrome.ink_1;
        div()
            .id("picker-create-worktree")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(move |style| style.text_color(ink))
            .child("Create worktree")
            .child(
                div()
                    .w(px(28.))
                    .h(px(17.))
                    .rounded_full()
                    .bg(if on { chrome.ink_1 } else { chrome.sunken })
                    .border_1()
                    .border_color(if on { chrome.ink_1 } else { chrome.ink_3 })
                    .flex()
                    .items_center()
                    .px(px(2.))
                    .when(on, |track| track.justify_end())
                    .child(div().size(px(11.)).rounded_full().bg(if on {
                        chrome.overlay
                    } else {
                        chrome.ink_3
                    })),
            )
            .tooltip(move |_, cx| {
                cx.new(|_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: tip.clone().into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker_worktree(cx)))
    }

    pub(crate) fn preparation_view(
        &self,
        preparation: &PreparationDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let PreparationDialog {
            project, config, ..
        } = preparation;
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
            panel = panel.child(dialog_text(chrome).font_family(MONO).child(command.clone()));
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
                        primary_button("approve-setup", "Approve and start", "↵", chrome).on_click(
                            cx.listener(|this, _, window, cx| this.approve_preparation(window, cx)),
                        ),
                    ),
            )
    }
}

/// The picker's title row: the mode, the project, and the Tab hint.
fn picker_header(mode: PickerMode, name: &str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .pt(px(8.))
        .px(px(10.))
        .pb(px(10.))
        .child(dialog_title(div()).child(mode.title()))
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
        )
}

/// No agent is on: say so and offer Settings.
fn picker_no_agents(
    panel: gpui::Stateful<gpui::Div>,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    panel
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
        )
}

/// One choice: its number key, name, and binary path.
fn picker_preset_row(
    i: usize,
    preset: &CliPreset,
    selected: bool,
    home: Option<&std::path::Path>,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let detail = preset
        .path
        .as_deref()
        .map(|path| model::tilde(path, home))
        .unwrap_or_else(|| {
            if preset.id == "terminal" {
                "Login shell".into()
            } else {
                String::new()
            }
        });
    list_row(selected, chrome)
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
            if let Some(Overlay::Picker(PickerDialog { index, .. })) = &mut this.overlay {
                *index = i;
            }
            this.launch(window, cx);
        }))
}
