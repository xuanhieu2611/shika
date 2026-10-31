//! One card: its row geometry, text, and status signal, and the sparkling
//! pixels shown while a card works or closes.
use super::*;

/// Every card's height: the 20px task line, the 6px gap, the 16px meta line,
/// and 23px of padding.
/// A closed card's row collapses from this.
pub(crate) const CARD_HEIGHT: f32 = 65.;

/// The space between cards, and between a project header and its cards.
pub(crate) const CARD_GAP: f32 = 6.;

/// A card dims to this while Close stops its terminals and removes its tree.
pub(crate) const CLOSING_CARD_OPACITY: f32 = 0.5;

/// The status signal at the right end of a card's first line: the 14px
/// working pixels, or an 8px dot centered in it.
pub(crate) const SIGNAL_SLOT: f32 = 14.;

/// A closed card shrinks toward its center to this size as it fades out.
pub(crate) const EXIT_SCALE: f32 = 0.94;

/// Project cards and a worker under its Lead sit this far in from the
/// column's content edge. A Lead card does not.
pub(crate) const WORKER_INDENT: f32 = 16.;

impl Shika {
    /// A card: task, the timer while working, and the status signal; then
    /// CLI, branch, the diff stat when ready, and key hints on the selected
    /// card. A setup stage, or "Setup failed", sits between the CLI and the
    /// branch. The status words are not painted here.
    /// `at` is the card's place in `cards`; a departing card has none and
    /// takes no clicks. Every card is [`CARD_HEIGHT`] tall.
    pub(crate) fn card_view(
        &self,
        card: &Card,
        at: Option<usize>,
        chrome: &Chrome,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = at.is_some() && self.selected_card() == at;
        let key = card_key(card.agent.view.entity_id());
        let colors = chrome.status(card.status);
        // The quiet meta steps one ink brighter on the selected card, as the
        // title does, so it stays readable on the lighter wash.
        let (quiet, quiet_separator) = if selected {
            (chrome.ink_3, chrome.ink_4)
        } else {
            (chrome.ink_4, chrome.ink_5)
        };
        let separator = || div().flex_none().text_color(quiet_separator).child("·");
        let hints: &[(&str, &str)] = if card.closing {
            &[]
        } else if card.creating && card.is_checkout() {
            &[("\u{2318}\u{21e7}W", "cancel")]
        } else if card.creating {
            &[("↵", "view setup"), ("\u{2318}\u{21e7}W", "cancel")]
        } else if card.launch_error.is_some() {
            &[("↵", "retry"), ("\u{2318}\u{21e7}W", "close")]
        } else if card.launch_preset == "terminal" {
            &[("↵", "terminal")]
        } else {
            match card.status {
                Status::Ready => &[("↵", "read")],
                Status::Working => &[("↵", "watch")],
                Status::Asking => &[("↵", "answer")],
                Status::Waiting => &[("↵", "write prompt")],
            }
        };
        let stat = card
            .diff
            .filter(|d| card.status == Status::Ready && d.files > 0)
            .map(|d| model::diff_stat_parts(d.files, d.insertions, d.deletions));
        // Setup progress and failure are facts the signal cannot say. Waiting,
        // Working, and Ready stay on the signal, the tint, the timer, and the
        // diff stat.
        let notice = if card.closing {
            Some("Closing...".to_string())
        } else if card.creating {
            Some(card.stage.clone())
        } else if card.launch_error.is_some() {
            Some("Setup failed".to_string())
        } else {
            None
        };
        // A live Lead's line two: its current step while Working, else its
        // latest reply. None keeps the CLI and "Lead".
        let lead_line = card
            .lead
            .as_ref()
            .filter(|_| notice.is_none())
            .and_then(|state| {
                let (_, current, _) = lead_steps::snapshot(&state.steps);
                let chat = self
                    .lead_chats
                    .get(&card.project)
                    .filter(|_| card.chat)
                    .map(|chat| chat.read(cx));
                lead_steps::card_line(
                    card.status == Status::Working,
                    current.as_deref(),
                    chat.and_then(|chat| chat.replaced_line()),
                    card.chat
                        .then(|| self.lead_snippets.get(&card.project))
                        .flatten()
                        .map(String::as_str),
                )
            });
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
                    .text_color(if card.named || selected {
                        chrome.ink_1
                    } else {
                        chrome.ink_2
                    })
                    // A Lead sits outside its project's group, so it names it.
                    .child(
                        match (
                            &card.lead,
                            self.projects.iter().find(|p| p.id == card.project),
                        ) {
                            (Some(_), Some(project)) => project.name.clone(),
                            _ => card.title.clone(),
                        },
                    ),
            )
            .when(card.status == Status::Working && !card.closing, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(colors.text)
                        .child(model::short_time(
                            card.activity.turn_started.unwrap_or(card.since).elapsed(),
                        )),
                )
            })
            .child(if card.closing || card.launch_preset == "terminal" {
                // Shell-only tasks have no inferred agent activity.
                div().flex_none().w(px(SIGNAL_SLOT)).into_any_element()
            } else {
                card_signal(&key, card.status, card.unseen(), chrome)
            });
        let meta = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(12.))
            .line_height(px(16.))
            // The CLI and branch sit back so the task reads first. Results
            // (the diff stat, the PR mark, the hints) stay a step brighter.
            .text_color(quiet)
            .when(lead_line.is_none(), |d| {
                d.child(cli_mark(
                    SharedString::from(format!("cli-{key}")),
                    &card.launch_preset,
                    &card.preset,
                    quiet,
                    chrome,
                ))
            })
            .when_some(lead_line.clone(), |d, line| {
                // The system font, truncating, in the card's line-two inks.
                d.child(div().flex_1().min_w_0().truncate().child(line))
            })
            .when(card.lead.is_some() && lead_line.is_none(), |d| {
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(chrome.ink_2)
                        .child("Lead"),
                )
            })
            .when_some(notice, |d, notice| {
                d.child(separator()).child(
                    div()
                        .min_w_0()
                        .truncate()
                        // A long setup stage gives way. "Setup failed" stays whole.
                        .when(!card.creating, |stage| stage.flex_none())
                        .text_color(if card.closing {
                            chrome.ink_2
                        } else {
                            colors.text
                        })
                        .font_weight(match card.status {
                            _ if card.closing => FontWeight::MEDIUM,
                            Status::Ready | Status::Asking => FontWeight::SEMIBOLD,
                            Status::Working => FontWeight::MEDIUM,
                            Status::Waiting => FontWeight::NORMAL,
                        })
                        .child(notice),
                )
            })
            .when(card.is_checkout(), |d| {
                let (bg, fg) = (chrome.toast_bg, chrome.toast_fg);
                d.child(separator()).child(
                    div()
                        .id(SharedString::from(format!("checkout-{key}")))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(
                            gpui::svg()
                                .data(CHECKOUT_ICON)
                                .size(px(12.))
                                .text_color(quiet),
                        )
                        .tooltip(move |_, cx| {
                            cx.new(|_| KeyTip {
                                bg,
                                fg,
                                text: "Runs in your checkout, no worktree".into(),
                            })
                            .into()
                        }),
                )
            })
            .when_some(
                if card.is_checkout() {
                    self.checkout_branch(&card.project)
                } else {
                    card.session
                        .as_ref()
                        .filter(|_| card.lead.is_none())
                        .and_then(model::card_branch)
                },
                |d, branch| {
                    // The branch truncates before the diff stat. A draft
                    // has no name yet, so line two shows only the CLI.
                    d.when(!card.is_checkout(), |d| d.child(separator())).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .child(branch.to_string()),
                    )
                },
            )
            .when_some(stat, |d, stat| {
                // Text on the card, not a control. A click here is the
                // card's click: select it and focus its terminal. The
                // panel opens from the shortcut, the toggle, or the menu.
                // The counts take the Changes panel's diff colors; the file
                // count stays ink.
                let (files, added, removed) = stat;
                d.child(separator()).child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(4.))
                        .text_color(chrome.ink_3)
                        .child(files)
                        .child(div().text_color(chrome.diff_added.text).child(added))
                        .child(div().text_color(chrome.diff_removed.text).child(removed)),
                )
            })
            .when_some(card.pr.as_ref(), |d, pr| {
                d.child(separator()).child(pr_mark(&key, pr, chrome, cx))
            })
            .when(lead_line.is_none(), |d| d.child(div().flex_1()))
            .when(selected && cards_focused, |d| {
                d.children(
                    hints
                        .iter()
                        .map(|(key, label)| row_hint(key, label, chrome)),
                )
            });
        let base = div()
            .id(SharedString::from(format!("card-{key}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(10.))
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .when_some(at, |d, i| {
                d.capture_any_mouse_down(cx.listener(
                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Right
                            || (event.button == MouseButton::Left && event.modifiers.control)
                        {
                            cx.stop_propagation();
                            this.open_card_menu(i, event.position, window, cx);
                        }
                    },
                ))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.busy || this.overlay.is_some() {
                        return;
                    }
                    this.selection.set(Some(Selection::Card(i)));
                    if this.busy {
                        // The old terminal is hidden now; the cards hold the keys.
                        window.focus(&this.focus, cx);
                    } else {
                        this.focus_shown(window, cx);
                    }
                }))
            });
        // The selected card is a soft wash over the column, like a hover,
        // so glass shows through it. The key hints show when the cards have
        // focus. Every other card is bare text.
        let base = if selected {
            base.bg(chrome.card_selected)
        } else {
            base.overflow_hidden()
        };
        // A Lead's antler tile spans both lines on the left.
        let base = if card.lead.is_some() {
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
        } else {
            base.child(task).child(meta)
        };
        base.when(selected && reveal_selection, |d| {
            d.child(self.selection_reveal(cx))
        })
    }
}

