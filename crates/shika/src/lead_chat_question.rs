//! Author questions use the native dialog shell, with correlated answers sent
//! back to Claude. Neither question payloads nor tool replies enter history.
use crate::{Chrome, Overlay, Selection, Shika, name_input::NameInput};
use gpui::{
    AppContext, Context, Entity, Focusable, InteractiveElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use serde_json::{Map, Value};
use shika_core::ChatQuestion;

pub struct QuestionDialog {
    pub session: String,
    pub question: ChatQuestion,
    at: usize,
    row: usize,
    selected: Vec<usize>,
    answers: Map<String, Value>,
    custom: Entity<NameInput>,
    error: Option<String>,
}
impl QuestionDialog {
    /// Names this question among the dialogs the input grace window tells
    /// apart: the request and which of its questions is showing.
    pub fn identity(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&self.session, &self.question.request_id, self.at).hash(&mut hasher);
        hasher.finish()
    }
}
impl Shika {
    pub(super) fn open_chat_question(
        &mut self,
        session: String,
        project: String,
        question: ChatQuestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A question arriving under another author dialog, or while the
        // author is typing, waits in the chat inbox; never overwrite a dialog,
        // let keystrokes land in it, or automatically answer it.
        // Questions stay first come, first served: a new one never passes
        // one already waiting.
        if self.overlay.is_some()
            || !self.chat_questions.is_empty()
            || !self.author_quiet(std::time::Instant::now(), cx)
        {
            self.chat_questions.push((session, project, question));
            return;
        }
        self.show_chat_question(session, question, window, cx);
    }
    /// Opens the dialog for a question the queue has already cleared.
    pub(super) fn show_chat_question(
        &mut self,
        session: String,
        question: ChatQuestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let custom = cx.new(|cx| NameInput::new(String::new(), self.chrome(window), cx));
        self.overlay_return_focus = window.focused(cx);
        self.overlay = Some(Overlay::LeadQuestion(QuestionDialog {
            session,
            question,
            at: 0,
            row: 0,
            selected: Vec::new(),
            answers: Map::new(),
            custom,
            error: None,
        }));
        window.focus(&self.focus, cx);
        cx.notify();
    }
    pub(super) fn answer_chat_question(
        &mut self,
        cancel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chrome = self.chrome(window);
        let Some(Overlay::LeadQuestion(dialog)) = &mut self.overlay else {
            return;
        };
        if !cancel {
            let question = &dialog.question.questions[dialog.at];
            let custom = dialog.custom.read(cx).value().trim().to_string();
            let labels = dialog
                .selected
                .iter()
                .map(|i| question.options[*i].label.clone())
                .collect::<Vec<_>>();
            let answer = if !custom.is_empty() {
                Value::String(custom)
            } else if labels.is_empty() {
                dialog.error = Some("Choose an option or write an answer.".into());
                cx.notify();
                return;
            } else if question.multi_select {
                Value::Array(labels.into_iter().map(Value::String).collect())
            } else {
                Value::String(labels[0].clone())
            };
            dialog.answers.insert(question.question.clone(), answer);
            if dialog.at + 1 < dialog.question.questions.len() {
                dialog.at += 1;
                dialog.row = 0;
                dialog.selected.clear();
                dialog.error = None;
                dialog.custom = cx.new(|cx| NameInput::new(String::new(), chrome, cx));
                window.focus(&self.focus, cx);
                cx.notify();
                return;
            }
        }
        let Some(Overlay::LeadQuestion(dialog)) = self.overlay.take() else {
            return;
        };
        let answer = if cancel {
            None
        } else {
            let mut input = dialog.question.input;
            input["answers"] = Value::Object(dialog.answers);
            Some(input)
        };
        self.core
            .lead_chat_answer(&dialog.session, dialog.question.request_id, answer);
        if let Some(index) = self
            .cards
            .iter()
            .position(|c| c.session.as_ref().is_some_and(|s| s.id == dialog.session))
        {
            self.selection.set(Some(Selection::Card(index)));
            self.overlay_return_focus = None;
            self.focus_terminal(window, cx);
        } else {
            self.restore_overlay_focus(window, cx);
        }
        cx.notify();
    }
    pub(super) fn chat_question_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog_ignores_key(&event.keystroke.key) {
            cx.stop_propagation();
            return;
        }
        let Some(Overlay::LeadQuestion(dialog)) = &mut self.overlay else {
            return;
        };
        if dialog.custom.read(cx).composing() {
            return;
        }
        let key = &event.keystroke;
        if key.key == "escape" {
            self.answer_chat_question(true, window, cx);
        } else if key.key == "enter" {
            self.answer_chat_question(false, window, cx);
        } else if key.key == "tab" {
            window.focus(&dialog.custom.focus_handle(cx), cx);
        } else if !dialog.custom.focus_handle(cx).is_focused(window) {
            let question = &dialog.question.questions[dialog.at];
            let count = question.options.len();
            match key.key.as_str() {
                "j" | "down" if count > 0 => dialog.row = (dialog.row + 1) % count,
                "k" | "up" if count > 0 => dialog.row = (dialog.row + count - 1) % count,
                "space" if count > 0 => {
                    if !question.multi_select {
                        dialog.selected.clear();
                    }
                    if let Some(at) = dialog.selected.iter().position(|i| *i == dialog.row) {
                        dialog.selected.remove(at);
                    } else {
                        dialog.selected.push(dialog.row);
                    }
                }
                "1" | "2" | "3" | "4" => {
                    let index = key.key.parse::<usize>().unwrap() - 1;
                    if index < count {
                        if !question.multi_select {
                            dialog.selected.clear();
                        }
                        dialog.selected.push(index);
                        dialog.row = index;
                    }
                }
                _ => return,
            }
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn chat_question_view(
        &self,
        dialog: &QuestionDialog,
        chrome: &Chrome,
        max_h: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let question = &dialog.question.questions[dialog.at];
        let mut panel = crate::dialog(chrome, max_h)
            .child(
                crate::dialog_title(div()).child(if question.header.is_empty() {
                    "Lead question".into()
                } else {
                    question.header.clone()
                }),
            )
            .child(crate::dialog_text(chrome).child(question.question.clone()));
        for (i, option) in question.options.iter().enumerate() {
            let selected = dialog.selected.contains(&i);
            panel = panel.child(
                crate::list_row(dialog.row == i, chrome)
                    .id(("lead-question-option", i))
                    .cursor_pointer()
                    .py(px(12.))
                    .gap(px(12.))
                    .aria_label(option.label.clone())
                    .role(gpui::Role::RadioButton)
                    .aria_toggled(if selected {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .child(div().text_color(chrome.ink_2).child(if selected {
                        "✓"
                    } else {
                        "○"
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().text_color(chrome.ink_1).child(option.label.clone()))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(chrome.ink_3)
                                    .child(option.description.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(Overlay::LeadQuestion(dialog)) = &mut this.overlay {
                            dialog.row = i;
                            if !dialog.question.questions[dialog.at].multi_select {
                                dialog.selected.clear();
                            }
                            if let Some(at) = dialog.selected.iter().position(|j| *j == i) {
                                dialog.selected.remove(at);
                            } else {
                                dialog.selected.push(i);
                            }
                        }
                        cx.notify();
                    })),
            );
        }
        panel
            .child(crate::dialog_text(chrome).child("Or write an answer"))
            .child(dialog.custom.clone())
            .when_some(dialog.error.as_ref(), |d, error| {
                d.child(crate::dialog_text(chrome).child(error.clone()))
            })
            .child(
                crate::dialog_buttons()
                    .child(
                        crate::dialog_button("cancel-lead-question", "Cancel", "esc", chrome)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answer_chat_question(true, window, cx)
                            })),
                    )
                    .child(
                        crate::primary_button(
                            "answer-lead-question",
                            if dialog.at + 1 < dialog.question.questions.len() {
                                "Next"
                            } else {
                                "Answer"
                            },
                            "↵",
                            chrome,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_chat_question(false, window, cx)
                        })),
                    ),
            )
    }
}

use gpui::prelude::FluentBuilder;
