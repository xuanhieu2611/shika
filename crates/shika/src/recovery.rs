//! Quit guard and explicit leftover recovery. No startup auto-launch or poller.
use super::*;

/// The leftover worktrees on disk and the row chosen in their list. Both
/// outlive the Leftovers dialog: the column footer counts the entries.
pub(super) struct LeftoverList {
    pub(super) entries: Vec<JournalEntry>,
    selected: usize,
}

impl LeftoverList {
    pub(super) fn new(entries: Vec<JournalEntry>) -> Self {
        Self {
            entries,
            selected: 0,
        }
    }
}

/// Reopen one leftover worktree: `index` is the chosen CLI in
/// `recovery_choices`.
pub(super) struct RecoverDialog {
    entry: JournalEntry,
    index: usize,
}

/// The Quit confirmation. `previous` is the overlay it covers, restored on
/// cancel with `focus`.
pub(super) struct QuitDialog {
    pub(super) previous: Option<Box<Overlay>>,
    pub(super) focus: Option<FocusHandle>,
}

impl Shika {
    pub(super) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Quit(_))) {
            return;
        }
        if self.busy {
            self.message("Wait for the current operation before quitting.".into());
            cx.notify();
            return;
        }
        let previous = self.overlay.take().map(Box::new);
        self.overlay = Some(Overlay::Quit(QuitDialog {
            previous,
            focus: window.focused(cx),
        }));
        window.focus(&self.focus, cx);
        self.observe_dialog(Instant::now());
        cx.notify();
    }

    pub(super) fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !matches!(self.overlay, Some(Overlay::Quit(_))) {
            return;
        }
        if let Some(Overlay::Quit(QuitDialog { previous, focus })) = self.overlay.take() {
            self.overlay = previous.map(|p| *p);
            if self.overlay.is_some() {
                // The preserved editor/dialog still owns its native field.
                window.focus(focus.as_ref().unwrap_or(&self.focus), cx);
            } else {
                // A notification may have changed selection while Quit was
                // open. Do not focus a terminal that is now hidden.
                self.overlay_return_focus = focus;
                self.restore_overlay_focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !matches!(self.overlay, Some(Overlay::Quit(_))) {
            return;
        }
        self.busy = true;
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.cancel_preparations();
                    core.checkpoint_sessions()
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.quit(),
                    Err(err) => this.message(format!("Could not save recovery information. {err}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn quit_dialog(
        &self,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        dialog(chrome, max_h)
            .child(dialog_title(div()).child("Quit Shika?"))
            .child(dialog_text(chrome).child("This stops all agents and shell terminals. Worktrees and files stay on disk. Reopen unfinished tasks from Leftover worktrees next time."))
            .child(dialog_text(chrome).child("Unsaved terminal input and shell tabs are not restored. Agent conversations can resume when the CLI saved them."))
            .children(
                crate::lead_chat::warning(self.queued_lead_messages(None, cx))
                    .map(|note| dialog_text(chrome).child(note)),
            )
            .child(dialog_buttons()
                .child(dialog_button("cancel-quit", "No, keep working", "N", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                .child(primary_button("confirm-quit", if self.busy { "Saving..." } else { "Yes, quit" }, "Y", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_quit(window, cx)))))
    }

    pub(super) fn inspect_leftover(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(entry) = self.leftovers.entries.get(index) else {
            return;
        };
        let path = entry.path.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.leftover_recovery(&path) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                match result {
                    Ok(entry) if matches!(this.overlay, Some(Overlay::Leftovers)) => {
                        let index = this
                            .recovery_choices()
                            .iter()
                            .position(|p| entry.recovery.as_ref().is_some_and(|r| r.cli == p.id))
                            .unwrap_or(0);
                        this.overlay = Some(Overlay::Recover(RecoverDialog { entry, index }));
                    }
                    Ok(_) => {}
                    Err(err) => this.message(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_recovery(
        &mut self,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(Overlay::Recover(RecoverDialog { entry, index })) = &self.overlay else {
            return;
        };
        let Some(preset) = self.recovery_choices().get(*index).cloned() else {
            return;
        };
        let project = self
            .projects
            .iter()
            .find(|p| entry.path.parent().and_then(|p| p.parent()) == Some(p.path.as_path()))
            .map(|p| p.id.clone());
        let Some(project) = project else {
            return;
        };
        let resume = !fresh
            && entry
                .recovery
                .as_ref()
                .is_some_and(|r| r.cli == preset.id && r.conversation.is_some());
        let launch = Launch::Recover {
            path: entry.path.clone(),
            resume,
        };
        self.begin_launch(project, preset, false, None, launch, None, window, cx);
    }

    pub(super) fn recovery_dialog(
        &self,
        recover: &RecoverDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let RecoverDialog { entry, index } = recover;
        let presets = self.recovery_choices();
        let terminal = presets.get(*index).is_some_and(|p| p.id == "terminal");
        let can_resume = self.recovery_can_resume(entry, *index);
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child("Reopen worktree?"))
            .child(dialog_text(chrome).font_family(MONO).child(entry.branch.clone()))
            .child(dialog_text(chrome).child("Keeps its branch, commits, and uncommitted files. Setup is not run again."))
            .child(dialog_text(chrome).child(if terminal {
                "Yes opens a new login shell in this worktree. Previous shell programs are not restored."
            } else if can_resume {
                "Yes resumes the saved agent conversation. Start fresh begins a new agent in this worktree instead."
            } else {
                "No saved conversation is available for this agent. Yes starts fresh in the same worktree."
            }));
        if presets.is_empty() {
            panel = panel
                .child(dialog_text(chrome).child("No agents available. Turn one on in Settings."));
        }
        for (i, preset) in presets.iter().enumerate() {
            panel = panel.child(
                list_row(i == *index, chrome)
                    .id(("recovery-cli", i))
                    .child(preset.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(Overlay::Recover(RecoverDialog { index, .. })) =
                            &mut this.overlay
                        {
                            *index = i;
                        }
                        cx.notify();
                    })),
            );
        }
        let mut buttons = dialog_buttons().child(
            dialog_button("cancel-recovery", "No", "N", chrome).on_click(cx.listener(
                |this, _, _, cx| {
                    this.overlay = Some(Overlay::Leftovers);
                    cx.notify();
                },
            )),
        );
        if !presets.is_empty() {
            if can_resume {
                buttons = buttons.child(
                    dialog_button("fresh-recovery", "Start fresh", "F", chrome).on_click(
                        cx.listener(|this, _, window, cx| this.start_recovery(true, window, cx)),
                    ),
                );
            }
            buttons = buttons.child(
                primary_button("start-recovery", "Yes", "Y", chrome).on_click(
                    cx.listener(|this, _, window, cx| this.start_recovery(false, window, cx)),
                ),
            );
        }
        panel.child(buttons)
    }

    pub(super) fn quit_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if key == "escape" {
            self.cancel_quit(window, cx);
        }
    }

    /// Whether the CLI chosen at `index` can resume the leftover's saved
    /// conversation.
    fn recovery_can_resume(&self, entry: &JournalEntry, index: usize) -> bool {
        self.recovery_choices().get(index).is_some_and(|p| {
            entry
                .recovery
                .as_ref()
                .is_some_and(|r| r.cli == p.id && r.conversation.is_some())
        })
    }

    /// Plain keys in Reopen: `j`/`k` choose the CLI, `f` starts fresh when a
    /// conversation could be resumed, and Escape is No. `y` and `n` are
    /// handled by `dialog_answer_key`.
    pub(super) fn recover_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.recovery_choices().len();
        let fresh = matches!(
            &self.overlay,
            Some(Overlay::Recover(RecoverDialog { entry, index })) if self.recovery_can_resume(entry, *index)
        );
        if let Some(Overlay::Recover(RecoverDialog { index, .. })) = &mut self.overlay {
            match key {
                "j" | "down" if len > 0 => *index = (*index + 1) % len,
                "k" | "up" if len > 0 => *index = (*index + len - 1) % len,
                _ => {}
            }
        }
        if key == "f" && fresh {
            self.start_recovery(true, window, cx);
        }
        if key == "escape" {
            self.overlay = Some(Overlay::Leftovers);
        }
    }

    pub(super) fn remove_leftover_key(&mut self, key: &str) {
        if key == "escape" {
            self.overlay = Some(Overlay::Leftovers);
        }
    }

    pub(super) fn leftovers_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.leftovers.entries.len();
        match key {
            "j" | "down" if len > 0 => {
                self.leftovers.selected = (self.leftovers.selected + 1) % len
            }
            "k" | "up" if len > 0 => {
                self.leftovers.selected = (self.leftovers.selected + len - 1) % len
            }
            "enter" if len > 0 => {
                self.inspect_leftover(self.leftovers.selected.min(len - 1), window, cx);
            }
            "d" if len > 0 => {
                self.overlay = Some(Overlay::RemoveLeftover(
                    self.leftovers.selected.min(len - 1),
                ))
            }
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }

    pub(super) fn leftovers_view(
        &self,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child("Leftover worktrees"))
            .child(dialog_text(chrome).child(
                "Their work remains on disk. Reopen a worktree to continue, or remove it to discard its work.",
            ));
        let mut rows = div().flex().flex_col().gap(px(2.)).mt(px(4.));
        for (i, entry) in self.leftovers.entries.iter().enumerate() {
            rows = rows.child(
                list_row(i == self.leftovers.selected, chrome)
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

    pub(super) fn remove_leftover_view(
        &self,
        i: usize,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        dialog(chrome, max_h)
            .child(dialog_title(div()).child("Remove leftover worktree?"))
            .child(
                dialog_text(chrome)
                    .font_family(MONO)
                    .text_size(px(12.))
                    .child(model::tilde(&self.leftovers.entries[i].path, home)),
            )
            .child(
                dialog_text(chrome).child("Yes deletes any uncommitted work and the local branch."),
            )
            .child(
                dialog_buttons()
                    .child(self.no_button("No", chrome, cx))
                    .child(
                        primary_button("remove-confirm", "Yes", "Y", chrome).on_click(
                            cx.listener(move |this, _, _, cx| this.remove_leftover(i, cx)),
                        ),
                    ),
            )
    }
}
