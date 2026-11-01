//! The terminal header on the 48px title bar row: the task's connected tabs,
//! the new shell button, Close task, and the Changes toggle.
use super::*;

impl Shika {
    /// The terminal header. With the column hidden, `show_column` (the
    /// wordmark and column toggle) leads the row.
    pub(super) fn terminal_header(
        &self,
        card: &Card,
        show_column: Option<gpui::Div>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let line = chrome.term_line;
        div()
            .id("terminal-header")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
            .children(show_column.map(|d| baseline(d, line)))
            .child(self.tab_strip(card, chrome, cx))
            .when(card.lead.is_none(), |d| {
                d.child(new_shell_button(chrome, cx))
            })
            .child(baseline(div(), line).flex_1().min_w_0())
            .child(self.close_task_button(card, chrome, cx))
            .when(!self.changes.open, |d| {
                d.child(
                    baseline(div(), line)
                        .flex()
                        .flex_col()
                        .justify_end()
                        .pr(px(12.))
                        .child(
                            div()
                                .h(px(CONTROL_BAND))
                                .flex()
                                .items_center()
                                .child(self.changes_toggle(chrome, cx)),
                        ),
                )
            })
    }

    /// Connected tabs: the active tab is filled like the terminal and its
    /// sides flare into the header's bottom line, which breaks under it. The
    /// line is drawn by each piece of the row, not the header, because the
    /// active tab cannot cover a line it is painted over while translucent.
    fn tab_strip(
        &self,
        card: &Card,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active_tab = card.active_tab;
        let line = chrome.term_line;
        // The strip holds a lead space before the first tab and a
        // trailing space after the last, wide enough for their outer
        // flares, so the scroll strip never clips a flare at its ends.
        let mut tabs = div()
            .id("terminal-tabs")
            .min_w_0()
            .h_full()
            .flex()
            .overflow_x_scroll()
            .track_scroll(&card.tab_scroll);
        tabs = tabs.child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(px(TAB_LEAD))
                .child(tab_segment(line, false, active_tab == 0)),
        );
        let labels = tab_labels(card);
        let last = labels.len() - 1;
        for (tab, (label, closable)) in labels.into_iter().enumerate() {
            let body = tab_body(tab, label, closable, active_tab == tab, chrome, cx);
            tabs = tabs.child(tab_wrapper(card, tab, chrome).pb(px(1.)).child(body));
        }
        tabs.child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(px(TAB_FLARE))
                .child(tab_segment(line, active_tab == last, false)),
        )
    }

    /// Close task at the right of the header: "Cancel setup" or "Cancel
    /// start" while the card is being created.
    fn close_task_button(&self, card: &Card, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        let term_hover = chrome.term_hover;
        baseline(div(), chrome.term_line)
            .flex()
            .flex_col()
            .justify_end()
            .pl(px(12.))
            // Closed, the Changes toggle follows 8 after Close task.
            .pr(px(if self.changes.open { 12. } else { 8. }))
            .child(
                div().h(px(CONTROL_BAND)).flex().items_center().child(
                    div()
                        .id("close")
                        .occlude()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .border_1()
                        .border_color(chrome.term_seg_active)
                        .rounded(px(6.))
                        .pl(px(10.))
                        .pr(px(6.))
                        .py(px(4.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(chrome.term_fg)
                        .cursor_pointer()
                        .hover(move |style| style.bg(term_hover))
                        .child(if card.creating && card.is_checkout() {
                            "Cancel start"
                        } else if card.creating {
                            "Cancel setup"
                        } else {
                            "Close task"
                        })
                        .child(kbd("\u{2318}\u{21e7}W", chrome.term_line, chrome.term_dim).py_0())
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                ),
            )
    }
}

/// A piece of the header row standing on its bottom line.
fn baseline(d: gpui::Div, line: Rgba) -> gpui::Div {
    d.flex_none().h_full().border_b_1().border_color(line)
}

/// A 1px line under a piece of the row, shortened by a flare's width on a
/// side where the active tab's flare meets it.
fn tab_segment(line: Rgba, flare_left: bool, flare_right: bool) -> gpui::Div {
    let inset = |flare: bool| px(if flare { TAB_FLARE } else { 0. });
    div()
        .absolute()
        .bottom_0()
        .h(px(1.))
        .left(inset(flare_left))
        .right(inset(flare_right))
        .bg(line)
}

/// Each tab's label, and whether it can be closed: the pinned CLI tab, then
/// the shells, numbered as they were opened.
fn tab_labels(card: &Card) -> Vec<(String, bool)> {
    std::iter::once((card.preset.to_string(), false))
        .chain(card.shells.iter().map(|pane| {
            let label = if pane.shell_number == 1 {
                "Shell".to_string()
            } else {
                format!("Shell {}", pane.shell_number)
            };
            (label, true)
        }))
        .collect()
}

/// One tab's label and, on a shell, its close button. A click selects it.
fn tab_body(
    tab: usize,
    label: String,
    closable: bool,
    active: bool,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let (term_fg, term_white) = (chrome.term_fg, chrome.term_white);
    let group = SharedString::from(format!("terminal-tab-{tab}"));
    div()
        .id(("terminal-tab", tab))
        .occlude()
        .group(group.clone())
        .relative()
        .h(px(TAB_HEIGHT))
        .min_w(px(88.))
        .max_w(px(180.))
        .flex()
        .items_center()
        .gap(px(6.))
        .pb(px(TAB_LABEL_LIFT))
        .pl(px(12.))
        .pr(px(if closable { 6. } else { 12. }))
        .text_size(px(12.))
        .line_height(px(16.))
        .cursor_pointer()
        .when(active, |d| d.text_color(term_white))
        .when(!active, |d| {
            d.text_color(chrome.term_dim)
                .hover(move |style| style.text_color(term_fg))
        })
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if let Some(i) = this.selected_card() {
                let card = &mut this.cards[i];
                if *hovered {
                    card.hovered_tab = Some(tab);
                } else if card.hovered_tab == Some(tab) {
                    card.hovered_tab = None;
                }
                cx.notify();
            }
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        )
        .when(closable, |d| {
            d.child(shell_close_button(tab, group, active, chrome, cx))
        })
        .on_click(cx.listener(move |this, _, window, cx| this.select_tab(tab, window, cx)))
}

