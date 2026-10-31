//! Rename task: opening the dialog and applying the display name.
use super::*;

/// The Rename task dialog, bound to the clicked or selected card's agent view.
pub(super) struct RenameDialog {
    target: gpui::EntityId,
    input: Entity<name_input::NameInput>,
    error: Option<String>,
}

impl Shika {
    pub(super) fn rename_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let index = match &self.overlay {
            Some(Overlay::CardMenu(menu)) => {
                menu.target_index(self.cards.iter().map(|card| card.agent.view.entity_id()))
            }
            None => self.selected_card(),
            _ => return,
        };
        let Some(card) = index.and_then(|index| self.cards.get(index)) else {
            return;
        };
        let Some(session) = card.session.as_ref().filter(|_| card.lead.is_none()) else {
            return;
        };
        let target = card.agent.view.entity_id();
        let title = session.title.clone();
        if self.overlay.is_none() {
            self.overlay_return_focus = window.focused(cx);
        }
        let chrome = self.chrome(window);
        let input = cx.new(|cx| name_input::NameInput::new(title, chrome, cx));
        window.focus(&input.focus_handle(cx), cx);
        self.overlay = Some(Overlay::Rename(RenameDialog {
            target,
            input,
            error: None,
        }));
        cx.notify();
    }

    pub(super) fn apply_task_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Rename(RenameDialog { target, input, .. })) = &self.overlay else {
            return;
        };
        if input.read(cx).composing() {
            return;
        }
        let title = input.read(cx).value().to_string();
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.agent.view.entity_id() == *target)
        else {
            self.cancel_overlay(window, cx);
            return;
        };
        let Some(session) = &self.cards[index].session else {
            self.cancel_overlay(window, cx);
            return;
        };
        match self.core.session_set_title(&session.id, &title) {
            Ok(session) => {
                self.cards[index].title = session.title.clone();
                self.cards[index].named = true;
                self.cards[index].session = Some(session);
                self.overlay = None;
                self.restore_overlay_focus(window, cx);
            }
            Err(error) => {
                if let Some(Overlay::Rename(RenameDialog { error: shown, .. })) = &mut self.overlay
                {
                    *shown = Some(error.to_string());
                }
            }
        }
        cx.notify();
    }

    /// Rename task's keys. The native text input owns typing, selection,
    /// clipboard, and IME.
    pub(super) fn rename_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        let Some(Overlay::Rename(RenameDialog { input, .. })) = &self.overlay else {
            return;
        };
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
    }

    pub(super) fn rename_view(
        &self,
        rename: &RenameDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let RenameDialog {
            target,
            input,
            error,
        } = rename;
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
}
