//! The metadata row under the terminal header (project, branch, Create PR,
//! and the focus hint), and the notices that can follow it.
use super::*;

impl Shika {
    /// The metadata row: where the task runs, Create PR on an ordinary task,
    /// and the focus hint.
    pub(super) fn metadata_row(
        &self,
        card: &Card,
        cards_focused: bool,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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
            .child(self.task_where(card, home, chrome))
            .when(!card.is_checkout(), |d| d.child(div().flex_1()))
            .when(
                card.session.is_some()
                    && !card.creating
                    && card.lead.is_none()
                    && !card.is_checkout(),
                |d| d.child(create_pr_button(chrome, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(chrome.term_faint)
                    .child(focus_hint(card, cards_focused)),
            )
    }

    /// The metadata row names the project and branch. The full worktree
    /// path is its tooltip. Before the worktree exists, the setup stage
    /// stands in for both.
    fn task_where(
        &self,
        card: &Card,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
    ) -> gpui::Stateful<gpui::Div> {
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
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
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
            })
    }
}

/// Create PR, a bordered button with its key cap.
fn create_pr_button(chrome: &Chrome, cx: &mut Context<Shika>) -> gpui::Stateful<gpui::Div> {
    let create_pr_chord = shortcuts::display("cmd-shift-p");
    let term_hover = chrome.term_hover;
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
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
        .child(kbd(create_pr_chord.clone(), chrome.term_line, chrome.term_dim).py_0())
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: format!("Commit, push, create PR  {create_pr_chord}").into(),
            })
            .into()
        })
        .on_click(cx.listener(|this, _, window, cx| this.create_pr(window, cx)))
}

/// The hint at the right of the metadata row: what typing or Enter does now.
fn focus_hint(card: &Card, cards_focused: bool) -> &'static str {
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
    }
}

/// After a resumed conversation's CLI exits: start a new conversation in the
/// same worktree.
pub(super) fn recovery_notice(chrome: &Chrome, cx: &mut Context<Shika>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .py(px(8.))
        .text_size(px(12.5))
        .child(
            notice_secondary("recovery-start-fresh", "Start fresh…", chrome)
                .gap(px(7.))
                .child(kbd("⌘⇧A", chrome.sunken, chrome.ink_3))
                .on_click(cx.listener(|this, _, window, cx| this.switch_agent(window, cx))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(chrome.term_dim)
                .child("The agent exited. Start a new conversation in this worktree."),
        )
}

/// A failed setup: Retry setup makes a fresh worktree.
pub(super) fn retry_setup_notice(chrome: &Chrome, cx: &mut Context<Shika>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .py(px(8.))
        .text_size(px(12.5))
        .child(
            notice_secondary("retry-setup", "Retry setup", chrome)
                .on_click(cx.listener(|this, _, window, cx| this.retry_preparation(window, cx))),
        )
        .child(
            div()
                .text_color(chrome.term_dim)
                .child("Creates a fresh worktree. Changed files remain in leftovers."),
        )
}