/// A shell tab's `×`: always shown on the active tab, on hover otherwise.
fn shell_close_button(
    tab: usize,
    group: SharedString,
    active: bool,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let (term_hover, term_white) = (chrome.term_hover, chrome.term_white);
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    div()
        .id(("shell-close", tab))
        .flex_none()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_color(chrome.term_dim)
        .when(!active, |d| {
            d.opacity(0.).group_hover(group, |style| style.opacity(1.))
        })
        .hover(move |style| style.bg(term_hover).text_color(term_white))
        .child("×")
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: "Close shell tab (stops its processes)  ⌘W".into(),
            })
            .into()
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.close_tab(tab, window, cx)
        }))
}

/// The slot a tab stands in, with its paint under the label.
///
/// The active tab's flares are painted outside its bounds, over the bottom
/// corners of its neighbors, so they never change the layout. A neighbor's
/// line stops where a flare meets it, because a translucent flare cannot
/// hide a line under it. Hover paints the same shape in the hover wash,
/// standing on the line instead of breaking it, and rounds the corner that
/// meets an active neighbor's flare so the two fit together.
fn tab_wrapper(card: &Card, tab: usize, chrome: &Chrome) -> gpui::Div {
    let active_tab = card.active_tab;
    let (line, tab_fill, term_hover) = (chrome.term_line, chrome.term_tab, chrome.term_hover);
    let wrapper = div()
        .relative()
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .justify_end();
    let (after_active, before_active) = (tab > 0 && active_tab == tab - 1, active_tab == tab + 1);
    if active_tab == tab {
        wrapper.child(tab_canvas(move |bounds, window| {
            paint_active_tab(bounds, tab_fill, line, window)
        }))
    } else {
        wrapper
            .when(card.hovered_tab == Some(tab), |d| {
                d.child(tab_canvas(move |bounds, window| {
                    if let Some(path) = tab_shape(bounds, !after_active, !before_active, false) {
                        window.paint_path(path, term_hover);
                    }
                }))
            })
            .child(tab_segment(line, after_active, before_active))
    }
}

/// The `+` after the tabs: a new shell tab. A Lead has none.
fn new_shell_button(chrome: &Chrome, cx: &mut Context<Shika>) -> gpui::Div {
    let (term_hover, term_white) = (chrome.term_hover, chrome.term_white);
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    baseline(div(), chrome.term_line)
        .flex()
        .flex_col()
        .justify_end()
        .pr(px(4.))
        .child(
            div().h(px(CONTROL_BAND)).flex().items_center().child(
                div()
                    .id("new-terminal")
                    .occlude()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(15.))
                    .text_color(chrome.term_dim)
                    .cursor_pointer()
                    .hover(move |style| style.bg(term_hover).text_color(term_white))
                    .child("+")
                    .tooltip(move |_, cx| {
                        cx.new(|_| KeyTip {
                            bg: tip_bg,
                            fg: tip_fg,
                            text: "New shell tab  ⌘T".into(),
                        })
                        .into()
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.new_shell(true, window, cx))),
            ),
        )
}
