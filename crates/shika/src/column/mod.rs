//! The left column: its layout, width and drag handle, the title bar, and the footer.
use super::*;

mod card;
mod leads;
mod projects;

pub(crate) use card::*;

/// The drag on the agent column's edge. It draws nothing: the column itself
/// follows the pointer.
pub(crate) struct ColumnDrag;

impl Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The narrowest column that fits the footer's key hints beside the Leftover
/// worktrees button. Beside the gear alone they fit the narrowest column.
const FOOTER_HINTS_FIT: f32 = 420.;

/// The invisible strip over the column's edge that starts a resize.
pub(crate) const COLUMN_HANDLE_WIDTH: f32 = 8.;

/// The top row of both halves of the window, which is the title bar.
pub(crate) const BAR_HEIGHT: f32 = 48.;

/// The traffic lights sit 18px from the left, centered in the 48px bar.
/// AppKit's buttons are 14px tall on macOS 26, so 17px above and below.
pub(crate) const TRAFFIC_LIGHTS: (f32, f32) = (18., 17.);

/// The traffic lights end at x=78. The wordmark starts 12px after them.
const WORDMARK_INSET: f32 = 90.;

impl Shika {
    /// The Leads section above the projects, each Lead followed by its
    /// workers; then every project's own cards, sorted by attention.
    pub(crate) fn column_layout(&self) -> model::ColumnLayout {
        let placements = self
            .cards
            .iter()
            .map(|c| model::Placement {
                project: &c.project,
                rank: c.status.rank(),
                lead: c.lead.is_some(),
                checkout: c.is_checkout(),
                session: c.session.as_ref().map(|s| s.id.as_str()),
                started_by: c.started_by.as_deref(),
            })
            .collect::<Vec<_>>();
        let projects = self
            .projects
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>();
        let resting = self
            .resting_leads()
            .into_iter()
            .map(|(project, cli)| model::Resting {
                project,
                cli: self.cli_name(cli),
            })
            .collect::<Vec<_>>();
        let departing = self
            .departing
            .iter()
            .filter_map(|d| match &d.home {
                Home::Project(project) => Some(project.as_str()),
                Home::Leads(_) => None,
            })
            .collect::<Vec<_>>();
        model::ColumnLayout::with_departing(&projects, &placements, &resting, &departing)
    }

    /// Hide or show the agent column. Focus stays where it is: with the
    /// column hidden, `j` / `k` still change the agent on screen.
    pub(crate) fn toggle_column(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.column.hidden = !self.column.hidden;
        self.save_settings();
        cx.notify();
    }

    /// The column's and the Changes panel's widths on screen, `None` while
    /// hidden or closed. A window too narrow for the stored widths and a
    /// readable terminal shows less: the panel gives way first, then the
    /// column.
    pub(crate) fn pane_widths(&self, window: &Window) -> (Option<Pixels>, Option<Pixels>) {
        let (column, panel) = changes::pane_widths(
            f32::from(window.viewport_size().width),
            MIN_TERMINAL_WIDTH,
            (!self.column.hidden)
                .then_some(self.column.width(f32::from(window.viewport_size().width))),
            self.changes
                .open
                .then_some(f32::from(self.changes.width.width)),
        );
        (column.map(px), panel.map(px))
    }

    /// The column's width on screen, or `None` while it is hidden.
    pub(crate) fn column_width(&self, window: &Window) -> Option<Pixels> {
        self.pane_widths(window).0
    }

    /// The column's edge follows the pointer and stops at the narrowest
    /// width. A drag never hides the column; only the toggle does.
    pub(crate) fn drag_column(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_active_drag_cursor_style(gpui::CursorStyle::ResizeLeftRight, window);
        self.column_drag_from.get_or_insert(self.column);
        // Beside the Changes panel as shown, the terminal still keeps 420.
        let panel = self.pane_widths(window).1.unwrap_or(px(0.));
        let viewport = window.viewport_size().width;
        let room = viewport - px(MIN_TERMINAL_WIDTH) - panel;
        let next = self
            .column
            .with_width(f32::from(x.min(room)), f32::from(viewport));
        if next != self.column {
            self.column = next;
            cx.notify();
        }
    }

