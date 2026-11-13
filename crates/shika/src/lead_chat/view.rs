//! The chat view: message list, empty and welcome states, the dock with
//! activity steps and banners, and the view's keys.
use super::{ANTLERS_ICON, ChatAction, LeadChat};
use crate::{Chrome, appearance};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

impl LeadChat {
    /// The activity lines, oldest first, as `(finished, text)`.
    fn activity_lines(&self) -> Vec<(bool, String)> {
        if self.activity == "Working..." {
            self.steps.lines(self.activity)
        } else {
            let mut lines = self.steps.lines(self.activity);
            if let Some(last) = lines.last_mut() {
                last.1 = self.activity.to_string();
            }
            lines
        }
    }

    /// The view's chrome: secondary and tertiary ink made legible on the
    /// terminal surface.
    fn view_chrome(&self) -> Chrome {
        let mut chrome = self.chrome;
        chrome.ink_3 = appearance::legible(
            chrome.ink_3,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        chrome.ink_2 = appearance::legible(
            chrome.ink_2,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        chrome
    }

    /// Gives the composer ink that is legible on its raised field.
    fn sync_composer_chrome(&self, chrome: &Chrome, cx: &mut Context<Self>) {
        let mut editor_chrome = *chrome;
        editor_chrome.ink_1 = appearance::legible(chrome.ink_1, chrome.raised, chrome.ink_1, 4.5);
        editor_chrome.ink_3 = appearance::legible(chrome.ink_3, chrome.raised, chrome.ink_1, 4.5);
        self.composer.update(cx, |input, cx| {
            if input.set_chrome(editor_chrome) {
                cx.notify();
            }
        });
    }

    /// Keys of the whole view: Enter on the action or "New messages", Tab
    /// cycling, Ctrl+Q back to the cards, Escape to stop, and list scrolling.
    fn chat_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        if key.key == "enter" && self.action_focus.is_focused(window) {
            if self.busy {
                cx.emit(ChatAction::Interrupt);
            } else if self.live {
                self.submit(cx);
            } else {
                cx.emit(ChatAction::Resume);
            }
            cx.stop_propagation();
            return;
        }
        if key.key == "enter" && self.new_focus.is_focused(window) {
            self.newest(cx);
            cx.stop_propagation();
            return;
        }
        if key.key == "tab" {
            if key.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
        }
        if key.key == "q" && key.modifiers.control {
            cx.emit(ChatAction::Back);
            cx.stop_propagation();
        }
        if key.key == "escape" && self.busy && !self.composer.read(cx).composing() {
            cx.emit(ChatAction::Interrupt);
            cx.stop_propagation();
        }
        if self.focus.is_focused(window) {
            match key.key.as_str() {
                "home" => self.list.scroll_to(gpui::ListOffset::default()),
                "end" => self.list.scroll_to_end(),
                "pageup" | "pagedown" => {
                    let offset = self.list.logical_scroll_top();
                    let step = if key.key == "pageup" {
                        offset.item_ix.saturating_sub(8)
                    } else {
                        (offset.item_ix + 8).min(self.rows.len())
                    };
                    self.list.scroll_to(gpui::ListOffset {
                        item_ix: step,
                        offset_in_item: offset.offset_in_item,
                    });
                }
                _ => {}
            }
        }
        cx.notify();
    }

    /// "Earlier messages" above the list while older history is not loaded.
    fn older_button(chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        crate::dialog_button("older-lead-messages", "Earlier messages", "", chrome).on_click(
            cx.listener(|this, _, _, cx| this.load(this.records.first().map(|m| m.record.id), cx)),
        )
    }

    /// The empty state of a starting or resting Lead: the mark and one line.
    fn resting_state(&self, chrome: &Chrome) -> gpui::Div {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .child(mark_tile(chrome))
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .text_color(chrome.ink_3)
                    .child(if self.starting {
                        "Starting Lead..."
                    } else {
                        "Lead is not running"
                    }),
            )
    }

