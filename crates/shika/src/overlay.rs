//! Painting and cancelling the current overlay (dialogs, pickers, menus).
use super::*;

impl Shika {
    pub(super) fn cancel_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Quit { .. })) {
            self.cancel_quit(window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.answer_chat_question(true, window, cx);
            return;
        }
        if self.busy
            && matches!(
                self.overlay,
                Some(
                    Overlay::Publish { .. }
                        | Overlay::WorktreeSetup(_)
                        | Overlay::SwitchAgent { .. }
                )
            )
        {
            return;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.finish_worktree_setup(window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::Adjust(_))) {
            self.close_adjust(false, window, cx);
            return;
        }
        self.commit_setting_edit(window, cx);
        self.key_recording = false;
        self.key_note = None;
        self.overlay = None;
        // The shown tab stays. A Lead that asked for Close still takes
        // selection and focus back in settle_lead_dialog.
        self.restore_overlay_focus(window, cx);
        self.settle_lead_dialog(window, cx);
        cx.notify();
    }

    /// Pointer menus and appearance previews stay undimmed. Other modal
    /// overlays reuse the dialog shadow as a full-window scrim.
    pub(super) fn overlay_view(
        &self,
        overlay: &Overlay,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Overlay::CardMenu(menu) = overlay {
            return self
                .card_menu_view(menu, chrome, window, cx)
                .into_any_element();
        }
        if let Overlay::RestingMenu { position, .. } = overlay {
            return self
                .resting_menu_view(*position, chrome, window, cx)
                .into_any_element();
        }
        let height = window.viewport_size().height;
        let picker = matches!(overlay, Overlay::Picker { .. });
        let top = height * if picker { 0.18 } else { 0.20 };
        let max_h = (height - top - px(24.)).max(px(120.));
        let panel = match overlay {
            Overlay::CardMenu(_) | Overlay::RestingMenu { .. } => {
                unreachable!("card menus render at the pointer")
            }
            Overlay::LeadQuestion(question) => self.chat_question_view(question, chrome, max_h, cx),
            Overlay::LeadClose { step, .. } => {
                let stopping = matches!(step, model::LeadClose::Stop { .. });
                let mut panel =
                    dialog(chrome, max_h).child(dialog_title(div()).child(step.title()));
                for fact in step.facts() {
                    panel = panel.child(dialog_text(chrome).child(fact));
                }
                let buttons = dialog_buttons().child(self.cancel_button("Cancel", chrome, cx));
                panel.child(if stopping {
                    buttons
                        .child(
                            dialog_button("forget-lead", "Forget", "\u{2318}\u{232b}", chrome)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if !this.busy {
                                        this.forget_lead_from_dialog(window, cx);
                                    }
                                })),
                        )
                        .child(primary_button("stop-lead", "Stop", "↵", chrome).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.finish_lead_close(false, window, cx)
                            }),
                        ))
                } else {
                    buttons.child(
                        primary_button("forget-lead", "Forget", "\u{2318}\u{232b}", chrome)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_lead_close(true, window, cx)
                            })),
                    )
                })
            }
            Overlay::WorktreeSetup(editor) => {
                self.worktree_setup_view(editor, chrome, max_h, window, cx)
            }
            Overlay::Adjust(adjust) => self.adjust_view(adjust, max_h, chrome, window, cx),
            Overlay::Rename {
                target,
                input,
                error,
            } => {
                let checkout = self
                    .cards
                    .iter()
                    .any(|card| card.agent.view.entity_id() == *target && card.is_checkout());
                input.update(cx, |input, _| input.set_chrome(*chrome));
                dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Rename task"))
                    .child(dialog_text(chrome).child(if checkout {
                        "Changes the task name. Your checkout and branch stay unchanged."
                    } else {
                        "Changes the task name and future Create PR defaults. The branch and worktree stay unchanged."
                    }))
                    .child(input.clone())
                    .when_some(error.as_ref(), |panel, error| panel.child(dialog_text(chrome).child(error.clone())))
                    .child(dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(primary_button("rename-task", "Rename task", "↵", chrome)
                            .on_click(cx.listener(|this, _, window, cx| this.apply_task_name(window, cx)))))
            }
            Overlay::SwitchAgent { .. } => self.switch_dialog(chrome, home, max_h, cx),
            Overlay::Publish {
                preview,
                title,
                body,
                target,
                row,
                error,
            } => {
                body.update(cx, |input, _| {
                    input.style(*chrome, false, true, Vec::new());
                });
                let description = body.clone();
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Create PR?"))
                    .child(
                        dialog_text(chrome)
                            .font_family(MONO)
                            .child(format!("{}: {}", preview.repository, preview.branch)),
                    )
                    .child(dialog_text(chrome).child("Commit and PR title"))
                    .child(
                        text_field("pr-title", 360., *row == 0, chrome)
                            .overflow_hidden()
                            .child(title.clone())
                            .when(*row == 0, |d| {
                                d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.busy {
                                    if let Some(Overlay::Publish { row, .. }) = &mut this.overlay {
                                        *row = 0;
                                    }
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                }
                            })),
                    )
                    .child(dialog_text(chrome).child("PR description"))
                    .child(
                        div()
                            .id("pr-body-field")
                            .w(px(360.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(chrome.raised)
                            .border_1()
                            .border_color(if *row == 1 {
                                chrome.focus
                            } else {
                                chrome.line_control
                            })
                            .cursor_text()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    let body = if let Some(Overlay::Publish { row, body, .. }) =
                                        &mut this.overlay
                                    {
                                        *row = 1;
                                        Some(body.clone())
                                    } else {
                                        None
                                    };
                                    if let Some(body) = body {
                                        window.focus(&body.focus_handle(cx), cx);
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(description),
                    )
                    .child(dialog_text(chrome).child("PR target"))
                    .child(
                        text_field("pr-target", 360., *row == 2, chrome)
                            .overflow_hidden()
                            .child(target.clone())
                            .when(*row == 2, |d| {
                                d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.busy {
                                    if let Some(Overlay::Publish { row, .. }) = &mut this.overlay {
                                        *row = 2;
                                    }
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                }
                            })),
                    );
                if preview.target.is_none() {
                    panel = panel.child(dialog_text(chrome).child("The recorded base is not an existing GitHub branch. Choose a target explicitly."));
                }
                if *row == 2 {
                    panel = panel.child(
                        div()
                            .id("pr-branches")
                            .max_h(px(BASE_LIST_MAX))
                            .overflow_y_scroll()
                            .children(
                                preview
                                    .branches
                                    .iter()
                                    .filter(|b| b.starts_with(target) && **b != preview.branch)
                                    .map(|b| {
                                        let branch = b.clone();
                                        list_row(b == target, chrome)
                                            .id(SharedString::from(format!("pr-branch-{b}")))
                                            .cursor_pointer()
                                            .font_family(MONO)
                                            .child(b.clone())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !this.busy {
                                                    if let Some(Overlay::Publish {
                                                        target,
                                                        error,
                                                        ..
                                                    }) = &mut this.overlay
                                                    {
                                                        *target = branch.clone();
                                                        *error = None;
                                                    }
                                                    cx.notify();
                                                }
                                            }))
                                    }),
                            ),
                    );
                }
                let file_count = preview.files.len();
                panel = panel.child(dialog_text(chrome).child(publish_change_label(file_count)));
                if file_count > 0 {
                    panel = panel.child(
                        div()
                            .id("pr-files")
                            .max_h(px(BASE_LIST_MAX))
                            .overflow_y_scroll()
                            .children(preview.files.iter().map(|file| {
                                dialog_text(chrome).font_family(MONO).child(file.clone())
                            })),
                    );
                }
                if let Some(error) = error {
                    panel = panel.child(dialog_text(chrome).child(error.clone()));
                }
                panel
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(hint("tab", "switch field", chrome))
                            .when(*row == 1, |hints| {
                                hints.child(hint("enter", "new line", chrome))
                            }),
                    )
                    .child(
                        dialog_buttons()
                            .child(self.cancel_button("Cancel", chrome, cx))
                            .child(
                                primary_button(
                                    "publish-pr",
                                    if self.busy {
                                        "Publishing..."
                                    } else {
                                        "Commit, push, create PR"
                                    },
                                    if *row == 1 {
                                        "\u{2318}\u{21a9}"
                                    } else {
                                        "\u{21a9}"
                                    },
                                    chrome,
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.publish_pr(window, cx)),
                                ),
                            ),
                    )
            }
            Overlay::Picker {
                project,
                index,
                mode,
            } => {
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
                                    if let Some(Overlay::Picker { index, .. }) = &mut this.overlay {
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
            Overlay::Preparation {
                project, config, ..
            } => {
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
                    panel =
                        panel.child(dialog_text(chrome).font_family(MONO).child(command.clone()));
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
                                primary_button("approve-setup", "Approve and start", "↵", chrome)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.approve_preparation(window, cx)
                                    })),
                            ),
                    )
            }
            Overlay::CheckoutClose { index } => {
                let i = *index;
                dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Stop the agent?"))
                    .child(dialog_text(chrome).child("The agent is still active. This stops its terminals. Your checkout and files stay unchanged."))
                    .child(dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(primary_button("stop-checkout", "Stop agent", "⌘↵", chrome)
                            .on_click(cx.listener(move |this, _, window, cx| this.finish_close(i, 4, window, cx)))))
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
                        "The worktree has uncommitted changes. Commit in the shell or use Create PR before closing. Close does not commit.",
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
                            dialog_button("discard", "Discard changes", "\u{2318}D", chrome)
                                .on_click(discard),
                        )
                        .child(
                            primary_button("push", "Push changes", "\u{2318}\u{21b5}", chrome)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.finish_close(i, 2, window, cx)
                                })),
                        )
                } else {
                    buttons.child(
                        primary_button("discard", "Discard changes", "\u{2318}D", chrome)
                            .on_click(discard),
                    )
                };
                panel.child(buttons)
            }
            Overlay::SwitchedClose {
                index,
                preview,
                working,
            } => {
                let i = *index;
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Close “{}” after branch switch?", self.cards[i].title)))
                    .child(dialog_text(chrome).child("This task started on:"))
                    .child(dialog_text(chrome).font_family(MONO).child(preview.recorded.clone()))
                    .child(dialog_text(chrome).child("The worktree is now on:"))
                    .child(dialog_text(chrome).font_family(MONO).child(preview.current.clone()))
                    .child(dialog_text(chrome).child("No unpublished work was found. Closing removes the worktree and stops all task terminals. Both local branches will be kept."));
                if *working {
                    panel = panel.child(
                        dialog_text(chrome)
                            .child("The agent is still active. Closing stops its current turn."),
                    );
                }
                panel.child(
                    dialog_buttons()
                        .child(self.cancel_button("Cancel", chrome, cx))
                        .child(
                            primary_button(
                                "close-switched",
                                "Close task",
                                "\u{2318}\u{21b5}",
                                chrome,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.finish_close(i, 3, window, cx),
                            )),
                        ),
                )
            }
            Overlay::Quit { .. } => self.quit_dialog(chrome, max_h, cx),
            Overlay::Recover { .. } => self.recovery_dialog(chrome, max_h, cx),
            Overlay::Leftovers => {
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child("Leftover worktrees"))
                    .child(dialog_text(chrome).child(
                        "Their work remains on disk. Reopen a worktree to continue, or remove it to discard its work.",
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
                                secondary_button(SharedString::from(format!("reopen-{i}")), chrome)
                                    .px(px(10.))
                                    .py(px(4.))
                                    .text_size(px(12.))
                                    .child("Reopen")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.inspect_leftover(i, window, cx)
                                    })),
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
                        .child(hint("↵", "reopen", chrome))
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
                            primary_button(
                                "remove-project",
                                "Remove project",
                                "\u{2318}\u{232b}",
                                chrome,
                            )
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
                                primary_button(
                                    "remove-confirm",
                                    "Discard worktree",
                                    "\u{2318}\u{232b}",
                                    chrome,
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_leftover(i, cx)),
                                ),
                            ),
                    )
            }
            Overlay::Base {
                project,
                text,
                error,
                choices,
                highlight,
            } => {
                let name = self
                    .projects
                    .iter()
                    .find(|p| &p.id == project)
                    .map(|p| p.name.as_str())
                    .unwrap_or("");
                let default = self.bases.get(project).and_then(|b| b.default_name.clone());
                let field = text_field("base-branch-value", 360., true, chrome)
                    .when(!text.is_empty(), |d| d.child(text.clone()))
                    .child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                    .when_some(default.filter(|_| text.is_empty()), |d, default| {
                        d.child(div().text_color(chrome.ink_4).child(default))
                    });
                let names = choices
                    .as_ref()
                    .map(|choices| choices.names.as_slice())
                    .unwrap_or(&[]);
                let checked_out = choices
                    .as_ref()
                    .and_then(|choices| choices.checked_out.clone());
                let matched = model::branch_matches(names, text);
                let highlight = highlight
                    .as_ref()
                    .copied()
                    .filter(|index| *index < matched.len());
                let action = if self.chosen_base_name().trim().is_empty() {
                    "Use default branch"
                } else {
                    "Set base branch"
                };
                let fetch_hint = choices.as_ref().is_some_and(|choices| choices.has_origin)
                    && !text.is_empty()
                    && matched.is_empty()
                    && error.is_none();
                let mut panel = dialog(chrome, max_h)
                    .child(dialog_title(div()).child(format!("Base branch for {name}")))
                    .child(
                        dialog_text(chrome)
                            .child("New agents branch from it. Running agents keep their base."),
                    )
                    .child(field.mt(px(4.)));
                if !matched.is_empty() {
                    let mut rows = div()
                        .id("base-branch-list")
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .max_h(px(BASE_LIST_MAX))
                        .overflow_y_scroll()
                        .track_scroll(&self.base_scroll);
                    for (row, index) in matched.iter().copied().enumerate() {
                        let branch = names[index].clone();
                        let label = branch.clone();
                        let checked = checked_out.as_deref() == Some(label.as_str());
                        rows = rows.child(
                            list_row(highlight == Some(row), chrome)
                                .id(("base-branch-row", row))
                                .w_full()
                                .justify_between()
                                .gap(px(8.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_base(branch.clone(), window, cx);
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .font_family(MONO)
                                        .text_size(px(12.))
                                        .child(label),
                                )
                                .when(checked, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(chrome.ink_3)
                                            .child("checked out"),
                                    )
                                }),
                        );
                    }
                    panel = panel.child(rows);
                }
                if fetch_hint {
                    panel = panel.child(
                        dialog_text(chrome)
                            .text_color(chrome.ink_3)
                            .child("Not on this machine. Enter fetches it from origin."),
                    );
                }
                panel
                    .when_some(error.clone(), |d, error| {
                        d.child(dialog_text(chrome).text_color(chrome.ink_1).child(error))
                    })
                    .child(
                        dialog_buttons()
                            .child(self.cancel_button("Cancel", chrome, cx))
                            .child(primary_button("set-base", action, "↵", chrome).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let branch = this.chosen_base_name();
                                    this.apply_base(branch, window, cx);
                                }),
                            )),
                    )
            }
            Overlay::Settings { section, row, .. } => {
                self.settings_panel(*section, *row, max_h, chrome, home, cx)
            }
        };
        div()
            .absolute()
            .occlude()
            .inset_0()
            .when(overlay.dims_background(), |d| d.bg(chrome.dialog_shadow))
            .flex()
            .items_start()
            .justify_center()
            .pt(top)
            .child(panel)
            .into_any_element()
    }
}
