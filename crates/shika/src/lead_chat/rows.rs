//! One list row: a time separator, an event line, or one markdown block
//! of a bubble, with its cached read-only text.
use super::{LeadChat, Message, Row, history::local_time, markdown};
use crate::{appearance, chat_input::ChatInput};
use gpui::{
    AppContext, Context, Focusable, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use shika_core::ChatContent;

impl LeadChat {
    pub(super) fn row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut chrome = self.chrome;
        chrome.ink_3 = appearance::legible(
            chrome.ink_3,
            appearance::with_alpha(chrome.term_surface, 1.),
            chrome.ink_1,
            4.5,
        );
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        match row {
            Row::Time { text, .. } => div()
                .w_full()
                .py(px(12.))
                .text_center()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child(text)
                .into_any_element(),
            Row::Event { text, .. } => div()
                .w_full()
                .py(px(12.))
                .text_center()
                .text_size(px(11.5))
                .text_color(chrome.ink_3)
                .child(text)
                .into_any_element(),
            Row::Block {
                message,
                block,
                first,
                last,
                gap,
            } => {
                let message = &self.records[message];
                let item = &message.blocks[block];
                let width = *self
                    .width_cache
                    .entry(message.record.id)
                    .or_insert_with(|| message_width(message, window));
                let author = matches!(message.record.content, ChatContent::Author { .. });
                let key = (message.record.id, block);
                let mut ink = chrome;
                ink.ink_1 = if author {
                    appearance::legible(
                        chrome.primary_fg,
                        chrome.primary_bg,
                        chrome.primary_fg,
                        4.5,
                    )
                } else {
                    appearance::legible(
                        chrome.ink_1,
                        if item.code {
                            chrome.sunken
                        } else {
                            chrome.raised
                        },
                        chrome.ink_1,
                        4.5,
                    )
                };
                let spans = markdown::highlights(item, ink);
                let input = if let Some(input) = self.text_cache.get(&key) {
                    input.clone()
                } else {
                    let input = cx.new(|cx| ChatInput::new(item.text.clone(), ink, cx));
                    cx.subscribe(&input, move |this, _, event, cx| {
                        this.text_selection(key, event, cx)
                    })
                    .detach();
                    self.text_cache.insert(key, input.clone());
                    input
                };
                let selection = self.selection_range(key, item.text.len());
                input.update(cx, |input, cx| {
                    let changed = input.style(ink, true, item.code, spans);
                    input.label(format!(
                        "{}, {}. {}",
                        if author { "You" } else { "Lead" },
                        local_time(message.record.timestamp),
                        if author {
                            "Submitted message"
                        } else {
                            "Completed reply"
                        }
                    ));
                    input.inline_style(
                        item.spans
                            .iter()
                            .filter(|s| s.code)
                            .map(|s| s.range.clone())
                            .collect(),
                        item.spans
                            .iter()
                            .filter_map(|s| {
                                s.link.as_ref().map(|link| (s.range.clone(), link.clone()))
                            })
                            .collect(),
                    );
                    let selected = input.select_range(selection);
                    if changed || selected {
                        cx.notify();
                    }
                });
                // Keep only a bounded set of shaped visible text entities.
                if self.text_cache.len() > 120 {
                    let active = input.entity_id();
                    self.text_cache.retain(|_, v| {
                        v.entity_id() == active || v.focus_handle(cx).is_focused(window)
                    });
                }
                div()
                    .w_full()
                    .flex()
                    .when(author, |d| d.justify_end())
                    .pt(px(if first { gap } else { 0. }))
                    .child(
                        div()
                            .max_w(gpui::relative(0.8))
                            .w(px(width))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(480.))
                                    .px(px(20.))
                                    .pt(px(if first {
                                        12.
                                    } else if author
                                        || item.continuation
                                        || (item.code
                                            && block > 0
                                            && message.blocks[block - 1].code)
                                    {
                                        0.
                                    } else {
                                        3.
                                    }))
                                    .pb(px(if last {
                                        12.
                                    } else if author
                                        || message
                                            .blocks
                                            .get(block + 1)
                                            .is_some_and(|b| b.continuation)
                                        || (item.code
                                            && message
                                                .blocks
                                                .get(block + 1)
                                                .is_some_and(|b| b.code))
                                    {
                                        0.
                                    } else {
                                        3.
                                    }))
                                    .when(first, |d| d.rounded_t(px(22.)))
                                    .when(last, |d| d.rounded_b(px(22.)))
                                    .bg(if author {
                                        chrome.primary_bg
                                    } else {
                                        chrome.raised
                                    })
                                    .child(
                                        div()
                                            .pl(px(item.indent as f32 * 20.))
                                            .when(item.code, |d| {
                                                d.bg(chrome.sunken)
                                                    .px(px(12.))
                                                    .pt(px(
                                                        if block == 0
                                                            || !message.blocks[block - 1].code
                                                        {
                                                            12.
                                                        } else {
                                                            0.
                                                        },
                                                    ))
                                                    .pb(px(
                                                        if !message
                                                            .blocks
                                                            .get(block + 1)
                                                            .is_some_and(|b| b.code)
                                                        {
                                                            12.
                                                        } else {
                                                            0.
                                                        },
                                                    ))
                                                    .when(
                                                        block == 0
                                                            || !message.blocks[block - 1].code,
                                                        |d| d.rounded_t(px(6.)),
                                                    )
                                                    .when(
                                                        !message
                                                            .blocks
                                                            .get(block + 1)
                                                            .is_some_and(|b| b.code),
                                                        |d| d.rounded_b(px(6.)),
                                                    )
                                            })
                                            .child(input),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }
}

fn message_width(message: &Message, window: &mut Window) -> f32 {
    let mut width = 0f32;
    for block in message.blocks.iter() {
        if block.text.chars().count() > 80 {
            return 480.;
        }
        let text: gpui::SharedString = block.text.clone().into();
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(if block.code {
                crate::MONO
            } else {
                crate::UI_FONT
            }),
            color: window.text_style().color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            text,
            px(if block.code { 12.5 } else { 14. }),
            &[run],
            None,
        );
        width = width.max(
            f32::from(line.width()) + if block.code { 24. } else { 0. } + block.indent as f32 * 20.,
        );
        if width >= 440. {
            return 480.;
        }
    }
    (width + 40.).clamp(44., 480.)
}
