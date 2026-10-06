//! The GPUI view: paints a snapshot and turns keys, IME text, mouse, and
//! focus into PTY input. It knows nothing about the engine; it only sees
//! `Terminal` and Shika's snapshot types.

use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, Edges, Element, ElementId,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, Font, FontFeatures, FontStyle, FontWeight,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InputHandler, InspectorElementId,
    InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, Keystroke, LayoutId,
    MouseButton as GpuiButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    PathBuilder, Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Style,
    Styled, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    outline, point, px, relative, size,
};

use crate::boxdraw::{self, BoxGlyph, Corner};
use crate::input::{self, Key, KeyMods, MouseAction, MouseButton};
use crate::terminal::Terminal;
use crate::theme::Palette;
use crate::types::{
    Cell, CellFlags, CellSide, CursorShape, Rgb, SelectionKind, Snapshot, TerminalSize,
    ViewportPoint,
};

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
}

/// Font and cell size, measured once per font setting.
#[derive(Clone)]
struct Metrics {
    /// Regular, bold, italic, bold italic.
    fonts: [Font; 4],
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    baseline: Pixels,
    underline_offset: Pixels,
}

impl Metrics {
    fn font_for(&self, flags: CellFlags) -> usize {
        flags.contains(CellFlags::BOLD) as usize | (flags.contains(CellFlags::ITALIC) as usize) << 1
    }
}

/// Where the grid was last painted, for mouse and IME positions.
#[derive(Clone, Copy)]
struct GridLayout {
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    rows: usize,
    cols: usize,
}

impl GridLayout {
    fn cell_at(&self, position: Point<Pixels>) -> (ViewportPoint, CellSide, bool) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let y = (position.y - self.origin.y) / self.line_height;
        let inside = y >= 0.0 && y < self.rows as f32;
        let col = (x.floor().max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (y.floor().max(0.0) as usize).min(self.rows.saturating_sub(1));
        let side = if x >= self.cols as f32 || (x >= 0.0 && x.fract() >= 0.5) {
            CellSide::Right
        } else {
            CellSide::Left
        };
        (ViewportPoint { row, col }, side, inside)
    }

    fn cell_bounds(&self, row: usize, col: usize, width_cells: usize) -> Bounds<Pixels> {
        Bounds::new(
            point(
                self.origin.x + self.cell_width * col as f32,
                self.origin.y + self.line_height * row as f32,
            ),
            size(self.cell_width * width_cells as f32, self.line_height),
        )
    }
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

