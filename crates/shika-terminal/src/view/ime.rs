//! IME composition and plain text input from the platform input handler.

use std::ops::Range;

use gpui::{App, Bounds, Context, InputHandler, Pixels, Point, UTF16Selection, Window};

use super::TerminalView;

impl TerminalView {
    fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.send_input(text.as_bytes(), cx);
        cx.notify();
    }

    fn set_marked_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = (!text.is_empty()).then(|| text.to_string());
        window.invalidate_character_coordinates();
        cx.notify();
    }
}

/// The platform side of IME and plain text input. The terminal has no
/// editable document, so the only text it reports is the composition.
pub(super) struct TerminalInputHandler {
    pub(super) view: gpui::Entity<TerminalView>,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let len = self
            .view
            .read(cx)
            .marked_text
            .as_deref()
            .map(utf16_len)
            .unwrap_or(0);
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .read(cx)
            .marked_text
            .as_deref()
            .map(|text| 0..utf16_len(text))
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let view = self.view.read(cx);
        let text = view.marked_text.as_deref()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = range_utf16.start.min(units.len());
        let end = range_utf16.end.clamp(start, units.len());
        *adjusted_range = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| view.commit_text(text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.set_marked_text(new_text, window, cx));
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            view.marked_text = None;
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let view = self.view.read(cx);
        let layout = view.layout?;
        let cursor = view.snapshot.as_ref()?.cursor;
        let (row, col) = cursor.map(|c| (c.row, c.col)).unwrap_or((0, 0));
        let mut bounds = layout.cell_bounds(row, col, 1);
        bounds.origin.x += layout.cell_width * range_utf16.start as f32;
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    /// Held keys repeat, as in every terminal, instead of opening the
    /// accent picker.
    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}
