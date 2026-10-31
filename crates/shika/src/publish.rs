//! Create PR: when it is available, opening the publish preview, and running
//! the publish pipeline.
use super::*;

impl Shika {
    /// Why Create PR is unavailable for the card at `index`. The shortcut and
    /// a Lead's `shika pr` both ask this, so they never disagree.
    pub(super) fn publish_blocker(&self, index: usize) -> Option<&'static str> {
        let card = &self.cards[index];
        if card.is_checkout() {
            Some("Checkout sessions do not publish.")
        } else if card.lead.is_some() {
            Some("The Lead has no branch, so there is nothing to publish.")
        } else if card.creating || card.running() {
            Some("Wait for the agent to finish before publishing.")
        } else if card.lead_publishing {
            Some(control::LEAD_PUBLISHING)
        } else {
            None
        }
    }

    pub(super) fn create_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].is_checkout() {
            return;
        }
        if let Some(why) = self.publish_blocker(index) {
            self.message(why.into());
            cx.notify();
            return;
        }
        let card = &self.cards[index];
        let Some(session) = &card.session else {
            return;
        };
        let id = session.id.clone();
        let core = self.core.clone();
        self.overlay_return_focus = window.focused(cx);
        self.busy = true;
        self.message("Preparing PR preview...".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.session_publish_preview(&id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(preview) => {
                        let target = preview.target.clone().unwrap_or_default();
                        let title = preview.title.clone();
                        let row = if target.is_empty() { 2 } else { 0 };
                        let chrome = this.chrome(window);
                        let body = cx.new(|cx| {
                            let mut input = chat_input::ChatInput::new(String::new(), chrome, cx);
                            input.label("PR description".into());
                            input.placeholder("Leave empty to use the commit messages.");
                            input.field_id("pr-description");
                            input.newline_on_enter();
                            input.max_chars(shika_core::MAX_PR_BODY_CHARS);
                            input.max_height(px(72.));
                            input.style(chrome, false, true, Vec::new());
                            input
                        });
                        this.overlay = Some(Overlay::Publish {
                            preview,
                            title,
                            body,
                            target,
                            row,
                            error: None,
                        });
                        this.toast = None;
                        window.focus(&this.focus, cx);
                    }
                    Err(e) => {
                        this.overlay_return_focus = None;
                        this.message(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn publish_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let body_text = match &self.overlay {
            Some(Overlay::Publish { body, .. }) => body.read(cx).value().to_string(),
            _ => return,
        };
        let Some(Overlay::Publish {
            preview,
            title,
            target,
            error,
            ..
        }) = &mut self.overlay
        else {
            return;
        };
        if title.trim().is_empty()
            || !preview.branches.contains(target)
            || *target == preview.branch
        {
            *error = Some("Enter a title and choose a different, existing target branch.".into());
            cx.notify();
            return;
        }
        if body_text.contains('\0') || body_text.chars().count() > shika_core::MAX_PR_BODY_CHARS {
            *error = Some(format!(
                "The PR description must be text of at most {} characters.",
                shika_core::MAX_PR_BODY_CHARS
            ));
            cx.notify();
            return;
        }
        if self.cards.iter().any(|c| {
            c.session
                .as_ref()
                .is_some_and(|s| s.id == preview.session_id)
                && c.running()
        }) {
            *error = Some("Wait for the agent to finish before publishing.".into());
            cx.notify();
            return;
        }
        let (preview, title, target) = (preview.clone(), title.clone(), target.clone());
        let session_id = preview.session_id.clone();
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                core.session_publish(&preview, &target, &title, Some(&body_text))
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(published) => {
                        this.overlay = None;
                        this.restore_overlay_focus(window, cx);
                        if let Some(card) = this.cards.iter_mut().find(|c| {
                            c.session.as_ref().is_some_and(|s| s.id == session_id)
                        }) {
                            card.pr = checks::PrWatch::new(&published, Instant::now());
                        }
                        this.message(format!("Published PR: {}", published.url));
                        cx.open_url(&published.url);
                    }
                    Err(e) => {
                        if let Some(Overlay::Publish { error, .. }) = &mut this.overlay {
                            *error = Some(format!("{e} Completed commits and pushes were kept. Cancel and reopen Create PR to retry."));
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
    }
}

/// The proposed new commit. Zero means the worktree is clean; commits already
/// on the branch can still be published.
pub(super) fn publish_change_label(count: usize) -> String {
    match count {
        0 => "Nothing new to commit".into(),
        1 => "1 changed file".into(),
        n => format!("{n} changed files"),
    }
}
