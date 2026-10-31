//! The Base branch dialog's work: resolving and prefetching each project's
//! base, listing local branches, the highlight, and applying a choice.
use super::*;

/// Six base-branch rows. The list scrolls after that.
pub(super) const BASE_LIST_MAX: f32 = 220.;

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
        self.base_scroll = gpui::ScrollHandle::new();
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Base {
            project: project.clone(),
            text,
            error: None,
            choices: None,
            highlight: None,
        });
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
                    let Some(Overlay::Base {
                        project: open,
                        text,
                        choices,
                        highlight,
                        ..
                    }) = &mut this.overlay
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
                    this.base_scroll.scroll_to_item(at);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The branch Enter would save: the highlighted row, or the field.
    pub(super) fn chosen_base_name(&self) -> String {
        let Some(Overlay::Base {
            text,
            choices,
            highlight,
            ..
        }) = &self.overlay
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
            let Some(Overlay::Base {
                text,
                choices,
                highlight,
                ..
            }) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::branch_highlight_after_type(matched, text);
            *highlight
        };
        if let Some(at) = at {
            self.base_scroll.scroll_to_item(at);
        }
    }

    pub(super) fn move_base_highlight(&mut self, delta: isize) {
        let at = {
            let Some(Overlay::Base {
                text,
                choices,
                highlight,
                ..
            }) = &mut self.overlay
            else {
                return;
            };
            let names = choices.as_ref().map(|c| c.names.as_slice()).unwrap_or(&[]);
            let matched = model::branch_matches(names, text).len();
            *highlight = model::move_branch_highlight(*highlight, matched, delta);
            *highlight
        };
        if let Some(at) = at {
            self.base_scroll.scroll_to_item(at);
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
        let Some(Overlay::Base { project, .. }) = &self.overlay else {
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
                        if matches!(&this.overlay, Some(Overlay::Base { project: open, .. }) if *open == project)
                        {
                            this.overlay = None;
                            this.restore_overlay_focus(window, cx);
                        }
                        this.refresh_bases(vec![project], cx);
                    }
                    Err(e) => {
                        if let Some(Overlay::Base { error, .. }) = &mut this.overlay {
                            *error = Some(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
