//! Native single-line input for task names and worktree setup fields. Follows
//! the pinned GPUI input example's EntityInputHandler contract, not Zed's editor
//! or terminal crates.
use super::{Chrome, text_field};
use crate::text_edit::{self, Paragraphs, Text, utf8_offset};
use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Render, ShapedLine, SharedString,
    Styled, TextRun, UTF16Selection, UnderlineStyle, Window, fill, point, px, size,
};
use std::ops::Range;

fn single_line(text: &str) -> String {
    text.chars()
        .filter_map(|ch| {
            if ch.is_whitespace() {
                Some(' ')
            } else if ch.is_control() {
                None
            } else {
                Some(ch)
            }
        })
        .collect()
}

pub struct NameInput {
    focus: FocusHandle,
    text: Text,
    chrome: Chrome,
    layout: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    scroll: Pixels,
    selecting: bool,
}
impl NameInput {
    pub fn new(value: String, chrome: Chrome, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: Text::new(value),
            chrome,
            layout: None,
            bounds: None,
            scroll: px(0.),
            selecting: false,
        }
    }
    pub fn value(&self) -> &str {
        &self.text.value
    }
    pub fn composing(&self) -> bool {
        self.text.marked.is_some()
    }
    pub fn set_chrome(&mut self, chrome: Chrome) {
        self.chrome = chrome;
    }
    fn index(&self, position: Point<Pixels>) -> usize {
        match (&self.layout, self.bounds) {
            (Some(line), Some(bounds)) => {
                line.closest_index_for_x(position.x - bounds.left() + self.scroll)
            }
            _ => 0,
        }
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let m = stroke.modifiers;
        // During composition the input method owns navigation and deletion.
        if self.composing() {
            return;
        }
        let selected = self.text.selection();
        match stroke.key.as_str() {
            "a" if m.platform => {
                self.text.anchor = 0;
                self.text.cursor = self.text.value.len();
            }
            "c" | "x" if m.platform => {
                if !selected.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.text.value[selected.clone()].into(),
                    ));
                    if stroke.key == "x" {
                        self.text.replace(selected, "");
                    }
                }
            }
            "v" if m.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.text.replace(selected, &single_line(&text));
                }
            }
            _ => {
                let Some(edit) = text_edit::edit_for(stroke, false) else {
                    return;
                };
                let mut kill = text_edit::KILL_BUFFER
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if edit == text_edit::Edit::Yank {
                    // The buffer can hold a newline the composer killed.
                    self.text.replace(selected, &single_line(&kill));
                } else {
                    let value = self.text.value.clone();
                    self.text.apply(edit, &Paragraphs::new(&value), &mut kill);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}
impl Focusable for NameInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EntityInputHandler for NameInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.text.utf8_range(range);
        *actual = Some(self.text.to_utf16(range.clone()));
        Some(self.text.value[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.text.to_utf16(self.text.selection()),
            reversed: self.text.cursor < self.text.anchor,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.text
            .marked
            .clone()
            .map(|range| self.text.to_utf16(range))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.text.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.text.utf8_range(r))
            .or(self.text.marked.clone())
            .unwrap_or(self.text.selection());
        self.text.replace(range, &single_line(text));
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.text.utf8_range(r))
            .or(self.text.marked.clone())
            .unwrap_or(self.text.selection());
        let start = range.start;
        let text = single_line(text);
        self.text.replace(range, &text);
        if !text.is_empty() {
            self.text.marked = Some(start..start + text.len());
        }
        if let Some(selected) = selection {
            self.text.anchor = start + utf8_offset(&text, selected.start);
            self.text.cursor = start + utf8_offset(&text, selected.end);
        }
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.layout.as_ref()?;
        let bounds = self.bounds?;
        let range = self.text.utf8_range(range);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start) - self.scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(range.end) - self.scroll,
                bounds.bottom(),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(
            self.text
                .to_utf16(self.index(position)..self.index(position))
                .start,
        )
    }
}
impl Render for NameInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::InteractiveElement;
        let measure = cx.entity();
        let paint = cx.entity();
        text_field(
            "task-name-input",
            360.,
            self.focus.is_focused(window),
            &self.chrome,
        )
        .w_full()
        .track_focus(&self.focus)
        .line_height(px(16.))
        .on_key_down(cx.listener(Self::key))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                window.focus(&this.focus, cx);
                let index = this.index(event.position);
                if event.click_count >= 2 {
                    this.text.anchor = 0;
                    this.text.cursor = this.text.value.len();
                } else {
                    this.text.move_to(index, event.modifiers.shift);
                }
                this.selecting = true;
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
            if this.selecting {
                this.text.move_to(this.index(event.position), true);
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.selecting = false),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.selecting = false),
        )
        .child(
            gpui::canvas(
                move |bounds, window, cx| {
                    let input = measure.read(cx);
                    let style = window.text_style();
                    let value: SharedString = input.text.value.clone().into();
                    let run = TextRun {
                        len: value.len(),
                        font: style.font(),
                        color: style.color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let runs = if let Some(marked) = &input.text.marked {
                        vec![
                            TextRun {
                                len: marked.start,
                                ..run.clone()
                            },
                            TextRun {
                                len: marked.len(),
                                underline: Some(UnderlineStyle {
                                    color: Some(style.color),
                                    thickness: px(1.),
                                    wavy: false,
                                }),
                                ..run.clone()
                            },
                            TextRun {
                                len: value.len() - marked.end,
                                ..run
                            },
                        ]
                        .into_iter()
                        .filter(|run| run.len > 0)
                        .collect()
                    } else {
                        vec![run]
                    };
                    let line = window.text_system().shape_line(
                        value,
                        style.font_size.to_pixels(window.rem_size()),
                        &runs,
                        None,
                    );
                    let cursor = line.x_for_index(input.text.cursor);
                    let width = (bounds.size.width - px(1.)).max(px(0.));
                    let scroll = input.scroll.max(cursor - width).min(cursor).max(px(0.));
                    (line, scroll)
                },
                move |bounds, (line, scroll), window, cx| {
                    let input = paint.read(cx);
                    let selection = input.text.selection();
                    let cursor = input.text.cursor;
                    let focus = input.focus.clone();
                    let chrome = input.chrome;
                    let origin = point(bounds.left() - scroll, bounds.top());
                    window.handle_input(
                        &focus,
                        gpui::ElementInputHandler::new(bounds, paint.clone()),
                        cx,
                    );
                    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                        if focus.is_focused(window) && !selection.is_empty() {
                            window.paint_quad(fill(
                                Bounds::from_corners(
                                    point(
                                        origin.x + line.x_for_index(selection.start),
                                        bounds.top(),
                                    ),
                                    point(
                                        origin.x + line.x_for_index(selection.end),
                                        bounds.bottom(),
                                    ),
                                ),
                                chrome.row_selected,
                            ));
                        }
                        let _ =
                            line.paint(origin, px(16.), gpui::TextAlign::Left, None, window, cx);
                        if focus.is_focused(window) && selection.is_empty() {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(origin.x + line.x_for_index(cursor), bounds.top()),
                                    size(px(1.), bounds.size.height),
                                ),
                                chrome.focus,
                            ));
                        }
                    });
                    paint.update(cx, |input, _| {
                        input.layout = Some(line);
                        input.bounds = Some(bounds);
                        input.scroll = scroll;
                    });
                },
            )
            .w_full()
            .h(px(16.)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_and_navigation_keep_graphemes_whole() {
        let mut text = Text::new("Aé👩‍💻e\u{301}".into());
        assert_eq!(text.previous(), "Aé👩‍💻".len());
        text.replace(text.previous()..text.cursor, "");
        assert_eq!(text.previous(), "Aé".len());
        text.anchor = 0;
        text.replace(text.selection(), "Renamed");
        assert_eq!(text.value, "Renamed");
        assert_eq!(text.selection(), 7..7);
    }
    #[test]
    fn single_line_paste() {
        assert_eq!(single_line("Name\r\nwith\twords\0"), "Name  with words");
    }
}
