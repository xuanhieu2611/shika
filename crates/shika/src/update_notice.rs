//! The in-app software update notice: answering it and painting its card.
use super::*;

impl Shika {
    /// Sparkle posts on the main thread. The corner card reads the queue here,
    /// and again immediately after a click.
    pub(super) fn drain_update_notice(&mut self, cx: &App) -> bool {
        if !cx.has_global::<updates::Updater>() {
            return false;
        }
        let mut changed = false;
        for event in cx.global::<updates::Updater>().poll() {
            changed = true;
            match event {
                updates::UpdateEvent::Show(card) => self.update_notice = Some(card),
                updates::UpdateEvent::Clear => self.update_notice = None,
                updates::UpdateEvent::Toast(text) => self.message(text),
            }
        }
        changed
    }

    fn answer_update(&mut self, action: updates::CardAction, cx: &mut Context<Self>) {
        if self.preview_update {
            self.advance_update_preview(action);
            cx.notify();
            return;
        }
        let open = match (&action, &self.update_notice) {
            (
                updates::CardAction::Primary,
                Some(updates::UpdateCard::Available {
                    offer: Some(updates::Offer::Open(url)),
                    ..
                }),
            ) => Some(url.clone()),
            _ => None,
        };
        if let Some(url) = open {
            cx.open_url(&url);
        }
        if cx.has_global::<updates::Updater>() {
            cx.global::<updates::Updater>().respond(action);
        }
        self.drain_update_notice(cx);
        cx.notify();
    }

    /// The sample card. Download shows the restart card. Every other button closes it.
    fn advance_update_preview(&mut self, action: updates::CardAction) {
        self.update_notice = match (&self.update_notice, action) {
            (
                Some(updates::UpdateCard::Available {
                    offer: Some(updates::Offer::Download),
                    version,
                    ..
                }),
                updates::CardAction::Primary,
            ) => Some(updates::UpdateCard::Ready {
                version: version.clone(),
            }),
            (Some(updates::UpdateCard::Available { .. }), updates::CardAction::Secondary)
            | (Some(updates::UpdateCard::Ready { .. }), _) => None,
            (other, _) => other.clone(),
        };
    }

    /// The update card. It does not take focus, so typing and Escape stay in
    /// the terminal. 320 is the column's minimum, with the card's own padding.
    pub(super) fn update_notice_panel(
        &self,
        card: &updates::UpdateCard,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notice = card.view();
        let max_h = (window.viewport_size().height - px(44.)).max(px(120.));
        let mut panel = panel_shell("update-notice", 320., max_h, chrome)
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .pt(px(12.))
            .px(px(14.))
            .pb(px(11.))
            .gap(px(6.))
            .child(dialog_title(div()).child(notice.title))
            .child(dialog_text(chrome).child(notice.body));
        if notice.secondary.is_some() || notice.primary.is_some() {
            panel = panel.child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .flex_wrap()
                    .when_some(notice.secondary, |row, label| {
                        row.child(
                            notice_secondary("update-secondary", label, chrome).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.answer_update(updates::CardAction::Secondary, cx)
                                }),
                            ),
                        )
                    })
                    .when_some(notice.primary, |row, label| {
                        row.child(notice_primary("update-primary", label, chrome).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.answer_update(updates::CardAction::Primary, cx)
                            }),
                        ))
                    }),
            );
        }
        panel
    }
}

pub(super) fn sample_update_notice() -> updates::UpdateCard {
    updates::UpdateCard::Available {
        version: "0.6.0".into(),
        body: updates::available_body("", false, Some(&updates::Offer::Download)),
        offer: Some(updates::Offer::Download),
    }
}
