//! The GPUI view: paints a snapshot and turns keys, IME text, mouse, and
//! focus into PTY input. It knows nothing about the engine; it only sees
//! `Terminal` and Shika's snapshot types.

mod element;
mod glyphs;
mod ime;
mod keys;
mod metrics;
mod mouse;
mod paint;

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, ClipboardItem, Context, Edges, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Pixels, Render, SharedString, Styled, Task, Window,
    actions, div, px,
};

use crate::input::{self, MouseButton};
use crate::terminal::Terminal;
use crate::theme::Palette;
use crate::types::{Snapshot, ViewportPoint};
use element::TerminalElement;
use metrics::{GridLayout, Metrics};

actions!(
    terminal,
    [
        /// Copy the selection. Without one, the action goes on to the app.
        Copy,
        /// Paste the clipboard, bracketed when the program asked for it.
        Paste,
        /// Scroll history up a page (outside the alternate screen).
        ScrollPageUp,
        /// Scroll history down a page (outside the alternate screen).
        ScrollPageDown,
    ]
);

/// Key context of a focused terminal. App bindings that must not fire
/// while typing in a terminal should use a context that excludes it.
pub const KEY_CONTEXT: &str = "Terminal";

/// Bind the terminal's Command shortcuts. Call once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pageup", ScrollPageUp, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pagedown", ScrollPageDown, Some(KEY_CONTEXT)),
    ]);
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalConfig {
    /// Tried in order; the first one installed is used.
    pub font_families: Vec<SharedString>,
    pub font_size: Pixels,
    /// Row height as a multiple of the font size.
    pub line_height: f32,
    /// Space between the view's edge and the grid, filled with the
    /// terminal background.
    pub padding: Edges<Pixels>,
    /// Option sends ESC plus the key, like WezTerm's default for the left
    /// Option key. Off, Option types the composed character (`å`).
    pub option_as_meta: bool,
}

impl Default for TerminalConfig {
    /// JetBrains Mono, then Menlo, at 14px with a 21px row, and the
    /// terminal pad from `design/DESIGN.md`. Menlo is on every Mac and
    /// has real bold and italic faces.
    fn default() -> Self {
        Self {
            font_families: vec!["JetBrains Mono".into(), "Menlo".into()],
            font_size: px(14.),
            line_height: 1.52,
            padding: Edges {
                top: px(16.),
                right: px(20.),
                bottom: px(28.),
                left: px(20.),
            },
            option_as_meta: true,
        }
    }
}

/// Paint counters, for checking that an idle terminal does not redraw and
/// that a flood is drawn at most once per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Frames this view was painted in.
    pub frames: u64,
    /// Frames that copied a new screen from the engine.
    pub snapshots: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    TitleChanged(Option<String>),
    Bell,
    /// Ctrl-click or Cmd-click on a link. The view does not spawn a process.
    OpenLink(String),
}

