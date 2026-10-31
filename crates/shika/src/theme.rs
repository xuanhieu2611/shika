//! The active theme on the root view: palette, chrome, and terminal opacity,
//! plus saving settings.json.
use super::*;

impl Shika {
    pub(super) fn terminal_opacity(&self) -> f32 {
        appearance::terminal_alpha_for(&self.appearance, self.reduce_transparency)
    }

    /// Whether this paint is dark: the forced theme mode, or macOS's
    /// appearance in System mode. Every theme lookup goes through here.
    fn is_dark(&self, window: &Window) -> bool {
        appearance::is_dark_for(self.theme.mode, window.appearance())
    }

    /// The catalog theme painting the window now.
    pub(super) fn active_theme(&self, window: &Window) -> &'static Theme {
        appearance::resolve_theme(&self.theme, self.is_dark(window))
    }

    pub(super) fn terminal_palette(&self, window: &Window) -> Palette {
        appearance::terminal_palette(
            &self.appearance,
            self.active_theme(window),
            self.reduce_transparency,
        )
    }

    pub(super) fn chrome(&self, window: &Window) -> Chrome {
        appearance::chrome_for(
            &self.appearance,
            self.active_theme(window),
            self.reduce_transparency,
        )
    }

    /// Give every live terminal view the current theme's palette and alpha,
    /// and invalidate Settings' cached usage chart before rendering.
    pub(super) fn push_terminal_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &self.overlay
        {
            view.set_chrome(&self.chrome(window), cx);
        }
        let opacity = self.terminal_opacity();
        let palette = self.terminal_palette(window);
        for card in &self.cards {
            for pane in std::iter::once(&card.agent).chain(card.shells.iter()) {
                pane.view.update(cx, |view, cx| {
                    view.set_palette(palette, cx);
                    view.set_background_opacity(opacity, cx);
                });
            }
        }
    }

    pub(super) fn save_settings(&mut self) {
        let settings = Settings {
            theme: self.theme.clone(),
            appearance: self.appearance,
            branch_prefix: self.branch_prefix.clone(),
            font_size: self.font_size,
            notification_sound: self.notification_sound,
            column: self.column,
            changes: self.changes.width,
            agents: self.agents.clone(),
            keys: self.keys.clone(),
            background: self.background.clone(),
        };
        if let Err(e) = self.core.save_settings(&settings) {
            self.message(e.to_string());
        }
    }
}
