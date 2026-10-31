//! Where keyboard focus lands after selection changes, dialogs, and closes.
use super::*;

impl Shika {
    pub(super) fn restore_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focus) = self.overlay_return_focus.take() else {
            // Nothing was saved: land on what can be typed into.
            self.land_focus(window, cx);
            return;
        };
        let cards = focus == self.focus;
        let changes = self.changes.open && focus == self.changes.focus;
        let selected_terminal = self
            .selected_card()
            .map(|index| self.cards[index].active_pane().view.focus_handle(cx));
        let chat_focus = self
            .selected_chat()
            .is_some_and(|chat| chat.read(cx).owns_focus(&focus, cx));
        if cards || changes || chat_focus || selected_terminal.as_ref() == Some(&focus) {
            window.focus(&focus, cx);
        } else if (self.cards.iter().any(|card| {
            std::iter::once(&card.agent)
                .chain(card.shells.iter())
                .any(|pane| pane.view.focus_handle(cx) == focus)
        }) || self
            .lead_chats
            .values()
            .any(|chat| chat.read(cx).owns_focus(&focus, cx)))
            && !self.busy
        {
            // A notification may have selected another task while editing.
            // Never restore focus to the old task's now-hidden terminal.
            self.focus_terminal(window, cx);
        } else {
            self.land_focus(window, cx);
        }
    }

    /// `focus_shown` when no dialog is up; the cards when one is, since
    /// dialogs take their keys through them.
    fn land_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            window.focus(&self.focus, cx);
        } else {
            self.focus_shown(window, cx);
        }
    }

    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.focus_pane(window, cx);
    }

    /// Focus follows what can be typed into. The cards get it only when
    /// nothing on screen can take typing (the author's Ctrl+Q is the other
    /// way there). For the places that lose their focus without being asked:
    /// a task closing, a Lead stopping, a setup cancelled, a launch started.
    /// An open dialog keeps the focus it took. Unlike `focus_terminal`, a
    /// running launch does not hold it back: the card being created is on
    /// screen and keeps the keys, so `j` cannot move the selection away.
    pub(super) fn focus_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        if self.shown().takes_typing() {
            self.focus_pane(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// `focus_shown` for a place that loses its focus unasked, except that an
    /// author who was on the cards (Ctrl+Q) or the Changes panel stays there.
    /// `was` is the focus from before the event; only focus that pointed at
    /// something now gone or hidden is replaced.
    pub(super) fn land_after(
        &mut self,
        was: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            return;
        }
        if let Some(was) = was {
            if was == self.focus {
                window.focus(&self.focus, cx);
                return;
            }
            if self.changes.open && was == self.changes.focus {
                window.focus(&self.changes.focus, cx);
                return;
            }
        }
        self.focus_shown(window, cx);
    }

    pub(super) fn focus_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.selected_chat() {
            window.focus(&chat.focus_handle(cx), cx);
            cx.notify();
            return;
        }
        if let Some(i) = self.selected_card() {
            let c = &self.cards[i];
            c.reveal_active_tab();
            let pane = c.active_pane();
            window.focus(&pane.view.focus_handle(cx), cx);
            cx.notify();
        }
    }
}
