//! Pointer menus: the card menu and the resting Lead menu, opening them,
//! their views, and their actions.
use super::*;

/// A context menu stays bound to the clicked card, not the current selection
/// or a vector index that asynchronous setup cleanup could invalidate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CardMenuAction {
    Rename,
    Close,
}

pub(super) struct CardMenu {
    pub(super) target: gpui::EntityId,
    pub(super) position: gpui::Point<Pixels>,
    pub(super) action: CardMenuAction,
}

impl CardMenu {
    pub(super) fn target_index(
        &self,
        mut ids: impl Iterator<Item = gpui::EntityId>,
    ) -> Option<usize> {
        ids.position(|id| id == self.target)
    }
}

impl Shika {
    pub(super) fn open_card_menu(
        &mut self,
        index: usize,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(card) = self.cards.get(index) else {
            return;
        };
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::CardMenu(CardMenu {
            target: card.agent.view.entity_id(),
            position,
            action: if card.renamable() {
                CardMenuAction::Rename
            } else {
                CardMenuAction::Close
            },
        }));
        // Do not select the card or enter its terminal just to open a menu.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn activate_card_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::CardMenu(menu)) if menu.action == CardMenuAction::Rename => {
                self.rename_task(window, cx)
            }
            Some(Overlay::CardMenu(_)) => self.close_from_card_menu(window, cx),
            _ => {}
        }
    }

    pub(super) fn close_from_card_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::CardMenu(menu)) = &self.overlay else {
            return;
        };
        let index = menu.target_index(self.cards.iter().map(|card| card.agent.view.entity_id()));
        self.overlay = None;
        // Close captures its opening focus for its own confirmation flow.
        self.restore_overlay_focus(window, cx);
        if let Some(index) = index {
            let panel_focused = self.changes.open && self.changes.focus.is_focused(window);
            let terminal_focused = !self.focus.is_focused(window) && !panel_focused;
            self.selection.set(Some(Selection::Card(index)));
            if terminal_focused {
                // Never leave a hidden, previously selected terminal focused.
                self.focus_terminal(window, cx);
            }
            self.close(window, cx);
        }
        cx.notify();
    }

    /// Right-click on a resting Lead: a menu with one item, Forget Lead.
    pub(super) fn open_resting_menu(
        &mut self,
        project: String,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::RestingMenu { project, position });
        // Do not select the row just to open a menu.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The menu's Forget Lead: the same dialog Cmd+Shift+W opens.
    pub(super) fn forget_from_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::RestingMenu { project, .. }) = &self.overlay else {
            return;
        };
        let project = project.clone();
        self.overlay = None;
        let returns_to = self.overlay_return_focus.take();
        self.open_lead_close(project, None, window, cx);
        if self.busy {
            self.overlay_return_focus = returns_to;
        }
    }

    /// A pointer-anchored menu with a transparent, click-consuming backdrop.
    pub(super) fn card_menu_view(
        &self,
        menu: &CardMenu,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = chrome.row_selected;
        let can_rename = menu
            .target_index(self.cards.iter().map(|card| card.agent.view.entity_id()))
            .is_some_and(|index| self.cards[index].renamable());
        let panel = dialog_shell(400., window.viewport_size().height, chrome)
            .w_auto()
            .p(px(8.))
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                list_row(menu.action == CardMenuAction::Rename && can_rename, chrome)
                    .id("card-menu-rename")
                    .gap(px(20.))
                    .justify_between()
                    .text_color(if can_rename {
                        chrome.ink_1
                    } else {
                        chrome.ink_4
                    })
                    .when(can_rename, |row| {
                        row.cursor_pointer().hover(move |style| style.bg(hover))
                    })
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered
                            && can_rename
                            && let Some(Overlay::CardMenu(menu)) = &mut this.overlay
                        {
                            menu.action = CardMenuAction::Rename;
                            cx.notify();
                        }
                    }))
                    .child("Rename task…")
                    .child(kbd("\u{2318}\u{21e7}R", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.rename_task(window, cx);
                    })),
            )
            .child(
                list_row(menu.action == CardMenuAction::Close, chrome)
                    .id("card-menu-close")
                    .gap(px(20.))
                    .justify_between()
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        if *hovered && let Some(Overlay::CardMenu(menu)) = &mut this.overlay {
                            menu.action = CardMenuAction::Close;
                            cx.notify();
                        }
                    }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child("Close task")
                    .child(kbd("\u{2318}\u{21e7}W", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_from_card_menu(window, cx);
                    })),
            );
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.cancel_overlay(window, cx);
            }))
            .child(gpui::deferred(
                gpui::anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            ))
    }

    /// The resting Lead's pointer menu: one item, Forget Lead.
    pub(super) fn resting_menu_view(
        &self,
        position: gpui::Point<Pixels>,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = chrome.row_selected;
        let panel = dialog_shell(400., window.viewport_size().height, chrome)
            .w_auto()
            .p(px(8.))
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                list_row(true, chrome)
                    .id("resting-menu-forget")
                    .gap(px(20.))
                    .justify_between()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child("Forget Lead")
                    .child(kbd("\u{2318}\u{232b}", chrome.sunken, chrome.ink_3))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.forget_from_menu(window, cx);
                    })),
            );
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.cancel_overlay(window, cx);
            }))
            .child(gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            ))
    }
}