    /// Empty space in a 48px top row drags the window, and a double-click
    /// zooms. The drag starts on the first move, so a double-click can still
    /// zoom. Buttons in the row occlude it, so pressing them never drags.
    pub(crate) fn title_drag<E: InteractiveElement>(
        &self,
        element: E,
        cx: &mut Context<Self>,
    ) -> E {
        element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, _| {
                    if event.click_count >= 2 {
                        this.title_drag = false;
                        window.titlebar_double_click();
                    } else {
                        this.title_drag = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.title_drag = false;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.title_drag = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.title_drag {
                    this.title_drag = false;
                    window.start_window_move();
                }
            }))
    }

    /// An invisible strip over the column's edge. Dragging it resizes the
    /// column, and a double-click puts back the default width. Its line shows
    /// on hover and for the length of a drag.
    pub(crate) fn column_handle(
        &self,
        width: Pixels,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let line = chrome.ink_4;
        div()
            .id("column-handle")
            .group("column-handle")
            .occlude()
            .absolute()
            .top_0()
            .bottom_0()
            .left(width - px(COLUMN_HANDLE_WIDTH / 2.))
            .w(px(COLUMN_HANDLE_WIDTH))
            .flex()
            .justify_center()
            .cursor_col_resize()
            .on_drag(ColumnDrag, |_, _, _, cx| cx.new(|_| ColumnDrag))
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.column = this.column.reset();
                    this.save_settings();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .when(self.column_drag_from.is_some(), |d| d.bg(line))
                    .group_hover("column-handle", move |style| style.bg(line)),
            )
    }

    /// The button that hides the column, in its top row, or shows it again,
    /// in the terminal's top row after the traffic lights.
    fn column_toggle(
        &self,
        ink: Rgba,
        ink_hover: Rgba,
        hover: Rgba,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let tip = if self.column.hidden {
            "Show agent column  \u{2318}B"
        } else {
            "Hide agent column  \u{2318}B"
        };
        div()
            .id("column-toggle")
            .group("column-toggle")
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
                cx.new(|_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: tip.into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_column(cx)))
            .child(
                gpui::svg()
                    .data(COLUMN_ICON)
                    .size(px(15.))
                    .text_color(ink)
                    .group_hover("column-toggle", move |style| style.text_color(ink_hover)),
            )
    }

    /// Keep the app name and column toggle together on every title surface.
    pub(crate) fn title_identity(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .child("Shika"),
            )
            .child(self.column_toggle(chrome.ink_3, chrome.ink_1, chrome.hover, chrome, cx))
    }

    /// Where the title bar's content starts on the left: after the traffic
    /// lights, or near the edge in full screen, where macOS hides them.
    pub(crate) fn title_inset(window: &Window) -> Pixels {
        if window.is_fullscreen() || window.is_simple_fullscreen() {
            px(18.)
        } else {
            px(WORDMARK_INSET)
        }
    }

    /// The sidebar half of the title bar: the real traffic lights, the
    /// wordmark, and the column toggle beside it. The system title is hidden.
    /// New agent sits on the Projects header and the gear in the footer.
    pub(crate) fn top_row(
        &self,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inset = Self::title_inset(window);
        let row = div()
            .id("titlebar")
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(inset)
            .pr(px(14.))
            .child(self.title_identity(chrome, cx));
        self.title_drag(row, cx)
    }

    /// The settings gear, leftovers, and the key hints. A column too narrow
    /// for the hints beside Leftover worktrees drops them; the keys still
    /// work.
    pub(crate) fn footer(
        &self,
        chrome: &Chrome,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hints_fit = self.leftovers.is_empty() || width >= px(FOOTER_HINTS_FIT);
        let hover = chrome.hover;
        let text_button = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(6.))
                .px(px(8.))
                .py(px(5.))
                .text_size(px(12.5))
                .line_height(px(16.))
                .text_color(chrome.ink_2)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
        };
        // One row. `justify_between` already pins the hints to the right.
        // `ml_auto` did that a second time and pushed the hints past the
        // column edge.
        div()
            .w_full()
            .flex_shrink_0()
            .border_t_1()
            .border_color(chrome.line_2)
            .pt(px(10.))
            .px(px(12.))
            .pb(px(12.))
            .flex()
            .flex_nowrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_nowrap()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        icon_button(
                            "settings",
                            Some(SETTINGS_ICON),
                            "Settings  \u{2318},",
                            chrome,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                        ),
                    )
                    .when(!self.leftovers.is_empty(), |d| {
                        d.child(
                            text_button("leftovers")
                                .flex_none()
                                .child(format!("Leftover worktrees ({})", self.leftovers.len()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.busy || this.overlay.is_some() {
                                        return;
                                    }
                                    this.overlay = Some(Overlay::Leftovers);
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(hints_fit, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_nowrap()
                        .items_center()
                        .gap_x(px(12.))
                        .child(hint("\u{2318}] \u{2318}[", "move", chrome))
                        .child(hint("\u{2318}\u{21e7}W", "close", chrome)),
                )
            })
    }
}