    /// Typed or pasted input. Jumps back to the live screen first, like
    /// every terminal does.
    fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if self.terminal.display_offset() != 0 {
            self.terminal.scroll_to_bottom();
            cx.notify();
        }
        self.terminal.write(bytes);
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_text.is_some() {
            return;
        }
        let Some((key, mods)) = key_for(&event.keystroke, self.config.option_as_meta) else {
            return;
        };
        let modes = self.terminal.modes();
        if let Some(bytes) = input::encode_key(key, mods, &modes) {
            self.send_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        match self.terminal.selection_text() {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let modes = self.terminal.modes();
        let bytes = input::encode_paste(&text, modes.bracketed_paste);
        self.send_input(&bytes, cx);
    }

    /// Files dropped from Finder or a screenshot thumbnail arrive as their
    /// escaped paths, pasted like any other text, and focus the terminal so
    /// the rest of the prompt can be typed.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let text = input::dropped_paths(paths.paths().iter().filter_map(|path| path.to_str()));
        if text.is_empty() {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let modes = self.terminal.modes();
        let bytes = input::encode_paste(&text, modes.bracketed_paste);
        self.send_input(&bytes, cx);
    }

    fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        let modes = self.terminal.modes();
        if modes.alt_screen {
            let key = if up { Key::PageUp } else { Key::PageDown };
            let mods = KeyMods {
                shift: true,
                ..KeyMods::NONE
            };
            if let Some(bytes) = input::encode_key(key, mods, &modes) {
                self.terminal.report(&bytes);
            }
            return;
        }
        self.terminal.scroll_page(up);
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let Some(layout) = self.layout else { return };
        let (at, side, _) = layout.cell_at(event.position);
        let modes = self.terminal.modes();
        if modes.mouse_reporting() && !event.modifiers.shift {
            let Some(button) = report_button(event.button) else {
                return;
            };
            if let Some(bytes) = input::encode_mouse(
                Some(button),
                MouseAction::Press,
                at,
                mouse_mods(&event.modifiers),
                &modes,
            ) {
                self.terminal.report(&bytes);
            }
            self.reported_button = Some(button);
            self.last_motion_cell = Some(at);
            return;
        }
        if event.button != GpuiButton::Left {
            return;
        }
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            _ => SelectionKind::Line,
        };
        self.terminal.start_selection(kind, at, side);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        if self.selecting {
            if event.pressed_button != Some(GpuiButton::Left) {
                self.selecting = false;
                return;
            }
            // Dragging past the top or bottom edge scrolls history.
            let top = layout.origin.y;
            let bottom = top + layout.line_height * layout.rows as f32;
            if event.position.y < top {
                self.terminal.scroll(1);
            } else if event.position.y >= bottom {
                self.terminal.scroll(-1);
            }
            let (at, side, _) = layout.cell_at(event.position);
            self.terminal.update_selection(at, side);
            cx.notify();
            return;
        }
        // A file dragged over the view is not the user's mouse.
        if cx.has_active_drag() {
            return;
        }
        let modes = self.terminal.modes();
        if !modes.mouse_reporting() || (!hovered && self.reported_button.is_none()) {
            return;
        }
        let (at, _, _) = layout.cell_at(event.position);
        if self.last_motion_cell == Some(at) {
            return;
        }
        self.last_motion_cell = Some(at);
        if let Some(bytes) = input::encode_mouse(
            self.reported_button,
            MouseAction::Motion,
            at,
            mouse_mods(&event.modifiers),
            &modes,
        ) {
            self.terminal.report(&bytes);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.selecting && event.button == GpuiButton::Left {
            self.selecting = false;
            cx.notify();
        }
        let Some(button) = self.reported_button else {
            return;
        };
        if report_button(event.button) != Some(button) {
            return;
        }
        self.reported_button = None;
        let Some(layout) = self.layout else { return };
        let (at, _, _) = layout.cell_at(event.position);
        let modes = self.terminal.modes();
        if let Some(bytes) = input::encode_mouse(
            Some(button),
            MouseAction::Release,
            at,
            mouse_mods(&event.modifiers),
            &modes,
        ) {
            self.terminal.report(&bytes);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => delta.y / layout.line_height,
        } + self.scroll_remainder;
        let whole = lines.trunc();
        self.scroll_remainder = lines - whole;
        let count = whole.abs() as usize;
        if count == 0 {
            return;
        }
        // Positive is toward older output, the same direction as the
        // content moving down under the fingers.
        let up = whole > 0.0;
        let modes = self.terminal.modes();
        if modes.mouse_reporting() && !event.modifiers.shift {
            let (at, _, _) = layout.cell_at(event.position);
            let button = if up {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            let mods = mouse_mods(&event.modifiers);
            if let Some(bytes) =
                input::encode_mouse(Some(button), MouseAction::Press, at, mods, &modes)
            {
                self.terminal
                    .report(&bytes.repeat(count.min(MAX_WHEEL_REPORTS)));
            }
            return;
        }
        if modes.alt_screen && modes.alternate_scroll {
            let key = if up { Key::Up } else { Key::Down };
            if let Some(bytes) = input::encode_key(key, KeyMods::NONE, &modes) {
                self.terminal
                    .report(&bytes.repeat(count.min(MAX_WHEEL_REPORTS)));
            }
            return;
        }
        self.terminal
            .scroll(if up { count as i32 } else { -(count as i32) });
        cx.notify();
    }

    fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.send_input(text.as_bytes(), cx);
        cx.notify();
    }

    fn set_marked_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = (!text.is_empty()).then(|| text.to_string());
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn metrics(&mut self, installed: &[String], window: &mut Window) -> Metrics {
        if let Some(metrics) = &self.metrics {
            return metrics.clone();
        }
        let metrics = measure(&self.config, installed, window);
        self.metrics = Some(metrics.clone());
        metrics
    }

