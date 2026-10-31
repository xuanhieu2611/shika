//! The right side of the window: the terminal header, the visible terminal or Lead chat, and the closing terminal.
use super::*;

/// The terminal keeps at least this much of the window beside the column.
/// The 960px window minimum leaves room for the column at 30% or 320px.
pub(super) const MIN_TERMINAL_WIDTH: f32 = 420.;

/// A closing task's terminal fades to a trace of its output.
const CLOSING_TERMINAL_OPACITY: f32 = 0.06;

impl Shika {
    /// The selected card's terminal under its header, or the empty state.
    pub(super) fn terminal_side(
        &self,
        chrome: &Chrome,
        cards_focused: bool,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if let Some(chat) = self.chat_side(chrome, window, cx) {
            return chat;
        }
        // With the column hidden, this row is the whole title bar: it starts
        // after the traffic lights with the wordmark and column toggle.
        let show_column = self.column.hidden.then(|| {
            div()
                .flex_none()
                .h(px(BAR_HEIGHT))
                .flex()
                .items_center()
                .pl(Self::title_inset(window))
                .pr(px(4.))
                .child(self.title_identity(chrome, cx))
        });
        // Each child paints its own background, so a translucent terminal is
        // not stacked over a second translucent fill.
        let mut right = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .text_color(chrome.term_fg);
        if let Some(i) = self.selected_card() {
            let card = &self.cards[i];
            let active_tab = card.active_tab;
            // The metadata row names the project and branch. The full worktree
            // path is its tooltip. Before the worktree exists, the setup stage
            // stands in for both.
            let path = SharedString::from(
                card.session
                    .as_ref()
                    .map(|s| model::tilde(&s.worktree, home))
                    .unwrap_or_else(|| card.stage.clone()),
            );
            let (project_name, branch, stage) = match card.session.as_ref() {
                Some(s) if s.is_checkout() => (
                    Some(path.clone()),
                    self.checkout_branch(&card.project)
                        .map(|branch| SharedString::from(branch.to_owned())),
                    None,
                ),
                Some(s) => (
                    self.projects
                        .iter()
                        .find(|p| p.id == card.project)
                        .map(|p| SharedString::from(p.name.clone())),
                    (s.kind == shika_core::SessionKind::Task)
                        .then(|| SharedString::from(s.branch.clone())),
                    None,
                ),
                None => (None, None, Some(SharedString::from(card.stage.clone()))),
            };
            let create_pr_chord = shortcuts::display("cmd-shift-p");
            let term_hover = chrome.term_hover;
            let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
            // Connected tabs: the active tab is filled like the terminal and
            // its sides flare into the header's bottom line, which breaks
            // under it. The line is drawn by each piece of the row, not the
            // header, because the active tab cannot cover a line it is
            // painted over while translucent.
            let line = chrome.term_line;
            let (tab_fill, term_fg, term_white) =
                (chrome.term_tab, chrome.term_fg, chrome.term_white);
            let baseline = |d: gpui::Div| d.flex_none().h_full().border_b_1().border_color(line);
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
            // A 1px line under a piece of the row, shortened by a flare's
            // width on a side where the active tab's flare meets it.
            let segment = |flare_left: bool, flare_right: bool| {
                let inset = |flare: bool| px(if flare { TAB_FLARE } else { 0. });
                div()
                    .absolute()
                    .bottom_0()
                    .h(px(1.))
                    .left(inset(flare_left))
                    .right(inset(flare_right))
                    .bg(line)
            };
            tabs = tabs.child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(TAB_LEAD))
                    .child(segment(false, active_tab == 0)),
            );
            let labels: Vec<_> = std::iter::once((card.preset.to_string(), false))
                .chain(card.shells.iter().map(|pane| {
                    let label = if pane.shell_number == 1 {
                        "Shell".to_string()
                    } else {
                        format!("Shell {}", pane.shell_number)
                    };
                    (label, true)
                }))
                .collect();
            let last = labels.len() - 1;
            for (tab, (label, closable)) in labels.into_iter().enumerate() {
                let active = active_tab == tab;
                let group = SharedString::from(format!("terminal-tab-{tab}"));
                let body = div()
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
                        d.child(
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
                                })),
                        )
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(tab, window, cx)),
                    );
                // The active tab's flares are painted outside its bounds, over
                // the bottom corners of its neighbors, so they never change
                // the layout. A neighbor's line stops where a flare meets it,
                // because a translucent flare cannot hide a line under it.
                // Hover paints the same shape in the hover wash, standing on
                // the line instead of breaking it, and rounds the corner that
                // meets an active neighbor's flare so the two fit together.
                let wrapper = div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_end();
                let (after_active, before_active) =
                    (tab > 0 && active_tab == tab - 1, active_tab == tab + 1);
                let wrapper = if active {
                    wrapper.child(tab_canvas(move |bounds, window| {
                        paint_active_tab(bounds, tab_fill, line, window)
                    }))
                } else {
                    wrapper
                        .when(card.hovered_tab == Some(tab), |d| {
                            d.child(tab_canvas(move |bounds, window| {
                                if let Some(path) =
                                    tab_shape(bounds, !after_active, !before_active, false)
                                {
                                    window.paint_path(path, term_hover);
                                }
                            }))
                        })
                        .child(segment(after_active, before_active))
                };
                tabs = tabs.child(wrapper.pb(px(1.)).child(body));
            }
            tabs = tabs.child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(TAB_FLARE))
                    .child(segment(active_tab == last, false)),
            );
            let header = div()
                .id("terminal-header")
                .h(px(BAR_HEIGHT))
                .flex_shrink_0()
                .flex()
                .bg(with_alpha(chrome.term_header, chrome.term_header_alpha))
                .children(show_column.map(baseline))
                .child(tabs)
                .when(card.lead.is_none(), |d| {
                    d.child(
                        baseline(div())
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
                                        .hover(move |style| {
                                            style.bg(term_hover).text_color(term_white)
                                        })
                                        .child("+")
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| KeyTip {
                                                bg: tip_bg,
                                                fg: tip_fg,
                                                text: "New shell tab  ⌘T".into(),
                                            })
                                            .into()
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_shell(true, window, cx)
                                        })),
                                ),
                            ),
                    )
                })
                .child(baseline(div()).flex_1().min_w_0())
                .child(
                    baseline(div())
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
                                    .child(
                                        kbd("\u{2318}\u{21e7}W", chrome.term_line, chrome.term_dim)
                                            .py_0(),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                                    ),
                            ),
                        ),
                )
                .when(!self.changes.open, |d| {
                    d.child(
                        baseline(div())
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
                });
            let pane = card.active_pane();
            right = right
                .child(self.title_drag(header, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(20.))
                        .pt(px(8.))
                        .pb(px(2.))
                        .flex_shrink_0()
                        .bg(chrome.term_surface)
                        .text_size(px(11.5))
                        .text_color(chrome.term_dim)
                        .child(
                            div()
                                .id("task-where")
                                .when(card.is_checkout(), |d| d.flex_1())
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .whitespace_nowrap()
                                .tooltip(move |_, cx| {
                                    cx.new(|_| KeyTip {
                                        bg: tip_bg,
                                        fg: tip_fg,
                                        text: path.clone(),
                                    })
                                    .into()
                                })
                                // The project never gives way before the branch.
                                .when_some(project_name, |d, name| {
                                    d.child(
                                        div()
                                            .when(!card.is_checkout(), |d| d.flex_none())
                                            .min_w_0()
                                            .truncate()
                                            .when(card.is_checkout(), |d| d.font_family(MONO))
                                            .text_size(px(12.))
                                            .text_color(chrome.term_fg)
                                            .child(name),
                                    )
                                })
                                .when_some(branch, |d, branch| {
                                    d.child(
                                        gpui::svg()
                                            .data(BRANCH_ICON)
                                            .flex_none()
                                            .size(px(12.))
                                            .text_color(chrome.term_dim),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(MONO)
                                            .when(card.is_checkout(), |d| {
                                                d.flex_none().max_w(gpui::relative(0.5))
                                            })
                                            .child(branch),
                                    )
                                })
                                .when_some(stage, |d, stage| {
                                    d.child(div().min_w_0().truncate().child(stage))
                                }),
                        )
                        .when(!card.is_checkout(), |d| d.child(div().flex_1()))
                        .when(
                            card.session.is_some()
                                && !card.creating
                                && card.lead.is_none()
                                && !card.is_checkout(),
                            |d| {
                                d.child(
                                    div()
                                        .id("create-pr")
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
                                        .child("Create PR")
                                        .child(
                                            kbd(
                                                create_pr_chord.clone(),
                                                chrome.term_line,
                                                chrome.term_dim,
                                            )
                                            .py_0(),
                                        )
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| KeyTip {
                                                bg: tip_bg,
                                                fg: tip_fg,
                                                text: format!(
                                                    "Commit, push, create PR  {create_pr_chord}"
                                                )
                                                .into(),
                                            })
                                            .into()
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.create_pr(window, cx)
                                        })),
                                )
                            },
                        )
                        .child(div().flex_none().text_color(chrome.term_faint).child(
                            if card.creating && card.is_checkout() {
                                "starting agent"
                            } else if card.creating {
                                "setup is non-interactive"
                            } else if card.launch_error.is_some() && cards_focused {
                                "↵ retry setup"
                            } else if cards_focused {
                                "↵ type here"
                            } else {
                                "ctrl q back to cards"
                            },
                        )),
                )
                .when(
                    matches!(card.launch, Launch::Recover { resume: true, .. })
                        && lock(&card.agent.state).exited,
                    |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .px(px(14.))
                                .py(px(8.))
                                .text_size(px(12.5))
                                .child(
                                    notice_secondary(
                                        "recovery-start-fresh",
                                        "Start fresh…",
                                        chrome,
                                    )
                                    .gap(px(7.))
                                    .child(kbd("⌘⇧A", chrome.sunken, chrome.ink_3))
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.switch_agent(window, cx),
                                    )),
                                )
                                .child(div().flex_1().min_w_0().text_color(chrome.term_dim).child(
                                    "The agent exited. Start a new conversation in this worktree.",
                                )),
                        )
                    },
                )
                .when(card.launch_error.is_some(), |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .px(px(14.))
                            .py(px(8.))
                            .text_size(px(12.5))
                            .child(
                                notice_secondary("retry-setup", "Retry setup", chrome).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.retry_preparation(window, cx)
                                    }),
                                ),
                            )
                            .child(div().text_color(chrome.term_dim).child(
                                "Creates a fresh worktree. Changed files remain in leftovers.",
                            )),
                    )
                })
                .child(closing_terminal(card, pane, chrome));
        } else {
            let icon = Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                include_bytes!("../../../assets/macos/shika-app-icon-256.png").to_vec(),
            ));
            let key = |key: &'static str, label: &'static str| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(kbd(key, chrome.term_seg, chrome.term_dim))
                    .child(label)
            };
            // A resting Lead says so, and a click on the area resumes it.
            let resting = self.selected_resting().map(|project| {
                let name = self
                    .projects
                    .iter()
                    .find(|p| p.id == project)
                    .map_or("The", |p| p.name.as_str());
                (project.to_string(), format!("{name} Lead is not running"))
            });
            let (message, keys) = match &resting {
                Some((_, message)) => (message.as_str(), key("\u{21b5}", "resume")),
                None => (
                    "No agent selected",
                    div()
                        .flex()
                        .gap(px(14.))
                        .child(key("j", "select"))
                        .child(key("\u{2318}N", "new agent")),
                ),
            };
            right = right.child(
                div()
                    .id("terminal-empty")
                    .relative()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .when_some(resting.as_ref(), |d, (project, _)| {
                        let project = project.clone();
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.resume_lead(project.clone(), window, cx)
                            }))
                    })
                    .bg(with_alpha(chrome.term_empty, chrome.term_empty_alpha))
                    .text_size(px(13.))
                    .text_color(chrome.term_faint)
                    .child(
                        self.title_drag(
                            div()
                                .id("terminal-drag")
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(BAR_HEIGHT))
                                .flex()
                                .children(show_column)
                                .child(div().flex_1())
                                .when(!self.changes.open, |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .h_full()
                                            .flex()
                                            .items_center()
                                            .pr(px(12.))
                                            .child(self.changes_toggle(chrome, cx)),
                                    )
                                }),
                            cx,
                        ),
                    )
                    .child(gpui::img(icon).size(px(64.)).mb(px(6.)))
                    .child(message.to_string())
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(chrome.term_fainter)
                            .child(keys),
                    ),
            );
        }
        if let Some((text, _)) = &self.toast {
            right = right.child(
                div()
                    .absolute()
                    .bottom(px(22.))
                    .left(px(20.))
                    .right(px(20.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(8.))
                            .bg(chrome.toast_bg)
                            .text_color(chrome.toast_fg)
                            .text_size(px(12.5))
                            .line_height(px(16.))
                            .shadow(vec![
                                BoxShadow::new(px(0.), px(6.), chrome.toast_shadow.into())
                                    .blur_radius(px(20.)),
                            ])
                            .child(text.clone()),
                    ),
            );
        }
        if let Some(card) = &self.update_notice
            && !card.centered()
        {
            right = right.child(
                div()
                    .absolute()
                    .bottom(px(22.))
                    .right(px(20.))
                    .child(self.update_notice_panel(card, chrome, window, cx)),
            );
        }
        right.into_any_element()
    }
}

/// The terminal area. While its task closes, the terminal fades to a trace
/// behind grey sparkling pixels and "Closing...", so the CLI's exit line and the
/// stopped session do not read as the result. Opacity only: the terminal
/// keeps its size, so its PTY is never resized.
fn closing_terminal(card: &Card, pane: &Pane, chrome: &Chrome) -> gpui::AnyElement {
    let terminal = div().flex_1().min_h_0().child(pane.view.clone());
    if !card.closing {
        return terminal.into_any_element();
    }
    let view = card.agent.view.entity_id();
    let fade = || gpui::Animation::new(model::CLOSING_FADE).with_easing(model::ease);
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(terminal.with_animation(
            SharedString::from(format!("terminal-closing-{view}")),
            fade(),
            |d, t| d.opacity(1. - (1. - CLOSING_TERMINAL_OPACITY) * t),
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .text_size(px(13.))
                .text_color(chrome.term_faint)
                .child(pixel_sparkle(
                    format!("closing-pixels-{view}"),
                    chrome.term_dim,
                    4.,
                    2.,
                ))
                .child("Closing...")
                .with_animation(
                    SharedString::from(format!("closing-label-{view}")),
                    fade(),
                    |d, t| d.opacity(t),
                ),
        )
        .into_any_element()
}
