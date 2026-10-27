//! Quit guard and explicit leftover recovery. No startup auto-launch or poller.
use super::*;

impl Shika {
    pub(super) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Quit { .. })) {
            return;
        }
        if self.busy {
            self.message("Wait for the current operation before quitting.".into());
            cx.notify();
            return;
        }
        let previous = self.overlay.take().map(Box::new);
        self.overlay = Some(Overlay::Quit {
            previous,
            focus: window.focused(cx),
        });
        window.focus(&self.focus, cx);
        self.observe_dialog(Instant::now());
        cx.notify();
    }

    pub(super) fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !matches!(self.overlay, Some(Overlay::Quit { .. })) {
            return;
        }
        if let Some(Overlay::Quit { previous, focus }) = self.overlay.take() {
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
        if self.busy || !matches!(self.overlay, Some(Overlay::Quit { .. })) {
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
            .child(dialog_buttons()
                .child(dialog_button("cancel-quit", "Cancel", "esc", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                .child(primary_button("confirm-quit", if self.busy { "Saving..." } else { "Quit Shika" }, "⌘↵", chrome)
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
        let Some(entry) = self.leftovers.get(index) else {
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
                            .offered_presets()
                            .iter()
                            .position(|p| entry.recovery.as_ref().is_some_and(|r| r.cli == p.id))
                            .unwrap_or(0);
                        this.overlay = Some(Overlay::Recover { entry, index });
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
        let Some(Overlay::Recover { entry, index }) = &self.overlay else {
            return;
        };
        let Some(preset) = self.offered_presets().get(*index).cloned() else {
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
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(Overlay::Recover { entry, index }) = &self.overlay else {
            unreachable!()
        };
        let presets = self.offered_presets();
        let can_resume = presets.get(*index).is_some_and(|p| {
            entry
                .recovery
                .as_ref()
                .is_some_and(|r| r.cli == p.id && r.conversation.is_some())
        });
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child("Reopen worktree?"))
            .child(dialog_text(chrome).font_family(MONO).child(entry.branch.clone()))
            .child(dialog_text(chrome).child("Keeps its branch, commits, and uncommitted files. Setup is not run again."))
            .child(dialog_text(chrome).child(if can_resume {
                "Resume the saved agent conversation, or start a fresh agent in this worktree."
            } else {
                "No saved conversation is available for this agent. Start fresh in the same worktree."
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
                        if let Some(Overlay::Recover { index, .. }) = &mut this.overlay {
                            *index = i;
                        }
                        cx.notify();
                    })),
            );
        }
        let mut buttons = dialog_buttons().child(
            dialog_button("cancel-recovery", "Cancel", "esc", chrome).on_click(cx.listener(
                |this, _, _, cx| {
                    this.overlay = Some(Overlay::Leftovers);
                    cx.notify();
                },
            )),
        );
        if !presets.is_empty() {
            if can_resume {
                buttons = buttons.child(
                    dialog_button("fresh-recovery", "Start fresh", "", chrome).on_click(
                        cx.listener(|this, _, window, cx| this.start_recovery(true, window, cx)),
                    ),
                );
            }
            buttons = buttons.child(
                primary_button(
                    "start-recovery",
                    if can_resume {
                        "Resume agent"
                    } else {
                        "Start fresh"
                    },
                    "⌘↵",
                    chrome,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.start_recovery(false, window, cx)),
                ),
            );
        }
        panel.child(buttons)
    }
}