/// The card and its leading gap occupy one nonnegative layout slot. At the
/// end of the exit there is nothing left to remove, including on an otherwise
/// empty project. The face's shrink is independent of this slot's collapse.
pub(crate) fn card_row_geometry(collapse: f32) -> (f32, f32) {
    let remaining = 1. - collapse.clamp(0., 1.);
    ((CARD_HEIGHT + CARD_GAP) * remaining, CARD_GAP * remaining)
}

pub(crate) fn card_row() -> gpui::Div {
    div()
        .w_full()
        .h(px(CARD_HEIGHT + CARD_GAP))
        .pt(px(CARD_GAP))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// A vector index belongs to a position, not a card. Keep the face and its
/// status animation keyed to the same view when earlier cards leave.
fn card_key(view: gpui::EntityId) -> SharedString {
    view.to_string().into()
}

/// The right end of a card's first line, one 14px slot so the signals line
/// up down the column: working pixels, a ready dot until the result is seen,
/// a grey waiting dot, and nothing once a Ready result has been seen.
/// The antler mark on a raised tile, left of both lines of a Lead's card.
pub(crate) fn antler_tile(selected: bool, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .mt(px(2.))
        .size(px(30.))
        .rounded(px(8.))
        .bg(if selected {
            chrome.raised_hover
        } else {
            chrome.raised
        })
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::svg()
                .data(lead_chat::ANTLERS_ICON)
                .size(px(18.))
                .text_color(chrome.ink_2),
        )
}

