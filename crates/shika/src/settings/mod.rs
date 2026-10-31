//! The Settings dialog: sections, row layout, opening it, and applying
//! changes, including the Agents toggles and the Usage range and filter.
use super::*;
use crate::usage_dashboard::UsageView;

mod keyboard;
mod view;

/// Settings sections, top to bottom in the list on the left.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    Appearance,
    Keyboard,
    Agents,
    Projects,
    Usage,
}

impl SettingsSection {
    pub(crate) const ALL: [SettingsSection; 5] = [
        SettingsSection::Appearance,
        SettingsSection::Keyboard,
        SettingsSection::Agents,
        SettingsSection::Projects,
        SettingsSection::Usage,
    ];

    /// One section up (`-1`) or down (`1`), wrapping.
    pub(crate) fn step(self, delta: i64) -> Self {
        let at = Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0) as i64;
        let len = Self::ALL.len() as i64;
        Self::ALL[(at + delta).rem_euclid(len) as usize]
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Keyboard => "Keyboard",
            Self::Agents => "Agents",
            Self::Projects => "Projects",
            Self::Usage => "Usage",
        }
    }
}

/// Settings rows, top to bottom: the theme mode, the light and dark themes,
/// the background image and its effect (hidden without an image), opacity,
/// blur, translucency, font size, the branch prefix, then notification sound.
const MODE_ROW: usize = 0;

const LIGHT_ROW: usize = 1;

const DARK_ROW: usize = 2;

pub(crate) const IMAGE_ROW: usize = 3;

pub(crate) const EFFECT_ROW: usize = 4;

pub(crate) const OPACITY_ROW: usize = 5;

pub(crate) const BLUR_ROW: usize = 6;

const APPLY_ROW: usize = 7;

pub(crate) const FONT_ROW: usize = 8;

pub(crate) const PREFIX_ROW: usize = 9;

const SOUND_ROW: usize = 10;

pub(crate) const SETTING_ROWS: usize = 11;

/// Wide enough for the longest catalog name, such as "Catppuccin Macchiato".
const THEME_FIELD_WIDTH: f32 = 168.;

/// Tall enough for the appearance rows and the usage chart. A short window
/// uses less, and the rows scroll with a scrollbar.
const SETTINGS_HEIGHT: f32 = 580.;

const SETTINGS_NAV_WIDTH: f32 = 148.;

/// One size for every section, so switching never resizes the dialog: the
/// usage year calendar beside the section list, inside the rows' 10px inset,
/// the 12px gap and the dialog's padding.
const SETTINGS_WIDTH: f32 = usage_dashboard::WIDTH + 20. + SETTINGS_NAV_WIDTH + 12. + 40.;

// It fits the app's 960px minimum window with the dialog's 40px margin.
const _: () = assert!(SETTINGS_WIDTH <= 960. - 40.);

/// The open Settings dialog.
pub(crate) struct SettingsDialog {
    /// Appearance, Keyboard, Agents, Projects, or Usage.
    section: SettingsSection,
    /// The selected row in that section: an appearance `*_ROW`, a keyboard
    /// command, or a preset index.
    row: usize,
    /// Digits typed into the selected number, or the prefix being typed, not
    /// yet applied.
    edit: Option<String>,
    /// The saved snapshot, read the first time Usage shows and kept until
    /// Settings closes.
    usage: Option<Box<UsageView>>,
    /// Settings, Keyboard, is waiting for the next Command shortcut.
    key_recording: bool,
    /// Why the last recorded shortcut was refused. Shown in the Keyboard footer.
    key_note: Option<String>,
}

impl SettingsDialog {
    /// Settings on `row` of `section`, nothing typed or recording.
    pub(crate) fn new(section: SettingsSection, row: usize) -> Self {
        Self {
            section,
            row,
            edit: None,
            usage: None,
            key_recording: false,
            key_note: None,
        }
    }

    pub(crate) fn section(&self) -> SettingsSection {
        self.section
    }

    pub(crate) fn row(&self) -> usize {
        self.row
    }

    pub(crate) fn usage(&self) -> Option<&UsageView> {
        self.usage.as_deref()
    }
}

/// Settings state that outlives the dialog: the row list keeps its scroll
/// offset across closing and reopening Settings and the dialogs it opens.
#[derive(Default)]
pub(crate) struct SettingsUi {
    /// The Settings row list, so `j` / `k` keep the selected row on screen
    /// when a short window makes that list scroll.
    scroll: gpui::ScrollHandle,
    /// Its scrollbar's thumb while the pointer drags it.
    grab: scrollbar::Grab,
}

