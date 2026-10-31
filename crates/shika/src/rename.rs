//! Rename task: opening the dialog and applying the display name.
use super::*;

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
        self.overlay = Some(Overlay::Rename {
            target,
            input,
            error: None,
        });
        cx.notify();
    }

    pub(super) fn apply_task_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::Rename { target, input, .. }) = &self.overlay else {
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
                if let Some(Overlay::Rename { error: shown, .. }) = &mut self.overlay {
                    *shown = Some(error.to_string());
                }
            }
        }
        cx.notify();
    }
}
