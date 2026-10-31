//! Text selection across the read-only message blocks and copying it.
use super::{ChatAction, LeadChat};
use crate::chat_input::InputEvent;
use gpui::Context;

impl LeadChat {
    pub(super) fn selection_range(
        &self,
        key: (i64, usize),
        length: usize,
    ) -> Option<std::ops::Range<usize>> {
        let (a, b) = self.selection?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if key < (a.0, a.1) || key > (b.0, b.1) {
            return None;
        }
        Some(
            if key == (a.0, a.1) {
                a.2.min(length)
            } else {
                0
            }..if key == (b.0, b.1) {
                b.2.min(length)
            } else {
                length
            },
        )
    }

    pub(super) fn text_selection(
        &mut self,
        key: (i64, usize),
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Select {
                anchor,
                cursor,
                start,
                shift,
            } => {
                if *start {
                    let anchor = if *shift {
                        self.selection
                            .map(|(a, _)| a)
                            .unwrap_or((key.0, key.1, *anchor))
                    } else {
                        (key.0, key.1, *anchor)
                    };
                    self.selection = Some((anchor, (key.0, key.1, *cursor)));
                    self.selecting = true;
                } else if self.selecting
                    && let Some((_, end)) = &mut self.selection
                {
                    *end = (key.0, key.1, *cursor);
                }
            }
            InputEvent::SelectMessage => {
                if let Some(message) = self.records.iter().find(|m| m.record.id == key.0)
                    && let Some(last) = message.blocks.last()
                {
                    self.selection = Some((
                        (key.0, 0, 0),
                        (key.0, message.blocks.len() - 1, last.text.len()),
                    ));
                }
            }
            InputEvent::CopySelection => {
                let mut selected = String::new();
                let mut previous = None;
                for message in &self.records {
                    for (block, item) in message.blocks.iter().enumerate() {
                        if let Some(range) =
                            self.selection_range((message.record.id, block), item.text.len())
                            && !range.is_empty()
                        {
                            if let Some(old) = previous {
                                selected.push_str(if old == message.record.id {
                                    "\n"
                                } else {
                                    "\n\n"
                                });
                            }
                            selected.push_str(&item.text[range]);
                            previous = Some(message.record.id);
                        }
                    }
                }
                if !selected.is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected));
                }
                return;
            }
            InputEvent::Back => {
                cx.emit(ChatAction::Back);
                return;
            }
            InputEvent::Interrupt if self.busy => {
                cx.emit(ChatAction::Interrupt);
                return;
            }
            _ => return,
        }
        for (key, input) in &self.text_cache {
            let range = self.selection_range(*key, input.read(cx).value().len());
            input.update(cx, |input, cx| {
                if input.select_range(range) {
                    cx.notify();
                }
            });
        }
        cx.notify();
    }
}
