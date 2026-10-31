//! Adding, reloading, and removing saved projects.
use super::*;

impl Shika {
    pub(super) fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let was = window.focused(cx);
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let result = cx
                    .background_executor()
                    .spawn(async move { core.add_project(&path) })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match result {
                        Ok(added) => {
                            this.projects = this.core.projects().unwrap_or_default();
                            this.refresh_bases(vec![added.project.id.clone()], cx);
                            this.selection
                                .set(Some(Selection::Project(added.project.id)));
                            // The new header shows nothing to type into, so
                            // the hidden terminal must not keep the focus.
                            this.land_after(was, window, cx);
                            if let Some(note) = added.note {
                                this.message(note);
                            }
                        }
                        Err(e) => this.message(e.to_string()),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Re-reads the saved projects, which hold each project's saved Lead.
    pub(super) fn reload_projects(&mut self) {
        if let Ok(projects) = self.core.projects() {
            self.projects = projects;
        }
    }

    pub(super) fn remove_project(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let removed_id = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.remove_project(&id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.cards.retain(|c| c.project != removed_id);
                        this.projects.retain(|p| p.id != removed_id);
                        this.lead_chats.remove(&removed_id);
                        this.chat_questions
                            .retain(|(_, project, _)| project != &removed_id);
                        this.bases.remove(&removed_id);
                        this.checkout_heads.remove(&removed_id);
                        let next = this
                            .projects
                            .first()
                            .map(|p| Selection::Project(p.id.clone()));
                        this.selection.set(next);
                        this.leftovers = this.core.leftovers_list().unwrap_or_default();
                        this.overlay = None;
                    }
                    Err(e) => this.message(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
