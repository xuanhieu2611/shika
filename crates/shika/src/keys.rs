//! The root key handler and the accidental-typing guards for dialogs.
use super::*;

impl Shika {
    /// Which confirmation dialog is on screen, for the input grace window.
    /// Text-entry dialogs, the picker, and Settings have none.
    fn dialog_identity(&self) -> Option<model::DialogId> {
        fn hash(text: &str) -> u64 {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            hasher.finish()
        }
        Some(match self.overlay.as_ref()? {
            Overlay::Close(dialog) => ("close", dialog.index as u64),
            Overlay::CheckoutClose(dialog) => ("checkout-close", dialog.index as u64),
            Overlay::SwitchedClose(dialog) => ("switched-close", dialog.index as u64),
            Overlay::LeadClose(lead::LeadCloseDialog { project, step, .. }) => (
                "lead-close",
                hash(project) ^ matches!(step, model::LeadClose::Forget { .. }) as u64,
            ),
            Overlay::RestingMenu { project, .. } => ("resting-menu", hash(project)),
            Overlay::RemoveProject(id) => ("remove-project", hash(id)),
            Overlay::RemoveLeftover(i) => ("remove-leftover", *i as u64),
            Overlay::Recover(_) => ("recover", 0),
            Overlay::Quit(_) => ("quit", 0),
            Overlay::Preparation(dialog) => ("preparation", hash(&dialog.project)),
            Overlay::LeadQuestion(dialog) => ("lead-question", dialog.identity()),
            _ => return None,
        })
    }

    /// Starts the grace window for a confirmation dialog newly on screen.
    /// Called from render, the tick, and every key, so a dialog is noticed
    /// whichever comes first.
    pub(super) fn observe_dialog(&mut self, now: Instant) {
        let id = self.dialog_identity();
        self.dialog_grace.observe(id, now);
    }

    /// Whether a key is dropped because the confirmation dialog has just
    /// appeared. Escape always works. Mouse clicks are not keys.
    pub(super) fn dialog_ignores_key(&mut self, key: &str) -> bool {
        let now = Instant::now();
        self.observe_dialog(now);
        key != "escape" && self.dialog_grace.active(now)
    }

    /// The Command chords that choose in a confirmation dialog. True when
    /// the keystroke belonged to one (acted on or deliberately dropped), so
    /// terminals and cards never see it while the dialog is open.
    fn dialog_chord_key(
        &mut self,
        chord: model::DialogChord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use model::DialogChord::{Delete, Primary};
        if !matches!(
            self.overlay,
            Some(
                Overlay::Close(_)
                    | Overlay::CheckoutClose(_)
                    | Overlay::SwitchedClose(_)
                    | Overlay::LeadClose(_)
                    | Overlay::RestingMenu { .. }
                    | Overlay::RemoveProject(_)
                    | Overlay::RemoveLeftover(_)
                    | Overlay::Recover(_)
                    | Overlay::Quit(_)
            )
        ) {
            return false;
        }
        if self.dialog_ignores_key("") || self.busy {
            return true;
        }
        match (&self.overlay, chord) {
            (Some(Overlay::Quit(_)), Primary) => self.confirm_quit(window, cx),
            (Some(Overlay::Recover(_)), Primary) => self.start_recovery(false, window, cx),
            (
                Some(Overlay::CheckoutClose(_) | Overlay::Close(_) | Overlay::SwitchedClose(_)),
                _,
            ) => self.close_chord_key(chord, window, cx),
            (Some(Overlay::LeadClose(_)), Delete) => self.forget_lead_from_dialog(window, cx),
            (Some(Overlay::RestingMenu { .. }), Delete) => self.forget_from_menu(window, cx),
            (Some(Overlay::RemoveProject(id)), Delete) => {
                let id = id.clone();
                self.remove_project(id, cx);
            }
            (Some(Overlay::RemoveLeftover(i)), Delete) => {
                let i = *i;
                self.remove_leftover(i, cx);
            }
            _ => {}
        }
        true
    }

    /// Whether the author has stopped typing in every terminal (agent and
    /// shell) and Lead chat input. A dialog that opens on its own waits for
    /// this, so keystrokes meant for a prompt never land in it.
    pub(super) fn author_quiet(&self, now: Instant, cx: &gpui::App) -> bool {
        let terminals = self.cards.iter().flat_map(|card| {
            std::iter::once(&card.agent)
                .chain(card.shells.iter())
                .map(|pane| lock(&pane.state).last_typed)
        });
        let chats = self
            .leads
            .chats
            .values()
            .map(|chat| chat.read(cx).last_typed);
        model::typing_quiet(terminals.chain(chats), now, control::BELL_TYPING_QUIET)
    }

