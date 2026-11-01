//! Opening, closing, and focus, and the panel's plain keys: scrolling, the
//! sideways shift, expanding, and refresh.

use super::STEP_COLUMNS;
use crate::Shika;
use gpui::{Context, FocusHandle, Focusable, KeyDownEvent, Window, px};

impl Shika {
    /// Open the panel and focus it, saving `return_focus` for its close.
    fn open_changes(
        &mut self,
        return_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.changes.open {
            self.changes.open = true;
            self.changes.target = None;
        }
        self.changes.return_focus = return_focus;
        window.focus(&self.changes.focus, cx);
        cx.notify();
    }

    /// Close the panel and drop its diff. From inside the panel, focus goes
    /// back to where it was when the panel opened; from anywhere else it
    /// stays put.
    pub fn close_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.changes.open {
            return;
        }
        let inside = self.changes.focus.is_focused(window);
        self.changes.open = false;
        self.changes.target = None;
        self.changes.clear();
        let saved = self.changes.return_focus.take();
        if inside {
            self.restore_changes_focus(saved, window, cx);
        }
        cx.notify();
    }

    /// The saved focus, if it still leads somewhere: the cards, or a live
    /// terminal. A terminal of another card than the one now selected gives
    /// way to the selected card's terminal, since the panel followed the
    /// selection. A closed view falls back to the cards.
    fn restore_changes_focus(
        &mut self,
        saved: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = saved.as_ref().filter(|handle| {
            self.cards.iter().any(|card| {
                std::iter::once(&card.agent)
                    .chain(card.shells.iter())
                    .any(|pane| pane.view.focus_handle(cx) == **handle)
            })
        });
        let Some(handle) = terminal.cloned() else {
            window.focus(&self.focus, cx);
            return;
        };
        let selected = self
            .selected_card()
            .map(|i| self.cards[i].active_pane().view.focus_handle(cx));
        if selected.as_ref() == Some(&handle) {
            window.focus(&handle, cx);
        } else if selected.is_some() && !self.busy {
            self.focus_terminal(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// Cmd+Option+B, the toggle buttons, and the View menu. Closing while
    /// something else has focus moves no focus.
    pub fn toggle_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        if self.changes.open {
            self.close_changes(window, cx);
        } else {
            let saved = window.focused(cx);
            self.open_changes(saved, window, cx);
        }
    }

    /// The panel's plain keys. They run only while the panel has focus, and
    /// none of them reaches a PTY.
    pub fn changes_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        let mods = &stroke.modifiers;
        if mods.platform || mods.control || mods.alt || mods.function {
            return;
        }
        let scroll = match (stroke.key.as_str(), mods.shift) {
            ("escape", false) => {
                self.close_changes(window, cx);
                None
            }
            ("j" | "down", false) => Some(Scroll::Rows(1)),
            ("k" | "up", false) => Some(Scroll::Rows(-1)),
            ("d", false) => Some(Scroll::HalfPage(1)),
            ("u", false) => Some(Scroll::HalfPage(-1)),
            ("space", false) => Some(Scroll::Page(1)),
            ("space", true) => Some(Scroll::Page(-1)),
            ("g", false) => Some(Scroll::Top),
            ("g", true) => Some(Scroll::Bottom),
            ("]", false) => Some(Scroll::NextFile),
            ("[", false) => Some(Scroll::PreviousFile),
            ("h" | "left", false) => {
                self.shift_changes(-self.step_columns_px(), cx);
                None
            }
            ("l" | "right", false) => {
                self.shift_changes(self.step_columns_px(), cx);
                None
            }
            ("o", false) => {
                self.expand_first_in_view(cx);
                None
            }
            ("r", false) => {
                self.fetch_changes(cx);
                None
            }
            _ => return,
        };
        cx.stop_propagation();
        if let Some(scroll) = scroll {
            self.scroll_changes(scroll, cx);
        }
    }

    fn step_columns_px(&self) -> f32 {
        self.changes
            .metrics
            .map_or(0., |m| f32::from(m.cell) * STEP_COLUMNS as f32)
    }

    fn scroll_changes(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        let (Some(_), Some(metrics)) = (self.changes.view(), self.changes.metrics) else {
            return;
        };
        let (top, viewport) = self.changes.top_and_viewport();
        let next = scroll_top(
            scroll,
            top,
            f32::from(metrics.row),
            self.changes.visible.len,
            viewport,
            &self.changes.visible.headers,
        );
        let state = self.changes.scroll.0.borrow();
        let offset = state.base_handle.offset();
        state
            .base_handle
            .set_offset(gpui::point(offset.x, px(-next)));
        drop(state);
        cx.notify();
    }

    /// Move the line text right (positive) or left, within the widest line.
    pub(super) fn shift_changes(&mut self, delta: f32, cx: &mut Context<Self>) {
        let next = (self.changes.shift + delta).clamp(0., self.changes.max_shift);
        if next != self.changes.shift {
            self.changes.shift = next;
            cx.notify();
        }
    }

    /// `o`: the first collapsed file on screen.
    fn expand_first_in_view(&mut self, cx: &mut Context<Self>) {
        let (Some(view), Some(metrics)) = (self.changes.view(), self.changes.metrics) else {
            return;
        };
        let (top, viewport) = self.changes.top_and_viewport();
        let found = first_in_view(
            &self.changes.visible.collapsed,
            top,
            f32::from(metrics.row),
            viewport,
        )
        .and_then(|row| self.changes.visible.raw(row))
        .and_then(|row| view.file_of(row));
        if let Some(file) = found {
            self.expand_changes_file(file, cx);
        }
    }
}

/// A scroll the keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    Rows(isize),
    HalfPage(isize),
    Page(isize),
    Top,
    Bottom,
    NextFile,
    PreviousFile,
}

/// The list's new top, in pixels from its first row, after `scroll` from
/// `top`. `row` is the row height, `viewport` the list's height, and
/// `headers` the file header rows in order. The result stays between the
/// first row and the last full page.
pub fn scroll_top(
    scroll: Scroll,
    top: f32,
    row: f32,
    rows: usize,
    viewport: f32,
    headers: &[usize],
) -> f32 {
    let max = (rows as f32 * row - viewport).max(0.);
    let page = (viewport / row).floor().max(1.);
    // Half a row of slack, so a header sitting at the top counts as there.
    let slack = row / 2.;
    let next = match scroll {
        Scroll::Rows(n) => top + n as f32 * row,
        Scroll::HalfPage(n) => top + n as f32 * (page / 2.).floor().max(1.) * row,
        Scroll::Page(n) => top + n as f32 * page * row,
        Scroll::Top => 0.,
        Scroll::Bottom => max,
        Scroll::NextFile => headers
            .iter()
            .map(|&header| header as f32 * row)
            .find(|&at| at > top + slack)
            .unwrap_or(top),
        Scroll::PreviousFile => headers
            .iter()
            .rev()
            .map(|&header| header as f32 * row)
            .find(|&at| at < top - slack)
            .unwrap_or(0.),
    };
    next.clamp(0., max)
}

/// The first of `rows` (sorted) on screen, with the list's top at `top` and
/// `viewport` high.
pub fn first_in_view(rows: &[usize], top: f32, row: f32, viewport: f32) -> Option<usize> {
    let first = (top / row).floor().max(0.) as usize;
    let last = ((top + viewport) / row).ceil().max(0.) as usize;
    let at = rows.partition_point(|&r| r < first);
    rows.get(at).copied().filter(|&r| r < last)
}