pub struct TerminalView {
    terminal: Terminal,
    config: TerminalConfig,
    focus_handle: FocusHandle,
    metrics: Option<Metrics>,
    layout: Option<GridLayout>,
    snapshot: Option<Arc<Snapshot>>,
    palette: Palette,
    /// Alpha of the default background, so a translucent window shows
    /// through. Cells with their own background color stay opaque.
    background_opacity: f32,
    /// IME composition in progress, drawn at the cursor and not yet sent.
    marked_text: Option<String>,
    selecting: bool,
    /// Ctrl or Cmd is held over a link, so the pointer is a hand.
    link_hover: bool,
    /// Button held while the program receives mouse reports.
    reported_button: Option<MouseButton>,
    last_motion_cell: Option<ViewportPoint>,
    /// Fractional wheel lines carried between trackpad events.
    scroll_remainder: f32,
    sync_timer: Option<Task<()>>,
    stats: FrameStats,
    _wake_task: Task<()>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(
        terminal: Terminal,
        config: TerminalConfig,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // One slot is enough: a pending wake already means "look again".
        let (tx, rx) = async_channel::bounded::<()>(1);
        terminal.set_waker(move || {
            let _ = tx.try_send(());
        });
        let wake_task = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |view, cx| view.on_wake(cx)).is_err() {
                    break;
                }
            }
        });

        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |view, window, cx| {
                view.focus_changed(window.is_window_active(), cx);
            }),
            cx.on_focus_out(&focus_handle, window, |view, _, _, cx| {
                view.focus_changed(false, cx);
            }),
            cx.observe_window_activation(window, |view, window, cx| {
                if view.focus_handle.is_focused(window) {
                    view.focus_changed(window.is_window_active(), cx);
                }
            }),
        ];

        terminal.set_palette(palette);
        Self {
            terminal,
            config,
            focus_handle,
            metrics: None,
            layout: None,
            snapshot: None,
            palette,
            background_opacity: 1.0,
            marked_text: None,
            selecting: false,
            link_hover: false,
            reported_button: None,
            last_motion_cell: None,
            scroll_remainder: 0.0,
            sync_timer: None,
            stats: FrameStats::default(),
            _wake_task: wake_task,
            _subscriptions: subscriptions,
        }
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    pub fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// The font family in use, once the view has been painted.
    pub fn font_family(&self) -> Option<SharedString> {
        self.metrics.as_ref().map(|m| m.fonts[0].family.clone())
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Change the font or behavior. A new font size resizes the grid on the
    /// next frame.
    pub fn set_config(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        if config != self.config {
            self.config = config;
            self.metrics = None;
            cx.notify();
        }
    }

    pub fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        self.palette = palette;
        self.terminal.set_palette(palette);
        cx.notify();
    }

    /// 1.0 is opaque. Values are clamped to 0.0..=1.0.
    pub fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity != self.background_opacity {
            self.background_opacity = opacity;
            cx.notify();
        }
    }

    fn on_wake(&mut self, cx: &mut Context<Self>) {
        let notices = self.terminal.take_notices();
        if let Some(title) = notices.title {
            cx.emit(TerminalEvent::TitleChanged(title));
        }
        if notices.bell {
            cx.emit(TerminalEvent::Bell);
        }
        if let Some(text) = notices.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if self.sync_timer.is_none()
            && let Some(deadline) = self.terminal.sync_deadline()
        {
            self.arm_sync_timer(deadline, cx);
        }
        cx.notify();
    }

    /// A program that opened a synchronized update and went quiet still
    /// gets drawn when the update times out.
    fn arm_sync_timer(&mut self, deadline: Instant, cx: &mut Context<Self>) {
        let delay = deadline.saturating_duration_since(Instant::now());
        self.sync_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |view, cx| {
                view.sync_timer = None;
                match view.terminal.flush_expired_sync() {
                    Some(next) => view.arm_sync_timer(next, cx),
                    None => cx.notify(),
                }
            });
        }));
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.terminal.modes().focus_reporting {
            self.terminal.report(input::encode_focus(focused));
        }
        if !focused {
            self.marked_text = None;
        }
        cx.notify();
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_drop(cx.listener(Self::drop_paths))
            .on_action(cx.listener(|view, _: &ScrollPageUp, _, cx| view.scroll_page(true, cx)))
            .on_action(cx.listener(|view, _: &ScrollPageDown, _, cx| view.scroll_page(false, cx)))
            .child(TerminalElement { view: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::keys::key_for;
    use super::metrics::vertical_align;
    use super::mouse::click_opens_link;
    use super::*;
    use crate::input::{Key, KeyMods};
    use gpui::{Keystroke, Modifiers, MouseButton as GpuiButton};

    fn stroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    #[test]
    fn baseline_keeps_the_em_box_centered_in_the_cell() {
        // JetBrains Mono at 12.5px in a 19px row: ascent 12.75, descent -3.75.
        // A signed descent would put the baseline at 17.75, under the letters.
        let (baseline, underline) = vertical_align(px(19.), px(12.75), px(-3.75));
        assert_eq!(baseline, px(14.));
        assert!(underline > baseline);
        assert!(underline < px(19.));
        assert_eq!(vertical_align(px(19.), px(12.75), px(3.75)).0, baseline);
    }

    #[test]
    fn plain_text_is_left_to_the_input_handler() {
        assert_eq!(key_for(&stroke("a", Modifiers::none()), true), None);
        assert_eq!(key_for(&stroke("a", Modifiers::shift()), true), None);
        assert_eq!(key_for(&stroke("space", Modifiers::none()), true), None);
        // Option composes when it is not Meta.
        assert_eq!(key_for(&stroke("e", Modifiers::alt()), false), None);
    }

    #[test]
    fn command_is_left_to_the_app() {
        assert_eq!(key_for(&stroke("c", Modifiers::command()), true), None);
        assert_eq!(key_for(&stroke("left", Modifiers::command()), true), None);
        for key in ["enter", "[", "]", "n", "t", "w"] {
            assert_eq!(key_for(&stroke(key, Modifiers::command()), true), None);
        }
        let command_shift = Modifiers {
            platform: true,
            shift: true,
            ..Modifiers::none()
        };
        for key in ["[", "]", "w"] {
            assert_eq!(key_for(&stroke(key, command_shift), true), None);
        }
    }

    #[test]
    fn ctrl_and_cmd_clicks_open_links() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::none()
        };
        let cmd = Modifiers {
            platform: true,
            ..Modifiers::none()
        };
        assert!(click_opens_link(GpuiButton::Left, &cmd));
        assert!(click_opens_link(GpuiButton::Left, &ctrl));
        assert!(click_opens_link(GpuiButton::Right, &Modifiers::none()));
        assert!(!click_opens_link(GpuiButton::Left, &Modifiers::none()));
        assert!(!click_opens_link(GpuiButton::Middle, &ctrl));
    }

    #[test]
    fn control_tab_is_left_to_the_app() {
        let control = Modifiers {
            control: true,
            ..Modifiers::none()
        };
        let control_shift = Modifiers {
            control: true,
            shift: true,
            ..Modifiers::none()
        };
        assert_eq!(key_for(&stroke("tab", control), true), None);
        assert_eq!(key_for(&stroke("tab", control_shift), true), None);
        assert_eq!(
            key_for(&stroke("tab", Modifiers::none()), true),
            Some((Key::Tab, KeyMods::NONE))
        );
    }

    #[test]
    fn named_keys_and_control_combos_are_encoded() {
        assert_eq!(
            key_for(&stroke("enter", Modifiers::none()), true),
            Some((Key::Enter, KeyMods::NONE))
        );
        assert_eq!(
            key_for(&stroke("c", Modifiers::control()), true),
            Some((
                Key::Char('c'),
                KeyMods {
                    ctrl: true,
                    ..KeyMods::NONE
                }
            ))
        );
        assert_eq!(
            key_for(&stroke("f5", Modifiers::none()), true),
            Some((Key::F(5), KeyMods::NONE))
        );
        assert_eq!(
            key_for(&stroke("b", Modifiers::alt()), true),
            Some((
                Key::Char('b'),
                KeyMods {
                    alt: true,
                    ..KeyMods::NONE
                }
            ))
        );
        let shift_alt = Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::none()
        };
        assert_eq!(
            key_for(&stroke("b", shift_alt), true),
            Some((
                Key::Char('B'),
                KeyMods {
                    alt: true,
                    shift: true,
                    ctrl: false
                }
            ))
        );
    }
}
