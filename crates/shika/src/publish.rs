//! Create PR: when it is available, opening the publish preview, and running
//! the publish pipeline.
use super::*;

/// The Create PR preview the author confirms.
pub(super) struct PublishDialog {
    preview: PublishPreview,
    title: String,
    /// Empty leaves the PR body to `gh --fill`.
    body: Entity<chat_input::ChatInput>,
    target: String,
    /// 0 is the title, 1 the description, 2 the target.
    row: usize,
    error: Option<String>,
}

impl PublishDialog {
    /// The task this preview publishes.
    pub(super) fn session_id(&self) -> &str {
        &self.preview.session_id
    }
}

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
                        this.overlay = Some(Overlay::Publish(PublishDialog {
                            preview,
                            title,
                            body,
                            target,
                            row,
                            error: None,
                        }));
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
            Some(Overlay::Publish(PublishDialog { body, .. })) => body.read(cx).value().to_string(),
            _ => return,
        };
        let Some(Overlay::Publish(PublishDialog {
            preview,
            title,
            target,
            error,
            ..
        })) = &mut self.overlay
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
                        if let Some(Overlay::Publish(PublishDialog { error, .. })) = &mut this.overlay {
                            *error = Some(format!("{e} Completed commits and pushes were kept. Cancel and reopen Create PR to retry."));
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    /// Create PR's keys. Tab moves between the title, description, and
    /// target; the description field owns its own typing.
    pub(super) fn publish_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if self.busy {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let (describing, composing) = match &self.overlay {
            Some(Overlay::Publish(PublishDialog { row, body, .. })) => {
                (*row == 1, body.read(cx).composing())
            }
            _ => (false, false),
        };
        // Composition belongs to the description field.
        if describing && composing {
            return;
        }
        let chord = stroke.modifiers.platform || stroke.modifiers.control || stroke.modifiers.alt;
        match stroke.key.as_str() {
            "escape" => self.cancel_overlay(window, cx),
            "tab" if !chord => {
                let body = if let Some(Overlay::Publish(PublishDialog { row, body, .. })) =
                    &mut self.overlay
                {
                    *row = if stroke.modifiers.shift {
                        (*row + 2) % 3
                    } else {
                        (*row + 1) % 3
                    };
                    (*row == 1).then(|| body.clone())
                } else {
                    None
                };
                if let Some(body) = body {
                    window.focus(&body.focus_handle(cx), cx);
                } else {
                    window.focus(&self.focus, cx);
                }
            }
            "enter" if describing => {
                let confirm = stroke.modifiers.platform
                    && !stroke.modifiers.control
                    && !stroke.modifiers.alt
                    && !stroke.modifiers.shift;
                if confirm {
                    self.publish_pr(window, cx);
                } else if chord {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                } else {
                    return;
                }
            }
            "enter" => self.publish_pr(window, cx),
            _ if describing => return,
            _ => {
                if let Some(Overlay::Publish(PublishDialog {
                    row,
                    title,
                    target,
                    error,
                    ..
                })) = &mut self.overlay
                {
                    let value = if *row == 0 { title } else { target };
                    if stroke.key == "backspace" {
                        value.pop();
                    } else if !stroke.modifiers.platform
                        && !stroke.modifiers.control
                        && !stroke.modifiers.alt
                        && let Some(typed) = &stroke.key_char
                    {
                        for ch in typed.chars().filter(|c| !c.is_control()) {
                            if value.len() < 256 {
                                value.push(ch);
                            }
                        }
                    }
                    *error = None;
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn publish_view(
        &self,
        publish: &PublishDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let PublishDialog {
            preview,
            title,
            body,
            target,
            row,
            error,
        } = publish;
        body.update(cx, |input, _| {
            input.style(*chrome, false, true, Vec::new());
        });
        let description = body.clone();
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child("Create PR?"))
            .child(
                dialog_text(chrome)
                    .font_family(MONO)
                    .child(format!("{}: {}", preview.repository, preview.branch)),
            )
            .child(dialog_text(chrome).child("Commit and PR title"))
            .child(
                text_field("pr-title", 360., *row == 0, chrome)
                    .overflow_hidden()
                    .child(title.clone())
                    .when(*row == 0, |d| {
                        d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.busy {
                            if let Some(Overlay::Publish(PublishDialog { row, .. })) =
                                &mut this.overlay
                            {
                                *row = 0;
                            }
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(dialog_text(chrome).child("PR description"))
            .child(
                div()
                    .id("pr-body-field")
                    .w(px(360.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .bg(chrome.raised)
                    .border_1()
                    .border_color(if *row == 1 {
                        chrome.focus
                    } else {
                        chrome.line_control
                    })
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if this.busy {
                                return;
                            }
                            let body =
                                if let Some(Overlay::Publish(PublishDialog { row, body, .. })) =
                                    &mut this.overlay
                                {
                                    *row = 1;
                                    Some(body.clone())
                                } else {
                                    None
                                };
                            if let Some(body) = body {
                                window.focus(&body.focus_handle(cx), cx);
                            }
                            cx.notify();
                        }),
                    )
                    .child(description),
            )
            .child(dialog_text(chrome).child("PR target"))
            .child(
                text_field("pr-target", 360., *row == 2, chrome)
                    .overflow_hidden()
                    .child(target.clone())
                    .when(*row == 2, |d| {
                        d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.busy {
                            if let Some(Overlay::Publish(PublishDialog { row, .. })) =
                                &mut this.overlay
                            {
                                *row = 2;
                            }
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }
                    })),
            );
        if preview.target.is_none() {
            panel = panel.child(dialog_text(chrome).child(
                "The recorded base is not an existing GitHub branch. Choose a target explicitly.",
            ));
        }
        if *row == 2 {
            panel = panel.child(
                div()
                    .id("pr-branches")
                    .max_h(px(BASE_LIST_MAX))
                    .overflow_y_scroll()
                    .children(
                        preview
                            .branches
                            .iter()
                            .filter(|b| b.starts_with(target) && **b != preview.branch)
                            .map(|b| {
                                let branch = b.clone();
                                list_row(b == target, chrome)
                                    .id(SharedString::from(format!("pr-branch-{b}")))
                                    .cursor_pointer()
                                    .font_family(MONO)
                                    .child(b.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.busy {
                                            if let Some(Overlay::Publish(PublishDialog {
                                                target,
                                                error,
                                                ..
                                            })) = &mut this.overlay
                                            {
                                                *target = branch.clone();
                                                *error = None;
                                            }
                                            cx.notify();
                                        }
                                    }))
                            }),
                    ),
            );
        }
        let file_count = preview.files.len();
        panel = panel.child(dialog_text(chrome).child(publish_change_label(file_count)));
        if file_count > 0 {
            panel = panel.child(
                div()
                    .id("pr-files")
                    .max_h(px(BASE_LIST_MAX))
                    .overflow_y_scroll()
                    .children(
                        preview
                            .files
                            .iter()
                            .map(|file| dialog_text(chrome).font_family(MONO).child(file.clone())),
                    ),
            );
        }
        if let Some(error) = error {
            panel = panel.child(dialog_text(chrome).child(error.clone()));
        }
        panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(hint("tab", "switch field", chrome))
                    .when(*row == 1, |hints| {
                        hints.child(hint("enter", "new line", chrome))
                    }),
            )
            .child(
                dialog_buttons()
                    .child(self.cancel_button("Cancel", chrome, cx))
                    .child(
                        primary_button(
                            "publish-pr",
                            if self.busy {
                                "Publishing..."
                            } else {
                                "Commit, push, create PR"
                            },
                            if *row == 1 {
                                "\u{2318}\u{21a9}"
                            } else {
                                "\u{21a9}"
                            },
                            chrome,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.publish_pr(window, cx))),
                    ),
            )
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