    /// Fit the grid to the bounds, then take a fresh snapshot only if the
    /// screen changed since the last frame.
    fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        installed: &[String],
        window: &mut Window,
    ) -> Frame {
        let metrics = self.metrics(installed, window);
        let padding = self.config.padding;
        let inner = Bounds::new(
            point(
                bounds.origin.x + padding.left,
                bounds.origin.y + padding.top,
            ),
            size(
                (bounds.size.width - padding.left - padding.right).max(px(0.)),
                (bounds.size.height - padding.top - padding.bottom).max(px(0.)),
            ),
        );
        let cols = (inner.size.width / metrics.cell_width).floor() as u16;
        let rows = (inner.size.height / metrics.line_height).floor() as u16;
        // A view squeezed to nothing (being laid out, or collapsed) keeps
        // its last size instead of telling the program it has two columns.
        if cols >= TerminalSize::MIN_COLS && rows >= TerminalSize::MIN_ROWS {
            let size = TerminalSize::new(rows, cols).with_cell(
                f32::from(metrics.cell_width).round() as u16,
                f32::from(metrics.line_height).round() as u16,
            );
            self.terminal.resize(size);
        }
        self.stats.frames += 1;
        if self.terminal.take_dirty() || self.snapshot.is_none() {
            self.snapshot = Some(Arc::new(self.terminal.snapshot()));
            self.stats.snapshots += 1;
        }
        let snapshot = self.snapshot.clone().expect("snapshot was just set");
        let layout = GridLayout {
            origin: inner.origin,
            cell_width: metrics.cell_width,
            line_height: metrics.line_height,
            rows: snapshot.rows,
            cols: snapshot.cols,
        };
        self.layout = Some(layout);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        Frame {
            snapshot,
            metrics,
            layout,
            hitbox,
            palette: self.palette,
            background_opacity: self.background_opacity,
            focused: self.focus_handle.is_focused(window) && window.is_window_active(),
            marked_text: self.marked_text.clone(),
        }
    }
}

const MAX_WHEEL_REPORTS: usize = 10;

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

/// Map a GPUI keystroke to the encoder's key. `None` for plain text: that
/// arrives through the input handler instead, so dead keys and IME work.
fn key_for(keystroke: &Keystroke, option_as_meta: bool) -> Option<(Key, KeyMods)> {
    let m = &keystroke.modifiers;
    // Command belongs to the app. Fn with a letter is a system shortcut.
    if m.platform || m.function {
        return None;
    }
    let named = match keystroke.key.as_str() {
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "escape" => Some(Key::Escape),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "insert" => Some(Key::Insert),
        "delete" => Some(Key::Delete),
        name => name
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=20).contains(n))
            .map(Key::F),
    };
    if let Some(key) = named {
        let mods = KeyMods {
            shift: m.shift,
            alt: m.alt,
            ctrl: m.control,
        };
        return Some((key, mods));
    }

    let ch = if keystroke.key == "space" {
        ' '
    } else {
        let mut chars = keystroke.key.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        ch
    };
    let meta = m.alt && option_as_meta;
    if !m.control && !meta {
        return None;
    }
    // GPUI reports Shift with letters as a lowercase key plus the flag;
    // other keys already carry the shifted character.
    let ch = if m.shift { ch.to_ascii_uppercase() } else { ch };
    Some((
        Key::Char(ch),
        KeyMods {
            shift: m.shift,
            alt: meta,
            ctrl: m.control,
        },
    ))
}

fn report_button(button: GpuiButton) -> Option<MouseButton> {
    match button {
        GpuiButton::Left => Some(MouseButton::Left),
        GpuiButton::Middle => Some(MouseButton::Middle),
        GpuiButton::Right => Some(MouseButton::Right),
        GpuiButton::Navigate(_) => None,
    }
}

fn mouse_mods(m: &gpui::Modifiers) -> KeyMods {
    KeyMods {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.control,
    }
}

/// Installed font family names, read once per process.
///
/// The SF Mono that ships inside macOS is hidden from this list (its family
/// is ".SF NS Mono") and is a variable font; GPUI can only load its regular
/// instance from bytes, so bold would be lost. "SF Mono" therefore matches
/// only when the user installed Apple's static SF Mono faces.
fn installed_fonts(cx: &App) -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| cx.text_system().all_font_names())
}

