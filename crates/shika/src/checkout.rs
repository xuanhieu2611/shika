//! Checkout entry and branch reads. No git writes, polling, setup or journal.
use super::*;

/// One local read per project at a time. An event during a read requests one
/// more read, so a selection/Ready cannot be lost to a stale in-flight result.
#[derive(Default)]
pub(super) struct Head {
    name: Option<String>,
    reading: bool,
    again: bool,
}

impl Head {
    fn begin(&mut self) -> bool {
        if self.reading {
            self.again = true;
            false
        } else {
            self.reading = true;
            true
        }
    }

    /// Returns whether a selection/Ready arrived during this read.
    fn finish(&mut self, name: Option<String>) -> bool {
        self.name = name;
        self.reading = false;
        std::mem::take(&mut self.again)
    }
}

impl Shika {
    pub(super) fn checkout_branch(&self, project: &str) -> Option<&str> {
        self.checkout_heads.get(project)?.name.as_deref()
    }

    pub(super) fn refresh_checkout_head(&mut self, project: String, cx: &mut Context<Self>) {
        let head = self.checkout_heads.entry(project.clone()).or_default();
        if !head.begin() {
            return;
        }
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let id = project.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.checkout_head(&id).map(|head| head.label().to_owned()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.projects.iter().any(|p| p.id == project) {
                    this.checkout_heads.remove(&project);
                    return;
                }
                let head = this.checkout_heads.entry(project.clone()).or_default();
                if head.finish(result.ok()) {
                    this.refresh_checkout_head(project, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Refresh once on each selection change, not on redraw or terminal focus.
    pub(super) fn sync_checkout_selection(&mut self, cx: &mut Context<Self>) {
        if self.checkout_selection_epoch == self.selection.epoch() {
            return;
        }
        self.checkout_selection_epoch = self.selection.epoch();
        if let Some(project) = self
            .selected_card()
            .filter(|&i| self.cards[i].is_checkout())
            .map(|i| self.cards[i].project.clone())
        {
            self.refresh_checkout_head(project, cx);
        }
    }

    fn show_checkout(
        &mut self,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(i) = self
            .cards
            .iter()
            .position(|c| c.project == project && c.is_checkout())
        else {
            return false;
        };
        self.selection.set(Some(Selection::Card(i)));
        self.refresh_checkout_head(project.to_owned(), cx);
        self.focus_terminal(window, cx);
        cx.notify();
        true
    }

    pub(super) fn new_in_checkout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(project) = self.project_id() else {
            self.add_project(window, cx);
            return;
        };
        if self.show_checkout(&project, window, cx) {
            return;
        }
        self.refresh_checkout_head(project.clone(), cx);
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::Picker {
            project,
            index: 0,
            mode: PickerMode::Checkout,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn start_checkout(
        &mut self,
        project: String,
        preset: CliPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Tab may have moved the picker to a project that already has one.
        if self.show_checkout(&project, window, cx) {
            self.overlay = None;
            self.overlay_return_focus = None;
            return;
        }
        self.begin_launch(
            project,
            preset,
            false,
            None,
            Launch::Checkout,
            None,
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_reads_coalesce_without_losing_selection_or_ready() {
        let mut head = Head::default();
        assert!(head.begin());
        for _ in 0..20 {
            assert!(!head.begin());
        }
        assert!(head.finish(Some("dev".into())));
        assert_eq!(head.name.as_deref(), Some("dev"));
        assert!(head.begin());
        assert!(!head.finish(Some("abc1234".into())));
        assert_eq!(head.name.as_deref(), Some("abc1234"));
        assert!(!head.reading);
        assert!(!head.again);
    }

    #[test]
    fn a_failed_branch_read_drops_the_old_label_and_can_retry() {
        let mut head = Head::default();
        assert!(head.begin());
        assert!(!head.finish(Some("dev".into())));
        assert!(head.begin());
        assert!(!head.finish(None));
        assert!(head.name.is_none());
        assert!(head.begin());
        assert!(!head.finish(Some("another-branch".into())));
        assert_eq!(head.name.as_deref(), Some("another-branch"));
    }

    #[test]
    fn picker_titles_distinguish_checkout_from_new_and_lead() {
        assert_eq!(PickerMode::Checkout.title(), "In checkout");
        assert_eq!(PickerMode::Task.title(), "New agent");
        assert_eq!(PickerMode::Lead.title(), "New Lead");
    }
}
