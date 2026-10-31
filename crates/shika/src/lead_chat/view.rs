//! The chat view: message list, empty and welcome states, the dock with
//! activity steps and banners, and the composer.
use super::{ANTLERS_ICON, ChatAction, LeadChat, SEND_ICON};
use crate::{Chrome, appearance};
use gpui::{
    AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render,
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

    /// The composer: one raised field with no border (the caret shows focus)
    /// and a round 32px action at its bottom right, Send or Stop. One
    /// element builder for both places it lives (the empty state's center
    /// and the bottom dock), so moving it keeps the same entity and focus.
    fn composer_box(
        &self,
        chrome: &Chrome,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = self.busy;
        let (bg, hover) = if busy || !enabled {
            (chrome.sunken, chrome.sunken)
        } else {
            (chrome.primary_bg, chrome.primary_hover)
        };
        let ring = self.action_focus.is_focused(window);
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = if busy { "Stop esc" } else { "Send \u{21b5}" };
        let clickable = busy || enabled;
        div()
            .flex()
            .items_end()
            .rounded(px(24.))
            .min_h(px(48.))
            .max_h(px(144.))
            .bg(chrome.raised)
            .pl(px(20.))
            .pr(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h(px(48.))
                    .flex()
                    .items_center()
                    .py(px(12.))
                    .pr(px(8.))
                    .child(div().flex_1().min_w_0().child(self.composer.clone())),
            )
            .child(
                div().flex_none().pb(px(8.)).child(
                    div()
                        .id(if busy {
                            "stop-lead-reply"
                        } else {
                            "send-lead-message"
                        })
                        .size(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(bg)
                        .track_focus(&self.action_focus)
                        .role(gpui::Role::Button)
                        .aria_label(if busy { "Stop generating" } else { "Send" })
                        .when(ring, |d| {
                            d.shadow(vec![
                                gpui::BoxShadow::new(px(0.), px(0.), chrome.focus.into())
                                    .spread_radius(px(2.)),
                            ])
                        })
                        .when(clickable, |d| {
                            d.cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        cx.emit(ChatAction::Interrupt);
                                    } else {
                                        this.submit(cx);
                                    }
                                }))
                        })
                        .tooltip(move |_, cx| {
                            cx.new(|_| crate::KeyTip {
                                bg: tip_bg,
                                fg: tip_fg,
                                text: tip.into(),
                            })
                            .into()
                        })
                        .child(if busy {
                            div()
                                .size(px(7.))
                                .rounded(px(2.))
                                .bg(chrome.ink_1)
                                .into_any_element()
                        } else {
                            gpui::svg()
                                .data(SEND_ICON)
                                .size(px(16.))
                                .text_color(if enabled {
                                    chrome.primary_fg
                                } else {
                                    chrome.ink_4
                                })
                                .into_any_element()
                        }),
                ),
            )
            .into_any_element()
    }
}