fn measure(config: &TerminalConfig, installed: &[String], window: &mut Window) -> Metrics {
    let family = pick_family(config, installed);
    // Ligatures off: glyphs are placed on the grid one cell at a time, and
    // runs skip blank cells, so a ligature could join text across a gap.
    let features = FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0)]));
    let base = Font {
        family,
        features,
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let fonts = [
        base.clone(),
        Font {
            weight: FontWeight::BOLD,
            ..base.clone()
        },
        Font {
            style: FontStyle::Italic,
            ..base.clone()
        },
        Font {
            weight: FontWeight::BOLD,
            style: FontStyle::Italic,
            ..base.clone()
        },
    ];
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&base);
    let font_size = config.font_size;
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|advance| advance.width)
        .unwrap_or(font_size * 0.6);
    let line_height = (font_size * config.line_height).round();
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size);
    let (baseline, underline_offset) = vertical_align(line_height, ascent, descent);
    Metrics {
        fonts,
        font_size,
        cell_width,
        line_height,
        baseline,
        underline_offset,
    }
}

/// Baseline and underline, both measured down from the top of the cell.
///
/// GPUI's font descent follows OpenType: it is negative when the face hangs
/// below the baseline. `baseline_offset` subtracts that signed value, which
/// pushes the baseline down by the whole descent, so the glyphs sit in the
/// bottom of the cell. The line painter treats descent as a positive
/// distance. This does the same.
fn vertical_align(line_height: Pixels, ascent: Pixels, descent: Pixels) -> (Pixels, Pixels) {
    let descent = px(f32::from(descent).abs());
    let padding_top = (line_height - ascent - descent) / 2.;
    let baseline = padding_top + ascent;
    let underline = gpui::underline_y_offset(line_height, ascent, descent);
    (baseline, underline)
}

fn pick_family(config: &TerminalConfig, installed: &[String]) -> SharedString {
    config
        .font_families
        .iter()
        .find(|family| installed.iter().any(|name| name == family.as_ref()))
        .cloned()
        .unwrap_or_else(|| "Menlo".into())
}

/// Everything paint needs, gathered in prepaint.
pub struct Frame {
    snapshot: Arc<Snapshot>,
    metrics: Metrics,
    layout: GridLayout,
    hitbox: Hitbox,
    palette: Palette,
    background_opacity: f32,
    focused: bool,
    marked_text: Option<String>,
}

struct TerminalElement {
    view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let installed = installed_fonts(cx);
        self.view.update(cx, |view, _cx| {
            view.prepare_frame(bounds, installed, window)
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        frame: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            TerminalInputHandler {
                view: self.view.clone(),
            },
            cx,
        );
        let mouse_cursor = if frame.snapshot.modes.mouse_reporting() {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(mouse_cursor, &frame.hitbox);
        register_mouse(&self.view, &frame.hitbox, window);

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            paint_frame(bounds, frame, window, cx);
        });
    }
}

fn register_mouse(view: &gpui::Entity<TerminalView>, hitbox: &Hitbox, window: &mut Window) {
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hitbox.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
                cx.stop_propagation();
            }
        }
    });
}

fn hsla(rgb: Rgb) -> Hsla {
    rgba(rgb, 1.0)
}

fn rgba(rgb: Rgb, alpha: f32) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.0,
        g: rgb.g as f32 / 255.0,
        b: rgb.b as f32 / 255.0,
        a: alpha,
    }
    .into()
}

/// Round to the device pixel grid so lines and blocks stay crisp.
fn snap(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).round() / scale)
}

