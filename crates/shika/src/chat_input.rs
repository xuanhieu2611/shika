//! Native multiline text editing and selection for the Lead composer. Follows
//! the pinned GPUI input example's EntityInputHandler contract, not Zed's editor
//! or terminal crates.
use super::Chrome;
use crate::text_edit::{self, Edit, Lines, Paragraphs, Text, utf8_offset};
use gpui::WrappedLineLayout;
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, EntityInputHandler, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Render, SharedString,
    Styled, TextLayout, UTF16Selection, Window, fill, point, px, size,
};
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// The composer's visual rows, read from the wrapped layout GPUI painted.
struct Rows {
    rows: Vec<Range<usize>>,
    /// Per row: its paragraph's layout, that paragraph's start, and which
    /// wrapped row of it this is.
    lines: Vec<(Arc<WrappedLineLayout>, usize, usize)>,
    line_height: Pixels,
}
impl Rows {
    fn new(layout: &TextLayout) -> Self {
        let mut rows = Vec::new();
        let mut lines = Vec::new();
        let mut start = 0;
        for line in layout.line_layouts() {
            let mut row_start = start;
            for (wrap, boundary) in line.wrap_boundaries().iter().enumerate() {
                let end = start
                    + line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index;
                rows.push(row_start..end);
                lines.push((line.clone(), start, wrap));
                row_start = end;
            }
            rows.push(row_start..start + line.len());
            lines.push((line.clone(), start, line.wrap_boundaries().len()));
            start += line.len() + 1;
        }
        Self {
            rows,
            lines,
            line_height: layout.line_height(),
        }
    }
}
impl Lines for Rows {
    fn rows(&self) -> &[Range<usize>] {
        &self.rows
    }
    fn x(&self, index: usize, row: usize) -> f32 {
        let (line, start, _) = &self.lines[row];
        if index == self.rows[row].start {
            return 0.;
        }
        line.position_for_index(index - start, self.line_height)
            .map_or(0., |p| p.x.into())
    }
    fn index_at(&self, row: usize, x: f32) -> usize {
        let (line, start, wrap) = &self.lines[row];
        let y = self.line_height * (*wrap as f32 + 0.5);
        start
            + line
                .closest_index_for_position(point(px(x), y), self.line_height)
                .unwrap_or_else(|i| i)
    }
}
/// How far to move the content so a line `line` tall at window y `top` sits
/// inside the viewport `view_top..view_bottom`: negative scrolls the content
/// up, positive down, zero when the line is already in view. A line taller
/// than the viewport lines up with its top.
fn reveal(top: Pixels, line: Pixels, view_top: Pixels, view_bottom: Pixels) -> Pixels {
    if top + line > view_bottom {
        (view_bottom - (top + line)).max(view_top - top)
    } else if top < view_top {
        view_top - top
    } else {
        px(0.)
    }
}
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