fn card_signal(card: &str, status: Status, unseen: bool, chrome: &Chrome) -> gpui::AnyElement {
    let slot = div().flex_none().flex().justify_center().w(px(SIGNAL_SLOT));
    let color = chrome.status(status).dot;
    match status {
        Status::Working => slot.child(working_pixels(card, color)),
        Status::Ready if !unseen => slot,
        _ => slot.child(div().size(px(8.)).rounded_full().bg(color)),
    }
    .into_any_element()
}

/// A working card's indicator: a 5x5 field of 2px pixels, 14px square, that
/// twinkle about eight at a time. Every size and offset is a whole point, so
/// the pixels stay crisp and the field sits 3px from the top of the 20px
/// line, level with the 8px dots. GPUI skips the loop when macOS asks for
/// reduced motion, leaving the first frame, a scatter of lit pixels.
fn working_pixels(card: &str, color: gpui::Rgba) -> gpui::AnyElement {
    pixel_sparkle(format!("working-{card}"), color, 2., 1.)
}

/// One sparkle cycle. Each pixel lights once per cycle.
const SPARKLE_CYCLE: Duration = Duration::from_millis(2000);

/// The share of a cycle one pixel stays lit, rising and falling.
const SPARKLE_TWINKLE: f32 = 0.36;

