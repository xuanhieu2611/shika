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
            Overlay::RestingMenu(RestingMenu { project, .. }) => ("resting-menu", hash(project)),
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
                    | Overlay::RestingMenu(_)
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
            (Some(Overlay::RestingMenu(_)), Delete) => self.forget_from_menu(window, cx),
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

    /// The root key router. Each phase takes the keystroke or passes it on,
    /// in this fixed order: Cmd+Q, the dialogs that take every keystroke,
    /// Ctrl+Q out of a terminal, Settings prefix typing, Publish and Base
    /// branch, shortcut recording, dialog chords, then a plain key for the
    /// open overlay or the cards.
    pub(super) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if self.quit_chord_key(stroke, window, cx) {
            return;
        }
        if self.typing_dialog_key(event, window, cx) {
            return;
        }
        if self.leave_terminal_key(stroke, window, cx) {
            return;
        }
        if self.settings_prefix_key(stroke, window, cx) {
            return;
        }
        if self.text_field_dialog_key(event, window, cx) {
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
            self.overlay_plain_key(key, window, cx);
            return;
        }
        self.cards_key(key, window, cx);
    }

    /// Exact Cmd+Q asks to quit, whatever is open or focused.
    fn quit_chord_key(
        &mut self,
        stroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
            && stroke.key == "q"
        {
            self.request_quit(window, cx);
            cx.stop_propagation();
            return true;
        }
        false
    }

    /// The overlays that take every keystroke, modifiers included, before
    /// any other handling: a Lead question, worktree setup, Rename, the card
    /// menu, and Switch agent.
    fn typing_dialog_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.chat_question_key(event, window, cx);
            return true;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.worktree_setup_key(event, window, cx);
            return true;
        }
        if matches!(self.overlay, Some(Overlay::Rename(_))) {
            self.rename_key(event, window, cx);
            return true;
        }
        // Menu typing belongs to the app, never to the previously focused PTY.
        if matches!(self.overlay, Some(Overlay::CardMenu(_))) {
            self.card_menu_key(event, window, cx);
            return true;
        }
        if matches!(self.overlay, Some(Overlay::SwitchAgent(_))) {
            self.switch_key(event, window, cx);
            return true;
        }
        false
    }

    /// Ctrl+Q leaves a focused terminal. Escape is typed into the program.
    fn leave_terminal_key(
        &mut self,
        stroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
            return true;
        }
        false
    }

    /// Publish and Base branch take every keystroke after Settings prefix
    /// typing has had its turn.
    fn text_field_dialog_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(&self.overlay, Some(Overlay::Publish(_))) {
            self.publish_key(event, window, cx);
            return true;
        }
        if matches!(&self.overlay, Some(Overlay::Base(_))) {
            self.base_key(event, window, cx);
            return true;
        }
        false
    }

    /// A plain key while an overlay is open: dropped in the input grace
    /// window or while busy, then Settings navigation, then the open
    /// dialog's handler. The cards and terminals never see it.
    fn overlay_plain_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_ignores_key(key) {
            cx.stop_propagation();
            return;
        }
        if self.busy {
            return;
        }
        if self.settings_nav_key(key, window, cx) {
            return;
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
            Some(Overlay::RestingMenu(_)) => {
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
            Some(Overlay::Settings(_)) => self.settings_row_key(key, window, cx),
            // Typed into above, before the modifier check.
            Some(Overlay::LeadQuestion(_))
            | Some(Overlay::WorktreeSetup(_))
            | Some(Overlay::Base(_))
            | Some(Overlay::Publish(_))
            | Some(Overlay::CardMenu(_))
            | Some(Overlay::Rename(_))
            | Some(Overlay::SwitchAgent(_))
            | None => {}
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The few keys the focused cards take: `j`, `k`, the arrows, and Enter.
    fn cards_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
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
