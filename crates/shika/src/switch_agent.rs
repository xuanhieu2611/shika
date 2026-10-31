//! Switch agent: starting another CLI in the selected task's worktree.
use super::*;

impl Shika {
    /// Why Switch agent is unavailable for the card at `index`.
    fn switch_blocker(&self, index: usize) -> Option<&'static str> {
        let card = &self.cards[index];
        if card.lead.is_some() {
            return Some("The Lead cannot switch agent.");
        }
        if card.creating {
            return Some("Wait for setup to finish before switching agents.");
        }
        if card.closing {
            return Some("This task is closing.");
        }
        if card.lead_publishing {
            return Some(control::LEAD_PUBLISHING);
        }
        if card.session.is_none() {
            return Some("This task has no agent to switch.");
        }
        let host = lock(&card.agent.state);
        if host.has_draft() {
            return Some("Send or clear the unsent text in the agent terminal before switching.");
        }
        if model::activity_requires_confirmation(
            card.status,
            host.exited,
            host.submission != card.submitted,
        ) {
            return Some("Wait for the agent to finish before switching agents.");
        }
        None
    }

    pub(super) fn switch_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if let Some(why) = self.switch_blocker(index) {
            self.message(why.into());
            cx.notify();
            return;
        }
        let session_id = self.cards[index]
            .session
            .as_ref()
            .map(|session| session.id.clone())
            .unwrap_or_default();
        let current = self.cards[index].launch_preset.clone();
        let listed = self
            .offered_presets()
            .iter()
            .position(|preset| preset.id == current)
            .unwrap_or(0);
        let chrome = self.chrome(window);
        let prompt = cx.new(|cx| name_input::NameInput::new(String::new(), chrome, cx));
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::SwitchAgent {
            session_id,
            prompt,
            index: listed,
            error: None,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn move_switch(&mut self, delta: isize) {
        let len = self.offered_presets().len();
        if len == 0 {
            return;
        }
        if let Some(Overlay::SwitchAgent { index, .. }) = &mut self.overlay {
            *index = (*index as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    fn switch_error(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(Overlay::SwitchAgent { error, .. }) = &mut self.overlay {
            *error = Some(text);
        }
        cx.notify();
    }

    pub(super) fn confirm_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (session_id, prompt_text, preset_index) = {
            let Some(Overlay::SwitchAgent {
                session_id,
                prompt,
                index,
                ..
            }) = &self.overlay
            else {
                return;
            };
            if prompt.read(cx).composing() {
                return;
            }
            (
                session_id.clone(),
                prompt.read(cx).value().to_string(),
                *index,
            )
        };
        let Some(card_index) = self.cards.iter().position(|card| {
            card.session
                .as_ref()
                .is_some_and(|session| session.id == session_id)
        }) else {
            self.switch_error("This task has no agent to switch.".into(), cx);
            return;
        };
        if let Some(why) = self.switch_blocker(card_index) {
            self.switch_error(why.into(), cx);
            return;
        }
        let prompt = switch_prompt(&prompt_text).map(str::to_owned);
        if self.catalog.is_none() {
            self.switch_error("Resolving login-shell PATH...".into(), cx);
            return;
        }
        let presets = self.offered_presets();
        let Some(preset) = presets.get(preset_index) else {
            self.switch_error("No agents available.".into(), cx);
            return;
        };
        let preset_id = preset.id.clone();
        let control = if self.cards[card_index].started_by.is_some() {
            match (
                self.cards[card_index].worker_token.clone(),
                self.control.as_ref(),
            ) {
                (Some(token), Some(server)) => Some(WorkerEnv {
                    socket: server.socket(),
                    token,
                    bin_dir: server.bin_dir(),
                }),
                (None, _) => {
                    self.switch_error("This worker has no control token.".into(), cx);
                    return;
                }
                (_, None) => {
                    self.switch_error("Shika's control socket is not running.".into(), cx);
                    return;
                }
            }
        } else {
            None
        };
        let measured = lock(&self.cards[card_index].agent.state)
            .measured
            .unwrap_or(TerminalSize::new(32, 100));
        let pane = Pane::new(
            self.core.clone(),
            true,
            self.terminal_opacity(),
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        );
        pane.terminal.resize(measured);
        let size = PtySize::new(measured.rows, measured.cols);
        let sink = agent_sink(pane.terminal.clone(), pane.state.clone());
        let state = pane.state.clone();
        self.cards[card_index].next_agent = Some(pane);
        let core = self.core.clone();
        let prompt_for_card = prompt.clone();
        let switched_id = session_id.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let session = core.session_switch_agent(
                        &session_id,
                        &preset_id,
                        prompt.as_deref(),
                        size,
                        sink,
                        control,
                    )?;
                    if let Some(pty) = session.pty {
                        bind_host(&core, &state, pty);
                    }
                    Ok::<_, shika_core::Error>(session)
                })
                .await;
            let _ =
                this.update_in(cx, |this, window, cx| {
                    this.busy = false;
                    match result {
                        Ok(session) => {
                            this.install_switched_agent(session, prompt_for_card, window, cx)
                        }
                        Err(err) => {
                            if let Some(card) = this.cards.iter_mut().find(|card| {
                                card.session.as_ref().is_some_and(|s| s.id == switched_id)
                            }) {
                                card.next_agent = None;
                            }
                            this.switch_error(err.to_string(), cx);
                        }
                    }
                });
        })
        .detach();
    }

    fn install_switched_agent(
        &mut self,
        session: Session,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((index, pane)) = self.cards.iter_mut().enumerate().find_map(|(index, card)| {
            card.session
                .as_ref()
                .is_some_and(|current| current.id == session.id)
                .then(|| card.next_agent.take())
                .flatten()
                .map(|pane| (index, pane))
        }) else {
            if matches!(self.overlay, Some(Overlay::SwitchAgent { .. })) {
                self.overlay = None;
                self.overlay_return_focus = None;
            }
            return;
        };
        let now = Instant::now();
        let activity = {
            let mut host = lock(&pane.state);
            apply_switch_turn(&mut host, prompt.as_deref(), !session.cli_titled, now)
        };
        let submitted = lock(&pane.state).submission;
        let card = &mut self.cards[index];
        let previous = std::mem::replace(&mut card.agent, pane);
        if card.launch_preset == "terminal" {
            card.shell_serial += 1;
            let mut shell = previous;
            shell.shell_number = card.shell_serial;
            card.shells.push(shell);
        }
        card.preset = session.preset_name.clone();
        card.launch_preset = session.preset_id.clone();
        if matches!(card.launch, Launch::Recover { .. }) {
            card.launch = Launch::Task;
        }
        card.usage_exit_seen = false;
        if !card.named {
            card.title = model::card_title(&session.title);
        }
        card.status = activity.status;
        card.since = activity.since;
        card.activity = activity;
        card.submitted = submitted;
        card.evidence = activity::OutputEvidence::default();
        card.lifecycle = lifecycle::Lifecycle::default();
        card.lifecycle_checked = None;
        // A draft task watches the new CLI's own title from its first turn.
        if session.cli_titled {
            card.title_watch.finish();
        } else {
            card.title_watch = TitleWatch::default();
        }
        card.active_tab = 0;
        card.session = Some(session);
        self.overlay = None;
        self.overlay_return_focus = None;
        self.selection.set(Some(Selection::Card(index)));
        self.cards[index].reveal_active_tab();
        let handle = self.cards[index].agent.view.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(super) fn switch_dialog(
        &self,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (prompt, index, error, checkout, terminal) = {
            let Some(Overlay::SwitchAgent {
                session_id,
                prompt,
                index,
                error,
            }) = &self.overlay
            else {
                return dialog(chrome, max_h);
            };
            let checkout = self.cards.iter().any(|card| {
                card.is_checkout() && card.session.as_ref().is_some_and(|s| s.id == *session_id)
            });
            let terminal = self.cards.iter().any(|card| {
                card.launch_preset == "terminal"
                    && card.session.as_ref().is_some_and(|s| s.id == *session_id)
            });
            (prompt.clone(), *index, error.clone(), checkout, terminal)
        };
        prompt.update(cx, |input, _| input.set_chrome(*chrome));
        let presets = self.offered_presets();
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child(if terminal {
                "Start agent?"
            } else {
                "Switch agent?"
            }))
            .child(dialog_text(chrome).child(if terminal {
                "The current terminal keeps running as a Shell tab."
            } else {
                "This stops the current agent only after the new one starts."
            }))
            .child(dialog_text(chrome).child(if checkout {
                "Your checkout, branch, commits, and uncommitted files stay. Shell tabs stay."
            } else {
                "The worktree, branch, commits, and uncommitted files stay. Shell tabs stay."
            }))
            .child(dialog_text(chrome).child("No transcript is copied."))
            .child(dialog_text(chrome).child("Prompt"))
            .child(prompt.clone())
            .child(
                dialog_text(chrome)
                    .text_color(chrome.ink_3)
                    .child("What should this agent do? Leave it empty to type in the terminal."),
            )
            .child(dialog_text(chrome).child("Agent"));
        if self.catalog.is_none() {
            panel = panel.child(dialog_text(chrome).child("Resolving login-shell PATH..."));
        } else if presets.is_empty() {
            panel = panel
                .child(dialog_text(chrome).child("No agents available"))
                .child(
                    dialog_text(chrome)
                        .text_color(chrome.ink_3)
                        .child("Turn one on in Settings."),
                );
        } else {
            let mut rows = div()
                .id("switch-agents")
                .max_h(px(BASE_LIST_MAX))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            for (i, preset) in presets.iter().enumerate() {
                let detail = preset
                    .path
                    .as_deref()
                    .map(|path| model::tilde(path, home))
                    .unwrap_or_else(|| format!("{} not found on PATH", preset.binary));
                rows = rows.child(
                    list_row(i == index, chrome)
                        .id(SharedString::from(format!("switch-agent-{}", preset.id)))
                        .gap(px(12.))
                        .cursor_pointer()
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
                            if this.busy {
                                return;
                            }
                            if let Some(Overlay::SwitchAgent { index, .. }) = &mut this.overlay {
                                *index = i;
                            }
                            window.focus(&this.focus, cx);
                            cx.notify();
                        })),
                );
            }
            panel = panel.child(rows);
        }
        if let Some(error) = error {
            panel = panel.child(dialog_text(chrome).child(error.to_string()));
        }
        let label = if self.busy {
            "Starting..."
        } else if terminal {
            "Start"
        } else {
            "Switch"
        };
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .child(hint("j k", "choose", chrome))
                    .child(hint("tab", "edit prompt", chrome))
                    .child(hint(
                        "enter",
                        if terminal { "start" } else { "switch" },
                        chrome,
                    ))
                    .child(hint("esc", "cancel", chrome)),
            )
            .child(
                dialog_buttons()
                    .child(self.cancel_button("Cancel", chrome, cx))
                    .child(primary_button("switch-agent", label, "↵", chrome).on_click(
                        cx.listener(|this, _, window, cx| this.confirm_switch(window, cx)),
                    )),
            )
    }
}

/// The prompt for a same-worktree switch. Empty starts the CLI with no
/// positional argument. A non-empty prompt is that argument; Core rejects
/// one the CLI would misread.
pub(super) fn switch_prompt(prompt: &str) -> Option<&str> {
    (!prompt.trim().is_empty()).then_some(prompt)
}

/// Arms the new agent host after a switch. `name` is true while the task is
/// still on its draft branch: then naming works as it does after New, from
/// the prompt or the first typed line. Otherwise nothing names the task. An
/// empty prompt leaves activity Waiting, so the card stays idle until the
/// author types. A prompt counts as a submission, and the returned activity
/// is already in that launch turn.
pub(super) fn apply_switch_turn(
    host: &mut HostState,
    prompt: Option<&str>,
    name: bool,
    now: Instant,
) -> activity::Activity {
    let mut activity = activity::Activity::new(now);
    if let Some(prompt) = prompt {
        host.seed_launch_prompt(prompt, name, now);
        host.launch_turn = false;
        activity.hold_for_launch();
        let _ = activity.advance(
            activity::Signal::Unknown,
            host.last_submission,
            None,
            now,
            false,
        );
    } else if !name {
        host.prompt.finish();
    }
    activity
}
