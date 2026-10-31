//! Task-scoped terminal tabs: adding, selecting, cycling, and closing shell tabs, and painting the tab strip.
use super::*;

/// A terminal tab, sitting on the bottom line of the 48px header and
/// reaching to 5px under its top.
pub(super) const TAB_HEIGHT: f32 = 42.;

/// Lifts a tab's label off the tab's own middle onto the bar's middle line,
/// where the traffic lights, the wordmark, and every header control sit.
/// The tab ends on the 1px bottom line, so its box is 5..47.
pub(super) const TAB_LABEL_LIFT: f32 = BAR_HEIGHT - 2. - TAB_HEIGHT;

/// A header control's band: the bar above its 1px bottom line, minus the
/// same 1px on top, so a control centered in it sits on the bar's middle.
pub(super) const CONTROL_BAND: f32 = BAR_HEIGHT - 2.;

/// The radius of a tab's top corners and of the concave flares that turn its
/// sides into the bottom line. The flares reach this far past the tab.
pub(super) const TAB_FLARE: f32 = 7.;

/// The first tab's label lines up with the terminal text.
pub(super) const TAB_LEAD: f32 = 8.;

impl Shika {
    pub(super) fn new_shell(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].creating {
            return;
        }
        if self.cards[index].lead.is_some() {
            self.message("The Lead has no shell tabs. It works from its terminal.".into());
            cx.notify();
            return;
        }
        let Some(session) = self.cards[index].session.clone() else {
            return;
        };
        let opacity = self.terminal_opacity();
        let mut pane = Pane::new(
            self.core.clone(),
            false,
            opacity,
            self.font_size.points(),
            self.terminal_palette(window),
            window,
            cx,
        );
        let state = pane.state.clone();
        let terminal = pane.terminal.clone();
        self.cards[index].shell_serial += 1;
        pane.shell_number = self.cards[index].shell_serial;
        self.cards[index].shells.push(pane);
        let tab = self.cards[index].shells.len();
        self.cards[index].active_tab = tab;
        self.cards[index].reveal_active_tab();
        if focus {
            // Focus the new view now. Its host queues typeahead until the
            // PTY is bound, and startup must not steal focus back later.
            self.focus_terminal(window, cx);
        }
        let core = self.core.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let measured = loop {
                if let Some(size) = lock(&state).measured {
                    break size;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this.upgrade().is_none() {
                    return;
                }
            };
            let output_state = state.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.open_shell(
                        &session.id,
                        PtySize::new(measured.rows, measured.cols),
                        move |_, event| match event {
                            PtyEvent::Output(bytes) => terminal.feed(&bytes),
                            PtyEvent::Exit(_) => lock(&output_state).exited = true,
                        },
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(pty) => {
                        bind_host(&this.core, &state, pty);
                    }
                    Err(e) => {
                        let shell_focused = this.cards[index].shells[tab - 1]
                            .view
                            .focus_handle(cx)
                            .is_focused(window);
                        this.cards[index].shells.remove(tab - 1);
                        this.cards[index].active_tab = model::tab_after_close(
                            this.cards[index].active_tab,
                            tab,
                            this.cards[index].shells.len(),
                        );
                        if shell_focused {
                            this.focus_terminal(window, cx);
                        }
                        this.message(e.to_string());
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        if self.cards[index].chat || tab > self.cards[index].shells.len() {
            return;
        }
        self.cards[index].active_tab = tab;
        self.focus_terminal(window, cx);
    }

    pub(super) fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_card() else {
            return;
        };
        let card = &self.cards[index];
        let tab = model::adjacent_tab(card.active_tab, card.shells.len(), forward);
        self.select_tab(tab, window, cx);
    }

    pub(super) fn close_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() || tab == 0 {
            return;
        }
        let Some(index) = self.selected_card() else {
            return;
        };
        let card = &self.cards[index];
        let Some(pane) = card.shells.get(tab - 1) else {
            return;
        };
        let Some(session) = card.session.as_ref() else {
            return;
        };
        let Some(pty) = lock(&pane.state).pty else {
            return;
        };
        let id = session.id.clone();
        let focused = pane.view.focus_handle(cx).is_focused(window);
        let card = &mut self.cards[index];
        card.shells.remove(tab - 1);
        card.active_tab = model::tab_after_close(card.active_tab, tab, card.shells.len());
        // Process teardown may block; never do it on the UI thread.
        let core = self.core.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = core.close_shell(&id, pty);
            })
            .detach();
        if focused {
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }
}

