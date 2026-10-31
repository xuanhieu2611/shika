//! The panel view: its title row, the list or an empty state, the toggle
//! button, and the resizable left edge with its width rules.

use super::paint::RowPaint;
use super::{
    CHANGES_ICON, ChangesDrag, Content, DiffView, HINTS_FIT, INSET, LINE_HEIGHT, LIST_BOTTOM,
    LIST_TOP, Metrics, READING_AFTER, Target, safe_label,
};
use crate::appearance::{Chrome, with_alpha};
use crate::{BAR_HEIGHT, CONTROL_BAND, KeyTip, MIN_TERMINAL_WIDTH, MONO, Shika, kbd, model};
use gpui::{
    AnyElement, AppContext, Context, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use shika_core::Changes;
use std::{sync::Arc, time::Instant};

impl Shika {
    /// A drag on the panel's edge. It stops at 320 and where the terminal
    /// would go under 420, never closes the panel, and never moves the
    /// column. The width is saved once the drag ends.
    pub fn drag_changes(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_active_drag_cursor_style(gpui::CursorStyle::ResizeLeftRight, window);
        self.changes.drag_from.get_or_insert(self.changes.width);
        let viewport = f32::from(window.viewport_size().width);
        let column = self.column_width(window).map_or(0., f32::from);
        let width = drag_width(viewport, f32::from(x), MIN_TERMINAL_WIDTH, column);
        let next = self.changes.width.with_width(width);
        if next != self.changes.width {
            self.changes.width = next;
            cx.notify();
        }
    }

    fn changes_metrics(&mut self, window: &Window) -> Metrics {
        let points = self.font_size.points();
        if let Some(metrics) = self.changes.metrics
            && metrics.points == points
        {
            return metrics;
        }
        let font_size = px(points);
        let text = window.text_system();
        let font = text.resolve_font(&gpui::font(MONO));
        let cell = text
            .advance(font, font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(font_size * 0.6);
        let metrics = Metrics {
            points,
            font_size,
            row: (font_size * LINE_HEIGHT).round().max(px(21.)),
            cell,
        };
        self.changes.metrics = Some(metrics);
        metrics
    }

    /// The show/hide button: at the right end of the terminal header while
    /// the panel is closed, and of the panel's title row while it is open,
    /// 12 from the window's edge in both places.
    pub fn changes_toggle(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = match (
            self.changes.open,
            crate::shortcuts::bound_label(&self.keys, "toggleChanges"),
        ) {
            (true, Some(shortcut)) => format!("Hide changes  {shortcut}"),
            (false, Some(shortcut)) => format!("Show changes  {shortcut}"),
            (true, None) => "Hide changes".to_string(),
            (false, None) => "Show changes".to_string(),
        };
        let (hover, ink_hover) = (chrome.term_hover, chrome.term_white);
        div()
            .id("changes-toggle")
            .group("changes-toggle")
            .occlude()
            .flex_none()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |_, cx| {
                let text = tip.clone();
                cx.new(move |_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: text.into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_changes(window, cx)))
            .child(
                gpui::svg()
                    .data(CHANGES_ICON)
                    .size(px(15.))
                    .text_color(chrome.term_dim)
                    .group_hover("changes-toggle", move |style| style.text_color(ink_hover)),
            )
    }

    /// The invisible strip over the panel's left edge. Dragging it resizes
    /// the panel; a double-click restores 480.
    pub fn changes_handle(
        &self,
        left: Pixels,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let line = chrome.ink_4;
        div()
            .id("changes-handle")
            .group("changes-handle")
            .occlude()
            .absolute()
            .top_0()
            .bottom_0()
            .left(left - px(crate::COLUMN_HANDLE_WIDTH / 2.))
            .w(px(crate::COLUMN_HANDLE_WIDTH))
            .flex()
            .justify_center()
            .cursor_col_resize()
            .on_drag(ChangesDrag, |_, _, _, cx| cx.new(|_| ChangesDrag))
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.changes.width = Changes::default();
                    this.save_settings();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .when(self.changes.drag_from.is_some(), |d| d.bg(line))
                    .group_hover("changes-handle", move |style| style.bg(line)),
            )
    }

    /// The panel: its title row, then the list or an empty state. It paints
    /// term-surface once; rows paint only their opaque tints.
    pub fn changes_panel(
        &mut self,
        width: Pixels,
        chrome: &Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.changes.focus.is_focused(window);
        let metrics = self.changes_metrics(window);
        let title = self.changes_title(width, focused, chrome, cx);
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(chrome.term_surface);
        let content = self.changes_body(width, metrics, chrome, cx);
        div()
            .id("changes")
            .track_focus(&self.changes.focus)
            .key_context("Changes")
            .on_key_down(cx.listener(Self::changes_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.changes.focus, cx);
                    cx.notify();
                }),
            )
            .relative()
            .flex_none()
            .w(width)
            .h_full()
            .flex()
            .flex_col()
            .text_color(chrome.term_fg)
            .child(title)
            .child(body.child(content))
            // The left hairline, over the panel's own fills: the window
            // under it can be transparent.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(1.))
                    .bg(chrome.term_line),
            )
    }

    /// The title row: "Changes", the diff stat, the hints when focused and
    /// wide enough, and the toggle at the right end.
    fn changes_title(
        &self,
        width: Pixels,
        focused: bool,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let stat = self
            .changes
            .view()
            .filter(|view| view.stat.files > 0)
            .map(|view| {
                model::diff_stat_label(view.stat.files, view.stat.insertions, view.stat.deletions)
            });
        let hint = |key: &'static str, label: &'static str| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.5))
                .line_height(px(14.))
                .text_color(chrome.term_faint)
                .child(kbd(key, chrome.term_line, chrome.term_dim))
                .child(label)
        };
        let title = div()
            .id("changes-title")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_end()
            .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
            .border_b_1()
            .border_color(chrome.term_line)
            .child(
                div()
                    .h(px(CONTROL_BAND))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(20.))
                    .pr(px(12.))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if focused {
                                chrome.term_white
                            } else {
                                chrome.term_fg
                            })
                            .child("Changes"),
                    )
                    .when_some(stat, |d, stat| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(chrome.term_dim)
                                .child(stat),
                        )
                    })
                    .when(focused && f32::from(width) >= HINTS_FIT, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .flex()
                                .gap(px(12.))
                                .child(hint("r", "refresh"))
                                .child(hint("esc", "close")),
                        )
                    })
                    .child(div().flex_1())
                    .child(self.changes_toggle(chrome, cx)),
            );
        self.title_drag(title, cx)
    }

    /// The list area's content: the list, or an empty or failed state.
    fn changes_body(
        &mut self,
        width: Pixels,
        metrics: Metrics,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match panel_body(
            self.changes.target.as_ref(),
            &self.changes.content,
            Instant::now(),
        ) {
            Body::Message(text) => body_empty(chrome).child(text).into_any_element(),
            Body::Blank => div().into_any_element(),
            Body::Failed(line) => body_empty(chrome)
                .gap(px(6.))
                .child(COULD_NOT_READ)
                .when(!line.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w_full()
                            .px(px(INSET))
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .text_color(chrome.term_fainter)
                            .child(safe_label(line)),
                    )
                })
                .into_any_element(),
            Body::List(view) => {
                let view = view.clone();
                self.changes_list(view, width, metrics, chrome, cx)
            }
        }
    }

    fn changes_list(
        &mut self,
        view: Arc<DiffView>,
        width: Pixels,
        metrics: Metrics,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = f32::from(metrics.cell).max(1.);
        // Room for line text: the panel less both insets and the gutter.
        let text_width =
            f32::from(width) - 4. * INSET - 2. - cell * (view.number_columns + 1) as f32;
        let text_columns = (text_width / cell).floor().max(1.) as usize;
        self.changes.max_shift = view.columns.saturating_sub(text_columns) as f32 * cell;
        self.changes.shift = self.changes.shift.clamp(0., self.changes.max_shift);
        let start = (self.changes.shift / cell).floor() as usize;
        let paint = RowPaint {
            row: metrics.row,
            cell: metrics.cell,
            start,
            offset: px(self.changes.shift - start as f32 * cell),
            count: text_columns + 2,
            dim: chrome.term_dim,
            text: chrome.term_text,
            faint: chrome.term_faint,
            line: chrome.term_line,
            hover: chrome.term_hover,
            header: chrome.diff_file_header(),
            header_dim: chrome.diff_file_header_text(chrome.term_dim),
            header_white: chrome.diff_file_header_text(chrome.term_white),
            header_added: chrome.diff_file_header_text(chrome.diff_added.text),
            header_removed: chrome.diff_file_header_text(chrome.diff_removed.text),
            added: chrome.diff_added,
            removed: chrome.diff_removed,
            shika: cx.entity().downgrade(),
        };
        let row = metrics.row;
        let visible = self.changes.visible.clone();
        let mut list = gpui::uniform_list("changes-rows", visible.len, move |range, _, _| {
            range
                .filter_map(|ix| visible.raw(ix))
                .map(|ix| {
                    let is_folded = view
                        .file_of(ix)
                        .is_some_and(|f| visible.folded.contains(&f));
                    paint.render(&view, ix, is_folded)
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.changes.scroll)
        .flex_1()
        .min_h_0()
        .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
            // The list scrolls itself vertically; sideways moves the text.
            let delta = event.delta.pixel_delta(row);
            if delta.x.abs() > delta.y.abs() {
                this.shift_changes(-f32::from(delta.x), cx);
            }
        }));
        // A sideways swipe must not scroll the list down.
        list.style().restrict_scroll_to_axis = Some(true);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .pt(px(LIST_TOP))
            .pb(px(LIST_BOTTOM))
            .font_family(MONO)
            .text_size(metrics.font_size)
            .line_height(metrics.row)
            .child(list)
            .into_any_element()
    }
}