    pub(super) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
            && stroke.key == "q"
        {
            self.request_quit(window, cx);
            cx.stop_propagation();
            return;
        }
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.chat_question_key(event, window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.worktree_setup_key(event, window, cx);
            return;
        }
        if let Some(Overlay::Rename { input, .. }) = &self.overlay {
            // The native text input owns typing, selection, clipboard, and IME.
            if !input.read(cx).composing()
                && !stroke.modifiers.platform
                && !stroke.modifiers.control
                && !stroke.modifiers.alt
            {
                match stroke.key.as_str() {
                    "enter" => self.apply_task_name(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }
            return;
        }
        // Menu typing belongs to the app, never to the previously focused PTY.
        if matches!(self.overlay, Some(Overlay::CardMenu(_))) {
            if !stroke.modifiers.platform && !stroke.modifiers.control && !stroke.modifiers.alt {
                match stroke.key.as_str() {
                    "enter" => self.activate_card_menu(window, cx),
                    "j" | "k" | "down" | "up" | "tab" => {
                        let can_rename = if let Some(Overlay::CardMenu(menu)) = &self.overlay {
                            menu.target_index(
                                self.cards.iter().map(|card| card.agent.view.entity_id()),
                            )
                            .is_some_and(|index| self.cards[index].renamable())
                        } else {
                            false
                        };
                        if can_rename && let Some(Overlay::CardMenu(menu)) = &mut self.overlay {
                            menu.action = if menu.action == CardMenuAction::Rename {
                                CardMenuAction::Close
                            } else {
                                CardMenuAction::Rename
                            };
                            cx.notify();
                        }
                    }
                    "escape" => self.cancel_overlay(window, cx),
                    _ => {}
                }
                cx.stop_propagation();
            }
            return;
        }
        if matches!(self.overlay, Some(Overlay::SwitchAgent { .. })) {
            if self.busy {
                cx.stop_propagation();
                return;
            }
            let (editing, composing) =
                if let Some(Overlay::SwitchAgent { prompt, .. }) = &self.overlay {
                    (
                        prompt.focus_handle(cx).is_focused(window),
                        prompt.read(cx).composing(),
                    )
                } else {
                    (false, false)
                };
            if composing {
                return;
            }
            let plain = !stroke.modifiers.platform
                && !stroke.modifiers.control
                && !stroke.modifiers.alt
                && !stroke.modifiers.shift;
            if editing {
                if plain {
                    let handled = match stroke.key.as_str() {
                        "enter" => {
                            self.confirm_switch(window, cx);
                            true
                        }
                        "escape" => {
                            self.cancel_overlay(window, cx);
                            true
                        }
                        "tab" => {
                            window.focus(&self.focus, cx);
                            true
                        }
                        "up" => {
                            self.move_switch(-1);
                            true
                        }
                        "down" => {
                            self.move_switch(1);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
                return;
            }
            if plain {
                match stroke.key.as_str() {
                    "j" | "down" => self.move_switch(1),
                    "k" | "up" => self.move_switch(-1),
                    "enter" => self.confirm_switch(window, cx),
                    "escape" => self.cancel_overlay(window, cx),
                    "tab" => {
                        if let Some(Overlay::SwitchAgent { prompt, .. }) = &self.overlay {
                            window.focus(&prompt.focus_handle(cx), cx);
                        }
                    }
                    _ => {}
                }
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // Ctrl+Q leaves a focused terminal. Escape is typed into the program.
        if self.overlay.is_none()
            && !self.focus.is_focused(window)
            && stroke.key == "q"
            && stroke.modifiers.control
            && !stroke.modifiers.platform
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
        {
            window.focus(&self.focus, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(Overlay::Settings {
            section: SettingsSection::Appearance,
            row: PREFIX_ROW,
            edit: Some(text),
            ..
        }) = &mut self.overlay
        {
            match stroke.key.as_str() {
                "enter" => self.commit_setting_edit(window, cx),
                "escape" => {
                    if let Some(Overlay::Settings { edit, .. }) = &mut self.overlay {
                        *edit = None;
                    }
                }
                "backspace" => {
                    text.pop();
                }
                _ => {
                    let typed = stroke.key_char.as_deref().filter(|_| {
                        !stroke.modifiers.platform
                            && !stroke.modifiers.control
                            && !stroke.modifiers.alt
                    });
                    if let Some(ch) = typed.and_then(|t| t.chars().next())
                        && (ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
                        && text.len() < 40
                    {
                        text.push(ch);
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if matches!(&self.overlay, Some(Overlay::Publish { .. })) {
            if self.busy {
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let (describing, composing) = match &self.overlay {
                Some(Overlay::Publish { row, body, .. }) => (*row == 1, body.read(cx).composing()),
                _ => (false, false),
            };
            // Composition belongs to the description field.
            if describing && composing {
                return;
            }
            let chord =
                stroke.modifiers.platform || stroke.modifiers.control || stroke.modifiers.alt;
            match stroke.key.as_str() {
                "escape" => self.cancel_overlay(window, cx),
                "tab" if !chord => {
                    let body = if let Some(Overlay::Publish { row, body, .. }) = &mut self.overlay {
                        *row = if stroke.modifiers.shift {
                            (*row + 2) % 3
                        } else {
                            (*row + 1) % 3
                        };
                        (*row == 1).then(|| body.clone())
                    } else {
                        None
                    };
                    if let Some(body) = body {
                        window.focus(&body.focus_handle(cx), cx);
                    } else {
                        window.focus(&self.focus, cx);
                    }
                }
                "enter" if describing => {
                    let confirm = stroke.modifiers.platform
                        && !stroke.modifiers.control
                        && !stroke.modifiers.alt
                        && !stroke.modifiers.shift;
                    if confirm {
                        self.publish_pr(window, cx);
                    } else if chord {
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    } else {
                        return;
                    }
                }
                "enter" => self.publish_pr(window, cx),
                _ if describing => return,
                _ => {
                    if let Some(Overlay::Publish {
                        row,
                        title,
                        target,
                        error,
                        ..
                    }) = &mut self.overlay
                    {
                        let value = if *row == 0 { title } else { target };
                        if stroke.key == "backspace" {
                            value.pop();
                        } else if !stroke.modifiers.platform
                            && !stroke.modifiers.control
                            && !stroke.modifiers.alt
                            && let Some(typed) = &stroke.key_char
                        {
                            for ch in typed.chars().filter(|c| !c.is_control()) {
                                if value.len() < 256 {
                                    value.push(ch);
                                }
                            }
                        }
                        *error = None;
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if matches!(&self.overlay, Some(Overlay::Base { .. })) {
            if !self.busy {
                let plain = !stroke.modifiers.platform
                    && !stroke.modifiers.control
                    && !stroke.modifiers.alt
                    && !stroke.modifiers.shift;
                match stroke.key.as_str() {
                    "enter" => {
                        let branch = self.chosen_base_name();
                        self.apply_base(branch, window, cx);
                    }
                    "escape" => self.cancel_overlay(window, cx),
                    "up" if plain => self.move_base_highlight(-1),
                    "down" if plain => self.move_base_highlight(1),
                    "backspace" => {
                        let changed =
                            if let Some(Overlay::Base { text, error, .. }) = &mut self.overlay {
                                let changed = text.pop().is_some();
                                if changed {
                                    *error = None;
                                }
                                changed
                            } else {
                                false
                            };
                        if changed {
                            self.sync_base_highlight();
                        }
                    }
                    _ => {
                        let typed = stroke.key_char.as_deref().filter(|_| {
                            !stroke.modifiers.platform
                                && !stroke.modifiers.control
                                && !stroke.modifiers.alt
                        });
                        let changed = if let Some(ch) = typed.and_then(|t| t.chars().next())
                            && model::branch_char(ch)
                        {
                            if let Some(Overlay::Base { text, error, .. }) = &mut self.overlay
                                && text.len() < model::BRANCH_MAX
                            {
                                text.push(ch);
                                *error = None;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if changed {
                            self.sync_base_highlight();
                        }
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.recording_shortcut() {
            self.capture_shortcut(stroke, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(chord) = model::dialog_chord(
            &stroke.key,
            stroke.modifiers.platform,
            stroke.modifiers.control,
            stroke.modifiers.alt,
            stroke.modifiers.shift,
        ) && self.dialog_chord_key(chord, window, cx)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if stroke.modifiers.platform
            || stroke.modifiers.control
            || stroke.modifiers.alt
            || stroke.modifiers.shift
        {
            return;
        }
        let key = stroke.key.as_str();
        if self.overlay.is_some() {
            if self.dialog_ignores_key(key) {
                cx.stop_propagation();
                return;
            }
            if self.busy {
                return;
            }
            let settings_nav = match &self.overlay {
                Some(Overlay::Settings {
                    section, row, edit, ..
                }) => Some((*section, *row, edit.is_some())),
                _ => None,
            };
            if let Some((section, at, editing)) = settings_nav {
                if !editing && matches!(key, "[" | "]") {
                    let delta = if key == "]" { 1 } else { -1 };
                    self.select_section(section.step(delta), window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Agents {
                    self.on_agents_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Projects {
                    self.on_projects_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing && section == SettingsSection::Keyboard {
                    self.on_keyboard_key(at, key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if section == SettingsSection::Usage {
                    self.on_usage_key(key, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if !editing
                    && section == SettingsSection::Appearance
                    && self.on_background_key(at, key, window, cx)
                {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
            }
            match &mut self.overlay {
                Some(Overlay::Picker(_)) => self.picker_key(key, window, cx),
                Some(Overlay::Adjust(_)) => self.adjust_key(key, window, cx),
                Some(Overlay::Preparation(_)) => self.preparation_key(key, window, cx),
                // Escape, and Enter to Stop the Lead, are the only plain keys
                // here. The destructive and publishing choices are Command
                // chords, handled by `dialog_chord_key`.
                Some(Overlay::Close(_) | Overlay::CheckoutClose(_) | Overlay::SwitchedClose(_)) => {
                    self.close_key(key, window, cx)
                }
                Some(Overlay::RestingMenu { .. }) => {
                    if key == "escape" {
                        self.cancel_overlay(window, cx);
                    }
                }
                Some(Overlay::LeadClose(_)) => self.lead_close_key(key, window, cx),
                Some(Overlay::Quit(_)) => self.quit_key(key, window, cx),
                Some(Overlay::Recover(_)) => self.recover_key(key),
                Some(Overlay::RemoveLeftover(_)) => self.remove_leftover_key(key),
                Some(Overlay::Leftovers) => self.leftovers_key(key, window, cx),
                Some(Overlay::RemoveProject(_)) => self.remove_project_key(key, window, cx),
                Some(Overlay::Settings { row, edit, .. }) => {
                    let at = *row;
                    let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
                    let number_row = matches!(at, OPACITY_ROW | BLUR_ROW | FONT_ROW);
                    let max_len = if at == FONT_ROW { 4 } else { 3 };
                    match (edit.as_mut(), key) {
                        (Some(text), _) if digit || (at == FONT_ROW && key == ".") => {
                            let accept = if key == "." {
                                !text.is_empty() && !text.contains('.')
                            } else {
                                true
                            };
                            if accept && text.len() < max_len {
                                text.push_str(key);
                            }
                        }
                        (Some(text), "backspace") => {
                            text.pop();
                        }
                        (Some(_), "escape") => *edit = None,
                        (Some(_), "enter") => self.commit_setting_edit(window, cx),
                        (Some(_), "h" | "l" | "left" | "right") => {}
                        (None, _) if digit && number_row => self.edit_setting(at, key, window, cx),
                        (None, "backspace") if number_row => self.edit_setting(at, "", window, cx),
                        (None, "enter") if at == PREFIX_ROW => {
                            let prefix = self.branch_prefix.clone();
                            self.edit_setting(at, &prefix, window, cx)
                        }
                        (None, "h" | "left") => self.step_setting(at, -1, window, cx),
                        (None, "l" | "right") => self.step_setting(at, 1, window, cx),
                        (None, "enter" | "escape") => self.cancel_overlay(window, cx),
                        (_, "j" | "down" | "tab") => {
                            let next = self.next_appearance_row(at, 1);
                            self.select_setting(next, window, cx)
                        }
                        (_, "k" | "up") => {
                            let next = self.next_appearance_row(at, -1);
                            self.select_setting(next, window, cx)
                        }
                        _ => {}
                    }
                }
                // Typed into above, before the modifier check.
                Some(Overlay::LeadQuestion(_))
                | Some(Overlay::WorktreeSetup(_))
                | Some(Overlay::Base { .. })
                | Some(Overlay::Publish { .. })
                | Some(Overlay::CardMenu(_))
                | Some(Overlay::Rename { .. })
                | Some(Overlay::SwitchAgent { .. })
                | None => {}
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !self.focus.is_focused(window) {
            return;
        }
        match key {
            "j" | "down" => self.move_selection(1, window, cx),
            "k" | "up" => self.move_selection(-1, window, cx),
            "enter" => match self.selection.clone() {
                Some(Selection::Resting(project)) => self.resume_lead(project, window, cx),
                // A failed setup has no terminal to enter: Enter retries.
                _ if self.shown() == model::Shown::FailedCard => self.retry_preparation(window, cx),
                _ => self.focus_terminal(window, cx),
            },
            _ => return,
        }
        cx.stop_propagation();
    }
}
