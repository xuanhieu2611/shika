//! Closing a task: the safe-close check, discard, push, or stop, and the card
//! playing its exit from the column. Also removing a leftover worktree.
use super::*;

/// A closed card on its way out. It is no longer in `cards`, so nothing can
/// select, count, or act on it; it only paints until its exit has played.
pub(super) struct Departing {
    pub(super) card: Card,
    pub(super) home: Home,
    /// Drawn before this card, the one that followed it in its group when it
    /// closed. `None` puts it at the end of its group.
    pub(super) before: Option<gpui::EntityId>,
    pub(super) key: u64,
}

enum CloseCheck {
    Normal(SessionGitState),
    Switched(shika_core::SwitchedBranchClose),
}

impl Shika {
    pub(super) fn remove_card(&mut self, index: usize) {
        self.project_target = None;
        self.cards.remove(index);
        let next = match self.selection.clone() {
            Some(Selection::Card(at)) if at > index => Some(Selection::Card(at - 1)),
            Some(Selection::Card(at)) if at == index => {
                if self.cards.is_empty() {
                    self.projects
                        .first()
                        .map(|p| Selection::Project(p.id.clone()))
                } else {
                    Some(Selection::Card(index.min(self.cards.len() - 1)))
                }
            }
            selection => selection,
        };
        self.selection.set(next);
        self.last_revealed_selection = None;
    }