/// The widths on screen of the agent column and the panel, from their
/// saved widths (`None` when hidden or closed). The terminal keeps
/// `min_terminal`: the panel gives way first, beside the column's saved
/// width, then the column, beside the panel as shown. Neither goes under
/// its minimum, so only then does the terminal shrink.
pub fn pane_widths(
    viewport: f32,
    min_terminal: f32,
    column: Option<f32>,
    panel: Option<f32>,
) -> (Option<f32>, Option<f32>) {
    let column_min = f32::from(shika_core::Column::MIN_WIDTH);
    let panel_min = f32::from(Changes::MIN_WIDTH);
    let panel = panel.map(|saved| {
        saved
            .min(viewport - min_terminal - column.unwrap_or(0.))
            .max(panel_min)
    });
    let column = column.map(|saved| {
        saved
            .min(viewport - min_terminal - panel.unwrap_or(0.))
            .max(column_min)
    });
    (column, panel)
}

/// The panel width a drag on its left edge asks for, before the saved
/// range applies: the pointer's distance from the window's right edge, but
/// never so wide that the terminal goes under `min_terminal` beside the
/// column as shown.
pub fn drag_width(viewport: f32, x: f32, min_terminal: f32, column: f32) -> f32 {
    (viewport - x).min(viewport - min_terminal - column)
}