fn paint_frame(bounds: Bounds<Pixels>, frame: &Frame, window: &mut Window, cx: &mut App) {
    let snapshot = &frame.snapshot;
    let layout = &frame.layout;
    let palette = &frame.palette;
    let background = snapshot.background;
    window.paint_quad(fill(bounds, rgba(background, frame.background_opacity)));

    // Where the block cursor goes, so the glyph under it can be drawn in
    // the cursor text color. Hidden while composing; the marked text sits
    // there instead.
    let cursor = snapshot.cursor.filter(|_| frame.marked_text.is_none());
    let block_cursor = cursor.filter(|c| frame.focused && c.shape == CursorShape::Block);

    for (row, line) in snapshot.lines.iter().enumerate() {
        paint_backgrounds(row, &line.cells, background, layout, window);
    }
    for (row, line) in snapshot.lines.iter().enumerate() {
        paint_selection(row, &line.cells, palette, layout, window);
    }
    if let Some(c) = block_cursor {
        let width = if c.wide { 2 } else { 1 };
        window.paint_quad(fill(
            layout.cell_bounds(c.row, c.col, width),
            hsla(snapshot.cursor_color),
        ));
    }
    let scale = window.scale_factor();
    for (row, line) in snapshot.lines.iter().enumerate() {
        let cursor_col = block_cursor.filter(|c| c.row == row).map(|c| c.col);
        paint_text(row, line, cursor_col, frame, scale, window);
        paint_decorations(row, &line.cells, cursor_col, frame, window);
    }

    if let Some(c) = cursor
        && block_cursor.is_none()
    {
        let width = if c.wide { 2 } else { 1 };
        let cell = layout.cell_bounds(c.row, c.col, width);
        let color = hsla(snapshot.cursor_color);
        let bar = px(2.).max(px(1.0 / scale));
        match (frame.focused, c.shape) {
            (true, CursorShape::Beam) => {
                window.paint_quad(fill(
                    Bounds::new(cell.origin, size(bar, cell.size.height)),
                    color,
                ));
            }
            (true, CursorShape::Underline) => {
                let origin = point(cell.origin.x, cell.origin.y + cell.size.height - bar);
                window.paint_quad(fill(Bounds::new(origin, size(cell.size.width, bar)), color));
            }
            _ => window.paint_quad(outline(cell, color, gpui::BorderStyle::Solid)),
        }
    }

    if let Some(text) = &frame.marked_text {
        paint_marked_text(text, snapshot, frame, window, cx);
    }
}

fn paint_backgrounds(
    row: usize,
    cells: &[Cell],
    default: Rgb,
    layout: &GridLayout,
    window: &mut Window,
) {
    let mut col = 0;
    while col < cells.len() {
        let color = cells[col].bg;
        let start = col;
        while col < cells.len() && cells[col].bg == color {
            col += 1;
        }
        if color != default {
            window.paint_quad(fill(
                layout.cell_bounds(row, start, col - start),
                hsla(color),
            ));
        }
    }
}

fn paint_selection(
    row: usize,
    cells: &[Cell],
    palette: &Palette,
    layout: &GridLayout,
    window: &mut Window,
) {
    let mut col = 0;
    while col < cells.len() {
        if !cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
            continue;
        }
        let start = col;
        while col < cells.len() && cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
        }
        window.paint_quad(fill(
            layout.cell_bounds(row, start, col - start),
            rgba(palette.selection, palette.selection_alpha),
        ));
    }
}