/// Paints the active terminal tab across `bounds`: the tab plus `TAB_FLARE`
/// on each side for its flares, and the 1px line row under it. The rounded
/// top and the concave flares are one shape, so the tab runs into the
/// terminal. The fill stops at the outline's outer edge, so nothing
/// translucent is stacked; neighbors stop their line where the outline
/// meets it.
pub(super) fn paint_active_tab(
    bounds: Bounds<Pixels>,
    fill: Rgba,
    line: Rgba,
    window: &mut Window,
) {
    let (f, h) = (TAB_FLARE, TAB_HEIGHT);
    let w = f32::from(bounds.size.width);
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let at = |x: f32, y: f32| gpui::point(px(x0 + x), px(y0 + y));
    let radius = |r: f32| gpui::point(px(r), px(r));
    let (left, right) = (f, w - f);
    if let Some(path) = tab_shape(bounds, true, true, true) {
        window.paint_path(path, fill);
    }

    // The outline runs on pixel centers, from the bottom line on the left,
    // up and over the tab, and back down into the bottom line on the right.
    let mut outline = gpui::PathBuilder::stroke(px(1.));
    outline.move_to(at(0., h + 0.5));
    outline.line_to(at(0.5, h + 0.5));
    outline.arc_to(radius(f), px(0.), false, false, at(left + 0.5, h - f + 0.5));
    outline.line_to(at(left + 0.5, f));
    outline.arc_to(radius(f - 0.5), px(0.), false, true, at(left + f, 0.5));
    outline.line_to(at(right - f, 0.5));
    outline.arc_to(radius(f - 0.5), px(0.), false, true, at(right - 0.5, f));
    outline.line_to(at(right - 0.5, h - f + 0.5));
    outline.arc_to(radius(f), px(0.), false, false, at(w - 0.5, h + 0.5));
    outline.line_to(at(w, h + 0.5));
    if let Ok(path) = outline.build() {
        window.paint_path(path, line);
    }
}

/// A canvas over a terminal tab, extending `TAB_FLARE` past both sides and
/// covering the line row under it, so the tab's flares stay out of layout.
pub(super) fn tab_canvas(
    paint: impl FnOnce(Bounds<Pixels>, &mut Window) + 'static,
) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint(bounds, window),
    )
    .absolute()
    .left(px(-TAB_FLARE))
    .right(px(-TAB_FLARE))
    .bottom_0()
    .h(px(TAB_HEIGHT + 1.))
}

/// The filled shape of a terminal tab across `bounds`, which extend
/// `TAB_FLARE` past both sides of the tab. The top corners are rounded. A
/// side with a flare curves outward into the bottom line; a side without one
/// rounds its bottom corner inward, exactly filling the inside of an active
/// neighbor's flare. The shape reaches the outer edge of the active tab's
/// 1px outline, and with `line_row` it also covers the line row under the
/// tab, so the active tab opens into the terminal.
pub(super) fn tab_shape(
    bounds: Bounds<Pixels>,
    flare_left: bool,
    flare_right: bool,
    line_row: bool,
) -> Option<gpui::Path<Pixels>> {
    let (f, h) = (TAB_FLARE, TAB_HEIGHT);
    let w = f32::from(bounds.size.width);
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let at = |x: f32, y: f32| gpui::point(px(x0 + x), px(y0 + y));
    let radius = gpui::point(px(f - 0.5), px(f - 0.5));
    let corner = gpui::point(px(f), px(f));
    let bottom = if line_row { h + 1. } else { h };
    // The tab's sides: x = f on the left and w - f on the right. A bottom
    // corner's arc meets the side at h - f + 0.5 and the line at f - 0.5
    // from the side.
    let (left, right) = (f, w - f);
    let (side_end, reach) = (h - f + 0.5, f - 0.5);
    let mut shape = gpui::PathBuilder::fill();
    let foot = if flare_left {
        left - reach
    } else {
        left + reach
    };
    shape.move_to(at(foot, bottom));
    shape.line_to(at(foot, h));
    shape.arc_to(radius, px(0.), false, !flare_left, at(left, side_end));
    shape.line_to(at(left, f));
    shape.arc_to(corner, px(0.), false, true, at(left + f, 0.));
    shape.line_to(at(right - f, 0.));
    shape.arc_to(corner, px(0.), false, true, at(right, f));
    shape.line_to(at(right, side_end));
    let foot = if flare_right {
        right + reach
    } else {
        right - reach
    };
    shape.arc_to(radius, px(0.), false, !flare_right, at(foot, h));
    shape.line_to(at(foot, bottom));
    shape.close();
    shape.build().ok()
}
