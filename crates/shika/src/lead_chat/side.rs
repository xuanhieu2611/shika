//! The chat side panel that replaces the terminal for a Claude Lead:
//! its header and the chat view.
use super::{ANTLERS_ICON, LeadChat};
use crate::{Chrome, Selection, Shika, Status, appearance};
use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window, div, prelude::FluentBuilder, px,
};

impl Shika {
    pub(crate) fn selected_chat(&self) -> Option<&Entity<LeadChat>> {
        let project = match &*self.selection {
            Some(Selection::Card(i)) if self.cards.get(*i).is_some_and(|c| c.chat) => {
                &self.cards[*i].project
            }
            Some(Selection::Resting(project))
                if self
                    .projects
                    .iter()
                    .find(|p| p.id == *project)
                    .and_then(|p| p.lead.as_ref())
                    .is_some_and(|lead| lead.cli == "claude") =>
            {
                project
            }
            _ => return None,
        };
        self.leads.chats.get(project)
    }

    pub(crate) fn chat_side(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let chat = self.selected_chat()?.clone();
        chat.update(cx, |chat, cx| chat.set_chrome(*chrome, cx));
        let project = self.shown_project()?;
        let project_name = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map_or("", |p| p.name.as_str())
            .to_string();
        let live = self.selected_card().is_some_and(|i| self.cards[i].chat);
        let workers = chat.read(cx).workers;
        let project_click = project.clone();
        let header = div()
            .h(px(crate::BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .pr(px(20.))
            .border_b_1()
            .border_color(chrome.term_line)
            .bg(appearance::with_alpha(
                chrome.term_header,
                chrome.term_header_alpha,
            ))
            .when(self.column.hidden, |d| {
                d.child(
                    div()
                        .pl(Self::title_inset(window))
                        .child(self.title_identity(chrome, cx)),
                )
            })
            .when(!self.column.hidden, |d| d.pl(px(20.)))
            .child(
                div()
                    .size(px(24.))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(chrome.raised)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        gpui::svg()
                            .data(ANTLERS_ICON)
                            .size(px(16.))
                            .text_color(chrome.ink_2),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(chrome.ink_1)
                            .child(project_name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(chrome.ink_3)
                            .child("Lead"),
                    ),
            )
            .when(live, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(chrome.ink_3)
                        .whitespace_nowrap()
                        .when_some(workers.dot(), |d, status| {
                            d.child(div().size(px(6.)).flex_none().rounded_full().bg(
                                match status {
                                    Status::Asking => chrome.asking.dot,
                                    Status::Ready => chrome.ready.dot,
                                    Status::Working => chrome.working.dot,
                                    Status::Waiting => chrome.waiting.dot,
                                },
                            ))
                        })
                        .child(workers.label()),
                )
            })
            .child(
                crate::dialog_button(
                    "lead-chat-header",
                    if live { "Stop Lead" } else { "Resume" },
                    if live { "⌘⇧W" } else { "↵" },
                    chrome,
                )
                .track_focus(&chat.read(cx).header_focus)
                .role(gpui::Role::Button)
                .aria_label(if live { "Stop Lead" } else { "Resume Lead" })
                .on_key_down(cx.listener({
                    let project = project.clone();
                    move |this, event: &gpui::KeyDownEvent, window, cx| match event
                        .keystroke
                        .key
                        .as_str()
                    {
                        "enter" | "space" => {
                            if live {
                                this.close(window, cx);
                            } else {
                                this.resume_lead(project.clone(), window, cx);
                            }
                            cx.stop_propagation();
                        }
                        "tab" => {
                            if event.keystroke.modifiers.shift {
                                window.focus_prev(cx);
                            } else {
                                window.focus_next(cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if live {
                        this.close(window, cx);
                    } else {
                        this.resume_lead(project_click.clone(), window, cx);
                    }
                })),
            );
        Some(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(self.title_drag(header, cx))
                .child(chat)
                .into_any_element(),
        )
    }
}