/// One glyph run per row. Blank cells are left out of the shaped text, and
/// every glyph is placed on its own cell, so fallback fonts (CJK, emoji)
/// with other advances cannot push the rest of the row off the grid.
fn paint_text(
    row: usize,
    line: &crate::types::Line,
    cursor_col: Option<usize>,
    frame: &Frame,
    scale: f32,
    window: &mut Window,
) {
    struct Placed {
        byte: usize,
        col: usize,
        color: Hsla,
    }

    let metrics = &frame.metrics;
    let layout = &frame.layout;
    let mut text = String::new();
    let mut placed: Vec<Placed> = Vec::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let row_top = layout.origin.y + layout.line_height * row as f32;

    for (col, cell) in line.cells.iter().enumerate() {
        if cell.is_blank() {
            continue;
        }
        let color = if cursor_col == Some(col) {
            hsla(frame.palette.cursor_text)
        } else {
            hsla(cell.fg)
        };
        if let Some(shape) = boxdraw::lookup(cell.ch) {
            let width = if cell.flags.contains(CellFlags::WIDE) {
                2
            } else {
                1
            };
            paint_box_glyph(
                shape,
                layout.cell_bounds(row, col, width),
                color,
                metrics,
                scale,
                window,
            );
            continue;
        }
        let start = text.len();
        text.push(cell.ch);
        if let Some(marks) = line.combining_at(col) {
            text.push_str(marks);
        }
        placed.push(Placed {
            byte: start,
            col,
            color,
        });
        let font = &metrics.fonts[metrics.font_for(cell.flags)];
        match runs.last_mut() {
            Some(run) if run.font == *font => run.len += text.len() - start,
            _ => runs.push(TextRun {
                len: text.len() - start,
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    if text.is_empty() {
        return;
    }

    let shaped = window
        .text_system()
        .layout_line(&text, metrics.font_size, &runs, None);
    let baseline_y = row_top + metrics.baseline;
    let mut current: Option<(usize, Pixels)> = None;
    for run in &shaped.runs {
        for glyph in &run.glyphs {
            let index = placed
                .partition_point(|p| p.byte <= glyph.index)
                .saturating_sub(1);
            let Some(cell) = placed.get(index) else {
                continue;
            };
            // The first glyph of a cell sits on the cell; later glyphs of
            // the same cell (combining marks) keep their shaped offset.
            let base_x = match current {
                Some((i, x)) if i == index => x,
                _ => {
                    current = Some((index, glyph.position.x));
                    glyph.position.x
                }
            };
            let x =
                layout.origin.x + layout.cell_width * cell.col as f32 + (glyph.position.x - base_x);
            let origin = point(x, baseline_y + glyph.position.y);
            let _ = if glyph.is_emoji {
                window.paint_emoji(origin, run.font_id, glyph.id, metrics.font_size)
            } else {
                window.paint_glyph(origin, run.font_id, glyph.id, metrics.font_size, cell.color)
            };
        }
    }
}

fn paint_box_glyph(
    shape: BoxGlyph,
    cell: Bounds<Pixels>,
    color: Hsla,
    metrics: &Metrics,
    scale: f32,
    window: &mut Window,
) {
    let light = px(1.0_f32.max((f32::from(metrics.font_size) / 12.0).round()));
    let heavy = light * 2.0;
    let x0 = cell.origin.x;
    let y0 = cell.origin.y;
    let w = cell.size.width;
    let h = cell.size.height;
    let cx = snap(x0 + w / 2.0, scale);
    let cy = snap(y0 + h / 2.0, scale);
    let x1 = x0 + w;
    let y1 = y0 + h;
    let thick = |weight: u8| match weight {
        0 => px(0.),
        1 => light,
        _ => heavy,
    };
    let rect = |left: Pixels, top: Pixels, right: Pixels, bottom: Pixels| {
        Bounds::from_corners(point(left, top), point(right, bottom))
    };
    match shape {
        BoxGlyph::Lines(arms) => {
            let horizontal = thick(arms.left.max(arms.right));
            let vertical = thick(arms.up.max(arms.down));
            if arms.left > 0 {
                let t = thick(arms.left);
                window.paint_quad(fill(
                    rect(x0, cy - t / 2.0, cx + vertical / 2.0, cy + t / 2.0),
                    color,
                ));
            }
            if arms.right > 0 {
                let t = thick(arms.right);
                window.paint_quad(fill(
                    rect(cx - vertical / 2.0, cy - t / 2.0, x1, cy + t / 2.0),
                    color,
                ));
            }
            if arms.up > 0 {
                let t = thick(arms.up);
                window.paint_quad(fill(
                    rect(cx - t / 2.0, y0, cx + t / 2.0, cy + horizontal / 2.0),
                    color,
                ));
            }
            if arms.down > 0 {
                let t = thick(arms.down);
                window.paint_quad(fill(
                    rect(cx - t / 2.0, cy - horizontal / 2.0, cx + t / 2.0, y1),
                    color,
                ));
            }
        }
        BoxGlyph::Arc(corner) => {
            let radius = (w / 2.0).min(h / 2.0);
            let (vertical_end, horizontal_end, dx, dy) = match corner {
                Corner::DownRight => (y1, x1, 1.0, 1.0),
                Corner::DownLeft => (y1, x0, -1.0, 1.0),
                Corner::UpLeft => (y0, x0, -1.0, -1.0),
                Corner::UpRight => (y0, x1, 1.0, -1.0),
            };
            let mut path = PathBuilder::stroke(light);
            path.move_to(point(cx, vertical_end));
            path.line_to(point(cx, cy + radius * dy));
            path.curve_to(point(cx + radius * dx, cy), point(cx, cy));
            path.line_to(point(horizontal_end, cy));
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        BoxGlyph::Blocks(rects, alpha) => {
            let color = color.opacity(alpha);
            for &(left, top, right, bottom) in rects {
                let bounds = rect(
                    snap(x0 + w * left, scale),
                    snap(y0 + h * top, scale),
                    snap(x0 + w * right, scale),
                    snap(y0 + h * bottom, scale),
                );
                window.paint_quad(fill(bounds, color));
            }
        }
    }
}

fn paint_decorations(
    row: usize,
    cells: &[Cell],
    cursor_col: Option<usize>,
    frame: &Frame,
    window: &mut Window,
) {
    let layout = &frame.layout;
    let metrics = &frame.metrics;
    let row_top = layout.origin.y + layout.line_height * row as f32;
    let thickness = px(1.);
    let mut col = 0;
    while col < cells.len() {
        let flags = cells[col].flags & (CellFlags::ANY_UNDERLINE | CellFlags::STRIKETHROUGH);
        let fg = cells[col].fg;
        let start = col;
        while col < cells.len()
            && cells[col].fg == fg
            && cells[col].flags & (CellFlags::ANY_UNDERLINE | CellFlags::STRIKETHROUGH) == flags
            && cursor_col != Some(col)
        {
            col += 1;
        }
        if col == start {
            // The block cursor cell: decorate it alone, in the cursor text color.
            col += 1;
        }
        if flags.is_empty() {
            continue;
        }
        let color = if cursor_col == Some(start) {
            hsla(frame.palette.cursor_text)
        } else {
            hsla(fg)
        };
        let x = layout.origin.x + layout.cell_width * start as f32;
        let width = layout.cell_width * (col - start) as f32;
        let underline_y = row_top + metrics.underline_offset;
        if flags.intersects(CellFlags::ANY_UNDERLINE) {
            let wavy = flags.contains(CellFlags::CURLY_UNDERLINE);
            let style = UnderlineStyle {
                thickness,
                color: Some(color),
                wavy,
            };
            window.paint_underline(point(x, underline_y), width, &style);
            if flags.contains(CellFlags::DOUBLE_UNDERLINE) {
                window.paint_underline(point(x, underline_y + thickness * 2.0), width, &style);
            }
        }
        if flags.contains(CellFlags::STRIKETHROUGH) {
            let y = row_top + metrics.baseline - metrics.font_size * 0.3;
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(width, thickness)),
                color,
            ));
        }
    }
}

fn paint_marked_text(
    text: &str,
    snapshot: &Snapshot,
    frame: &Frame,
    window: &mut Window,
    cx: &mut App,
) {
    let layout = &frame.layout;
    let metrics = &frame.metrics;
    let (row, col) = snapshot.cursor.map(|c| (c.row, c.col)).unwrap_or((0, 0));
    let color = hsla(snapshot.foreground);
    let run = TextRun {
        len: text.len(),
        font: metrics.fonts[0].clone(),
        color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: None,
    };
    let shaped =
        window
            .text_system()
            .shape_line(text.to_string().into(), metrics.font_size, &[run], None);
    let origin = layout.cell_bounds(row, col, 1).origin;
    let width = shaped.width.max(layout.cell_width);
    window.paint_quad(fill(
        Bounds::new(origin, size(width, layout.line_height)),
        hsla(snapshot.background),
    ));
    let _ = shaped.paint(
        origin,
        layout.line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// The platform side of IME and plain text input. The terminal has no
/// editable document, so the only text it reports is the composition.
struct TerminalInputHandler {
    view: gpui::Entity<TerminalView>,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let len = self
            .view
            .read(cx)
            .marked_text
            .as_deref()
            .map(utf16_len)
            .unwrap_or(0);
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .read(cx)
            .marked_text
            .as_deref()
            .map(|text| 0..utf16_len(text))
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let view = self.view.read(cx);
        let text = view.marked_text.as_deref()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = range_utf16.start.min(units.len());
        let end = range_utf16.end.clamp(start, units.len());
        *adjusted_range = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| view.commit_text(text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.set_marked_text(new_text, window, cx));
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            view.marked_text = None;
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let view = self.view.read(cx);
        let layout = view.layout?;
        let cursor = view.snapshot.as_ref()?.cursor;
        let (row, col) = cursor.map(|c| (c.row, c.col)).unwrap_or((0, 0));
        let mut bounds = layout.cell_bounds(row, col, 1);
        bounds.origin.x += layout.cell_width * range_utf16.start as f32;
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    /// Held keys repeat, as in every terminal, instead of opening the
    /// accent picker.
    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

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
        for key in ["[", "]"] {
            assert_eq!(key_for(&stroke(key, command_shift), true), None);
        }
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
