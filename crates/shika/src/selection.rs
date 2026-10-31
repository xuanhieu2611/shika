//! Which card and project are selected, row order, moving between agents,
//! and the Go to tab and Go to agent actions.
use super::*;

/// Jump to a task-local tab. Zero is the pinned agent, so Cmd+1 selects it
/// until Settings points that chord at an agent.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = shika, no_json)]
pub(super) struct SelectTerminal(pub(super) usize);

/// Select the nth card in the column, skipping project headers. Zero is the
/// first card. A missing card does nothing.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = shika, no_json)]
pub(super) struct SelectAgent(pub(super) usize);

impl Shika {
    pub(super) fn selected_card(&self) -> Option<usize> {
        match *self.selection {
            Some(Selection::Card(i)) if i < self.cards.len() => Some(i),
            _ => None,
        }
    }

    /// The project the selection belongs to: a selected header, a card of
    /// it (its own, its Lead, or a worker of its Lead), or its resting Lead.
    pub(super) fn selected_project(&self) -> Option<&str> {
        match &*self.selection {
            Some(Selection::Project(id) | Selection::Resting(id)) => Some(id),
            Some(Selection::Card(i)) => self.cards.get(*i).map(|c| c.project.as_str()),
            None => None,
        }
    }

    /// The CLI's display name for a saved Lead. The catalog loads a moment
    /// after launch; until then the preset id stands in.
    pub(super) fn cli_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.iter().find(|p| p.id == id))
            .map_or(id, |preset| preset.name.as_str())
    }

    /// The project a clicked header aimed project commands at, while the
    /// selection is still the one it was clicked under.
    pub(super) fn target_project(&self) -> Option<&str> {
        model::header_target(self.project_target.as_ref(), self.selection.epoch())
            .filter(|id| self.projects.iter().any(|p| p.id == *id))
    }

    /// The project a project-scoped command aims at: the clicked header's,
    /// else the shown one. Anything describing what is on screen uses
    /// [`Self::shown_project`].
    pub(super) fn project_id(&self) -> Option<String> {
        if let Some(project) = self.target_project() {
            return Some(project.to_string());
        }
        self.shown_project()
    }

    /// The project the selection belongs to, never the header target.
    pub(super) fn shown_project(&self) -> Option<String> {
        match &*self.selection {
            Some(Selection::Project(p) | Selection::Resting(p)) => Some(p.clone()),
            Some(Selection::Card(i)) => self.cards.get(*i).map(|c| c.project.clone()),
            _ => self.projects.first().map(|p| p.id.clone()),
        }
    }

    /// Walking order for `j` / `k`, Cmd+] / Cmd+[ and Go to agent N: the Leads
    /// section (no headers), then each project's header and cards.
    fn rows(&self) -> Vec<Selection> {
        let layout = self.column_layout();
        let mut rows = vec![];
        for entry in &layout.leads {
            match entry.kind {
                model::LeadKind::Running { .. } => {
                    rows.extend(entry.cards().into_iter().map(Selection::Card))
                }
                model::LeadKind::Resting { .. } => {
                    rows.push(Selection::Resting(entry.project.clone()))
                }
            }
        }
        for entry in &layout.projects {
            rows.push(Selection::Project(entry.project.clone()));
            rows.extend(entry.cards.iter().copied().map(Selection::Card));
        }
        rows
    }

    pub(super) fn move_selection(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        // Headers, including a project with no cards, are not stops.
        let Some(row) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) else {
            return;
        };
        self.selection.set(Some(row));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn move_agent(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if let Some(row) = adjacent_agent(&self.rows(), self.selection.as_ref(), delta) {
            self.select_agent_keeping_surface(row, window, cx);
        }
    }

    /// Select an agent row and keep the surface that invoked it: a terminal
    /// stays in the destination terminal, the Changes panel stays in the
    /// panel, and the cards stay on the cards. A resting Lead has no
    /// terminal, so leaving a terminal for it lands on the cards.
    fn select_agent_keeping_surface(
        &mut self,
        row: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel_focused = self.changes.open && self.changes.focus.is_focused(window);
        let terminal_focused = !self.focus.is_focused(window) && !panel_focused;
        let card = matches!(row, Selection::Card(_));
        self.selection.set(Some(row));
        if terminal_focused {
            if card && !self.busy {
                // A failed-setup card shows no terminal that takes typing.
                self.focus_shown(window, cx);
            } else {
                window.focus(&self.focus, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn select_agent(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(row) = nth_agent(&self.rows(), index) else {
            return;
        };
        self.select_agent_keeping_surface(row, window, cx);
    }

    /// A click on a project header. With a terminal or Lead chat on screen it
    /// changes neither that nor the focus: the project only becomes the target
    /// of New, Lead, and Base branch until the selection changes. Otherwise
    /// the header is selected and the cards get the focus.
    pub(super) fn click_project(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shown().takes_typing() {
            self.project_target = Some((project, self.selection.epoch()));
        } else {
            self.project_target = None;
            self.selection.set(Some(Selection::Project(project)));
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// What the selection puts on screen, as far as typing goes.
    pub(super) fn shown(&self) -> model::Shown {
        match &*self.selection {
            None => model::Shown::Nothing,
            Some(Selection::Project(_)) => model::Shown::Header,
            Some(Selection::Resting(_)) => model::Shown::RestingLead,
            Some(Selection::Card(i)) => match self.cards.get(*i) {
                None => model::Shown::Nothing,
                Some(card) if card.launch_error.is_some() => model::Shown::FailedCard,
                Some(_) => model::Shown::Card,
            },
        }
    }
}

/// The nth agent in column order, skipping project headers. None when the
/// column does not have that many.
pub(super) fn nth_agent(rows: &[Selection], n: usize) -> Option<Selection> {
    rows.iter()
        .filter(|row| !matches!(row, Selection::Project(_)))
        .nth(n)
        .cloned()
}