impl Render for LeadChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let view = cx.entity();
        let mut editor_chrome = chrome;
        editor_chrome.ink_1 = appearance::legible(chrome.ink_1, chrome.raised, chrome.ink_1, 4.5);
        editor_chrome.ink_3 = appearance::legible(chrome.ink_3, chrome.raised, chrome.ink_1, 4.5);
        self.composer.update(cx, |input, cx| {
            if input.set_chrome(editor_chrome) {
                cx.notify();
            }
        });
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
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                if key.key == "enter" && this.action_focus.is_focused(window) {
                    if this.busy {
                        cx.emit(ChatAction::Interrupt);
                    } else if this.live {
                        this.submit(cx);
                    } else {
                        cx.emit(ChatAction::Resume);
                    }
                    cx.stop_propagation();
                    return;
                }
                if key.key == "enter" && this.new_focus.is_focused(window) {
                    this.newest(cx);
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
                if key.key == "escape" && this.busy && !this.composer.read(cx).composing() {
                    cx.emit(ChatAction::Interrupt);
                    cx.stop_propagation();
                }
                if this.focus.is_focused(window) {
                    match key.key.as_str() {
                        "home" => this.list.scroll_to(gpui::ListOffset::default()),
                        "end" => this.list.scroll_to_end(),
                        "pageup" | "pagedown" => {
                            let offset = this.list.logical_scroll_top();
                            let step = if key.key == "pageup" {
                                offset.item_ix.saturating_sub(8)
                            } else {
                                (offset.item_ix + 8).min(this.rows.len())
                            };
                            this.list.scroll_to(gpui::ListOffset {
                                item_ix: step,
                                offset_in_item: offset.offset_in_item,
                            });
                        }
                        _ => {}
                    }
                }
                cx.notify();
            }))
            .child(
                div()
                    .max_w(px(640.))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(self.older, |d| {
                        d.child(
                            crate::dialog_button(
                                "older-lead-messages",
                                "Earlier messages",
                                "",
                                &chrome,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load(this.records.first().map(|m| m.record.id), cx)
                            })),
                        )
                    })
                    .when(self.rows.is_empty() && self.loading, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(13.))
                                .text_color(chrome.ink_3)
                                .child("Loading conversation..."),
                        )
                    })
                    .when(empty && !welcome, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(14.))
                                .child(mark_tile(&chrome))
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
                                ),
                        )
                    })
                    .when(welcome, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap(px(14.))
                                .child(mark_tile(&chrome))
                                .child(
                                    div()
                                        .text_size(px(17.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(chrome.ink_2)
                                        .child("What should we work on?"),
                                )
                                .children(welcome_composer.map(|c| div().w_full().child(c)))
                                .children(welcome_hint),
                        )
                    })
                    .when(!self.rows.is_empty(), |d| d.child(list)),
            )
            .when(!welcome || dock_extras, |d| {
                d.child(
                    div()
                        .max_w(px(640.))
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .pt(px(12.))
                        .when(self.busy, |d| {
                            let lines = self.activity_lines();
                            let single = lines.len() == 1;
                            d.child(
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
                                            .text_color(if finished {
                                                chrome.ink_4
                                            } else {
                                                chrome.ink_3
                                            })
                                            .child(text)
                                    })),
                            )
                        })
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
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .text_color(chrome.ink_2)
                                    .child("Work may already have started.")
                                    .child(
                                        crate::dialog_button(
                                            "retry-lead-message",
                                            "Retry message",
                                            "",
                                            &chrome,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if this.live
                                                    && !this.busy
                                                    && !this.saving
                                                    && !this.save_failed
                                                {
                                                    this.retry = None;
                                                    this.saving = true;
                                                    cx.emit(ChatAction::Send(text.clone()));
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    ),
                            )
                        })
                        .when(self.save_failed, |d| {
                            d.child(
                                crate::dialog_button("retry-chat-save", "Retry save", "", &chrome)
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(ChatAction::RetrySave)),
                                    ),
                            )
                        })
                        .when(self.newer || !self.list.is_following_tail(), |d| {
                            d.child(
                                div().flex().justify_center().child(
                                    crate::dialog_button(
                                        "new-lead-messages",
                                        "New messages",
                                        "",
                                        &chrome,
                                    )
                                    .track_focus(&self.new_focus)
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.newest(cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
                            )
                        })
                        .children(dock_composer)
                        .when(!self.live && !self.starting, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(12.))
                                    .text_size(px(13.))
                                    .text_color(chrome.ink_3)
                                    .child("Lead is not running")
                                    .child(
                                        crate::dialog_button(
                                            "resume-chat-lead",
                                            "Resume",
                                            "↵",
                                            &chrome,
                                        )
                                        .track_focus(&self.action_focus)
                                        .on_click(
                                            cx.listener(|_, _, _, cx| cx.emit(ChatAction::Resume)),
                                        ),
                                    ),
                            )
                        })
                        .children(dock_hint),
                )
            })
    }
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
