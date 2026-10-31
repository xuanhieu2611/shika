//! One list row: a time separator, an event line, or one markdown block
//! of a bubble, with its cached read-only text.
use super::{LeadChat, Message, Row, history::local_time, markdown};
use crate::{Chrome, appearance, chat_input::ChatInput};
use gpui::{
    AppContext, Context, Entity, Focusable, HighlightStyle, IntoElement, ParentElement, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use markdown::Block;
use shika_core::ChatContent;
use std::collections::HashMap;
use std::ops::Range;

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
            Row::Time { text, .. } => separator_row(&chrome, text),
            Row::Event { text, .. } => separator_row(&chrome, text),
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
                let ink = block_ink(chrome, author, item.code);
                let spans = markdown::highlights(item, ink);
                let input = cached_text_input(&mut self.text_cache, key, item, ink, cx);
                let selection = self.selection_range(key, item.text.len());
                input.update(cx, |input, cx| {
                    if style_block_input(input, message, block, ink, spans, selection) {
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
                let (top, bottom) = bubble_padding(message, block, first, last, author);
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
                                    .pt(px(top))
                                    .pb(px(bottom))
                                    .when(first, |d| d.rounded_t(px(22.)))
                                    .when(last, |d| d.rounded_b(px(22.)))
                                    .bg(if author {
                                        chrome.primary_bg
                                    } else {
                                        chrome.raised
                                    })
                                    .child(
                                        code_frame(
                                            div().pl(px(item.indent as f32 * 20.)),
                                            &chrome,
                                            message,
                                            block,
                                        )
                                        .child(input),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        }
    }
}

/// A centered tertiary line: a time separator or an event row.
fn separator_row(chrome: &Chrome, text: String) -> gpui::AnyElement {
    div()
        .w_full()
        .py(px(12.))
        .text_center()
        .text_size(px(11.5))
        .text_color(chrome.ink_3)
        .child(text)
        .into_any_element()
}

/// The block's chrome with primary ink made legible on its bubble: the
/// primary fill for the author, the raised fill or sunken code for the Lead.
fn block_ink(chrome: Chrome, author: bool, code: bool) -> Chrome {
    let mut ink = chrome;
    ink.ink_1 = if author {
        appearance::legible(chrome.primary_fg, chrome.primary_bg, chrome.primary_fg, 4.5)
    } else {
        appearance::legible(
            chrome.ink_1,
            if code { chrome.sunken } else { chrome.raised },
            chrome.ink_1,
            4.5,
        )
    };
    ink
}

/// The block's read-only text, reused from the cache by `(record id, block)`
/// or created with its selection events wired to the chat.
fn cached_text_input(
    cache: &mut HashMap<(i64, usize), Entity<ChatInput>>,
    key: (i64, usize),
    item: &Block,
    ink: Chrome,
    cx: &mut Context<LeadChat>,
) -> Entity<ChatInput> {
    if let Some(input) = cache.get(&key) {
        input.clone()
    } else {
        let input = cx.new(|cx| ChatInput::new(item.text.clone(), ink, cx));
        cx.subscribe(&input, move |this, _, event, cx| {
            this.text_selection(key, event, cx)
        })
        .detach();
        cache.insert(key, input.clone());
        input
    }
}

/// Styles, labels, and selects the block's text. True when anything
/// changed and the input needs a redraw.
fn style_block_input(
    input: &mut ChatInput,
    message: &Message,
    block: usize,
    ink: Chrome,
    spans: Vec<(Range<usize>, HighlightStyle)>,
    selection: Option<Range<usize>>,
) -> bool {
    let item = &message.blocks[block];
    let author = matches!(message.record.content, ChatContent::Author { .. });
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
            .filter_map(|s| s.link.as_ref().map(|link| (s.range.clone(), link.clone())))
            .collect(),
    );
    let selected = input.select_range(selection);
    changed || selected
}

/// The bubble's top and bottom padding for one block: 12 at the bubble's
/// ends, none between author lines, continuations, and adjacent code
/// blocks, and 3 between other blocks.
fn bubble_padding(
    message: &Message,
    block: usize,
    first: bool,
    last: bool,
    author: bool,
) -> (f32, f32) {
    let item = &message.blocks[block];
    let top = if first {
        12.
    } else if author
        || item.continuation
        || (item.code && block > 0 && message.blocks[block - 1].code)
    {
        0.
    } else {
        3.
    };
    let bottom = if last {
        12.
    } else if author
        || message
            .blocks
            .get(block + 1)
            .is_some_and(|b| b.continuation)
        || (item.code && message.blocks.get(block + 1).is_some_and(|b| b.code))
    {
        0.
    } else {
        3.
    };
    (top, bottom)
}

/// A code block's sunken frame: padded and rounded at the ends of a run of
/// adjacent code blocks. Other blocks pass through unchanged.
fn code_frame(d: gpui::Div, chrome: &Chrome, message: &Message, block: usize) -> gpui::Div {
    let item = &message.blocks[block];
    d.when(item.code, |d| {
        d.bg(chrome.sunken)
            .px(px(12.))
            .pt(px(if block == 0 || !message.blocks[block - 1].code {
                12.
            } else {
                0.
            }))
            .pb(px(
                if !message.blocks.get(block + 1).is_some_and(|b| b.code) {
                    12.
                } else {
                    0.
                },
            ))
            .when(block == 0 || !message.blocks[block - 1].code, |d| {
                d.rounded_t(px(6.))
            })
            .when(
                !message.blocks.get(block + 1).is_some_and(|b| b.code),
                |d| d.rounded_b(px(6.)),
            )
    })
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