impl SettingsUi {
    /// Bring a row of the list on screen at the next layout.
    pub(crate) fn scroll_to(&self, row: usize) {
        self.scroll.scroll_to_item(row);
    }
}

impl Shika {
    /// View > Usage and its shortcut (Cmd+Shift+U until Settings changes
    /// it) open Settings on Usage, or switch an open Settings to it. Settings
    /// already on Usage closes and restores the focus that opened it; any
    /// other overlay keeps the shortcut inert.
    pub(crate) fn toggle_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.key_recording() {
            return;
        }
        match &self.overlay {
            Some(Overlay::Settings(SettingsDialog {
                section: SettingsSection::Usage,
                ..
            })) => self.cancel_overlay(window, cx),
            Some(Overlay::Settings(SettingsDialog { edit: None, .. })) => {
                self.select_section(SettingsSection::Usage, window, cx)
            }
            Some(_) => {}
            None => {
                self.overlay_return_focus = window.focused(cx);
                self.show_settings(SettingsSection::Usage, window, cx);
            }
        }
    }

    /// Reads the saved store once per Settings; CLI files stay on the
    /// collector.
    fn load_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Settings(SettingsDialog {
            section: SettingsSection::Usage,
            usage: usage @ None,
            ..
        })) = &mut self.overlay
        else {
            return;
        };
        let view = UsageView::new();
        let request = view.request.clone();
        *usage = Some(Box::new(view));
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    core.recover_token_usage();
                    core.token_usage()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let chrome = this.chrome(window);
                if let Some(view) = this.usage_view_mut()
                    && Arc::ptr_eq(&view.request, &request)
                {
                    view.load(report, &chrome, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn usage_view_mut(&mut self) -> Option<&mut UsageView> {
        match &mut self.overlay {
            Some(Overlay::Settings(SettingsDialog {
                usage: Some(view), ..
            })) => Some(view),
            _ => None,
        }
    }

    fn set_usage_filter(&mut self, filter: UsageFilter, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings(SettingsDialog {
            usage: Some(view), ..
        })) = &mut self.overlay
        {
            view.set_filter(filter, cx);
        }
        cx.notify();
    }

    fn set_usage_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings(SettingsDialog {
            usage: Some(view), ..
        })) = &mut self.overlay
        {
            view.set_range(range, cx);
        }
        cx.notify();
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.overlay.is_some() {
            return;
        }
        self.overlay_return_focus = window.focused(cx);
        self.show_settings(SettingsSection::Appearance, window, cx);
    }

    /// Replace whatever overlay is up. The picker uses this to open Agents
    /// without dropping the focus it already saved.
    pub(crate) fn show_settings(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.overlay = Some(Overlay::Settings(SettingsDialog::new(section, 0)));
        self.load_usage(window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// One step left (`-1`) or right (`1`) on a settings row. Applies and
    /// saves at once so the window is the preview.
    pub(crate) fn step_setting(
        &mut self,
        row: usize,
        delta: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_setting_edit(window, cx);
        if row == MODE_ROW {
            self.set_theme_mode(self.theme.mode.step(delta), window, cx);
            return;
        }
        if row == LIGHT_ROW || row == DARK_ROW {
            let dark = row == DARK_ROW;
            let from = appearance::resolve_theme(&self.theme, dark);
            self.set_theme(dark, appearance::step_theme(from, delta), window, cx);
            return;
        }
        if row == FONT_ROW {
            self.set_font_size(self.font_size.step(delta), cx);
            return;
        }
        if row == SOUND_ROW {
            self.set_notification_sound(delta > 0, cx);
            return;
        }
        let mut next = self.appearance;
        match row {
            OPACITY_ROW => next = next.with_opacity(i64::from(next.opacity) + delta * 5),
            BLUR_ROW => next = next.with_blur(i64::from(next.blur) + delta * 5),
            APPLY_ROW => {
                next.translucency = if delta < 0 {
                    Translucency::Sidebar
                } else {
                    Translucency::SidebarAndTerminal
                }
            }
            _ => return,
        }
        self.set_appearance(next, window, cx);
    }

    /// Select a settings row and start typing into its number. An empty
    /// field shows the current value until a digit arrives.
    pub(crate) fn edit_setting(
        &mut self,
        row: usize,
        digits: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_setting_edit(window, cx);
        if let Some(Overlay::Settings(SettingsDialog { row: at, edit, .. })) = &mut self.overlay {
            *at = row;
            *edit = Some(digits.to_string());
        }
        cx.notify();
    }

    /// Apply typed digits, pulled into range, or the typed prefix, made
    /// safe for git. Nothing typed keeps a number.
    pub(crate) fn commit_setting_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Settings(SettingsDialog {
            section, row, edit, ..
        })) = &mut self.overlay
        else {
            return;
        };
        if *section != SettingsSection::Appearance {
            *edit = None;
            return;
        }
        let row = *row;
        if row == PREFIX_ROW {
            if let Some(text) = edit.take() {
                let prefix = shika_core::normalize_branch_prefix(&text);
                if prefix != self.branch_prefix {
                    self.branch_prefix = prefix;
                    self.save_settings();
                }
                cx.notify();
            }
            return;
        }
        if row == FONT_ROW {
            if let Some(text) = edit.take()
                && let Some(size) = FontSize::from_text(&text)
            {
                self.set_font_size(size, cx);
            }
            return;
        }
        let Some(value) = edit.take().and_then(|text| text.parse::<i64>().ok()) else {
            return;
        };
        let next = match row {
            OPACITY_ROW => self.appearance.with_opacity(value),
            BLUR_ROW => self.appearance.with_blur(value),
            _ => return,
        };
        self.set_appearance(next, window, cx);
    }

    fn set_appearance(&mut self, next: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.appearance {
            return;
        }
        self.appearance = next;
        self.reduce_transparency = appearance::reduce_transparency();
        appearance::apply(&next, self.background.is_some(), window);
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }

    /// System, Light, or Dark. A forced mode also sets the app's AppKit
    /// appearance, so the traffic lights and menus match the paint.
    fn set_theme_mode(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.theme.mode {
            return;
        }
        self.theme.mode = mode;
        appearance::apply_mode(mode);
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }

    /// The light or dark pick. It shows at once when that side is painting.
    fn set_theme(
        &mut self,
        dark: bool,
        theme: &'static Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = if dark {
            &mut self.theme.dark
        } else {
            &mut self.theme.light
        };
        if *id == theme.id {
            return;
        }
        *id = theme.id.to_string();
        self.push_terminal_theme(window, cx);
        self.save_settings();
        cx.notify();
    }

    fn set_font_size(&mut self, next: FontSize, cx: &mut Context<Self>) {
        if next == self.font_size {
            return;
        }
        self.font_size = next;
        let size = px(next.points());
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shells.iter()) {
                pane.view.update(cx, |view, cx| {
                    let mut config = view.config().clone();
                    config.font_size = size;
                    view.set_config(config, cx);
                });
            }
        }
        self.save_settings();
        cx.notify();
    }

    fn set_notification_sound(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.notification_sound {
            return;
        }
        self.notification_sound = on;
        self.save_settings();
        cx.notify();
    }

    /// Move the Settings selection, applying any typed value first, and keep
    /// the row on screen. The rows are the children of the scrolling list.
    pub(crate) fn select_setting(
        &mut self,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_setting_edit(window, cx);
        self.set_key_recording(false);
        self.set_key_note(None);
        if let Some(Overlay::Settings(SettingsDialog { row, .. })) = &mut self.overlay {
            *row = to;
            self.settings_ui.scroll.scroll_to_item(to);
        }
        cx.notify();
    }

    /// Switch section. A typed value is applied first. The same section keeps
    /// its row.
    pub(crate) fn select_section(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.commit_setting_edit(window, cx);
        self.set_key_recording(false);
        self.set_key_note(None);
        if let Some(Overlay::Settings(SettingsDialog {
            section: at,
            row,
            edit,
            ..
        })) = &mut self.overlay
            && *at != section
        {
            *at = section;
            *row = 0;
            *edit = None;
            self.settings_ui.scroll.scroll_to_item(0);
        }
        self.load_usage(window, cx);
        cx.notify();
    }

    fn set_agent_enabled(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if self.agents.enabled(id) == on {
            return;
        }
        self.agents.set_enabled(id, on);
        self.save_settings();
        cx.notify();
    }

    fn set_agent_row(&mut self, row: usize, on: bool, cx: &mut Context<Self>) {
        let Some(id) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.get(row))
            .map(|preset| preset.id.clone())
        else {
            return;
        };
        self.set_agent_enabled(&id, on, cx);
    }

    pub(crate) fn on_agents_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.presets.len())
            .unwrap_or(0);
        match key {
            "j" | "down" | "tab" if count > 0 => {
                self.select_setting((row + 1) % count, window, cx);
            }
            "k" | "up" if count > 0 => {
                self.select_setting((row + count - 1) % count, window, cx);
            }
            "h" | "left" => self.set_agent_row(row, false, cx),
            "l" | "right" => self.set_agent_row(row, true, cx),
            "enter" | "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }

    /// The provider row steps like a segment, stopping at either end; the
    /// range has a letter each.
    pub(crate) fn on_usage_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let filter = self.usage_view_mut().map(|view| view.filter);
        match key {
            "h" | "left" | "l" | "right" => {
                if let Some(filter) = filter {
                    let step = if matches!(key, "h" | "left") { -1 } else { 1 };
                    self.set_usage_filter(filter.step(step), cx);
                }
            }
            "enter" | "escape" => self.cancel_overlay(window, cx),
            _ => {
                if let Some(range) = UsageRange::from_key(key) {
                    self.set_usage_range(range, cx);
                }
            }
        }
    }

    /// Typing into the branch prefix, before the modifier check: the field
    /// takes every key. False when the prefix is not being typed.
    pub(crate) fn settings_prefix_key(
        &mut self,
        stroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Overlay::Settings(SettingsDialog {
            section: SettingsSection::Appearance,
            row: PREFIX_ROW,
            edit: Some(text),
            ..
        })) = &mut self.overlay
        else {
            return false;
        };
        match stroke.key.as_str() {
            "enter" => self.commit_setting_edit(window, cx),
            "escape" => {
                if let Some(Overlay::Settings(SettingsDialog { edit, .. })) = &mut self.overlay {
                    *edit = None;
                }
            }
            "backspace" => {
                text.pop();
            }
            _ => {
                let typed = stroke.key_char.as_deref().filter(|_| {
                    !stroke.modifiers.platform && !stroke.modifiers.control && !stroke.modifiers.alt
                });
                if let Some(ch) = typed.and_then(|t| t.chars().next())
                    && (ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
                    && text.len() < 40
                {
                    text.push(ch);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// A plain key in Settings, past the grace window: `[` `]` switch
    /// section, and every section but Appearance's ordinary rows takes its
    /// own keys. True when the key was handled.
    pub(crate) fn settings_nav_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let settings_nav = match &self.overlay {
            Some(Overlay::Settings(SettingsDialog {
                section, row, edit, ..
            })) => Some((*section, *row, edit.is_some())),
            _ => None,
        };
        if let Some((section, at, editing)) = settings_nav {
            if !editing && matches!(key, "[" | "]") {
                let delta = if key == "]" { 1 } else { -1 };
                self.select_section(section.step(delta), window, cx);
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            if !editing && section == SettingsSection::Agents {
                self.on_agents_key(at, key, window, cx);
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            if !editing && section == SettingsSection::Projects {
                self.on_projects_key(at, key, window, cx);
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            if !editing && section == SettingsSection::Keyboard {
                self.on_keyboard_key(at, key, window, cx);
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            if section == SettingsSection::Usage {
                self.on_usage_key(key, window, cx);
                cx.stop_propagation();
                cx.notify();
                return true;
            }
            if !editing
                && section == SettingsSection::Appearance
                && self.on_background_key(at, key, window, cx)
            {
                cx.stop_propagation();
                cx.notify();
                return true;
            }
        }
        false
    }

    /// The Appearance rows: typed numbers, `h` / `l` steps, and moving
    /// between rows. The caller stops propagation and repaints.
    pub(crate) fn settings_row_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Settings(SettingsDialog { row, edit, .. })) = &mut self.overlay else {
            return;
        };
        let at = *row;
        let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
        let number_row = matches!(at, OPACITY_ROW | BLUR_ROW | FONT_ROW);
        let max_len = if at == FONT_ROW { 4 } else { 3 };
        match (edit.as_mut(), key) {
            (Some(text), _) if digit || (at == FONT_ROW && key == ".") => {
                let accept = if key == "." {
                    !text.is_empty() && !text.contains('.')
                } else {
                    true
                };
                if accept && text.len() < max_len {
                    text.push_str(key);
                }
            }
            (Some(text), "backspace") => {
                text.pop();
            }
            (Some(_), "escape") => *edit = None,
            (Some(_), "enter") => self.commit_setting_edit(window, cx),
            (Some(_), "h" | "l" | "left" | "right") => {}
            (None, _) if digit && number_row => self.edit_setting(at, key, window, cx),
            (None, "backspace") if number_row => self.edit_setting(at, "", window, cx),
            (None, "enter") if at == PREFIX_ROW => {
                let prefix = self.branch_prefix.clone();
                self.edit_setting(at, &prefix, window, cx)
            }
            (None, "h" | "left") => self.step_setting(at, -1, window, cx),
            (None, "l" | "right") => self.step_setting(at, 1, window, cx),
            (None, "enter" | "escape") => self.cancel_overlay(window, cx),
            (_, "j" | "down" | "tab") => {
                let next = self.next_appearance_row(at, 1);
                self.select_setting(next, window, cx)
            }
            (_, "k" | "up") => {
                let next = self.next_appearance_row(at, -1);
                self.select_setting(next, window, cx)
            }
            _ => {}
        }
    }
}
