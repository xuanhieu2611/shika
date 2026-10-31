//! The composer: the raised message field and its round Send or Stop action.
use super::{ChatAction, LeadChat, SEND_ICON};
use crate::Chrome;
use gpui::{
    AppContext, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

impl LeadChat {
    /// The composer: one raised field with no border (the caret shows focus)
    /// and a round 32px action at its bottom right, Send or Stop. One
    /// element builder for both places it lives (the empty state's center
    /// and the bottom dock), so moving it keeps the same entity and focus.
    pub(super) fn composer_box(
        &self,
        chrome: &Chrome,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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
                div()
                    .flex_none()
                    .pb(px(8.))
                    .child(self.composer_action(chrome, enabled, window, cx)),
            )
            .into_any_element()
    }

    /// The round 32px action: Send (primary with a draft, sunken and inert
    /// without one) or Stop while a turn runs, with its key tip.
    fn composer_action(
        &self,
        chrome: &Chrome,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    gpui::BoxShadow::new(px(0.), px(0.), chrome.focus.into()).spread_radius(px(2.)),
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
            })
    }
}