pub struct ChatInput {
    focus: FocusHandle,
    text: Text,
    chrome: Chrome,
    layout: Option<TextLayout>,
    bounds: Option<Bounds<Pixels>>,
    scroll: gpui::ScrollHandle,
    readonly: bool,
    code: bool,
    spans: Vec<(Range<usize>, gpui::HighlightStyle)>,
    monos: Vec<Range<usize>>,
    links: Vec<(Range<usize>, String)>,
    hover_link: Option<String>,
    active_link: Option<usize>,
    shared_selection: bool,
    label: String,
    placeholder: String,
    field_id: SharedString,
    /// Enter inserts a newline. The Lead composer leaves this off and submits.
    newline_on_enter: bool,
    /// Character cap for an insertion. None keeps the whole replacement.
    max_chars: Option<usize>,
    max_h: Pixels,
    history: Vec<(String, usize, usize)>,
    redo: Vec<(String, usize, usize)>,
    selecting: bool,
    ensure_cursor: bool,
}
impl ChatInput {
    pub fn new(value: String, chrome: Chrome, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle().tab_stop(true),
            text: Text::new(value),
            chrome,
            layout: None,
            bounds: None,
            scroll: gpui::ScrollHandle::new(),
            readonly: false,
            code: false,
            spans: Vec::new(),
            monos: Vec::new(),
            links: Vec::new(),
            hover_link: None,
            active_link: None,
            shared_selection: false,
            label: "Message Lead".into(),
            placeholder: "Message Lead".into(),
            field_id: "native-chat-text".into(),
            newline_on_enter: false,
            max_chars: None,
            max_h: px(120.),
            history: Vec::new(),
            redo: Vec::new(),
            selecting: false,
            ensure_cursor: false,
        }
    }
    pub fn value(&self) -> &str {
        &self.text.value
    }
    pub fn composing(&self) -> bool {
        self.text.marked.is_some()
    }
    pub fn set_chrome(&mut self, chrome: Chrome) -> bool {
        let changed = self.chrome != chrome;
        self.chrome = chrome;
        changed
    }
    pub fn clear_if(&mut self, submitted: &str, cx: &mut Context<Self>) {
        if self.text.value == submitted {
            self.text = Text::default();
            cx.notify();
        }
    }
    pub fn style(
        &mut self,
        chrome: Chrome,
        readonly: bool,
        code: bool,
        spans: Vec<(Range<usize>, gpui::HighlightStyle)>,
    ) -> bool {
        let changed = self.chrome != chrome
            || self.readonly != readonly
            || self.code != code
            || self.spans != spans;
        self.chrome = chrome;
        self.readonly = readonly;
        self.code = code;
        self.spans = spans;
        changed
    }
    pub fn label(&mut self, label: String) {
        self.label = label;
    }
    pub fn placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }
    pub fn field_id(&mut self, id: impl Into<SharedString>) {
        self.field_id = id.into();
    }
    /// Enter inserts a line. Confirm stays with the caller.
    pub fn newline_on_enter(&mut self) {
        self.newline_on_enter = true;
    }
    pub fn max_chars(&mut self, max: usize) {
        self.max_chars = Some(max);
    }
    pub fn max_height(&mut self, height: Pixels) {
        self.max_h = height;
    }
    /// The editor's scroll position, for a scrollbar drawn beside it.
    pub fn scroll_handle(&self) -> gpui::ScrollHandle {
        self.scroll.clone()
    }
    /// `None` when a capped field has no room left for `text`.
    fn accept(&self, range: Range<usize>, text: &str) -> Option<String> {
        let text = clean_text(text);
        let Some(max) = self.max_chars else {
            return Some(text);
        };
        let kept = self.text.value[..range.start].chars().count()
            + self.text.value[range.end..].chars().count();
        let room = max.saturating_sub(kept);
        if room == 0 && !text.is_empty() {
            return None;
        }
        Some(text.chars().take(room).collect())
    }
    pub fn select_range(&mut self, range: Option<Range<usize>>) -> bool {
        let before = (self.text.selection(), self.shared_selection);
        self.shared_selection = range.as_ref().is_some_and(|r| !r.is_empty());
        if let Some(range) = range {
            self.text.anchor = range.start;
            self.text.cursor = range.end;
        } else {
            self.text.anchor = self.text.cursor;
        }
        before != (self.text.selection(), self.shared_selection)
    }
    pub fn inline_style(&mut self, monos: Vec<Range<usize>>, links: Vec<(Range<usize>, String)>) {
        self.monos = monos;
        self.links = links;
    }
    fn index(&self, position: Point<Pixels>) -> usize {
        match (&self.layout, self.bounds) {
            (Some(layout), Some(_)) => layout
                .index_for_position(position)
                .unwrap_or_else(|i| i)
                .min(self.text.value.len()),
            _ => 0,
        }
    }
    /// Where the caret paints. GPUI puts a wrap boundary at the end of the
    /// upper row; like NSTextView, the caret goes to the start of the lower
    /// row unless it is upstream.
    fn caret(&self, layout: &TextLayout) -> Option<Point<Pixels>> {
        let position = layout.position_for_index(self.text.cursor)?;
        if self.text.upstream || self.text.value.is_empty() {
            return Some(position);
        }
        let rows = Rows::new(layout);
        let row = self.text.row(&rows);
        let cursor = self.text.cursor;
        let wrapped = row > 0 && rows.rows[row].start == cursor && rows.rows[row - 1].end == cursor;
        Some(if wrapped {
            point(layout.bounds().left(), position.y + rows.line_height)
        } else {
            position
        })
    }
    /// A press past the end of a wrapped row lands on the boundary, and the
    /// caret stays on the row that was pressed.
    fn upstream_at(&self, index: usize, position: Point<Pixels>) -> bool {
        self.layout
            .as_ref()
            .and_then(|layout| layout.position_for_index(index))
            .is_some_and(|caret| position.y < caret.y + px(20.))
    }
    fn checkpoint(&mut self) {
        self.remember((self.text.value.clone(), self.text.anchor, self.text.cursor));
    }
    fn remember(&mut self, entry: (String, usize, usize)) {
        self.history.push(entry);
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.redo.clear();
    }
    fn paint_inline_code(
        &self,
        layout: &TextLayout,
        bounds: Bounds<Pixels>,
        background: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        for range in &self.monos {
            let mut start = range.start;
            while start < range.end {
                let Some(origin) = layout.position_for_index(start) else {
                    break;
                };
                let mut end = layout
                    .index_for_position(point(bounds.right(), origin.y + px(10.)))
                    .unwrap_or_else(|i| i)
                    .min(range.end);
                if end <= start {
                    end = (start
                        + self.text.value[start..]
                            .chars()
                            .next()
                            .map_or(0, char::len_utf8))
                    .min(range.end);
                }
                if end <= start {
                    break;
                }
                if background {
                    let right = layout
                        .position_for_index(end)
                        .filter(|p| p.y == origin.y)
                        .map_or(bounds.right(), |p| p.x);
                    window.paint_quad(gpui::quad(
                        Bounds::new(origin, size((right - origin.x).max(px(1.)), px(20.))),
                        px(4.),
                        self.chrome.hover,
                        px(0.),
                        self.chrome.ink_1,
                        gpui::BorderStyle::default(),
                    ));
                } else {
                    for (offset, glyph) in self.text.value[start..end].grapheme_indices(true) {
                        let index = start + offset;
                        let Some(position) = layout.position_for_index(index) else {
                            continue;
                        };
                        let selected = (self.shared_selection || self.focus.is_focused(window))
                            && self.text.selection().contains(&index);
                        let surface = if selected {
                            self.chrome.row_selected
                        } else {
                            crate::appearance::mix(
                                self.chrome.raised,
                                self.chrome.hover,
                                self.chrome.hover.a,
                            )
                        };
                        let text: gpui::SharedString = glyph.to_string().into();
                        let run = gpui::TextRun {
                            len: text.len(),
                            font: gpui::font(super::MONO),
                            color: crate::appearance::legible(
                                self.chrome.ink_1,
                                surface,
                                self.chrome.ink_1,
                                4.5,
                            )
                            .into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let line = window
                            .text_system()
                            .shape_line(text, px(12.5), &[run], None);
                        let _ =
                            line.paint(position, px(20.), gpui::TextAlign::Left, None, window, cx);
                    }
                }
                start = end;
            }
        }
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let m = stroke.modifiers;
        // During composition the input method owns navigation and deletion.
        if self.composing() {
            return;
        }
        if self.emit_command_key(stroke, cx) {
            return;
        }
        if stroke.key == "enter" && !self.readonly {
            self.enter_key(m, cx);
            return;
        }
        if stroke.key == "z" && m.platform && !self.readonly {
            self.undo_key(m, cx);
            return;
        }
        if self.link_key(stroke, cx) {
            return;
        }
        self.edit_key(stroke, cx);
    }
    /// Keys the composer hands to its owner: Command-A and Command-C on a
    /// read-only message, Ctrl+Q back to the cards, and Escape. True when the
    /// key was one of them.
    fn emit_command_key(&mut self, stroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let m = stroke.modifiers;
        if self.readonly && m.platform && matches!(stroke.key.as_str(), "a" | "c") {
            cx.emit(if stroke.key == "a" {
                InputEvent::SelectMessage
            } else {
                InputEvent::CopySelection
            });
            cx.stop_propagation();
            return true;
        }
        if stroke.key == "q" && m.control {
            cx.emit(InputEvent::Back);
            cx.stop_propagation();
            return true;
        }
        if stroke.key == "escape" {
            cx.emit(InputEvent::Interrupt);
            cx.stop_propagation();
            return true;
        }
        false
    }
    /// Enter in an editable composer: a newline with Shift (or when Enter
    /// inserts newlines), otherwise Submit.
    fn enter_key(&mut self, m: gpui::Modifiers, cx: &mut Context<Self>) {
        let chord = m.platform || m.control || m.alt;
        let newline = (m.shift && !chord) || (self.newline_on_enter && !chord);
        if newline {
            let range = self.text.selection();
            if let Some(inserted) = self.accept(range.clone(), "\n") {
                self.checkpoint();
                self.ensure_cursor = true;
                self.text.replace(range, &inserted);
                cx.emit(InputEvent::Changed);
                cx.notify();
            }
        } else if !self.newline_on_enter {
            cx.emit(InputEvent::Submit);
        }
        cx.stop_propagation();
    }
    /// Command-Z undoes, Command-Shift-Z redoes.
    fn undo_key(&mut self, m: gpui::Modifiers, cx: &mut Context<Self>) {
        let from = if m.shift {
            &mut self.redo
        } else {
            &mut self.history
        };
        if let Some((value, anchor, cursor)) = from.pop() {
            let old = (self.text.value.clone(), self.text.anchor, self.text.cursor);
            if m.shift {
                self.history.push(old);
            } else {
                self.redo.push(old);
            }
            self.text = Text::new(value);
            self.text.anchor = anchor;
            self.text.cursor = cursor;
            self.ensure_cursor = true;
            cx.emit(InputEvent::Changed);
            cx.notify();
        }
        cx.stop_propagation();
    }
    /// Links in a read-only message: Tab and Shift-Tab select the next or
    /// previous one, Enter opens the selected one. True when the key was
    /// handled.
    fn link_key(&mut self, stroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let m = stroke.modifiers;
        if self.readonly && stroke.key == "tab" && !self.links.is_empty() {
            let next = if m.shift {
                self.active_link.and_then(|i| i.checked_sub(1))
            } else {
                Some(self.active_link.map_or(0, |i| i + 1)).filter(|i| *i < self.links.len())
            };
            if let Some(next) = next {
                self.active_link = Some(next);
                self.text.anchor = self.links[next].0.start;
                self.text.cursor = self.links[next].0.end;
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            self.active_link = None;
        }
        if self.readonly
            && stroke.key == "enter"
            && let Some(index) = self.active_link
        {
            let _ = super::open_terminal_link(&self.links[index].1);
            cx.stop_propagation();
            return true;
        }
        false
    }
    /// Select all, the clipboard, and text editing and movement keys.
    fn edit_key(&mut self, stroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        let m = stroke.modifiers;
        let before = self.text.value.clone();
        let caret = (self.text.anchor, self.text.cursor);
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
                    if stroke.key == "x" && !self.readonly {
                        self.checkpoint();
                        self.text.replace(selected, "");
                    }
                }
            }
            "v" if m.platform && !self.readonly => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                    && let Some(inserted) = self.accept(selected.clone(), &text)
                {
                    self.checkpoint();
                    self.text.replace(selected, &inserted);
                }
            }
            _ => {
                let Some(edit) = text_edit::edit_for(stroke, true) else {
                    return;
                };
                if self.readonly && !matches!(edit, Edit::Move(..)) {
                    return;
                }
                if let Edit::Scroll(top) = edit {
                    let offset = self.scroll.offset();
                    let y = if top {
                        px(0.)
                    } else {
                        -self.scroll.max_offset().y
                    };
                    self.scroll.set_offset(point(offset.x, y));
                } else {
                    let entry = (before.clone(), self.text.anchor, self.text.cursor);
                    let mut kill = text_edit::KILL_BUFFER
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    // Before the layout catches up with the text, rows are
                    // paragraphs; an empty composer lays out its placeholder.
                    match self.layout.as_ref().filter(|layout| {
                        !before.is_empty() && self.bounds.is_some() && layout.text() == before
                    }) {
                        Some(layout) => self.text.apply(edit, &Rows::new(layout), &mut kill),
                        None => self.text.apply(edit, &Paragraphs::new(&before), &mut kill),
                    };
                    drop(kill);
                    if self.text.value != before {
                        self.remember(entry);
                    }
                }
            }
        }
        // Only an edit or a caret move brings the caret into view; a key that
        // changed nothing, or a Home/End scroll, leaves the scroll where the
        // wheel put it.
        self.ensure_cursor = !self.readonly
            && (before != self.text.value || caret != (self.text.anchor, self.text.cursor));
        if before != self.text.value {
            cx.emit(InputEvent::Changed);
        }
        cx.stop_propagation();
        cx.notify();
    }
    /// The words and highlights painted: the composer's spans, the marked
    /// text's underline, the selection, and hidden ink under inline code.
    fn highlights(
        &self,
        placeholder: bool,
        window: &Window,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        let mut spans = self.spans.clone();
        if let Some(marked) = &self.text.marked {
            spans.push((
                marked.clone(),
                gpui::HighlightStyle {
                    underline: Some(gpui::UnderlineStyle {
                        color: Some(self.chrome.ink_1.into()),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..Default::default()
                },
            ));
        }
        let selection = self.text.selection();
        if !placeholder
            && !selection.is_empty()
            && (self.focus.is_focused(window) || self.shared_selection)
        {
            spans.push((
                selection,
                gpui::HighlightStyle {
                    background_color: Some(self.chrome.row_selected.into()),
                    color: Some(
                        crate::appearance::legible(
                            self.chrome.ink_1,
                            self.chrome.row_selected,
                            self.chrome.ink_1,
                            4.5,
                        )
                        .into(),
                    ),
                    ..Default::default()
                },
            ));
        }
        for range in &self.monos {
            spans.push((
                range.clone(),
                gpui::HighlightStyle {
                    color: Some(crate::appearance::with_alpha(self.chrome.ink_1, 0.).into()),
                    ..Default::default()
                },
            ));
        }
        spans
    }
    /// A press focuses the composer: one click places the caret (Shift
    /// extends), two select a word, three select everything.
    fn mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let index = self.index(event.position);
        if event.click_count >= 3 {
            self.text.anchor = 0;
            self.text.cursor = self.text.value.len();
        } else if event.click_count == 2 {
            let range = self
                .text
                .value
                .split_word_bound_indices()
                .find(|(i, w)| *i <= index && index < i + w.len())
                .map(|(i, w)| i..i + w.len())
                .unwrap_or(index..index);
            self.text.anchor = range.start;
            self.text.cursor = range.end;
        } else {
            let upstream = self.upstream_at(index, event.position);
            self.text.move_to(index, event.modifiers.shift);
            self.text.upstream = upstream;
        }
        self.selecting = true;
        if self.readonly {
            cx.emit(InputEvent::Select {
                anchor: self.text.anchor,
                cursor: self.text.cursor,
                start: true,
                shift: event.modifiers.shift,
            });
        }
        cx.notify();
    }
    /// The pointer moved: the hovered link, and the selection while dragging.
    fn mouse_move(&mut self, event: &gpui::MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let index = self.index(event.position);
        self.hover_link = self
            .links
            .iter()
            .find(|(range, _)| range.contains(&index))
            .map(|(_, link)| link.clone());
        if self.selecting {
            let upstream = self.upstream_at(index, event.position);
            self.text.move_to(index, true);
            self.text.upstream = upstream;
        }
        if self.readonly && event.pressed_button == Some(MouseButton::Left) {
            cx.emit(InputEvent::Select {
                anchor: self.text.anchor,
                cursor: index,
                start: false,
                shift: false,
            });
        }
        cx.notify();
    }
    /// A release ends the drag; a click with nothing selected opens the link
    /// under it.
    fn mouse_up(&mut self, event: &gpui::MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
        if self.text.selection().is_empty() {
            let index = self.index(event.position);
            if let Some((_, link)) = self.links.iter().find(|(range, _)| range.contains(&index)) {
                let _ = super::open_terminal_link(link);
            }
        }
    }
    /// The top layer: inline code drawn over the text, the input handler, the
    /// caret, and scrolling the caret into view after an edit.
    fn caret_canvas(
        paint: gpui::Entity<Self>,
        layout: TextLayout,
        readonly: bool,
        placeholder: bool,
    ) -> impl IntoElement {
        gpui::canvas(
            move |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                paint.update(cx, |input, cx| {
                    input.paint_inline_code(&layout, bounds, false, window, cx)
                });
                let input = paint.read(cx);
                let focused = input.focus.is_focused(window);
                let selection = input.text.selection();
                if !readonly {
                    window.handle_input(
                        &input.focus,
                        gpui::ElementInputHandler::new(bounds, paint.clone()),
                        cx,
                    );
                }
                if focused && !readonly && selection.is_empty() {
                    let position = if placeholder {
                        Some(bounds.origin)
                    } else {
                        input.caret(&layout)
                    };
                    if let Some(position) = position {
                        window.paint_quad(fill(
                            Bounds::new(position, size(px(1.), px(20.))),
                            input.chrome.focus,
                        ));
                    }
                }
                let cursor = input.caret(&layout);
                paint.update(cx, |input, cx| {
                    input.bounds = Some(bounds);
                    if !readonly
                        && std::mem::take(&mut input.ensure_cursor)
                        && let Some(cursor) = cursor
                    {
                        let view = input.scroll.bounds();
                        let mut offset = input.scroll.offset();
                        offset.y += reveal(cursor.y, px(20.), view.top(), view.bottom());
                        if offset != input.scroll.offset() {
                            input.scroll.set_offset(offset);
                            cx.notify();
                        }
                    }
                });
            },
        )
        // Pinned to the top of the content: an absolute child with no inset
        // sits after the text, which gave the editor a blank screenful of
        // extra scroll below its last line.
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}
impl Focusable for ChatInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EntityInputHandler for ChatInput {
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
        if self.readonly {
            return;
        }
        let Some(inserted) = self.accept(range.clone(), text) else {
            return;
        };
        self.checkpoint();
        self.ensure_cursor = true;
        self.text.replace(range, &inserted);
        cx.emit(InputEvent::Changed);
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
        if self.readonly {
            return;
        }
        let Some(text) = self.accept(range.clone(), text) else {
            return;
        };
        self.ensure_cursor = true;
        self.text.replace(range, &text);
        cx.emit(InputEvent::Changed);
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
        let layout = self.layout.as_ref()?;
        let range = self.text.utf8_range(range);
        let start = layout.position_for_index(range.start)?;
        let end = layout.position_for_index(range.end)?;
        Some(Bounds::from_corners(
            start,
            point(end.x.max(start.x + px(1.)), end.y + px(20.)),
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
#[derive(Clone)]
pub enum InputEvent {
    Select {
        anchor: usize,
        cursor: usize,
        start: bool,
        shift: bool,
    },
    SelectMessage,
    CopySelection,
    Changed,
    Submit,
    Interrupt,
    Back,
}
impl gpui::EventEmitter<InputEvent> for ChatInput {}
impl Render for ChatInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{InteractiveElement, StatefulInteractiveElement, prelude::FluentBuilder};
        let paint = cx.entity();
        let placeholder = self.text.value.is_empty() && !self.readonly;
        let value = if placeholder {
            self.placeholder.clone()
        } else {
            self.text.value.clone()
        };
        let spans = self.highlights(placeholder, window);
        let text = gpui::StyledText::new(value)
            .with_highlights(spans)
            .with_font_family_overrides(
                self.monos
                    .iter()
                    .map(|range| (range.clone(), super::MONO.into())),
            );
        let layout = text.layout().clone();
        self.layout = Some(layout.clone());
        let readonly = self.readonly;
        let code_paint = paint.clone();
        let code_layout = layout.clone();
        gpui::div()
            .id(self.field_id.clone())
            .relative()
            .min_w_0()
            .min_h(px(20.))
            .w_full()
            .track_focus(&self.focus)
            .role(if readonly {
                gpui::Role::Document
            } else {
                gpui::Role::MultilineTextInput
            })
            .aria_label(self.label.clone())
            .aria_value(self.text.value.clone())
            .aria_placeholder(self.placeholder.clone())
            .text_size(px(if self.code { 12.5 } else { 14. }))
            .line_height(px(20.))
            .text_color(if placeholder {
                self.chrome.ink_3
            } else {
                self.chrome.ink_1
            })
            .when(self.code, |d| d.font_family(super::MONO))
            .when(!readonly, |d| {
                d.min_h(px(20.))
                    .max_h(self.max_h)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
            })
            .when(self.code, |d| d.overflow_x_scroll())
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .when_some(self.hover_link.clone(), |d, link| {
                let chrome = self.chrome;
                d.tooltip(move |_, cx| {
                    cx.new(|_| super::KeyTip {
                        bg: chrome.toast_bg,
                        fg: chrome.toast_fg,
                        text: link.clone().into(),
                    })
                    .into()
                })
            })
            .child(
                gpui::canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        code_paint.update(cx, |input, cx| {
                            input.paint_inline_code(&code_layout, bounds, true, window, cx)
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                gpui::div()
                    .when(self.code, |d| d.whitespace_nowrap())
                    .child(text),
            )
            .child(Self::caret_canvas(paint, layout, readonly, placeholder))
    }
}

#[cfg(test)]
mod tests;
