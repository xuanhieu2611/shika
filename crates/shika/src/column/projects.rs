//! The Projects section: project headers, their cards, departing cards, and
//! scrolling the selected card into view.
use super::*;

impl Shika {
    /// The Projects header: its label, then Add project and New agent, the
    /// `+` lining up with each project header's `+` below it.
    pub(crate) fn projects_header(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        section_header("Projects", chrome)
            .child(
                icon_button(
                    "add-project",
                    Some(FOLDER_PLUS_ICON),
                    "Add project  \u{2318}O",
                    chrome,
                )
                .on_click(cx.listener(|this, _, window, cx| this.add_project(window, cx))),
            )
            .child(
                icon_button("new-agent", None, "New agent  \u{2318}N", chrome)
                    .on_click(cx.listener(|this, _, window, cx| this.picker(window, cx))),
            )
    }

    /// One project: its header, then the author's own cards. `indices` is
    /// that list: not a Lead, and not a worker of a live Lead.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project_group(
        &self,
        project: &Project,
        indices: Vec<usize>,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = project.id.clone();
        let selected = *self.selection == Some(Selection::Project(id.clone()))
            || self.target_project() == Some(id.as_str());
        let plus_kept = self.selected_project() == Some(id.as_str());
        let group = SharedString::from(format!("project-group-{id}"));
        let hover = chrome.hover;
        let ink_1 = chrome.ink_1;
        let select_id = id.clone();
        let remove_id = id.clone();
        let new_id = id.clone();
        let base_id = id.clone();
        let base_name = self.bases.get(&id).and_then(|b| b.name.clone());
        let base_group = SharedString::from(format!("base-group-{id}"));
        let base_chord = shortcuts::display("cmd-shift-b");
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let path = SharedString::from(model::tilde(&project.path, home));
        let header = div()
            .id(SharedString::from(format!("project-{id}")))
            .relative()
            .when(selected && reveal_selection, |d| {
                d.child(self.selection_reveal(cx))
            })
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(8.))
            .pr(px(4.))
            .min_h(px(24.))
            .rounded(px(6.))
            .when(selected, |d| d.bg(chrome.row_selected))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy || this.overlay.is_some() {
                    return;
                }
                this.click_project(select_id.clone(), window, cx);
            }))
            // The path is in the name's tooltip, not on the row.
            .child(
                div()
                    .id(SharedString::from(format!("name-{id}")))
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(chrome.ink_1)
                    .tooltip(move |_, cx| {
                        cx.new(|_| KeyTip {
                            bg: tip_bg,
                            fg: tip_fg,
                            text: path.clone(),
                        })
                        .into()
                    })
                    .child(project.name.clone()),
            )
            // The branch New starts from. A click or `b` changes it.
            .when_some(base_name, |d, name| {
                d.child(
                    div()
                        .id(SharedString::from(format!("base-{id}")))
                        .group(base_group.clone())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(chrome.ink_3)
                        .hover(move |style| style.text_color(ink_1))
                        .tooltip(move |_, cx| {
                            cx.new(|_| KeyTip {
                                bg: tip_bg,
                                fg: tip_fg,
                                text: format!("Base branch  {base_chord}").into(),
                            })
                            .into()
                        })
                        // The glyph tells the branch apart from the path before it.
                        .child(
                            gpui::svg()
                                .data(BRANCH_ICON)
                                .flex_none()
                                .size(px(12.))
                                .text_color(chrome.ink_3)
                                .group_hover(base_group, move |style| style.text_color(ink_1)),
                        )
                        .child(name)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_base(base_id.clone(), window, cx);
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id(SharedString::from(format!("forget-{id}")))
                    .flex_none()
                    .px(px(4.))
                    .rounded(px(4.))
                    .text_size(px(11.5))
                    .text_color(chrome.ink_3)
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
                    .hover(move |style| style.text_color(ink_1))
                    .child("Remove")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if !this.busy && this.overlay.is_none() {
                            this.overlay = Some(Overlay::RemoveProject(remove_id.clone()));
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("new-in-{id}")))
                    .flex_none()
                    .size(px(24.))
                    // Hidden, the slot keeps its width so nothing moves.
                    .when(!plus_kept, |d| {
                        d.opacity(0.)
                            .group_hover(group.clone(), |style| style.opacity(1.))
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(16.))
                    .text_color(chrome.ink_3)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover).text_color(ink_1))
                    .child("+")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.picker_for(new_id.clone(), window, cx);
                    })),
            );
        // Each card owns its leading gap. Collapsing a negative margin in a
        // gapped flex column corrupts GPUI's intrinsic group height, squeezing
        // surviving cards and moving other project headers out of place.
        let column = div().flex_none().flex().flex_col().child(header);
        self.card_rows(
            column,
            indices,
            &Home::Project(id),
            chrome,
            cards_focused,
            reveal_selection,
            cx,
        )
    }

    /// The rows of one group: each card, with the exits of cards that left
    /// it drawn where they were. Project cards and a worker under its Lead
    /// are indented. A Lead card is not.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn card_rows(
        &self,
        mut column: gpui::Div,
        indices: Vec<usize>,
        place: &Home,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let departing = self
            .departing
            .iter()
            .filter(|d| d.home == *place)
            .collect::<Vec<_>>();
        let shown = indices
            .iter()
            .map(|&at| self.cards[at].agent.view.entity_id())
            .collect::<Vec<_>>();
        for at in indices {
            let card = &self.cards[at];
            let view = card.agent.view.entity_id();
            for leaving in departing.iter().filter(|d| d.before == Some(view)) {
                column = column.child(self.departing_view(leaving, chrome, cx));
            }
            let face = self
                .card_view(card, Some(at), chrome, cards_focused, reveal_selection, cx)
                .w_full()
                .h(px(CARD_HEIGHT));
            let face = if card.closing {
                face.with_animation(
                    SharedString::from(format!("card-closing-{view}")),
                    gpui::Animation::new(model::CLOSING_FADE).with_easing(model::ease),
                    |d, t| d.opacity(1. - (1. - CLOSING_CARD_OPACITY) * t),
                )
                .into_any_element()
            } else {
                face.into_any_element()
            };
            let indent = matches!(place, Home::Project(_))
                || (matches!(place, Home::Leads(_)) && card.lead.is_none());
            column = column.child(
                card_row()
                    .when(indent, |d| d.pl(px(WORKER_INDENT)))
                    .child(face),
            );
        }
        // Cards whose follower is gone too leave from the end of the group.
        for leaving in departing
            .iter()
            .filter(|d| d.before.is_none_or(|view| !shown.contains(&view)))
        {
            column = column.child(self.departing_view(leaving, chrome, cx));
        }
        column
    }

    /// A closed card's exit: it shrinks toward its center and fades out, then
    /// its row collapses so the cards below slide up. GPUI cannot scale text,
    /// so the box shrinks and the text keeps its size; it is fading by then.
    /// Both animations start on the same frame and share one clock.
    fn departing_view(
        &self,
        leaving: &Departing,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = SharedString::from(format!("departing-{}", leaving.key));
        let from = if leaving.card.closing {
            CLOSING_CARD_OPACITY
        } else {
            1.
        };
        let face = self
            .card_view(&leaving.card, None, chrome, false, false, cx)
            .size_full();
        let exit = || gpui::Animation::new(model::EXIT);
        let indent = matches!(leaving.home, Home::Project(_))
            || (matches!(leaving.home, Home::Leads(_)) && leaving.card.lead.is_none());
        card_row()
            .overflow_hidden()
            .when(indent, |d| d.pl(px(WORKER_INDENT)))
            .child(div().child(face).with_animation(
                SharedString::from(format!("{key}-shrink")),
                exit(),
                move |d, t| {
                    let (shrink, _) = model::card_exit(t);
                    let scale = 1. - (1. - EXIT_SCALE) * shrink;
                    d.w(gpui::relative(scale))
                        .h(px(CARD_HEIGHT * scale))
                        .opacity(from * (1. - shrink))
                },
            ))
            .with_animation(
                SharedString::from(format!("{key}-collapse")),
                exit(),
                |d, t| {
                    let (_, collapse) = model::card_exit(t);
                    let (height, gap) = card_row_geometry(collapse);
                    d.h(px(height)).pt(px(gap))
                },
            )
    }

    /// Measure the selected row after layout, then reveal only the clipped
    /// edge. Do this on selection changes, not on every terminal redraw, so
    /// manual scrolling stays under the user's control.
    pub(crate) fn selection_reveal(&self, cx: &Context<Self>) -> impl IntoElement {
        let scroll = self.sidebar_scroll.clone();
        let selection = self.selection.clone();
        let entity = cx.entity().downgrade();
        div().absolute().inset_0().child(
            gpui::canvas(
                move |bounds, window, _cx| {
                    let viewport = scroll.bounds();
                    // Project reorder translates prepaint (and hitboxes), not
                    // layout. Reveal the final row, not its in-flight position.
                    let translation = window.element_offset().y - scroll.offset().y;
                    let delta = model::reveal_delta(
                        (bounds.top() - translation).into(),
                        (bounds.bottom() - translation).into(),
                        viewport.top().into(),
                        viewport.bottom().into(),
                    );
                    if delta == 0. {
                        return;
                    }
                    let scroll = scroll.clone();
                    let selection = selection.clone();
                    let entity = entity.clone();
                    window.on_next_frame(move |_, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            if *this.selection == selection {
                                scroll.set_offset(scroll.offset() + gpui::point(px(0.), px(delta)));
                                cx.notify();
                            }
                        });
                    });
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }
}
