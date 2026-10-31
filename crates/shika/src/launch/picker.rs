//! The New picker and the worktree setup approval dialog: their state,
//! keys, and views. Launching itself lives in `launch/mod.rs`.
use super::*;

/// The New picker. `index` is a row in `new_choices(mode)` for `project`.
pub(crate) struct PickerDialog {
    pub(crate) project: String,
    pub(crate) index: usize,
    pub(crate) mode: PickerMode,
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
            .child(
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
                    ),
            );
        let offered = self.new_choices(*mode);
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
                    .unwrap_or_else(|| {
                        if preset.id == "terminal" {
                            "Login shell".into()
                        } else {
                            String::new()
                        }
                    });
                if preset.id == "terminal" && i > 0 {
                    panel = panel.child(
                        div()
                            .mt(px(6.))
                            .mb(px(6.))
                            .border_t_1()
                            .border_color(chrome.line_2),
                    );
                }
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
                            if let Some(Overlay::Picker(PickerDialog { index, .. })) =
                                &mut this.overlay
                            {
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
                .child(if *mode == PickerMode::Checkout {
                    div().min_w_0().truncate().font_family(MONO).child(format!(
                        "{name} · {}",
                        self.checkout_branch(project).unwrap_or("Reading branch...")
                    ))
                } else {
                    match self.bases.get(project).and_then(|b| b.name.clone()) {
                        // Where New will start, so it is never a surprise.
                        Some(name) => div()
                            .flex()
                            .gap(px(4.))
                            .child(if *mode == PickerMode::Lead {
                                "reads a checkout of"
                            } else {
                                "branches from"
                            })
                            .child(div().font_family(MONO).child(name)),
                        None if *mode == PickerMode::Lead => {
                            div().child("reads a detached worktree")
                        }
                        None => div().child("creates a branch and a worktree"),
                    }
                }),
        )
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
