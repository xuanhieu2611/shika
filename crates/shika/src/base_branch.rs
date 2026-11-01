//! The Base branch dialog's work: resolving and prefetching each project's
//! base, listing local branches, the highlight, and applying a choice.
use super::*;

/// Six base-branch rows. The list scrolls after that.
pub(super) const BASE_LIST_MAX: f32 = 220.;

/// The branch New starts from in `project`. `text` is the field, always
/// being typed into; `error` is why the last Enter was refused.
/// `choices` is None until the branches already on disk have been read.
/// `highlight` is a row in the filtered list. None with an empty field
/// uses the default branch; None with an unknown name fetches it.
pub(super) struct BaseDialog {
    project: String,
    text: String,
    error: Option<String>,
    choices: Option<KnownBranches>,
    highlight: Option<usize>,
    /// The Base branch list, so Up and Down can bring the highlight into view.
    scroll: gpui::ScrollHandle,
}

impl Shika {
    /// Resolves the base branch of each project in `ids`, off the main thread.
    /// A project whose base cannot be read loses its label.
    pub(super) fn refresh_bases(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move {
                    ids.into_iter()
                        .map(|id| {
                            let base = core.project_base(&id).ok();
                            (id, base)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for (id, base) in resolved {
                    match base {
                        Some(base) => this.bases.insert(id, base),
                        None => this.bases.remove(&id),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Starts fetching the project's base branch while the picker is open,
    /// so New starts from the remote's latest without waiting for it.
    pub(super) fn prefetch_base(&mut self, project: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = project.clone();
            cx.background_executor()
                .spawn(async move {
                    let _ = core.prefetch_base(&id);
                })
                .await;
            // A branch that was only on origin may exist locally now.
            let _ = this.update(cx, |this, cx| this.refresh_bases(vec![project], cx));
        })
        .detach();
    }

    /// The Base branch dialog for one project, from Cmd+Shift+B or its header label.
    pub(super) fn open_base(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let text = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.base_branch.clone())
            .unwrap_or_default();
        self.refresh_bases(vec![project.clone()], cx);
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Base(BaseDialog {
            project: project.clone(),
            text,
            error: None,
            choices: None,
            highlight: None,
            scroll: gpui::ScrollHandle::new(),
        }));
        self.load_base_branches(project, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Reads branches already on disk. The dialog stays usable while this runs.
    fn load_base_branches(&mut self, project: String, cx: &mut Context<Self>) {
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = project.clone();
            let listed = cx
                .background_executor()
                .spawn(async move { core.project_branches(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let at = {
                    let Some(Overlay::Base(BaseDialog {
                        project: open,
                        text,
                        choices,
                        highlight,
                        ..
                    })) = &mut this.overlay
                    else {
                        return;
                    };
                    if *open != project {
                        return;
                    }
                    let Ok(found) = listed else {
                        return;
                    };
                    let matched = model::branch_matches(&found.names, text).len();
                    *highlight = model::branch_highlight_after_type(matched, text);
                    let at = *highlight;
                    *choices = Some(found);
                    at
                };
                if let Some(at) = at {
                    this.scroll_base_to(at);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The branch Enter would save: the highlighted row, or the field.
    pub(super) fn chosen_base_name(&self) -> String {
        let Some(Overlay::Base(BaseDialog {
            text,
            choices,
            highlight,
            ..
        })) = &self.overlay
        else {
            return String::new();
        };
        let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
        let matched = model::branch_matches(names, text);
        let shown: Vec<&str> = matched.iter().map(|index| names[*index].as_str()).collect();
        model::branch_to_apply(text, &shown, *highlight).to_string()
    }

    /// After the field changes, the highlight returns to the first match.
    pub(super) fn sync_base_highlight(&mut self) {
        let at = {
            let Some(Overlay::Base(BaseDialog {
                text,
                choices,
                highlight,
                ..
            })) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::branch_highlight_after_type(matched, text);
            *highlight
        };
        if let Some(at) = at {
            self.scroll_base_to(at);
        }
    }

    pub(super) fn move_base_highlight(&mut self, delta: isize) {
        let at = {
            let Some(Overlay::Base(BaseDialog {
                text,
                choices,
                highlight,
                ..
            })) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::move_branch_highlight(*highlight, matched, delta);
            *highlight
        };
        if let Some(at) = at {
            self.scroll_base_to(at);
        }
    }

    /// Brings the open dialog's row `at` into view.
    fn scroll_base_to(&self, at: usize) {
        if let Some(Overlay::Base(base)) = &self.overlay {
            base.scroll.scroll_to_item(at);
        }
    }

    /// Saves `branch` once core finds it, or shows why not.
    /// Empty goes back to the default branch.
    pub(super) fn apply_base(
        &mut self,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(Overlay::Base(BaseDialog { project, .. })) = &self.overlay else {
            return;
        };
        let project = project.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let id = project.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.set_project_base(&id, &branch) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(saved) => {
                        if let Some(p) = this.projects.iter_mut().find(|p| p.id == saved.id) {
                            *p = saved;
                        }
                        if matches!(&this.overlay, Some(Overlay::Base(BaseDialog { project: open, .. })) if *open == project)
                        {
                            this.overlay = None;
                            this.restore_overlay_focus(window, cx);
                        }
                        this.refresh_bases(vec![project], cx);
                    }
                    Err(e) => {
                        if let Some(Overlay::Base(BaseDialog { error, .. })) = &mut this.overlay {
                            *error = Some(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Base branch dialog's keys. The field takes typing; Up and Down
    /// move the highlight.
    pub(super) fn base_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
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
                    let changed = if let Some(Overlay::Base(BaseDialog { text, error, .. })) =
                        &mut self.overlay
                    {
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
                        if let Some(Overlay::Base(BaseDialog { text, error, .. })) =
                            &mut self.overlay
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
    }

    pub(super) fn base_view(
        &self,
        base: &BaseDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let BaseDialog {
            project,
            text,
            error,
            choices,
            highlight,
            scroll,
        } = base;
        let name = self
            .projects
            .iter()
            .find(|p| &p.id == project)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        let default = self.bases.get(project).and_then(|b| b.default_name.clone());
        let field = base_field(text, default, chrome);
        let names = choices
            .as_ref()
            .map(|choices| choices.names.as_slice())
            .unwrap_or(&[]);
        let checked_out = choices
            .as_ref()
            .and_then(|choices| choices.checked_out.clone());
        let matched = model::branch_matches(names, text);
        let highlight = highlight
            .as_ref()
            .copied()
            .filter(|index| *index < matched.len());
        let action = if self.chosen_base_name().trim().is_empty() {
            "Use default branch"
        } else {
            "Set base branch"
        };
        let fetch_hint = choices.as_ref().is_some_and(|choices| choices.has_origin)
            && !text.is_empty()
            && matched.is_empty()
            && error.is_none();
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child(format!("Base branch for {name}")))
            .child(
                dialog_text(chrome)
                    .child("New agents branch from it. Running agents keep their base."),
            )
            .child(field.mt(px(4.)));
        if !matched.is_empty() {
            let mut rows = div()
                .id("base-branch-list")
                .flex()
                .flex_col()
                .gap(px(2.))
                .max_h(px(BASE_LIST_MAX))
                .overflow_y_scroll()
                .track_scroll(scroll);
            for (row, index) in matched.iter().copied().enumerate() {
                let branch = names[index].clone();
                rows = rows.child(base_branch_row(
                    row,
                    branch,
                    highlight == Some(row),
                    checked_out.as_deref(),
                    chrome,
                    cx,
                ));
            }
            panel = panel.child(rows);
        }
        if fetch_hint {
            panel = panel.child(
                dialog_text(chrome)
                    .text_color(chrome.ink_3)
                    .child("Not on this machine. Enter fetches it from origin."),
            );
        }
        panel
            .when_some(error.clone(), |d, error| {
                d.child(dialog_text(chrome).text_color(chrome.ink_1).child(error))
            })
            .child(
                dialog_buttons()
                    .child(self.cancel_button("Cancel", chrome, cx))
                    .child(
                        primary_button("set-base", action, "↵", chrome).on_click(cx.listener(
                            |this, _, window, cx| {
                                let branch = this.chosen_base_name();
                                this.apply_base(branch, window, cx);
                            },
                        )),
                    ),
            )
    }
}

/// The branch field, with the default branch as its placeholder.
fn base_field(text: &str, default: Option<String>, chrome: &Chrome) -> gpui::Stateful<gpui::Div> {
    text_field("base-branch-value", 360., true, chrome)
        .when(!text.is_empty(), |d| d.child(text.to_string()))
        .child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
        .when_some(default.filter(|_| text.is_empty()), |d, default| {
            d.child(div().text_color(chrome.ink_4).child(default))
        })
}

/// One local branch under the field. A click applies it.
fn base_branch_row(
    row: usize,
    branch: String,
    highlighted: bool,
    checked_out: Option<&str>,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let label = branch.clone();
    let checked = checked_out == Some(label.as_str());
    list_row(highlighted, chrome)
        .id(("base-branch-row", row))
        .w_full()
        .justify_between()
        .gap(px(8.))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            this.apply_base(branch.clone(), window, cx);
        }))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .font_family(MONO)
                .text_size(px(12.))
                .child(label),
        )
        .when(checked, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(chrome.ink_3)
                    .child("checked out"),
            )
        })
}