    /// The dock under the list: activity steps, error and retry banners,
    /// the docked composer, the resting Resume row, and the back hint.
    fn dock(
        &self,
        chrome: &Chrome,
        composer: Option<AnyElement>,
        hint: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .max_w(px(640.))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .pt(px(12.))
            .when(self.busy, |d| d.child(self.activity_lane(chrome)))
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(19.))
                        .text_color(chrome.ink_2)
                        .child(error),
                )
            })
            .when_some(self.retry.clone(), |d, text| {
                d.child(Self::retry_message(chrome, text, cx))
            })
            .when(self.save_failed, |d| {
                d.child(
                    crate::dialog_button("retry-chat-save", "Retry save", "", chrome)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatAction::RetrySave))),
                )
            })
            .children(composer)
            .when(!self.live && !self.starting, |d| {
                d.child(self.resume_row(chrome, cx))
            })
            .children(hint)
    }

    /// The running turn's step list: finished steps above the current one.
    fn activity_lane(&self, chrome: &Chrome) -> gpui::Div {
        let lines = self.activity_lines();
        let single = lines.len() == 1;
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(12.))
            .when(single, |d| d.h(px(20.)))
            .children(lines.into_iter().map(|(finished, text)| {
                div()
                    .h(px(16.))
                    .line_height(px(16.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(if finished { chrome.ink_4 } else { chrome.ink_3 })
                    .child(text)
            }))
    }

    /// "Work may already have started." with Retry message, after the CLI
    /// rejected a send.
    fn retry_message(chrome: &Chrome, text: String, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .text_size(px(13.))
            .text_color(chrome.ink_2)
            .child("Work may already have started.")
            .child(
                crate::dialog_button("retry-lead-message", "Retry message", "", chrome).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if this.live && !this.busy && !this.saving && !this.save_failed {
                            this.retry = None;
                            this.saving = true;
                            cx.emit(ChatAction::Send(text.clone()));
                            cx.notify();
                        }
                    }),
                ),
            )
    }

    /// "New messages": jumps back to the newest page and follows the tail.
    fn new_messages_button(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        div().flex().justify_center().child(
            crate::dialog_button("new-lead-messages", "New messages", "", chrome)
                .track_focus(&self.new_focus)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.newest(cx);
                    cx.notify();
                })),
        )
    }

    /// "Lead is not running" with Resume, in place of a resting Lead's
    /// composer.
    fn resume_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .text_size(px(13.))
            .text_color(chrome.ink_3)
            .child("Lead is not running")
            .child(
                crate::dialog_button("resume-chat-lead", "Resume", "↵", chrome)
                    .track_focus(&self.action_focus)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatAction::Resume))),
            )
    }
}

impl Render for LeadChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = self.view_chrome();
        let view = cx.entity();
        self.sync_composer_chrome(&chrome, cx);
        if self.rows.is_empty() && !self.loading && self.error.is_none() && self.epoch == 0 {
            self.epoch = 1;
            self.load(None, cx);
        }
        let focused = self.focused(window, cx);
        let enabled = self.live && !self.saving && !self.save_failed && self.has_draft(cx);
        let empty = self.rows.is_empty() && !self.loading;
        let welcome = empty && self.live && !self.starting;
        let list = gpui::list(self.list.clone(), move |index, window, cx| {
            view.update(cx, |this, cx| this.row(index, window, cx))
        })
        .flex_1()
        .min_h_0()
        .w_full();
        let composer = self.composer_box(&chrome, enabled, window, cx);
        let hint = focused.then(|| {
            div()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child("ctrl q back to cards")
                .into_any_element()
        });
        let (welcome_composer, dock_composer) = if welcome {
            (Some(composer), None)
        } else if self.live {
            (None, Some(composer))
        } else {
            (None, None)
        };
        let (welcome_hint, dock_hint) = if welcome { (hint, None) } else { (None, hint) };
        let dock_extras = self.error.is_some() || self.retry.is_some() || self.save_failed;
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(20.))
            .pt(px(20.))
            .pb(px(20.))
            .bg(chrome.term_surface)
            .track_focus(&self.focus)
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_key_down(cx.listener(Self::chat_key_down))
            .child(
                div()
                    .max_w(px(640.))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(self.older, |d| d.child(Self::older_button(&chrome, cx)))
                    .when(self.rows.is_empty() && self.loading, |d| {
                        d.child(loading_state(&chrome))
                    })
                    .when(empty && !welcome, |d| d.child(self.resting_state(&chrome)))
                    .when(welcome, |d| {
                        d.child(welcome_state(&chrome, welcome_composer, welcome_hint))
                    })
                    .when(!self.rows.is_empty(), |d| {
                        // "New messages" floats over the list's bottom edge. In
                        // the dock it resized the list the moment the author
                        // scrolled up, clipping the last rows and moving the
                        // bottom the list was following.
                        d.child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .flex()
                                .flex_col()
                                .child(list)
                                .when(self.newer || !self.list.is_following_tail(), |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .bottom_0()
                                            .left_0()
                                            .right_0()
                                            .child(self.new_messages_button(&chrome, cx)),
                                    )
                                }),
                        )
                    }),
            )
            .when(!welcome || dock_extras, |d| {
                d.child(self.dock(&chrome, dock_composer, dock_hint, cx))
            })
    }
}

/// "Loading conversation..." while the first page loads.
fn loading_state(chrome: &Chrome) -> gpui::Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(chrome.ink_3)
        .child("Loading conversation...")
}

/// A live Lead with no visible history: the mark, "What should we work
/// on?", and the composer as one centered group.
fn welcome_state(
    chrome: &Chrome,
    composer: Option<AnyElement>,
    hint: Option<AnyElement>,
) -> gpui::Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(14.))
        .child(mark_tile(chrome))
        .child(
            div()
                .text_size(px(17.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(chrome.ink_2)
                .child("What should we work on?"),
        )
        .children(composer.map(|c| div().w_full().child(c)))
        .children(hint)
}

/// The antler mark on a raised tile: the Lead's empty and resting states.
fn mark_tile(chrome: &Chrome) -> gpui::Div {
    div()
        .size(px(52.))
        .flex_none()
        .rounded(px(14.))
        .bg(chrome.raised)
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::svg()
                .data(ANTLERS_ICON)
                .size(px(30.))
                .text_color(chrome.ink_2),
        )
}