/// The list area's state, before any element is built.
#[derive(Debug)]
pub enum Body<'a> {
    /// One centered line, such as "No changes".
    Message(&'static str),
    /// The first read for a newly shown card, under 500ms old.
    Blank,
    /// [`COULD_NOT_READ`], with git's first error line under it unless empty.
    Failed(&'a str),
    List(&'a Arc<DiffView>),
}

/// The title of the failed state.
pub const COULD_NOT_READ: &str = "Could not read changes";

/// What the list area shows for `target` and `content` at `now`.
pub fn panel_body<'a>(target: Option<&Target>, content: &'a Content, now: Instant) -> Body<'a> {
    match (target, content) {
        (None | Some(Target::NoAgent), _) => Body::Message("No agent selected"),
        (Some(Target::Preparing), _) => Body::Message("Preparing worktree"),
        (Some(Target::SetupFailed), _) => Body::Message("Setup failed"),
        (Some(Target::Lead), _) => Body::Message("The Lead does not change files"),
        (Some(Target::Session(_)), Content::Reading { since }) => {
            if now.saturating_duration_since(*since) >= READING_AFTER {
                Body::Message("Reading changes")
            } else {
                Body::Blank
            }
        }
        (Some(Target::Session(_)), Content::Failed(line)) => Body::Failed(line),
        (Some(Target::Session(_)), Content::Ready(view)) if view.rows.is_empty() => {
            Body::Message("No changes")
        }
        (Some(Target::Session(_)), Content::Ready(view)) => Body::List(view),
    }
}

/// The empty state's box, centered in the list area.
fn body_empty(chrome: &Chrome) -> gpui::Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(chrome.term_faint)
}