/// When each pixel of the 5x5 field lights, row by row: pixel `i` starts at
/// `SPARKLE_SLOT[i] / 25` of the cycle. Pixels that start within four slots
/// of each other are never neighbors, so the field reads as scattered light
/// rather than a sweep.
pub(crate) const SPARKLE_SLOT: [u8; 25] = [
    2, 14, 9, 20, 8, //
    7, 19, 4, 15, 3, //
    1, 13, 24, 10, 22, //
    21, 6, 18, 5, 17, //
    16, 11, 23, 12, 0,
];

/// How lit a pixel is at phase `t` of the cycle: off, or a third, two
/// thirds, or full, in one rise and fall starting at its slot.
pub(crate) fn sparkle_level(t: f32, slot: u8) -> f32 {
    let local = (t - f32::from(slot) / 25.).rem_euclid(1.);
    if local >= SPARKLE_TWINKLE {
        return 0.;
    }
    ((std::f32::consts::PI * local / SPARKLE_TWINKLE).sin() * 3.).round() / 3.
}

/// A 5x5 field of `size` pixels, `gap` apart, that twinkle in `color`. A lit
/// pixel blooms: a soft glow of its own color, as strong as the pixel is lit.
pub(crate) fn pixel_sparkle(
    id: String,
    color: gpui::Rgba,
    size: f32,
    gap: f32,
) -> gpui::AnyElement {
    let side = 5. * size + 4. * gap;
    div()
        .flex_none()
        .relative()
        .size(px(side))
        .with_animation(
            SharedString::from(id),
            gpui::Animation::new(SPARKLE_CYCLE)
                .repeat_synced()
                .with_max_fps(30.),
            move |d, t| {
                d.children(SPARKLE_SLOT.iter().enumerate().filter_map(|(i, &slot)| {
                    let level = sparkle_level(t, slot);
                    (level > 0.).then(|| {
                        div()
                            .absolute()
                            .left(px((i % 5) as f32 * (size + gap)))
                            .top(px((i / 5) as f32 * (size + gap)))
                            .size(px(size))
                            .bg(with_alpha(color, color.a * level))
                            .shadow(vec![
                                BoxShadow::new(
                                    px(0.),
                                    px(0.),
                                    with_alpha(color, color.a * level * 0.5).into(),
                                )
                                .blur_radius(px(size * 1.5)),
                            ])
                    })
                }))
            },
        )
        .into_any_element()
}

/// The PR number and its checks: a ring while they run, a check when they
/// pass, a cross in the failed color when one fails, and the number alone
/// when the repository reports none. A click opens the PR.
fn pr_mark(
    card: &str,
    pr: &checks::PrWatch,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> impl IntoElement {
    let mark = pr.mark();
    let color = if mark == checks::Mark::Failed {
        chrome.failed.text
    } else {
        chrome.ink_3
    };
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let tip = model::checks_tip(mark);
    let url = pr.url.clone();
    div()
        .id(SharedString::from(format!("pr-{card}")))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_color(color)
        .when(mark == checks::Mark::Failed, |d| {
            d.font_weight(FontWeight::MEDIUM)
        })
        .child(
            div()
                .font_family(MONO)
                .text_size(px(11.5))
                .child(format!("#{}", pr.number)),
        )
        .map(|d| match mark {
            checks::Mark::Pending => d.child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .border_1()
                    .border_color(chrome.ink_4),
            ),
            checks::Mark::Passed => d.child("\u{2713}"),
            checks::Mark::Failed => d.child("\u{2717}"),
            checks::Mark::None => d,
        })
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: tip.into(),
            })
            .into()
        })
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.open_url(&url);
        }))
}

/// The CLI on a card's second line: its mark at 14px in the line's ink, with
/// the name as its tooltip. A CLI without a mark keeps its name.
pub(crate) fn cli_mark(
    id: SharedString,
    preset: &str,
    name: &str,
    color: Rgba,
    chrome: &Chrome,
) -> gpui::AnyElement {
    let Some(mark) = agent_marks::for_preset(preset) else {
        return div().flex_none().child(name.to_string()).into_any_element();
    };
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let name = SharedString::from(name.to_string());
    div()
        .id(id)
        .flex_none()
        .child(gpui::svg().data(mark).size(px(14.)).text_color(color))
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: name.clone(),
            })
            .into()
        })
        .into_any_element()
}
