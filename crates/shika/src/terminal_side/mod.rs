//! The right side of the window: the terminal header, the visible terminal or Lead chat, the empty state, and the toast.
use super::*;

mod header;
mod metadata;

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
            let header = self.terminal_header(card, show_column, chrome, cx);
            let pane = card.active_pane();
            right = right
                .child(self.title_drag(header, cx))
                .child(self.metadata_row(card, cards_focused, home, chrome, cx))
                .when(
                    matches!(card.launch, Launch::Recover { resume: true, .. })
                        && lock(&card.agent.state).exited,
                    |d| d.child(metadata::recovery_notice(chrome, cx)),
                )
                .when(card.launch_error.is_some(), |d| {
                    d.child(metadata::retry_setup_notice(chrome, cx))
                })
                .child(closing_terminal(card, pane, chrome));
        } else {
            right = right.child(self.empty_terminal(show_column, chrome, cx));
        }
        if let Some((text, _)) = &self.toast {
            right = right.child(toast(text.clone(), chrome));
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

    /// The empty state: "No agent selected" with its keys, or a resting
    /// Lead, which a click on the area resumes. A plain strip over it holds
    /// the column toggle (when hidden) and the Changes toggle.
    fn empty_terminal(
        &self,
        show_column: Option<gpui::Div>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let icon = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            include_bytes!("../../../../assets/macos/shika-app-icon-256.png").to_vec(),
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
            .child(self.empty_title_strip(show_column, chrome, cx))
            .child(gpui::img(icon).size(px(64.)).mb(px(6.)))
            .child(message.to_string())
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(chrome.term_fainter)
                    .child(keys),
            )
    }

    /// The plain title bar strip over the empty state: it drags the window
    /// and holds the column toggle (when hidden) and the Changes toggle.
    fn empty_title_strip(
        &self,
        show_column: Option<gpui::Div>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        )
    }
}

/// The toast, centered near the bottom of the terminal side.
fn toast(text: String, chrome: &Chrome) -> gpui::Div {
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
                    BoxShadow::new(px(0.), px(6.), chrome.toast_shadow.into()).blur_radius(px(20.)),
                ])
                .child(text),
        )
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
