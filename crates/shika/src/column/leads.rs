//! The Leads section: live Leads with their workers, and resting Leads.
use super::*;

impl Shika {
    /// The Leads section: its header with New Lead, then each Lead with its
    /// workers under it. With no Lead it is the header alone. A project with
    /// a Lead leaving still draws its exit here after the entry is gone.
    pub(crate) fn leads_section(
        &self,
        layout: &model::ColumnLayout,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().flex_none().flex().flex_col().child(
            section_header("Leads", chrome).child(
                icon_button("new-lead", None, "New Lead  \u{2318}L", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.new_lead(window, cx))),
            ),
        );
        for project in &self.projects {
            let entry = layout.leads.iter().find(|e| e.project == project.id);
            let leaving = self
                .departing
                .iter()
                .any(|d| d.home == Home::Leads(project.id.clone()));
            if entry.is_none() && !leaving {
                continue;
            }
            let indices = entry.map(|e| e.cards()).unwrap_or_default();
            section = self.card_rows(
                section,
                indices,
                &Home::Leads(project.id.clone()),
                chrome,
                cards_focused,
                reveal_selection,
                cx,
            );
            // A stopped Lead's row follows the exit of its card.
            if let Some(model::LeadEntry {
                kind: model::LeadKind::Resting { cli },
                ..
            }) = entry
            {
                section = section.child(
                    card_row().child(
                        self.resting_view(
                            &project.id,
                            &project.name,
                            cli,
                            chrome,
                            cards_focused,
                            reveal_selection,
                            cx,
                        )
                        .w_full()
                        .h(px(CARD_HEIGHT)),
                    ),
                );
            }
        }
        section
    }

    /// A saved Lead that is not running. It reads like its live card, with
    /// no signal, timer, or diff stat: the project, then the CLI and "Lead".
    /// A click or Enter resumes it.
    #[allow(clippy::too_many_arguments)]
    fn resting_view(
        &self,
        project: &str,
        name: &str,
        cli: &str,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self.selected_resting() == Some(project);
        let cli_id = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.lead.as_ref())
            .map_or("", |lead| lead.cli.as_str());
        // The latest reply's first line; only a Lead with a chat has one.
        let snippet = self.lead_snippets.get(project).cloned();
        let (quiet, quiet_separator) = if selected {
            (chrome.ink_3, chrome.ink_4)
        } else {
            (chrome.ink_4, chrome.ink_5)
        };
        let task = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if selected { chrome.ink_1 } else { chrome.ink_2 })
                    .child(name.to_string()),
            )
            // The signal's place stays empty, so the name truncates where a
            // live Lead's does.
            .child(div().flex_none().w(px(SIGNAL_SLOT)));
        let meta = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(quiet)
            .when_some(snippet.clone(), |d, line| {
                d.child(div().flex_1().min_w_0().truncate().child(line))
            })
            .when(snippet.is_none(), |d| {
                d.child(cli_mark(
                    SharedString::from(format!("cli-resting-{project}")),
                    cli_id,
                    cli,
                    quiet,
                    chrome,
                ))
                .child(div().flex_none().text_color(quiet_separator).child("·"))
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(chrome.ink_2)
                        .child("Lead"),
                )
                .child(div().flex_1())
            })
            .when(selected && cards_focused, |d| {
                d.child(row_hint("↵", "resume", chrome))
            });
        let (click, menu) = (project.to_string(), project.to_string());
        let base = div()
            .id(SharedString::from(format!("resting-lead-{project}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(10.))
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .capture_any_mouse_down(cx.listener(
                move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if event.button == MouseButton::Right
                        || (event.button == MouseButton::Left && event.modifiers.control)
                    {
                        cx.stop_propagation();
                        this.open_resting_menu(menu.clone(), event.position, window, cx);
                    }
                },
            ))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy || this.overlay.is_some() {
                    return;
                }
                this.selection.set(Some(Selection::Resting(click.clone())));
                this.resume_lead(click.clone(), window, cx);
            }));
        let base = if selected {
            base.bg(chrome.card_selected)
        } else {
            base.overflow_hidden()
        };
        base.flex_row()
            .gap(px(10.))
            .child(antler_tile(selected, chrome))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(task)
                    .child(meta),
            )
            .when(selected && reveal_selection, |d| {
                d.child(self.selection_reveal(cx))
            })
    }
}
