//! Painting and cancelling the current overlay (dialogs, pickers, menus).
use super::*;

impl Shika {
    pub(super) fn cancel_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Quit(_))) {
            self.cancel_quit(window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::LeadQuestion(_))) {
            self.answer_chat_question(true, window, cx);
            return;
        }
        if self.busy
            && matches!(
                self.overlay,
                Some(Overlay::Publish(_) | Overlay::WorktreeSetup(_) | Overlay::SwitchAgent(_))
            )
        {
            return;
        }
        if matches!(self.overlay, Some(Overlay::WorktreeSetup(_))) {
            self.finish_worktree_setup(window, cx);
            return;
        }
        if matches!(self.overlay, Some(Overlay::Adjust(_))) {
            self.close_adjust(false, window, cx);
            return;
        }
        self.commit_setting_edit(window, cx);
        self.overlay = None;
        // The shown tab stays. A Lead that asked for Close still takes
        // selection and focus back in settle_lead_dialog.
        self.restore_overlay_focus(window, cx);
        self.settle_lead_dialog(window, cx);
        cx.notify();
    }

    /// Pointer menus and appearance previews stay undimmed. Other modal
    /// overlays reuse the dialog shadow as a full-window scrim.
    pub(super) fn overlay_view(
        &self,
        overlay: &Overlay,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Overlay::CardMenu(menu) = overlay {
            return self
                .card_menu_view(menu, chrome, window, cx)
                .into_any_element();
        }
        if let Overlay::RestingMenu(menu) = overlay {
            return self
                .resting_menu_view(menu, chrome, window, cx)
                .into_any_element();
        }
        let height = window.viewport_size().height;
        let picker = matches!(overlay, Overlay::Picker(_));
        let top = height * if picker { 0.18 } else { 0.20 };
        let max_h = (height - top - px(24.)).max(px(120.));
        let panel = match overlay {
            Overlay::CardMenu(_) | Overlay::RestingMenu(_) => {
                unreachable!("card menus render at the pointer")
            }
            Overlay::LeadQuestion(question) => self.chat_question_view(question, chrome, max_h, cx),
            Overlay::LeadClose(dialog) => self.lead_close_view(dialog, chrome, max_h, cx),
            Overlay::WorktreeSetup(editor) => {
                self.worktree_setup_view(editor, chrome, max_h, window, cx)
            }
            Overlay::Adjust(adjust) => self.adjust_view(adjust, max_h, chrome, window, cx),
            Overlay::Rename(rename) => self.rename_view(rename, chrome, max_h, cx),
            Overlay::SwitchAgent(_) => self.switch_dialog(chrome, home, max_h, cx),
            Overlay::Publish(publish) => self.publish_view(publish, chrome, max_h, cx),
            Overlay::Picker(dialog) => self.picker_view(dialog, chrome, home, max_h, cx),
            Overlay::Preparation(dialog) => self.preparation_view(dialog, chrome, max_h, cx),
            Overlay::CheckoutClose(dialog) => self.checkout_close_view(dialog, chrome, max_h, cx),
            Overlay::Close(dialog) => self.close_view(dialog, chrome, max_h, cx),
            Overlay::SwitchedClose(dialog) => self.switched_close_view(dialog, chrome, max_h, cx),
            Overlay::Quit(_) => self.quit_dialog(chrome, max_h, cx),
            Overlay::Recover(recover) => self.recovery_dialog(recover, chrome, max_h, cx),
            Overlay::Leftovers => self.leftovers_view(chrome, home, max_h, cx),
            Overlay::RemoveProject(id) => self.remove_project_view(id, chrome, max_h, cx),
            Overlay::RemoveLeftover(i) => self.remove_leftover_view(*i, chrome, home, max_h, cx),
            Overlay::Base(base) => self.base_view(base, chrome, max_h, cx),
            Overlay::Settings(dialog) => self.settings_panel(dialog, max_h, chrome, home, cx),
        };
        div()
            .absolute()
            .occlude()
            .inset_0()
            .when(overlay.dims_background(), |d| d.bg(chrome.dialog_shadow))
            .flex()
            .items_start()
            .justify_center()
            .pt(top)
            .child(panel)
            .into_any_element()
    }
}

pub(crate) enum Overlay {
    LeadQuestion(lead_chat::QuestionDialog),
    WorktreeSetup(Box<worktree_setup::Editor>),
    /// Frame the background image, opened from Settings, which it returns to.
    Adjust(Box<background_view::Adjust>),
    CardMenu(CardMenu),
    Rename(rename::RenameDialog),
    Publish(PublishDialog),
    SwitchAgent(switch_agent::SwitchDialog),
    Picker(launch::PickerDialog),
    CheckoutClose(close::CheckoutCloseDialog),
    Close(close::CloseDialog),
    RestingMenu(RestingMenu),
    /// Stop a live Lead, or forget one.
    LeadClose(lead::LeadCloseDialog),
    SwitchedClose(close::SwitchedCloseDialog),
    Preparation(launch::PreparationDialog),
    Leftovers,
    Recover(recovery::RecoverDialog),
    Quit(recovery::QuitDialog),
    RemoveLeftover(usize),
    RemoveProject(String),
    Settings(SettingsDialog),
    Base(BaseDialog),
}

impl Overlay {
    /// Appearance previews and pointer menus leave the underlying app unaltered.
    pub(crate) fn dims_background(&self) -> bool {
        !matches!(
            self,
            Self::Settings(_) | Self::Adjust(_) | Self::CardMenu(_) | Self::RestingMenu(_)
        )
    }

    /// How a refusal names this overlay to a Lead.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::LeadQuestion(_) => "the Lead question dialog",
            Self::WorktreeSetup(_) => "the Worktree setup dialog",
            Self::Adjust(_) => "the Adjust background dialog",
            Self::CardMenu(_) => "a card menu",
            Self::Rename(_) => "the Rename task dialog",
            Self::Publish(_) => "the Create PR dialog",
            Self::SwitchAgent(_) => "the Switch agent dialog",
            Self::Picker(_) => "the New picker",
            Self::Close(_) | Self::SwitchedClose(_) | Self::CheckoutClose(_) => {
                "the Close task dialog"
            }
            Self::RestingMenu(_) => "a card menu",
            Self::LeadClose(_) => "the Lead's Stop dialog",
            Self::Preparation(_) => "the setup approval dialog",
            Self::Leftovers | Self::RemoveLeftover(_) | Self::Recover(_) => "the leftovers list",
            Self::Quit(_) => "the Quit dialog",
            Self::RemoveProject(_) => "the Remove project dialog",
            Self::Settings(_) => "Settings",
            Self::Base(_) => "the Base branch dialog",
        }
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