    /// Takes a closed card out of `cards` and lets it play its exit in place
    /// for [`model::EXIT`]. Selection is the caller's. With macOS Reduce
    /// motion the card goes at once.
    pub(super) fn depart(&mut self, index: usize, cx: &mut Context<Self>) {
        self.project_target = None;
        let project = self.cards[index].project.clone();
        // It leaves from the group it was shown in, before the card that
        // followed it there.
        let layout = self.column_layout();
        let (home, group) = match layout
            .leads
            .iter()
            .find(|entry| entry.cards().contains(&index))
        {
            Some(entry) => (Home::Leads(project), entry.cards()),
            None => (
                Home::Project(project.clone()),
                layout
                    .projects
                    .iter()
                    .find(|entry| entry.project == project)
                    .map(|entry| entry.cards.clone())
                    .unwrap_or_default(),
            ),
        };
        let before = group
            .into_iter()
            .skip_while(|&i| i != index)
            .nth(1)
            .map(|i| self.cards[i].agent.view.entity_id());
        let card = self.cards.remove(index);
        self.last_revealed_selection = None;
        if cx.reduce_motion() {
            return;
        }
        self.departing_serial += 1;
        let key = self.departing_serial;
        self.departing.push(Departing {
            card,
            home,
            before,
            key,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(model::EXIT).await;
            let _ = this.update(cx, |this, cx| {
                this.departing.retain(|d| d.key != key);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(project) = self.selected_resting().map(str::to_string) {
            self.open_lead_close(project, None, window, cx);
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating {
            if let Some(control) = &self.cards[index].launch_control {
                control.cancel();
            }
            cx.notify();
            return;
        }
        if self.cards[index].launch_error.is_some() {
            let was = window.focused(cx);
            self.remove_card(index);
            self.land_after(was, window, cx);
            cx.notify();
            return;
        }
        let Some(session) = &self.cards[index].session else {
            return;
        };
        let id = session.id.clone();
        if self.cards[index].lead.is_some() {
            // A Lead is stopped, not closed: its conversation is kept.
            let project = self.cards[index].project.clone();
            self.open_lead_close(project, Some(id), window, cx);
            return;
        }
        if self.cards[index].lead_publishing {
            self.message(control::LEAD_PUBLISHING.into());
            cx.notify();
            return;
        }
        let task_id = id.clone();
        let working = self.cards[index].running();
        if self.cards[index].is_checkout() {
            self.overlay_return_focus = window.focused(cx);
            if working {
                self.overlay = Some(Overlay::CheckoutClose { index });
                window.focus(&self.focus, cx);
                cx.notify();
            } else {
                self.finish_close(index, 0, window, cx);
            }
            return;
        }
        let core = self.core.clone();
        self.overlay_return_focus = window.focused(cx);
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match core.session_git_state(&id, working) {
                        Ok(state) => Ok(CloseCheck::Normal(state)),
                        Err(shika_core::Error::TaskBranchChanged { .. }) => core
                            .session_switched_close_check(&id)
                            .map(CloseCheck::Switched),
                        Err(e) => Err(e),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(CloseCheck::Switched(preview)) => {
                        this.overlay = Some(Overlay::SwitchedClose {
                            index,
                            preview,
                            working,
                        });
                        window.focus(&this.focus, cx);
                    }
                    Ok(CloseCheck::Normal(state)) if state.requires_confirmation() => {
                        this.overlay = Some(Overlay::Close { index, state });
                        window.focus(&this.focus, cx);
                    }
                    Ok(_) => this.finish_close(index, 0, window, cx),
                    Err(e) => {
                        this.overlay_return_focus = None;
                        this.lead_dialog_failed(&task_id, e.to_string());
                        this.message(e.to_string());
                    }
                };
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn finish_close(
        &mut self,
        index: usize,
        action: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(session) = self.cards.get(index).and_then(|c| c.session.as_ref()) else {
            return;
        };
        // No numeric task action may turn Checkout's Stop into Discard/Push.
        if session.is_checkout() && !matches!(action, 0 | 4) {
            return;
        }
        let id = session.id.clone();
        let task_id = id.clone();
        // Use the same check before showing choices and before removing the
        // task. Redraw bytes and draft typing are never active-turn evidence.
        let working = self.cards[index].running();
        let preview = match &self.overlay {
            Some(Overlay::SwitchedClose { preview, .. }) if action == 3 => Some(preview.clone()),
            _ => None,
        };
        let core = self.core.clone();
        self.busy = true;
        self.cards[index].closing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match action {
                        1 => core.session_discard(&id),
                        2 => core.session_push_and_close(&id),
                        4 => core.session_stop_checkout(&id),
                        3 => match preview {
                            Some(preview) => core.session_close_switched(&id, &preview),
                            None => Err(shika_core::Error::CloseNeedsConfirmation),
                        },
                        _ => core.session_close(&id, working),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                // Find the card again: positions can change while Close runs.
                let index = this
                    .cards
                    .iter()
                    .position(|c| c.session.as_ref().is_some_and(|s| s.id == task_id));
                match result {
                    Ok(()) => {
                        this.lead_dialog_succeeded(&task_id, control::close_outcome(action).into());
                        if let Some(index) = index {
                            this.depart(index, cx);
                            let next = if this.cards.is_empty() {
                                this.projects
                                    .first()
                                    .map(|p| Selection::Project(p.id.clone()))
                            } else {
                                Some(Selection::Card(index.min(this.cards.len() - 1)))
                            };
                            this.selection.set(next);
                            // The replacement takes the closed card's place.
                            // Revealing it would undo manual scrolling during
                            // the row's exit. A Lead restore still reveals.
                            this.last_revealed_selection = this.selection.clone();
                        }
                        this.overlay = None;
                        let was = this.overlay_return_focus.take();
                        this.land_after(was, window, cx);
                    }
                    Err(e) => {
                        if let Some(index) = index {
                            this.cards[index].closing = false;
                        }
                        this.lead_dialog_failed(&task_id, e.to_string());
                        this.message(e.to_string());
                    }
                };
                this.settle_lead_dialog(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn remove_leftover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(entry) = self.leftovers.get(index) else {
            return;
        };
        let path = entry.path.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.leftover_remove(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.leftovers.remove(index);
                        this.overlay = if this.leftovers.is_empty() {
                            None
                        } else {
                            Some(Overlay::Leftovers)
                        };
                    }
                    Err(e) => this.message(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
